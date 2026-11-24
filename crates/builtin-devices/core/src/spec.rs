//! The shared device description, preset model and parameter-map state codec.

use std::sync::OnceLock;

use komponist_i18n::Lang;
use komponist_model::params::layout::DeviceLayout;
use komponist_model::params::{ParamId, ParamInfo};
use komponist_model::{DeviceRole, PluginFormat, PluginRef};

/// The `"params"` object of a saved parameter-map device state.
pub type StateParams = serde_json::Map<String, serde_json::Value>;

/// Catalog metadata for a built-in device, independent of its processor and state format.
pub struct CatalogSpec {
    /// Catalog ID, saved in projects.
    pub id: &'static str,
    /// The name stored in projects ([`PluginRef::name`]) and the German name.
    pub name: &'static str,
    /// Localized catalog and card name.
    pub name_in: fn(Lang) -> &'static str,
    /// Localized catalog description exported by the device.
    pub description_in: Option<fn(Lang) -> &'static str>,
    /// Role in the engine's device chain.
    pub role: DeviceRole,
    /// CLAP-style features for browser categories and search.
    pub features: &'static [&'static str],
    /// Vendor stored in the project reference.
    pub vendor: &'static str,
    /// Whether the device offers a sidechain input.
    pub sidechain: bool,
}

impl CatalogSpec {
    /// Creates the project reference for this device. The stored name is independent of UI language.
    pub fn plugin_ref(&self) -> PluginRef {
        PluginRef {
            format: PluginFormat::Builtin,
            id: self.id.into(),
            name: self.name.into(),
            vendor: self.vendor.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            module: Box::default(),
        }
    }
}

/// Catalog metadata, layout, presets and state for a device.
pub struct Spec {
    /// The catalog entry for the device.
    pub catalog: CatalogSpec,
    /// The parameters in a language; the index is the wire id and ids, kinds, ranges and defaults
    /// are the same in every language.
    pub params: fn(Lang) -> Vec<ParamInfo>,
    /// The body of the device frame in a language (page names, graph titles, choice labels).
    pub layout: fn(Lang) -> DeviceLayout,
    /// The factory presets in menu order (WP-96); none for [`no_presets`].
    pub presets: fn() -> &'static [Preset],
    /// Earlier preset names and the names they resolve to today.
    pub preset_aliases: &'static [(&'static str, &'static str)],
    /// Compatibility migration applied to persisted parameters before snapping them.
    pub migrate_params: Option<fn(&mut StateParams)>,
    /// Per-language caches dedicated to this device.
    pub cache: SpecCache,
}

/// A factory preset of an internal device (WP-96): the parameters that differ from the defaults,
/// by model id, so a preset keeps working when a parameter is added later (it gets its default).
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// Name in the menu; also what a device state stores as its loaded preset, so it stays unique
    /// in its device and never changes.
    pub name: &'static str,
    /// Folder in the menu, `None` for a flat list.
    pub category: Option<&'static str>,
    /// The parameters that differ from the defaults: model id, plain value.
    pub changes: &'static [(&'static str, f64)],
}

impl Preset {
    /// A preset without a category.
    pub const fn new(name: &'static str, changes: &'static [(&'static str, f64)]) -> Preset {
        Preset {
            name,
            category: None,
            changes,
        }
    }
}

/// `Spec::presets` of a device without presets.
pub fn no_presets() -> &'static [Preset] {
    &[]
}

/// Memoizes the parameter table and layout of a device independently for each language.
pub struct SpecCache {
    params: [OnceLock<Vec<ParamInfo>>; Lang::ALL.len()],
    layout: [OnceLock<DeviceLayout>; Lang::ALL.len()],
}

impl SpecCache {
    /// Empty caches for a device's static [`Spec`].
    pub const fn new() -> Self {
        Self {
            params: [const { OnceLock::new() }; Lang::ALL.len()],
            layout: [const { OnceLock::new() }; Lang::ALL.len()],
        }
    }
}

impl Default for SpecCache {
    fn default() -> Self {
        Self::new()
    }
}

impl Spec {
    /// The parameters in German; the index is the wire id. Ids, kinds, ranges and defaults are
    /// the same in every language, so code that does not show a text uses this; views use
    /// [`Spec::params_in`].
    pub fn params(&'static self) -> &'static [ParamInfo] {
        self.params_in(Lang::De)
    }

    /// The parameters with the names, choices and value texts of `lang`; the index is the wire id.
    pub fn params_in(&'static self, lang: Lang) -> &'static [ParamInfo] {
        self.cache.params[lang as usize].get_or_init(|| {
            let texts = crate::value_texts(lang);
            (self.params)(lang)
                .into_iter()
                .map(|p| p.texts(texts))
                .collect()
        })
    }

    /// The layout of the device frame's body in German (see [`Spec::layout_in`]).
    pub fn layout(&'static self) -> &'static DeviceLayout {
        self.layout_in(Lang::De)
    }

    /// The layout of the device frame's body with the page names, graph titles and choice labels
    /// of `lang`.
    pub fn layout_in(&'static self, lang: Lang) -> &'static DeviceLayout {
        self.cache.layout[lang as usize].get_or_init(|| (self.layout)(lang))
    }

    /// The factory presets in menu order (empty: the device has none).
    pub fn presets(&self) -> &'static [Preset] {
        (self.presets)()
    }

    /// Resolves a former preset name, leaving names without an alias as they are.
    pub fn current_preset_name<'a>(&self, name: &'a str) -> &'a str {
        self.preset_aliases
            .iter()
            .find(|(old, _)| *old == name)
            .map_or(name, |(_, current)| current)
    }

    /// All plain values (by wire id) of preset `index`: the defaults with the preset's changes
    /// (snapped). `None` if there is no such preset.
    pub fn preset_values(&'static self, index: usize) -> Option<Vec<f64>> {
        let preset = self.presets().get(index)?;
        let mut values = self.defaults();
        for (key, value) in preset.changes {
            let wire = self.wire(&ParamId::new(*key))? as usize;
            values[wire] = self.params()[wire].snap(*value);
        }
        Some(values)
    }

    /// The device state after loading preset `index` into a device with state `old`: the preset's
    /// values plus its name under `"preset"`, and the other keys of `old` kept (the sampler's
    /// links). `None` if there is no such preset.
    pub fn preset_state(&'static self, old: Option<&[u8]>, index: usize) -> Option<Vec<u8>> {
        let values = self.preset_values(index)?;
        let name = self.presets()[index].name;
        let mut state: serde_json::Value =
            serde_json::from_slice(&self.encode_state(&values)).expect("an encoded state is JSON");
        state["preset"] = name.into();
        let encoded = serde_json::to_vec(&state).expect("a state encodes as JSON");
        Some(self.keep_extras(old, encoded))
    }

    /// The preset a device state says it loaded (`"preset"`) and whether the values differ from it
    /// now („ *“). `None` for a state without a (known) preset, as every project saved before
    /// WP-96. Earlier names resolve through [`Spec::preset_aliases`].
    pub fn loaded_preset(&'static self, state: Option<&[u8]>) -> Option<(usize, bool)> {
        let value: serde_json::Value = serde_json::from_slice(state?).ok()?;
        let name = self.current_preset_name(value.get("preset")?.as_str()?);
        let index = self.presets().iter().position(|p| p.name == name)?;
        let now = self.decode_state(state).ok()?;
        Some((index, Some(&now) != self.preset_values(index).as_ref()))
    }

    /// The wire id of parameter `id`.
    pub fn wire(&'static self, id: &ParamId) -> Option<u32> {
        self.params()
            .iter()
            .position(|p| p.id == *id)
            .map(|i| i as u32)
    }

    /// The default plain values, by wire id.
    pub fn defaults(&'static self) -> Vec<f64> {
        self.params().iter().map(|p| p.default).collect()
    }

    /// Decodes a device state into plain values by wire id; `None` (a device without state) gives
    /// the defaults. Persisted state stores a JSON object with a `"params"` map keyed by stable
    /// model IDs. Missing values default, unknown IDs are ignored and values are snapped.
    pub fn decode_state(&'static self, state: Option<&[u8]>) -> Result<Vec<f64>, StateError> {
        let Some(bytes) = state else {
            return Ok(self.defaults());
        };
        let mut value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(StateError::Json)?;
        let params = value
            .get_mut("params")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or(StateError::NoParams)?;
        if let Some(migrate) = self.migrate_params {
            migrate(params);
        }
        Ok(self
            .params()
            .iter()
            .map(|p| {
                let stored = params
                    .get(p.id.as_str())
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(p.default);
                p.snap(stored)
            })
            .collect())
    }

    /// Encodes plain values (by wire id; missing ones are the default) as device state.
    pub fn encode_state(&'static self, values: &[f64]) -> Vec<u8> {
        let params: serde_json::Map<String, serde_json::Value> = self
            .params()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let v = p.snap(values.get(i).copied().unwrap_or(p.default));
                (p.id.as_str().to_owned(), serde_json::json!(v))
            })
            .collect();
        // Numbers and strings only: encoding into memory cannot fail.
        serde_json::to_vec(&serde_json::json!({ "params": params }))
            .expect("a parameter map encodes as JSON")
    }

    /// Why `state` is unreadable: not JSON, not an object, or a `params` entry that is not a map
    /// (a corrupted state, or one written by a newer build). `None` for a readable state, one
    /// without `params` included (the sampler's sample links alone mean default values) and for no
    /// state at all.
    pub fn state_error(&'static self, state: Option<&[u8]>) -> Option<StateError> {
        match self.decode_state(state) {
            Ok(_) => None,
            Err(StateError::NoParams) => {
                let value: serde_json::Value = serde_json::from_slice(state?).ok()?;
                (!value.as_object().is_some_and(|o| !o.contains_key("params")))
                    .then_some(StateError::NoParams)
            }
            Err(e) => Some(e),
        }
    }

    /// `state` with parameter `wire` set to `value` (a state without `params` counts as the
    /// defaults). `None` for an unknown wire id and for an unreadable state
    /// ([`Self::state_error`]): the stored bytes stay as they are until the user replaces them on
    /// purpose, e.g. by loading a preset.
    pub fn with_value(
        &'static self,
        state: Option<&[u8]>,
        wire: u32,
        value: f64,
    ) -> Option<Vec<u8>> {
        let info = self.params().get(wire as usize)?;
        if self.state_error(state).is_some() {
            return None;
        }
        let mut values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
        values[wire as usize] = info.snap(value);
        Some(self.keep_extras(state, self.encode_state(&values)))
    }

    /// `encoded` (a state of just parameters) plus every other top-level key of `old` (the
    /// sampler's sample links), so a knob turn does not lose them.
    pub fn keep_extras(&self, old: Option<&[u8]>, encoded: Vec<u8>) -> Vec<u8> {
        let Some(serde_json::Value::Object(extras)) =
            old.and_then(|s| serde_json::from_slice(s).ok())
        else {
            return encoded;
        };
        let Ok(serde_json::Value::Object(mut new)) = serde_json::from_slice(&encoded) else {
            return encoded;
        };
        for (key, value) in extras {
            new.entry(key).or_insert(value);
        }
        serde_json::to_vec(&new).unwrap_or(encoded)
    }
}

/// A device state that is not an internal device's.
#[derive(Debug)]
pub enum StateError {
    /// Not JSON.
    Json(serde_json::Error),
    /// JSON without a `params` object.
    NoParams,
}

impl core::fmt::Display for StateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StateError::Json(e) => write!(f, "invalid device state: {e}"),
            StateError::NoParams => f.write_str("invalid device state: no params"),
        }
    }
}

impl std::error::Error for StateError {}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::params::ParamKind;

    static TEST_DEVICE: Spec = Spec {
        catalog: CatalogSpec {
            id: "test-device",
            name: "Testgerät",
            description_in: None,
            name_in: |lang| match lang {
                Lang::De => "Testgerät",
                Lang::En => "Test device",
            },
            role: DeviceRole::Effect,
            features: &[],
            vendor: "komponist",
            sidechain: false,
        },
        params: |_| {
            vec![ParamInfo::new(
                "level",
                "Level",
                ParamKind::lin(0.0, 1.0),
                0.5,
            )]
        },
        layout: |_| DeviceLayout::knob_grid([ParamId::new("level")], 1),
        presets: || {
            static PRESETS: [Preset; 1] = [Preset::new("Loud", &[("level", 1.0)])];
            &PRESETS
        },
        preset_aliases: &[("Laut", "Loud")],
        migrate_params: Some(|params| {
            if let Some(gain) = params.remove("gain") {
                params.entry("level").or_insert(gain);
            }
        }),
        cache: SpecCache::new(),
    };

    #[test]
    fn state_migration_runs_before_snapping_and_preserves_extra_state_when_editing() {
        let old = br#"{"params":{"gain":10},"extra":true}"#;
        assert_eq!(TEST_DEVICE.decode_state(Some(old)).unwrap(), vec![1.0]);
        assert_eq!(
            TEST_DEVICE.with_value(Some(old), 0, 0.25).unwrap(),
            br#"{"extra":true,"params":{"level":0.25}}"#,
        );
    }

    #[test]
    fn former_preset_name_resolves_and_the_dirty_flag_tracks_current_values() {
        let old = br#"{"params":{"level":1},"preset":"Laut"}"#;
        assert_eq!(TEST_DEVICE.loaded_preset(Some(old)), Some((0, false)));
        let edited = TEST_DEVICE.with_value(Some(old), 0, 0.25).unwrap();
        assert_eq!(TEST_DEVICE.loaded_preset(Some(&edited)), Some((0, true)));
    }
}
