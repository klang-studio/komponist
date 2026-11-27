use std::collections::BTreeSet;
use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::{ManuallyDrop, MaybeUninit};
use std::path::Path;
use std::ptr;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::ext::params::{
    CLAP_EXT_PARAMS, CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_BYPASS, CLAP_PARAM_IS_ENUM,
    CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_READONLY, CLAP_PARAM_IS_STEPPED, clap_param_info,
    clap_plugin_params,
};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};
use komponist_plugin_api::params::{ParamInfo, param_flags};
use komponist_plugin_api::{
    device_canvas,
    native_layout::{MAX_BYTES, NativeLayout},
};
use libloading::Library;
use serde_json::{Value, json};

use crate::{EXTENSION_ID, PLUGIN_ID, Result, bundle};

#[repr(C)]
struct LayoutExtension {
    get_layout: Option<unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, *mut u32) -> u32>,
}

#[repr(C)]
struct DisplayExtension {
    draw: Option<unsafe extern "C" fn(*const clap_plugin, *mut u8, u32) -> u32>,
}

/// Unlike the app's bridge, this host calls the exported CLAP ABI from a standalone process.
pub fn example(bundle: &Path, plain: bool) -> Result<Value> {
    let executable = bundle::executable(bundle)?.canonicalize()?;
    let bundle_path = CString::new(
        bundle
            .canonicalize()?
            .to_str()
            .ok_or("non-UTF-8 bundle path")?,
    )?;
    // SAFETY: This command loads the SDK example produced in the preceding build step.
    let library = unsafe { Library::new(executable) }?;
    // Keep the library mapped until process exit in case the plugin registered TLS destructors.
    let library = ManuallyDrop::new(library);
    // SAFETY: The CLAP entry specification declares this symbol as an exported data object.
    let symbol = unsafe { library.get::<*const clap_plugin_entry>(b"clap_entry\0") }?;
    // SAFETY: The symbol points to the entry table for the loaded library.
    let entry = unsafe { (*symbol).as_ref() }.ok_or("CLAP entry is null")?;
    if !clap_version_is_compatible(entry.clap_version) {
        return Err("incompatible CLAP entry version".into());
    }
    let init = entry.init.ok_or("CLAP entry has no init")?;
    let deinit = entry.deinit.ok_or("CLAP entry has no deinit")?;
    let get_factory = entry.get_factory.ok_or("CLAP entry has no factory hook")?;
    // SAFETY: The bundle path is a C string that remains live for initialization.
    if !unsafe { init(bundle_path.as_ptr()) } {
        return Err("CLAP entry initialization failed".into());
    }
    let _entry_guard = EntryGuard(deinit);
    // SAFETY: The entry was initialized and the ID designates the standard plugin factory.
    let factory = unsafe {
        get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr())
            .cast::<clap_plugin_factory>()
            .as_ref()
    }
    .ok_or("CLAP plugin factory is missing")?;
    let count = factory
        .get_plugin_count
        .ok_or("factory has no count hook")?;
    let descriptor = factory
        .get_plugin_descriptor
        .ok_or("factory has no descriptor hook")?;
    let create = factory.create_plugin.ok_or("factory has no create hook")?;
    // SAFETY: The factory belongs to the live entry and only index 0 is requested after the count.
    if unsafe { count(factory) } != 1 {
        return Err("expected one CLAP plugin descriptor".into());
    }
    // SAFETY: The count and the descriptor's lifetime are guaranteed by the factory.
    let descriptor = unsafe { descriptor(factory, 0).as_ref() }.ok_or("null CLAP descriptor")?;
    let id = descriptor_text(descriptor.id)?;
    let name = descriptor_text(descriptor.name)?;
    let version = descriptor_text(descriptor.version)?;
    if id != PLUGIN_ID
        || version != env!("CARGO_PKG_VERSION")
        || name.trim().is_empty()
        || !clap_version_is_compatible(descriptor.clap_version)
    {
        return Err(format!("unexpected CLAP descriptor {id} {version} ({name})").into());
    }
    // SAFETY: The host table is static and the descriptor ID remains live for the factory call.
    let plugin =
        unsafe { create(factory, &HOST, descriptor.id).as_ref() }.ok_or("create_plugin failed")?;
    let _plugin_guard = PluginGuard {
        plugin,
        destroy: plugin.destroy.ok_or("plugin has no destroy hook")?,
    };
    let init = plugin.init.ok_or("plugin has no init hook")?;
    // SAFETY: The factory returned this uninitialized plugin to this main-thread host.
    if !unsafe { init(plugin) } {
        return Err("CLAP plugin initialization failed".into());
    }

    let params = inventory(plugin)?;
    let layout = layout(plugin, &params, plain)?;
    let display = display(plugin, plain)?;
    Ok(json!({
        "plugin": {"id": id, "name": name, "version": version},
        "params": params,
        "layout": layout,
        "display": display,
    }))
}

fn inventory(plugin: &clap_plugin) -> Result<Vec<ParamInfo>> {
    // SAFETY: The ID names the standard CLAP params vtable and the instance is initialized.
    let params = unsafe {
        extension(plugin, CLAP_EXT_PARAMS)?
            .cast::<clap_plugin_params>()
            .as_ref()
    }
    .ok_or("CLAP params extension is missing")?;
    let count = params.count.ok_or("params extension has no count hook")?;
    let info = params.get_info.ok_or("params extension has no info hook")?;
    let value = params
        .get_value
        .ok_or("params extension has no value hook")?;
    // SAFETY: This live plugin owns the vtable, and the call is on the main thread.
    let count = unsafe { count(plugin) };
    let expected = [("Gain", 0.75_f64), ("Mix", 1.0_f64)];
    if count as usize != expected.len() {
        return Err(format!("expected two example parameters, found {count}").into());
    }

    let mut ids = BTreeSet::new();
    let mut scanned = Vec::new();
    for index in 0..count {
        let mut parameter = MaybeUninit::<clap_param_info>::zeroed();
        // SAFETY: The index is below the advertised count and storage is sized for the ABI table.
        if !unsafe { info(plugin, index, parameter.as_mut_ptr()) } {
            return Err(format!("CLAP parameter info unavailable for index {index}").into());
        }
        // SAFETY: get_info succeeded. The structure contains numbers, pointers, and byte arrays.
        let parameter = unsafe { parameter.assume_init() };
        let name = array_text(parameter.name)?;
        let module = array_text(parameter.module)?;
        let flags = flags(parameter.flags);
        let expected_default = expected
            .iter()
            .find(|(expected_name, _)| *expected_name == name)
            .map(|(_, default)| *default)
            .ok_or_else(|| format!("unexpected parameter {name}"))?;
        let mut current = f64::NAN;
        // SAFETY: This is an advertised parameter ID and current is an unaliased output value.
        let has_value = unsafe { value(plugin, parameter.id, &mut current) };
        if parameter.id == u32::MAX
            || !ids.insert(parameter.id)
            || scanned.iter().any(|item: &ParamInfo| item.name == name)
            || flags != param_flags::AUTOMATABLE
            || parameter.min_value != 0.0
            || parameter.max_value != 1.0
            || parameter.default_value != expected_default
            || !has_value
            || current != expected_default
        {
            return Err(format!("unexpected CLAP parameter properties for {name}").into());
        }
        scanned.push(ParamInfo {
            id: parameter.id,
            name,
            module,
            min: parameter.min_value,
            max: parameter.max_value,
            default: parameter.default_value,
            step_count: 0,
            flags,
        });
    }
    Ok(scanned)
}

fn flags(clap: u32) -> u32 {
    [
        (CLAP_PARAM_IS_AUTOMATABLE, param_flags::AUTOMATABLE),
        (CLAP_PARAM_IS_BYPASS, param_flags::BYPASS),
        (CLAP_PARAM_IS_ENUM, param_flags::ENUM),
        (CLAP_PARAM_IS_HIDDEN, param_flags::HIDDEN),
        (CLAP_PARAM_IS_READONLY, param_flags::READ_ONLY),
        (CLAP_PARAM_IS_STEPPED, param_flags::STEPPED),
    ]
    .into_iter()
    .filter_map(|(input, output)| (clap & input != 0).then_some(output))
    .fold(0, |all, flag| all | flag)
}

fn layout(plugin: &clap_plugin, params: &[ParamInfo], plain: bool) -> Result<Value> {
    let ptr = extension(plugin, EXTENSION_ID)?;
    if plain {
        if !ptr.is_null() {
            return Err("standard-only example advertises the native layout extension".into());
        }
        return Ok(Value::Null);
    }
    // SAFETY: This extension ID names a layout table owned by the initialized plugin.
    let extension = unsafe { ptr.cast::<LayoutExtension>().as_ref() }
        .ok_or("native layout extension missing from the built example")?;
    let get_layout = extension
        .get_layout
        .ok_or("native extension has no layout hook")?;
    let mut required = 0;
    // SAFETY: The sizing query passes no destination and a writable count.
    let status = unsafe { get_layout(plugin, ptr::null_mut(), 0, &mut required) };
    if status != 2 || !(1..=MAX_BYTES).contains(&required) {
        return Err(format!("invalid sizing reply: status {status}, size {required}").into());
    }

    let mut bytes = vec![0_u8; required as usize];
    let mut written = 0;
    // SAFETY: The length is the bounded count just returned by the same instance.
    let status = unsafe { get_layout(plugin, bytes.as_mut_ptr(), required, &mut written) };
    if status != 0 || written != required {
        return Err(format!("invalid layout copy: status {status}, size {written}").into());
    }

    let layout = NativeLayout::parse(&bytes).map_err(|error| error.to_string())?;
    layout
        .validate_params(params)
        .map_err(|error| error.to_string())?;
    let bindings: BTreeSet<_> = layout
        .pages
        .iter()
        .flat_map(|page| page.cells.iter().flat_map(|cell| cell.parameter_ids()))
        .collect();
    if bindings != params.iter().map(|param| param.id).collect() {
        return Err("not every advertised example parameter is bound in the layout".into());
    }
    let bilingual = |labels: &komponist_plugin_api::native_layout::Labels| {
        labels.contains_key("en") && labels.contains_key("de")
    };
    if layout.default_locale != "en"
        || layout.pages.iter().any(|page| {
            !bilingual(&page.labels)
                || page.cells.iter().any(|cell| {
                    if let Some(graph) = &cell.graph {
                        !(bilingual(&graph.name) && bilingual(&graph.description))
                    } else if let Some(canvas) = &cell.canvas {
                        !(bilingual(&canvas.name) && bilingual(&canvas.description))
                    } else {
                        !cell.labels.as_ref().is_some_and(bilingual)
                    }
                })
        })
    {
        return Err("example layout has incomplete English/German labels".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn display(plugin: &clap_plugin, plain: bool) -> Result<Value> {
    let pointer = extension(plugin, device_canvas::EXTENSION_ID)?;
    if plain {
        if !pointer.is_null() {
            return Err("standard-only example advertises the device display extension".into());
        }
        return Ok(Value::Null);
    }
    // SAFETY: The ID designates the display vtable of the initialized plugin.
    let table = unsafe { pointer.cast::<DisplayExtension>().as_ref() }
        .ok_or("example display extension missing")?;
    let draw = table.draw.ok_or("example display hook missing")?;
    let mut bytes = vec![0; device_canvas::MAX_BYTES];
    // SAFETY: The destination stays writable for the claimed capacity for this call.
    let written = unsafe { draw(plugin, bytes.as_mut_ptr(), device_canvas::MAX_BYTES as u32) };
    let bytes = bytes
        .get(..written as usize)
        .ok_or("oversized display list")?;
    let display =
        device_canvas::DisplayList::decode(bytes).ok_or("invalid example display list")?;
    if !display
        .frames
        .iter()
        .any(|frame| frame.key == "response" && !frame.primitives.is_empty())
    {
        return Err("the example produced no gain response canvas commands".into());
    }
    Ok(serde_json::to_value(display)?)
}

fn descriptor_text(text: *const c_char) -> Result<String> {
    if text.is_null() {
        return Err("missing CLAP descriptor text".into());
    }
    // SAFETY: CLAP requires these descriptor fields to be NUL-terminated UTF-8 strings.
    Ok(unsafe { CStr::from_ptr(text) }.to_str()?.to_owned())
}

fn array_text<const N: usize>(text: [c_char; N]) -> Result<String> {
    let bytes = text.map(|byte| byte as u8);
    Ok(CStr::from_bytes_until_nul(&bytes)?.to_str()?.to_owned())
}

fn extension(plugin: &clap_plugin, id: &CStr) -> Result<*const c_void> {
    let get_extension = plugin
        .get_extension
        .ok_or("CLAP plugin has no extension hook")?;
    // SAFETY: All callers use the initialized plugin on the main thread and a C string ID.
    Ok(unsafe { get_extension(plugin, id.as_ptr()) })
}

struct EntryGuard(unsafe extern "C" fn());

impl Drop for EntryGuard {
    fn drop(&mut self) {
        // SAFETY: This guard is created once after successful init and outlives the plugin.
        unsafe { (self.0)() };
    }
}

struct PluginGuard<'a> {
    plugin: &'a clap_plugin,
    destroy: unsafe extern "C" fn(*const clap_plugin),
}

impl Drop for PluginGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: The instance is live, never activated, and destroy is called only here.
        unsafe { (self.destroy)(self.plugin) };
    }
}

unsafe extern "C" fn empty_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

unsafe extern "C" fn ignore_request(_: *const clap_host) {}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"Native CLAP SDK validation host".as_ptr(),
    vendor: c"klang-studio".as_ptr(),
    url: c"https://github.com/klang-studio/komponist".as_ptr(),
    version: c"1.0".as_ptr(),
    get_extension: Some(empty_extension),
    request_restart: Some(ignore_request),
    request_process: Some(ignore_request),
    request_callback: Some(ignore_request),
};
