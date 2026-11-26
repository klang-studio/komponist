//! Standard CLAP discovery and loading for embedded presets.

use std::borrow::Cow;
use std::ffi::{CStr, CString, c_char, c_void};
use std::marker::PhantomData;
use std::ptr;
use std::sync::{Arc, OnceLock};

use komponist_device_core::pinned_clap_state::{self, FACTORY_PRESET_FIELD};
use komponist_native_clap_sdk::clap_abi::{
    ext::{preset_load::*, state::*},
    factory::{plugin_factory::*, preset_discovery::*},
    host::clap_host,
    plugin::{clap_plugin, clap_plugin_descriptor},
    stream::clap_istream,
    universal_plugin_id::clap_universal_plugin_id,
    version::CLAP_VERSION,
};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::{LibraryWorkers, PluginDescriptor, Wrapper};

use crate::DeviceDefinition;

/// Metadata for an embedded preset. The load key is stable independently of its label.
#[doc(hidden)]
pub struct FactoryPreset {
    pub name: &'static str,
    pub key: Cow<'static, str>,
    pub category: Option<&'static str>,
}

/// A device's embedded preset inventory and state encoding.
#[doc(hidden)]
pub trait FactoryPresets {
    fn metadata() -> impl Iterator<Item = FactoryPreset>;

    fn aliases() -> &'static [(&'static str, &'static str)] {
        &[]
    }

    fn state(key: &str) -> Option<Vec<u8>>;
}

impl<T: DeviceDefinition> FactoryPresets for T {
    fn metadata() -> impl Iterator<Item = FactoryPreset> {
        T::SPEC.presets().iter().map(|preset| FactoryPreset {
            name: preset.name,
            key: preset.name.into(),
            category: preset.category,
        })
    }

    fn state(key: &str) -> Option<Vec<u8>> {
        let index = T::SPEC
            .presets()
            .iter()
            .position(|preset| preset.name == key)?;
        let values = T::SPEC.preset_values(index)?;
        let params = T::SPEC
            .params()
            .iter()
            .zip(values)
            .map(|(info, value)| {
                (
                    info.id.as_str().into(),
                    pinned_clap_state::parameter(info, value).0,
                )
            })
            .collect();
        let fields = [(
            FACTORY_PRESET_FIELD.into(),
            serde_json::json!(key).to_string().into(),
        )]
        .into_iter()
        .collect();
        Some(pinned_clap_state::encode(T::VERSION, params, fields))
    }
}

/// The ordinary nice-plug instance factory and the additional preset provider of one device.
#[doc(hidden)]
pub struct PresetEntry<T, P> {
    plugin: OnceLock<PluginDescriptor>,
    provider: OnceLock<clap_preset_discovery_provider_descriptor>,
    workers: LibraryWorkers,
    marker: PhantomData<fn() -> (T, P)>,
}

impl<T: FactoryPresets, P: ClapPlugin> Default for PresetEntry<T, P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: FactoryPresets, P: ClapPlugin> PresetEntry<T, P> {
    pub const fn new() -> Self {
        Self {
            plugin: OnceLock::new(),
            provider: OnceLock::new(),
            workers: LibraryWorkers::new(),
            marker: PhantomData,
        }
    }

    pub fn init(&self) -> bool {
        nice_plug::wrapper::setup_logger::<P>();
        self.plugin.get_or_init(PluginDescriptor::for_plugin::<P>);
        self.workers.initialize();
        true
    }

    pub fn deinit(&self) {
        self.workers.deinitialize();
    }

    pub fn descriptor(&self, index: u32) -> *const clap_plugin_descriptor {
        if index != 0 {
            return ptr::null();
        }
        self.plugin.get().map_or(ptr::null(), |plugin| {
            ptr::from_ref(plugin.clap_plugin_descriptor())
        })
    }

    /// # Safety
    /// The host must be live and the ID must be a valid C string, or either pointer may be null.
    pub unsafe fn create(&self, host: *const clap_host, id: *const c_char) -> *const clap_plugin {
        if host.is_null() || id.is_null() {
            return ptr::null();
        }
        // SAFETY: The caller supplies the CLAP factory arguments for this call.
        let id = unsafe { CStr::from_ptr(id) };
        if !self
            .plugin
            .get()
            .is_some_and(|plugin| plugin.clap_id() == id)
        {
            return ptr::null();
        }
        // SAFETY: The host stays live for this instance, as required by CLAP.
        let Some(instance) = (unsafe { Wrapper::<P>::new(host) }) else {
            return ptr::null();
        };
        instance.retain_background_worker(&self.workers);
        // SAFETY: The nice-plug destroy callback owns the retained Arc and releases it.
        unsafe { (*Arc::into_raw(instance)).clap_plugin.as_ptr() }
    }

    pub fn provider(&self, index: u32) -> *const clap_preset_discovery_provider_descriptor {
        // SAFETY: Non-null descriptors returned here are owned by the entry and stay live.
        let Some(plugin) = (unsafe { self.descriptor(index).as_ref() }) else {
            return ptr::null();
        };
        self.provider
            .get_or_init(|| clap_preset_discovery_provider_descriptor {
                clap_version: CLAP_VERSION,
                id: plugin.id,
                name: plugin.name,
                vendor: c"klang-studio".as_ptr(),
            })
    }

    /// # Safety
    /// The indexer must stay live through provider destruction and ID must be valid or null.
    pub unsafe fn create_provider(
        &self,
        indexer: *const clap_preset_discovery_indexer,
        id: *const c_char,
    ) -> *const clap_preset_discovery_provider {
        let descriptor = self.provider(0);
        if indexer.is_null() || id.is_null() || descriptor.is_null() {
            return ptr::null();
        }
        // SAFETY: Both identifier pointers are live C strings during this factory call.
        if unsafe { CStr::from_ptr(id) != CStr::from_ptr((*descriptor).id) } {
            return ptr::null();
        }
        Box::into_raw(Box::new(clap_preset_discovery_provider {
            desc: descriptor,
            provider_data: indexer.cast_mut().cast(),
            init: Some(provider_init),
            destroy: Some(provider_destroy),
            get_metadata: Some(provider_metadata::<T>),
            get_extension: None,
        }))
    }
}

/// # Safety
/// The ID must be a valid C string or null.
#[doc(hidden)]
pub unsafe fn factory(
    id: *const c_char,
    plugin: &'static clap_plugin_factory,
    presets: &'static clap_preset_discovery_factory,
) -> *const c_void {
    if id.is_null() {
        return ptr::null();
    }
    // SAFETY: Validity of the ID is guaranteed by the factory caller.
    let id = unsafe { CStr::from_ptr(id) };
    if id == CLAP_PLUGIN_FACTORY_ID {
        ptr::from_ref(plugin).cast()
    } else if id == CLAP_PRESET_DISCOVERY_FACTORY_ID
        || id == CLAP_PRESET_DISCOVERY_FACTORY_ID_COMPAT
    {
        ptr::from_ref(presets).cast()
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn provider_init(provider: *const clap_preset_discovery_provider) -> bool {
    let location = clap_preset_discovery_location {
        flags: CLAP_PRESET_DISCOVERY_IS_FACTORY_CONTENT,
        name: c"Factory presets".as_ptr(),
        kind: CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN,
        location: ptr::null(),
    };
    // SAFETY: The provider holds the live host indexer supplied when the instance was created.
    unsafe {
        let indexer = (*provider)
            .provider_data
            .cast::<clap_preset_discovery_indexer>();
        (*indexer)
            .declare_location
            .is_some_and(|declare| declare(indexer, &location))
    }
}

unsafe extern "C" fn provider_destroy(provider: *const clap_preset_discovery_provider) {
    // SAFETY: The factory allocated the instance as a box and CLAP destroys it exactly once.
    drop(unsafe { Box::from_raw(provider.cast_mut()) });
}

unsafe extern "C" fn provider_metadata<T: FactoryPresets>(
    provider: *const clap_preset_discovery_provider,
    kind: u32,
    location: *const c_char,
    receiver: *const clap_preset_discovery_metadata_receiver,
) -> bool {
    if kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN || !location.is_null() || receiver.is_null() {
        return false;
    }
    // SAFETY: The provider and receiver are live during this synchronous CLAP call.
    let (receiver_ref, plugin_id) = unsafe { (&*receiver, (*(*provider).desc).id) };
    let (Some(begin), Some(compatible)) = (receiver_ref.begin_preset, receiver_ref.add_plugin_id)
    else {
        return false;
    };
    let plugin = clap_universal_plugin_id {
        abi: c"clap".as_ptr(),
        id: plugin_id,
    };
    for preset in T::metadata() {
        let (Ok(name), Ok(key)) = (
            CString::new(preset.name),
            CString::new(preset.key.into_owned()),
        ) else {
            return false;
        };
        // SAFETY: The strings and plugin descriptor live through the synchronous callback.
        if !unsafe { begin(receiver, name.as_ptr(), key.as_ptr()) } {
            return false;
        }
        // SAFETY: The receiver and descriptor are valid for this callback.
        unsafe { compatible(receiver, &plugin) };
        if let Some(category) = preset.category
            && let Some(add_feature) = receiver_ref.add_feature
            && let Ok(category) = CString::new(category)
        {
            // SAFETY: The category remains alive through this synchronous callback.
            unsafe { add_feature(receiver, category.as_ptr()) };
        }
    }
    true
}

pub(crate) fn load_extension<T: DeviceDefinition>(id: &CStr) -> *const c_void {
    if T::PUBLISH_FACTORY_PRESETS {
        embedded_load_extension::<T>(id)
    } else {
        ptr::null()
    }
}

/// The standard load table for a device that publishes embedded factory presets.
#[doc(hidden)]
pub fn embedded_load_extension<T: FactoryPresets>(id: &CStr) -> *const c_void {
    if id == CLAP_EXT_PRESET_LOAD || id == CLAP_EXT_PRESET_LOAD_COMPAT {
        let table: &'static clap_plugin_preset_load = &clap_plugin_preset_load {
            from_location: Some(load::<T>),
        };
        ptr::from_ref(table).cast()
    } else {
        ptr::null()
    }
}

unsafe extern "C" fn load<T: FactoryPresets>(
    plugin: *const clap_plugin,
    kind: u32,
    location: *const c_char,
    key: *const c_char,
) -> bool {
    if plugin.is_null()
        || key.is_null()
        || !location.is_null()
        || kind != CLAP_PRESET_DISCOVERY_LOCATION_PLUGIN
    {
        return false;
    }
    // SAFETY: CLAP holds the supplied strings and instance alive through the callback.
    let (Ok(key), Some(query)) = (unsafe { CStr::from_ptr(key) }.to_str(), unsafe {
        (*plugin).get_extension
    }) else {
        return false;
    };
    let Some(state) = T::state(key) else {
        return false;
    };
    // SAFETY: The plugin is live and the extension ID requests the standard state table.
    let table = unsafe {
        query(plugin, CLAP_EXT_STATE.as_ptr())
            .cast::<clap_plugin_state>()
            .as_ref()
    };
    let Some(restore) = table.and_then(|table| table.load) else {
        return false;
    };
    let mut bytes = state.as_slice();
    let input = clap_istream {
        ctx: ptr::from_mut(&mut bytes).cast(),
        read: Some(read),
    };
    // SAFETY: The input stream and its bytes remain live through the synchronous state callback.
    unsafe { restore(plugin, &input) }
}

unsafe extern "C" fn read(stream: *const clap_istream, output: *mut c_void, count: u64) -> i64 {
    // SAFETY: The pinned wrapper passes this live input stream and a writable output buffer.
    let bytes = unsafe { &mut *(*stream).ctx.cast::<&[u8]>() };
    let count = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .min(bytes.len());
    if count != 0 {
        // SAFETY: The stream has at least count readable bytes and the host owns the output buffer.
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast(), count) };
        *bytes = &bytes[count..];
    }
    count as i64
}

/// Exports a spec-based device and its standard CLAP preset provider from one library.
/// Opt in using the definition's `PUBLISH_FACTORY_PRESETS` flag.
#[macro_export]
macro_rules! export_spec_with_presets {
    ($definition:ty, $plugin:ty) => {
        const _: () = assert!(<$definition as $crate::DeviceDefinition>::PUBLISH_FACTORY_PRESETS);
        $crate::export_with_presets!($definition, $plugin);
    };
}

/// Exports a device and its embedded preset provider from one library.
#[macro_export]
macro_rules! export_with_presets {
    ($definition:ty, $plugin:ty) => {
        #[doc(hidden)]
        mod clap_with_presets {
            use std::ffi::{c_char, c_void};

            use super::*;
            use $crate::presets::{PresetEntry, abi::*};

            static ENTRY: PresetEntry<$definition, $plugin> = PresetEntry::new();
            static PLUGINS: clap_plugin_factory = clap_plugin_factory {
                get_plugin_count: Some(plugin_count),
                get_plugin_descriptor: Some(plugin_descriptor),
                create_plugin: Some(create_plugin),
            };
            static PRESETS: clap_preset_discovery_factory = clap_preset_discovery_factory {
                count: Some(provider_count),
                get_descriptor: Some(provider_descriptor),
                create: Some(create_provider),
            };

            pub(super) extern "C" fn init(_: *const c_char) -> bool {
                ENTRY.init()
            }

            pub(super) extern "C" fn deinit() {
                ENTRY.deinit();
            }

            pub(super) unsafe extern "C" fn factory(id: *const c_char) -> *const c_void {
                // SAFETY: The host supplies a valid CLAP factory ID or null.
                unsafe { $crate::presets::factory(id, &PLUGINS, &PRESETS) }
            }

            unsafe extern "C" fn plugin_count(_: *const clap_plugin_factory) -> u32 {
                1
            }

            unsafe extern "C" fn plugin_descriptor(
                _: *const clap_plugin_factory,
                index: u32,
            ) -> *const clap_plugin_descriptor {
                ENTRY.descriptor(index)
            }

            unsafe extern "C" fn create_plugin(
                _: *const clap_plugin_factory,
                host: *const clap_host,
                id: *const c_char,
            ) -> *const clap_plugin {
                // SAFETY: CLAP holds the host and ID live for their respective lifetimes.
                unsafe { ENTRY.create(host, id) }
            }

            unsafe extern "C" fn provider_count(_: *const clap_preset_discovery_factory) -> u32 {
                1
            }

            unsafe extern "C" fn provider_descriptor(
                _: *const clap_preset_discovery_factory,
                index: u32,
            ) -> *const clap_preset_discovery_provider_descriptor {
                ENTRY.provider(index)
            }

            unsafe extern "C" fn create_provider(
                _: *const clap_preset_discovery_factory,
                indexer: *const clap_preset_discovery_indexer,
                id: *const c_char,
            ) -> *const clap_preset_discovery_provider {
                // SAFETY: CLAP holds the indexer and ID live for their respective lifetimes.
                unsafe { ENTRY.create_provider(indexer, id) }
            }
        }

        #[unsafe(no_mangle)]
        #[used]
        pub static clap_entry: $crate::presets::abi::clap_plugin_entry =
            $crate::presets::abi::clap_plugin_entry {
                clap_version: $crate::presets::abi::CLAP_VERSION,
                init: Some(clap_with_presets::init),
                deinit: Some(clap_with_presets::deinit),
                get_factory: Some(clap_with_presets::factory),
            };
    };
}

#[doc(hidden)]
pub mod abi {
    pub use komponist_native_clap_sdk::clap_abi::{
        entry::clap_plugin_entry,
        factory::{
            plugin_factory::clap_plugin_factory,
            preset_discovery::{
                clap_preset_discovery_factory, clap_preset_discovery_indexer,
                clap_preset_discovery_provider, clap_preset_discovery_provider_descriptor,
            },
        },
        host::clap_host,
        plugin::{clap_plugin, clap_plugin_descriptor},
        version::CLAP_VERSION,
    };
}
