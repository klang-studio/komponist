use clap_sys::plugin::clap_plugin;
use komponist_plugin_api::device_canvas::{DisplayList, EXTENSION_ID, MAX_BYTES};

#[repr(C)]
struct Extension {
    draw: Option<unsafe extern "C" fn(*const clap_plugin, *mut u8, u32) -> u32>,
}

pub(super) fn draw(plugin: &clap_plugin) -> Option<DisplayList> {
    // SAFETY: The initialized plugin owns this immutable extension table.
    let table = unsafe {
        plugin.get_extension?(plugin, EXTENSION_ID.as_ptr())
            .cast::<Extension>()
            .as_ref()
    }?;
    let draw = table.draw?;
    let mut bytes = vec![0; MAX_BYTES];
    // SAFETY: The buffer has the advertised capacity, is writable, and is dropped after the call.
    let written = unsafe { draw(plugin, bytes.as_mut_ptr(), MAX_BYTES as u32) };
    DisplayList::decode(bytes.get(..written as usize)?)
}
