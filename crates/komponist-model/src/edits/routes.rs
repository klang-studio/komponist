//! Routing edits (wave 9, ARCHITECTURE §6.8): buses (groups and effect buses), outputs, sends,
//! sidechains, duplicating tracks with their routing.
//!
//! Every edit here keeps the invariants of [`crate::routing`]: it validates its arguments, applies
//! the change, puts every group's members back behind it and rejects the result with
//! [`EditError::RoutingCycle`] when the routing graph would get a cycle. Deleting tracks (buses
//! too) is [`super::remove_tracks`]; it repairs every reference to them in the same step.
//!
//! Group membership is the routing: a track is a member of a group because it outputs to it, so
//! moving a track into or out of a group is [`set_output`].

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::{
    DefaultNames, EditError, EditResult, copy_clip, locate_track, next_hue, track_mut, unique,
    update_track, valid_db, valid_hue, valid_name,
};
use crate::audio::AudioInput;
use crate::ids::{DeviceId, SendId, TrackId};
use crate::project::{
    Channel, ChannelRef, Device, Hue, MidiChannelSel, MidiInputSel, Project, SEND_LEVEL_DEFAULT_DB,
    SEND_TAP_DEFAULT, Sidechain, TRACK_HEIGHT_DEFAULT, Tap, Track, TrackKind, TrackOutput,
    TrackSend,
};
use crate::routing::{self, RoutingIssue};

/// German name base of a new effect bus: „Bus 1“, „Bus 2“, … (WP-100 §6); see [`DefaultNames`].
pub const DEFAULT_BUS_NAME: &str = "Bus";
/// German name base of a new group: „Gruppe 1“, „Gruppe 2“, … (WP-100 §6); see [`DefaultNames`].
pub const DEFAULT_GROUP_NAME: &str = "Gruppe";

/// What a new bus gets. Two ways to make one (owner decision 1 C): [`NewBus::group`] (Strg G,
/// „In Gruppe zusammenfassen“) and [`NewBus::effect`] („Send zu neuem Bus“).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewBus {
    /// Name; `None` = „Gruppe n“ or „Bus n“ with the first free `n` from 1.
    pub name: Option<String>,
    /// Colour; `None` = [`next_hue`] (owner decision 14 A).
    pub hue: Option<Hue>,
    /// Shown as a group: a foldable folder of the tracks that output to it.
    pub group: bool,
    /// Display index; `None` = where the first member was (a group with members), else the end.
    pub index: Option<usize>,
    /// Tracks that output to the new bus (a group's members). Members inside another listed
    /// member's group stay in it.
    pub members: Vec<TrackId>,
    /// Tracks that get a send to the new bus ([`SEND_LEVEL_DEFAULT_DB`], [`SEND_TAP_DEFAULT`]).
    /// Buses cannot send (owner decision 11 A).
    pub senders: Vec<TrackId>,
}

impl NewBus {
    /// Strg G: a group of `members` where the first of them was.
    pub fn group(members: Vec<TrackId>) -> NewBus {
        NewBus {
            group: true,
            members,
            ..NewBus::default()
        }
    }

    /// „Send zu neuem Bus“: an effect bus at the end, fed by a send from each of `senders`.
    pub fn effect(senders: Vec<TrackId>) -> NewBus {
        NewBus {
            senders,
            ..NewBus::default()
        }
    }
}

/// A send property (send list row, its context menu).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SendProp {
    /// Level in dB, clamped to `VOLUME_DB_MIN ..= VOLUME_DB_MAX` (a drag is one gesture).
    LevelDb(f32),
    /// Pre or post fader.
    Tap(Tap),
    /// On or off; an off send keeps its level.
    Enabled(bool),
    /// Another target bus (rejected when the track already sends there or it closes a cycle).
    Target(TrackId),
}

/// `"{base} {n}"` with the first `n` from 1 that no track uses.
pub fn numbered_track_name(project: &Project, base: &str) -> String {
    (1u64..)
        .map(|n| format!("{base} {n}"))
        .find(|name| project.tracks.iter().all(|t| t.name != *name))
        .expect("one of more names than tracks is free")
}

/// Rejects a result that breaks a routing invariant.
pub(super) fn check_routing(p: &Project) -> Result<(), EditError> {
    routing::check(p).map_err(|issue| match issue {
        RoutingIssue::Cycle | RoutingIssue::SelfReference(_) => EditError::RoutingCycle,
        RoutingIssue::NotABus { target, .. } => EditError::NotABus(target),
        RoutingIssue::MissingTrack { missing, .. } => EditError::TrackNotFound(missing),
        RoutingIssue::DuplicateSend { .. } => {
            EditError::Invalid("the track already sends to this bus")
        }
        RoutingIssue::BusContent(_) => EditError::Invalid("a bus takes audio effects only"),
        RoutingIssue::KindContent(_) => {
            EditError::Invalid("the track kind does not take this content")
        }
        RoutingIssue::InputNotABus { target, .. } => EditError::NotABus(target),
        RoutingIssue::NotAGroup(_) | RoutingIssue::GroupOrder => EditError::Invalid("broken group"),
    })
}

/// Puts every group's members behind it and checks the routing.
fn finish(mut p: Project) -> EditResult {
    routing::normalize_groups(&mut p);
    check_routing(&p)?;
    Ok(p)
}

/// The bus with `id`, or why not.
fn bus(project: &Project, id: TrackId) -> Result<&Arc<Track>, EditError> {
    let track = project.track(id).ok_or(EditError::TrackNotFound(id))?;
    if !track.is_bus() {
        return Err(EditError::NotABus(id));
    }
    Ok(track)
}

/// A new send from a track to `target` with the defaults.
fn new_send(id: SendId, target: TrackId) -> TrackSend {
    TrackSend {
        id,
        target,
        level_db: SEND_LEVEL_DEFAULT_DB,
        tap: SEND_TAP_DEFAULT,
        enabled: true,
    }
}

/// Adds a bus (one undo step for all of it, WP-100 §6).
///
/// - A group with members takes the place of the topmost member. The members (except those inside
///   another listed member's group) output to it; the group outputs where they all did (so a
///   group made inside a group nests), else to the master.
/// - Without members the bus goes to `index` (or the end) and outputs to the master; a group
///   inserted before a group member joins that member's group.
/// - Every sender gets a send to the new bus.
pub fn add_bus(project: &Project, spec: NewBus) -> Result<(Project, TrackId), EditError> {
    add_bus_in(project, spec, &DefaultNames::DE)
}

/// [`add_bus`] with the name bases of `names` for a bus or group without a name (the app passes
/// the words of its language).
pub fn add_bus_in(
    project: &Project,
    spec: NewBus,
    names: &DefaultNames,
) -> Result<(Project, TrackId), EditError> {
    let members = unique(&spec.members);
    let senders = unique(&spec.senders);
    for &id in members.iter().chain(&senders) {
        locate_track(project, id)?;
    }
    if senders
        .iter()
        .any(|&s| project.track(s).is_some_and(|t| t.is_bus()))
    {
        return Err(EditError::Invalid("a bus has no sends"));
    }
    let hue = match spec.hue {
        Some(hue) => valid_hue(hue)?,
        None => next_hue(project),
    };
    let name = match &spec.name {
        Some(name) => valid_name(name)?,
        None if spec.group => numbered_track_name(project, names.group),
        None => numbered_track_name(project, names.bus),
    };
    let roots: Vec<&Arc<Track>> = project
        .tracks
        .iter()
        .filter(|t| members.contains(&t.id))
        .filter(|t| {
            !routing::group_ancestors(project, t.id)
                .iter()
                .any(|a| members.contains(a))
        })
        .collect();
    let mut p = project.clone();
    let id = TrackId(p.alloc_id());
    let len = p.tracks.len();
    let (index, output) = match roots.first() {
        Some(first) => {
            let index = match spec.index {
                Some(index) => index.min(len),
                None => locate_track(project, first.id)?,
            };
            let output = Some(first.output)
                .filter(|o| roots.iter().all(|r| r.output == *o))
                .filter(|o| !matches!(o, TrackOutput::Bus(b) if members.contains(b)))
                .unwrap_or(TrackOutput::Master);
            (index, output)
        }
        None => {
            let index = spec.index.unwrap_or(len).min(len);
            let output = match p.tracks.get(index) {
                Some(next) if spec.group => routing::group_of(project, next.id)
                    .map_or(TrackOutput::Master, TrackOutput::Bus),
                _ => TrackOutput::Master,
            };
            (index, output)
        }
    };
    let roots: HashSet<TrackId> = roots.iter().map(|t| t.id).collect();
    p.tracks.insert(
        index,
        Arc::new(Track {
            id,
            name,
            hue,
            kind: TrackKind::Bus,
            channel: Channel::default(),
            arm: false,
            monitor: false,
            input: MidiInputSel::All,
            midi_channel: MidiChannelSel::Omni,
            audio: crate::audio::AudioIo::default(),
            height: TRACK_HEIGHT_DEFAULT,
            clips: Vec::new(),
            output,
            sends: Vec::new(),
            folder: spec.group,
            folded: false,
        }),
    );
    for i in 0..p.tracks.len() {
        let track = p.tracks[i].id;
        if roots.contains(&track) {
            track_mut(&mut p, i).output = TrackOutput::Bus(id);
        }
        if senders.contains(&track) && p.tracks[i].send_to(id).is_none() {
            let send = SendId(p.alloc_id());
            track_mut(&mut p, i).sends.push(new_send(send, id));
        }
    }
    Ok((finish(p)?, id))
}

/// Routes the main output of every listed track (or bus) to `output`. Routing a track to a group
/// makes it a member (it moves behind the group's members); routing a member elsewhere takes it out
/// (it lands right behind the group's block).
pub fn set_output(project: &Project, tracks: &[TrackId], output: TrackOutput) -> EditResult {
    let indices = tracks
        .iter()
        .map(|&id| locate_track(project, id))
        .collect::<Result<Vec<_>, _>>()?;
    if let TrackOutput::Bus(target) = output {
        bus(project, target)?;
    }
    let mut p = project.clone();
    for index in indices {
        update_track(&mut p, index, |t| {
            t.output = output;
            Ok(())
        })?;
    }
    finish(p)
}

/// Adds a send from `track` to the bus `target` at [`SEND_LEVEL_DEFAULT_DB`], [`SEND_TAP_DEFAULT`],
/// enabled. Returns its id. A bus cannot send (owner decision 11 A), a track sends at most once to
/// the same bus.
pub fn add_send(
    project: &Project,
    track: TrackId,
    target: TrackId,
) -> Result<(Project, SendId), EditError> {
    let index = locate_track(project, track)?;
    bus(project, target)?;
    let source = &project.tracks[index];
    if source.is_bus() {
        return Err(EditError::Invalid("a bus has no sends"));
    }
    if source.send_to(target).is_some() {
        return Err(EditError::Invalid("the track already sends to this bus"));
    }
    let mut p = project.clone();
    let id = SendId(p.alloc_id());
    track_mut(&mut p, index).sends.push(new_send(id, target));
    Ok((finish(p)?, id))
}

/// Removes a send.
pub fn remove_send(project: &Project, send: SendId) -> EditResult {
    let (track, index) = project
        .find_send(send)
        .ok_or(EditError::SendNotFound(send))?;
    let track_index = locate_track(project, track.id)?;
    let mut p = project.clone();
    track_mut(&mut p, track_index).sends.remove(index);
    Ok(p)
}

/// Changes a send.
pub fn set_send(project: &Project, send: SendId, prop: SendProp) -> EditResult {
    let (track, index) = project
        .find_send(send)
        .ok_or(EditError::SendNotFound(send))?;
    let track_index = locate_track(project, track.id)?;
    if let SendProp::Target(target) = prop {
        bus(project, target)?;
        if track
            .sends
            .iter()
            .any(|s| s.target == target && s.id != send)
        {
            return Err(EditError::Invalid("the track already sends to this bus"));
        }
    }
    let mut p = project.clone();
    let s = &mut track_mut(&mut p, track_index).sends[index];
    match prop {
        SendProp::LevelDb(db) => s.level_db = valid_db(db)?,
        SendProp::Tap(tap) => s.tap = tap,
        SendProp::Enabled(on) => s.enabled = on,
        SendProp::Target(target) => s.target = target,
    }
    finish(p)
}

/// Sets or clears the sidechain source of a device (the „SC“ popover). The source is a track or
/// bus other than the one holding the device and nothing that track feeds; a device on the master
/// can take any track. [`Sidechain::new`] gives the default tap.
pub fn set_sidechain(
    project: &Project,
    device: DeviceId,
    sidechain: Option<Sidechain>,
) -> EditResult {
    let (holder, index, _) = project
        .find_device(device)
        .ok_or(EditError::DeviceNotFound(device))?;
    if let Some(sc) = sidechain {
        locate_track(project, sc.source)?;
        if holder == ChannelRef::Track(sc.source) {
            return Err(EditError::RoutingCycle);
        }
    }
    let mut p = project.clone();
    let channel = super::channel_mut(&mut p, holder)?;
    Arc::make_mut(&mut channel.devices[index]).sidechain = sidechain;
    finish(p)
}

/// Ctrl D on tracks: copies each listed track (clips, notes, devices, sends with new ids) right
/// behind itself (behind its block for a group), named like `unique_track_name` makes it, not
/// armed. A copy keeps the output, sends and device sidechains of its original; references
/// between listed tracks point at the copies (the copies of a group's listed members are members
/// of the group's copy, a duplicated kick feeds the duplicated compressor). Returns the copies in
/// the order of `tracks`.
pub fn duplicate_tracks(
    project: &Project,
    tracks: &[TrackId],
) -> Result<(Project, Vec<TrackId>), EditError> {
    let ids = unique(tracks);
    for &id in &ids {
        locate_track(project, id)?;
    }
    let mut p = project.clone();
    let mut copy_of: HashMap<TrackId, TrackId> = HashMap::with_capacity(ids.len());
    let mut copies = Vec::with_capacity(ids.len());
    for &id in &ids {
        let index = locate_track(&p, id)?;
        let original = Arc::clone(&p.tracks[index]);
        let mut copy = Track::clone(&original);
        copy.id = TrackId(p.alloc_id());
        copy.name = super::unique_track_name(&p, &original.name);
        copy.arm = false;
        copy.channel.devices = original
            .channel
            .devices
            .iter()
            .map(|d| {
                let mut d = Device::clone(d);
                d.id = DeviceId(p.alloc_id());
                Arc::new(d)
            })
            .collect();
        copy.clips = original
            .clips
            .iter()
            .map(|c| Arc::new(copy_clip(&mut p, c)))
            .collect();
        copy.sends = original
            .sends
            .iter()
            .map(|s| TrackSend {
                id: SendId(p.alloc_id()),
                ..s.clone()
            })
            .collect();
        let mut end = index + 1;
        while end < p.tracks.len() && routing::group_ancestors(&p, p.tracks[end].id).contains(&id) {
            end += 1;
        }
        copy_of.insert(id, copy.id);
        copies.push(copy.id);
        p.tracks.insert(end, Arc::new(copy));
    }
    let remap = |id: TrackId| copy_of.get(&id).copied().unwrap_or(id);
    for &copy in &copies {
        let index = locate_track(&p, copy)?;
        update_track(&mut p, index, |t| {
            if let TrackOutput::Bus(b) = t.output {
                t.output = TrackOutput::Bus(remap(b));
            }
            for s in &mut t.sends {
                s.target = remap(s.target);
            }
            if let Some(bus) = t.input_bus() {
                t.audio.input = AudioInput::Bus(remap(bus));
            }
            for d in &mut t.channel.devices {
                if let Some(sc) = d.sidechain
                    && copy_of.contains_key(&sc.source)
                {
                    Arc::make_mut(d).sidechain = Some(Sidechain {
                        source: remap(sc.source),
                        ..sc
                    });
                }
            }
            Ok(())
        })?;
    }
    Ok((finish(p)?, copies))
}

/// After `removed` tracks are gone from `p` (a copy of `before`): outputs to a removed bus go
/// where that bus went (transitively; deleting a group dissolves it, owner decision 13 A), sends to
/// it are dropped, sidechains from a removed track are cleared (master chain too). Tracks without
/// such references keep their `Arc`.
pub(super) fn detach_removed(p: &mut Project, before: &Project, removed: &HashSet<TrackId>) {
    if removed.is_empty() {
        return;
    }
    let gone = |id: &TrackId| removed.contains(id);
    let resolve_output = |mut output: TrackOutput| {
        for _ in 0..=before.tracks.len() {
            match output {
                TrackOutput::Bus(b) if gone(&b) => {
                    output = before.track(b).map_or(TrackOutput::Master, |t| t.output);
                }
                _ => return output,
            }
        }
        TrackOutput::Master
    };
    let refers = |t: &Track| {
        t.input_bus().is_some_and(|b| gone(&b))
            || matches!(t.output, TrackOutput::Bus(b) if gone(&b))
            || t.sends.iter().any(|s| gone(&s.target))
            || t.channel
                .devices
                .iter()
                .any(|d| d.sidechain.is_some_and(|sc| gone(&sc.source)))
    };
    for index in 0..p.tracks.len() {
        if !refers(&p.tracks[index]) {
            continue;
        }
        let t = track_mut(p, index);
        t.output = resolve_output(t.output);
        t.sends.retain(|s| !gone(&s.target));
        if t.input_bus().is_some_and(|b| gone(&b)) {
            t.audio.input = AudioInput::None;
        }
        clear_sidechains(&mut t.channel, removed);
    }
    if p.master
        .devices
        .iter()
        .any(|d| d.sidechain.is_some_and(|sc| gone(&sc.source)))
    {
        clear_sidechains(Arc::make_mut(&mut p.master), removed);
    }
}

/// Clears every sidechain of `channel` whose source is in `removed`.
fn clear_sidechains(channel: &mut Channel, removed: &HashSet<TrackId>) {
    for d in &mut channel.devices {
        if d.sidechain.is_some_and(|sc| removed.contains(&sc.source)) {
            Arc::make_mut(d).sidechain = None;
        }
    }
}
