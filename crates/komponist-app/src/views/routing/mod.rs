//! The routing UI shared by the mixer, the arrangement, the inspector and the dock popover
//! (WP-103; owner decisions on WP-100, routing proposal (internal notes), ARCHITECTURE §6.8).
//!
//! This module is data and widgets, no view: [`output_targets`] and [`send_targets`] list what a
//! track can be routed to (the unusable targets stay in the list, greyed, with the reason), the
//! [`widgets`] draw the send row, the output button and the target popover. The texts and
//! AccessKit names are in the catalog (`routing.ftl`); the functions that build texts take the
//! [`Lang`].
//!
//! Everything goes through [`crate::actions::EditAction`]: `SetOutput`, `AddSend`, `SetSend`,
//! `RemoveSend`, `GroupTracks`, `SendToNewBus`.

pub mod metrics;
pub mod widgets;

use komponist_i18n::Lang;
use komponist_model::format::fmt_db;
use komponist_model::{Project, SendId, Tap, Track, TrackId, TrackOutput, routing};

/// One row of an output or send target list.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// The bus; `None` is the master.
    pub id: Option<TrackId>,
    /// The row's text.
    pub label: String,
    /// The bus's hue for the dot; `None` for the master.
    pub hue: Option<u16>,
    /// A group (shown as a folder).
    pub group: bool,
    /// Why the row cannot be chosen; `None` if it can.
    pub blocked: Option<String>,
}

impl Target {
    /// The track output this row stands for.
    pub fn output(&self) -> TrackOutput {
        self.id.map_or(TrackOutput::Master, TrackOutput::Bus)
    }
}

/// The master row.
fn master_row(lang: Lang) -> Target {
    Target {
        id: None,
        label: lang.routing_master().to_owned(),
        hue: None,
        group: false,
        blocked: None,
    }
}

/// The reason `bus` cannot take `track`'s signal: the track itself or a bus that feeds it.
fn feedback_reason(lang: Lang, track: &Track, bus: &Track) -> String {
    if bus.id == track.id {
        lang.routing_reason_self(&track.name)
    } else {
        lang.routing_reason_feedback(&bus.name, &track.name)
    }
}

/// The drag payload of a track header or strip being dragged: dropped on a bus strip (mixer) or
/// a group header (arrangement) it sets the output (owner, 2026-11-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDrag {
    /// The tracks dragged, in display order.
    pub tracks: Vec<TrackId>,
}

/// What dropping a dragged track header on a bus (a group header in the arrangement, a bus strip
/// in the mixer) does (owner, 2026-11-15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputDrop {
    /// The tracks get the bus as their output (`SetOutput`).
    Set,
    /// All of them output there already: nothing to do.
    Unchanged,
    /// It would close a loop: the German reason, shown as the hint.
    Refused(String),
}

/// The effect of dropping `tracks` on `bus`: refused with the reason when one of them is the bus
/// itself or is fed by it, else set (or unchanged when every track outputs there already).
pub fn output_drop(lang: Lang, project: &Project, tracks: &[TrackId], bus: TrackId) -> OutputDrop {
    let Some(target) = project.track(bus).filter(|b| b.is_bus()) else {
        return OutputDrop::Unchanged;
    };
    for &id in tracks {
        let Some(source) = project.track(id) else {
            continue;
        };
        if !routing::output_targets(project, id).contains(&TrackOutput::Bus(bus)) {
            return OutputDrop::Refused(feedback_reason(lang, source, target));
        }
    }
    let all_there = tracks
        .iter()
        .filter_map(|&t| project.track(t))
        .all(|t| t.output == TrackOutput::Bus(bus));
    if all_there {
        OutputDrop::Unchanged
    } else {
        OutputDrop::Set
    }
}

/// Every output of `track`: the master, then the buses in display order. A bus that would close
/// a loop (the track itself, or anything that feeds it) is blocked, with the reason.
pub fn output_targets(lang: Lang, project: &Project, track: TrackId) -> Vec<Target> {
    let Some(source) = project.track(track) else {
        return Vec::new();
    };
    let valid = routing::output_targets(project, track);
    let mut rows = vec![master_row(lang)];
    rows.extend(project.tracks.iter().filter(|t| t.is_bus()).map(|bus| {
        let ok = valid.contains(&TrackOutput::Bus(bus.id));
        Target {
            id: Some(bus.id),
            label: bus.name.clone(),
            hue: Some(bus.hue.0),
            group: bus.folder,
            blocked: (!ok).then(|| feedback_reason(lang, source, bus)),
        }
    }));
    rows
}

/// Every bus `track` can get a send to: the buses in display order. Groups are blocked (a group
/// takes tracks as outputs), so are buses the track sends to already and buses that would close a
/// loop. A bus has no sends (owner decision 11 A): the list is empty for it.
pub fn send_targets(lang: Lang, project: &Project, track: TrackId) -> Vec<Target> {
    let Some(source) = project.track(track) else {
        return Vec::new();
    };
    if source.is_bus() {
        return Vec::new();
    }
    let valid = routing::send_targets(project, track);
    project
        .tracks
        .iter()
        .filter(|t| t.is_bus())
        .map(|bus| {
            let blocked = if bus.folder {
                Some(lang.routing_reason_group(&bus.name))
            } else if source.send_to(bus.id).is_some() {
                Some(lang.routing_reason_used(&bus.name))
            } else if !valid.contains(&bus.id) {
                Some(feedback_reason(lang, source, bus))
            } else {
                None
            };
            Target {
                id: Some(bus.id),
                label: bus.name.clone(),
                hue: Some(bus.hue.0),
                group: bus.folder,
                blocked,
            }
        })
        .collect()
}

/// The buses a send of `track` can be re-pointed to (`SendProp::Target`): the targets of
/// [`send_targets`] plus the send's own bus, which stays chosen.
pub fn retarget_rows(
    lang: Lang,
    project: &Project,
    track: TrackId,
    current: TrackId,
) -> Vec<Target> {
    let mut rows = send_targets(lang, project, track);
    for row in &mut rows {
        if row.id == Some(current) {
            row.blocked = None;
        }
    }
    rows
}

/// One send of a track as the rows show it.
#[derive(Debug, Clone, PartialEq)]
pub struct SendInfo {
    /// The send.
    pub id: SendId,
    /// The sending track.
    pub track: TrackId,
    /// Its bus.
    pub target: TrackId,
    /// The bus's name (empty if the bus is gone, which the model prevents).
    pub name: String,
    /// The bus's hue.
    pub hue: u16,
    /// Level in dB.
    pub level_db: f32,
    /// Pre or post fader.
    pub tap: Tap,
    /// On or off.
    pub enabled: bool,
}

impl SendInfo {
    /// The level as the row shows it: „−6.0“ without a unit, „aus“ when off. The compact row
    /// (80 px strips) drops a zero decimal („−10“, as the mockup), so the name keeps its room.
    pub fn value_text(&self, lang: Lang, compact: bool) -> String {
        if !self.enabled {
            return lang.routing_off().to_owned();
        }
        let text = fmt_db(self.level_db);
        match text.strip_suffix(".0") {
            Some(whole) if compact => whole.to_owned(),
            _ => text,
        }
    }
}

/// The sends of `track`, in the order of its send list.
pub fn sends_of(project: &Project, track: &Track) -> Vec<SendInfo> {
    track
        .sends
        .iter()
        .map(|s| {
            let bus = project.track(s.target);
            SendInfo {
                id: s.id,
                track: track.id,
                target: s.target,
                name: bus.map(|b| b.name.clone()).unwrap_or_default(),
                hue: bus.map_or(0, |b| b.hue.0),
                level_db: s.level_db,
                tap: s.tap,
                enabled: s.enabled,
            }
        })
        .collect()
}

/// Where `track`'s output goes, as a row: name and hue of the bus, or the master.
pub fn output_of(lang: Lang, project: &Project, track: &Track) -> Target {
    match track.output {
        TrackOutput::Master => master_row(lang),
        TrackOutput::Bus(id) => match project.track(id) {
            Some(bus) => Target {
                id: Some(id),
                label: bus.name.clone(),
                hue: Some(bus.hue.0),
                group: bus.folder,
                blocked: None,
            },
            None => master_row(lang),
        },
    }
}

/// What the dock strip's routing chip shows: an arrow and the output bus's dot, or the dots of the
/// sends (up to two). `None` without a bus output and without sends: no chip.
#[derive(Debug, Clone, PartialEq)]
pub struct ChipInfo {
    /// The output bus's hue, if the output is not the master.
    pub output: Option<u16>,
    /// The hues of the send targets, in list order.
    pub sends: Vec<u16>,
}

/// The chip of `track`, if it has one.
pub fn chip_info(project: &Project, track: &Track) -> Option<ChipInfo> {
    let output = match track.output {
        TrackOutput::Master => None,
        TrackOutput::Bus(id) => project.track(id).map(|bus| bus.hue.0),
    };
    let sends: Vec<u16> = sends_of(project, track).iter().map(|s| s.hue).collect();
    (output.is_some() || !sends.is_empty()).then_some(ChipInfo { output, sends })
}

/// The members of a group, then how many sends feed a bus: the second line of a bus strip
/// („2 Spuren“ for a group, „3 Sends“ for an effect bus).
pub fn inputs_line(lang: Lang, project: &Project, bus: &Track) -> String {
    if bus.folder {
        lang.routing_tracks(routing::group_members(project, bus.id).len())
    } else {
        lang.routing_sends_count(input_sends(project, bus.id))
    }
}

/// How many sends point at `bus`.
pub fn input_sends(project: &Project, bus: TrackId) -> usize {
    project
        .tracks
        .iter()
        .flat_map(|t| &t.sends)
        .filter(|s| s.target == bus)
        .count()
}

/// The tracks that feed `bus` through their output or a send: the rows of the inspector's
/// „Eingänge“ list, with the send's level when it comes through a send.
pub fn inputs_of(project: &Project, bus: TrackId) -> Vec<(TrackId, Option<SendId>)> {
    let mut out = Vec::new();
    for t in &project.tracks {
        if t.output == TrackOutput::Bus(bus) {
            out.push((t.id, None));
        }
        out.extend(
            t.sends
                .iter()
                .filter(|s| s.target == bus)
                .map(|s| (t.id, Some(s.id))),
        );
    }
    out
}

/// „Bass › presse (Vor Fader)“: the devices that take `track` as their sidechain source, as
/// the inspector lists them. Empty when none does.
pub fn sidechain_users(lang: Lang, project: &Project, track: TrackId) -> Vec<String> {
    let mut out = Vec::new();
    let holders = project
        .tracks
        .iter()
        .map(|t| (t.name.as_str(), &t.channel))
        .chain(std::iter::once((lang.routing_master(), &*project.master)));
    for (name, channel) in holders {
        for d in &channel.devices {
            if let Some(sc) = d.sidechain.filter(|s| s.source == track) {
                let tap = match sc.tap {
                    Tap::Pre => lang.routing_pre(),
                    Tap::Post => lang.routing_post(),
                };
                out.push(format!(
                    "{name} › {} ({tap})",
                    crate::views::plugin_name(lang, &d.plugin)
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::edits::{self, NewBus};
    use komponist_model::fixtures::demo_project;

    #[test]
    fn output_targets_grey_the_loops_with_a_reason() {
        let p = demo_project();
        let drums = p.tracks[0].id;
        let bass = p.tracks[1].id;
        let (p, group) = edits::add_bus(&p, NewBus::group(vec![drums, bass])).unwrap();
        let (p, fx) = edits::add_bus(&p, NewBus::effect(vec![drums])).unwrap();
        // Drums: the master and both buses.
        let rows = output_targets(Lang::De, &p, drums);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.blocked.is_none()));
        // The group outputs to the effect bus: the bus cannot go back into the group, nor into
        // itself.
        let p = edits::set_output(&p, &[group], TrackOutput::Bus(fx)).unwrap();
        let rows = output_targets(Lang::De, &p, fx);
        let blocked = |id| {
            rows.iter()
                .find(|r| r.id == Some(id))
                .unwrap()
                .blocked
                .clone()
        };
        assert!(blocked(fx).unwrap().contains("sich selbst"));
        assert!(blocked(group).unwrap().contains("speist"));
        assert!(rows[0].blocked.is_none(), "the master is always open");
    }

    #[test]
    fn send_targets_exclude_groups_and_used_buses() {
        let p = demo_project();
        let drums = p.tracks[0].id;
        let bass = p.tracks[1].id;
        let (p, group) = edits::add_bus(&p, NewBus::group(vec![bass])).unwrap();
        let (p, fx) = edits::add_bus(&p, NewBus::effect(vec![drums])).unwrap();
        let rows = send_targets(Lang::De, &p, drums);
        let by = |id| rows.iter().find(|r| r.id == Some(id)).unwrap();
        assert!(by(group).blocked.as_deref().unwrap().contains("Gruppe"));
        assert!(
            by(fx)
                .blocked
                .as_deref()
                .unwrap()
                .starts_with("Schon verwendet")
        );
        assert!(
            send_targets(Lang::De, &p, group).is_empty(),
            "a bus has no sends"
        );
    }

    #[test]
    fn chips_and_lines() {
        let p = demo_project();
        let drums = p.tracks[0].id;
        assert_eq!(chip_info(&p, &p.tracks[0]), None);
        let (p, fx) = edits::add_bus(&p, NewBus::effect(vec![drums])).unwrap();
        let chip = chip_info(&p, p.track(drums).unwrap()).unwrap();
        assert_eq!(chip.output, None);
        assert_eq!(chip.sends.len(), 1);
        assert_eq!(inputs_line(Lang::De, &p, p.track(fx).unwrap()), "1 Send");
        assert_eq!(
            Lang::De.routing_send_on("Hall", "\u{2212}6.0", "post"),
            "Send Hall, \u{2212}6.0 dB, nach Fader"
        );
        assert_eq!(
            Lang::De.routing_group_name("Rhythmus", "folded"),
            "Gruppe Rhythmus, eingeklappt"
        );
    }
}
