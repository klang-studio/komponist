//! Load the actual binary as a CLAP host and compare its two extension APIs.
#[path = "../abi.rs"]
mod abi;

use std::ffi::{CStr, CString, c_char, c_void};
use std::mem::MaybeUninit;
use std::path::Path;
use std::ptr;

use clap_sys::entry::clap_plugin_entry;
use clap_sys::ext::params::{CLAP_EXT_PARAMS, clap_param_info, clap_plugin_params};
use clap_sys::factory::plugin_factory::{CLAP_PLUGIN_FACTORY_ID, clap_plugin_factory};
use clap_sys::host::clap_host;
use clap_sys::version::{CLAP_VERSION, clap_version_is_compatible};
use libloading::{Library, Symbol};
use serde_json::{Value, json};

use abi::{
    ABSENT, BUFFER_TOO_SMALL, ERROR, EXTENSION_ID, LayoutExtension, MAX_LAYOUT_BYTES, OK, PLUGIN_ID,
};

unsafe extern "C" fn no_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

unsafe extern "C" fn no_request(_: *const clap_host) {}

fn probe(path: &Path, collision: bool) -> Value {
    let host = clap_host {
        clap_version: CLAP_VERSION,
        host_data: ptr::null_mut(),
        name: c"Komponist layout probe".as_ptr(),
        vendor: c"klang-studio".as_ptr(),
        url: c"https://github.com/klang-studio/komponist".as_ptr(),
        version: c"0.1.0".as_ptr(),
        get_extension: Some(no_extension),
        request_restart: Some(no_request),
        request_process: Some(no_request),
        request_callback: Some(no_request),
    };
    let path_c = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
    let plugin_id = CString::new(PLUGIN_ID).unwrap();

    // SAFETY: The verification script supplies our own prototype binary, which exports the CLAP
    // entry and this extension. Pointers from CLAP are used within the CLAP init/destroy lifecycle.
    unsafe {
        let library = Library::new(path).unwrap();
        let entry: Symbol<*const clap_plugin_entry> = library.get(b"clap_entry\0").unwrap();
        let entry = entry.as_ref().unwrap();
        assert!(clap_version_is_compatible(entry.clap_version));
        assert!(entry.init.unwrap()(path_c.as_ptr()));
        let factory = (entry.get_factory.unwrap()(CLAP_PLUGIN_FACTORY_ID.as_ptr())
            as *const clap_plugin_factory)
            .as_ref()
            .unwrap();
        assert_eq!(factory.get_plugin_count.unwrap()(factory), 1);
        let instance = factory.create_plugin.unwrap()(factory, &host, plugin_id.as_ptr());
        let plugin = instance.as_ref().expect("create CLAP instance");
        assert!(plugin.init.unwrap()(instance));

        let extension = plugin.get_extension.unwrap();
        let params = (extension(instance, CLAP_EXT_PARAMS.as_ptr()) as *const clap_plugin_params)
            .as_ref()
            .expect("standard CLAP params");
        let count = params.count.unwrap()(instance);
        assert_eq!(count, 2);
        let mut inventory = Vec::new();
        for index in 0..count {
            let mut info = MaybeUninit::<clap_param_info>::uninit();
            assert!(params.get_info.unwrap()(instance, index, info.as_mut_ptr()));
            let info = info.assume_init();
            let name = CStr::from_ptr(info.name.as_ptr()).to_str().unwrap();
            let mut value = f64::NAN;
            assert!(params.get_value.unwrap()(instance, info.id, &mut value));
            assert_eq!(value, info.default_value);
            inventory.push(json!({
                "id": info.id,
                "name": name,
                "flags": info.flags,
                "min": info.min_value,
                "max": info.max_value,
                "default": info.default_value,
            }));
        }
        let layout = (extension(instance, EXTENSION_ID.as_ptr()) as *const LayoutExtension)
            .as_ref()
            .expect("native layout extension table");
        let get_layout = layout.get_layout;
        let mut needed = u32::MAX;
        let status = get_layout(instance, ptr::null_mut(), 0, &mut needed);
        assert_eq!(
            get_layout(instance, ptr::null_mut(), 0, ptr::null_mut()),
            ERROR
        );
        let mut wrong_capacity_written = u32::MAX;
        assert_eq!(
            get_layout(instance, ptr::null_mut(), 10, &mut wrong_capacity_written),
            ERROR
        );
        assert_eq!(wrong_capacity_written, 0);

        let descriptor = if collision {
            assert_eq!(status, ABSENT, "colliding author IDs yield no layout");
            assert_eq!(needed, 0);
            assert_eq!(inventory[0]["id"], 2112);
            assert_eq!(inventory[1]["id"], inventory[0]["id"]);
            Value::Null
        } else {
            assert_eq!(status, BUFFER_TOO_SMALL);
            assert!((2..=MAX_LAYOUT_BYTES).contains(&needed));
            let mut dest = vec![0xa5; needed as usize + 1];
            let mut written = 0;
            assert_eq!(
                get_layout(instance, dest.as_mut_ptr(), needed - 1, &mut written),
                BUFFER_TOO_SMALL
            );
            assert_eq!(written, needed);
            assert!(dest.iter().all(|byte| *byte == 0xa5));
            assert_eq!(
                get_layout(instance, dest.as_mut_ptr(), needed, &mut written),
                OK
            );
            assert_eq!(written, needed);
            assert_eq!(
                dest[needed as usize], 0xa5,
                "bytes beyond capacity are untouched"
            );
            let descriptor: Value = serde_json::from_slice(&dest[..needed as usize]).unwrap();
            let cells = descriptor["pages"][0]["cells"].as_array().unwrap();
            assert_eq!(cells.len(), 2);
            for (cell, (name, expected)) in
                cells.iter().zip([("Gain", 3_165_055), ("Mix", 108_124)])
            {
                let param = inventory
                    .iter()
                    .find(|param| param["name"] == name)
                    .unwrap();
                assert_eq!(param["id"], expected);
                assert_eq!(cell["param_id"], param["id"]);
                assert_eq!(cell["labels"]["en"], name);
            }
            descriptor
        };

        plugin.destroy.unwrap()(instance);
        entry.deinit.unwrap()();
        json!({"id": PLUGIN_ID, "params": inventory, "layout": descriptor})
    }
}

fn main() {
    let mut cli = std::env::args_os().skip(1);
    let path = cli
        .next()
        .expect("usage: probe <plugin-path> [--collision]");
    let collision = match cli.next() {
        Some(arg) if arg == "--collision" => true,
        None => false,
        _ => panic!("usage: probe <plugin-path> [--collision]"),
    };
    assert!(cli.next().is_none());
    let path = Path::new(&path).canonicalize().unwrap();
    println!(
        "{}",
        serde_json::to_string_pretty(&probe(&path, collision)).unwrap()
    );
}
