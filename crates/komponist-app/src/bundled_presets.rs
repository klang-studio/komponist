//! Factory preset states for authenticated devices using the pinned CLAP wrappers.

use komponist_builtin::Internal;
use komponist_device_core::pinned_clap_state::{self, FACTORY_PRESET_FIELD};
use komponist_model::Device;
use serde_json::{Map, Value};

/// Load a factory preset while retaining unrelated fields such as sampler assets.
pub(crate) fn state(kind: Internal, device: &Device, index: usize) -> Option<Vec<u8>> {
    let values = kind.preset_values(index)?;
    let mut fields = match &device.state {
        Some(state) => pinned_clap_state::decode(state)?
            .get_mut("fields")?
            .as_object_mut()
            .map(std::mem::take)?,
        None => Map::new(),
    };
    let name = kind.presets()[index].name;
    fields.insert(
        FACTORY_PRESET_FIELD.into(),
        Value::String(serde_json::to_string(name).ok()?),
    );
    let params = kind
        .params()
        .iter()
        .zip(values)
        .map(|(info, value)| {
            (
                info.id.as_str().to_owned(),
                pinned_clap_state::parameter(info, value).0,
            )
        })
        .collect();
    Some(pinned_clap_state::encode(
        &device.plugin.version,
        params,
        fields,
    ))
}

/// The index of the recognized factory preset saved by the wrapper.
pub(crate) fn index(kind: Internal, state: &[u8]) -> Option<usize> {
    let state = pinned_clap_state::decode(state)?;
    let raw = state.get("fields")?.get(FACTORY_PRESET_FIELD)?.as_str()?;
    let name: String = serde_json::from_str(raw).ok()?;
    kind.presets().iter().position(|preset| preset.name == name)
}
