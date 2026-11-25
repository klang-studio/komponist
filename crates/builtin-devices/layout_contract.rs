//! Common native-layout ABI checks for the bundled plugins.

use std::ffi::{CStr, c_char, c_void};
use std::ptr;
use std::sync::Arc;

use clap_sys::host::clap_host;
use clap_sys::plugin::clap_plugin;
use clap_sys::version::CLAP_VERSION;
use komponist_native_clap_sdk::{EXTENSION_ID, NativeClapLayout};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;

unsafe extern "C" fn no_extension(_: *const clap_host, _: *const c_char) -> *const c_void {
    ptr::null()
}

static HOST: clap_host = clap_host {
    clap_version: CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: c"Bundled CLAP test host".as_ptr(),
    vendor: c"klang-studio".as_ptr(),
    url: c"https://github.com/klang-studio/komponist".as_ptr(),
    version: c"1".as_ptr(),
    get_extension: Some(no_extension),
    request_restart: None,
    request_process: None,
    request_callback: None,
};

pub(crate) struct Instance(pub(crate) *const clap_plugin);

impl Instance {
    pub(crate) fn new<P: ClapPlugin>() -> Self {
        // SAFETY: The static host remains live for the instance. Transfer the returned Arc to
        // CLAP; the destroy callback consumes it.
        unsafe {
            let wrapper = Arc::into_raw(Wrapper::<P>::new(&HOST).unwrap());
            let instance = Self((*wrapper).clap_plugin.as_ptr());
            assert!(((*instance.0).init.unwrap())(instance.0));
            instance
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: This Instance owns the reference transferred during construction.
        unsafe { ((*self.0).destroy.unwrap())(self.0) };
    }
}

#[repr(C)]
struct LayoutExtension {
    get_layout: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, *mut u32) -> u32,
}

fn layout_bytes(instance: &Instance, extension_id: &CStr) -> Vec<u8> {
    let plugin = instance.0;
    // SAFETY: The plugin is initialized and this table follows the published C layout.
    unsafe {
        let extension = ((*plugin).get_extension.unwrap())(plugin, extension_id.as_ptr());
        assert!(!extension.is_null());
        let layout = &*extension.cast::<LayoutExtension>();
        let mut size = 0;
        assert_eq!(
            (layout.get_layout)(plugin, ptr::null_mut(), 0, &mut size),
            2
        );
        assert!((1..=65_536).contains(&size));
        let mut bytes = vec![0; size as usize];
        let mut written = 0;
        assert_eq!(
            (layout.get_layout)(plugin, bytes.as_mut_ptr(), size, &mut written),
            0
        );
        assert_eq!(written, size);
        bytes
    }
}

/// Check the exported native layout on two instances.
pub(crate) fn read_layout<P: NativeClapLayout>() -> serde_json::Value {
    read_layout_extension::<P>(EXTENSION_ID)
}

pub(crate) fn read_layout_extension<P: NativeClapLayout>(extension_id: &CStr) -> serde_json::Value {
    let first = Instance::new::<P>();
    let second = Instance::new::<P>();
    let bytes = layout_bytes(&first, extension_id);
    assert_eq!(bytes, layout_bytes(&second, extension_id));
    let layout: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    for locale in ["en", "de"] {
        for label in ["names", "descriptions"] {
            assert!(
                layout["metadata"][label][locale]
                    .as_str()
                    .is_some_and(|text| !text.is_empty()),
                "{} has no {locale} {label} in its CLAP layout",
                P::NAME,
            );
        }
    }
    layout
}

pub(crate) fn assert_localized_bindings<P: NativeClapLayout>(count: usize) {
    let layout = read_layout::<P>();
    assert_eq!(layout["locales"], serde_json::json!(["en", "de"]));
    let ids: std::collections::BTreeSet<_> = layout["pages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|page| page["cells"].as_array().unwrap())
        .map(|cell| cell["param_id"].as_u64().unwrap())
        .collect();
    assert_eq!(ids.len(), count);
}
