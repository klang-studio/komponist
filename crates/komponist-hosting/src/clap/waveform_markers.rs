//! Access to the optional main-thread CLAP waveform marker callbacks.

use clap_sys::plugin::clap_plugin;
use komponist_plugin_api::waveform_markers::{
    EXTENSION_ID, EditStatus, Extension, MAX_EDIT_BYTES, MAX_SNAPSHOT_BYTES, Request, Snapshot,
};

fn extension(plugin: &clap_plugin) -> Option<&Extension> {
    // SAFETY: The initialized plugin is queried on its main thread. The plugin owns the
    // immutable extension table until the instance is destroyed.
    let table = unsafe {
        plugin.get_extension?(plugin, EXTENSION_ID.as_ptr())
            .cast::<Extension>()
            .as_ref()
    }?;
    (table.snapshot.is_some() && table.edit.is_some()).then_some(table)
}

pub(super) fn snapshot(plugin: &clap_plugin, slot: u32) -> Option<Snapshot> {
    let read = extension(plugin)?.snapshot?;
    let mut bytes = [0; MAX_SNAPSHOT_BYTES];
    // SAFETY: This is the live originating plugin and the buffer is writable for the stated
    // capacity until the synchronous callback returns.
    let written = unsafe {
        read(
            std::ptr::from_ref(plugin).cast(),
            slot,
            bytes.as_mut_ptr(),
            bytes.len() as u32,
        )
    };
    Snapshot::decode(bytes.get(..written as usize)?)
}

pub(super) fn edit(plugin: &clap_plugin, slot: u32, request: Request) -> Option<EditStatus> {
    let edit = extension(plugin)?.edit?;
    let mut bytes = [0; MAX_EDIT_BYTES];
    let len = request.encode(&mut bytes)?;
    // SAFETY: This is the live originating plugin and the buffer contains one complete request
    // of the stated length until the synchronous callback returns.
    let status = unsafe {
        edit(
            std::ptr::from_ref(plugin).cast(),
            slot,
            bytes.as_ptr(),
            len as u32,
        )
    };
    EditStatus::from_raw(status)
}

#[cfg(test)]
mod tests {
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr;

    use komponist_plugin_api::waveform_markers::Edit;

    use super::*;

    const REQUEST: Request = Request {
        revision: 42,
        operation: Edit::Remove { index: 1 },
    };

    fn expected_snapshot() -> Snapshot {
        Snapshot {
            revision: REQUEST.revision,
            editable: true,
            positions: vec![0.25, 0.5],
            slice_options: None,
            default_options: None,
            preparation: None,
        }
    }

    unsafe extern "C" fn read(
        _plugin: *const c_void,
        slot: u32,
        destination: *mut u8,
        capacity: u32,
    ) -> u32 {
        match slot {
            0 | 1 => {
                // SAFETY: The host supplied a writable buffer that lives through this call.
                let bytes =
                    unsafe { std::slice::from_raw_parts_mut(destination, capacity as usize) };
                expected_snapshot()
                    .encode(bytes)
                    .map_or(0, |written| written as u32 + slot)
            }
            2 => capacity + 1,
            _ => 0,
        }
    }

    unsafe extern "C" fn write(
        _plugin: *const c_void,
        slot: u32,
        source: *const u8,
        length: u32,
    ) -> u32 {
        // SAFETY: The host supplied a readable buffer that lives through this call.
        let bytes = unsafe { std::slice::from_raw_parts(source, length as usize) };
        if Request::decode(bytes) != Some(REQUEST) {
            return EditStatus::Invalid as u32;
        }
        slot
    }

    static COMPLETE: Extension = Extension {
        snapshot: Some(read),
        edit: Some(write),
    };
    static SNAPSHOT_ONLY: Extension = Extension {
        snapshot: Some(read),
        edit: None,
    };
    static EDIT_ONLY: Extension = Extension {
        snapshot: None,
        edit: Some(write),
    };

    unsafe extern "C" fn get_extension(
        plugin: *const clap_plugin,
        id: *const c_char,
    ) -> *const c_void {
        // SAFETY: The host passes the live fixture and a static C string.
        unsafe {
            if CStr::from_ptr(id) == EXTENSION_ID {
                (*plugin).plugin_data.cast_const()
            } else {
                ptr::null()
            }
        }
    }

    fn fixture(table: Option<&'static Extension>) -> clap_plugin {
        clap_plugin {
            desc: ptr::null(),
            plugin_data: table.map_or(ptr::null_mut(), |table| {
                ptr::from_ref(table).cast_mut().cast()
            }),
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
        }
    }

    #[test]
    fn requires_both_callbacks() {
        for table in [None, Some(&SNAPSHOT_ONLY), Some(&EDIT_ONLY)] {
            let plugin = fixture(table);
            assert_eq!(snapshot(&plugin, 0), None);
            assert_eq!(edit(&plugin, 0, REQUEST), None);
        }
    }

    #[test]
    fn validates_snapshot_bytes_and_edit_status() {
        let plugin = fixture(Some(&COMPLETE));
        assert_eq!(snapshot(&plugin, 0), Some(expected_snapshot()));
        for slot in 1..=3 {
            assert_eq!(snapshot(&plugin, slot), None);
        }
        for status in [
            EditStatus::Invalid,
            EditStatus::Applied,
            EditStatus::Unchanged,
            EditStatus::Stale,
            EditStatus::Unavailable,
        ] {
            assert_eq!(edit(&plugin, status as u32, REQUEST), Some(status));
        }
        assert_eq!(edit(&plugin, u32::MAX, REQUEST), None);
        let invalid = Request {
            operation: Edit::Insert { seconds: f64::NAN },
            ..REQUEST
        };
        assert_eq!(edit(&plugin, 1, invalid), None);
    }
}
