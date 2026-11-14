//! The device chain as data: one [`Card`] per device of a track, with its state, size and the
//! parameters its body shows, and the drop arithmetic of reordering. Pure, so the chain is
//! tested without pixels.

use komponist_builtin::Macro;
use komponist_ipc::ParamValue;
use komponist_ipc::control::ParamInfo as WireInfo;
use komponist_model::params::{ParamInfo, auto_layout};
use komponist_model::{Device, DeviceId, DeviceRole, Track};
use komponist_plugins::params::{is_listed, param_info};
use komponist_plugins::{DeviceStatus, FailReason};

use super::metrics;
use crate::device_host::{DeviceHost, is_pisynth};
use crate::ui_state::PluginWindow;
use crate::views::format_tag;

/// German texts of the device chain (`Screens.dc.html` `card()`, the `isDevices` markup, S05/S06
/// notes).
pub mod names {
    /// "Öffnen" (`openT` default).
    pub const OPEN: &str = "Öffnen";
    /// The open button while the plugin window is open (`openT:'Geöffnet'`).
    pub const OPENED: &str = "Geöffnet";
    /// The open button of a plugin without own GUI (`openT:'Parameter'`): opens the generic view.
    pub const PARAMS: &str = "Parameter";
    /// Loading (`msg`).
    pub const LOADING: &str = "Lädt …";
    /// Its text (`msg2`).
    pub const LOADING_TEXT: &str = "Plugin wird gescannt und instanziiert.";
    /// Crashed (`msg`).
    pub const CRASHED: &str = "Abgestürzt";
    /// Its text (`msg2`).
    pub const CRASHED_TEXT: &str = "Das Plugin wurde beendet, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.";
    /// The text when loading failed (the status bar's „… lässt sich nicht laden“ plus the rest
    /// of `msg2`).
    pub const LOAD_FAILED_TEXT: &str = "Das Plugin lässt sich nicht laden, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.";
    /// The title when loading failed (the status bar's „… lässt sich nicht laden“; not in S05).
    pub const LOAD_FAILED: &str = "Lässt sich nicht laden";
    /// The title of a plugin that stopped answering („hängt“ as in the status bar; not in S05).
    pub const UNRESPONSIVE: &str = "Hängt";
    /// Its text.
    pub const UNRESPONSIVE_TEXT: &str = "Das Plugin antwortet nicht mehr, die Spur läuft ohne dieses Gerät weiter. Die letzte Einstellung ist gesichert.";
    /// "Neu laden" (`btnT`).
    pub const RELOAD: &str = "Neu laden";
    /// A missing plugin (S06 label „Plugin fehlt“).
    pub const MISSING: &str = "Plugin fehlt";
    /// Its text, from the S06 dialog („… nicht installiert oder liegen nicht in einem
    /// Plugin-Ordner. Ihre Einstellungen bleiben im Projekt gespeichert …“).
    pub const MISSING_TEXT: &str = "Nicht installiert oder nicht in einem Plugin-Ordner. Die Einstellungen bleiben im Projekt gespeichert und werden wieder geladen, sobald das Plugin gefunden wird.";
    /// A state that failed to load (WP-45 note, the status bar's text).
    pub const STATE_WARNING: &str = "Zustand konnte nicht geladen werden";
    /// The drop area after the last card.
    pub const DROP_HINT: &str = "Effekt aus dem Browser hierher ziehen";
    /// The header button (`+ Gerät`).
    pub const ADD: &str = "+ Gerät";
    /// Accessible name and tooltip of the bypass switch (on = the device runs).
    pub const ACTIVE: &str = "Aktiv";
    /// Tooltip detail of the switch (S05 note „Schalter (Bypass)“).
    pub const BYPASS: &str = "Bypass";
    /// Context menu: remove the device.
    pub const REMOVE: &str = "Entfernen";
    /// Accessible name of the context menu and prefix of a card's group.
    pub const DEVICE: &str = "Gerät";
    /// Accessible name of the card row.
    pub const CHAIN: &str = "Geräte-Kette";

    /// „5 Geräte · Signal läuft von links nach rechts“.
    pub fn info(count: usize) -> String {
        let devices = if count == 1 { "Gerät" } else { "Geräte" };
        format!("{count} {devices} · Signal läuft von links nach rechts")
    }

    /// The accessible name of a card: „Gerät Surge XT“.
    pub fn card(name: &str) -> String {
        format!("{DEVICE} {name}")
    }
}

/// The runtime state a card shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardState {
    /// Running (or bypassed: [`Card::bypass`]).
    Ok,
    /// "Lädt": progress, the device passes audio through.
    Loading,
    /// "Abgestürzt": red edge, "Neu laden".
    Crashed(FailReason),
    /// "Fehlt": placeholder in red, state kept.
    Missing,
}

impl From<DeviceStatus> for CardState {
    fn from(status: DeviceStatus) -> CardState {
        match status {
            DeviceStatus::Ok => CardState::Ok,
            DeviceStatus::Loading => CardState::Loading,
            DeviceStatus::Crashed(reason) => CardState::Crashed(reason),
            DeviceStatus::Missing => CardState::Missing,
        }
    }
}

/// The title and text of a failed device's card, and of the S05 frame's body: a crash reads
/// „Abgestürzt“; a plugin that cannot load or does not answer must not (owner test 2026-11-14),
/// it reads „Lässt sich nicht laden“ with the reason, or „Hängt“.
pub fn failure_texts(reason: &FailReason) -> (&'static str, String) {
    match reason {
        FailReason::Exited { .. } => (names::CRASHED, names::CRASHED_TEXT.to_owned()),
        FailReason::LoadFailed(why) => {
            let why = why.trim();
            let text = if why.is_empty() {
                names::LOAD_FAILED_TEXT.to_owned()
            } else {
                format!("{} ({why})", names::LOAD_FAILED_TEXT)
            };
            (names::LOAD_FAILED, text)
        }
        FailReason::Unresponsive => (names::UNRESPONSIVE, names::UNRESPONSIVE_TEXT.to_owned()),
    }
}

/// A knob of a card: the parameter's description in the parameter model plus its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// The id the device is addressed by (`Command::SetParam::id`: pisynth's macro id, the
    /// plugin's own parameter id); the model id is `info.id`.
    pub wire: u32,
    /// Name, kind, range, default, unit, flags.
    pub info: ParamInfo,
    /// Plain value.
    pub value: f64,
    /// Text of `value` (the plugin's own, else [`ParamInfo::format`]).
    pub text: String,
}

impl Param {
    /// Name under the knob.
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// `value` clamped and, for a stepped parameter, on a step.
    pub fn snap(&self, value: f64) -> f64 {
        self.info.snap(value)
    }

    /// The text of plain value `v`: the plugin's own while `v` is the value it describes, else
    /// the model's formatting.
    pub fn text_at(&self, v: f64) -> String {
        if (v - self.value).abs() < 1e-9 && !self.text.is_empty() {
            self.text.clone()
        } else {
            self.info.format(v)
        }
    }
}

/// One device card.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    /// The device.
    pub device: DeviceId,
    /// Plugin name.
    pub name: String,
    /// "INTERN" / "CLAP" / "VST3".
    pub tag: &'static str,
    /// The track's instrument (head `surface.3`, not draggable).
    pub instrument: bool,
    /// Bypassed.
    pub bypass: bool,
    /// Runtime state.
    pub state: CardState,
    /// Why the stored state did not load, while that matters.
    pub warning: Option<String>,
    /// The knobs (empty while not running).
    pub params: Vec<Param>,
    /// The plugin has its own GUI ("Öffnen"); else the window shows the generic view ("Parameter").
    pub has_gui: bool,
    /// Its plugin window is open ("Geöffnet").
    pub open: bool,
    /// Width in design px.
    pub width: f32,
    /// Columns of the parameter grid.
    pub columns: usize,
    /// Blocks that missed their deadline, while the head shows "zu spät" (set by the view).
    pub late: Option<u64>,
}

impl Card {
    /// The open button's label, `None` while it is hidden (loading, crashed, missing).
    pub fn open_label(&self) -> Option<&'static str> {
        match self.state {
            CardState::Ok if self.open => Some(names::OPENED),
            CardState::Ok if self.has_gui => Some(names::OPEN),
            CardState::Ok => Some(names::PARAMS),
            _ => None,
        }
    }
}

/// The cards of `track`'s chain, left to right.
pub fn cards(track: &Track, host: &dyn DeviceHost, windows: &[PluginWindow]) -> Vec<Card> {
    track
        .channel
        .devices
        .iter()
        .map(|d| card(d, host, windows))
        .collect()
}

fn card(device: &Device, host: &dyn DeviceHost, windows: &[PluginWindow]) -> Card {
    let state = CardState::from(host.status(device.id));
    let params = if state == CardState::Ok {
        if is_pisynth(device) {
            pisynth_params(device)
        } else {
            host.params(device.id)
                .map(|infos| plugin_params(infos, |id| host.param_value(device.id, id)))
                .unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    let instrument = device.role == DeviceRole::Instrument;
    let n = params.len();
    let narrow = (1..=metrics::NARROW_PARAMS).contains(&n);
    let width = match state {
        CardState::Loading => metrics::CARD_WIDTH_NARROW,
        CardState::Crashed(_) | CardState::Missing => metrics::CARD_WIDTH_MESSAGE,
        CardState::Ok if instrument => metrics::CARD_WIDTH_INSTRUMENT,
        CardState::Ok if narrow => metrics::CARD_WIDTH_NARROW,
        CardState::Ok => metrics::CARD_WIDTH_EFFECT,
    };
    Card {
        device: device.id,
        name: device.plugin.name.clone(),
        tag: format_tag(device.plugin.format),
        instrument,
        bypass: device.bypass,
        warning: host.state_warning(device.id).map(str::to_owned),
        params,
        has_gui: host.has_gui(device.id),
        open: windows.iter().any(|w| w.device == device.id),
        width,
        columns: if narrow { n } else { metrics::COLUMNS },
        late: None,
        state,
    }
}

/// pisynth's 8 macros, read from the patch in the device state.
pub fn pisynth_params(device: &Device) -> Vec<Param> {
    let patch = komponist_builtin::decode_state(device.state.as_deref())
        .unwrap_or_else(|_| komponist_builtin::default_patch());
    Macro::ALL
        .iter()
        .map(|m| {
            let value = m.get(&patch);
            Param {
                wire: m.id(),
                info: m.param_info(),
                value,
                text: m.text(value),
            }
        })
        .collect()
}

/// The knobs of a foreign plugin's card: the auto-layout of the parameters a generic view lists
/// (not hidden, not read-only, not the plugin's own bypass): its macros if it reports any, else
/// the first 12, with their last known value and text.
pub fn plugin_params<'a>(
    infos: &[WireInfo],
    value: impl Fn(u32) -> Option<&'a ParamValue>,
) -> Vec<Param> {
    let listed: Vec<&WireInfo> = infos.iter().filter(|p| is_listed(p)).collect();
    let models: Vec<ParamInfo> = listed.iter().map(|p| param_info(p)).collect();
    auto_layout(&models)
        .into_iter()
        .map(|i| plugin_param(listed[i], models[i].clone(), value(listed[i].id)))
        .collect()
}

/// Every parameter a generic view lists, for the plugin frame's grid (no auto-layout limit).
pub fn all_plugin_params<'a>(
    infos: &[WireInfo],
    value: impl Fn(u32) -> Option<&'a ParamValue>,
) -> Vec<Param> {
    infos
        .iter()
        .filter(|p| is_listed(p))
        .map(|p| plugin_param(p, param_info(p), value(p.id)))
        .collect()
}

fn plugin_param(wire: &WireInfo, info: ParamInfo, known: Option<&ParamValue>) -> Param {
    let value = known.map_or(wire.default, |k| k.value);
    let text = known
        .map(|k| k.text.clone())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| info.format(value));
    Param {
        wire: wire.id,
        info,
        value,
        text,
    }
}

/// The insert slot (0 ..= cards) for a pointer at `x`, given each card's `(left, right)`: before
/// the first card whose middle lies right of `x`.
pub fn slot_at(cards: &[(f32, f32)], x: f32) -> usize {
    cards
        .iter()
        .position(|(l, r)| x < (l + r) / 2.0)
        .unwrap_or(cards.len())
}

/// The first slot an effect may take: after the instrument, if the chain starts with one.
pub fn first_effect_slot(cards: &[Card]) -> usize {
    usize::from(cards.first().is_some_and(|c| c.instrument))
}

/// The `to_index` of `MoveDevice` (the index without the moved card) for the card at `from`
/// dropped at insert slot `slot`, or `None` when the order stays.
pub fn move_target(from: usize, slot: usize, first: usize) -> Option<usize> {
    let slot = slot.max(first);
    let to = if slot > from { slot - 1 } else { slot };
    (to != from).then_some(to)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_host::BuiltinHost;
    use komponist_ipc::ParamValue;
    use komponist_model::fixtures::demo_project;

    use komponist_ipc::control::param_flags;

    fn info(id: u32, name: &str, flags: u32) -> WireInfo {
        WireInfo {
            id,
            name: name.into(),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.5,
            step_count: 0,
            flags,
        }
    }

    fn many(n: u32) -> Vec<WireInfo> {
        let mut infos = vec![
            info(1, "Bypass", param_flags::BYPASS),
            info(2, "Meter", param_flags::READ_ONLY),
            info(3, "Intern", param_flags::HIDDEN),
        ];
        infos.extend((10..10 + n).map(|i| info(i, &format!("P{i}"), param_flags::AUTOMATABLE)));
        infos
    }

    #[test]
    fn the_auto_layout_skips_hidden_parameters_and_takes_twelve() {
        let values = [ParamValue {
            id: 10,
            value: 0.18,
            text: "80 Hz".into(),
        }];
        let params = plugin_params(&many(20), |id| values.iter().find(|v| v.id == id));
        assert_eq!(params.len(), komponist_model::params::AUTO_MAX);
        assert_eq!(params[0].name(), "P10");
        assert_eq!((params[0].wire, params[0].info.id.as_str()), (10, "p10"));
        assert_eq!((params[0].value, params[0].text.as_str()), (0.18, "80 Hz"));
        assert_eq!((params[1].value, params[1].text.as_str()), (0.5, "50 %"));
        assert_eq!(params[11].name(), "P21");
        // The plugin frame's grid keeps all of them.
        assert_eq!(all_plugin_params(&many(20), |_| None).len(), 20);
        assert_eq!(plugin_params(&many(5), |_| None).len(), 5);
    }

    #[test]
    fn reported_macros_win_over_the_first_twelve() {
        let mut infos = many(20);
        for i in [15, 12] {
            infos[3 + (i - 10)].flags |= param_flags::MACRO;
        }
        let names: Vec<_> = plugin_params(&infos, |_| None)
            .iter()
            .map(|p| p.name().to_owned())
            .collect();
        assert_eq!(names, ["P12", "P15"]);
    }

    #[test]
    fn the_card_grid_is_four_columns_like_the_auto_layout() {
        assert_eq!(metrics::COLUMNS, komponist_model::params::AUTO_COLUMNS);
    }

    #[test]
    fn values_are_texted_and_snapped() {
        let algo = pisynth_params(
            &komponist_model::fixtures::demo_project().tracks[2]
                .channel
                .devices[0],
        )
        .remove(0);
        assert_eq!(algo.name(), "Algorithmus");
        assert_eq!(algo.snap(7.4), 7.0);
        assert_eq!(algo.snap(40.0), 32.0);
        assert_eq!(algo.text_at(0.0), "Frei");
        let ratio = pisynth_params(
            &komponist_model::fixtures::demo_project().tracks[2]
                .channel
                .devices[0],
        )
        .remove(1);
        assert_eq!(ratio.snap(3.3), 3.5);
        // The plugin's own text only for the value it describes.
        let own = ParamValue {
            id: 1,
            value: 0.18,
            text: "-12.0 dB".into(),
        };
        let p = plugin_params(&[info(1, "Gain", 0)], |_| Some(&own)).remove(0);
        assert_eq!(p.text_at(0.18), "-12.0 dB");
        assert_eq!(p.text_at(0.25), "25 %");
    }

    #[test]
    fn the_lead_chain_with_the_builtin_host() {
        let project = demo_project();
        let lead = &project.tracks[2];
        let mut host = BuiltinHost::new();
        for d in &lead.channel.devices {
            let _ = host.create_processor(
                d,
                komponist_plugins::ProcessorSetup {
                    sample_rate: 48_000.0,
                    max_block: 256,
                },
            );
        }
        let cards = cards(lead, &host, &[]);
        let names: Vec<&str> = cards.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "pisynth",
                "LSP Para EQ x8",
                "TAL-Chorus-LX",
                "Dragonfly Hall Reverb"
            ]
        );
        let pisynth = &cards[0];
        assert!(pisynth.instrument && pisynth.state == CardState::Ok);
        assert_eq!(pisynth.width, metrics::CARD_WIDTH_INSTRUMENT);
        assert_eq!(pisynth.tag, "INTERN");
        let macros: Vec<&str> = pisynth.params.iter().map(|p| p.name()).collect();
        assert_eq!(
            macros,
            [
                "Algorithmus",
                "Ratio 2",
                "Index",
                "Feedback",
                "Attack",
                "Decay",
                "Sustain",
                "Release"
            ]
        );
        assert_eq!(pisynth.open_label(), Some(names::PARAMS));
        // Without plugin manager the effects are missing; TAL is bypassed.
        assert_eq!(cards[1].state, CardState::Missing);
        assert_eq!(cards[1].width, metrics::CARD_WIDTH_MESSAGE);
        assert_eq!(cards[1].open_label(), None);
        assert!(cards[2].bypass);
        assert_eq!(first_effect_slot(&cards), 1);
    }

    #[test]
    fn drop_slots_and_move_targets() {
        let cards = [(0.0, 280.0), (288.0, 538.0), (546.0, 746.0)];
        assert_eq!(slot_at(&cards, 100.0), 0);
        assert_eq!(slot_at(&cards, 200.0), 1);
        assert_eq!(slot_at(&cards, 500.0), 2);
        assert_eq!(slot_at(&cards, 900.0), 3);
        // Effect at 1 dropped after the last card: to index 2 of the chain without it.
        assert_eq!(move_target(1, 3, 1), Some(2));
        assert_eq!(move_target(2, 1, 1), Some(1));
        assert_eq!(move_target(1, 1, 1), None);
        assert_eq!(move_target(1, 2, 1), None, "right after itself");
        assert_eq!(move_target(2, 0, 1), Some(1), "never before the instrument");
    }
}
