//! Typed callbacks for the optional device file-slot and offline preparation extension.

use std::ffi::{CStr, c_void};
use std::marker::PhantomData;
use std::path::Path;

use clap_sys::plugin::clap_plugin;
use komponist_ipc::device_assets::{
    Bindings, EXTENSION_ID, Extension, MAX_BINDINGS_BYTES, MAX_SNAPSHOT_BYTES, OfflineRequest,
    OfflineStatus, Snapshot,
};
use nice_plug::prelude::ClapPlugin;
use nice_plug::wrapper::clap::Wrapper;

/// A control-thread view of the plugin's asset state.
pub trait AssetProvider: Send + Sync + 'static {
    /// Whether this plugin advertises file slots with editable bindings.
    const FILE_SLOTS: bool;
    /// Whether this plugin supports explicit preparation before an offline render.
    const OFFLINE_PREPARATION: bool;

    /// Returns the complete file-slot inventory.
    fn asset_slots(&self) -> Option<Snapshot> {
        None
    }

    /// Updates the named bindings atomically. File loading may continue on a worker.
    fn set_asset_bindings(&self, _bindings: Bindings) -> bool {
        false
    }

    /// Starts or polls an offline request, or clears it after a render.
    fn prepare_offline(&self, _request: Option<OfflineRequest>) -> OfflineStatus {
        OfflineStatus::Failed
    }
}

/// Returns the table for `id` from a plugin's custom CLAP extension hook.
///
/// The same plugin must publish an `Arc<T>` from its custom extension data hook.
pub fn extension<P: ClapPlugin, T: AssetProvider>(id: &CStr) -> *const c_void {
    if id == EXTENSION_ID && (T::FILE_SLOTS || T::OFFLINE_PREPARATION) {
        std::ptr::from_ref(&Callbacks::<P, T>::TABLE).cast()
    } else {
        std::ptr::null()
    }
}

struct Callbacks<P, T>(PhantomData<(P, T)>);

impl<P: ClapPlugin, T: AssetProvider> Callbacks<P, T> {
    const TABLE: Extension = Extension {
        snapshot: if T::FILE_SLOTS {
            Some(Self::snapshot)
        } else {
            None
        },
        set_bindings: if T::FILE_SLOTS {
            Some(Self::set_bindings)
        } else {
            None
        },
        prepare_offline: if T::OFFLINE_PREPARATION {
            Some(Self::prepare_offline)
        } else {
            None
        },
    };

    unsafe extern "C" fn snapshot(plugin: *const c_void, output: *mut u8, capacity: u32) -> u32 {
        if output.is_null() || capacity == 0 {
            return 0;
        }
        // SAFETY: The CLAP caller supplies a buffer writable for the advertised capacity.
        let output = unsafe {
            std::slice::from_raw_parts_mut(output, (capacity as usize).min(MAX_SNAPSHOT_BYTES))
        };
        // SAFETY: The live plugin pointer came from the query for this extension.
        unsafe {
            Wrapper::<P>::with_custom_extension_data::<T, _>(plugin.cast::<clap_plugin>(), |data| {
                data.asset_slots()?.encode(output)
            })
        }
        .flatten()
        .map_or(0, |size| size as u32)
    }

    unsafe extern "C" fn set_bindings(plugin: *const c_void, input: *const u8, length: u32) -> u32 {
        let length = length as usize;
        if input.is_null() || length == 0 || length > MAX_BINDINGS_BYTES {
            return 0;
        }
        // SAFETY: The CLAP caller supplies a buffer readable for the advertised length.
        let bytes = unsafe { std::slice::from_raw_parts(input, length) };
        let Some(bindings) = Bindings::decode(bytes) else {
            return 0;
        };
        if bindings.slots.iter().any(|slot| {
            slot.path
                .as_deref()
                .is_some_and(|path| !Path::new(path).is_absolute())
        }) {
            return 0;
        }
        // SAFETY: The live plugin pointer came from the query for this extension.
        unsafe {
            Wrapper::<P>::with_custom_extension_data::<T, _>(plugin.cast::<clap_plugin>(), |data| {
                data.set_asset_bindings(bindings)
            })
        }
        .map_or(0, u32::from)
    }

    unsafe extern "C" fn prepare_offline(
        plugin: *const c_void,
        id: u32,
        sample_rate: f64,
        tempo: f64,
    ) -> u32 {
        let request = OfflineRequest {
            id,
            sample_rate,
            tempo,
        };
        let work = if (id, sample_rate, tempo) == (0, 0.0, 0.0) {
            None
        } else if request.valid() {
            Some(request)
        } else {
            return OfflineStatus::Failed as u32;
        };
        // SAFETY: The live plugin pointer came from the query for this extension.
        unsafe {
            Wrapper::<P>::with_custom_extension_data::<T, _>(plugin.cast::<clap_plugin>(), |data| {
                data.prepare_offline(work)
            })
        }
        .unwrap_or(OfflineStatus::Failed) as u32
    }
}
