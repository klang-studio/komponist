//! Project-relative asset handling for the two verified bundled samplers. This adapter
//! uses their pinned nice-plug state encoding and remains outside the CLAP audio path.

use std::path::Path;

use komponist_device_core::pinned_clap_state::{MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD};
use serde_json::Value;

use crate::bundled_clap::VerifiedClap;

pub(super) fn is_sampler(bundle: &VerifiedClap) -> bool {
    matches!(
        bundle.plugin_id(),
        "org.klang-studio.komponist.sampler" | "org.klang-studio.komponist.general-sampler"
    )
}

/// Rewrite only the bundled asset field. Keep the original state if the blob uses an
/// unknown framing or if it has no usable relative links.
pub(super) fn runtime_state(state: &[u8], project_dir: &Path) -> Option<Vec<u8>> {
    let (length, json) = state.split_first_chunk::<8>()?;
    if u64::from_le_bytes(*length) != json.len() as u64 {
        return None;
    }
    let mut wrapper: Value = serde_json::from_slice(json).ok()?;
    let field = wrapper.get_mut("fields")?.get_mut(SAMPLE_ASSETS_FIELD)?;
    let assets = field.as_str()?;
    if assets.len() > MAX_SAMPLE_ASSET_BYTES {
        return None;
    }
    let refreshed = komponist_sampler::links::refresh_absolute_fallbacks(assets, project_dir)?;
    if refreshed.len() > MAX_SAMPLE_ASSET_BYTES {
        return None;
    }
    *field = Value::String(refreshed);
    let json = serde_json::to_vec(&wrapper).ok()?;
    let mut state = Vec::with_capacity(8 + json.len());
    state.extend_from_slice(&(json.len() as u64).to_le_bytes());
    state.extend_from_slice(&json);
    Some(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_device_core::pinned_clap_state;
    use serde_json::{Map, json};

    #[test]
    fn rewrites_only_resolvable_absolute_fallbacks_and_keeps_other_state() {
        let moved = tempfile::tempdir().unwrap();
        let sample = moved.path().join("sample.wav");
        std::fs::write(&sample, []).unwrap();
        let old = moved.path().join("old/sample.wav");
        let assets = json!({
            "samples": {
                "1": {"abs": old, "rel": "sample.wav", "name": "Tone", "extra": true},
                "2": {"abs": old, "rel": "missing.wav", "name": "Missing"},
            },
            "sample_edit": {"slices": [0.5]},
        });
        let state = pinned_clap_state::encode(
            "0.1.0",
            Map::from_iter([("transpose".into(), json!({"i32": 4}))]),
            Map::from_iter([
                (SAMPLE_ASSETS_FIELD.into(), json!(assets.to_string())),
                ("factory_preset".into(), json!("unchanged")),
            ]),
        );
        let rebased = runtime_state(&state, moved.path()).unwrap();
        assert!(runtime_state(&rebased, moved.path()).is_none());
        assert!(runtime_state(&state[..state.len() - 1], moved.path()).is_none());

        let mut saved: Value = serde_json::from_slice(&rebased[8..]).unwrap();
        let field = &mut saved["fields"][SAMPLE_ASSETS_FIELD];
        let refreshed: Value = serde_json::from_str(field.as_str().unwrap()).unwrap();
        let mut expected = assets;
        expected["samples"]["1"]["abs"] = json!(sample);
        assert_eq!(refreshed, expected);
        let original: Value = serde_json::from_slice(&state[8..]).unwrap();
        *field = original["fields"][SAMPLE_ASSETS_FIELD].clone();
        assert_eq!(saved, original);
    }
}
