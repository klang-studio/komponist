//! Main-thread reader for the C ABI in docs/clap-device-layout/clap-device-layout.h.

use std::ffi::CStr;

use clap_sys::plugin::clap_plugin;
use komponist_ipc::native_layout::{LayoutError, MAX_BYTES, NativeLayout};

const OK: u32 = 0;
const ABSENT: u32 = 1;
const BUFFER_TOO_SMALL: u32 = 2;

#[repr(C)]
struct Extension {
    get_layout: Option<unsafe extern "C" fn(*const clap_plugin, *mut u8, u32, *mut u32) -> u32>,
}

pub(super) fn read(
    plugin: &clap_plugin,
    id: &CStr,
    version: u8,
) -> Result<Option<(Vec<u8>, NativeLayout)>, LayoutError> {
    let Some(extension) = plugin.get_extension else {
        return Ok(None);
    };
    // SAFETY: this is the live initialized instance, queried on its CLAP main thread.
    let table = unsafe { extension(plugin, id.as_ptr()).cast::<Extension>().as_ref() };
    let Some(table) = table else {
        return Ok(None);
    };
    let get = table
        .get_layout
        .ok_or(LayoutError::Invalid("missing callback"))?;
    let mut size = 0;
    // SAFETY: the length output is writable and the null/zero pair requests only the size.
    let status = unsafe { get(plugin, std::ptr::null_mut(), 0, &mut size) };
    if status == ABSENT && size == 0 {
        return Ok(None);
    }
    if status != BUFFER_TOO_SMALL || !(1..=MAX_BYTES).contains(&size) {
        return Err(LayoutError::Invalid("size query"));
    }
    let mut bytes = vec![0; size as usize];
    let mut written = 0;
    // SAFETY: the destination has exactly size writable bytes and the output length is separate.
    if unsafe { get(plugin, bytes.as_mut_ptr(), size, &mut written) } != OK || written != size {
        return Err(LayoutError::Invalid("layout copy"));
    }
    let layout = NativeLayout::parse(&bytes)?;
    if layout.schema_version != version {
        return Err(LayoutError::Unsupported);
    }
    Ok(Some((bytes, layout)))
}
