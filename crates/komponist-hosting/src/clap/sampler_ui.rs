use clap_sys::plugin::clap_plugin;
use komponist_ipc::sampler_ui::{EXTENSION_ID, MAX_BYTES, Snapshot};

#[repr(C)]
struct Extension {
    snapshot: Option<unsafe extern "C" fn(*const clap_plugin, *mut u8, u32) -> u32>,
    preview: Option<unsafe extern "C" fn(*const clap_plugin, f64)>,
}

fn extension(plugin: &clap_plugin) -> Option<&Extension> {
    // SAFETY: This live instance is queried on its CLAP main thread. The table, if present, is
    // immutable and valid until the instance is destroyed.
    unsafe { plugin.get_extension?(plugin, EXTENSION_ID.as_ptr()).cast::<Extension>().as_ref() }
}

pub(super) fn snapshot(plugin: &clap_plugin) -> Option<Snapshot> {
    let read = extension(plugin)?.snapshot?;
    let mut bytes = vec![0; MAX_BYTES];
    // SAFETY: The buffer has the advertised capacity and is writable until the call returns.
    let len = unsafe { read(plugin, bytes.as_mut_ptr(), MAX_BYTES as u32) } as usize;
    Snapshot::decode(bytes.get(..len)?)
}

pub(super) fn preview(plugin: &clap_plugin, time: Option<f64>) {
    if time.is_some_and(|time| !time.is_finite() || time < 0.0) {
        return;
    }
    if let Some(preview) = extension(plugin).and_then(|extension| extension.preview) {
        // SAFETY: The initialized instance is accessed on its main thread. Negative time releases
        // the preceding audition.
        unsafe { preview(plugin, time.unwrap_or(-1.0)) };
    }
}
