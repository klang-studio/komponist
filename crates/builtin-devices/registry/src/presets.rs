//! Preset compatibility for historical projects with built-in device references.

use crate::{BuiltinKind, Internal};
use komponist_device_core::pinned_clap_state::{self, FACTORY_PRESET_FIELD};
use komponist_ipc::clap_presets::{ClapPreset, ClapPresetLocation, ClapPresetSource};
use komponist_model::{Device, PluginRef};

impl BuiltinKind {
    /// The pre-CLAP browser only listed presets from the original instrument bank.
    /// Packaged and external devices supply their complete inventories through CLAP.
    pub fn browsable_legacy_presets(self) -> bool {
        self == Self::Hexa
    }
}

/// Factory metadata for a historical in-process device.
pub fn legacy_inventory(kind: BuiltinKind) -> Vec<ClapPreset> {
    let plugin = kind.catalog().plugin_ref();
    let entry = |index: usize, name: &str, category: Option<&str>| ClapPreset {
        plugin_id: plugin.id.clone(),
        source: legacy_source(index),
        name: name.to_owned(),
        localized_names: Default::default(),
        features: category.into_iter().map(str::to_owned).collect(),
        factory: true,
    };
    match kind {
        BuiltinKind::Hexa => komponist_hexa::presets()
            .map(|preset| entry(preset.index, preset.name, Some(preset.category)))
            .collect(),
        BuiltinKind::Internal(device) => device
            .presets()
            .iter()
            .enumerate()
            .map(|(index, preset)| entry(index, preset.name, preset.category))
            .collect(),
    }
}

/// Categories in provider order. A preset without a category stays in the main list.
pub fn categories(presets: &[ClapPreset]) -> Vec<&str> {
    let mut categories = Vec::new();
    for category in presets.iter().filter_map(|preset| preset.category()) {
        if !categories.contains(&category) {
            categories.push(category);
        }
    }
    categories
}

/// A stable selector for an entry of an old in-process device.
pub fn legacy_source(index: usize) -> ClapPresetSource {
    ClapPresetSource {
        location: ClapPresetLocation::Plugin,
        load_key: Some(index.to_string().into_bytes()),
    }
}

/// Applies a preset of a historical in-process device in its original state format.
pub fn legacy_state(
    plugin: &PluginRef,
    state: Option<&[u8]>,
    source: &ClapPresetSource,
) -> Option<Vec<u8>> {
    if source.location != ClapPresetLocation::Plugin {
        return None;
    }
    let index = std::str::from_utf8(source.load_key.as_deref()?)
        .ok()?
        .parse::<usize>()
        .ok()?;
    match BuiltinKind::from_plugin(plugin)? {
        BuiltinKind::Hexa => {
            komponist_hexa::preset_patch(index).map(|patch| komponist_hexa::encode_state(&patch))
        }
        BuiltinKind::Internal(kind) => kind.preset_state(state, index),
    }
}

/// The preset represented in a previously saved device state and whether it was edited.
/// `bundled_id` is the CLAP ID of a running instance from the verified package.
pub fn saved_preset(
    device: &Device,
    bundled_id: Option<&str>,
    presets: &[ClapPreset],
    param_base: impl Fn(&str) -> Option<f64>,
) -> Option<(usize, bool)> {
    let legacy = BuiltinKind::from_plugin(&device.plugin);
    let bundled_kind = bundled_id.and_then(|id| {
        BuiltinKind::all()
            .find(|kind| format!("org.klang-studio.komponist.{}", kind.catalog().id) == id)
    });
    let (source, changed) = match (legacy, bundled_kind) {
        (Some(BuiltinKind::Hexa), _) | (_, Some(BuiltinKind::Hexa)) => {
            let patch = if legacy.is_some() {
                komponist_hexa::decode_state(device.state.as_deref()).ok()
            } else {
                device
                    .state
                    .as_deref()
                    .and_then(komponist_hexa::state::decode_clap_patch)
            }
            .unwrap_or_else(komponist_hexa::default_patch);
            let encoded = komponist_hexa::encode_state(&patch);
            (
                legacy_source(hexa_states().iter().position(|state| *state == encoded)?),
                false,
            )
        }
        (Some(BuiltinKind::Internal(kind)), _) => {
            let (index, changed) = kind.loaded_preset(device.state.as_deref())?;
            (legacy_source(index), changed)
        }
        (_, Some(BuiltinKind::Internal(kind))) => {
            let name = wrapped_name(device.state.as_deref()?)?;
            let preset = presets.iter().find(|preset| preset.name == name)?;
            (
                preset.source.clone(),
                wrapper_preset_changed(kind, &preset.name, param_base),
            )
        }
        _ => return None,
    };
    Some((
        presets.iter().position(|preset| preset.source == source)?,
        changed,
    ))
}

fn wrapped_name(state: &[u8]) -> Option<String> {
    let state = pinned_clap_state::decode(state)?;
    let name = state.get("fields")?.get(FACTORY_PRESET_FIELD)?.as_str()?;
    serde_json::from_str(name).ok()
}

fn wrapper_preset_changed(
    kind: Internal,
    name: &str,
    param_base: impl Fn(&str) -> Option<f64>,
) -> bool {
    let Some(index) = kind.presets().iter().position(|preset| preset.name == name) else {
        return false;
    };
    let Some(values) = kind.preset_values(index) else {
        return false;
    };
    kind.params().iter().zip(values).any(|(param, value)| {
        param_base(param.id.as_str())
            .is_some_and(|base| (base - pinned_clap_state::to_clap(param, value)).abs() > 1e-6)
    })
}

fn hexa_states() -> &'static [Vec<u8>] {
    static STATES: std::sync::OnceLock<Vec<Vec<u8>>> = std::sync::OnceLock::new();
    STATES.get_or_init(|| {
        komponist_hexa::presets()
            .filter_map(|preset| {
                komponist_hexa::preset_patch(preset.index)
                    .map(|patch| komponist_hexa::encode_state(&patch))
            })
            .collect()
    })
}
