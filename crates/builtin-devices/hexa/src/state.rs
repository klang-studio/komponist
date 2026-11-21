//! hexa's device state (the patch as JSON) and the factory presets.
//!
//! The state is the [`Patch`] serialised with hexa's own serde derive as UTF-8 JSON, nothing
//! around it. hexa's fields carry serde defaults, so a state written before a field existed
//! still loads, and unknown fields are ignored. Loading sanitizes the patch (out-of-range values
//! clamped, non-finite ones replaced), so a state from a file never puts invalid values into the
//! engine. All functions here allocate and run on the UI thread.

use hexa_core::Patch;

/// Number of factory presets (`hexa_core::PRESET_COUNT`).
pub const PRESET_COUNT: usize = hexa_core::PRESET_COUNT;

/// A factory preset as the browser lists it ("Presets · hexa").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PresetInfo {
    /// Index in hexa's factory bank, `0 .. PRESET_COUNT`; stable (new presets go to the end).
    pub index: usize,
    /// Name ("E-Piano").
    pub name: &'static str,
    /// Category ("Tasten").
    pub category: &'static str,
}

/// All factory presets in bank order, without building their patches. Index 0 is "Init".
pub fn presets() -> impl Iterator<Item = PresetInfo> {
    (0..PRESET_COUNT).filter_map(|index| {
        Some(PresetInfo {
            index,
            name: hexa_core::preset_name(index)?,
            category: hexa_core::preset_category(index)?,
        })
    })
}

/// The preset categories in menu order ("Init", "Tasten", …).
pub fn categories() -> impl Iterator<Item = &'static str> {
    hexa_core::categories()
}

/// The patch of factory preset `index`, `None` if out of range.
pub fn preset_patch(index: usize) -> Option<Patch> {
    hexa_core::preset(index).map(|preset| preset.patch)
}

/// The patch of a device without state: factory preset 0, "Init" (the default patch with the
/// factory volume and pitch-bend range).
pub fn default_patch() -> Patch {
    preset_patch(0).unwrap_or_default()
}

/// Encodes `patch` (sanitized first) as device state (JSON).
pub fn encode_state(patch: &Patch) -> Vec<u8> {
    // A patch is structs, arrays, enums and numbers, all finite after sanitizing: nothing JSON
    // cannot represent, so encoding into memory cannot fail.
    serde_json::to_vec(&patch.sanitized()).expect("a sanitized patch encodes as JSON")
}

/// Decodes device state; `None` (a device without state) gives [`default_patch`]. The patch is
/// sanitized.
pub fn decode_state(state: Option<&[u8]>) -> Result<Patch, StateError> {
    match state {
        None => Ok(default_patch()),
        Some(bytes) => serde_json::from_slice::<Patch>(bytes)
            .map(Patch::sanitized)
            .map_err(StateError),
    }
}

/// Convert a complete legacy project patch into the pinned Hexa CLAP state stream.
/// Older or out-of-slider patch fields remain authoritative for audio.
pub fn legacy_patch_to_clap_state(legacy: &[u8], version: &str) -> Result<Vec<u8>, StateError> {
    use komponist_device_core::pinned_clap_state;
    use komponist_i18n::Lang;
    use serde_json::{Map, Value, json};

    let patch = decode_state(Some(legacy))?;
    let mut applied = Vec::with_capacity(crate::device::WIRE_COUNT);
    let params = (0..crate::device::WIRE_COUNT)
        .map(|wire| {
            let field = crate::device::Field::from_wire(wire as u32).expect("valid Hexa wire ID");
            let info = field.info_in(Lang::En);
            let (value, raw) = pinned_clap_state::parameter(&info, field.get(&patch));
            applied.push(raw);
            (info.id.as_str().to_owned(), value)
        })
        .collect();
    let saved_patch = json!({"patch": patch, "applied": applied}).to_string();
    let fields = Map::from_iter([("hexa-patch-v1".to_owned(), Value::from(saved_patch))]);
    Ok(pinned_clap_state::encode(version, params, fields))
}

/// A device state that is not a hexa patch.
#[derive(Debug)]
pub struct StateError(serde_json::Error);

impl core::fmt::Display for StateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid hexa state: {}", self.0)
    }
}

impl std::error::Error for StateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_list_comes_from_the_factory_bank() {
        let list: Vec<_> = presets().collect();
        assert_eq!(list.len(), PRESET_COUNT);
        const { assert!(PRESET_COUNT >= 131) };
        assert_eq!((list[0].name, list[0].category), ("Init", "Init"));
        assert_eq!((list[1].name, list[1].category), ("E-Piano", "Tasten"));
        assert!(list.iter().enumerate().all(|(i, p)| p.index == i));
        // Categories appear in menu order, each as one run.
        let mut order: Vec<&str> = list.iter().map(|p| p.category).collect();
        order.dedup();
        assert_eq!(order, categories().collect::<Vec<_>>());
        assert_eq!(preset_patch(PRESET_COUNT), None);
    }

    #[test]
    fn state_round_trips_every_preset() {
        for index in 0..PRESET_COUNT {
            let patch = preset_patch(index).unwrap();
            let state = encode_state(&patch);
            assert_eq!(decode_state(Some(&state)).unwrap(), patch, "preset {index}");
        }
    }

    #[test]
    fn missing_state_is_init_and_garbage_is_an_error() {
        assert_eq!(decode_state(None).unwrap(), preset_patch(0).unwrap());
        let err = decode_state(Some(b"{\"volume\": ")).unwrap_err();
        assert!(err.to_string().starts_with("invalid hexa state"));
        assert!(decode_state(Some(b"[1, 2]")).is_err());
    }

    #[test]
    fn decoding_sanitizes() {
        let mut patch = preset_patch(1).unwrap();
        patch.volume = 7.0;
        patch.operators[2].ratio = 1000.0;
        let json = serde_json::to_vec(&patch).unwrap();
        let loaded = decode_state(Some(&json)).unwrap();
        assert_eq!((loaded.volume, loaded.operators[2].ratio), (1.0, 32.0));
    }

    #[test]
    fn state_is_readable_json() {
        let state = encode_state(&default_patch());
        let value: serde_json::Value = serde_json::from_slice(&state).unwrap();
        assert_eq!(value["volume"], serde_json::json!(0.25));
        assert!(
            value["operators"]
                .as_array()
                .is_some_and(|ops| ops.len() == 6)
        );
    }
}
