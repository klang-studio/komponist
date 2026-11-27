//! Publishes the existing atomic playheads and pad hits as named streams.

use std::ffi::c_void;

use clap_sys::plugin::clap_plugin;
use komponist_plugin_api::signal_taps::{
    Descriptor, Extension, MAX_PAD_ACTIVITY_FRAME, PAD_ACTIVITY, PLAYHEAD_POSITION, PadActivity,
};
use nice_plug::wrapper::clap::Wrapper;

use super::{SampleParameters, Sampled, SampledDefinition};

const PLAYHEAD: Descriptor = Descriptor::new("playhead", PLAYHEAD_POSITION, 1, 8);
const PADS: Descriptor = Descriptor::new("pads", PAD_ACTIVITY, 1, 64);

pub(super) fn extension<T: SampledDefinition>() -> *const c_void {
    let table: &'static Extension = &Extension {
        count: Some(count),
        info: Some(info::<T>),
        begin: Some(begin),
        read: Some(read::<T>),
    };
    std::ptr::from_ref(table).cast()
}

unsafe extern "C" fn count(_: *const c_void) -> u32 {
    1
}

unsafe extern "C" fn info<T: SampledDefinition>(
    _: *const c_void,
    index: u32,
    out: *mut Descriptor,
) -> u32 {
    if index != 0 || out.is_null() {
        return 0;
    }
    // SAFETY: The caller supplies one writable descriptor.
    unsafe { out.write(if T::PADS == 1 { PLAYHEAD } else { PADS }) };
    1
}

unsafe extern "C" fn begin(_: *const c_void, _: u32) {}

unsafe extern "C" fn read<T: SampledDefinition>(
    plugin: *const c_void,
    index: u32,
    out: *mut f32,
    capacity: u32,
) -> u32 {
    let size = if T::PADS == 1 {
        1
    } else {
        MAX_PAD_ACTIVITY_FRAME
    };
    if plugin.is_null() || index != 0 || out.is_null() || (capacity as usize) < size {
        return 0;
    }
    // SAFETY: The table is registered by this wrapper for its live CLAP instance. Its playhead
    // accessor only reads an atomic, and the host supplies an output buffer for this callback.
    unsafe {
        Wrapper::<Sampled<T>>::with_custom_extension_data::<SampleParameters<T>, _>(
            plugin.cast::<clap_plugin>(),
            |params| {
                if T::PADS == 1 {
                    out.write(params.samples.playhead(0).unwrap_or(-1.0));
                } else {
                    let (hit, pad) = params.samples.last_hit();
                    PadActivity::write(
                        std::slice::from_raw_parts_mut(out, size),
                        hit,
                        pad,
                        (0..T::PADS).map(|pad| params.samples.playhead(pad)),
                    );
                }
            },
        )
    }
    .map_or(0, |()| size as u32)
}
