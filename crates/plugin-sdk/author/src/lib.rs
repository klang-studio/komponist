//! Source-pinned adaptor from komponist device layouts to the native CLAP layout ABI.
//!
//! The layout uses nice-plug author parameter IDs. The adaptor resolves them through the
//! wrapper's live ID map and validates the resulting bytes with the same validator as the host.

#![deny(missing_docs)]

pub mod assets;
pub mod display;

#[doc(hidden)]
pub use clap_sys as clap_abi;

use std::collections::BTreeMap;
use std::ffi::{CStr, c_void};
use std::fmt;
use std::mem::MaybeUninit;
use std::ptr;

use clap_sys::{
    ext::params::{
        CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_BYPASS, CLAP_PARAM_IS_ENUM,
        CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_READONLY, CLAP_PARAM_IS_STEPPED, clap_plugin_params,
    },
    plugin::clap_plugin,
};
use komponist_ipc::control::{ParamInfo, param_flags};
use komponist_ipc::native_layout::{
    CAP_CELL_SELECTIONS, CAP_CONDITIONAL_CONTROLS, CAP_EQ_BAND_CONTROLS, CAP_FM_TOPOLOGY,
    CAP_PAD_BANK, CAP_PANEL_SELECTIONS, CAP_PARAMETER_DISPLAY, CAP_SOURCE_FRACTION_TRIM, GraphKind,
    MAX_AUTHOR_IDS, MAX_BYTES, NativeLayout, valid_author_id,
};
use komponist_model::params::context::{BAND, placeholders};
use komponist_model::params::layout::validate;
use komponist_model::params::{ParamContext, resolve};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;
use serde_json::{Value, json};

pub use komponist_device_core::layout;
pub use komponist_ipc::device_assets;
pub use komponist_ipc::effect_meter;
pub use komponist_ipc::native_layout::{
    Canvas, DisplayScale, DisplayUnit, FmCarrier, FmDiagram, Heading, Labels, Pad,
    ParameterDisplay, TrimUnit,
};
pub use komponist_ipc::sampler_ui;
pub use komponist_ipc::signal_taps;
pub use komponist_model::params::{
    ParamId,
    layout::{Cell, CellParam, DeviceLayout, GraphBinding, GraphConfig, Half, Page, WidgetKind},
};

/// ID of the native device layout extension.
pub use komponist_ipc::native_layout::EXTENSION_ID;

const OK: u32 = 0;
const BUFFER_TOO_SMALL: u32 = 2;
const ERROR: u32 = 3;

#[repr(C)]
struct LayoutExtension {
    get_layout: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, *mut u32) -> u32,
}

/// A stable page key and its display labels.
pub struct PageLabels {
    /// Local, stable page key, independent of language.
    pub key: String,
    /// Page labels, including the default locale.
    pub labels: Labels,
}

/// A device core grid and its localized presentation. Cell parameters use nice-plug author IDs.
#[derive(Default)]
pub struct Panel {
    /// Fallback locale for page and parameter labels.
    pub default_locale: String,
    /// Declared locales, including the configured default.
    pub locales: Vec<String>,
    /// The device core grid. A `CellParam::Choice`
    /// or `CellParam::Select` selects bindings for control and graph author-ID
    /// templates. Numeric selectors with the same key share their selection across
    /// pages.
    pub grid: DeviceLayout,
    /// Page labels in the same order as the pages of the grid.
    pub pages: Vec<PageLabels>,
    /// Optional translated labels keyed by nice-plug author parameter ID, or by the
    /// context key of a selector.
    pub parameter_labels: BTreeMap<ParamId, Labels>,
    /// Labels for segments and dropdowns, keyed by author ID and ordered by the CLAP plain value.
    /// A selector uses its context key and option order. Each choice needs a label
    /// in the default locale.
    pub choice_labels: BTreeMap<ParamId, Vec<Labels>>,
    /// Optional localized names and descriptions shown in the device catalog.
    pub device_labels: Option<PluginLabels>,
    /// Labels for every graph cell in the grid.
    pub graph_labels: BTreeMap<(usize, usize), GraphLabels>,
    /// The metadata of every canvas cell, by zero-based page and cell indices.
    pub canvases: BTreeMap<(usize, usize), Canvas>,
    /// Signal tap keys for gain-reduction meter cells, by zero-based page and cell indices.
    pub reduction_taps: BTreeMap<(usize, usize), String>,
    /// Select pages from a pad grid instead of from the panel header.
    pub pad_bank: Option<PadBank>,
    /// Section labels keyed by zero-based page index.
    pub headings: BTreeMap<usize, Vec<Heading>>,
    /// Physical ranges and units keyed by author ID. The SDK exports entries bound by a cell.
    pub parameter_display: BTreeMap<ParamId, ParameterDisplay>,
    /// A knob's alternate parameter and the Boolean selecting it, keyed by the primary ID.
    pub parameter_alternates: BTreeMap<ParamId, (ParamId, ParamId)>,
    /// Optional full names for a selected band's compact choice labels.
    pub full_band_choice_labels: BTreeMap<ParamId, Vec<Labels>>,
}

/// Localized presentation alongside the standard CLAP descriptor.
pub struct PluginLabels {
    /// Device names with an entry for the panel's default locale.
    pub names: Labels,
    /// Optional descriptions. Include the default locale when provided.
    pub descriptions: Labels,
}

/// Localized labels required for each graph in a panel.
pub struct GraphLabels {
    /// Accessible name and default visible title, including the default locale.
    /// The SDK expands `{selection}` to the localized option for a following graph.
    pub name: Labels,
    /// Accessible description, including the default locale.
    pub description: Labels,
    /// Omit the painted title while retaining the accessible name.
    pub hide_title: bool,
    /// Optional graph layer role to named signal tap.
    pub signal_taps: BTreeMap<String, String>,
    /// Optional host-managed audio file slot for a waveform graph.
    pub asset_slot: Option<u32>,
    /// Show the asset slot's waveform marker list when its extension is available.
    pub waveform_markers: bool,
    /// Draw the host's file and viewport controls for the waveform asset slot.
    pub waveform_toolbar: bool,
    /// Fixed direction and fade controls replaced while the waveform edits individual slices.
    pub waveform_slice_controls: Option<[ParamId; 3]>,
    /// Optional parameter containing the MIDI note for waveform audition.
    pub audition_note: Option<ParamId>,
    /// Optional waveform trim units. The default is milliseconds.
    pub trim_unit: Option<TrimUnit>,
}

/// A pad-selected panel. Each entry corresponds to a page in the same order.
pub struct PadBank {
    /// Pad note and asset bindings.
    pub pads: Vec<Pad>,
    /// Named pad-activity stream.
    pub activity_tap: Option<String>,
    /// Nice-plug author ID of the optional follow-input toggle.
    pub follow_param: Option<ParamId>,
}

/// A nice-plug plugin supplying an immutable native layout.
pub trait NativeClapLayout: ClapPlugin {
    /// Returns the panel definition. Each call must produce the same panel for a plugin type.
    /// The host invokes this on its control thread after successful CLAP initialization.
    fn panel() -> Panel;
}

/// The layout could not be represented as native CLAP metadata.
#[derive(Debug)]
pub struct LayoutError(String);

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for LayoutError {}

fn error(message: impl Into<String>) -> LayoutError {
    LayoutError(message.into())
}

/// Recognizes the native layout ID and supplies the table for `P`.
///
/// Call this from `ClapPlugin::clap_custom_extension`. Standard extensions still come
/// from the nice-plug wrapper.
pub fn extension<P: NativeClapLayout>(id: &CStr) -> *const c_void {
    if id != EXTENSION_ID {
        return ptr::null();
    }
    let table: &'static LayoutExtension = &LayoutExtension {
        get_layout: get_layout::<P>,
    };
    ptr::from_ref(table).cast()
}

/// Advertises the public sidechain fallback to the main input.
///
/// `AUX_CONFIGURATION_ID` is the standard CLAP audio ports config with a main stereo input
/// and an auxiliary sidechain. Pass `u32::MAX` if the default configuration already has one.
/// Ordinary hosts can still use the standard audio ports config extension.
pub fn sidechain_fallback_to_main<const AUX_CONFIGURATION_ID: u32>(id: &CStr) -> *const c_void {
    use komponist_ipc::sidechain_fallback::{EXTENSION_ID, MAIN_INPUT, SidechainFallback};

    if id != EXTENSION_ID {
        return ptr::null();
    }
    let table: &'static SidechainFallback = &SidechainFallback {
        mode: MAIN_INPUT,
        auxiliary_config_id: AUX_CONFIGURATION_ID,
    };
    ptr::from_ref(table).cast()
}

/// Compiles a panel to bounded UTF-8 JSON. Call on the CLAP main thread.
///
/// # Safety
///
/// `plugin` must point to a live, successfully initialized instance of the nice-plug wrapper
/// for `P`, with its standard CLAP parameter extension available.
pub unsafe fn compile<P: ClapPlugin>(
    plugin: *const clap_plugin,
    panel: &Panel,
) -> Result<Vec<u8>, LayoutError> {
    if panel.grid.pages.len() != panel.pages.len() {
        return Err(error("every grid page needs a key and localized labels"));
    }
    // SAFETY: The caller supplies the initialized wrapper instance on its control thread.
    let inventory = unsafe { inventory(plugin)? };
    let mut resolved = BTreeMap::new();
    let mut resolve_id = |id: &ParamId| -> Result<u32, LayoutError> {
        if let Some(wire_id) = resolved.get(id) {
            return Ok(*wire_id);
        }
        // SAFETY: The caller supplies this exact Wrapper<P> instance.
        let wire_id = unsafe { Wrapper::<P>::custom_extension_param_id(plugin, id.as_str()) }
            .ok_or_else(|| error(format!("unknown or ambiguous author ID {id}")))?;
        resolved.insert(id.clone(), wire_id);
        Ok(wire_id)
    };
    let mut pages = Vec::new();
    let mut conditional_controls = false;
    let mut eq_band_controls = false;
    let mut cell_selections = false;
    let mut panel_selections = false;
    let mut numeric_selectors = BTreeMap::new();
    for cell in panel.grid.pages.iter().flat_map(|page| &page.cells) {
        if let CellParam::Select { key, .. } = &cell.param {
            *numeric_selectors.entry(*key).or_insert(0usize) += 1;
        }
    }
    let mut context = ParamContext::new().with(BAND, 1);
    for (page_index, (page, text)) in panel.grid.pages.iter().zip(&panel.pages).enumerate() {
        let selectors: BTreeMap<_, _> = page
            .cells
            .iter()
            .filter_map(|cell| selector_options(&cell.param))
            .collect();
        for (key, options) in &selectors {
            if let Some((value, _)) = options.first() {
                context.set(key, value);
            }
        }
        let mut cells = Vec::new();
        let bands = page.cells.iter().find_map(|cell| match &cell.param {
            CellParam::Graph(GraphBinding {
                config: GraphConfig::Eq { bands },
                select: Some(BAND),
                ..
            }) => Some(*bands),
            _ => None,
        });
        for (cell_index, cell) in page.cells.iter().enumerate() {
            let index = (page_index, cell_index);
            if cell.widget == WidgetKind::PadGrid {
                if panel.pad_bank.as_ref().is_none()
                    || cell.half.is_some()
                    || cell.carrier.is_some()
                    || cell.disabled_by.is_some()
                    || cell.param != CellParam::None
                {
                    return Err(error(
                        "a pad grid requires a pad bank and an unbound full cell",
                    ));
                }
                cells.push(json!({
                    "widget": "pad_grid", "col": cell.col, "row": cell.row,
                    "cols": cell.cols, "rows": cell.rows,
                }));
                continue;
            }
            if cell.widget == WidgetKind::MeterGr {
                let reduction_tap = panel
                    .reduction_taps
                    .get(&index)
                    .ok_or_else(|| error("missing gain-reduction meter tap"))?;
                if cell.half.is_some()
                    || cell.carrier.is_some()
                    || cell.disabled_by.is_some()
                    || cell.param != CellParam::None
                {
                    return Err(error(
                        "a gain-reduction meter needs a full cell without parameter bindings",
                    ));
                }
                cells.push(json!({
                    "widget": "meter_gr", "col": cell.col, "row": cell.row,
                    "cols": cell.cols, "rows": cell.rows, "reduction_tap": reduction_tap,
                }));
                continue;
            }
            if cell.widget == WidgetKind::Canvas {
                let canvas = panel
                    .canvases
                    .get(&index)
                    .ok_or_else(|| error("missing canvas metadata"))?;
                if cell.half.is_some()
                    || cell.carrier.is_some()
                    || cell.disabled_by.is_some()
                    || cell.param != CellParam::None
                {
                    return Err(error(
                        "a canvas needs a full cell without parameter bindings",
                    ));
                }
                cells.push(json!({
                    "widget": "canvas", "col": cell.col, "row": cell.row,
                    "cols": cell.cols, "rows": cell.rows, "canvas": canvas,
                }));
                continue;
            }
            if let CellParam::Graph(binding) = &cell.param {
                let labels = panel
                    .graph_labels
                    .get(&index)
                    .ok_or_else(|| error("missing graph labels"))?;
                let selector =
                    selector_for(binding.bind.iter().map(|(_, id)| id.as_str()), &selectors)?;
                let choice_labels =
                    selector.and_then(|(key, _)| panel.choice_labels.get(&ParamId::new(key)));
                cells.push(compile_graph(
                    cell,
                    binding,
                    labels,
                    selector,
                    choice_labels.map(Vec::as_slice),
                    &mut resolve_id,
                )?);
                eq_band_controls |= binding.select.is_some()
                    || binding
                        .bind
                        .iter()
                        .any(|(slot, _)| slot == "spectrum_enabled");
                continue;
            }
            if let Some((key, options)) = selector_options(&cell.param) {
                if cell.carrier.is_some() || cell.disabled_by.is_some() {
                    return Err(error("native selectors require an unbound cell"));
                }
                let widget = match (cell.widget, cell.half) {
                    (WidgetKind::Segment, Some(_)) => "selector",
                    (WidgetKind::ColumnSelect, None)
                        if matches!(cell.param, CellParam::Select { .. }) =>
                    {
                        "column_selector"
                    }
                    _ => return Err(error("invalid native selector geometry")),
                };
                let default_label = match &cell.param {
                    CellParam::Choice { label, .. } => *label,
                    _ => key,
                };
                let id = ParamId::new(key);
                let localized = |text: &str| {
                    [(panel.default_locale.clone(), text.to_owned())]
                        .into_iter()
                        .collect::<Labels>()
                };
                let title = panel
                    .parameter_labels
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| localized(default_label));
                let names =
                    panel.choice_labels.get(&id).cloned().unwrap_or_else(|| {
                        options.iter().map(|(_, name)| localized(name)).collect()
                    });
                if names.len() != options.len() {
                    return Err(error("selector labels must match its options"));
                }
                let mut selector = json!({"key": key, "labels": title, "options": names});
                let shared = matches!(cell.param, CellParam::Select { .. })
                    && numeric_selectors.get(key).is_some_and(|count| *count > 1);
                if shared {
                    selector["shared"] = json!(true);
                }
                let mut wire_cell = json!({
                    "widget": widget, "col": cell.col, "row": cell.row,
                    "cols": cell.cols, "rows": cell.rows, "selector": selector,
                });
                if let Some(half) = cell.half {
                    wire_cell["half"] = json!(match half {
                        Half::Top => "top",
                        Half::Bottom => "bottom",
                    });
                }
                cells.push(wire_cell);
                cell_selections = true;
                panel_selections |= shared || options.len() > 4 || widget == "column_selector";
                continue;
            }
            let widget = widget_name(cell.widget)?;
            let mut wire_cell = json!({
                "widget": widget,
                "col": cell.col,
                "row": cell.row,
                "cols": cell.cols,
                "rows": cell.rows,
            });
            if let Some(driver) = &cell.disabled_by {
                wire_cell["disabled_by"] = json!(resolve_id(driver)?);
                conditional_controls = true;
            }
            if let Some(half) = cell.half {
                wire_cell["half"] = Value::from(match half {
                    Half::Top => "top",
                    Half::Bottom => "bottom",
                });
            }
            let mut binding = |id: &ParamId, full: Option<&[Labels]>| {
                compile_control_binding(id, cell.widget, panel, full, &inventory, &mut resolve_id)
            };
            let selector = match &cell.param {
                CellParam::Template(template) => selector_for([template.as_str()], &selectors)?,
                _ => None,
            };
            if let (CellParam::Template(template), Some((key, options))) = (&cell.param, selector)
                && cell.carrier.is_none()
                && cell.disabled_by.is_none()
            {
                let variants = options
                    .iter()
                    .map(|(value, _)| {
                        let id = resolve(template, &ParamContext::new().with(key, value))
                            .map_err(|failure| error(failure.to_string()))?;
                        binding(&id, None).map(Value::Object)
                    })
                    .collect::<Result<Vec<_>, LayoutError>>()?;
                wire_cell["follows"] = json!(key);
                wire_cell["variants"] = json!(variants);
            } else if let CellParam::Template(template) = &cell.param
                && let Some(bands) = bands.filter(|count| (1..=8).contains(count))
                && cell.carrier.is_none()
                && cell.disabled_by.is_none()
            {
                let controls = (1..=bands)
                    .map(|band| {
                        let id = resolve(template, &ParamContext::new().with(BAND, band))
                            .map_err(|failure| error(failure.to_string()))?;
                        let full = panel.full_band_choice_labels.get(&id).map(Vec::as_slice);
                        binding(&id, full).map(Value::Object)
                    })
                    .collect::<Result<Vec<_>, LayoutError>>()?;
                wire_cell["band_controls"] = Value::Array(controls);
            } else {
                wire_cell
                    .as_object_mut()
                    .expect("cell object")
                    .extend(binding(fixed_binding(cell)?, None)?);
            }
            cells.push(wire_cell);
        }
        let mut wire_page = json!({
            "key": text.key,
            "labels": text.labels,
            "columns": page.cols,
            "cells": cells,
        });
        if !page.dividers.is_empty() {
            wire_page["dividers"] = json!(page.dividers);
        }
        if let Some(headings) = panel.headings.get(&page_index) {
            wire_page["headings"] = json!(headings);
        }
        pages.push(wire_page);
    }
    let mut parameter_alternates = BTreeMap::new();
    for (param, (alternate, when)) in &panel.parameter_alternates {
        let primary = resolve_id(param)?;
        let when = resolve_id(when)?;
        let widget = if panel.choice_labels.contains_key(alternate) {
            WidgetKind::Segment
        } else {
            WidgetKind::Knob
        };
        let mut binding =
            compile_control_binding(alternate, widget, panel, None, &inventory, &mut resolve_id)?;
        binding.insert("when".into(), json!(when));
        parameter_alternates.insert(primary, binding);
    }
    let parameter_display = panel
        .parameter_display
        .iter()
        .filter_map(|(id, display)| Some((*resolved.get(id)?, display)))
        .collect::<BTreeMap<_, _>>();
    let pad_bank = panel.pad_bank.as_ref();
    if let Some(failure) = validate(&panel.grid, &context, &|id| resolved.contains_key(id))
        .into_iter()
        .find(|error| {
            pad_bank.is_none()
                || !matches!(
                    error,
                    komponist_model::params::layout::LayoutError::TooManyPages(_)
                )
        })
    {
        return Err(error(failure.to_string()));
    }
    let mut document = json!({
        "default_locale": panel.default_locale,
        "locales": panel.locales,
        "pages": pages,
    });
    let mut capabilities = Vec::new();
    if eq_band_controls {
        capabilities.push(CAP_EQ_BAND_CONTROLS);
    }
    if cell_selections {
        capabilities.push(CAP_CELL_SELECTIONS);
    }
    if panel_selections {
        capabilities.push(CAP_PANEL_SELECTIONS);
    }
    if panel
        .canvases
        .values()
        .any(|canvas| canvas.fm_topology.is_some())
    {
        capabilities.push(CAP_FM_TOPOLOGY);
    }
    if conditional_controls || !parameter_alternates.is_empty() {
        capabilities.push(CAP_CONDITIONAL_CONTROLS);
    }
    if !parameter_alternates.is_empty() {
        document["parameter_alternates"] = json!(parameter_alternates);
    }
    if !parameter_display.is_empty() {
        capabilities.push(CAP_PARAMETER_DISPLAY);
        document["parameter_display"] = json!(parameter_display);
    }
    if panel
        .graph_labels
        .values()
        .any(|labels| labels.trim_unit == Some(TrimUnit::SourceFraction))
    {
        capabilities.push(CAP_SOURCE_FRACTION_TRIM);
    }
    if let Some(bank) = pad_bank {
        capabilities.push(CAP_PAD_BANK);
        let mut value = json!({"pads": bank.pads});
        if let Some(tap) = &bank.activity_tap {
            value["activity_tap"] = json!(tap);
        }
        if let Some(param) = &bank.follow_param {
            value["follow_param"] = json!(
                resolved
                    .get(param)
                    .ok_or_else(|| error("unbound pad-follow parameter"))?
            );
        }
        document["pad_bank"] = value;
    }
    if !capabilities.is_empty() {
        document["required_capabilities"] = json!(capabilities);
    }
    if let Some(labels) = &panel.device_labels {
        document["metadata"] = json!({"names": labels.names, "descriptions": labels.descriptions});
    }
    // This optional metadata preserves compatibility with layouts naming only
    // part of the inventory. Emit the map only if the live wrapper resolves a
    // complete bijection. Invalid decoration should not discard a valid layout.
    if (1..=MAX_AUTHOR_IDS).contains(&inventory.len()) {
        for id in panel.parameter_labels.keys() {
            if valid_author_id(id.as_str()) && !resolved.contains_key(id) {
                // SAFETY: This is the initialized live wrapper supplied by the caller.
                if let Some(numeric) =
                    unsafe { Wrapper::<P>::custom_extension_param_id(plugin, id.as_str()) }
                {
                    resolved.insert(id.clone(), numeric);
                }
            }
        }
        let expected: std::collections::BTreeSet<_> =
            inventory.iter().map(|info| info.id).collect();
        if resolved.len() == inventory.len()
            && resolved
                .values()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                == expected
            && resolved.keys().all(|key| valid_author_id(key.as_str()))
        {
            document["author_ids"] = json!(resolved);
            if serde_json::to_vec(&document).is_ok_and(|bytes| bytes.len() > MAX_BYTES as usize) {
                document
                    .as_object_mut()
                    .expect("layout JSON object")
                    .remove("author_ids");
            }
        }
    }
    let bytes = serde_json::to_vec(&document).map_err(|failure| error(failure.to_string()))?;
    NativeLayout::parse(&bytes)
        .and_then(|layout| {
            if panel.device_labels.is_some() && layout.localized_plugin().is_none() {
                return Err(komponist_ipc::native_layout::LayoutError::Invalid(
                    "plugin metadata",
                ));
            }
            layout.validate_params(&inventory)
        })
        .map_err(|failure| error(failure.to_string()))?;
    Ok(bytes)
}

type SelectionOptions<'a> = &'a [(&'static str, &'static str)];

fn selector_options(param: &CellParam) -> Option<(&'static str, SelectionOptions<'_>)> {
    const NUMBERS: SelectionOptions<'static> = &[
        ("1", "1"),
        ("2", "2"),
        ("3", "3"),
        ("4", "4"),
        ("5", "5"),
        ("6", "6"),
        ("7", "7"),
        ("8", "8"),
    ];
    match param {
        CellParam::Choice { key, options, .. } => Some((key, options)),
        CellParam::Select { key, count } if (2..=8).contains(count) => {
            Some((key, &NUMBERS[..usize::from(*count)]))
        }
        _ => None,
    }
}

fn selector_for<'a>(
    texts: impl IntoIterator<Item = &'a str>,
    selectors: &BTreeMap<&'static str, SelectionOptions<'a>>,
) -> Result<Option<(&'static str, SelectionOptions<'a>)>, LayoutError> {
    let mut current = None;
    for text in texts {
        for key in placeholders(text).map_err(|failure| error(failure.to_string()))? {
            let Some((&key, &options)) = selectors.get_key_value(key) else {
                continue;
            };
            if current.is_some_and(|(previous, _)| previous != key) {
                return Err(error("a cell may follow only one selector"));
            }
            current = Some((key, options));
        }
    }
    Ok(current)
}

fn compile_graph(
    cell: &Cell,
    binding: &GraphBinding,
    labels: &GraphLabels,
    selector: Option<(&str, SelectionOptions<'_>)>,
    choice_labels: Option<&[Labels]>,
    resolve_id: &mut impl FnMut(&ParamId) -> Result<u32, LayoutError>,
) -> Result<Value, LayoutError> {
    if cell.widget != WidgetKind::Graph
        || cell.half == Some(Half::Bottom)
        || cell.carrier.is_some()
        || cell.disabled_by.is_some()
        || binding
            .select
            .is_some_and(|key| key != BAND || !matches!(binding.config, GraphConfig::Eq { .. }))
    {
        return Err(error(
            "the native CLAP layout requires an unconditional graph with fixed bindings",
        ));
    }
    let mut graph = json!({"name": labels.name, "description": labels.description});
    if binding.select.is_some() {
        graph["select_band"] = Value::Bool(true);
    }
    if labels.hide_title {
        graph["hide_title"] = json!(true);
    }
    if !labels.signal_taps.is_empty() {
        graph["signal_taps"] = json!(labels.signal_taps);
    }
    if let Some(slot) = labels.asset_slot {
        graph["asset_slot"] = json!(slot);
    }
    if labels.waveform_markers {
        graph["waveform_markers"] = json!(true);
    }
    if labels.waveform_toolbar {
        graph["waveform_toolbar"] = json!(true);
    }
    if let Some(ids) = &labels.waveform_slice_controls {
        let ids = ids
            .iter()
            .map(&mut *resolve_id)
            .collect::<Result<Vec<_>, _>>()?;
        graph["waveform_slice_controls"] = json!(ids);
    }
    if let Some(note) = &labels.audition_note {
        graph["audition_note"] = json!(resolve_id(note)?);
    }
    if let Some(unit) = labels.trim_unit {
        graph["trim_unit"] = json!(unit);
    }
    let kind = match binding.config {
        GraphConfig::Envelope => GraphKind::Envelope,
        GraphConfig::Eq { bands } => {
            graph["bands"] = json!(bands);
            GraphKind::Eq
        }
        GraphConfig::Filter => GraphKind::Filter,
        GraphConfig::Transfer => GraphKind::Transfer,
        GraphConfig::Waveform => GraphKind::Waveform,
        GraphConfig::Xy => GraphKind::Xy,
        GraphConfig::Lfo { points } => {
            graph["points"] = json!(points);
            GraphKind::Lfo
        }
        GraphConfig::Compressor => GraphKind::Compressor,
        GraphConfig::GrHistory => GraphKind::GrHistory,
        GraphConfig::FilterVa => GraphKind::FilterVa,
        GraphConfig::LfoShape => GraphKind::LfoShape,
        GraphConfig::Wavetable => GraphKind::Wavetable,
        GraphConfig::Signal => GraphKind::Signal,
    };
    graph["kind"] = json!(kind);
    let mut bindings = BTreeMap::new();
    for (slot, author) in &binding.bind {
        if selector.is_some() && author.contains('{') {
            continue;
        }
        let wire = resolve_id(&ParamId::new(author))?;
        if bindings.insert(slot, wire).is_some() {
            return Err(error(format!("duplicate graph slot {slot}")));
        }
    }
    graph["slots"] = json!(bindings);
    let mut cell_value = json!({
        "widget": "graph",
        "col": cell.col,
        "row": cell.row,
        "cols": cell.cols,
        "rows": cell.rows,
        "graph": graph,
    });
    if let Some((key, options)) = selector {
        let mut variants = Vec::new();
        for (index, (value, label)) in options.iter().enumerate() {
            let ctx = ParamContext::new().with(key, value);
            let mut slots = BTreeMap::new();
            for (slot, author) in binding
                .bind
                .iter()
                .filter(|(_, author)| author.contains('{'))
            {
                let id = resolve(author, &ctx).map_err(|failure| error(failure.to_string()))?;
                if slots.insert(slot, resolve_id(&id)?).is_some() {
                    return Err(error(format!("duplicate graph slot {slot}")));
                }
            }
            let mut variant = json!({"slots": slots});
            if labels
                .name
                .values()
                .any(|name| name.contains("{selection}"))
            {
                let names: Labels = labels
                    .name
                    .iter()
                    .map(|(locale, name)| {
                        let translated = choice_labels
                            .and_then(|labels| labels.get(index))
                            .and_then(|labels| labels.get(locale))
                            .map_or(*label, String::as_str);
                        (locale.clone(), name.replace("{selection}", translated))
                    })
                    .collect();
                variant["name"] = json!(names);
            }
            variants.push(variant);
        }
        if let Some(name) = variants.first().and_then(|variant| variant.get("name")) {
            cell_value["graph"]["name"] = name.clone();
        }
        cell_value["follows"] = json!(key);
        cell_value["graph"]["variants"] = json!(variants);
    }
    if cell.half.is_some() {
        cell_value["half"] = json!("top");
    }
    Ok(cell_value)
}

fn compile_control_binding(
    id: &ParamId,
    widget: WidgetKind,
    panel: &Panel,
    full: Option<&[Labels]>,
    inventory: &[ParamInfo],
    resolve_id: &mut impl FnMut(&ParamId) -> Result<u32, LayoutError>,
) -> Result<serde_json::Map<String, Value>, LayoutError> {
    let mut binding = serde_json::Map::new();
    let param_id = resolve_id(id)?;
    binding.insert("param_id".into(), json!(param_id));
    if let Some(labels) = panel.parameter_labels.get(id) {
        binding.insert("labels".into(), json!(labels));
    }
    if matches!(widget, WidgetKind::Segment | WidgetKind::Dropdown) {
        let param = inventory
            .iter()
            .find(|param| param.id == param_id)
            .ok_or_else(|| error(format!("missing CLAP inventory entry for {id}")))?;
        let labels = panel
            .choice_labels
            .get(id)
            .ok_or_else(|| error(format!("missing choice labels for {id}")))?;
        let mut choices = compile_choices(param, labels)?;
        if let Some(full) = full {
            if full.len() != choices.len() {
                return Err(error("full and short band choice counts must match"));
            }
            for (choice, labels) in choices.iter_mut().zip(full) {
                choice["full_labels"] = json!(labels);
            }
        }
        binding.insert("choices".into(), Value::Array(choices));
    }
    Ok(binding)
}

fn fixed_binding(cell: &Cell) -> Result<&ParamId, LayoutError> {
    match &cell.param {
        CellParam::Id(id) if cell.carrier.is_none() => Ok(id),
        _ => Err(error("unsupported native CLAP parameter binding")),
    }
}

fn widget_name(widget: WidgetKind) -> Result<&'static str, LayoutError> {
    match widget {
        WidgetKind::Knob => Ok("knob"),
        WidgetKind::KnobLarge => Ok("knob_large"),
        WidgetKind::Number => Ok("number"),
        WidgetKind::ToggleButton => Ok("toggle_button"),
        WidgetKind::Segment => Ok("segment"),
        WidgetKind::Dropdown => Ok("dropdown"),
        WidgetKind::MiniFader => Ok("mini_fader"),
        _ => Err(error("widget is unavailable in the native CLAP layout")),
    }
}

fn compile_choices(param: &ParamInfo, labels: &[Labels]) -> Result<Vec<Value>, LayoutError> {
    // The final validator checks the flags and the full sequence against the live inventory.
    let first = param.min as i32;
    labels
        .iter()
        .enumerate()
        .map(|(index, labels)| {
            let value = i32::try_from(index)
                .ok()
                .and_then(|index| first.checked_add(index))
                .ok_or_else(|| error("choice value outside the CLAP layout integer range"))?;
            Ok(json!({"value": value, "labels": labels}))
        })
        .collect()
}

unsafe fn inventory(plugin: *const clap_plugin) -> Result<Vec<ParamInfo>, LayoutError> {
    // SAFETY: The instance and its C extension table remain live for this main-thread call.
    let plugin_ref = unsafe { plugin.as_ref() }.ok_or_else(|| error("null CLAP instance"))?;
    let get_extension = plugin_ref
        .get_extension
        .ok_or_else(|| error("missing CLAP extension callback"))?;
    // SAFETY: `plugin` is the live instance whose own `get_extension` this is, and the id is a
    // NUL-terminated static string. The returned pointer is null or an extension table of the plugin.
    let table = unsafe { get_extension(plugin, CLAP_EXT_PARAMS.as_ptr()) };
    // SAFETY: For `CLAP_EXT_PARAMS` the table is a `clap_plugin_params` that stays valid as long as the
    // instance; `as_ref` turns a null pointer into `None`, which is rejected here.
    let table = unsafe { table.cast::<clap_plugin_params>().as_ref() }
        .ok_or_else(|| error("missing CLAP parameter extension"))?;
    let (Some(count), Some(get_info)) = (table.count, table.get_info) else {
        return Err(error("incomplete CLAP parameter extension"));
    };
    let mut result = Vec::new();
    // SAFETY: `count` comes from the plugin's own parameter table and takes the instance it belongs to.
    for index in 0..unsafe { count(plugin) } {
        let mut info = MaybeUninit::uninit();
        // SAFETY: `index` is below the count just reported, and `info.as_mut_ptr()` points to writable
        // storage for one `clap_param_info`; the plugin only writes it and returns false on failure.
        if !unsafe { get_info(plugin, index, info.as_mut_ptr()) } {
            return Err(error("CLAP parameter info unavailable"));
        }
        // SAFETY: get_info returned true and initialized the output.
        let info = unsafe { info.assume_init() };
        if !info.default_value.is_finite()
            || !(info.min_value..=info.max_value).contains(&info.default_value)
        {
            return Err(error(format!(
                "invalid CLAP default for parameter {}",
                info.id
            )));
        }
        let mut flags = 0;
        for (clap, wire) in [
            (CLAP_PARAM_IS_STEPPED, param_flags::STEPPED),
            (CLAP_PARAM_IS_HIDDEN, param_flags::HIDDEN),
            (CLAP_PARAM_IS_READONLY, param_flags::READ_ONLY),
            (CLAP_PARAM_IS_BYPASS, param_flags::BYPASS),
            (CLAP_PARAM_IS_AUTOMATABLE, param_flags::AUTOMATABLE),
            (CLAP_PARAM_IS_ENUM, param_flags::ENUM),
        ] {
            if info.flags & clap != 0 {
                flags |= wire;
            }
        }
        let step_count = if flags & param_flags::STEPPED != 0 {
            (info.max_value - info.min_value).round() as u32
        } else {
            0
        };
        result.push(ParamInfo {
            id: info.id,
            name: String::new(),
            module: String::new(),
            min: info.min_value,
            max: info.max_value,
            default: info.default_value,
            step_count,
            flags,
        });
    }
    Ok(result)
}

unsafe extern "C" fn get_layout<P: NativeClapLayout>(
    plugin: *const clap_plugin,
    destination: *mut u8,
    capacity: u32,
    required_or_written: *mut u32,
) -> u32 {
    if required_or_written.is_null() {
        return ERROR;
    }
    // SAFETY: The caller supplies a writable length pointer and a separate destination.
    unsafe { required_or_written.write(0) };
    if plugin.is_null() || (destination.is_null() && capacity != 0) {
        return ERROR;
    }
    // SAFETY: The caller supplies the initialized Wrapper<P> that returned this table.
    let result = unsafe { compile::<P>(plugin, &P::panel()) };
    let Ok(bytes) = result else {
        return ERROR;
    };
    let Ok(len) = u32::try_from(bytes.len()) else {
        return ERROR;
    };
    if len > MAX_BYTES {
        return ERROR;
    }
    // SAFETY: `required_or_written` was checked non-null above and is the caller's writable length
    // pointer (already written once with 0 under the same contract).
    unsafe { required_or_written.write(len) };
    if len > capacity {
        return BUFFER_TOO_SMALL;
    }
    // SAFETY: On success the destination covers the full document.
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len()) };
    OK
}

#[cfg(test)]
mod tests;
