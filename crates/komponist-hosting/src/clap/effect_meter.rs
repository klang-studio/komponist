//! Access to the optional bounded CLAP effect-meter extension.

use clap_sys::plugin::clap_plugin;
use componist_ipc::effect_meter::{BlockMeter, EXTENSION_ID, Extension};

#[derive(Clone, Copy)]
pub(super) struct EffectMeter {
    begin: unsafe extern "C" fn(*const core::ffi::c_void, u32),
    read: unsafe extern "C" fn(*const core::ffi::c_void, *mut BlockMeter) -> u32,
}

impl EffectMeter {
    pub(super) fn query(plugin: &clap_plugin) -> Option<Self> {
        let get = plugin.get_extension?;
        // SAFETY: The initialized instance is live and the extension table is immutable.
        let table = unsafe {
            get(plugin, EXTENSION_ID.as_ptr())
                .cast::<Extension>()
                .as_ref()
        }?;
        Some(Self {
            begin: table.begin?,
            read: table.read?,
        })
    }

    /// The caller supplies the active audio-thread handle belonging to this instance.
    pub(super) fn begin(self, plugin: &clap_plugin, detector: bool) {
        // SAFETY: The caller uses the originating live instance on its audio thread.
        unsafe { (self.begin)(core::ptr::from_ref(plugin).cast(), u32::from(detector)) };
    }

    /// The caller supplies the active audio-thread handle belonging to this instance.
    pub(super) fn end(self, plugin: &clap_plugin) -> Option<BlockMeter> {
        let mut readings = BlockMeter::EMPTY;
        // SAFETY: The caller uses the originating live instance on its audio thread and the
        // output has capacity for exactly one block of readings.
        let available = unsafe { (self.read)(core::ptr::from_ref(plugin).cast(), &mut readings) };
        (available == 1 && readings.validated()).then_some(readings)
    }
}
