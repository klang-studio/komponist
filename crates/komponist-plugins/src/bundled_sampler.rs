//! Project-relative asset handling for the two verified bundled samplers. This adapter
//! uses their pinned nice-plug state encoding and remains outside the CLAP audio path.

use std::path::{Path, PathBuf};

use komponist_builtin::Internal;
use komponist_model::PluginRef;
use komponist_sampler::links::{self, Collector, SampleLink};

use komponist_device_core::pinned_clap_state::{MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD};
use serde_json::Value;

use crate::catalog::requires_bundled;

fn kind_by_id(id: &str) -> Option<Internal> {
    match id {
        "org.klang-studio.komponist.sampler" => Some(Internal::Sampler),
        "org.klang-studio.komponist.general-sampler" => Some(Internal::GeneralSampler),
        _ => None,
    }
}

/// The sampler kind for a project reference that requires the packaged bundle.
pub fn kind(plugin: &PluginRef) -> Option<Internal> {
    requires_bundled(plugin)
        .then(|| kind_by_id(&plugin.id))
        .flatten()
}

fn decode(state: &[u8]) -> Option<Value> {
    let (length, json) = state.split_first_chunk::<8>()?;
    if u64::from_le_bytes(*length) != json.len() as u64 {
        return None;
    }
    serde_json::from_slice(json).ok()
}

fn asset_field(wrapper: &Value) -> Option<&str> {
    let field = wrapper.get("fields")?.get(SAMPLE_ASSETS_FIELD)?.as_str()?;
    (field.len() <= MAX_SAMPLE_ASSET_BYTES).then_some(field)
}

/// Read the missing links in a bundled sampler state for the project relink dialog.
pub fn missing_links(state: &[u8], project_dir: Option<&Path>) -> Vec<(usize, SampleLink)> {
    let Some(wrapper) = decode(state) else {
        return Vec::new();
    };
    let assets = asset_field(&wrapper).map(str::as_bytes);
    links::missing(assets, project_dir)
}

/// Collect the sample files referenced by the bundled wrapper state.
pub fn collect_links(
    collector: &mut Collector,
    state: Option<&[u8]>,
    project_dir: Option<&Path>,
) -> std::io::Result<()> {
    let wrapper = state.and_then(decode);
    let assets = wrapper.as_ref().and_then(asset_field).map(str::as_bytes);
    collector.collect(assets, project_dir)?;
    Ok(())
}

/// Replace one missing link while preserving the bundled wrapper's parameters and metadata.
pub fn relink(state: &[u8], pad: usize, link: &SampleLink) -> Option<Vec<u8>> {
    update_assets(state, |assets| {
        String::from_utf8(links::write(Some(assets.as_bytes()), pad, Some(link))).ok()
    })
}

/// Resolve the old links before rebasing them for the destination project folder.
pub fn rebase_links(
    state: &[u8],
    old_dir: Option<&Path>,
    new_dir: &Path,
    destination: impl Fn(PathBuf) -> PathBuf,
) -> Option<Vec<u8>> {
    update_assets(state, |assets| {
        String::from_utf8(links::rebase_with(
            Some(assets.as_bytes()),
            old_dir,
            new_dir,
            destination,
        )?)
        .ok()
    })
}

/// Refresh only the bundled asset field before a runtime restore.
pub(super) fn runtime_state(state: &[u8], project_dir: &Path) -> Option<Vec<u8>> {
    update_assets(state, |assets| {
        links::refresh_absolute_fallbacks(assets, project_dir)
    })
}

fn update_assets(state: &[u8], update: impl FnOnce(&str) -> Option<String>) -> Option<Vec<u8>> {
    let mut wrapper = decode(state)?;
    let updated = update(asset_field(&wrapper)?)?;
    if updated.len() > MAX_SAMPLE_ASSET_BYTES {
        return None;
    }
    *wrapper.get_mut("fields")?.get_mut(SAMPLE_ASSETS_FIELD)? = Value::String(updated);
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
    fn save_as_keeps_the_original_link_even_when_the_destination_has_a_matching_name() {
        let scratch = tempfile::tempdir().unwrap();
        let old = scratch.path().join("old");
        let new = scratch.path().join("new");
        std::fs::create_dir(&old).unwrap();
        std::fs::create_dir(&new).unwrap();
        let sample = old.join("sample.wav");
        let unrelated = new.join("sample.wav");
        std::fs::write(&sample, b"original").unwrap();
        std::fs::write(&unrelated, b"unrelated").unwrap();
        let original = json!({
            "samples": {"1": {"abs": sample, "rel": "sample.wav", "name": "Tone"}},
            "sample_edit": {"slices": [0.5]},
        });
        let state = pinned_clap_state::encode(
            "0.1.0",
            Map::new(),
            Map::from_iter([(SAMPLE_ASSETS_FIELD.into(), json!(original.to_string()))]),
        );
        let rebased = rebase_links(&state, Some(&old), &new, |path| path).unwrap();
        let wrapper = decode(&rebased).unwrap();
        let assets = asset_field(&wrapper).unwrap();
        let [(pad, link)] = links::read(Some(assets.as_bytes())).try_into().unwrap();
        assert_eq!(pad, 0);
        assert_eq!(link.resolve(Some(&new)).as_deref(), Some(sample.as_path()));
        assert_eq!(
            serde_json::from_str::<Value>(assets).unwrap()["sample_edit"],
            original["sample_edit"],
        );
        std::fs::remove_file(&sample).unwrap();
        assert_eq!(missing_links(&rebased, Some(&new)), [(0, link)]);
        let relinked = relink(&rebased, 0, &SampleLink::new(&unrelated, Some(&new))).unwrap();
        assert!(missing_links(&relinked, Some(&new)).is_empty());
        let wrapper = decode(&relinked).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(asset_field(&wrapper).unwrap()).unwrap()["sample_edit"],
            original["sample_edit"],
        );
    }

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
