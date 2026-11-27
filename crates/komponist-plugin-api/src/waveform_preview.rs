//! Main-thread audition of a position within a linked audio asset.

use std::ffi::{CStr, c_void};

pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.waveform-preview";

#[repr(C)]
pub struct Extension {
    /// Start at a nonnegative source time in seconds. A value of -1 releases the audition.
    pub preview: Option<unsafe extern "C" fn(*const c_void, u32, f64)>,
}

pub fn encode_time(time: Option<f64>) -> Option<f64> {
    match time {
        Some(time) if time.is_finite() && time >= 0.0 => Some(time),
        Some(_) => None,
        None => Some(-1.0),
    }
}
