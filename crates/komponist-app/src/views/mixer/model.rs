//! The mixer as data: one [`Strip`] per track and the master, with the inserts and their runtime
//! state, and the drop arithmetic of the insert slots. Pure, so the mixer is tested without
//! pixels.

use std::collections::BTreeSet;

use komponist_i18n::Lang;
use komponist_model::routing as model_routing;
use komponist_model::{
    Channel, ChannelRef, Device, DeviceId, DeviceRole, PluginFormat, Project, TrackId,
};
use komponist_plugins::{DeviceStatus, FailReason};

use crate::device_host::DeviceHost;
use crate::platform::Platform;
use crate::views::routing::{self, ChipInfo, SendInfo, Target};

/// The `base` colour of track hue `hue` (colour bar, chips, swatches), without building a whole
/// `TrackPalette` (which computes 256 note colours).
pub fn base_color(hue: u16) -> komponist_tokens::Srgba {
    track_color(hue, komponist_tokens::track::formula::BASE)
}

/// The `dim` colour of track hue `hue` (the bar of a muted strip).
pub fn dim_color(hue: u16) -> komponist_tokens::Srgba {
    track_color(hue, komponist_tokens::track::formula::DIM)
}

fn track_color(hue: u16, f: komponist_tokens::track::LightnessChroma) -> komponist_tokens::Srgba {
    komponist_tokens::oklch_to_srgb(f.l, f.c, f64::from(hue))
}

/// The scroll that shows strip `index` whole in an area `width` wide (strips `pitch` apart, each
/// `strip` wide), starting from `scroll`: the least change.
pub fn scroll_to_strip(index: usize, pitch: f32, strip: f32, width: f32, scroll: f32) -> f32 {
    let left = index as f32 * pitch;
    let right = left + strip;
    if right - scroll > width {
        right - width
    } else if left < scroll {
        left
    } else {
        scroll
    }
}

/// The runtime state an insert slot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// Running.
    Ok,
    /// "lädt": passes audio through.
    Loading,
    /// "abgestürzt": the plugin process ended.
    Crashed,
    /// "lässt sich nicht laden": loading failed (not a crash).
    LoadFailed,
    /// "hängt": the plugin stopped answering (not a crash).
    Unresponsive,
    /// "fehlt": placeholder, state kept.
    Missing,
}

impl From<&DeviceStatus> for SlotState {
    fn from(status: &DeviceStatus) -> SlotState {
        match status {
            DeviceStatus::Ok => SlotState::Ok,
            DeviceStatus::Loading => SlotState::Loading,
            DeviceStatus::Crashed(FailReason::Exited { .. }) => SlotState::Crashed,
            DeviceStatus::Crashed(FailReason::LoadFailed(_)) => SlotState::LoadFailed,
            DeviceStatus::Crashed(FailReason::Unresponsive) => SlotState::Unresponsive,
            DeviceStatus::Missing => SlotState::Missing,
        }
    }
}

/// One insert slot.
#[derive(Debug, Clone, PartialEq)]
pub struct Insert {
    /// The device.
    pub device: DeviceId,
    /// Plugin name.
    pub name: String,
    /// The track's instrument (`surface.3`, semibold, not draggable).
    pub instrument: bool,
    /// A MIDI effect: a note stage in front of the instrument (WP-88).
    pub midi: bool,
    /// Bypassed (`text.3`).
    pub bypass: bool,
    /// Runtime state.
    pub state: SlotState,
    /// An internal device (shipped with komponist, no GUI of its own): a click opens its frame in
    /// the Geräte tab instead of a plugin window (device handoff §9).
    pub internal: bool,
    /// Blocks that missed their deadline, while the slot shows "zu spät" (set by the view).
    pub late: Option<u64>,
}

impl Insert {
    /// The tooltip detail: the state when it is not plain "running", else the interactions.
    pub fn detail(&self, lang: Lang) -> &'static str {
        match self.state {
            SlotState::Loading => lang.mixer_slot_loading(),
            SlotState::Crashed => lang.mixer_slot_crashed(),
            SlotState::LoadFailed => lang.mixer_slot_load_failed(),
            SlotState::Unresponsive => lang.mixer_slot_unresponsive(),
            SlotState::Missing => lang.mixer_slot_missing(),
            SlotState::Ok if self.bypass => lang.mixer_slot_bypass(),
            SlotState::Ok => {
                Platform::current().pick(lang.mixer_slot_detail(), lang.mixer_slot_detail_mac())
            }
        }
    }
}

/// What a strip stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripKind {
    /// A MIDI track.
    Track,
    /// A bus: `group` when it is shown as a folder (Strg G), else an effect bus.
    Bus {
        /// A group.
        group: bool,
    },
    /// The master.
    Master,
}

/// The routing parts of a strip (wave 9): empty for the master.
#[derive(Debug, Clone, PartialEq)]
pub struct Routing {
    /// Where the output goes.
    pub output: Target,
    /// The sends (tracks only; a bus has none, owner decision 11 A).
    pub sends: Vec<SendInfo>,
    /// The dock chip, if the strip has one.
    pub chip: Option<ChipInfo>,
    /// A bus: its inputs as the line „2 Spuren“ / „3 Sends“.
    pub inputs: Option<String>,
}

/// One channel strip.
#[derive(Debug, Clone, PartialEq)]
pub struct Strip {
    /// Track, bus or master.
    pub kind: StripKind,
    /// Routing; `None` for the master.
    pub routing: Option<Routing>,
    /// A bus whose solo only follows a soloed track that feeds it ("läuft mit").
    pub follows_solo: bool,
    /// The channel.
    pub channel: ChannelRef,
    /// Track name, or "Master".
    pub name: String,
    /// Second line: the instrument, or "Ausgang 1–2".
    pub line: String,
    /// The track's hue; `None` for the master (its bar is `text.2`).
    pub hue: Option<u16>,
    /// Selected (synchronous with the arrangement).
    pub selected: bool,
    /// Muted or muted by a solo elsewhere: dim bar and name.
    pub dim: bool,
    /// Muted by a solo elsewhere ("stumm durch Solo" on M).
    pub implied: bool,
    /// Fader level in dB.
    pub volume_db: f32,
    /// Balance.
    pub pan: f32,
    /// M.
    pub mute: bool,
    /// S.
    pub solo: bool,
    /// R.
    pub arm: bool,
    /// The inserts, top to bottom in signal order.
    pub inserts: Vec<Insert>,
}

impl Strip {
    /// The track of a track strip.
    pub fn track(&self) -> Option<TrackId> {
        match self.channel {
            ChannelRef::Track(t) => Some(t),
            ChannelRef::Master => None,
        }
    }

    /// Whether this is the master.
    pub fn is_master(&self) -> bool {
        self.channel == ChannelRef::Master
    }

    /// Whether this is a bus (group or effect bus).
    pub fn is_bus(&self) -> bool {
        matches!(self.kind, StripKind::Bus { .. })
    }

    /// Number of MIDI effects at the head of the chain.
    pub fn midi_effect_count(&self) -> usize {
        self.inserts.iter().take_while(|i| i.midi).count()
    }

    /// The first slot an audio effect may take: after the MIDI effects and the instrument.
    pub fn first_effect_slot(&self) -> usize {
        let midi = self.midi_effect_count();
        midi + usize::from(self.inserts.get(midi).is_some_and(|i| i.instrument))
    }
}

fn inserts(lang: Lang, channel: &Channel, host: &dyn DeviceHost) -> Vec<Insert> {
    channel
        .devices
        .iter()
        .map(|d: &std::sync::Arc<Device>| Insert {
            device: d.id,
            name: crate::views::plugin_name(lang, &d.plugin),
            instrument: d.role == DeviceRole::Instrument,
            midi: d.role == DeviceRole::MidiEffect,
            bypass: d.bypass,
            state: SlotState::from(&host.status(d.id)),
            internal: d.plugin.format == PluginFormat::Builtin && !host.has_gui(d.id),
            late: None,
        })
        .collect()
}

/// The strips of `project`: the tracks in display order, then the buses in display order (they
/// stand right before the master, owner decision 3 A), then the master. Mute and solo follow the
/// signal path ([`routing::gates`]): a strip is dim when its output is muted or solo-muted.
pub fn strips(
    lang: Lang,
    project: &Project,
    selected: &BTreeSet<TrackId>,
    host: &dyn DeviceHost,
) -> Vec<Strip> {
    let any_solo = project.any_solo();
    let gates = any_solo.then(|| model_routing::gates(project));
    let strip = |t: &komponist_model::Track| {
        let ch = &t.channel;
        let implied = match &gates {
            Some(g) => !ch.mute && !ch.solo && g.output(t.id).solo_muted,
            None => false,
        };
        let follows_solo = t.is_bus() && any_solo && !ch.solo && !ch.mute && !implied;
        let line = if t.is_bus() {
            if t.folder {
                lang.routing_kind_group()
            } else {
                lang.routing_kind_effect_bus()
            }
            .to_owned()
        } else {
            ch.instrument()
                .map(|(_, d)| crate::views::plugin_name(lang, &d.plugin))
                .unwrap_or_default()
        };
        Strip {
            kind: if t.is_bus() {
                StripKind::Bus { group: t.folder }
            } else {
                StripKind::Track
            },
            routing: Some(Routing {
                output: routing::output_of(lang, project, t),
                sends: routing::sends_of(project, t),
                chip: routing::chip_info(project, t),
                inputs: t.is_bus().then(|| routing::inputs_line(lang, project, t)),
            }),
            follows_solo,
            channel: ChannelRef::Track(t.id),
            name: t.name.clone(),
            line,
            hue: Some(t.hue.0),
            selected: selected.contains(&t.id),
            dim: ch.mute || implied,
            implied,
            volume_db: ch.volume_db,
            pan: ch.pan,
            mute: ch.mute,
            solo: ch.solo,
            arm: t.arm,
            inserts: inserts(lang, ch, host),
        }
    };
    let mut out: Vec<Strip> = project
        .tracks
        .iter()
        .filter(|t| !t.is_bus())
        .map(|t| strip(t))
        .collect();
    out.extend(
        project
            .tracks
            .iter()
            .filter(|t| t.is_bus())
            .map(|t| strip(t)),
    );
    let m = &project.master;
    out.push(Strip {
        kind: StripKind::Master,
        routing: None,
        follows_solo: false,
        channel: ChannelRef::Master,
        name: lang.mixer_master().to_owned(),
        line: lang.mixer_master_output().to_owned(),
        hue: None,
        selected: false,
        dim: false,
        implied: false,
        volume_db: m.volume_db,
        pan: m.pan,
        mute: m.mute,
        solo: false,
        arm: false,
        inserts: inserts(lang, m, host),
    });
    out
}

/// The most rows any strip's send list needs, `sends + 1` for the „+“ row, at least
/// [`super::metrics::SEND_ROWS`] and at most the strip's cap. The send area is always there, also
/// before the first bus (owner, 2026-11-15); 0 only without any track or bus strip.
pub fn send_rows(strips: &[Strip]) -> usize {
    use super::metrics::{SEND_ROWS, send_rows_for};
    let most = strips
        .iter()
        .filter_map(|s| s.routing.as_ref())
        .map(|r| r.sends.len() + 1)
        .max();
    most.map_or(0, |most| send_rows_for(most).max(SEND_ROWS))
}

/// The insert slot (0 ..= slots) for a pointer at `y`, given each slot's `(top, bottom)`: before
/// the first slot whose middle lies below `y`.
pub fn slot_at(slots: &[(f32, f32)], y: f32) -> usize {
    crate::views::devices::model::slot_at(slots, y)
}

/// The `to_index` of `MoveDevice` for the slot at `from` dropped at insert position `slot`, or
/// `None` when the order stays (never before the instrument).
pub fn move_target(from: usize, slot: usize, first: usize) -> Option<usize> {
    crate::views::devices::model::move_target(from, slot, first)
}

/// [`move_target`] for the insert at `from` of a MIDI effect or an effect: slots `lo ..= hi`.
pub fn move_target_in(from: usize, slot: usize, lo: usize, hi: usize) -> Option<usize> {
    crate::views::devices::model::move_target_in(from, slot, lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plugin that cannot load or does not answer is not "abgestürzt" (owner test 2026-11-14).
    #[test]
    fn slot_states_tell_failures_apart() {
        let slot = |reason| SlotState::from(&DeviceStatus::Crashed(reason));
        let exited = FailReason::Exited {
            code: None,
            signal: Some(11),
        };
        assert_eq!(slot(exited), SlotState::Crashed);
        assert_eq!(
            slot(FailReason::LoadFailed("x".to_owned())),
            SlotState::LoadFailed
        );
        assert_eq!(slot(FailReason::Unresponsive), SlotState::Unresponsive);
        let detail = |state| {
            Insert {
                device: DeviceId(1),
                name: String::new(),
                instrument: false,
                midi: false,
                bypass: false,
                state,
                internal: false,
                late: None,
            }
            .detail(Lang::De)
        };
        assert_eq!(detail(SlotState::Crashed), "abgestürzt");
        assert_eq!(detail(SlotState::LoadFailed), "lässt sich nicht laden");
        assert_eq!(detail(SlotState::Unresponsive), "hängt");
    }

    /// A builtin host that has seen every device of `p` (unknown devices report `Ok`).
    fn host_for(p: &komponist_model::Project) -> BuiltinHost {
        let mut host = BuiltinHost::new();
        for t in &p.tracks {
            for d in &t.channel.devices {
                let _ = host.create_processor(
                    d,
                    komponist_plugins::ProcessorSetup {
                        sample_rate: 48_000.0,
                        max_block: 256,
                    },
                );
            }
        }
        host
    }
    use crate::device_host::BuiltinHost;
    use komponist_model::edits::{self, ChannelProp, TrackProp};
    use komponist_model::fixtures::demo_project;

    #[test]
    fn the_demo_has_six_strips_and_the_master() {
        let p = demo_project();
        let lead = p.tracks[2].id;
        let strips = strips(Lang::De, &p, &BTreeSet::from([lead]), &host_for(&p));
        let names: Vec<&str> = strips.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(
            names,
            ["Drums", "Bass", "Lead", "Pad", "Keys", "Arp", "Master"]
        );
        let lead = &strips[2];
        assert!(lead.selected && !lead.dim);
        assert_eq!(lead.line, "hexa");
        assert_eq!(lead.first_effect_slot(), 1);
        assert!(lead.inserts[0].instrument);
        // Without plugin manager the effects are missing; TAL is bypassed.
        assert_eq!(lead.inserts[1].state, SlotState::Missing);
        assert_eq!(
            lead.inserts[1].detail(Lang::De),
            komponist_i18n::de::MIXER_SLOT_MISSING
        );
        assert!(lead.inserts[2].bypass);
        // Arp is muted: dim.
        assert!(strips[5].mute && strips[5].dim);
        let master = strips.last().unwrap();
        assert!(master.is_master() && master.track().is_none());
        assert_eq!(master.line, "Ausgang 1–2");
        assert_eq!(master.first_effect_slot(), 0);
    }

    #[test]
    fn a_solo_dims_the_others() {
        let p = demo_project();
        let drums = p.tracks[0].id;
        let p = edits::set_track_prop(&p, &[drums], TrackProp::Channel(ChannelProp::Solo(true)))
            .unwrap();
        let strips = strips(Lang::De, &p, &BTreeSet::new(), &BuiltinHost::new());
        assert!(!strips[0].dim && !strips[0].implied);
        assert!(strips[1].dim && strips[1].implied);
        // Arp is muted itself, not by the solo.
        assert!(strips[5].dim && !strips[5].implied);
        assert!(!strips[6].dim, "the master is never dim");
    }

    #[test]
    fn colours_are_the_palette_s() {
        for hue in komponist_model::Hue::ALL {
            let p = komponist_tokens::TrackPalette::new(hue.0);
            assert_eq!(base_color(hue.0), p.base);
            assert_eq!(dim_color(hue.0), p.dim);
        }
    }

    #[test]
    fn slots_and_targets() {
        let slots = [(0.0, 20.0), (22.0, 42.0), (44.0, 64.0)];
        assert_eq!(slot_at(&slots, 5.0), 0);
        assert_eq!(slot_at(&slots, 35.0), 2);
        assert_eq!(slot_at(&slots, 80.0), 3);
        assert_eq!(move_target(1, 3, 1), Some(2));
        assert_eq!(move_target(2, 0, 1), Some(1));
        assert_eq!(move_target(1, 2, 1), None);
    }
}
