//! Every change to a project, as a pure function (ARCHITECTURE §5.3).
//!
//! Shape: `fn(&Project, args) -> Result<Project, EditError>`; edits that create objects return the
//! new ids as well (`Result<(Project, Id), EditError>`). An edit clones only the path to what it
//! changes (`Arc::make_mut`), allocates ids from [`Project::alloc_id`], keeps all invariants
//! documented in [`crate::project`] (sorted clips and notes, non-empty takes, …) and never mutates
//! its input. On error the input is untouched and no partial result exists.
//!
//! Callers go through [`crate::Document::apply`] / [`crate::Document::apply_with`] so every edit
//! becomes an undo step (or part of a gesture). The app turns UI interactions into these calls; the
//! acceptance tests call them directly.
//!
//! Conventions:
//! - Objects are addressed by id. Clips, notes and devices are found anywhere in the project.
//! - Note edits work on the **active take** of a MIDI clip.
//! - Numeric values from drags (volume, pan, velocity, swing, …) are **clamped** into their range;
//!   structurally invalid input (unknown id, wrong clip kind, hue not in [`Hue::ALL`], meter with a
//!   bad denominator) is an [`EditError`].
//! - Snapping is the caller's job (grid helpers in [`crate::time::Grid`]); edits take exact ticks.

use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;
use std::sync::Arc;

use crate::ids::{ClipId, DeviceId, NoteId, SendId, TrackId};
use crate::project::{
    BUILTIN_KLOPFER, BUILTIN_SAMPLER, Channel, ChannelRef, Clip, ClipContent, ClipKind,
    DEFAULT_STEP_ROWS, Device, DeviceRole, Hue, MAX_STEPS, MAX_SWING, MidiChannelSel, MidiClip,
    MidiInputSel, Note, PluginFormat, PluginRef, Project, Sidechain, Step, StepClip, StepRow,
    TRACK_HEIGHT_DEFAULT, TRACK_HEIGHT_MAX, TRACK_HEIGHT_MIN, Take, Track, TrackKind, TrackOutput,
    VOLUME_DB_MAX, VOLUME_DB_MIN,
};
use crate::time::{Meter, StepResolution, Tempo, Tick, TickRange};

mod routes;

pub use routes::{
    DEFAULT_BUS_NAME, DEFAULT_GROUP_NAME, NewBus, SendProp, add_bus, add_bus_in, add_send,
    duplicate_tracks, numbered_track_name, remove_send, set_output, set_send, set_sidechain,
};

#[cfg(test)]
mod routing_tests;
#[cfg(test)]
pub(crate) mod tests;

// ------------------------------------------------------------------------------------------------
// Errors
// ------------------------------------------------------------------------------------------------

/// Why an edit was rejected. The input project is unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// The project has no unused object identifiers left.
    IdsExhausted,
    /// No track with this id.
    TrackNotFound(TrackId),
    /// No clip with this id.
    ClipNotFound(ClipId),
    /// No note with this id in the clip's active take.
    NoteNotFound(NoteId),
    /// No device with this id.
    DeviceNotFound(DeviceId),
    /// No send with this id.
    SendNotFound(SendId),
    /// The track is not a bus, but the edit needs one (output, send or group target).
    NotABus(TrackId),
    /// The edit would close a cycle in the routing graph (outputs, sends, sidechains), including
    /// a track routing or tapping into itself.
    RoutingCycle,
    /// The clip has the other content kind (note edit on a step clip, step edit on a MIDI clip).
    WrongClipKind {
        /// The clip.
        clip: ClipId,
        /// The kind the edit needs.
        expected: ClipKind,
    },
    /// An index (track position, chain slot, step row, step, take) is out of range.
    IndexOutOfRange {
        /// What the index addresses ("step row", "take", …).
        what: &'static str,
        /// The rejected index.
        index: usize,
    },
    /// A value that clamping cannot repair (hue not in [`Hue::ALL`], meter denominator not a power
    /// of two, tempo outside the range, an instrument as effect, …).
    Invalid(&'static str),
}

impl fmt::Display for EditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EditError::IdsExhausted => write!(f, "project object identifiers exhausted"),
            EditError::TrackNotFound(id) => write!(f, "track {} not found", id.0),
            EditError::ClipNotFound(id) => write!(f, "clip {} not found", id.0),
            EditError::NoteNotFound(id) => write!(f, "note {} not found", id.0),
            EditError::DeviceNotFound(id) => write!(f, "device {} not found", id.0),
            EditError::SendNotFound(id) => write!(f, "send {} not found", id.0),
            EditError::NotABus(id) => write!(f, "track {} is not a bus", id.0),
            EditError::RoutingCycle => write!(f, "the routing would have a cycle"),
            EditError::WrongClipKind { clip, expected } => {
                write!(f, "clip {} is not a {expected:?} clip", clip.0)
            }
            EditError::IndexOutOfRange { what, index } => write!(f, "{what} {index} out of range"),
            EditError::Invalid(reason) => write!(f, "invalid edit: {reason}"),
        }
    }
}

impl std::error::Error for EditError {}

/// Result of an edit that returns only the new root.
pub type EditResult = Result<Project, EditError>;

// ------------------------------------------------------------------------------------------------
// Argument types
// ------------------------------------------------------------------------------------------------

/// A project-level property.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectProp {
    /// Project name (non-empty after trimming, else `Invalid`).
    Name(String),
    /// Tempo; outside [`Tempo::MIN_BPM`] ..= [`Tempo::MAX_BPM`] is `Invalid`.
    Tempo(Tempo),
    /// Meter; `num` 1 ..= 32 and `den` in {1, 2, 4, 8, 16}, else `Invalid`.
    Meter(Meter),
    /// Loop range; an empty range is `Invalid`, a start before 0 is clamped to 0. Apply with
    /// [`crate::Document::apply_untracked`] when set from the ruler.
    LoopRange(TickRange),
    /// Loop on/off. Apply with [`crate::Document::apply_untracked`].
    LoopOn(bool),
    /// Metronome on/off. Apply with [`crate::Document::apply_untracked`].
    Metronome(bool),
    /// Count-in bars, clamped to 0 ..= 4. Apply with [`crate::Document::apply_untracked`].
    CountInBars(u8),
}

/// What a new track gets.
#[derive(Debug, Clone, PartialEq)]
pub struct NewTrack {
    /// Name; `None` = the instrument's name (the sampler: "Drums"), or "MIDI" without instrument, made
    /// unique with [`unique_track_name`].
    pub name: Option<String>,
    /// Colour; `None` = [`next_hue`].
    pub hue: Option<Hue>,
    /// Instrument to put at `devices[0]`; `None` = no instrument.
    pub instrument: Option<PluginRef>,
    /// Display index to insert at; `None` = append at the end.
    pub index: Option<usize>,
    /// Arm the new track, disarm all others and switch its monitoring on (prototype `addTrack`, S10:
    /// an instrument dropped into the arrangement makes an armed track).
    pub arm_exclusive: bool,
}

/// A mixer value of a channel (track or master).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChannelProp {
    /// Fader level in dB, clamped to `VOLUME_DB_MIN ..= VOLUME_DB_MAX`; `-inf` becomes
    /// `VOLUME_DB_MIN`.
    VolumeDb(f32),
    /// Balance, clamped to `-1.0 ..= 1.0`.
    Pan(f32),
    /// Mute.
    Mute(bool),
    /// Solo (ignored on the master).
    Solo(bool),
}

/// A track property; applies to every track in the id list (drag across toggles, Alt height).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackProp {
    /// Rename (trimmed; empty is `Invalid`).
    Name(String),
    /// Colour; must be one of [`Hue::ALL`].
    Hue(Hue),
    /// A mixer value of the track's channel.
    Channel(ChannelProp),
    /// Record arm. `Arm(true)` also switches monitoring on (S02). Ignored on buses.
    Arm(bool),
    /// Input monitoring. Ignored on buses.
    Monitor(bool),
    /// MIDI input.
    Input(MidiInputSel),
    /// MIDI channel; `Channel(c)` with `c > 15` is `Invalid`.
    MidiChannel(MidiChannelSel),
    /// Header height, clamped to `TRACK_HEIGHT_MIN ..= TRACK_HEIGHT_MAX`.
    Height(u16),
    /// Fold or unfold a group in the arrangement; ignored on other tracks and on effect buses.
    Folded(bool),
}

/// Alt-click toggles: only this track has the flag afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExclusiveFlag {
    /// Solo only this track.
    Solo,
    /// Arm only this track (and switch its monitoring on).
    Arm,
}

/// A note to create; ids are allocated by the edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NewNote {
    /// Clip-local start, clamped to ≥ 0.
    pub start: Tick,
    /// Length, clamped to ≥ 1 tick.
    pub length: Tick,
    /// Pitch, clamped to `0 ..= 127`.
    pub pitch: u8,
    /// Velocity, clamped to `1 ..= 127`.
    pub velocity: u8,
}

/// Name and note of a step row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StepRowSpec {
    /// Row name ("Kick", or a CLAP note name).
    pub name: String,
    /// MIDI note the row plays.
    pub note: u8,
}

/// Content of a new clip.
#[derive(Debug, Clone, PartialEq)]
pub enum NewClipContent {
    /// A MIDI clip with one take holding these notes (empty = an empty clip; the piano roll then
    /// preselects the pencil).
    Midi {
        /// Initial notes, clip-local.
        notes: Vec<NewNote>,
        /// Name of the first take ("" for a drawn clip, "Take n" for a recording).
        take_name: String,
    },
    /// A step clip with these rows (empty = [`crate::project::DEFAULT_STEP_ROWS`]), 16 steps at 1/16,
    /// no swing, all steps off.
    Step {
        /// Row names and notes, top to bottom.
        rows: Vec<StepRowSpec>,
    },
}

/// What a new clip gets.
#[derive(Debug, Clone, PartialEq)]
pub struct NewClip {
    /// Timeline start, clamped to ≥ 0.
    pub start: Tick,
    /// Length, clamped to ≥ 1 tick (a new clip from a double-click is one bar).
    pub length: Tick,
    /// Clip name ("MIDI-Clip", "Pattern 2", "● Aufnahme"), chosen by the caller.
    pub name: String,
    /// Content.
    pub content: NewClipContent,
}

/// A clip property set from the inspector.
#[derive(Debug, Clone, PartialEq)]
pub enum ClipProp {
    /// Rename (trimmed; empty is `Invalid`).
    Name(String),
    /// Timeline start, clamped to ≥ 0 (keeps the clip on its track).
    Start(Tick),
    /// Transposition in semitones, clamped to `-48 ..= 48`.
    Transpose(i8),
    /// Velocity change in percent, clamped to `-100 ..= 100`.
    VelocityPct(i8),
}

/// How [`set_notes_velocity`] changes velocities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VelocityChange {
    /// Every note gets this velocity (velocity lane), clamped to `1 ..= 127`.
    Set(u8),
    /// Every note's velocity moves by this amount (Ctrl-drag ↕), clamped per note to `1 ..= 127`.
    Offset(i16),
}

/// A property of one step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StepProp {
    /// On or off (click, paint drag). Turning on keeps the stored velocity, probability, ratchet.
    On(bool),
    /// Velocity, clamped to `1 ..= 127` (drag ↕, lane).
    Velocity(u8),
    /// Probability in percent, clamped to `0 ..= 100` (Alt-drag ↕).
    Probability(u8),
    /// Ratchet, clamped to `1 ..= 4` (Shift-click cycles 1 → 2 → 3 → 4 → 1; the caller computes the
    /// next value).
    Ratchet(u8),
}

/// A pattern property of a step clip.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StepClipProp {
    /// Pattern length in steps, clamped to `1 ..= 32`. Doubling from `n` to `2n` while steps
    /// `n .. 2n` of every row are off copies steps `0 .. n` into them (prototype `toggleLen`);
    /// otherwise hidden steps keep their values.
    LengthSteps(u8),
    /// Step resolution.
    Resolution(StepResolution),
    /// Swing, clamped to `0.0 ..= 0.75`.
    Swing(f32),
}

/// A property of a step row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StepRowProp {
    /// Row name.
    Name(String),
    /// MIDI note, clamped to `0 ..= 127`.
    Note(u8),
    /// Row mute.
    Mute(bool),
    /// Row solo.
    Solo(bool),
}

/// A device to insert; the id is allocated by the edit, bypass starts off.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDevice {
    /// MIDI effect (track chains only, before the instrument), instrument (right behind the MIDI
    /// effects) or audio effect.
    pub role: DeviceRole,
    /// The plugin.
    pub plugin: PluginRef,
    /// Initial state (`None` = plugin defaults; a preset from the browser passes its state).
    pub state: Option<Arc<[u8]>>,
    /// The sidechain of the device a copy is made of (copy, duplicate, paste; `None` for a new
    /// device). [`insert_device`] keeps it only while its source still exists and can feed the
    /// copy's chain without a cycle ([`crate::routing::sidechain_sources`]); otherwise the copy
    /// starts without one (no error).
    pub sidechain: Option<Sidechain>,
}

// ------------------------------------------------------------------------------------------------
// Helpers for callers
// ------------------------------------------------------------------------------------------------

/// The hue a new track gets: the first of [`Hue::AUTO_ORDER`] not used by any track; if all are
/// used, `Hue::ALL[track_count % 14]` (prototype `addTrack`).
pub fn next_hue(project: &Project) -> Hue {
    Hue::AUTO_ORDER
        .into_iter()
        .find(|hue| project.tracks.iter().all(|t| t.hue != *hue))
        .unwrap_or(Hue::ALL[project.tracks.len() % Hue::ALL.len()])
}

/// `base` if no track has that name, else `"{base} 2"`, `"{base} 3"`, … (first free).
pub fn unique_track_name(project: &Project, base: &str) -> String {
    let taken = |name: &str| project.tracks.iter().any(|t| t.name == name);
    if !taken(base) {
        return base.to_owned();
    }
    (2u64..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken(name))
        .expect("one of more names than tracks is free")
}

/// Number for the next recorded take: one more than the highest `n` of any clip or take named
/// `"Take n"` in the project, starting at 1. The caller formats the UI name "Take n".
pub fn next_take_number(project: &Project) -> u32 {
    fn number(name: &str) -> Option<u32> {
        let digits = name.strip_prefix("Take ")?;
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok()
    }
    project
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter())
        .flat_map(|c| {
            let takes = match &c.content {
                ClipContent::Midi(m) => m.takes.as_slice(),
                ClipContent::Step(_) => &[],
            };
            std::iter::once(c.name.as_str()).chain(takes.iter().map(|t| t.name.as_str()))
        })
        .filter_map(number)
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

// ------------------------------------------------------------------------------------------------
// Project
// ------------------------------------------------------------------------------------------------

/// Sets a project-level property.
pub fn set_project_prop(project: &Project, prop: ProjectProp) -> EditResult {
    let mut p = project.clone();
    match prop {
        ProjectProp::Name(name) => p.name = valid_name(&name)?,
        ProjectProp::Tempo(tempo) => {
            if !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&tempo.bpm) {
                return Err(EditError::Invalid("tempo out of range"));
            }
            p.tempo = tempo;
        }
        ProjectProp::Meter(meter) => {
            if !valid_meter(meter) {
                return Err(EditError::Invalid("meter not supported"));
            }
            p.meter = meter;
        }
        ProjectProp::LoopRange(range) => {
            let range = TickRange::new(range.start.max(Tick::ZERO), range.end);
            if range.is_empty() {
                return Err(EditError::Invalid("empty loop range"));
            }
            p.loop_range = range;
        }
        ProjectProp::LoopOn(on) => p.loop_on = on,
        ProjectProp::Metronome(on) => p.metronome = on,
        ProjectProp::CountInBars(bars) => p.count_in_bars = bars.min(MAX_COUNT_IN_BARS),
    }
    Ok(p)
}

/// Sets volume, pan or mute of the master strip.
pub fn set_master_prop(project: &Project, prop: ChannelProp) -> EditResult {
    if let ChannelProp::Solo(_) = prop {
        return Ok(project.clone());
    }
    let mut p = project.clone();
    apply_channel_prop(Arc::make_mut(&mut p.master), prop)?;
    Ok(p)
}

// ------------------------------------------------------------------------------------------------
// Tracks
// ------------------------------------------------------------------------------------------------

/// Adds a MIDI track (Ctrl T, instrument drop). Height [`crate::project::TRACK_HEIGHT_DEFAULT`],
/// 0 dB, centred, input "all", Omni. Inserted before a group member, it joins that member's group
/// (outputs to the group bus); elsewhere it outputs to the master.
pub fn add_track(project: &Project, spec: NewTrack) -> Result<(Project, TrackId), EditError> {
    add_track_in(project, spec, &DefaultNames::DE)
}

/// The words default names are made of (`MIDI 2`, `Bus 1`, `Gruppe 1`, `Drums`). The model has no
/// language: it writes German unless the caller passes the words of its own ([`add_track_in`],
/// [`add_bus_in`]). Once made, a name is content and never changes with the language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultNames {
    /// Base of a track without instrument: „MIDI“, „MIDI 2“, …
    pub track: &'static str,
    /// Name of a track with the drum sampler.
    pub drums: &'static str,
    /// Base of an effect bus: „Bus 1“, …
    pub bus: &'static str,
    /// Base of a group: „Gruppe 1“, …
    pub group: &'static str,
}

impl DefaultNames {
    /// German: `MIDI`, `Drums`, `Bus`, `Gruppe`.
    pub const DE: DefaultNames = DefaultNames {
        track: DEFAULT_TRACK_NAME,
        drums: DRUMS_TRACK_NAME,
        bus: DEFAULT_BUS_NAME,
        group: DEFAULT_GROUP_NAME,
    };
}

impl Default for DefaultNames {
    fn default() -> Self {
        DefaultNames::DE
    }
}

/// [`add_track`] with the name bases of `names` for a track without a name.
pub fn add_track_in(
    project: &Project,
    spec: NewTrack,
    names: &DefaultNames,
) -> Result<(Project, TrackId), EditError> {
    let hue = match spec.hue {
        Some(hue) => valid_hue(hue)?,
        None => next_hue(project),
    };
    let name = match &spec.name {
        Some(name) => valid_name(name)?,
        None => {
            let base = match &spec.instrument {
                Some(plugin) if is_drum_sampler(plugin) => names.drums,
                Some(plugin) if !plugin.name.trim().is_empty() => plugin.name.trim(),
                _ => names.track,
            };
            unique_track_name(project, base)
        }
    };
    let mut p = project.clone();
    let id = TrackId(p.alloc_id()?);
    let mut channel = Channel::default();
    if let Some(plugin) = spec.instrument {
        channel.devices.push(Arc::new(Device {
            id: DeviceId(p.alloc_id()?),
            role: DeviceRole::Instrument,
            plugin,
            bypass: false,
            state: None,
            sidechain: None,
        }));
    }
    if spec.arm_exclusive {
        for index in 0..p.tracks.len() {
            update_track(&mut p, index, |t| {
                t.arm = false;
                Ok(())
            })?;
        }
    }
    let index = spec.index.unwrap_or(p.tracks.len()).min(p.tracks.len());
    let group = p
        .tracks
        .get(index)
        .and_then(|t| crate::routing::group_of(&p, t.id));
    let track = Track {
        id,
        name,
        hue,
        kind: TrackKind::Midi,
        channel,
        arm: spec.arm_exclusive,
        monitor: spec.arm_exclusive,
        input: MidiInputSel::All,
        midi_channel: MidiChannelSel::Omni,
        height: TRACK_HEIGHT_DEFAULT,
        clips: Vec::new(),
        output: group.map_or(TrackOutput::Master, TrackOutput::Bus),
        sends: Vec::new(),
        folder: false,
        folded: false,
    };
    p.tracks.insert(index, Arc::new(track));
    crate::routing::normalize_groups(&mut p);
    Ok((p, id))
}

/// Removes tracks and buses with all their clips and devices (Entf with tracks selected), and
/// every routing reference to them in the same step: outputs to a removed bus go where that bus
/// went (deleting a group dissolves it; its members join the group it was in, or the top level),
/// sends to it are removed, sidechains from a removed track are cleared (ARCHITECTURE §6.8.3).
pub fn remove_tracks(project: &Project, tracks: &[TrackId]) -> EditResult {
    for &id in tracks {
        locate_track(project, id)?;
    }
    let removed: HashSet<TrackId> = tracks.iter().copied().collect();
    let mut p = project.clone();
    p.tracks.retain(|t| !removed.contains(&t.id));
    routes::detach_removed(&mut p, project, &removed);
    crate::routing::normalize_groups(&mut p);
    Ok(p)
}

/// Moves tracks (keeping their relative order) so the first of them lands at display index
/// `to_index` of the list *without* the moved tracks (drag reorder, 2 px insert line). `to_index`
/// is clamped to the list end. Group membership does not change (that is the output,
/// [`set_output`]): a moved group takes its members along, and a member stays in its group's
/// block.
pub fn move_tracks(project: &Project, tracks: &[TrackId], to_index: usize) -> EditResult {
    for &id in tracks {
        locate_track(project, id)?;
    }
    let (moved, mut rest): (Vec<_>, Vec<_>) = project
        .tracks
        .iter()
        .cloned()
        .partition(|t| tracks.contains(&t.id));
    let at = to_index.min(rest.len());
    rest.splice(at..at, moved);
    let mut p = project.clone();
    p.tracks = rest;
    crate::routing::normalize_groups(&mut p);
    Ok(p)
}

/// Sets a property on every listed track.
pub fn set_track_prop(project: &Project, tracks: &[TrackId], prop: TrackProp) -> EditResult {
    let indices = tracks
        .iter()
        .map(|&id| locate_track(project, id))
        .collect::<Result<Vec<_>, _>>()?;
    let prop = match prop {
        TrackProp::Name(name) => TrackProp::Name(valid_name(&name)?),
        TrackProp::Hue(hue) => TrackProp::Hue(valid_hue(hue)?),
        TrackProp::MidiChannel(MidiChannelSel::Channel(c)) if c > 15 => {
            return Err(EditError::Invalid("MIDI channel out of range"));
        }
        prop => prop,
    };
    let mut p = project.clone();
    for index in indices {
        update_track(&mut p, index, |t| apply_track_prop(t, &prop))?;
    }
    Ok(p)
}

/// Alt-click on S or R: the flag is on for `track` and off for all others.
pub fn set_exclusive(project: &Project, track: TrackId, flag: ExclusiveFlag) -> EditResult {
    locate_track(project, track)?;
    let mut p = project.clone();
    for index in 0..p.tracks.len() {
        let this = p.tracks[index].id == track;
        update_track(&mut p, index, |t| {
            match flag {
                ExclusiveFlag::Solo => t.channel.solo = this,
                ExclusiveFlag::Arm => {
                    let this = this && !t.is_bus();
                    t.arm = this;
                    t.monitor |= this;
                }
            }
            Ok(())
        })?;
    }
    Ok(p)
}

// ------------------------------------------------------------------------------------------------
// Clips
// ------------------------------------------------------------------------------------------------

/// Adds a clip to a track (double-click on empty lane, recording start).
pub fn add_clip(
    project: &Project,
    track: TrackId,
    spec: NewClip,
) -> Result<(Project, ClipId), EditError> {
    let track_index = locate_track(project, track)?;
    if project.tracks[track_index].is_bus() {
        return Err(EditError::Invalid("a bus holds no clips"));
    }
    let mut p = project.clone();
    let id = ClipId(p.alloc_id()?);
    let content = match spec.content {
        NewClipContent::Midi { notes, take_name } => {
            let mut notes: Vec<Note> = notes
                .iter()
                .map(|n| Ok(new_note(NoteId(p.alloc_id()?), n)))
                .collect::<Result<_, EditError>>()?;
            sort_notes(&mut notes);
            ClipContent::Midi(MidiClip {
                takes: vec![Take {
                    name: take_name,
                    notes: Arc::new(notes),
                }],
                active_take: 0,
            })
        }
        NewClipContent::Step { rows } => ClipContent::Step(new_step_clip(&rows)),
    };
    let clip = Clip {
        id,
        name: spec.name,
        start: spec.start.max(Tick::ZERO),
        length: spec.length.max(MIN_LENGTH),
        loop_length: None,
        mute: false,
        transpose: 0,
        velocity_pct: 0,
        content,
    };
    insert_clip(track_mut(&mut p, track_index), Arc::new(clip));
    Ok((p, id))
}

/// Removes clips (Entf).
pub fn remove_clips(project: &Project, clips: &[ClipId]) -> EditResult {
    for &id in clips {
        locate_clip(project, id)?;
    }
    let mut p = project.clone();
    for index in 0..p.tracks.len() {
        if p.tracks[index].clips.iter().any(|c| clips.contains(&c.id)) {
            track_mut(&mut p, index)
                .clips
                .retain(|c| !clips.contains(&c.id));
        }
    }
    Ok(p)
}

/// Moves clips by `delta` ticks and `track_delta` tracks (drag; negative = up). The group moves
/// together: `delta` is clamped so no clip starts before tick 0 and `track_delta` so no clip leaves
/// the track list.
pub fn move_clips(
    project: &Project,
    clips: &[ClipId],
    delta: Tick,
    track_delta: i32,
) -> EditResult {
    let group = ClipGroup::new(project, clips, delta, track_delta)?;
    if group.delta == Tick::ZERO && group.track_delta == 0 {
        return Ok(project.clone());
    }
    let mut p = project.clone();
    for index in 0..p.tracks.len() {
        if p.tracks[index]
            .clips
            .iter()
            .any(|c| group.clips.iter().any(|(_, g)| g.id == c.id))
        {
            track_mut(&mut p, index)
                .clips
                .retain(|c| group.clips.iter().all(|(_, g)| g.id != c.id));
        }
    }
    for (track_index, clip) in &group.clips {
        let mut clip = Clip::clone(clip);
        clip.start += group.delta;
        insert_clip(
            track_mut(&mut p, group.target(*track_index)),
            Arc::new(clip),
        );
    }
    Ok(p)
}

/// Alt-drag: copies clips (new clip and note ids) to the moved position, originals stay. Same
/// clamping as [`move_clips`]. Returns the copies in the order of `clips`.
pub fn copy_clips(
    project: &Project,
    clips: &[ClipId],
    delta: Tick,
    track_delta: i32,
) -> Result<(Project, Vec<ClipId>), EditError> {
    let group = ClipGroup::new(project, clips, delta, track_delta)?;
    let mut p = project.clone();
    let mut copies = Vec::with_capacity(group.clips.len());
    for (track_index, clip) in &group.clips {
        let mut copy = copy_clip(&mut p, clip)?;
        copy.start += group.delta;
        copies.push(copy.id);
        insert_clip(
            track_mut(&mut p, group.target(*track_index)),
            Arc::new(copy),
        );
    }
    Ok((p, copies))
}

/// Ctrl D: copies each clip right behind itself (`start + length`) on the same track. Returns the
/// copies in the order of `clips`.
pub fn duplicate_clips(
    project: &Project,
    clips: &[ClipId],
) -> Result<(Project, Vec<ClipId>), EditError> {
    let located = unique(clips)
        .into_iter()
        .map(|id| {
            let (track_index, clip_index) = locate_clip(project, id)?;
            Ok((
                track_index,
                Arc::clone(&project.tracks[track_index].clips[clip_index]),
            ))
        })
        .collect::<Result<Vec<_>, EditError>>()?;
    let mut p = project.clone();
    let mut copies = Vec::with_capacity(located.len());
    for (track_index, clip) in located {
        let mut copy = copy_clip(&mut p, &clip)?;
        copy.start = clip.end();
        copies.push(copy.id);
        insert_clip(track_mut(&mut p, track_index), Arc::new(copy));
    }
    Ok((p, copies))
}

/// Sets the timeline length (right edge drag), clamped to ≥ 1 tick. The start never moves; content
/// beyond the new end is kept but not played.
pub fn set_clip_length(project: &Project, clip: ClipId, length: Tick) -> EditResult {
    edit_clip(project, clip, |c| {
        c.length = length.max(MIN_LENGTH);
        Ok(())
    })
}

/// Sets the clip loop (top-right corner drag, inspector "Loop" / "Loop-Länge"): `loop_length`
/// `Some(l)` with `l ≥ 1` repeats `0 .. l`; `None` turns looping off. `length` is the new timeline
/// length (≥ 1). Step clips accept only `None` (`WrongClipKind` otherwise) and just take the length.
pub fn set_clip_loop(
    project: &Project,
    clip: ClipId,
    loop_length: Option<Tick>,
    length: Tick,
) -> EditResult {
    edit_clip(project, clip, |c| {
        if loop_length.is_some() && c.kind() == ClipKind::Step {
            return Err(EditError::WrongClipKind {
                clip: c.id,
                expected: ClipKind::Midi,
            });
        }
        c.loop_length = loop_length.map(|l| l.max(MIN_LENGTH));
        c.length = length.max(MIN_LENGTH);
        Ok(())
    })
}

/// Ctrl E: splits every listed clip that strictly contains `at` (timeline tick) into a left part
/// (same id, ends at `at`) and a right part (new id, starts at `at`). Clips not containing `at` are
/// skipped. MIDI: notes starting before `at` stay left (cut at `at`), the others move right; a looped
/// clip is unrolled first, so both parts are unlooped. Step: both parts keep the pattern; the right
/// part's rows are rotated so the audible result does not change. Returns the right parts.
pub fn split_clips(
    project: &Project,
    clips: &[ClipId],
    at: Tick,
) -> Result<(Project, Vec<ClipId>), EditError> {
    let ids = unique(clips);
    for &id in &ids {
        locate_clip(project, id)?;
    }
    let mut p = project.clone();
    let mut rights = Vec::new();
    for id in ids {
        // Look up again: right parts inserted before may have shifted the clip's index.
        let (track_index, clip_index) = locate_clip(&p, id)?;
        let clip = Arc::clone(&p.tracks[track_index].clips[clip_index]);
        if !(clip.start < at && at < clip.end()) {
            continue;
        }
        let (left, right) = split_clip(&mut p, &clip, at - clip.start)?;
        rights.push(right.id);
        let track = track_mut(&mut p, track_index);
        // Same id and start: the left part keeps the clip's place in the order.
        track.clips[clip_index] = Arc::new(left);
        insert_clip(track, Arc::new(right));
    }
    Ok((p, rights))
}

/// Key `0`: mutes or unmutes clips.
pub fn set_clips_muted(project: &Project, clips: &[ClipId], muted: bool) -> EditResult {
    for &id in clips {
        locate_clip(project, id)?;
    }
    let mut p = project.clone();
    for &id in clips {
        let location = locate_clip(&p, id)?;
        update_clip(&mut p, location, |c| {
            c.mute = muted;
            Ok(())
        })?;
    }
    Ok(p)
}

/// Sets a clip property from the inspector.
pub fn set_clip_prop(project: &Project, clip: ClipId, prop: ClipProp) -> EditResult {
    let prop = match prop {
        ClipProp::Name(name) => ClipProp::Name(valid_name(&name)?),
        prop => prop,
    };
    edit_clip(project, clip, |c| {
        match prop {
            ClipProp::Name(name) => c.name = name,
            ClipProp::Start(start) => c.start = start.max(Tick::ZERO),
            ClipProp::Transpose(semitones) => c.transpose = semitones.clamp(-48, 48),
            ClipProp::VelocityPct(percent) => c.velocity_pct = percent.clamp(-100, 100),
        }
        Ok(())
    })
}

// ------------------------------------------------------------------------------------------------
// Notes (active take of a MIDI clip)
// ------------------------------------------------------------------------------------------------

/// Adds notes (pencil, double-click, recording, paste). Returns their ids in input order.
pub fn add_notes(
    project: &Project,
    clip: ClipId,
    notes: &[NewNote],
) -> Result<(Project, Vec<NoteId>), EditError> {
    let mut p = project.clone();
    let ids: Vec<NoteId> = notes
        .iter()
        .map(|_| p.alloc_id().map(NoteId))
        .collect::<Result<_, _>>()?;
    with_notes(&mut p, clip, |list| {
        list.extend(notes.iter().zip(&ids).map(|(n, &id)| new_note(id, n)));
        Ok(())
    })?;
    Ok((p, ids))
}

/// Deletes notes (Entf, eraser, pointer double-click).
pub fn remove_notes(project: &Project, clip: ClipId, notes: &[NoteId]) -> EditResult {
    let selection = NoteSelection::new(notes);
    let mut p = project.clone();
    with_notes(&mut p, clip, |list| {
        selection.check(list)?;
        list.retain(|n| !selection.contains(n.id));
        Ok(())
    })?;
    Ok(p)
}

/// Moves notes in time and pitch (drag, ← →, ↑ ↓, Shift ↑ ↓ = ±12). The group moves together:
/// `delta` is clamped so no note starts before clip-local 0, `pitch_delta` so all stay in
/// `0 ..= 127`.
pub fn move_notes(
    project: &Project,
    clip: ClipId,
    notes: &[NoteId],
    delta: Tick,
    pitch_delta: i32,
) -> EditResult {
    let selection = NoteSelection::new(notes);
    let mut p = project.clone();
    with_notes(&mut p, clip, |list| {
        selection.check(list)?;
        let (delta, pitch_delta) = clamp_note_move(list, &selection, delta, pitch_delta);
        for n in list.iter_mut().filter(|n| selection.contains(n.id)) {
            n.start += delta;
            n.pitch = shift_pitch(n.pitch, pitch_delta);
        }
        Ok(())
    })?;
    Ok(p)
}

/// Alt-drag: copies notes to the moved position (same clamping as [`move_notes`]); originals stay.
/// Returns the copies in the order of `notes`.
pub fn copy_notes(
    project: &Project,
    clip: ClipId,
    notes: &[NoteId],
    delta: Tick,
    pitch_delta: i32,
) -> Result<(Project, Vec<NoteId>), EditError> {
    let selection = NoteSelection::new(notes);
    let order = unique(notes);
    let mut p = project.clone();
    let ids: Vec<NoteId> = order
        .iter()
        .map(|_| p.alloc_id().map(NoteId))
        .collect::<Result<_, _>>()?;
    with_notes(&mut p, clip, |list| {
        selection.check(list)?;
        let (delta, pitch_delta) = clamp_note_move(list, &selection, delta, pitch_delta);
        let copies: Vec<Note> = order
            .iter()
            .zip(&ids)
            .filter_map(|(original, &id)| {
                let n = list.iter().find(|n| n.id == *original)?;
                Some(Note {
                    id,
                    start: n.start + delta,
                    pitch: shift_pitch(n.pitch, pitch_delta),
                    ..*n
                })
            })
            .collect();
        list.extend(copies);
        Ok(())
    })?;
    Ok((p, ids))
}

/// Edge drag: changes the length of all listed notes by `delta` ticks, each clamped to ≥ 1 tick.
pub fn resize_notes(project: &Project, clip: ClipId, notes: &[NoteId], delta: Tick) -> EditResult {
    let selection = NoteSelection::new(notes);
    let mut p = project.clone();
    with_notes(&mut p, clip, |list| {
        selection.check(list)?;
        for n in list.iter_mut().filter(|n| selection.contains(n.id)) {
            n.length = (n.length + delta).max(MIN_LENGTH);
        }
        Ok(())
    })?;
    Ok(p)
}

/// Changes velocities (Ctrl-drag ↕, velocity lane).
pub fn set_notes_velocity(
    project: &Project,
    clip: ClipId,
    notes: &[NoteId],
    change: VelocityChange,
) -> EditResult {
    let selection = NoteSelection::new(notes);
    let mut p = project.clone();
    with_notes(&mut p, clip, |list| {
        selection.check(list)?;
        for n in list.iter_mut().filter(|n| selection.contains(n.id)) {
            n.velocity = match change {
                VelocityChange::Set(v) => v.clamp(1, 127),
                VelocityChange::Offset(d) => (i16::from(n.velocity) + d).clamp(1, 127) as u8,
            };
        }
        Ok(())
    })?;
    Ok(p)
}

/// Key `Q`: snaps start and length of the listed notes (`None` = all notes of the take) to `grid`
/// ticks: `start = round(start / grid) × grid`, `length = max(grid, round(length / grid) × grid)`
/// (prototype `quantize`).
pub fn quantize_notes(
    project: &Project,
    clip: ClipId,
    notes: Option<&[NoteId]>,
    grid: Tick,
) -> EditResult {
    if grid <= Tick::ZERO {
        return Err(EditError::Invalid("grid must be positive"));
    }
    let selection = notes.map(NoteSelection::new);
    let mut p = project.clone();
    with_notes(&mut p, clip, |list| {
        if let Some(selection) = &selection {
            selection.check(list)?;
        }
        let selected = |n: &Note| selection.as_ref().is_none_or(|s| s.contains(n.id));
        for n in list.iter_mut().filter(|n| selected(n)) {
            n.start = round_to(n.start, grid);
            n.length = round_to(n.length, grid).max(grid);
        }
        Ok(())
    })?;
    Ok(p)
}

// ------------------------------------------------------------------------------------------------
// Steps
// ------------------------------------------------------------------------------------------------

/// Sets a property of step `step` (`0 .. 32`) in row `row` of a step clip.
pub fn set_step(
    project: &Project,
    clip: ClipId,
    row: usize,
    step: usize,
    prop: StepProp,
) -> EditResult {
    with_step_clip(project, clip, |s| {
        let row = s.rows.get_mut(row).ok_or(EditError::IndexOutOfRange {
            what: "step row",
            index: row,
        })?;
        let step = row.steps.get_mut(step).ok_or(EditError::IndexOutOfRange {
            what: "step",
            index: step,
        })?;
        match prop {
            StepProp::On(on) => step.on = on,
            StepProp::Velocity(v) => step.velocity = v.clamp(1, 127),
            StepProp::Probability(p) => step.probability = p.min(100),
            StepProp::Ratchet(r) => step.ratchet = r.clamp(1, 4),
        }
        Ok(())
    })
}

/// Sets a pattern property of a step clip (Länge, Auflösung, Swing).
pub fn set_step_clip_prop(project: &Project, clip: ClipId, prop: StepClipProp) -> EditResult {
    if let StepClipProp::Swing(swing) = prop
        && swing.is_nan()
    {
        return Err(EditError::Invalid("swing is not a number"));
    }
    with_step_clip(project, clip, |s| {
        match prop {
            StepClipProp::LengthSteps(steps) => {
                let old = usize::from(s.length_steps);
                let new = usize::from(steps.clamp(1, MAX_STEPS as u8));
                let hidden_off = |r: &StepRow| r.steps[old..new].iter().all(|st| !st.on);
                if new == 2 * old && s.rows.iter().all(hidden_off) {
                    for row in &mut s.rows {
                        row.steps.copy_within(..old, old);
                    }
                }
                s.length_steps = new as u8;
            }
            StepClipProp::Resolution(resolution) => s.resolution = resolution,
            StepClipProp::Swing(swing) => s.swing = swing.clamp(0.0, MAX_SWING),
        }
        Ok(())
    })
}

/// Sets a property of step row `row`.
pub fn set_step_row(project: &Project, clip: ClipId, row: usize, prop: StepRowProp) -> EditResult {
    let prop = match prop {
        StepRowProp::Name(name) => StepRowProp::Name(valid_name(&name)?),
        prop => prop,
    };
    with_step_clip(project, clip, |s| {
        let r = s.rows.get_mut(row).ok_or(EditError::IndexOutOfRange {
            what: "step row",
            index: row,
        })?;
        match prop {
            StepRowProp::Name(name) => r.name = name,
            StepRowProp::Note(note) => r.note = note.min(127),
            StepRowProp::Mute(mute) => r.mute = mute,
            StepRowProp::Solo(solo) => r.solo = solo,
        }
        Ok(())
    })
}

/// Replaces the row names and notes of a step clip (instrument changed, CLAP note names arrived).
/// Rows keep their steps by index; extra rows start empty, missing rows are dropped.
pub fn set_step_rows(project: &Project, clip: ClipId, rows: &[StepRowSpec]) -> EditResult {
    with_step_clip(project, clip, |s| {
        s.rows.truncate(rows.len());
        for (index, spec) in rows.iter().enumerate() {
            match s.rows.get_mut(index) {
                Some(row) => {
                    row.name.clone_from(&spec.name);
                    row.note = spec.note.min(127);
                }
                None => s.rows.push(new_step_row(&spec.name, spec.note)),
            }
        }
        Ok(())
    })
}

/// "Als MIDI-Clip": turns a step clip into a MIDI clip with the same id, name, start and length.
/// The notes are what the step clip plays (pattern repeated over the clip length, swing and
/// ratchets applied, muted and non-soloed rows left out). Steps with probability 0 are left out;
/// other probabilities are dropped (the notes always play).
pub fn step_clip_to_midi(project: &Project, clip: ClipId) -> EditResult {
    let location = locate_clip(project, clip)?;
    let source = &project.tracks[location.0].clips[location.1];
    let ClipContent::Step(steps) = &source.content else {
        return Err(EditError::WrongClipKind {
            clip,
            expected: ClipKind::Step,
        });
    };
    let hits = steps.hits(source.length);
    let mut p = project.clone();
    let notes: Vec<Note> = hits
        .into_iter()
        .filter(|h| h.probability > 0)
        .map(|h| {
            Ok(Note {
                id: NoteId(p.alloc_id()?),
                start: h.start,
                length: h.length,
                pitch: h.note,
                velocity: h.velocity,
            })
        })
        .collect::<Result<_, EditError>>()?;
    update_clip(&mut p, location, |c| {
        c.loop_length = None;
        c.content = ClipContent::Midi(MidiClip {
            takes: vec![Take {
                name: String::new(),
                notes: Arc::new(notes),
            }],
            active_take: 0,
        });
        Ok(())
    })?;
    Ok(p)
}

// ------------------------------------------------------------------------------------------------
// Devices
// ------------------------------------------------------------------------------------------------

/// Inserts a device into a chain at `index` (clamped to the chain length). The chain keeps its
/// order `[MIDI effects…] → instrument → [effects…]`: a MIDI effect goes into the MIDI part (index
/// clamped to it), an instrument right behind the MIDI effects (only when the track has none), an
/// audio effect behind the instrument. Master chains take audio effects only. A copy's
/// sidechain ([`NewDevice::sidechain`]) is kept if its source can feed this chain, else dropped.
pub fn insert_device(
    project: &Project,
    channel: ChannelRef,
    index: usize,
    spec: NewDevice,
) -> Result<(Project, DeviceId), EditError> {
    let chain: &Channel = match channel {
        ChannelRef::Track(id) => {
            let track = &project.tracks[locate_track(project, id)?];
            if track.is_bus() && spec.role != DeviceRole::Effect {
                return Err(EditError::Invalid("a bus takes audio effects only"));
            }
            &track.channel
        }
        ChannelRef::Master => {
            if spec.role != DeviceRole::Effect {
                return Err(EditError::Invalid("the master takes effects only"));
            }
            &project.master
        }
    };
    let midi = chain.midi_effect_count();
    let index = index.min(chain.devices.len());
    let index = match spec.role {
        DeviceRole::MidiEffect => index.min(midi),
        DeviceRole::Instrument if chain.instrument().is_some() => {
            return Err(EditError::Invalid("the track already has an instrument"));
        }
        DeviceRole::Instrument if index != midi => {
            return Err(EditError::Invalid(
                "an instrument goes right behind the MIDI effects",
            ));
        }
        DeviceRole::Instrument => midi,
        DeviceRole::Effect => index.max(chain.first_effect_index()),
    };
    let mut p = project.clone();
    let id = DeviceId(p.alloc_id()?);
    channel_mut(&mut p, channel)?.devices.insert(
        index,
        Arc::new(Device {
            id,
            role: spec.role,
            plugin: spec.plugin,
            bypass: false,
            state: spec.state,
            sidechain: None,
        }),
    );
    // The copy's source must still exist and must not lie downstream of the new holder (#7).
    if let Some(sc) = spec.sidechain
        && crate::routing::sidechain_sources(&p, id).contains(&sc.source)
    {
        let (_, at, _) = p.find_device(id).expect("just inserted");
        Arc::make_mut(&mut channel_mut(&mut p, channel)?.devices[at]).sidechain = Some(sc);
    }
    Ok((p, id))
}

/// Puts `plugin` as the instrument of `track` (browser drop on a track): replaces the existing
/// instrument (new device id, state `None`) or inserts one behind the MIDI effects.
pub fn replace_instrument(
    project: &Project,
    track: TrackId,
    plugin: PluginRef,
) -> Result<(Project, DeviceId), EditError> {
    let track_index = locate_track(project, track)?;
    if project.tracks[track_index].is_bus() {
        return Err(EditError::Invalid("a bus takes audio effects only"));
    }
    let mut p = project.clone();
    let id = DeviceId(p.alloc_id()?);
    let device = Arc::new(Device {
        id,
        role: DeviceRole::Instrument,
        plugin,
        bypass: false,
        state: None,
        sidechain: None,
    });
    let channel = &mut track_mut(&mut p, track_index).channel;
    match channel.instrument() {
        Some((index, _)) => channel.devices[index] = device,
        None => {
            let index = channel.midi_effect_count();
            channel.devices.insert(index, device);
        }
    }
    Ok((p, id))
}

/// Removes a device from its chain.
pub fn remove_device(project: &Project, device: DeviceId) -> EditResult {
    let (channel, index, _) = project
        .find_device(device)
        .ok_or(EditError::DeviceNotFound(device))?;
    let mut p = project.clone();
    channel_mut(&mut p, channel)?.devices.remove(index);
    Ok(p)
}

/// Moves a MIDI effect or an audio effect within its part of the chain to `to_index` of the chain
/// without it (drag reorder). An instrument cannot move; an effect cannot go before the instrument
/// and a MIDI effect cannot go behind it (`Invalid`).
pub fn move_device(project: &Project, device: DeviceId, to_index: usize) -> EditResult {
    let (channel, index, found) = project
        .find_device(device)
        .ok_or(EditError::DeviceNotFound(device))?;
    if found.role == DeviceRole::Instrument {
        return Err(EditError::Invalid("an instrument cannot move"));
    }
    let role = found.role;
    let mut p = project.clone();
    let channel = channel_mut(&mut p, channel)?;
    let moved = channel.devices.remove(index);
    let to_index = to_index.min(channel.devices.len());
    match role {
        DeviceRole::MidiEffect if to_index > channel.midi_effect_count() => {
            return Err(EditError::Invalid(
                "a MIDI effect cannot go behind the instrument",
            ));
        }
        DeviceRole::Effect if to_index < channel.first_effect_index() => {
            return Err(EditError::Invalid(
                "an effect cannot go before the instrument",
            ));
        }
        _ => {}
    }
    channel.devices.insert(to_index, moved);
    Ok(p)
}

/// Sets bypass (switch on the device card, Alt-click on an insert slot).
pub fn set_device_bypass(project: &Project, device: DeviceId, bypass: bool) -> EditResult {
    edit_device(project, device, |d| d.bypass = bypass)
}

/// Stores a device state: a parameter value, a preset, a sample link, a plugin's own snapshot.
/// A tracked edit (one undo step per gesture). States that must not enter the history go through
/// [`crate::Document::rewrite_untracked`] (sample paths rewritten on save, per step) or
/// [`crate::Document::apply_untracked_to_root`] (the echo of an undo, a baseline taken for the
/// file), never through [`crate::Document::apply_untracked`], which would give every step the
/// same state.
pub fn set_device_state(
    project: &Project,
    device: DeviceId,
    state: Option<Arc<[u8]>>,
) -> EditResult {
    edit_device(project, device, |d| d.state = state)
}

// ------------------------------------------------------------------------------------------------
// Takes
// ------------------------------------------------------------------------------------------------

/// Appends an empty take named `name` to a MIDI clip and makes it active (loop recording: one take
/// per pass). Returns the take index.
pub fn add_take(
    project: &Project,
    clip: ClipId,
    name: String,
) -> Result<(Project, usize), EditError> {
    let (p, index) = edit_clip_with(project, clip, |c| {
        let m = midi_mut(c)?;
        m.takes.push(Take {
            name,
            notes: Arc::new(Vec::new()),
        });
        m.active_take = m.takes.len() - 1;
        Ok(m.active_take)
    })?;
    Ok((p, index))
}

/// Makes take `take` the active one (inspector take list).
pub fn select_take(project: &Project, clip: ClipId, take: usize) -> EditResult {
    edit_clip(project, clip, |c| {
        let m = midi_mut(c)?;
        if take >= m.takes.len() {
            return Err(EditError::IndexOutOfRange {
                what: "take",
                index: take,
            });
        }
        m.active_take = take;
        Ok(())
    })
}

// ------------------------------------------------------------------------------------------------
// Private helpers
// ------------------------------------------------------------------------------------------------

/// Shortest clip or note length.
const MIN_LENGTH: Tick = Tick(1);
/// Longest count-in in bars (the edit clamps to it).
pub const MAX_COUNT_IN_BARS: u8 = 4;
/// Default name of a track with the drum sampler (prototype `addTrack`).
const DRUMS_TRACK_NAME: &str = "Drums";
/// Default name of a track without instrument.
const DEFAULT_TRACK_NAME: &str = "MIDI";

/// A trimmed, non-empty name.
fn valid_name(name: &str) -> Result<String, EditError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(EditError::Invalid("empty name"));
    }
    Ok(name.to_owned())
}

/// `hue` if it is one of the token hues.
fn valid_hue(hue: Hue) -> Result<Hue, EditError> {
    if Hue::ALL.contains(&hue) {
        Ok(hue)
    } else {
        Err(EditError::Invalid("hue is not a token hue"))
    }
}

/// Whether `meter` is supported: `num` 1 ..= 32, `den` a power of two up to 16.
pub(crate) fn valid_meter(meter: Meter) -> bool {
    (1..=32).contains(&meter.num) && matches!(meter.den, 1 | 2 | 4 | 8 | 16)
}

/// Whether `plugin` is the built-in drum sampler (or klopfer as the built-in it once was).
fn is_drum_sampler(plugin: &PluginRef) -> bool {
    plugin.format == PluginFormat::Builtin
        && (plugin.id == BUILTIN_SAMPLER || plugin.id == BUILTIN_KLOPFER)
}

/// `ids` without repetitions, in first-seen order.
fn unique<T: Copy + Eq + Hash>(ids: &[T]) -> Vec<T> {
    let mut seen = HashSet::with_capacity(ids.len());
    ids.iter().copied().filter(|id| seen.insert(*id)).collect()
}

/// A level in dB clamped to `VOLUME_DB_MIN ..= VOLUME_DB_MAX` (−∞ to the minimum); NaN is
/// `Invalid`.
fn valid_db(db: f32) -> Result<f32, EditError> {
    if db.is_nan() {
        return Err(EditError::Invalid("volume is not a number"));
    }
    Ok(db.clamp(VOLUME_DB_MIN, VOLUME_DB_MAX))
}

fn apply_channel_prop(channel: &mut Channel, prop: ChannelProp) -> Result<(), EditError> {
    match prop {
        ChannelProp::VolumeDb(db) => channel.volume_db = valid_db(db)?,
        ChannelProp::Pan(pan) => {
            if pan.is_nan() {
                return Err(EditError::Invalid("pan is not a number"));
            }
            channel.pan = pan.clamp(-1.0, 1.0);
        }
        ChannelProp::Mute(mute) => channel.mute = mute,
        ChannelProp::Solo(solo) => channel.solo = solo,
    }
    Ok(())
}

/// Applies a validated track property.
fn apply_track_prop(track: &mut Track, prop: &TrackProp) -> Result<(), EditError> {
    match prop {
        TrackProp::Name(name) => track.name.clone_from(name),
        TrackProp::Hue(hue) => track.hue = *hue,
        TrackProp::Channel(prop) => apply_channel_prop(&mut track.channel, *prop)?,
        TrackProp::Arm(_) | TrackProp::Monitor(_) if track.is_bus() => {}
        TrackProp::Arm(arm) => {
            track.arm = *arm;
            track.monitor |= *arm;
        }
        TrackProp::Monitor(monitor) => track.monitor = *monitor,
        TrackProp::Folded(folded) => track.folded = *folded && track.is_group(),
        TrackProp::Input(input) => track.input = input.clone(),
        TrackProp::MidiChannel(channel) => track.midi_channel = *channel,
        TrackProp::Height(height) => {
            track.height = (*height).clamp(TRACK_HEIGHT_MIN, TRACK_HEIGHT_MAX)
        }
    }
    Ok(())
}

// --- Locating and replacing nodes ---------------------------------------------------------------

fn locate_track(project: &Project, id: TrackId) -> Result<usize, EditError> {
    project.track_index(id).ok_or(EditError::TrackNotFound(id))
}

/// Track index and clip index of a clip.
fn locate_clip(project: &Project, id: ClipId) -> Result<(usize, usize), EditError> {
    project
        .tracks
        .iter()
        .enumerate()
        .find_map(|(t, track)| track.clips.iter().position(|c| c.id == id).map(|c| (t, c)))
        .ok_or(EditError::ClipNotFound(id))
}

/// The track at `index`, unshared.
fn track_mut(project: &mut Project, index: usize) -> &mut Track {
    Arc::make_mut(&mut project.tracks[index])
}

/// The channel `channel` refers to, unshared.
fn channel_mut(project: &mut Project, channel: ChannelRef) -> Result<&mut Channel, EditError> {
    match channel {
        ChannelRef::Track(id) => {
            let index = locate_track(project, id)?;
            Ok(&mut track_mut(project, index).channel)
        }
        ChannelRef::Master => Ok(Arc::make_mut(&mut project.master)),
    }
}

/// Runs `f` on a copy of the track at `index` and stores the copy only if it differs, so tracks an
/// edit leaves as they are stay `Arc::ptr_eq` (engine change detection).
fn update_track(
    project: &mut Project,
    index: usize,
    f: impl FnOnce(&mut Track) -> Result<(), EditError>,
) -> Result<(), EditError> {
    let mut track = Track::clone(&project.tracks[index]);
    f(&mut track)?;
    if track != *project.tracks[index] {
        project.tracks[index] = Arc::new(track);
    }
    Ok(())
}

/// Runs `f` on a copy of the clip at `location` and stores the copy only if it differs (keeping
/// the track's clips sorted).
fn update_clip<T>(
    project: &mut Project,
    (track_index, clip_index): (usize, usize),
    f: impl FnOnce(&mut Clip) -> Result<T, EditError>,
) -> Result<T, EditError> {
    let old = &project.tracks[track_index].clips[clip_index];
    let mut clip = Clip::clone(old);
    let value = f(&mut clip)?;
    if clip != **old {
        let moved = clip.start != old.start;
        let track = track_mut(project, track_index);
        track.clips[clip_index] = Arc::new(clip);
        if moved {
            sort_clips(&mut track.clips);
        }
    }
    Ok(value)
}

/// Applies `f` to the clip with `id` in a copy of `project`.
fn edit_clip_with<T>(
    project: &Project,
    id: ClipId,
    f: impl FnOnce(&mut Clip) -> Result<T, EditError>,
) -> Result<(Project, T), EditError> {
    let location = locate_clip(project, id)?;
    let mut p = project.clone();
    let value = update_clip(&mut p, location, f)?;
    Ok((p, value))
}

fn edit_clip(
    project: &Project,
    id: ClipId,
    f: impl FnOnce(&mut Clip) -> Result<(), EditError>,
) -> EditResult {
    edit_clip_with(project, id, f).map(|(p, ())| p)
}

/// Applies `f` to the device with `id` in a copy of `project`.
fn edit_device(project: &Project, id: DeviceId, f: impl FnOnce(&mut Device)) -> EditResult {
    let (channel, index, _) = project
        .find_device(id)
        .ok_or(EditError::DeviceNotFound(id))?;
    let mut p = project.clone();
    f(Arc::make_mut(
        &mut channel_mut(&mut p, channel)?.devices[index],
    ));
    Ok(p)
}

/// Inserts `clip` into a track's clips, keeping them sorted by start, then id.
fn insert_clip(track: &mut Track, clip: Arc<Clip>) {
    let key = (clip.start, clip.id);
    let at = track.clips.partition_point(|c| (c.start, c.id) <= key);
    track.clips.insert(at, clip);
}

fn sort_clips(clips: &mut [Arc<Clip>]) {
    clips.sort_by_key(|c| (c.start, c.id));
}

fn sort_notes(notes: &mut [Note]) {
    notes.sort_by_key(|n| (n.start, n.pitch, n.id));
}

// --- Clips --------------------------------------------------------------------------------------

/// A note from a [`NewNote`], clamped into range.
fn new_note(id: NoteId, spec: &NewNote) -> Note {
    Note {
        id,
        start: spec.start.max(Tick::ZERO),
        length: spec.length.max(MIN_LENGTH),
        pitch: spec.pitch.min(127),
        velocity: spec.velocity.clamp(1, 127),
    }
}

fn new_step_row(name: &str, note: u8) -> StepRow {
    StepRow {
        name: name.to_owned(),
        note: note.min(127),
        mute: false,
        solo: false,
        steps: [Step::default(); MAX_STEPS],
    }
}

/// A new pattern: 16 steps at 1/16, no swing, all steps off.
fn new_step_clip(rows: &[StepRowSpec]) -> StepClip {
    let rows = if rows.is_empty() {
        DEFAULT_STEP_ROWS
            .iter()
            .map(|&(name, note)| new_step_row(name, note))
            .collect()
    } else {
        rows.iter().map(|r| new_step_row(&r.name, r.note)).collect()
    };
    StepClip {
        rows,
        length_steps: 16,
        resolution: StepResolution::Sixteenth,
        swing: 0.0,
    }
}

/// A copy of `clip` with a new clip id and new note ids (same order, so the notes stay sorted).
fn copy_clip(project: &mut Project, clip: &Clip) -> Result<Clip, EditError> {
    let mut copy = clip.clone();
    copy.id = ClipId(project.alloc_id()?);
    if let ClipContent::Midi(m) = &mut copy.content {
        for take in &mut m.takes {
            let notes = take
                .notes
                .iter()
                .map(|n| {
                    Ok(Note {
                        id: NoteId(project.alloc_id()?),
                        ..*n
                    })
                })
                .collect::<Result<_, EditError>>()?;
            take.notes = Arc::new(notes);
        }
    }
    Ok(copy)
}

/// The clips of a move or copy with the group's clamped deltas.
struct ClipGroup {
    /// Track index and clip, in the order of the request without repetitions.
    clips: Vec<(usize, Arc<Clip>)>,
    /// Time delta, clamped so no clip starts before tick 0.
    delta: Tick,
    /// Track delta, clamped so every clip stays on a track.
    track_delta: i64,
}

impl ClipGroup {
    fn new(
        project: &Project,
        ids: &[ClipId],
        delta: Tick,
        track_delta: i32,
    ) -> Result<ClipGroup, EditError> {
        let clips = unique(ids)
            .into_iter()
            .map(|id| {
                let (t, c) = locate_clip(project, id)?;
                Ok((t, Arc::clone(&project.tracks[t].clips[c])))
            })
            .collect::<Result<Vec<_>, EditError>>()?;
        let earliest = clips
            .iter()
            .map(|(_, c)| c.start)
            .min()
            .unwrap_or(Tick::ZERO);
        let top = clips.iter().map(|&(t, _)| t).min().unwrap_or(0) as i64;
        let bottom = clips.iter().map(|&(t, _)| t).max().unwrap_or(0) as i64;
        let last = project.tracks.len() as i64 - 1;
        let group = ClipGroup {
            clips,
            delta: delta.max(-earliest),
            track_delta: i64::from(track_delta).clamp(-top, (last - bottom).max(-top)),
        };
        if group
            .clips
            .iter()
            .any(|&(t, _)| project.tracks[group.target(t)].is_bus())
        {
            return Err(EditError::Invalid("a bus holds no clips"));
        }
        Ok(group)
    }

    /// Track index a clip from `track_index` moves to.
    fn target(&self, track_index: usize) -> usize {
        (track_index as i64 + self.track_delta) as usize
    }
}

/// Splits `clip` at clip-local `at` (`0 < at < length`) into a left part (same id) and a right
/// part (new id); see [`split_clips`].
fn split_clip(project: &mut Project, clip: &Clip, at: Tick) -> Result<(Clip, Clip), EditError> {
    let mut left = clip.clone();
    let mut right = clip.clone();
    right.id = ClipId(project.alloc_id()?);
    right.start = clip.start + at;
    right.length = clip.length - at;
    left.length = at;
    match &mut right.content {
        ClipContent::Midi(right_midi) => {
            let ClipContent::Midi(left_midi) = &mut left.content else {
                unreachable!("left and right are copies of one clip");
            };
            for (l, r) in left_midi.takes.iter_mut().zip(&mut right_midi.takes) {
                let notes = match clip.loop_length {
                    Some(loop_length) => unroll(project, &l.notes, loop_length, clip.length)?,
                    None => l.notes.to_vec(),
                };
                let (mut before, mut after) = (Vec::new(), Vec::new());
                for n in notes {
                    if n.start < at {
                        before.push(Note {
                            length: n.length.min(at - n.start),
                            ..n
                        });
                    } else {
                        after.push(Note {
                            id: NoteId(project.alloc_id()?),
                            start: n.start - at,
                            ..n
                        });
                    }
                }
                sort_notes(&mut before);
                sort_notes(&mut after);
                l.notes = Arc::new(before);
                r.notes = Arc::new(after);
            }
            left.loop_length = None;
            right.loop_length = None;
        }
        ClipContent::Step(pattern) => {
            // Start the right part with the step that plays at `at` (exact when `at` lies on a
            // step boundary).
            let len = usize::from(pattern.length_steps).clamp(1, MAX_STEPS);
            let shift = (at.0 / pattern.resolution.ticks().0) as usize % len;
            for row in &mut pattern.rows {
                row.steps[..len].rotate_left(shift);
            }
        }
    }
    Ok((left, right))
}

/// The notes a looped take plays within `length`: repetitions of `0 .. loop_length`, each note cut
/// at its repetition's end and at `length`. The first repetition keeps the note ids.
fn unroll(
    project: &mut Project,
    notes: &[Note],
    loop_length: Tick,
    length: Tick,
) -> Result<Vec<Note>, EditError> {
    let mut out = Vec::new();
    let mut offset = Tick::ZERO;
    while offset < length {
        for n in notes.iter().filter(|n| n.start < loop_length) {
            let start = offset + n.start;
            if start >= length {
                break;
            }
            let end = (start + n.length).min(offset + loop_length).min(length);
            let id = if offset == Tick::ZERO {
                n.id
            } else {
                NoteId(project.alloc_id()?)
            };
            out.push(Note {
                id,
                start,
                length: end - start,
                ..*n
            });
        }
        offset += loop_length;
    }
    Ok(out)
}

// --- Notes --------------------------------------------------------------------------------------

/// The MIDI content of a clip.
fn midi_mut(clip: &mut Clip) -> Result<&mut MidiClip, EditError> {
    match &mut clip.content {
        ClipContent::Midi(m) => Ok(m),
        ClipContent::Step(_) => Err(EditError::WrongClipKind {
            clip: clip.id,
            expected: ClipKind::Midi,
        }),
    }
}

/// Runs `f` on the notes of the active take of a MIDI clip and sorts them afterwards.
fn with_notes(
    project: &mut Project,
    clip: ClipId,
    f: impl FnOnce(&mut Vec<Note>) -> Result<(), EditError>,
) -> Result<(), EditError> {
    let location = locate_clip(project, clip)?;
    update_clip(project, location, |c| {
        let m = midi_mut(c)?;
        let notes = Arc::make_mut(&mut m.takes[m.active_take].notes);
        f(notes)?;
        sort_notes(notes);
        Ok(())
    })
}

/// A set of note ids from a request.
struct NoteSelection {
    ids: HashSet<NoteId>,
}

impl NoteSelection {
    fn new(ids: &[NoteId]) -> NoteSelection {
        NoteSelection {
            ids: ids.iter().copied().collect(),
        }
    }

    fn contains(&self, id: NoteId) -> bool {
        self.ids.contains(&id)
    }

    /// `NoteNotFound` for the first id not in `notes`.
    fn check(&self, notes: &[Note]) -> Result<(), EditError> {
        let present: HashSet<NoteId> = notes.iter().map(|n| n.id).collect();
        match self.ids.iter().find(|id| !present.contains(id)) {
            Some(&missing) => Err(EditError::NoteNotFound(missing)),
            None => Ok(()),
        }
    }
}

/// Clamps a group move so no selected note starts before 0 or leaves `0 ..= 127`.
fn clamp_note_move(
    notes: &[Note],
    selection: &NoteSelection,
    delta: Tick,
    pitch_delta: i32,
) -> (Tick, i64) {
    let selected = || notes.iter().filter(|n| selection.contains(n.id));
    let earliest = selected().map(|n| n.start).min().unwrap_or(Tick::ZERO);
    let lowest = selected().map(|n| i64::from(n.pitch)).min().unwrap_or(0);
    let highest = selected().map(|n| i64::from(n.pitch)).max().unwrap_or(127);
    (
        delta.max(-earliest),
        i64::from(pitch_delta).clamp(-lowest, 127 - highest),
    )
}

/// `pitch + delta` for a delta from [`clamp_note_move`].
fn shift_pitch(pitch: u8, delta: i64) -> u8 {
    (i64::from(pitch) + delta).clamp(0, 127) as u8
}

/// `t` rounded to a multiple of `grid`, halves up (prototype `Math.round`).
fn round_to(t: Tick, grid: Tick) -> Tick {
    let q = t.0.div_euclid(grid.0);
    let r = t.0.rem_euclid(grid.0);
    Tick((q + i64::from(2 * r >= grid.0)) * grid.0)
}

// --- Steps --------------------------------------------------------------------------------------

/// Applies `f` to the pattern of a step clip in a copy of `project`.
fn with_step_clip(
    project: &Project,
    clip: ClipId,
    f: impl FnOnce(&mut StepClip) -> Result<(), EditError>,
) -> EditResult {
    edit_clip(project, clip, |c| match &mut c.content {
        ClipContent::Step(s) => f(s),
        ClipContent::Midi(_) => Err(EditError::WrongClipKind {
            clip,
            expected: ClipKind::Step,
        }),
    })
}
