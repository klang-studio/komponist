//! The device chain as data: one [`Card`] per device of a track, with its state, size and the
//! parameters its body shows, and the drop arithmetic of reordering. Pure, so the chain is
//! tested without pixels.

use komponist_builtin::Macro;
use komponist_ipc::ParamInfo;
use komponist_ipc::control::param_flags;
use komponist_model::{Device, DeviceId, DeviceRole, Track};
use komponist_plugins::{DeviceStatus, FailReason};
use komponist_ui::widgets::number_field::format_number;

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

/// A knob of a card.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// Parameter id (pisynth: the macro id).
    pub id: u32,
    /// Name under the knob.
    pub name: String,
    /// Plain value.
    pub value: f64,
    /// Range.
    pub min: f64,
    /// Range.
    pub max: f64,
    /// Alt-click value.
    pub default: f64,
    /// Steps of a stepped parameter, 0 = continuous.
    pub steps: u32,
    /// Text of the value (the plugin's, else formatted here).
    pub text: String,
}

impl Param {
    /// `value` clamped and, for a stepped parameter, on a step.
    pub fn snap(&self, value: f64) -> f64 {
        let v = value.clamp(self.min.min(self.max), self.max.max(self.min));
        if self.steps == 0 || self.max <= self.min {
            return v;
        }
        let step = (self.max - self.min) / f64::from(self.steps);
        self.min + ((v - self.min) / step).round() * step
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
    /// The knobs are pisynth's macros (their text comes from `Macro::text`).
    pub macros: bool,
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
    let macros = is_pisynth(device);
    let params = if state == CardState::Ok {
        if macros {
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
        macros,
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
                id: m.id(),
                name: m.name().to_owned(),
                value,
                min: m.min(),
                max: m.max(),
                default: m.default_value(),
                steps: m.step_count(),
                text: m.text(value),
            }
        })
        .collect()
}

/// The first [`metrics::MAX_PARAMS`] parameters a generic grid shows (not hidden, not read-only,
/// not the plugin's own bypass), with their last known value and text.
pub fn plugin_params<'a>(
    infos: &[ParamInfo],
    value: impl Fn(u32) -> Option<&'a komponist_ipc::ParamValue>,
) -> Vec<Param> {
    let hidden = param_flags::HIDDEN | param_flags::READ_ONLY | param_flags::BYPASS;
    infos
        .iter()
        .filter(|p| p.flags & hidden == 0)
        .take(metrics::MAX_PARAMS)
        .map(|p| {
            let known = value(p.id);
            let v = known.map_or(p.default, |k| k.value);
            let text = known
                .map(|k| k.text.clone())
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| format_value(p.min, p.max, p.step_count, v));
            Param {
                id: p.id,
                name: p.name.clone(),
                value: v,
                min: p.min,
                max: p.max,
                default: p.default,
                steps: p.step_count,
                text,
            }
        })
        .collect()
}

/// The text of a value without the plugin's own (values from its GUI during processing, WP-45
/// note): normalised parameters (VST3, nice-plug's CLAP) as "40 %", stepped ones as integers,
/// the rest with two decimals.
pub fn format_value(min: f64, max: f64, steps: u32, value: f64) -> String {
    if min == 0.0 && max == 1.0 && steps == 0 {
        format!("{} %", (value * 100.0).round())
    } else if steps > 0 && max - min == f64::from(steps) {
        format!("{}", value.round())
    } else {
        format_number(value, 2)
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

    fn info(id: u32, name: &str, flags: u32) -> ParamInfo {
        ParamInfo {
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

    #[test]
    fn the_generic_grid_skips_hidden_parameters_and_takes_eight() {
        let mut infos = vec![
            info(1, "Bypass", param_flags::BYPASS),
            info(2, "Meter", param_flags::READ_ONLY),
            info(3, "Intern", param_flags::HIDDEN),
        ];
        infos.extend((10..20).map(|i| info(i, &format!("P{i}"), param_flags::AUTOMATABLE)));
        let values = [ParamValue {
            id: 10,
            value: 0.18,
            text: "80 Hz".into(),
        }];
        let params = plugin_params(&infos, |id| values.iter().find(|v| v.id == id));
        assert_eq!(params.len(), metrics::MAX_PARAMS);
        assert_eq!(params[0].name, "P10");
        assert_eq!((params[0].value, params[0].text.as_str()), (0.18, "80 Hz"));
        assert_eq!((params[1].value, params[1].text.as_str()), (0.5, "50 %"));
    }

    #[test]
    fn values_are_formatted_and_snapped() {
        assert_eq!(format_value(0.0, 1.0, 0, 0.4), "40 %");
        assert_eq!(format_value(0.0, 32.0, 32, 7.0), "7");
        assert_eq!(format_value(-60.0, 0.0, 0, -12.0), "−12.00");
        let p = Param {
            id: 0,
            name: "Algorithmus".into(),
            value: 0.0,
            min: 0.0,
            max: 32.0,
            default: 0.0,
            steps: 32,
            text: String::new(),
        };
        assert_eq!(p.snap(7.4), 7.0);
        assert_eq!(p.snap(40.0), 32.0);
        let half = Param {
            steps: 64,
            ..p.clone()
        };
        assert_eq!(half.snap(3.3), 3.5);
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
        let macros: Vec<&str> = pisynth.params.iter().map(|p| p.name.as_str()).collect();
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
