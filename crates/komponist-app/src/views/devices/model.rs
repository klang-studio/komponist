//! The device chain as data: one [`Card`] per device of a track, with its state, size and the
//! parameters its body shows, and the drop arithmetic of reordering. Pure, so the chain is
//! tested without pixels.

use komponist_builtin::Macro;
use komponist_ipc::ParamValue;
use komponist_ipc::control::ParamInfo as WireInfo;
use komponist_model::params::{ParamId, ParamInfo, ParamKind, auto_layout};
use komponist_model::{Device, DeviceId, DeviceRole, Track};
use komponist_plugins::params::{is_listed, param_info};
use komponist_plugins::{DeviceStatus, FailReason};
use komponist_tokens::device;

use super::layout::{self, DeviceLayout};
use super::metrics;
use crate::device_host::{DeviceHost, internal_kind, is_pisynth};
use crate::meters::DeviceLevels;
use crate::spectrum::Analysis;
use crate::ui_state::{PluginWindow, Reveal};
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
    /// Accessible name and tooltip of the ⋯ button (handoff `aria-label="Weitere Aktionen"`).
    pub const MORE: &str = "Weitere Aktionen";
    /// The preset button without a matching factory preset, and the menu's name.
    pub const PRESET: &str = "Preset";
    /// The page switch's accessible name.
    pub const PAGES: &str = "Seiten";
    /// The accessible name of the operator switch on pisynth's envelope page.
    pub const OPERATOR: &str = "Operator";
    /// Context menu of a parameter: set it to its default (the Alt-click).
    pub const TO_DEFAULT: &str = "Auf Standard";
    /// Its shortcut column.
    pub const TO_DEFAULT_SHORTCUT: &str = "Alt-Klick";
    /// Context menu of a parameter: open the text entry.
    pub const ENTER_VALUE: &str = "Wert eingeben";
    /// Accessible name of a parameter's context menu.
    pub const PARAMETER: &str = "Parameter";
    /// Title of the FM display with free routing.
    pub const FM_FREE: &str = "Algorithmus Frei";
    /// Band of an EQ, in the place of a parameter („EQ · Band 2 · Spur 1“).
    pub const BAND: &str = "Band";
    /// The range's „bis“ („20 Hz bis 20 kHz“).
    pub const TO: &str = "bis";
    /// The master channel in the place of a parameter.
    pub const MASTER: &str = "Master";
    /// Context menu: collapse the frame to its 28 px strip.
    pub const COLLAPSE: &str = "Einklappen";
    /// Context menu: expand a collapsed frame.
    pub const EXPAND: &str = "Aufklappen";
    /// Context menu: copy the device behind itself.
    pub const DUPLICATE: &str = "Duplizieren";
    /// Accessible names of the chain's scrollbars.
    pub const SCROLL_H: &str = "Geräte-Kette horizontal scrollen";
    /// … and of the vertical one.
    pub const SCROLL_V: &str = "Geräte-Kette vertikal scrollen";
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

    /// The accessible name of the preset button: „Preset: Glas-Glocke“ (handoff `aria-label`).
    pub fn preset(name: &str) -> String {
        format!("{PRESET}: {name}")
    }

    /// The accessible name of a collapsed frame (handoff `collapsed()`: „… aufklappen“).
    pub fn expand(name: &str) -> String {
        format!("{name} aufklappen")
    }

    /// The accessible name of a chain head entry: „Gerät pisynth anzeigen“.
    pub fn reveal(name: &str) -> String {
        format!("{DEVICE} {name} anzeigen")
    }

    /// The name of a widget kind (handoff §4), shown by the placeholder of the graph (WP-84).
    pub fn widget(kind: super::layout::WidgetKind) -> &'static str {
        use super::layout::WidgetKind as K;
        match kind {
            K::Knob => "Regler",
            K::KnobLarge => "Großer Regler",
            K::Number => "Zahlenfeld",
            K::Segment => "Umschalter",
            K::ToggleButton => "Schalt-Button",
            K::Dropdown => "Dropdown",
            K::MiniFader => "Mini-Fader",
            K::MeterGr => "GR-Meter",
            K::PadGrid => "Pad-Raster",
            K::FmAlgorithm => "FM-Algorithmus",
            K::ColumnSelect => "Spaltenwahl",
            K::Graph => "Graph",
        }
    }

    /// The accessible name of operator column `n`: „Operator 3“.
    pub fn operator(n: usize) -> String {
        format!("{OPERATOR} {n}")
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
    /// A MIDI effect: a note stage left of the instrument (WP-88). Draggable within the MIDI part.
    pub midi_effect: bool,
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
    /// The declarative layout of the body (WP-82): foreign plugins get the auto-layout.
    pub layout: DeviceLayout,
    /// The page shown (set by the view from the view state).
    pub page: usize,
    /// Shown as the 28 px strip (set by the view).
    pub collapsed: bool,
    /// The device has factory presets (pisynth): the head shows the preset dropdown.
    pub presets: bool,
    /// The factory preset the state is, if any.
    pub preset: Option<usize>,
    /// Width in design px (set by the view: page columns, head, collapsed).
    pub width: f32,
    /// Blocks that missed their deadline, while the head shows "zu spät" (set by the view).
    pub late: Option<u64>,
    /// pisynth's routing as the patch has it: the FM display draws it for algorithm 0 („Frei“).
    pub routing: Option<komponist_builtin::algorithms::Routing>,
    /// The levels of the device's gain-reduction meter (set by the view from `Meters`); `None`
    /// for a device without a level feed.
    pub levels: Option<DeviceLevels>,
    /// The device's sample rate and live spectrum (WP-92, set by the view from `Meters`); `None`
    /// for a device without a sample tap.
    pub analysis: Option<Analysis>,
}

impl Card {
    /// The parameter `id` of the body's cells.
    pub fn param(&self, id: &ParamId) -> Option<&Param> {
        self.params.iter().find(|p| p.info.id == *id)
    }

    /// The layout page shown (the last one if `page` is out of range).
    pub fn current_page(&self) -> Option<&layout::Page> {
        self.layout.page(self.page)
    }

    /// The frame's width: 28 collapsed, else the page's columns, but at least `head` (what the
    /// head needs for its parts) and, for a card that shows a message, the 4-column width.
    pub fn frame_width(&self, head: f32) -> f32 {
        if self.collapsed {
            return device::frame::COLLAPSED_W;
        }
        let grid = match self.state {
            CardState::Ok => self.current_page().map_or(0.0, layout::Page::width),
            _ => layout::device_width(metrics::MESSAGE_COLUMNS),
        };
        grid.max(head)
    }

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

/// The parameter the user touched last, as the inspector's "Parameter" section and the status
/// line show it (WP-89).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchedParam {
    /// The device's track (`None`: the master chain).
    pub track: Option<komponist_model::TrackId>,
    /// Parameter name ("Frequenz 2").
    pub name: String,
    /// "EQ · Band 2 · Spur 1": device, band or operator if the id has one, track.
    pub place: String,
    /// The current value as text.
    pub value: String,
    /// The default as text.
    pub default: String,
    /// "20 Hz bis 20 kHz".
    pub range: String,
}

/// The band or operator an id belongs to: `b2_freq` → „Band 2“, `op3_ratio` → „Operator 3“.
pub fn place_of(id: &ParamId) -> Option<String> {
    let (head, _) = id.as_str().split_once('_')?;
    let number = |prefix: &str| {
        head.strip_prefix(prefix)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if let Some(n) = number("op") {
        Some(format!("{} {n}", names::OPERATOR))
    } else {
        number("b").map(|n| format!("{} {n}", names::BAND))
    }
}

/// The text of a parameter's range: „20 Hz bis 20 kHz“, „Aus · An“, the choices of an enum.
pub fn range_text(info: &ParamInfo) -> String {
    match &info.kind {
        ParamKind::Bool => format!("{} · {}", info.format(0.0), info.format(1.0)),
        ParamKind::Enum(choices) => choices.join(" · "),
        _ => {
            let (min, max) = info.range();
            format!("{} {} {}", info.format(min), names::TO, info.format(max))
        }
    }
}

/// What the touched parameter `touch` is now, `None` when its device is gone, not running, or the
/// parameter is unknown to it.
pub fn touched(
    project: &komponist_model::Project,
    host: &dyn DeviceHost,
    windows: &[PluginWindow],
    touch: &crate::ui_state::ParamTouch,
) -> Option<TouchedParam> {
    use komponist_model::ChannelRef;
    let (channel, _, device) = project.find_device(touch.device)?;
    let card = card(device, host, windows);
    let p = card.param(&touch.param)?;
    let (track, track_name) = match channel {
        ChannelRef::Track(t) => (Some(t), project.track(t)?.name.clone()),
        ChannelRef::Master => (None, names::MASTER.to_owned()),
    };
    let mut parts = vec![card.name.clone()];
    parts.extend(place_of(&p.info.id));
    parts.push(track_name);
    Some(TouchedParam {
        track,
        name: p.name().to_owned(),
        place: parts.join(" · "),
        value: p.text.clone(),
        default: p.info.format(p.info.default),
        range: range_text(&p.info),
    })
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
        } else if let Some(kind) = internal_kind(device) {
            internal_params(kind, device)
        } else {
            host.params(device.id)
                .map(|infos| plugin_params(infos, |id| host.param_value(device.id, id)))
                .unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    let instrument = device.role == DeviceRole::Instrument;
    let midi_effect = device.role == DeviceRole::MidiEffect;
    // pisynth and the internal devices bring their own layout; everything else gets the
    // auto-layout.
    let layout = if is_pisynth(device) {
        komponist_builtin::device::layout()
    } else {
        internal_kind(device).map_or_else(
            || {
                DeviceLayout::knob_grid(
                    params.iter().map(|p| p.info.id.clone()),
                    metrics::COLUMNS as u8,
                )
            },
            |kind| kind.layout().clone(),
        )
    };
    #[cfg(debug_assertions)]
    if state == CardState::Ok {
        let known = |id: &ParamId| params.iter().any(|p| p.info.id == *id);
        let errors = layout::validate(&layout, &super::cells::default_context(), &known);
        debug_assert!(
            errors.is_empty(),
            "layout of {}: {errors:?}",
            device.plugin.name
        );
    }
    let presets = is_pisynth(device);
    let preset = presets
        .then(|| {
            let patch = komponist_builtin::decode_state(device.state.as_deref())
                .unwrap_or_else(|_| komponist_builtin::default_patch());
            let encoded = komponist_builtin::encode_state(&patch);
            crate::windows::plugin_frame::factory_states()
                .iter()
                .position(|s| *s == encoded)
        })
        .flatten();
    Card {
        device: device.id,
        name: device.plugin.name.clone(),
        tag: format_tag(device.plugin.format),
        instrument,
        midi_effect,
        bypass: device.bypass,
        warning: host.state_warning(device.id).map(str::to_owned),
        params,
        has_gui: host.has_gui(device.id),
        open: windows.iter().any(|w| w.device == device.id),
        layout,
        page: 0,
        collapsed: false,
        presets,
        preset,
        width: layout::device_width(metrics::COLUMNS as u8),
        late: None,
        levels: None,
        analysis: None,
        routing: presets.then(|| {
            let patch = komponist_builtin::decode_state(device.state.as_deref())
                .unwrap_or_else(|_| komponist_builtin::default_patch());
            komponist_builtin::algorithms::routing(&patch)
        }),
        state,
    }
}

/// The parameters of an internal effect with the values in its device state (the defaults for a
/// state that does not load); the wire id is the index in `kind.params()`.
pub fn internal_params(kind: komponist_builtin::Internal, device: &Device) -> Vec<Param> {
    let values = kind
        .decode_state(device.state.as_deref())
        .unwrap_or_else(|_| kind.defaults());
    kind.params()
        .iter()
        .zip(values)
        .enumerate()
        .map(|(i, (info, value))| Param {
            wire: i as u32,
            text: info.format(value),
            info: info.clone(),
            value,
        })
        .collect()
}

/// The parameters of pisynth's card (`komponist_builtin::device`), read from the patch in the
/// device state; the wire id is `Field::wire`.
pub fn pisynth_params(device: &Device) -> Vec<Param> {
    let patch = komponist_builtin::decode_state(device.state.as_deref())
        .unwrap_or_else(|_| komponist_builtin::default_patch());
    komponist_builtin::device::params()
        .iter()
        .map(|p| {
            let value = p.field.get(&patch);
            Param {
                wire: p.wire,
                text: p.info.format(value),
                info: p.info.clone(),
                value,
            }
        })
        .collect()
}

/// pisynth's 8 macros, read from the patch in the device state (the plugin window's parameter
/// view lists them; the card has its own parameters, [`pisynth_params`]).
pub fn pisynth_macro_params(device: &Device) -> Vec<Param> {
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

/// The `(left, right)` of every frame in the scrolled content (scroll 0): the first starts at
/// `pad`, the others follow with `gap` between them.
pub fn spans(widths: &[f32], pad: f32, gap: f32) -> Vec<(f32, f32)> {
    let mut x = pad;
    widths
        .iter()
        .map(|w| {
            let span = (x, x + w);
            x += w + gap;
            span
        })
        .collect()
}

/// The content's width: padding, the frames with gaps, the drop slot, padding.
pub fn content_width(spans: &[(f32, f32)], pad: f32, gap: f32, drop_slot: f32) -> f32 {
    spans.last().map_or(pad, |(_, r)| r + gap) + drop_slot + pad
}

/// The scroll that places the frame `span` in a view `width` wide, from `scroll`, as `mode` says
/// (not clamped).
pub fn reveal_scroll(span: (f32, f32), mode: Reveal, width: f32, scroll: f32, pad: f32) -> f32 {
    let (left, right) = span;
    match mode {
        Reveal::Center => (left + right) / 2.0 - width / 2.0,
        Reveal::Start => left - pad,
        Reveal::Visible => {
            if right - scroll > width - pad {
                right - (width - pad)
            } else if left - scroll < pad {
                left - pad
            } else {
                scroll
            }
        }
    }
}

/// Whether the card's graph shows a live spectrum when it is on screen: the device runs, is not
/// bypassed or collapsed and has „Analyse“ on (`analyse` ≥ 0.5; a device without that parameter
/// has none).
pub fn shows_spectrum(card: &Card) -> bool {
    card.state == CardState::Ok
        && !card.bypass
        && !card.collapsed
        && card
            .param(&ParamId::new("analyse"))
            .is_some_and(|p| p.value >= 0.5)
}

/// Which frames show at least partly in a view `width` wide scrolled by `scroll`.
pub fn visible(spans: &[(f32, f32)], scroll: f32, width: f32) -> Vec<bool> {
    spans
        .iter()
        .map(|(l, r)| *r > scroll && *l < scroll + width)
        .collect()
}

/// Whether the chain scrolls vertically: the area is lower than the head, the padding and the
/// frame (owner 2026-11-14: a dock below 320 px, which is this plus the tab strip).
pub fn scrolls_vertically(area_height: f32) -> bool {
    area_height < metrics::CHAIN_MIN_HEIGHT
}

/// The insert slot (0 ..= cards) for a pointer at `x`, given each card's `(left, right)`: before
/// the first card whose middle lies right of `x`.
pub fn slot_at(cards: &[(f32, f32)], x: f32) -> usize {
    cards
        .iter()
        .position(|(l, r)| x < (l + r) / 2.0)
        .unwrap_or(cards.len())
}

/// The number of MIDI effects at the head of the chain: the last insert slot a MIDI effect may take.
pub fn midi_slots(cards: &[Card]) -> usize {
    cards.iter().take_while(|c| c.midi_effect).count()
}

/// The first slot an audio effect may take: behind the MIDI effects and the instrument.
pub fn first_effect_slot(cards: &[Card]) -> usize {
    let midi = midi_slots(cards);
    midi + usize::from(cards.get(midi).is_some_and(|c| c.instrument))
}

/// The insert slots `(lo, hi)` the card at `index` may be dropped at: a MIDI effect in front of
/// the instrument, any other effect behind it.
pub fn drop_bounds(cards: &[Card], index: usize) -> (usize, usize) {
    if cards.get(index).is_some_and(|c| c.midi_effect) {
        (0, midi_slots(cards))
    } else {
        (first_effect_slot(cards), usize::MAX)
    }
}

/// The `to_index` of `MoveDevice` (the index without the moved card) for the card at `from`
/// dropped at insert slot `slot`, or `None` when the order stays.
pub fn move_target(from: usize, slot: usize, first: usize) -> Option<usize> {
    move_target_in(from, slot, first, usize::MAX)
}

/// [`move_target`] with the slot kept in `lo ..= hi`.
pub fn move_target_in(from: usize, slot: usize, lo: usize, hi: usize) -> Option<usize> {
    let slot = slot.clamp(lo, hi.max(lo));
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
        let algo = pisynth_macro_params(
            &komponist_model::fixtures::demo_project().tracks[2]
                .channel
                .devices[0],
        )
        .remove(0);
        assert_eq!(algo.name(), "Algorithmus");
        assert_eq!(algo.snap(7.4), 7.0);
        assert_eq!(algo.snap(40.0), 32.0);
        assert_eq!(algo.text_at(0.0), "Frei");
        let ratio = pisynth_macro_params(
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
        // WP-86: ten columns on the first page, eight on the envelope page.
        assert_eq!(pisynth.frame_width(0.0), 612.0, "10 columns");
        let pages: Vec<_> = pisynth
            .layout
            .pages
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert_eq!(pages, ["Operatoren", "Hüllkurven", "LFO", "Global"]);
        assert!(pisynth.presets);
        assert_eq!(pisynth.tag, "INTERN");
        assert_eq!(
            pisynth.params.len(),
            komponist_builtin::device::params().len()
        );
        assert_eq!(pisynth.params[0].name(), "Algorithmus");
        assert_eq!(pisynth.open_label(), Some(names::PARAMS));
        // Without plugin manager the effects are missing; TAL is bypassed.
        assert_eq!(cards[1].state, CardState::Missing);
        assert_eq!(cards[1].frame_width(0.0), 252.0);
        assert_eq!(cards[1].open_label(), None);
        assert!(cards[2].bypass);
        assert_eq!(first_effect_slot(&cards), 1);
    }

    #[test]
    fn frames_are_placed_left_to_right_and_the_content_adds_the_drop_slot() {
        let spans = spans(&[252.0, 28.0, 432.0], 10.0, 8.0);
        assert_eq!(spans, [(10.0, 262.0), (270.0, 298.0), (306.0, 738.0)]);
        // 738 + gap 8 + slot 120 + padding 10.
        assert_eq!(content_width(&spans, 10.0, 8.0, 120.0), 876.0);
        assert_eq!(content_width(&[], 10.0, 8.0, 120.0), 140.0);
        assert_eq!(visible(&spans, 0.0, 280.0), [true, true, false]);
        assert_eq!(visible(&spans, 262.0, 100.0), [false, true, true]);
    }

    #[test]
    fn reveal_scrolls_centre_start_or_the_least_distance() {
        let span = (306.0, 738.0);
        // Centre of the frame (522) in the middle of 400.
        assert_eq!(reveal_scroll(span, Reveal::Center, 400.0, 0.0, 10.0), 322.0);
        assert_eq!(reveal_scroll(span, Reveal::Start, 400.0, 0.0, 10.0), 296.0);
        // Visible: unchanged when it fits, else the nearest edge.
        assert_eq!(
            reveal_scroll((10.0, 262.0), Reveal::Visible, 400.0, 0.0, 10.0),
            0.0
        );
        assert_eq!(
            reveal_scroll(span, Reveal::Visible, 400.0, 0.0, 10.0),
            348.0
        );
        assert_eq!(
            reveal_scroll((10.0, 262.0), Reveal::Visible, 400.0, 100.0, 10.0),
            0.0
        );
    }

    #[test]
    fn a_frame_is_as_wide_as_the_columns_of_its_page() {
        use super::layout::Page;
        let project = demo_project();
        let mut card = card(
            &project.tracks[2].channel.devices[0],
            &BuiltinHost::new(),
            &[],
        );
        card.state = CardState::Ok;
        let page = |name: &str, cols| Page {
            name: name.into(),
            cols,
            cells: Vec::new(),
            dividers: Vec::new(),
        };
        card.layout = DeviceLayout {
            pages: vec![page("Operatoren", 10), page("Hüllkurven", 8)],
        };
        assert_eq!(card.frame_width(0.0), 612.0);
        card.page = 1;
        assert_eq!(
            card.frame_width(0.0),
            492.0,
            "the chain moves up with the page"
        );
        // The head widens a frame that is too narrow for it; collapsed is 28 whatever the page.
        assert_eq!(card.frame_width(700.0), 700.0);
        card.collapsed = true;
        assert_eq!(card.frame_width(700.0), 28.0);
    }

    #[test]
    fn the_chain_scrolls_vertically_below_the_320_px_dock() {
        // Dock 320 = tab strip 28 + area 292: head 28 + padding 2 × 10 + frame 244.
        assert_eq!(metrics::CHAIN_MIN_HEIGHT, 292.0);
        assert!(!scrolls_vertically(292.0));
        assert!(scrolls_vertically(291.0));
        assert_eq!(
            metrics::CHAIN_MIN_HEIGHT + komponist_tokens::size::TAB,
            320.0
        );
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

    #[test]
    fn the_place_of_a_parameter_names_its_band_or_operator() {
        let place = |id: &str| place_of(&ParamId::new(id));
        assert_eq!(place("b2_freq").as_deref(), Some("Band 2"));
        assert_eq!(place("op3_ratio").as_deref(), Some("Operator 3"));
        assert_eq!(place("op12_level").as_deref(), Some("Operator 12"));
        for none in ["gain", "feedback", "b_freq", "bass_level", "op_x", "p17"] {
            assert_eq!(place(none), None, "{none}");
        }
    }

    #[test]
    fn the_range_reads_as_text() {
        use komponist_model::params::Unit;
        let hz =
            ParamInfo::new("f", "Frequenz", ParamKind::lin(20.0, 20_000.0), 1_000.0).unit(Unit::Hz);
        assert_eq!(range_text(&hz), "20 Hz bis 20 kHz");
        let on = ParamInfo::new("b", "Schalter", ParamKind::Bool, 0.0);
        assert_eq!(range_text(&on), "Aus · Ein");
        let mode = ParamInfo::new(
            "m",
            "Modus",
            ParamKind::Enum(vec!["Auf".into(), "Ab".into()]),
            0.0,
        );
        assert_eq!(range_text(&mode), "Auf · Ab");
    }
    #[test]
    fn the_spectrum_shows_only_for_a_running_eq_with_analyse_on() {
        use komponist_builtin::Internal;
        let device = |state: Option<Vec<u8>>, bypass| Device {
            id: DeviceId(1),
            role: DeviceRole::Effect,
            plugin: Internal::Eq.plugin_ref(),
            bypass,
            state: state.map(Into::into),
        };
        let host = BuiltinHost::new();
        // A project saved before „Analyse“ existed has no such parameter in its state: on.
        let old = br#"{"params": {"out": 0.0}}"#.to_vec();
        let on = card(&device(Some(old), false), &host, &[]);
        assert!(shows_spectrum(&on));
        let off_state = Internal::Eq.with_value(None, 31, 0.0).unwrap();
        assert_eq!(Internal::Eq.wire(&ParamId::new("analyse")), Some(31));
        assert!(!shows_spectrum(&card(
            &device(Some(off_state), false),
            &host,
            &[]
        )));
        assert!(!shows_spectrum(&card(&device(None, true), &host, &[])));
        let mut collapsed = on.clone();
        collapsed.collapsed = true;
        assert!(!shows_spectrum(&collapsed));
        // Devices without the parameter have none.
        let delay = Device {
            plugin: Internal::Delay.plugin_ref(),
            ..device(None, false)
        };
        assert!(!shows_spectrum(&card(&delay, &host, &[])));
    }
}
