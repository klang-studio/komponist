use clap_sys::plugin::clap_plugin;
use std::ffi::CStr;

pub const PLUGIN_ID: &str = "org.klang-studio.komponist.nice-layout-prototype";
pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.device-layout/1";
pub const OK: u32 = 0;
pub const ABSENT: u32 = 1;
pub const BUFFER_TOO_SMALL: u32 = 2;
pub const ERROR: u32 = 3;
pub const MAX_LAYOUT_BYTES: u32 = 65_536;

#[repr(C)]
pub struct LayoutExtension {
    pub get_layout: unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, *mut u32) -> u32,
}
