//! The validated inventory and audio-thread callbacks for CLAP signal taps.

use core::ffi::c_void;

use clap_sys::plugin::clap_plugin;
use komponist_plugin_api::signal_taps::{
    BlockTaps, Descriptor, EXTENSION_ID, Extension, MAX_TAPS, valid_inventory,
};

#[derive(Clone, Copy)]
pub(super) struct SignalTaps {
    descriptors: [Descriptor; MAX_TAPS],
    count: usize,
    begin: unsafe extern "C" fn(*const c_void, u32),
    read: unsafe extern "C" fn(*const c_void, u32, *mut f32, u32) -> u32,
}

impl SignalTaps {
    pub(super) fn query(plugin: &clap_plugin) -> Option<Self> {
        let get = plugin.get_extension?;
        // SAFETY: This is the live initialized plugin on its main thread.
        let table = unsafe {
            get(plugin, EXTENSION_ID.as_ptr())
                .cast::<Extension>()
                .as_ref()
        }?;
        let plugin = core::ptr::from_ref(plugin).cast();
        let mut descriptors = [Descriptor::EMPTY; MAX_TAPS];
        let count = table.count?;
        let info = table.info?;
        // SAFETY: The table belongs to the originating plugin and has a main-thread inventory.
        let count = unsafe { count(plugin) } as usize;
        let candidates = descriptors.get_mut(..count)?;
        for (index, descriptor) in candidates.iter_mut().enumerate() {
            // SAFETY: index is in the advertised inventory and output points to one descriptor.
            if unsafe { info(plugin, index as u32, descriptor) } != 1 {
                return None;
            }
        }
        if !valid_inventory(candidates) {
            return None;
        }
        Some(Self {
            descriptors,
            count,
            begin: table.begin?,
            read: table.read?,
        })
    }

    pub(super) fn inventory(&self) -> &[Descriptor] {
        &self.descriptors[..self.count]
    }

    pub(super) fn begin(&self, plugin: &clap_plugin, mask: u32) {
        let mask = mask & ((1 << self.count) - 1);
        if mask != 0 {
            // SAFETY: The caller supplies the live originating instance on its audio thread.
            unsafe { (self.begin)(core::ptr::from_ref(plugin).cast(), mask) };
        }
    }

    pub(super) fn read(&self, plugin: &clap_plugin, mask: u32, out: &mut BlockTaps) {
        for index in 0..self.count {
            if mask & (1 << index) != 0 {
                out.fill(index, |samples| {
                    // SAFETY: The caller supplies the live originating instance on its audio
                    // thread. The output pointer has the advertised writable capacity.
                    unsafe {
                        (self.read)(
                            core::ptr::from_ref(plugin).cast(),
                            index as u32,
                            samples.as_mut_ptr(),
                            samples.len() as u32,
                        ) as usize
                    }
                });
            }
        }
    }
}
