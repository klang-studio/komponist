//! The routing UI shared by the mixer, the arrangement, the inspector and the dock popover
//! (WP-103; owner decisions on WP-100, routing proposal (internal notes), ARCHITECTURE §6.8).
//!
//! This module is data and widgets, no view: [`output_targets`] and [`send_targets`] list what a
//! track can be routed to (the unusable targets stay in the list, greyed, with the reason), the
//! [`widgets`] draw the send row, the output button and the target popover, and [`names`] holds
//! the German texts and AccessKit names (the views that use them are not migrated to the
//! catalog yet, `docs/I18N.md`).
//!
//! Everything goes through [`crate::actions::EditAction`]: `SetOutput`, `AddSend`, `SetSend`,
//! `RemoveSend`, `GroupTracks`, `SendToNewBus`.

pub mod metrics;
pub mod widgets;

use komponist_model::format::fmt_db;
use komponist_model::{Project, SendId, Tap, Track, TrackId, TrackOutput, routing};

/// German texts and accessible names of the routing UI (routing proposal, „Texte“ and
/// „AccessKit-Namen“; English drafts in the comments, for the catalog once the views migrate).
pub mod names {
    use komponist_model::Tap;
    use komponist_model::format::fmt_db;

    /// Section label of the sends [Sends].
    pub const SENDS: &str = "Sends";
    /// Section label of the buses in the mixer [Buses].
    pub const BUSSES: &str = "Busse";
    /// Section label of a bus's inputs [Inputs].
    pub const INPUTS: &str = "Eingänge";
    /// The output dropdown [Output].
    pub const OUTPUT: &str = "Ausgang";
    /// The master as a target [Master].
    pub const MASTER: &str = "Master";
    /// Output menu: a new effect bus [New bus …].
    pub const NEW_BUS: &str = "Neuer Bus …";
    /// Send popover: a new effect bus with a send to it (owner, 2026-11-15) [New effect bus].
    pub const NEW_EFFECT_BUS: &str = "Neuer Effekt-Bus";
    /// Output menu: a new group around the track [New group …].
    pub const NEW_GROUP: &str = "Neue Gruppe …";
    /// The „+“ row of a send list [Add send].
    pub const ADD_SEND: &str = "Send hinzufügen";
    /// Pre fader [Pre fader].
    pub const PRE: &str = "Vor Fader";
    /// Post fader [Post fader].
    pub const POST: &str = "Nach Fader";
    /// Short form, orange in the send row [pre].
    pub const PRE_SHORT: &str = "pre";
    /// Short form [post].
    pub const POST_SHORT: &str = "post";
    /// A disabled send's value [off].
    pub const OFF: &str = "aus";
    /// Context menu of a track: group the selection (Strg G) [Group tracks].
    pub const GROUP_TRACKS: &str = "In Gruppe zusammenfassen";
    /// Context menu: an effect bus fed by this track [Send to new bus].
    pub const SEND_TO_NEW_BUS: &str = "Send zu neuem Bus";
    /// Context menu: the sends submenu [Send to …].
    pub const SEND_TO: &str = "Send an …";
    /// Context menu: the output submenu [Output …].
    pub const OUTPUT_TO: &str = "Ausgang";
    /// Context menu of a send: remove it [Remove].
    pub const REMOVE: &str = "Entfernen";
    /// Context menu of a send: switch it off [Switch off].
    pub const SWITCH_OFF: &str = "Ausschalten";
    /// Context menu of a send: switch it on [Switch on].
    pub const SWITCH_ON: &str = "Einschalten";
    /// Context menu of a group: dissolve it, the tracks stay (owner decision 13 A) [Ungroup].
    pub const UNGROUP: &str = "Gruppe auflösen";
    /// Context menu of an effect bus [Delete bus].
    pub const DELETE_BUS: &str = "Bus löschen";
    /// Context menu: rename [Rename].
    pub const RENAME: &str = "Umbenennen";
    /// Context menu: colour [Colour].
    pub const COLOR: &str = "Farbe";
    /// The inspector section [Routing].
    pub const ROUTING: &str = "Routing";
    /// The inspector's sidechain line [Sidechain source for:].
    pub const SIDECHAIN_FOR: &str = "Sidechain-Quelle für:";
    /// Kind of a group [Group].
    pub const KIND_GROUP: &str = "Gruppe";
    /// Kind of an effect bus [Effect bus].
    pub const KIND_EFFECT_BUS: &str = "Effekt-Bus";
    /// The „Bus“ type of the new-track dialog [Bus].
    pub const KIND_BUS: &str = "Bus";
    /// Section of the arrangement below „Neue Spur“ [Buses].
    pub const SECTION: &str = "Busse";
    /// The reason a bus takes no clips [Buses hold no clips.].
    pub const NO_CLIPS: &str = "Busse nehmen keine Clips auf.";
    /// A bus takes audio effects only; a drop of an instrument is refused [Buses take effects only.].
    pub const EFFECTS_ONLY: &str = "Busse nehmen nur Effekte auf.";
    /// Tooltip of a strip's „S“ that only follows a solo [Follows solo because a soloed track
    /// feeds this bus.].
    pub const SOLO_FOLLOWS: &str =
        "Läuft mit, weil eine solo geschaltete Spur auf diesen Bus ausgibt.";
    /// Dock popover title prefix [Routing · Lead].
    pub const POPOVER_HINT: &str = "Strg+Klick auf den Chip: ganze Liste im Inspektor";
    /// Inspector: nothing routes here [No inputs].
    pub const NO_INPUTS: &str = "Noch nichts geroutet";
    /// The mixer strip of a bus: the line under the name.
    pub fn bus_line(group: bool) -> &'static str {
        if group { KIND_GROUP } else { KIND_EFFECT_BUS }
    }

    /// „Busspur Hall“: a bus strip (rule 12).
    pub fn bus_strip(name: &str) -> String {
        format!("Busspur {name}")
    }

    /// „Send Hall, −6.0 dB, nach Fader“: a send row [Send Hall, −6.0 dB, post fader].
    pub fn send(target: &str, db: f32, tap: Tap, enabled: bool) -> String {
        let tap = match tap {
            Tap::Pre => "vor Fader",
            Tap::Post => "nach Fader",
        };
        if enabled {
            format!("Send {target}, {} dB, {tap}", fmt_db(db))
        } else {
            format!("Send {target}, aus, {tap}")
        }
    }

    /// „Send Lead an Hall: Vor Fader“: the pre/post switch [Send Lead to Hall: Pre fader].
    pub fn send_tap(track: &str, target: &str, tap: Tap) -> String {
        let tap = match tap {
            Tap::Pre => PRE,
            Tap::Post => POST,
        };
        format!("Send {track} an {target}: {tap}")
    }

    /// „Ausgang: Master“: the output button [Output: Master].
    pub fn output(target: &str) -> String {
        format!("Ausgang: {target}")
    }

    /// „Ausgang Lead“: the name of the output list.
    pub fn output_list(track: &str) -> String {
        format!("Ausgang {track}")
    }

    /// „Send hinzufügen, Lead“: the „+“ row.
    pub fn add_send(track: &str) -> String {
        format!("{ADD_SEND}, {track}")
    }

    /// „Routing Lead: Ausgang Master, 2 Sends“: the dock chip [Routing Lead: output Master,
    /// 2 sends].
    pub fn chip(track: &str, output: &str, sends: usize) -> String {
        match sends {
            0 => format!("Routing {track}: Ausgang {output}"),
            1 => format!("Routing {track}: Ausgang {output}, 1 Send"),
            n => format!("Routing {track}: Ausgang {output}, {n} Sends"),
        }
    }

    /// „Routing · Lead“: the popover's title.
    pub fn popover_title(track: &str) -> String {
        format!("{ROUTING} · {track}")
    }

    /// „Gruppe Rhythmus, eingeklappt“: a group header (rule 12) [Group Rhythmus, folded].
    pub fn group(name: &str, folded: bool) -> String {
        let state = if folded { "eingeklappt" } else { "ausgeklappt" };
        format!("Gruppe {name}, {state}")
    }

    /// „Effekt-Bus Hall“: the header of an effect bus [Effect bus Hall].
    pub fn effect_bus(name: &str) -> String {
        format!("{KIND_EFFECT_BUS} {name}")
    }

    /// „2 Spuren“, „1 Spur“: the line under a group's name [2 tracks].
    pub fn tracks(n: usize) -> String {
        if n == 1 {
            "1 Spur".to_owned()
        } else {
            format!("{n} Spuren")
        }
    }

    /// „3 Sends“, „1 Send“ [3 sends].
    pub fn sends(n: usize) -> String {
        if n == 1 {
            "1 Send".to_owned()
        } else {
            format!("{n} Sends")
        }
    }

    /// „Effekt-Bus · 3 Sends · keine Clips“: the lane of an effect bus [Effect bus · 3 sends · no
    /// clips].
    pub fn bus_lane(sends: usize) -> String {
        format!("{KIND_EFFECT_BUS} · {} · keine Clips", self::sends(sends))
    }

    /// „Summe der Spuren (nur Anzeige)“: the lane of a group [Sum of the tracks (display only)].
    pub const GROUP_LANE: &str = "Summe der Spuren (nur Anzeige)";

    /// „Gruppe auflösen: Rhythmus“ for the accessible name of a menu entry.
    pub fn blocked(label: &str) -> String {
        format!("{label}, nicht möglich: Rückkopplung")
    }

    /// „Nicht möglich: „Hall“ speist „Rhythmus“ schon (Rückkopplung).“ [Not possible: “Hall”
    /// already feeds “Rhythmus” (feedback loop).]
    pub fn reason_feedback(target: &str, source: &str) -> String {
        format!("Nicht möglich: „{target}“ speist „{source}“ schon (Rückkopplung).")
    }

    /// The reason a track cannot output to itself.
    pub fn reason_self(track: &str) -> String {
        format!("Nicht möglich: „{track}“ kann nicht an sich selbst ausgeben.")
    }

    /// The reason a bus is no send target: the send exists [Already used: Hall].
    pub fn reason_used(target: &str) -> String {
        format!("Schon verwendet: {target}")
    }

    /// The reason a group is no send target (a group takes tracks as outputs).
    pub fn reason_group(target: &str) -> String {
        format!("„{target}“ ist eine Gruppe: Eine Gruppe nimmt man als Ausgang, nicht als Send.")
    }

    /// „Ausgang geändert: Lead → Hall“ is not needed; the tooltip of the output button.
    pub fn output_tip(track: &str) -> String {
        format!("Ausgang von „{track}“")
    }

    /// The mixer's line under a bus name when it is not a group.
    pub fn solo_follows(name: &str) -> String {
        format!("Solo {name}, läuft mit")
    }
}

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
fn master_row() -> Target {
    Target {
        id: None,
        label: names::MASTER.to_owned(),
        hue: None,
        group: false,
        blocked: None,
    }
}

/// The reason `bus` cannot take `track`'s signal: the track itself or a bus that feeds it.
fn feedback_reason(track: &Track, bus: &Track) -> String {
    if bus.id == track.id {
        names::reason_self(&track.name)
    } else {
        names::reason_feedback(&bus.name, &track.name)
    }
}

/// Every output of `track`: the master, then the buses in display order. A bus that would close
/// a loop (the track itself, or anything that feeds it) is blocked, with the reason.
pub fn output_targets(project: &Project, track: TrackId) -> Vec<Target> {
    let Some(source) = project.track(track) else {
        return Vec::new();
    };
    let valid = routing::output_targets(project, track);
    let mut rows = vec![master_row()];
    rows.extend(project.tracks.iter().filter(|t| t.is_bus()).map(|bus| {
        let ok = valid.contains(&TrackOutput::Bus(bus.id));
        Target {
            id: Some(bus.id),
            label: bus.name.clone(),
            hue: Some(bus.hue.0),
            group: bus.folder,
            blocked: (!ok).then(|| feedback_reason(source, bus)),
        }
    }));
    rows
}

/// Every bus `track` can get a send to: the buses in display order. Groups are blocked (a group
/// takes tracks as outputs), so are buses the track sends to already and buses that would close a
/// loop. A bus has no sends (owner decision 11 A): the list is empty for it.
pub fn send_targets(project: &Project, track: TrackId) -> Vec<Target> {
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
                Some(names::reason_group(&bus.name))
            } else if source.send_to(bus.id).is_some() {
                Some(names::reason_used(&bus.name))
            } else if !valid.contains(&bus.id) {
                Some(feedback_reason(source, bus))
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
pub fn retarget_rows(project: &Project, track: TrackId, current: TrackId) -> Vec<Target> {
    let mut rows = send_targets(project, track);
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
    pub fn value_text(&self, compact: bool) -> String {
        if !self.enabled {
            return names::OFF.to_owned();
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
pub fn output_of(project: &Project, track: &Track) -> Target {
    match track.output {
        TrackOutput::Master => master_row(),
        TrackOutput::Bus(id) => match project.track(id) {
            Some(bus) => Target {
                id: Some(id),
                label: bus.name.clone(),
                hue: Some(bus.hue.0),
                group: bus.folder,
                blocked: None,
            },
            None => master_row(),
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
    let output = output_of(project, track).hue;
    let sends: Vec<u16> = sends_of(project, track).iter().map(|s| s.hue).collect();
    (output.is_some() || !sends.is_empty()).then_some(ChipInfo { output, sends })
}

/// The members of a group, then how many sends feed a bus: the second line of a bus strip
/// („2 Spuren“ for a group, „3 Sends“ for an effect bus).
pub fn inputs_line(project: &Project, bus: &Track) -> String {
    if bus.folder {
        names::tracks(routing::group_members(project, bus.id).len())
    } else {
        names::sends(input_sends(project, bus.id))
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
pub fn sidechain_users(project: &Project, track: TrackId) -> Vec<String> {
    let mut out = Vec::new();
    let holders = project
        .tracks
        .iter()
        .map(|t| (t.name.as_str(), &t.channel))
        .chain(std::iter::once((names::MASTER, &*project.master)));
    for (name, channel) in holders {
        for d in &channel.devices {
            if let Some(sc) = d.sidechain.filter(|s| s.source == track) {
                let tap = match sc.tap {
                    Tap::Pre => names::PRE,
                    Tap::Post => names::POST,
                };
                out.push(format!("{name} › {} ({tap})", d.plugin.name));
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
        let rows = output_targets(&p, drums);
        assert_eq!(rows.len(), 3);
        assert!(rows.iter().all(|r| r.blocked.is_none()));
        // The group outputs to the effect bus: the bus cannot go back into the group, nor into
        // itself.
        let p = edits::set_output(&p, &[group], TrackOutput::Bus(fx)).unwrap();
        let rows = output_targets(&p, fx);
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
        let rows = send_targets(&p, drums);
        let by = |id| rows.iter().find(|r| r.id == Some(id)).unwrap();
        assert!(by(group).blocked.as_deref().unwrap().contains("Gruppe"));
        assert!(
            by(fx)
                .blocked
                .as_deref()
                .unwrap()
                .starts_with("Schon verwendet")
        );
        assert!(send_targets(&p, group).is_empty(), "a bus has no sends");
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
        assert_eq!(inputs_line(&p, p.track(fx).unwrap()), "1 Send");
        assert_eq!(
            names::send("Hall", -6.0, Tap::Post, true),
            "Send Hall, \u{2212}6.0 dB, nach Fader"
        );
        assert_eq!(
            names::group("Rhythmus", true),
            "Gruppe Rhythmus, eingeklappt"
        );
    }
}
