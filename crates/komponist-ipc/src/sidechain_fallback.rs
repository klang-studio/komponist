//! Shared representation of the public CLAP sidechain fallback extension.

use std::ffi::CStr;

/// ID passed to the initialized plugin's CLAP `get_extension`.
pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.sidechain-fallback/1";
/// Send the stereo main signal to the sidechain when no source is connected.
pub const MAIN_INPUT: u32 = 1;
/// Use the current audio ports configuration without selecting a different one.
pub const CURRENT_CONFIGURATION: u32 = u32::MAX;

/// Read-only table owned by the plugin and valid until destruction.
#[repr(C)]
pub struct SidechainFallback {
    pub mode: u32,
    pub auxiliary_config_id: u32,
}
