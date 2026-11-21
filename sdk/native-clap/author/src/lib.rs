//! Source-pinned adaptor from komponist device layouts to the native CLAP v1 ABI.
//!
//! The layout uses nice-plug author parameter IDs. The adaptor resolves them through the
//! wrapper's live ID map and validates the resulting bytes with the same v1 validator as the host.

#![deny(missing_docs)]

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
use komponist_ipc::native_layout::{MAX_BYTES, NativeLayout};
use komponist_model::params::ParamContext;
use komponist_model::params::layout::validate;
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;
use serde_json::{Value, json};

pub use komponist_device_core::layout;
pub use komponist_ipc::native_layout::Labels;
pub use komponist_model::params::{
    ParamId,
    layout::{Cell, CellParam, DeviceLayout, Half, Page, WidgetKind},
};

/// Extension ID from the v1 C header.
pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.device-layout/1";

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
pub struct Panel {
    /// Fallback locale for page and parameter labels.
    pub default_locale: String,
    /// Declared locales, including the configured default.
    pub locales: Vec<String>,
    /// The device core grid. The adaptor accepts fixed parameter bindings.
    pub grid: DeviceLayout,
    /// Page labels in the same order as the pages of the grid.
    pub pages: Vec<PageLabels>,
    /// Optional translated labels keyed by nice-plug author parameter ID.
    pub parameter_labels: BTreeMap<ParamId, Labels>,
    /// Labels for segments and dropdowns, keyed by author ID and ordered by the CLAP plain value.
    /// Each choice needs a label in the default locale.
    pub choice_labels: BTreeMap<ParamId, Vec<Labels>>,
}

/// A nice-plug plugin supplying an immutable native layout.
pub trait NativeClapLayout: ClapPlugin {
    /// Returns the panel definition. Each call must produce the same panel for a plugin type.
    /// The host invokes this on its control thread after successful CLAP initialization.
    fn panel() -> Panel;
}

/// The layout could not be represented as native CLAP v1 metadata.
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

/// Compiles a panel to bounded UTF-8 v1 JSON. Call on the CLAP main thread.
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
    let mut pages = Vec::new();
    for (page, text) in panel.grid.pages.iter().zip(&panel.pages) {
        if !page.dividers.is_empty() {
            return Err(error("native CLAP v1 has no grid dividers"));
        }
        let mut cells = Vec::new();
        for cell in &page.cells {
            let id = fixed_binding(cell)?;
            let widget = widget_name(cell.widget)?;
            let param_id = if let Some(wire_id) = resolved.get(id) {
                *wire_id
            } else {
                // SAFETY: The caller supplies this exact Wrapper<P> instance.
                let wire_id =
                    unsafe { Wrapper::<P>::custom_extension_param_id(plugin, id.as_str()) }
                        .ok_or_else(|| error(format!("unknown or ambiguous author ID {id}")))?;
                resolved.insert(id.clone(), wire_id);
                wire_id
            };
            let mut wire_cell = json!({
                "widget": widget,
                "col": cell.col,
                "row": cell.row,
                "cols": cell.cols,
                "rows": cell.rows,
                "param_id": param_id,
            });
            if let Some(half) = cell.half {
                wire_cell["half"] = Value::from(match half {
                    Half::Top => "top",
                    Half::Bottom => "bottom",
                });
            }
            if let Some(labels) = panel.parameter_labels.get(id) {
                wire_cell["labels"] = json!(labels);
            }
            if matches!(cell.widget, WidgetKind::Segment | WidgetKind::Dropdown) {
                let param = inventory
                    .iter()
                    .find(|param| param.id == param_id)
                    .ok_or_else(|| error(format!("missing CLAP inventory entry for {id}")))?;
                let labels = panel
                    .choice_labels
                    .get(id)
                    .ok_or_else(|| error(format!("missing choice labels for {id}")))?;
                wire_cell["choices"] = Value::Array(compile_choices(param, labels)?);
            }
            cells.push(wire_cell);
        }
        pages.push(json!({
            "key": text.key,
            "labels": text.labels,
            "columns": page.cols,
            "cells": cells,
        }));
    }
    if let Some(failure) = validate(&panel.grid, &ParamContext::new(), &|id| {
        resolved.contains_key(id)
    })
    .into_iter()
    .next()
    {
        return Err(error(failure.to_string()));
    }
    let bytes = serde_json::to_vec(&json!({
        "schema_version": 1,
        "default_locale": panel.default_locale,
        "locales": panel.locales,
        "pages": pages,
    }))
    .map_err(|failure| error(failure.to_string()))?;
    NativeLayout::parse(&bytes)
        .and_then(|layout| layout.validate_params(&inventory))
        .map_err(|failure| error(failure.to_string()))?;
    Ok(bytes)
}

fn fixed_binding(cell: &Cell) -> Result<&ParamId, LayoutError> {
    match &cell.param {
        CellParam::Id(id) if cell.carrier.is_none() && cell.disabled_by.is_none() => Ok(id),
        _ => Err(error(
            "native CLAP v1 needs a fixed, unconditional parameter binding",
        )),
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
        _ => Err(error("widget is unavailable in native CLAP v1")),
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
    let table = unsafe { get_extension(plugin, CLAP_EXT_PARAMS.as_ptr()) };
    let table = unsafe { table.cast::<clap_plugin_params>().as_ref() }
        .ok_or_else(|| error("missing CLAP parameter extension"))?;
    let (Some(count), Some(get_info)) = (table.count, table.get_info) else {
        return Err(error("incomplete CLAP parameter extension"));
    };
    let mut result = Vec::new();
    for index in 0..unsafe { count(plugin) } {
        let mut info = MaybeUninit::uninit();
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
    // SAFETY: The v1 caller supplies a writable length pointer and a separate destination.
    unsafe { required_or_written.write(0) };
    if plugin.is_null() || (destination.is_null() && capacity != 0) {
        return ERROR;
    }
    // SAFETY: The v1 caller supplies the initialized Wrapper<P> that returned this table.
    let Ok(bytes) = (unsafe { compile::<P>(plugin, &P::panel()) }) else {
        return ERROR;
    };
    let Ok(len) = u32::try_from(bytes.len()) else {
        return ERROR;
    };
    if len > MAX_BYTES {
        return ERROR;
    }
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
