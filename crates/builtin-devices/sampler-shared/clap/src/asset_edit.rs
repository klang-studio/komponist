//! Device-owned reset of dependent slice settings in detached, persisted state.

use std::ffi::c_void;

use komponist_device_core::pinned_clap_state::{
    MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD, decode, encode_document,
};
use komponist_ipc::device_assets::edit_state::{self, Edits, Intent, Prepared};
use komponist_sampler_shared::settings::Settings;
use serde_json::Value;

use crate::SampledDefinition;

pub(crate) fn extension<T: SampledDefinition>() -> *const c_void {
    let extension = &edit_state::Extension {
        prepare: Some(prepare::<T>),
    };
    std::ptr::from_ref(extension).cast()
}

unsafe extern "C" fn prepare<T: SampledDefinition>(
    _plugin: *const c_void,
    edits: *const u8,
    edits_len: u32,
    state: *const u8,
    state_len: u32,
    output: *mut c_void,
    write: edit_state::WriteState,
) -> u32 {
    if edits.is_null() || !(1..=edit_state::MAX_EDITS_BYTES).contains(&(edits_len as usize)) {
        return edit_state::REJECTED;
    }
    // SAFETY: The host retains a readable edit buffer of the given length for this call.
    let Some(edits) =
        Edits::decode(unsafe { std::slice::from_raw_parts(edits, edits_len as usize) })
    else {
        return edit_state::REJECTED;
    };
    let state = match (state.is_null(), state_len as usize) {
        (true, 0) => None,
        // SAFETY: The host retains a readable state buffer of the given length for this call.
        (false, 1..=edit_state::MAX_STATE_BYTES) => {
            Some(unsafe { std::slice::from_raw_parts(state, state_len as usize) })
        }
        _ => return edit_state::REJECTED,
    };
    match reset::<T>(&edits, state) {
        Ok(None) => edit_state::UNCHANGED,
        Ok(Some(state)) if edit_state::valid_state(&state) => {
            // SAFETY: The host supplied this output sink. The bytes remain live until it returns.
            if unsafe { write(output, state.as_ptr(), state.len() as u32) } {
                edit_state::REPLACED
            } else {
                edit_state::REJECTED
            }
        }
        _ => edit_state::REJECTED,
    }
}

fn reset<T: SampledDefinition>(edits: &Edits, state: Option<&[u8]>) -> Prepared {
    let codec = T::SLICE_SETTINGS.as_ref().ok_or(edit_state::Rejected)?;
    if edits.slots.iter().any(|edit| edit.slot as usize >= T::PADS) {
        return Err(edit_state::Rejected);
    }
    let Some(state) =
        state.filter(|_| edits.slots.iter().any(|edit| edit.intent != Intent::Relink))
    else {
        return Ok(None);
    };
    let mut document = decode(state).ok_or(edit_state::Rejected)?;
    let fields = document
        .get_mut("fields")
        .and_then(Value::as_object_mut)
        .ok_or(edit_state::Rejected)?;
    let Some(assets) = fields.get_mut(SAMPLE_ASSETS_FIELD) else {
        return Ok(None);
    };
    let before = assets
        .as_str()
        .filter(|bytes| bytes.len() <= MAX_SAMPLE_ASSET_BYTES)
        .ok_or(edit_state::Rejected)?;
    if !serde_json::from_str::<Value>(before).is_ok_and(|asset| asset.is_object()) {
        return Err(edit_state::Rejected);
    }
    let updated = (codec.write)(before.as_bytes(), &Settings::default());
    if updated == before.as_bytes() {
        return Ok(None);
    }
    if updated.len() > MAX_SAMPLE_ASSET_BYTES {
        return Err(edit_state::Rejected);
    }
    *assets = Value::String(String::from_utf8(updated).map_err(|_| edit_state::Rejected)?);
    Ok(Some(encode_document(&document)))
}
