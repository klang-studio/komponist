//! Device-owned trim and slice resets in detached, persisted state.

use std::ffi::c_void;

use clap_sys::plugin::clap_plugin;
use komponist_device_core::pinned_clap_state::{
    MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD, decode, encode, encode_document, parameter,
};
use komponist_ipc::device_assets::edit_state::{self, Edits, Intent, Prepared};
use komponist_sampler_shared::settings::Settings;
use nice_plug::wrapper::clap::Wrapper;
use serde_json::{Map, Value};

use crate::{Sampled, SampledDefinition};

pub(crate) fn extension<T: SampledDefinition>() -> *const c_void {
    let extension = &edit_state::Extension {
        prepare: Some(prepare::<T>),
    };
    std::ptr::from_ref(extension).cast()
}

pub(crate) fn extension_v2<T: SampledDefinition>() -> *const c_void {
    let extension = &edit_state::ExtensionV2 {
        prepare: Some(prepare_v2::<T>),
    };
    std::ptr::from_ref(extension).cast()
}

unsafe extern "C" fn prepare_v2<T: SampledDefinition>(
    plugin: *const c_void,
    edits: *const u8,
    edits_len: u32,
    state: *const u8,
    state_len: u32,
    output: *mut c_void,
    write: edit_state::WriteState,
    reset_bases: edit_state::ResetParameterBases,
) -> u32 {
    if plugin.is_null() {
        return edit_state::REJECTED;
    }
    // SAFETY: This callback is registered on the originating initialized CLAP instance. All
    // caller-supplied input buffers remain readable until the callback returns.
    let Some((edits, state)) = (unsafe { read_request(edits, edits_len, state, state_len) }) else {
        return edit_state::REJECTED;
    };
    let lookup = |name: &str| {
        // SAFETY: The wrapper registered this callback for the matching sampler type and owns
        // the immutable parameter ID map for the lifetime of this live instance.
        unsafe {
            Wrapper::<Sampled<T>>::custom_extension_param_id(plugin.cast::<clap_plugin>(), name)
        }
    };
    let Ok(change) = reset_state_and_bases::<T>(&edits, state, lookup) else {
        return edit_state::REJECTED;
    };
    if let Some(state) = &change.state {
        let bytes = state.as_bytes();
        // SAFETY: The output sink and the bounded temporary state are live for the call.
        if !unsafe { write(output, bytes.as_ptr(), bytes.len() as u32) } {
            return edit_state::REJECTED;
        }
    }
    let ids = &change.reset_parameter_bases;
    if !ids.is_empty() {
        // SAFETY: The output sink and the bounded array are live for the call.
        if !unsafe { reset_bases(output, ids.as_ptr(), ids.len() as u32) } {
            return edit_state::REJECTED;
        }
    }
    if change.state.is_some() || !ids.is_empty() {
        edit_state::REPLACED
    } else {
        edit_state::UNCHANGED
    }
}

fn reset_state_and_bases<T: SampledDefinition>(
    edits: &Edits,
    state: Option<&[u8]>,
    mut id: impl FnMut(&str) -> Option<u32>,
) -> edit_state::PreparedChange {
    if edits.slots.iter().any(|edit| edit.slot as usize >= T::PADS) {
        return Err(edit_state::Rejected);
    }
    let slices = if T::SLICE_SETTINGS.is_some() {
        reset::<T>(edits, state)?
    } else {
        None
    };
    let mut trims = edits
        .slots
        .iter()
        .filter_map(|edit| T::trim_for_asset_edit(edit.slot as usize, edit.intent))
        .flatten()
        .peekable();
    if trims.peek().is_none() {
        return Ok(edit_state::Change {
            state: slices.map(edit_state::State::try_from).transpose()?,
            reset_parameter_bases: Vec::new(),
        });
    }
    let initial;
    let bytes = if let Some(bytes) = slices.as_deref().or(state) {
        bytes
    } else {
        initial = encode(T::VERSION, Map::new(), Map::new());
        &initial
    };
    let mut document = decode(bytes).ok_or(edit_state::Rejected)?;
    let params = document
        .get_mut("params")
        .and_then(Value::as_object_mut)
        .ok_or(edit_state::Rejected)?;
    let mut ids = Vec::new();
    for wire in trims {
        let info = T::SPEC.params().get(wire).ok_or(edit_state::Rejected)?;
        let name = info.id.as_str();
        ids.push(id(name).ok_or(edit_state::Rejected)?);
        params.insert(name.into(), parameter(info, info.default).0);
    }
    let replacement = encode_document(&document);
    if !edit_state::valid_base_resets(&ids) {
        return Err(edit_state::Rejected);
    }
    Ok(edit_state::Change {
        state: (state != Some(replacement.as_slice()))
            .then_some(replacement)
            .map(edit_state::State::try_from)
            .transpose()?,
        reset_parameter_bases: ids,
    })
}

/// # Safety
///
/// Non-null buffers must be readable for their supplied lengths through the returned borrow.
unsafe fn read_request<'a>(
    edits: *const u8,
    edits_len: u32,
    state: *const u8,
    state_len: u32,
) -> Option<(Edits, Option<&'a [u8]>)> {
    if edits.is_null() || !(1..=edit_state::MAX_EDITS_BYTES).contains(&(edits_len as usize)) {
        return None;
    }
    // SAFETY: The caller retains the input for the size checked above.
    let edits = Edits::decode(unsafe { std::slice::from_raw_parts(edits, edits_len as usize) })?;
    let state = match (state.is_null(), state_len as usize) {
        (true, 0) => None,
        // SAFETY: The caller retains the input for the size checked above.
        (false, 1..=edit_state::MAX_STATE_BYTES) => {
            Some(unsafe { std::slice::from_raw_parts(state, state_len as usize) })
        }
        _ => return None,
    };
    Some((edits, state))
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
    // SAFETY: The host retains the two read buffers for this synchronous callback.
    let Some((edits, state)) = (unsafe { read_request(edits, edits_len, state, state_len) }) else {
        return edit_state::REJECTED;
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
