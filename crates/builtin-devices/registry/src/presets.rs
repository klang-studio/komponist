//! Preset compatibility for historical projects with built-in device references.

use crate::BuiltinKind;
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

/// The preset an in-binary reference device's own state names, and whether it was edited. These
/// devices (`PluginFormat::Builtin`, the test host until #294) have no CLAP preset-load; every
/// CLAP device, bundled or not, uses the selection the host recorded instead.
pub fn legacy_selection(device: &Device, presets: &[ClapPreset]) -> Option<(usize, bool)> {
    let (source, changed) = match BuiltinKind::from_plugin(&device.plugin)? {
        BuiltinKind::Hexa => {
            let patch = komponist_hexa::decode_state(device.state.as_deref())
                .unwrap_or_else(|_| komponist_hexa::default_patch());
            let encoded = komponist_hexa::encode_state(&patch);
            (
                legacy_source(hexa_states().iter().position(|state| *state == encoded)?),
                false,
            )
        }
        BuiltinKind::Internal(kind) => {
            let (index, changed) = kind.loaded_preset(device.state.as_deref())?;
            (legacy_source(index), changed)
        }
    };
    Some((
        presets.iter().position(|preset| preset.source == source)?,
        changed,
    ))
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
