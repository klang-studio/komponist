//! A stand-alone CLAP provider and plugin. It uses the public ABI without device SDK code.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use clap_sys::entry::clap_plugin_entry;
use clap_sys::ext::{preset_load::*, state::*, state_context::*};
use clap_sys::factory::{plugin_factory::*, preset_discovery::*};
use clap_sys::host::clap_host;
use clap_sys::plugin::{clap_plugin, clap_plugin_descriptor};
use clap_sys::stream::{clap_istream, clap_ostream};
use clap_sys::universal_plugin_id::clap_universal_plugin_id;
use clap_sys::version::CLAP_VERSION;

use crate::HostedPlugin;
use komponist_ipc::clap_presets::PresetReport;

use super::*;

const ID: &CStr = c"org.example.community-presets";
const PRESET_KEY: &CStr = c"number://42";
const PROVIDER: clap_preset_discovery_provider_descriptor =
    clap_preset_discovery_provider_descriptor {
        clap_version: CLAP_VERSION,
        id: c"org.example.provider".as_ptr(),
        name: c"Community provider".as_ptr(),
        vendor: c"Community".as_ptr(),
    };

static FACTORY: clap_preset_discovery_factory = clap_preset_discovery_factory {
    count: Some(provider_count),
    get_descriptor: Some(provider_descriptor),
    create: Some(create_provider),
};

static DESCRIPTOR: clap_plugin_descriptor = clap_plugin_descriptor {
    clap_version: CLAP_VERSION,
    id: ID.as_ptr(),
    name: c"Community Presets".as_ptr(),
    vendor: c"Community".as_ptr(),
    url: ptr::null(),
    manual_url: ptr::null(),
    support_url: ptr::null(),
    version: c"1.0.0".as_ptr(),
    description: ptr::null(),
    features: ptr::null(),
};

static PLUGIN_FACTORY: clap_plugin_factory = clap_plugin_factory {
    get_plugin_count: Some(plugin_count),
    get_plugin_descriptor: Some(plugin_descriptor),
    create_plugin: Some(create_plugin),
};

static ENTRY: clap_plugin_entry = clap_plugin_entry {
    clap_version: CLAP_VERSION,
    init: Some(init_entry),
    deinit: Some(deinit_entry),
    get_factory: Some(factory),
};

/// The host of the instance, for the host side of preset-load.
static HOST: AtomicPtr<clap_host> = AtomicPtr::new(ptr::null_mut());

/// Calls the host side of preset-load like a plug-in that reports its loads.
fn report(key: &CStr, error: Option<&CStr>) {
    let host = HOST.load(Ordering::Acquire);
    // SAFETY: the instance's host stays alive while the instance calls it on the main thread.
    unsafe {
        let ext = ((*host).get_extension.unwrap())(host, CLAP_EXT_PRESET_LOAD.as_ptr())
            .cast::<clap_host_preset_load>();
        assert!(!ext.is_null(), "the host offers preset-load");
        match error {
            None => ((*ext).loaded.unwrap())(
                host,
                CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
                ptr::null(),
                key.as_ptr(),
            ),
            Some(message) => ((*ext).on_error.unwrap())(
                host,
                CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
                ptr::null(),
                key.as_ptr(),
                0,
                message.as_ptr(),
            ),
        }
    }
}

static PRESETS: clap_plugin_preset_load = clap_plugin_preset_load {
    from_location: Some(load),
};

static STATE: clap_plugin_state = clap_plugin_state {
    save: Some(save),
    load: Some(restore),
};

static STATE_CONTEXT: clap_plugin_state_context = clap_plugin_state_context {
    save: Some(save_context),
    load: Some(restore_context),
};

unsafe extern "C" fn init_entry(_: *const c_char) -> bool {
    true
}
unsafe extern "C" fn deinit_entry() {}

unsafe extern "C" fn factory(id: *const c_char) -> *const c_void {
    // SAFETY: the host supplies a valid factory ID.
    match unsafe { CStr::from_ptr(id) } {
        id if id == CLAP_PRESET_DISCOVERY_FACTORY_ID => ptr::from_ref(&FACTORY).cast(),
        id if id == CLAP_PLUGIN_FACTORY_ID => ptr::from_ref(&PLUGIN_FACTORY).cast(),
        _ => ptr::null(),
    }
}

unsafe extern "C" fn provider_count(_: *const clap_preset_discovery_factory) -> u32 {
    1
}
unsafe extern "C" fn provider_descriptor(
    _: *const clap_preset_discovery_factory,
    index: u32,
) -> *const clap_preset_discovery_provider_descriptor {
    if index == 0 { &PROVIDER } else { ptr::null() }
}

unsafe extern "C" fn create_provider(
    _: *const clap_preset_discovery_factory,
    indexer: *const clap_preset_discovery_indexer,
    _: *const c_char,
) -> *const clap_preset_discovery_provider {
    Box::into_raw(Box::new(clap_preset_discovery_provider {
        desc: &PROVIDER,
        provider_data: indexer.cast_mut().cast(),
        init: Some(init_provider),
        destroy: Some(destroy_provider),
        get_metadata: Some(metadata),
        get_extension: None,
    }))
}

unsafe extern "C" fn init_provider(provider: *const clap_preset_discovery_provider) -> bool {
    let location = clap_preset_discovery_location {
        flags: CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
        name: c"Factory".as_ptr(),
        kind: CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
        location: ptr::null(),
    };
    // SAFETY: create_provider retained the live indexer and its callbacks may run in init.
    unsafe {
        let indexer = (*provider)
            .provider_data
            .cast::<clap_preset_discovery_indexer>();
        ((*indexer).declare_location.unwrap())(indexer, &location)
    }
}

unsafe extern "C" fn destroy_provider(provider: *const clap_preset_discovery_provider) {
    // SAFETY: this is the allocation from create_provider and destroy is called once.
    drop(unsafe { Box::from_raw(provider.cast_mut()) });
}

unsafe extern "C" fn metadata(
    _: *const clap_preset_discovery_provider,
    kind: u32,
    location: *const c_char,
    receiver: *const clap_preset_discovery_metadata_receiver,
) -> bool {
    if kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || !location.is_null() {
        return false;
    }
    // SAFETY: the receiver and these callback arguments are valid for the duration of the call.
    unsafe {
        let id = clap_universal_plugin_id {
            abi: c"clap".as_ptr(),
            id: ID.as_ptr(),
        };
        if !((*receiver).begin_preset.unwrap())(receiver, c"New name".as_ptr(), PRESET_KEY.as_ptr())
        {
            return false;
        }
        ((*receiver).add_plugin_id.unwrap())(receiver, &id);
        ((*receiver).add_feature.unwrap())(receiver, c"keys".as_ptr());
        let extra = (*receiver).add_extra_info.unwrap();
        extra(
            receiver,
            c"org.klang-studio.komponist.preset-name.de".as_ptr(),
            c"Neuer Name".as_ptr(),
        );
        extra(
            receiver,
            c"org.example.unrelated".as_ptr(),
            c"ignored".as_ptr(),
        );
    }
    true
}

unsafe extern "C" fn plugin_count(_: *const clap_plugin_factory) -> u32 {
    1
}
unsafe extern "C" fn plugin_descriptor(
    _: *const clap_plugin_factory,
    index: u32,
) -> *const clap_plugin_descriptor {
    if index == 0 { &DESCRIPTOR } else { ptr::null() }
}

unsafe extern "C" fn create_plugin(
    _: *const clap_plugin_factory,
    host: *const clap_host,
    _: *const c_char,
) -> *const clap_plugin {
    HOST.store(host.cast_mut(), Ordering::Release);
    Box::into_raw(Box::new(clap_plugin {
        desc: &DESCRIPTOR,
        plugin_data: Box::into_raw(Box::new(Cell::new(0u8))).cast(),
        init: Some(init_plugin),
        destroy: Some(destroy_plugin),
        activate: None,
        deactivate: None,
        start_processing: None,
        stop_processing: None,
        reset: None,
        process: None,
        get_extension: Some(extension),
        on_main_thread: Some(main_thread),
    }))
}

/// The plug-in's own browser loads the preset and reports it.
unsafe extern "C" fn main_thread(plugin: *const clap_plugin) {
    // SAFETY: the plugin data is the instance's own allocation.
    unsafe { (*(*plugin).plugin_data.cast::<Cell<u8>>()).set(42) };
    report(PRESET_KEY, None);
}

/// The plug-in names the preset it starts in while it initialises.
unsafe extern "C" fn init_plugin(_: *const clap_plugin) -> bool {
    report(PRESET_KEY, None);
    true
}

unsafe extern "C" fn destroy_plugin(plugin: *const clap_plugin) {
    // SAFETY: create_plugin owns both allocations and the instance is destroyed once.
    unsafe {
        let raw = Box::from_raw(plugin.cast_mut());
        drop(Box::from_raw(raw.plugin_data.cast::<Cell<u8>>()));
    }
}

unsafe extern "C" fn extension(_: *const clap_plugin, id: *const c_char) -> *const c_void {
    // SAFETY: the host supplies a valid extension ID.
    match unsafe { CStr::from_ptr(id) } {
        id if id == CLAP_EXT_PRESET_LOAD => ptr::from_ref(&PRESETS).cast(),
        id if id == CLAP_EXT_STATE => ptr::from_ref(&STATE).cast(),
        id if id == CLAP_EXT_STATE_CONTEXT => ptr::from_ref(&STATE_CONTEXT).cast(),
        _ => ptr::null(),
    }
}

unsafe extern "C" fn load(
    plugin: *const clap_plugin,
    kind: u32,
    path: *const c_char,
    key: *const c_char,
) -> bool {
    if kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || !path.is_null() || key.is_null() {
        return false;
    }
    // SAFETY: the plugin and the non-null key are provided by the live host instance.
    unsafe {
        let key = CStr::from_ptr(key);
        if key != PRESET_KEY {
            report(key, Some(c"no such preset"));
            return false;
        }
        (*(*plugin).plugin_data.cast::<Cell<u8>>()).set(42);
        report(key, None);
    }
    true
}

unsafe extern "C" fn save(plugin: *const clap_plugin, stream: *const clap_ostream) -> bool {
    // SAFETY: state is the live plugin's own allocation and the stream comes from the host.
    unsafe {
        let value = (*(*plugin).plugin_data.cast::<Cell<u8>>()).get();
        ((*stream).write.unwrap())(stream, ptr::from_ref(&value).cast(), 1) == 1
    }
}

unsafe extern "C" fn restore(plugin: *const clap_plugin, stream: *const clap_istream) -> bool {
    let mut value = 0u8;
    // SAFETY: state is the live plugin's own allocation and the stream comes from the host.
    unsafe {
        if ((*stream).read.unwrap())(stream, ptr::from_mut(&mut value).cast(), 1) != 1 {
            return false;
        }
        (*(*plugin).plugin_data.cast::<Cell<u8>>()).set(value);
    }
    true
}

unsafe extern "C" fn save_context(
    plugin: *const clap_plugin,
    stream: *const clap_ostream,
    context: u32,
) -> bool {
    // SAFETY: these are the same live pointers the ordinary state callback accepts.
    context == CLAP_STATE_CONTEXT_FOR_PRESET && unsafe { save(plugin, stream) }
}

unsafe extern "C" fn restore_context(
    plugin: *const clap_plugin,
    stream: *const clap_istream,
    context: u32,
) -> bool {
    // SAFETY: these are the same live pointers the ordinary state callback accepts.
    context == CLAP_STATE_CONTEXT_FOR_PRESET && unsafe { restore(plugin, stream) }
}

#[test]
fn community_clap_discovers_loads_and_round_trips_a_preset() {
    // SAFETY: the static entry uses matching ABI types and keeps all tables and instance data alive.
    let entry = unsafe { PluginEntry::load_from_raw(&ENTRY, c"community.clap") }.unwrap();
    let host = HostInfo::new("Test Host", "", "", "1.0.0").unwrap();
    let presets = discover_entry(&entry, &host);
    let [preset] = presets.as_slice() else {
        panic!("unexpected presets {presets:?}")
    };
    assert_eq!(preset.plugin_id, ID.to_str().unwrap());
    assert_eq!(preset.display_name("DE"), "Neuer Name");
    assert_eq!(preset.display_name("fr"), "New name");
    assert_eq!(preset.features, ["keys"]);
    assert!(preset.factory);
    assert_eq!(
        preset.source.load_key.as_deref(),
        Some(PRESET_KEY.to_bytes())
    );

    let mut plugin = super::super::ClapPlugin::load_from_entry(
        Path::new("community.clap"),
        &preset.plugin_id,
        crate::scan::probe_context(),
        entry,
    )
    .unwrap();
    // A report during creation and init names the preset the plug-in starts in.
    assert_eq!(
        plugin.take_preset_reports(),
        [PresetReport::Loaded {
            source: preset.source.clone(),
            startup: true,
            epoch: 1,
        }]
    );
    plugin.load_clap_preset(preset).unwrap();
    let saved = plugin.save_preset_state().unwrap();
    assert_eq!(saved, [42]);
    plugin.load_preset_state(&[17]).unwrap();
    assert_eq!(plugin.save_preset_state().unwrap(), [17]);
    plugin.load_preset_state(&saved).unwrap();
    assert_eq!(plugin.save_state().unwrap(), [42]);

    let mut wrong_preset = preset.clone();
    wrong_preset.plugin_id = "org.example.unrelated".into();
    assert!(plugin.load_clap_preset(&wrong_preset).is_err());
    wrong_preset.plugin_id.clone_from(&preset.plugin_id);
    wrong_preset.source.load_key = Some(b"unknown".to_vec());
    let error = plugin
        .load_clap_preset(&wrong_preset)
        .unwrap_err()
        .to_string();
    assert!(error.contains("no such preset"), "{error}");
    assert_eq!(plugin.save_state().unwrap(), [42]);
    // Reports during the host's own loads belong to those loads.
    assert_eq!(plugin.take_preset_reports(), []);

    // A failed request leaves the earlier successful report and its loaded state in place.
    plugin.on_main_thread();
    assert!(plugin.load_clap_preset(&wrong_preset).is_err());
    assert_eq!(
        plugin.take_preset_reports(),
        [PresetReport::Loaded {
            source: preset.source.clone(),
            startup: false,
            epoch: plugin
                .preset_load_epoch()
                .unwrap()
                .load(std::sync::atomic::Ordering::Acquire),
        }]
    );
    assert_eq!(plugin.save_state().unwrap(), [42]);

    // A load the plug-in reported before a successful host load is superseded by it.
    plugin.on_main_thread();
    plugin.load_clap_preset(preset).unwrap();
    assert_eq!(plugin.take_preset_reports(), []);

    // A load the plug-in starts itself reaches the host through preset-load.
    plugin.load_state(&[0]).unwrap();
    plugin.on_main_thread();
    assert_eq!(
        plugin.take_preset_reports(),
        [PresetReport::Loaded {
            source: preset.source.clone(),
            startup: false,
            epoch: plugin
                .preset_load_epoch()
                .unwrap()
                .load(std::sync::atomic::Ordering::Acquire),
        }]
    );
    assert_eq!(plugin.take_preset_reports(), []);
    assert_eq!(plugin.save_state().unwrap(), [42]);

    // Older plug-ins only expose the ordinary state extension.
    plugin.ext.state_context = None;
    plugin.load_preset_state(&[64]).unwrap();
    assert_eq!(plugin.save_preset_state().unwrap(), [64]);
}

#[cfg(unix)]
#[test]
fn follows_declared_symlink_roots_but_skips_links_inside_them() {
    use std::os::unix::fs::symlink;

    let dir = std::env::temp_dir().join(format!("kp-preset-root-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let actual = dir.join("actual");
    fs::create_dir_all(&actual).unwrap();
    fs::write(actual.join("real.preset"), []).unwrap();
    symlink("real.preset", actual.join("alias.preset")).unwrap();
    symlink(".", actual.join("cycle")).unwrap();
    let folder = dir.join("linked-folder");
    symlink("actual", &folder).unwrap();
    let file = dir.join("linked-file.preset");
    symlink("actual/real.preset", &file).unwrap();

    let extensions = BTreeSet::from(["preset".to_owned()]);
    let mut budget = 5;
    let location = |path: &Path| ClapPresetLocation::File(path.to_str().unwrap().to_owned());
    assert_eq!(
        preset_files(&folder, &extensions, &mut budget),
        [location(&folder.join("real.preset"))]
    );
    assert_eq!(budget, 2);
    assert_eq!(
        preset_files(&file, &extensions, &mut budget),
        [location(&file)]
    );
    assert_eq!(budget, 1);
    fs::remove_dir_all(dir).unwrap();
}
