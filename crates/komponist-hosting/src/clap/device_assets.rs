//! Access to the optional CLAP file slots and offline preparation.

use std::ffi::c_void;

use clap_sys::plugin::clap_plugin;
use komponist_plugin_api::device_assets::edit_state;
use komponist_plugin_api::device_assets::{
    Bindings, EXTENSION_ID, Extension, MAX_BINDINGS_BYTES, MAX_SNAPSHOT_BYTES, OfflineRequest,
    OfflineStatus, Snapshot,
};

fn extension(plugin: &clap_plugin) -> Option<&Extension> {
    // SAFETY: The initialized instance is queried on the CLAP main thread. The immutable extension
    // table, if present, is valid until that instance is destroyed.
    unsafe {
        plugin.get_extension?(plugin, EXTENSION_ID.as_ptr())
            .cast::<Extension>()
            .as_ref()
    }
}

pub(super) fn supports_slots(plugin: &clap_plugin) -> bool {
    extension(plugin).is_some_and(|table| table.snapshot.is_some() && table.set_bindings.is_some())
}

pub(super) fn snapshot(plugin: &clap_plugin) -> Option<Snapshot> {
    let read = extension(plugin)?.snapshot?;
    let mut bytes = vec![0; MAX_SNAPSHOT_BYTES];
    // SAFETY: This is the live originating plugin and the buffer is valid for the stated capacity
    // until the callback returns.
    let len = unsafe {
        read(
            std::ptr::from_ref(plugin).cast(),
            bytes.as_mut_ptr(),
            MAX_SNAPSHOT_BYTES as u32,
        )
    } as usize;
    Snapshot::decode(bytes.get(..len)?)
}

pub(super) fn set_bindings(plugin: &clap_plugin, bindings: &Bindings) -> bool {
    let Some(set) = extension(plugin).and_then(|table| table.set_bindings) else {
        return false;
    };
    let mut bytes = vec![0; MAX_BINDINGS_BYTES];
    let Some(len) = bindings.encode(&mut bytes) else {
        return false;
    };
    // SAFETY: This is the live originating plugin and the buffer contains the complete encoded
    // update. The callback may read only the stated length until it returns.
    unsafe {
        set(
            std::ptr::from_ref(plugin).cast(),
            bytes.as_ptr(),
            len as u32,
        ) == 1
    }
}

fn edit_state_callback(plugin: &clap_plugin) -> Option<&edit_state::Extension> {
    // SAFETY: The initialized instance is queried on the CLAP main thread. The extension
    // table remains valid until the instance is destroyed.
    unsafe {
        plugin.get_extension?(plugin, edit_state::EXTENSION_ID.as_ptr())
            .cast::<edit_state::Extension>()
            .as_ref()
    }
}

pub(super) fn supports_edit_state(plugin: &clap_plugin) -> bool {
    edit_state_callback(plugin).is_some_and(|table| table.prepare.is_some())
}

#[derive(Default)]
struct StateOutput {
    failed: bool,
    change: edit_state::Change,
}

unsafe extern "C" fn write_state(output: *mut c_void, bytes: *const u8, len: u32) -> bool {
    // SAFETY: The host supplies a live and unique StateOutput for the synchronous callback.
    let output = unsafe { &mut *output.cast::<StateOutput>() };
    if output.change.state.is_some()
        || bytes.is_null()
        || !(1..=edit_state::MAX_STATE_BYTES).contains(&(len as usize))
    {
        output.failed = true;
        return false;
    }
    // SAFETY: The callback's ABI requires a readable source of this length until the write returns.
    let bytes = unsafe { std::slice::from_raw_parts(bytes, len as usize) };
    output.change.state = edit_state::State::try_from(bytes.to_vec()).ok();
    output.change.state.is_some()
}

unsafe extern "C" fn reset_parameter_bases(output: *mut c_void, ids: *const u32, len: u32) -> bool {
    // SAFETY: The host supplies a live and unique StateOutput for the synchronous callback.
    let output = unsafe { &mut *output.cast::<StateOutput>() };
    if !output.change.reset_parameter_bases.is_empty()
        || ids.is_null()
        || !(1..=edit_state::MAX_PARAMETER_BASE_RESETS).contains(&(len as usize))
    {
        output.failed = true;
        return false;
    }
    // SAFETY: The callback's ABI requires this aligned array to remain readable until return.
    let ids = unsafe { std::slice::from_raw_parts(ids, len as usize) };
    if !edit_state::valid_base_resets(ids) {
        output.failed = true;
        return false;
    }
    output.change.reset_parameter_bases.extend_from_slice(ids);
    true
}

pub(super) fn prepare_edit_state(
    plugin: &clap_plugin,
    edits: &edit_state::Edits,
    state: Option<&[u8]>,
) -> edit_state::PreparedChange {
    let prepare = edit_state_callback(plugin)
        .and_then(|table| table.prepare)
        .ok_or(edit_state::Rejected)?;
    let mut edit_bytes = [0; edit_state::MAX_EDITS_BYTES];
    let edit_len = edits.encode(&mut edit_bytes).ok_or(edit_state::Rejected)?;
    let (state_bytes, state_len) = match state {
        Some(bytes) if edit_state::valid_state(bytes) => (bytes.as_ptr(), bytes.len() as u32),
        Some(_) => return Err(edit_state::Rejected),
        None => (std::ptr::null(), 0),
    };
    let mut output = StateOutput::default();
    let context = std::ptr::from_mut(&mut output).cast();
    let instance = std::ptr::from_ref(plugin).cast();
    // SAFETY: All inputs and the output context remain valid until the main-thread call returns.
    // This pure operation cannot touch shared processing state.
    let status = unsafe {
        prepare(
            instance,
            edit_bytes.as_ptr(),
            edit_len as u32,
            state_bytes,
            state_len,
            context,
            write_state,
            reset_parameter_bases,
        )
    };
    let changed = output.change.state.is_some() || !output.change.reset_parameter_bases.is_empty();
    if !output.failed
        && (status == edit_state::REPLACED && changed
            || status == edit_state::UNCHANGED && !changed)
    {
        Ok(output.change)
    } else {
        Err(edit_state::Rejected)
    }
}

pub(super) fn supports_offline_preparation(plugin: &clap_plugin) -> bool {
    extension(plugin).is_some_and(|table| table.prepare_offline.is_some())
}

pub(super) fn prepare_offline(
    plugin: &clap_plugin,
    request: Option<OfflineRequest>,
) -> Option<OfflineStatus> {
    let prepare = extension(plugin)?.prepare_offline?;
    let (id, sample_rate, tempo) = match request {
        Some(request) if request.valid() => (request.id, request.sample_rate, request.tempo),
        Some(_) => return Some(OfflineStatus::Failed),
        None => (0, 0.0, 0.0),
    };
    // SAFETY: This is the live originating plugin, called from the CLAP main thread. The caller
    // keeps audio processing detached while starting, polling or clearing offline preparation.
    let status = unsafe { prepare(std::ptr::from_ref(plugin).cast(), id, sample_rate, tempo) };
    Some(OfflineStatus::from_raw(status))
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr;

    use super::*;

    unsafe extern "C" fn oversized_snapshot(
        _plugin: *const c_void,
        _output: *mut u8,
        capacity: u32,
    ) -> u32 {
        capacity + 1
    }

    unsafe extern "C" fn prepare(
        _plugin: *const c_void,
        request: u32,
        sample_rate: f64,
        tempo: f64,
    ) -> u32 {
        match (request, sample_rate, tempo) {
            (0, 0.0, 0.0) => OfflineStatus::Ready as u32,
            (42, 48_000.0, 120.0) => OfflineStatus::Preparing as u32,
            _ => u32::MAX,
        }
    }

    static EXT: Extension = Extension {
        snapshot: Some(oversized_snapshot),
        set_bindings: None,
        prepare_offline: Some(prepare),
    };

    unsafe extern "C" fn get_extension(
        _plugin: *const clap_plugin,
        id: *const c_char,
    ) -> *const c_void {
        // SAFETY: The only caller is the host query above, passing a static C string.
        if unsafe { CStr::from_ptr(id) } == EXTENSION_ID {
            ptr::from_ref(&EXT).cast()
        } else {
            ptr::null()
        }
    }

    #[test]
    fn rejects_malformed_snapshot_and_negotiates_offline_preparation() {
        let plugin = clap_plugin {
            desc: ptr::null(),
            plugin_data: ptr::null_mut(),
            init: None,
            destroy: None,
            activate: None,
            deactivate: None,
            start_processing: None,
            stop_processing: None,
            reset: None,
            process: None,
            get_extension: Some(get_extension),
            on_main_thread: None,
        };
        let request = OfflineRequest {
            id: 42,
            sample_rate: 48_000.0,
            tempo: 120.0,
        };
        assert_eq!(snapshot(&plugin), None);
        assert!(supports_offline_preparation(&plugin));
        assert_eq!(
            prepare_offline(&plugin, Some(request)),
            Some(OfflineStatus::Preparing)
        );
        assert_eq!(prepare_offline(&plugin, None), Some(OfflineStatus::Ready));
        assert_eq!(
            prepare_offline(&plugin, Some(OfflineRequest { id: 7, ..request })),
            Some(OfflineStatus::Failed),
        );
        assert_eq!(
            prepare_offline(&plugin, Some(OfflineRequest { id: 0, ..request })),
            Some(OfflineStatus::Failed),
        );
        assert_eq!(
            prepare_offline(
                &clap_plugin {
                    get_extension: None,
                    ..plugin
                },
                None
            ),
            None
        );
    }
}
