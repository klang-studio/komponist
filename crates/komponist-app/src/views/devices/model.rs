//! The device chain as data: one [`Card`] per device of a track, with its state, size and the
//! parameters its body shows, and the drop arithmetic of reordering. Pure, so the chain is
//! tested without pixels.

use komponist_builtin::{BuiltinKind, Internal};
use komponist_device_core::pinned_clap_state;
use komponist_hexa::Macro;
use komponist_i18n::Lang;
use komponist_ipc::ParamValue;
use komponist_ipc::control::ParamInfo as WireInfo;
use komponist_ipc::native_layout::{DisplayScale, DisplayUnit, NativeLayout};
use komponist_model::automation::AutomationOwner;
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit, auto_layout};
use komponist_model::{Device, DeviceId, DeviceRole, Track};
use komponist_plugins::params::{is_listed, param_info};
use komponist_plugins::{DeviceStatus, FailReason};
use komponist_tokens::device;

use super::layout::{self, DeviceLayout};
use super::metrics;
use crate::device_host::{DeviceHost, LegacyParameters, internal_kind, is_hexa};
use crate::gain_trace::Dynamics;
use crate::meters::DeviceLevels;
use crate::spectrum::Analysis;
use crate::ui_state::{PluginWindow, Reveal};
use crate::views::format_tag;

/// Marks a loaded preset whose values were changed since (as in the plugin window's header).
pub const CHANGED: &str = " *";

/// The name of a widget kind (handoff §4), shown by the placeholder of the graph (WP-84).
pub fn widget_name(lang: Lang, kind: super::layout::WidgetKind) -> &'static str {
    use super::layout::WidgetKind as K;
    match kind {
        K::Knob => lang.devices_widget_knob(),
        K::KnobLarge => lang.devices_widget_knob_large(),
        K::Number => lang.devices_widget_number(),
        K::Segment => lang.devices_widget_segment(),
        K::ToggleButton => lang.devices_widget_toggle_button(),
        K::Dropdown => lang.devices_widget_dropdown(),
        K::MiniFader => lang.devices_widget_mini_fader(),
        K::MeterGr => lang.devices_widget_meter_gr(),
        K::PadGrid => lang.devices_widget_pad_grid(),
        K::FmAlgorithm => lang.devices_widget_fm_algorithm(),
        K::ColumnSelect => lang.devices_widget_column_select(),
        K::Graph => lang.devices_widget_graph(),
        K::Canvas => lang.devices_widget_canvas(),
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
pub fn failure_texts(lang: Lang, reason: &FailReason) -> (&'static str, String) {
    match reason {
        FailReason::Exited { .. } => (
            lang.devices_crashed(),
            lang.devices_crashed_text().to_owned(),
        ),
        FailReason::LoadFailed(why) => {
            let why = why.trim();
            let text = if why.is_empty() {
                lang.devices_load_failed_text().to_owned()
            } else {
                format!("{} ({why})", lang.devices_load_failed_text())
            };
            (lang.devices_load_failed(), text)
        }
        FailReason::Unresponsive => (
            lang.devices_unresponsive(),
            lang.devices_unresponsive_text().to_owned(),
        ),
    }
}

/// A knob of a card: the parameter's description in the parameter model plus its current value.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    /// The id the device is addressed by (`Command::SetParam::id`: hexa's macro id, the
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
    /// The project address when this card presents bundled CLAP controls with author IDs.
    pub(super) fn project_id(&self, bundled_editor: bool) -> ParamId {
        if bundled_editor {
            ParamId::plugin(self.wire)
        } else {
            self.info.id.clone()
        }
    }

    /// The display value at an automation or macro position in the project's parameter domain.
    pub(super) fn value_at_normalized(&self, bundled_editor: bool, position: f64) -> f64 {
        if bundled_editor {
            let (min, max) = pinned_clap_state::clap_range(&self.info);
            pinned_clap_state::from_clap(&self.info, min + position * (max - min))
        } else {
            self.info.to_plain(position)
        }
    }

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
    /// Owning track or master when this card belongs to a project.
    pub automation_owner: Option<AutomationOwner>,
    /// Parameters driven by a curve at the playhead, with the colour of their automation dot.
    pub automation: std::collections::BTreeMap<ParamId, egui::Color32>,
    pub(crate) macros: std::collections::BTreeMap<ParamId, crate::views::track_macros::Control>,
    /// Whether this track is in macro mapping mode.
    pub(crate) macro_mapping: bool,
    /// The number of macro assignments to this plugin (including unresolved parameters).
    pub(crate) macro_mappings: usize,
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
    pub(crate) native_layout: Option<std::sync::Arc<komponist_ipc::native_layout::NativeLayout>>,
    pub(super) display: Option<std::sync::Arc<komponist_ipc::device_canvas::DisplayList>>,
    /// Live file slots with their host-owned link status.
    pub(super) asset_slots: Vec<(komponist_ipc::device_assets::Slot, bool)>,
    pub(super) asset_waveforms: std::collections::BTreeMap<u32, super::asset_waveform::Waveform>,
    pub(super) asset_names: std::collections::BTreeMap<u32, String>,
    pub(super) pad_activity: Option<komponist_ipc::signal_taps::PadActivity>,
    /// Whether a verified bundle uses the app's parameter model with author-ID bindings.
    pub(super) bundled_editor: bool,
    /// The declarative layout of the body, with an automatic layout for generic plugins.
    pub layout: DeviceLayout,
    /// The page shown (set by the view from the view state).
    pub page: usize,
    /// Shown as the 28 px strip (set by the view).
    pub collapsed: bool,
    /// Whether to show the preset dropdown.
    pub presets: bool,
    pub(super) user_presets: bool,
    /// The selected entry in the preset inventory.
    pub preset: Option<usize>,
    pub(super) preset_name: Option<String>,
    /// The device has a sidechain input: the head shows the "SC" chip (WP-104). Internal devices
    /// say so in their spec, plugins through [`DeviceHost::has_sidechain`].
    pub has_sidechain: bool,
    /// The sidechain source and tap the device has now, if any.
    pub sidechain: Option<komponist_model::Sidechain>,
    /// The loaded internal preset was changed since („ *“): a value differs from it now.
    pub preset_changed: bool,
    /// Width in design px (set by the view: page columns, head, collapsed).
    pub width: f32,
    /// Blocks that missed their deadline, while the head shows "zu spät" (set by the view).
    pub late: Option<u64>,
    /// hexa's routing as the patch has it: the FM display draws it for algorithm 0 („Frei“).
    pub routing: Option<komponist_hexa::algorithms::Routing>,
    /// The levels of the device's gain-reduction meter (set by the view from `Meters`); `None`
    /// for a device without a level feed.
    pub levels: Option<DeviceLevels>,
    /// Per-cell levels from named gain-reduction taps on the current native page.
    pub(super) native_meter_levels: std::collections::BTreeMap<u8, DeviceLevels>,
    /// The device's sample rate and live spectrum (WP-92, set by the view from `Meters`); `None`
    /// for a device without a sample tap.
    pub analysis: Option<Analysis>,
    /// The live level and reduction history of a compressor (WP-93, set by the view from
    /// `Meters`); `None` for a device without a compressor graph.
    pub dynamics: Option<Dynamics>,
    pub(super) graph_signals: std::collections::BTreeMap<(u8, u8), GraphSignals>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct GraphSignals {
    pub(super) analysis: Option<Analysis>,
    pub(super) dynamics: Option<Dynamics>,
    pub(super) playhead: Option<f32>,
    pub(super) markers: Option<komponist_ipc::waveform_markers::Snapshot>,
}

impl Card {
    pub(super) fn pad_bank(&self) -> Option<&komponist_ipc::native_layout::PadBank> {
        self.native_layout.as_ref()?.pad_bank.as_ref()
    }

    pub(super) fn has_page_switch(&self) -> bool {
        self.layout.pages.len() > 1 && self.pad_bank().is_none()
    }

    pub(super) fn pad_for_asset(&self, slot: Option<u32>) -> Option<usize> {
        let slot = slot?;
        self.pad_bank()?
            .pads
            .iter()
            .position(|pad| pad.asset_slot == slot)
    }

    /// The parameter `id` of the body's cells.
    pub fn param(&self, id: &ParamId) -> Option<&Param> {
        self.params
            .iter()
            .find(|p| p.info.id == *id || (self.bundled_editor && id.plugin_id() == Some(p.wire)))
    }

    pub(super) fn wire_value(&self, param: &Param, plain: f64) -> f64 {
        if self.bundled_editor {
            pinned_clap_state::to_clap(&param.info, plain)
        } else if self.has_normalized_display(param) {
            param.info.to_normalized(plain)
        } else {
            plain
        }
    }

    pub(super) fn has_normalized_display(&self, param: &Param) -> bool {
        !self.bundled_editor
            && self
                .native_layout
                .as_ref()
                .is_some_and(|layout| layout.parameter_display(param.wire).is_some())
    }

    pub(super) fn native_page(&self) -> Option<&komponist_ipc::native_layout::Page> {
        let pages = &self.native_layout.as_ref()?.pages;
        pages.get(self.page).or_else(|| pages.last())
    }

    pub(super) fn native_cell(
        &self,
        cell: &layout::Cell,
    ) -> Option<&komponist_ipc::native_layout::Cell> {
        let page = self.native_page()?;
        let half = cell.half.map(|half| match half {
            layout::Half::Top => komponist_ipc::native_layout::Half::Top,
            layout::Half::Bottom => komponist_ipc::native_layout::Half::Bottom,
        });
        page.cells
            .iter()
            .find(|native| native.row == cell.row && native.col == cell.col && native.half == half)
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
    pub fn open_label(&self, lang: Lang) -> Option<&'static str> {
        match self.state {
            CardState::Ok if self.open => Some(lang.devices_opened()),
            CardState::Ok if self.has_gui => Some(lang.devices_open()),
            CardState::Ok => Some(lang.devices_params()),
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
pub fn place_of(lang: Lang, id: &ParamId) -> Option<String> {
    let (head, _) = id.as_str().split_once('_')?;
    let number = |prefix: &str| {
        head.strip_prefix(prefix)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    };
    if let Some(n) = number("op") {
        Some(lang.devices_operator_n(n))
    } else {
        number("b").map(|n| format!("{} {n}", lang.devices_band()))
    }
}

/// The text of a parameter's range: „20 Hz bis 20 kHz“, „Aus · An“, the choices of an enum.
pub fn range_text(lang: Lang, info: &ParamInfo) -> String {
    match &info.kind {
        ParamKind::Bool => format!("{} · {}", info.format(0.0), info.format(1.0)),
        ParamKind::Enum(choices) => choices.join(" · "),
        _ => {
            let (min, max) = info.range();
            lang.devices_range(info.format(min), info.format(max))
        }
    }
}

/// What the touched parameter `touch` is now, `None` when its device is gone, not running, or the
/// parameter is unknown to it.
pub fn touched(
    lang: Lang,
    project: &komponist_model::Project,
    host: &dyn DeviceHost,
    windows: &[PluginWindow],
    touch: &crate::ui_state::ParamTouch,
) -> Option<TouchedParam> {
    use komponist_model::ChannelRef;
    let (channel, _, device) = project.find_device(touch.device)?;
    let card = card(lang, device, host, windows);
    let p = card.param(&touch.param)?;
    let (track, track_name) = match channel {
        ChannelRef::Track(t) => (Some(t), project.track(t)?.name.clone()),
        ChannelRef::Master => (None, lang.devices_master().to_owned()),
    };
    let mut parts = vec![card.name.clone()];
    let place = place_of(lang, &p.info.id).or_else(|| {
        card.native_layout
            .as_ref()?
            .pages
            .iter()
            .flat_map(|page| &page.cells)
            .find_map(|cell| {
                cell.band_controls
                    .iter()
                    .position(|binding| binding.param_id == p.wire)
            })
            .map(|index| format!("{} {}", lang.devices_band(), index + 1))
    });
    parts.extend(place);
    parts.push(track_name);
    Some(TouchedParam {
        track,
        name: p.name().to_owned(),
        place: parts.join(" · "),
        value: p.text.clone(),
        default: p.info.format(p.info.default),
        range: range_text(lang, &p.info),
    })
}

/// The cards of `track`'s chain, left to right.
pub fn cards(
    lang: Lang,
    track: &Track,
    host: &dyn DeviceHost,
    windows: &[PluginWindow],
) -> Vec<Card> {
    track
        .channel
        .devices
        .iter()
        .map(|d| card(lang, d, host, windows))
        .collect()
}

pub(crate) fn card(
    lang: Lang,
    device: &Device,
    host: &dyn DeviceHost,
    windows: &[PluginWindow],
) -> Card {
    let state = CardState::from(host.status(device.id));
    let native_layout =
        if state == CardState::Ok && device.plugin.format == komponist_model::PluginFormat::Clap {
            host.native_layout(device.id).cloned()
        } else {
            None
        };
    let verified = host.verified_bundled_kind(device.id);
    let editor = native_layout.as_ref().and_then(|native| {
        if native.has_parameter_display() {
            return None;
        }
        let legacy = native.schema_version < 2;
        match verified? {
            BuiltinKind::Hexa if legacy => Some((
                Some(hexa_editor_layout(lang)),
                bundled_params(device.id, host, native, hexa_editor_infos(lang))?,
            )),
            BuiltinKind::Internal(kind) => {
                let layout = match kind {
                    // The native panel owns the layout. The existing parameter model
                    // supplies scales and localized value text for gestures and previews.
                    _ if native.has_audio_asset_editor() => None,
                    Internal::Hall => None,
                    _ if legacy && kind.clap_v1_grid_fallback() => {
                        Some(kind.layout_in(lang).clone())
                    }
                    _ if kind.clap_v2_legacy_parameters() => None,
                    _ => return None,
                };
                Some((
                    layout,
                    bundled_params(device.id, host, native, kind.params_in(lang))?,
                ))
            }
            _ => None,
        }
    });
    let bundled_editor = editor.is_some();
    let (editor_layout, editor_params) = editor.unzip();
    let editor_layout = editor_layout.flatten();
    let legacy_layout = editor_layout.is_some();
    let params = if let Some(params) = editor_params {
        params
    } else if state == CardState::Ok {
        if is_hexa(device) {
            hexa_params(lang, device)
        } else if let Some(kind) = internal_kind(device) {
            internal_params(lang, kind, device)
        } else {
            host.params(device.id)
                .map(|infos| {
                    let value = |id| host.param_value(device.id, id);
                    if let Some(native) = &native_layout {
                        let ids: std::collections::BTreeSet<_> = native
                            .pages
                            .iter()
                            .flat_map(|page| {
                                page.cells.iter().flat_map(|cell| cell.parameter_ids())
                            })
                            .collect();
                        infos
                            .iter()
                            .filter(|info| ids.contains(&info.id))
                            .map(|info| native_param(lang, native, info, value(info.id)))
                            .collect()
                    } else {
                        plugin_params(lang, infos, value)
                    }
                })
                .unwrap_or_default()
        }
    } else {
        Vec::new()
    };
    let instrument = device.role == DeviceRole::Instrument;
    let midi_effect = device.role == DeviceRole::MidiEffect;
    let layout = if let Some(layout) = editor_layout {
        layout
    } else if let Some(native) = &native_layout {
        native_card_layout(native, lang)
    } else if is_hexa(device) {
        komponist_hexa::device::layout_in(lang)
    } else {
        internal_kind(device).map_or_else(
            || {
                DeviceLayout::knob_grid(
                    params.iter().map(|p| p.info.id.clone()),
                    metrics::COLUMNS as u8,
                )
            },
            |kind| kind.layout_in(lang).clone(),
        )
    };
    #[cfg(debug_assertions)]
    if state == CardState::Ok {
        let known = |id: &ParamId| {
            params
                .iter()
                .any(|p| p.info.id == *id || (bundled_editor && id.plugin_id() == Some(p.wire)))
        };
        let mut errors = layout::validate(&layout, &super::cells::default_context(), &known);
        if native_layout
            .as_ref()
            .is_some_and(|native| native.pad_bank.is_some())
        {
            errors.retain(|error| !matches!(error, layout::LayoutError::TooManyPages(_)));
        }
        debug_assert!(
            errors.is_empty(),
            "layout of {}: {errors:?}",
            device.plugin.name
        );
    }
    let hexa = is_hexa(device);
    let bundled_hexa = verified == Some(BuiltinKind::Hexa);
    let hexa_patch = if hexa {
        komponist_hexa::decode_state(device.state.as_deref()).ok()
    } else if bundled_hexa {
        device
            .state
            .as_deref()
            .and_then(komponist_hexa::state::decode_clap_patch)
    } else {
        None
    };
    let hexa_patch =
        (hexa || bundled_hexa).then(|| hexa_patch.unwrap_or_else(komponist_hexa::default_patch));
    let preset_entries = host.clap_presets(device);
    let selected = host.saved_preset(device, preset_entries);
    Card {
        device: device.id,
        automation_owner: None,
        automation: Default::default(),
        macros: Default::default(),
        macro_mapping: false,
        macro_mappings: 0,
        name: crate::views::catalog_plugin_name(lang, host.catalog(), &device.plugin),
        tag: format_tag(lang, device.plugin.format),
        instrument,
        midi_effect,
        bypass: device.bypass,
        warning: host.state_warning(device.id).map(str::to_owned),
        params,
        has_gui: host.has_gui(device.id),
        open: windows.iter().any(|w| w.device == device.id),
        layout,
        native_layout: if legacy_layout { None } else { native_layout },
        display: host.device_display(device.id).cloned(),
        asset_slots: host.asset_slots(device.id).map_or_else(Vec::new, |assets| {
            assets
                .slots
                .iter()
                .map(|slot| (slot.clone(), device.asset_links.contains_key(&slot.id)))
                .collect()
        }),
        asset_waveforms: Default::default(),
        asset_names: device
            .asset_links
            .iter()
            .filter_map(|(&id, link)| Some((id, link.name()?.into_owned())))
            .collect(),
        pad_activity: None,
        bundled_editor,
        page: 0,
        collapsed: false,
        presets: !preset_entries.is_empty(),
        user_presets: host.supports_user_presets(device.id),
        preset: selected.map(|(index, _)| index),
        preset_name: selected
            .and_then(|(index, _)| preset_entries.get(index))
            .map(|preset| preset.display_name(lang.code()).to_owned()),
        preset_changed: selected.is_some_and(|(_, changed)| changed),
        has_sidechain: internal_kind(device)
            .map_or_else(|| host.has_sidechain(device.id), |k| k.has_sidechain()),
        sidechain: device.sidechain,
        width: layout::device_width(metrics::COLUMNS as u8),
        late: None,
        levels: None,
        native_meter_levels: Default::default(),
        analysis: None,
        dynamics: None,
        graph_signals: Default::default(),
        routing: hexa_patch.as_ref().map(komponist_hexa::algorithms::routing),
        state,
    }
}

fn hexa_editor_infos(lang: Lang) -> &'static [ParamInfo] {
    use std::sync::OnceLock;

    static INFOS: [OnceLock<Vec<ParamInfo>>; Lang::ALL.len()] =
        [const { OnceLock::new() }; Lang::ALL.len()];
    INFOS[lang as usize].get_or_init(|| {
        use komponist_hexa::device::{Field, WIRE_COUNT};
        let patch = komponist_hexa::default_patch();
        (0..WIRE_COUNT)
            .map(|wire| {
                let field = Field::from_wire(wire as u32).expect("valid Hexa wire ID");
                let mut info = field.info_in(lang);
                info.default = info.snap(field.get(&patch));
                info
            })
            .collect()
    })
}

fn hexa_editor_layout(lang: Lang) -> DeviceLayout {
    let mut layout = komponist_hexa::device::layout_in(lang);
    // Keep all of the CLAP controls accessible alongside the full operators and envelopes.
    let global = &mut layout.pages[3];
    global.cells.extend(
        Macro::ALL
            .into_iter()
            .filter(|item| !matches!(item, Macro::Algorithm | Macro::Feedback))
            .enumerate()
            .map(|(col, item)| layout::Cell::knob(col as u8, 0, ParamId::new(item.key()))),
    );
    layout.pages.retain(|page| !page.cells.is_empty());
    layout
}

/// The parameters of an internal effect with the values in its device state (the defaults for a
/// state that does not load); the wire id is the index in `kind.params()`.
pub fn internal_params(
    lang: Lang,
    kind: komponist_builtin::Internal,
    device: &Device,
) -> Vec<Param> {
    let values = kind
        .decode_state(device.state.as_deref())
        .unwrap_or_else(|_| kind.defaults());
    kind.params_in(lang)
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

/// The parameters of hexa's card (`komponist_hexa::device`), read from the patch in the
/// device state; the wire id is `Field::wire`.
pub fn hexa_params(lang: Lang, device: &Device) -> Vec<Param> {
    let patch = komponist_hexa::decode_state(device.state.as_deref())
        .unwrap_or_else(|_| komponist_hexa::default_patch());
    komponist_hexa::device::params_in(lang)
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

/// The standalone frame parameters read from a legacy device's saved state.
pub fn legacy_frame_params(lang: Lang, device: &Device) -> Vec<Param> {
    LegacyParameters::new(device)
        .map(|parameters| parameters.frame_params(lang))
        .unwrap_or_default()
        .into_iter()
        .map(|(param, text)| Param {
            wire: param.wire,
            info: param.info,
            value: param.value,
            text,
        })
        .collect()
}

pub(super) fn native_card_layout(
    native: &komponist_ipc::native_layout::NativeLayout,
    lang: Lang,
) -> DeviceLayout {
    use komponist_ipc::native_layout::{GraphKind, Half, Widget};
    use layout::{Cell, CellParam, GraphBinding, GraphConfig, Page, WidgetKind};
    DeviceLayout {
        pages: native
            .pages
            .iter()
            .map(|page| Page {
                name: native
                    .label(&page.labels, lang.code())
                    .unwrap_or_default()
                    .to_owned(),
                cols: page.columns,
                cells: page
                    .cells
                    .iter()
                    .map(|cell| {
                        if let Some(graph) = &cell.graph {
                            let config = match graph.kind {
                                GraphKind::Envelope => GraphConfig::Envelope,
                                GraphKind::Eq => GraphConfig::Eq {
                                    bands: graph.bands.expect("validated band count"),
                                },
                                GraphKind::Filter => GraphConfig::Filter,
                                GraphKind::Transfer => GraphConfig::Transfer,
                                GraphKind::Waveform => GraphConfig::Waveform,
                                GraphKind::Xy => GraphConfig::Xy,
                                GraphKind::Lfo => GraphConfig::Lfo {
                                    points: graph.points.expect("validated point count"),
                                },
                                GraphKind::Compressor => GraphConfig::Compressor,
                                GraphKind::GrHistory => GraphConfig::GrHistory,
                            };
                            let mut binding = GraphBinding::new(config);
                            if graph.select_band {
                                binding = binding.selecting(layout::BAND);
                            }
                            binding.bind = graph
                                .slots
                                .iter()
                                .map(|(slot, id)| {
                                    (slot.clone(), ParamId::plugin(*id).as_str().to_owned())
                                })
                                .collect();
                            return Cell::span(
                                cell.col,
                                cell.row,
                                cell.cols,
                                cell.rows,
                                WidgetKind::Graph,
                                CellParam::Graph(binding),
                            );
                        }
                        let widget = match cell.widget {
                            Widget::Knob => WidgetKind::Knob,
                            Widget::KnobLarge => WidgetKind::KnobLarge,
                            Widget::Number => WidgetKind::Number,
                            Widget::ToggleButton => WidgetKind::ToggleButton,
                            Widget::Segment => WidgetKind::Segment,
                            Widget::Dropdown => WidgetKind::Dropdown,
                            Widget::MiniFader => WidgetKind::MiniFader,
                            Widget::Graph => unreachable!("validated graph binding"),
                            Widget::Canvas | Widget::MeterGr | Widget::PadGrid => {
                                let widget = match cell.widget {
                                    Widget::Canvas => WidgetKind::Canvas,
                                    Widget::MeterGr => WidgetKind::MeterGr,
                                    _ => WidgetKind::PadGrid,
                                };
                                return Cell::span(
                                    cell.col,
                                    cell.row,
                                    cell.cols,
                                    cell.rows,
                                    widget,
                                    CellParam::None,
                                );
                            }
                        };
                        let param = CellParam::Id(ParamId::plugin(
                            cell.param_id
                                .or_else(|| {
                                    cell.band_controls.first().map(|binding| binding.param_id)
                                })
                                .expect("validated control binding"),
                        ));
                        let mut model_cell = match cell.half {
                            Some(half) => Cell::half(
                                cell.col,
                                cell.row,
                                cell.cols,
                                match half {
                                    Half::Top => layout::Half::Top,
                                    Half::Bottom => layout::Half::Bottom,
                                },
                                widget,
                                param,
                            ),
                            None => {
                                Cell::span(cell.col, cell.row, cell.cols, cell.rows, widget, param)
                            }
                        };
                        model_cell.disabled_by = cell.disabled_by.map(ParamId::plugin);
                        model_cell
                    })
                    .collect(),
                dividers: page.dividers.clone(),
            })
            .collect(),
    }
}

/// Bind an authored device editor to the authoritative CLAP inventory. The editor uses the
/// device's localized names and units, while events and automation use the live CLAP wire IDs.
pub(super) fn bundled_params<'a>(
    device: DeviceId,
    host: &dyn DeviceHost,
    native: &komponist_ipc::native_layout::NativeLayout,
    infos: impl IntoIterator<Item = &'a ParamInfo>,
) -> Option<Vec<Param>> {
    let ids = native.author_ids()?;
    let inventory = host.params(device)?;
    infos
        .into_iter()
        .map(|info| {
            let id = *ids.get(info.id.as_str())?;
            let wire = inventory.iter().find(|wire| wire.id == id)?;
            if !is_listed(wire)
                || (wire.min, wire.max) != pinned_clap_state::clap_range(info)
                || wire.step_count != info.steps()
            {
                return None;
            }
            let raw = host
                .param_value(device, id)
                .map_or(wire.default, |p| p.value);
            // Use the exact device default when the wrapper still has its rounded f32 default.
            let value = if raw == wire.default {
                info.default
            } else {
                pinned_clap_state::from_clap(info, raw)
            };
            Some(Param {
                wire: id,
                info: info.clone(),
                value,
                text: info.format(value),
            })
        })
        .collect()
}

/// The knobs of a foreign plugin's card: the auto-layout of the parameters a generic view lists
/// (not hidden, not read-only, not the plugin's own bypass): its macros if it reports any, else
/// the first 12, with their last known value and text.
pub fn plugin_params<'a>(
    lang: Lang,
    infos: &[WireInfo],
    value: impl Fn(u32) -> Option<&'a ParamValue>,
) -> Vec<Param> {
    let listed: Vec<&WireInfo> = infos.iter().filter(|p| is_listed(p)).collect();
    let models: Vec<ParamInfo> = listed.iter().map(|p| plugin_info(lang, p)).collect();
    auto_layout(&models)
        .into_iter()
        .map(|i| plugin_param(listed[i], models[i].clone(), value(listed[i].id)))
        .collect()
}

/// Every parameter a generic view lists, for the plugin frame's grid (no auto-layout limit).
pub fn all_plugin_params<'a>(
    lang: Lang,
    infos: &[WireInfo],
    value: impl Fn(u32) -> Option<&'a ParamValue>,
) -> Vec<Param> {
    infos
        .iter()
        .filter(|p| is_listed(p))
        .map(|p| plugin_param(p, plugin_info(lang, p), value(p.id)))
        .collect()
}

/// A bridged plugin's parameter: its own names, our value words ("Off", "40%").
fn plugin_info(lang: Lang, wire: &WireInfo) -> ParamInfo {
    param_info(wire).texts(komponist_device_core::value_texts(lang))
}

/// Apply the panel's display domain while retaining the live CLAP ID and flags.
pub(super) fn native_param(
    lang: Lang,
    layout: &NativeLayout,
    wire: &WireInfo,
    known: Option<&ParamValue>,
) -> Param {
    let mut info = plugin_info(lang, wire);
    if !layout.has_parameter_display() && !layout.has_eq_band_controls() {
        return plugin_param(wire, info, known);
    }
    if let Some((labels, choices)) =
        layout
            .pages
            .iter()
            .flat_map(|page| &page.cells)
            .find_map(|cell| {
                if cell.param_id == Some(wire.id) {
                    Some((cell.labels.as_ref(), cell.choices.as_deref()))
                } else {
                    cell.band_controls
                        .iter()
                        .find(|binding| binding.param_id == wire.id)
                        .map(|binding| (binding.labels.as_ref(), binding.choices.as_deref()))
                }
            })
    {
        if let Some(label) = labels.and_then(|labels| layout.label(labels, lang.code())) {
            info.name = label.to_owned();
        }
        if let Some(choices) = choices
            && choices.first().is_some_and(|choice| choice.value == 0)
        {
            info.kind = ParamKind::Enum(
                choices
                    .iter()
                    .map(|choice| {
                        layout
                            .label(
                                choice.full_labels.as_ref().unwrap_or(&choice.labels),
                                lang.code(),
                            )
                            .unwrap_or_default()
                            .to_owned()
                    })
                    .collect(),
            );
        }
    }
    let raw = known.map_or(wire.default, |value| value.value);
    let value = if let Some(display) = layout.parameter_display(wire.id) {
        let (min, max) = display.range();
        let scale = match display.scale {
            DisplayScale::Linear => Scale::Lin,
            DisplayScale::Logarithmic => Scale::Log,
        };
        info.kind = ParamKind::scaled(min, max, scale);
        info.unit = match display.unit {
            DisplayUnit::None => Unit::None,
            DisplayUnit::Hertz => Unit::Hz,
            DisplayUnit::Milliseconds => Unit::Ms,
            DisplayUnit::Percent => Unit::Percent,
            DisplayUnit::Decibels => Unit::Db,
            DisplayUnit::DecibelWidth => Unit::DbWidth,
            DisplayUnit::Ratio => Unit::Ratio,
        };
        info.default = display.default_value();
        if raw == wire.default {
            // A normalized CLAP default can be rounded to single precision.
            info.default
        } else {
            info.to_plain(raw)
        }
    } else if matches!(info.kind, ParamKind::Bool | ParamKind::Enum(_)) {
        raw
    } else {
        return plugin_param(wire, info, known);
    };
    Param {
        wire: wire.id,
        text: info.format(value),
        info,
        value,
    }
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

/// Whether the card has a compressor graph: the transfer curve with a live level or the reduction
/// history.
pub fn has_dynamics_graph(card: &Card) -> bool {
    card.layout.pages.iter().any(|page| {
        page.cells.iter().any(|c| {
            matches!(
                &c.param,
                layout::CellParam::Graph(b)
                    if matches!(
                        b.config,
                        layout::GraphConfig::Compressor | layout::GraphConfig::GrHistory
                    )
            )
        })
    })
}

/// Whether that graph shows live data when it is on screen: the device runs and is neither
/// bypassed nor collapsed.
pub fn shows_dynamics(card: &Card) -> bool {
    card.state == CardState::Ok && !card.bypass && !card.collapsed && has_dynamics_graph(card)
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

/// Where a plugin of some role goes in a chain, and how the chain marks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// Insert line at this slot.
    Line(usize),
    /// Replace the instrument (ring around its card, at this index).
    Instrument(usize),
}

impl Placement {
    /// The index for `PluginAction::Add`'s `DropTarget::Chain`.
    pub fn index(self) -> usize {
        match self {
            Placement::Line(i) | Placement::Instrument(i) => i,
        }
    }
}

/// The one rule for adding a plugin of `role` at `slot` (the slot under the pointer, or the chain's
/// length for the end): an instrument replaces the instrument, or goes in front of the audio
/// effects of a track without one; a MIDI effect stays in the MIDI part (`midi` = its length);
/// an audio effect stays behind the instrument (`first` = [`first_effect_slot`]). The master
/// chain (`master`) takes audio effects only, so the other roles give `None`. Used by the drop of
/// a browser plugin and by the add menu.
pub fn place_plugin(
    role: DeviceRole,
    master: bool,
    slot: usize,
    first: usize,
    midi: usize,
) -> Option<Placement> {
    Some(match role {
        DeviceRole::Instrument | DeviceRole::MidiEffect if master => return None,
        DeviceRole::Instrument if first > midi => Placement::Instrument(midi),
        DeviceRole::Instrument => Placement::Line(midi),
        DeviceRole::MidiEffect => Placement::Line(slot.min(midi)),
        DeviceRole::Effect => Placement::Line(slot.max(first)),
    })
}

/// `(first, midi)` of a chain of `devices`: the first slot an audio effect may take
/// ([`first_effect_slot`]) and the number of MIDI effects ([`midi_slots`]), for a chain that has
/// no cards (the master's, the add menu's).
pub fn chain_slots(devices: &[std::sync::Arc<Device>]) -> (usize, usize) {
    let midi = devices
        .iter()
        .take_while(|d| d.role == DeviceRole::MidiEffect)
        .count();
    let first = midi
        + usize::from(
            devices
                .get(midi)
                .is_some_and(|d| d.role == DeviceRole::Instrument),
        );
    (first, midi)
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
        let params = plugin_params(Lang::De, &many(20), |id| values.iter().find(|v| v.id == id));
        assert_eq!(params.len(), komponist_model::params::AUTO_MAX);
        assert_eq!(params[0].name(), "P10");
        assert_eq!((params[0].wire, params[0].info.id.as_str()), (10, "p10"));
        assert_eq!((params[0].value, params[0].text.as_str()), (0.18, "80 Hz"));
        assert_eq!((params[1].value, params[1].text.as_str()), (0.5, "50 %"));
        assert_eq!(params[11].name(), "P21");
        // The plugin frame's grid keeps all of them.
        assert_eq!(all_plugin_params(Lang::De, &many(20), |_| None).len(), 20);
        assert_eq!(plugin_params(Lang::De, &many(5), |_| None).len(), 5);
    }

    #[test]
    fn reported_macros_win_over_the_first_twelve() {
        let mut infos = many(20);
        for i in [15, 12] {
            infos[3 + (i - 10)].flags |= param_flags::MACRO;
        }
        let names: Vec<_> = plugin_params(Lang::De, &infos, |_| None)
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
        let algo = legacy_frame_params(
            Lang::De,
            &komponist_model::fixtures::demo_project().tracks[2]
                .channel
                .devices[0],
        )
        .remove(0);
        assert_eq!(algo.name(), "Algorithmus");
        assert_eq!(algo.snap(7.4), 7.0);
        assert_eq!(algo.snap(40.0), 32.0);
        assert_eq!(algo.text_at(0.0), "Frei");
        let ratio = legacy_frame_params(
            Lang::De,
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
        let p = plugin_params(Lang::De, &[info(1, "Gain", 0)], |_| Some(&own)).remove(0);
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
        let cards = cards(Lang::De, lead, &host, &[]);
        let names: Vec<&str> = cards.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Hexa",
                "LSP Para EQ x8",
                "TAL-Chorus-LX",
                "Dragonfly Hall Reverb"
            ]
        );
        let hexa = &cards[0];
        assert!(hexa.instrument && hexa.state == CardState::Ok);
        // WP-86: ten columns on the first page, eight on the envelope page.
        assert_eq!(hexa.frame_width(0.0), 612.0, "10 columns");
        let pages: Vec<_> = hexa.layout.pages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(pages, ["Operatoren", "Hüllkurven", "LFO", "Global"]);
        assert!(hexa.presets);
        assert_eq!(hexa.tag, "INTERN");
        assert_eq!(hexa.params.len(), komponist_hexa::device::params().len());
        assert_eq!(hexa.params[0].name(), "Algorithmus");
        assert_eq!(hexa.open_label(Lang::De), Some(Lang::De.devices_params()));
        // Without plugin manager the effects are missing; TAL is bypassed.
        assert_eq!(cards[1].state, CardState::Missing);
        assert_eq!(cards[1].frame_width(0.0), 252.0);
        assert_eq!(cards[1].open_label(Lang::De), None);
        assert!(cards[2].bypass);
        assert_eq!(first_effect_slot(&cards), 1);
    }

    #[test]
    fn native_metadata_uses_the_same_page_as_the_card_grid() {
        let project = demo_project();
        let host = BuiltinHost::new();
        let mut card = cards(Lang::En, &project.tracks[2], &host, &[]).remove(0);
        let native = komponist_ipc::native_layout::NativeLayout::parse(include_bytes!(
            "../../../../../docs/clap-device-layout/example-v2.json"
        ))
        .unwrap();
        card.layout = native_card_layout(&native, Lang::En);
        card.native_layout = Some(std::sync::Arc::new(native));

        for (index, title, key) in [
            (0, "Graphs", "graphs"),
            (1, "Display", "display"),
            (2, "Display", "display"),
        ] {
            card.page = index;
            let current = card.current_page().unwrap();
            let native = card.native_page().unwrap();
            assert_eq!(current.name, title);
            assert_eq!(native.key, key);
            assert!(std::ptr::eq(
                card.native_cell(&current.cells[0]).unwrap(),
                &native.cells[0],
            ));
        }
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
            Lang::De,
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
        let place = |id: &str| place_of(Lang::De, &ParamId::new(id));
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
        assert_eq!(range_text(Lang::De, &hz), "20 Hz bis 20 kHz");
        let on = ParamInfo::new("b", "Schalter", ParamKind::Bool, 0.0);
        assert_eq!(range_text(Lang::De, &on), "Aus · Ein");
        let mode = ParamInfo::new(
            "m",
            "Modus",
            ParamKind::Enum(vec!["Auf".into(), "Ab".into()]),
            0.0,
        );
        assert_eq!(range_text(Lang::De, &mode), "Auf · Ab");
    }
    #[test]
    fn the_spectrum_shows_only_for_a_running_eq_with_analyse_on() {
        use komponist_builtin::Internal;
        let device = |state: Option<Vec<u8>>, bypass| Device {
            id: DeviceId(1),
            role: DeviceRole::Effect,
            plugin: Internal::Eq.plugin_ref(),
            bypass,
            parameter_bases: Default::default(),
            asset_links: Default::default(),
            state: state.map(Into::into),
            sidechain: None,
        };
        let host = BuiltinHost::new();
        // A project saved before „Analyse“ existed has no such parameter in its state: on.
        let old = br#"{"params": {"out": 0.0}}"#.to_vec();
        let on = card(Lang::De, &device(Some(old), false), &host, &[]);
        assert!(shows_spectrum(&on));
        let off_state = Internal::Eq.with_value(None, 31, 0.0).unwrap();
        assert_eq!(Internal::Eq.wire(&ParamId::new("analyse")), Some(31));
        assert!(!shows_spectrum(&card(
            Lang::De,
            &device(Some(off_state), false),
            &host,
            &[]
        )));
        assert!(!shows_spectrum(&card(
            Lang::De,
            &device(None, true),
            &host,
            &[]
        )));
        let mut collapsed = on.clone();
        collapsed.collapsed = true;
        assert!(!shows_spectrum(&collapsed));
        // Devices without the parameter have none.
        let delay = Device {
            plugin: Internal::Delay.plugin_ref(),
            ..device(None, false)
        };
        assert!(!shows_spectrum(&card(Lang::De, &delay, &host, &[])));
    }
}
