//! Project-relative asset handling for the two verified bundled samplers. This adapter
//! uses their pinned nice-plug state encoding and remains outside the CLAP audio path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use komponist_builtin::Internal;
use komponist_ipc::ParamInfo as ClapParamInfo;
use komponist_ipc::control::MAX_STATE_PARAMETER_BASES;
use komponist_ipc::native_layout::{GraphKind, NativeLayout};
use komponist_model::params::ParamInfo;
use komponist_model::project::SavedPluginState;
use komponist_model::{Device, PluginRef};
use komponist_sampler::links::{self, Collector, SampleLink};
use komponist_sampler::settings::Settings;
use komponist_sampler::{PADS, PadField};

use komponist_device_core::pinned_clap_state::{self, MAX_SAMPLE_ASSET_BYTES, SAMPLE_ASSETS_FIELD};
use serde_json::{Map, Value};

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

enum EmbeddedLinkState<'a> {
    Bundled(&'a [u8]),
    Legacy(&'a [u8]),
}

impl<'a> EmbeddedLinkState<'a> {
    fn from_device(device: &'a Device, snapshot: Option<&'a [u8]>) -> Option<Self> {
        if kind(&device.plugin).is_some() {
            Some(Self::Bundled(snapshot.or(device.state.as_deref())?))
        } else {
            kind_by_id(crate::legacy_bundled::bundled_clap_id(&device.plugin)?)?;
            Some(Self::Legacy(device.state.as_deref()?))
        }
    }
}

/// Collect legacy embedded files, including links an older bundle may still use.
pub fn collect_embedded_links(
    device: &Device,
    collector: &mut Collector,
    snapshot: Option<&[u8]>,
    project_dir: Option<&Path>,
) -> std::io::Result<()> {
    let wrapper;
    let state = match EmbeddedLinkState::from_device(device, snapshot) {
        Some(EmbeddedLinkState::Bundled(state)) => {
            wrapper = decode(state);
            wrapper.as_ref().and_then(asset_field).map(str::as_bytes)
        }
        Some(EmbeddedLinkState::Legacy(state)) => Some(state),
        None => return Ok(()),
    };
    for (_, link) in links::read(state) {
        if let Some(found) = link.resolve(project_dir) {
            collector.collect_file(&found)?;
        }
    }
    Ok(())
}

/// Read the auxiliary sample document embedded in the bundled CLAP state.
pub fn assets(state: &[u8]) -> Option<String> {
    Some(asset_field(&decode(state)?)?.to_owned())
}

fn pad_count(kind: Internal) -> usize {
    match kind {
        Internal::Sampler => PADS,
        Internal::GeneralSampler => 1,
        _ => 0,
    }
}

/// Extract old project file links when a bundled sampler starts using host-managed assets.
/// Other saved fields, including slice options and trim parameters, stay in the wrapper state.
pub(crate) fn move_embedded_links(
    device: &komponist_model::Device,
) -> Option<komponist_model::Device> {
    let kind = kind(&device.plugin)?;
    let state = device.state.as_deref()?;
    let moved: Vec<_> = links::read(Some(assets(state)?.as_bytes()))
        .into_iter()
        .filter(|(pad, _)| *pad < pad_count(kind))
        .collect();
    if moved.is_empty() {
        return None;
    }
    let state = update_assets(state, |assets| {
        let mut updated = assets.as_bytes().to_vec();
        for (pad, _) in &moved {
            updated = links::write(Some(&updated), *pad, None);
        }
        String::from_utf8(updated).ok()
    })?;
    let mut result = device.clone();
    for (pad, link) in moved {
        result
            .asset_links
            .entry(pad as u32)
            .or_insert(komponist_model::plugin_assets::AssetLink {
                abs: link.abs,
                rel: link.rel,
                display_name: (!link.name.is_empty()).then_some(link.name),
            });
    }
    result.state = Some(state.into());
    Some(result)
}

/// Merge manual parameter edits into an asset-bearing state before editing its links.
pub fn edit_params<'a>(
    version: &str,
    state: Option<&[u8]>,
    edits: impl Iterator<Item = (&'a komponist_model::params::ParamInfo, f64)>,
) -> Option<Vec<u8>> {
    let mut changes = edits.filter(|(_, value)| value.is_finite()).peekable();
    changes.peek()?;
    let (mut params, fields) = if let Some(state) = state {
        let mut state = decode(state)?;
        (
            std::mem::take(state.get_mut("params")?.as_object_mut()?),
            std::mem::take(state.get_mut("fields")?.as_object_mut()?),
        )
    } else {
        (Map::new(), Map::new())
    };
    for (info, raw) in changes {
        let value = pinned_clap_state::from_clap(info, raw);
        params.insert(
            info.id.as_str().into(),
            pinned_clap_state::parameter(info, value).0,
        );
    }
    Some(pinned_clap_state::encode(version, params, fields))
}

/// Replace or clear a pad's link. A new recording resets its playback region. Relinking an
/// existing General Sampler source preserves its slice map, as in the original device.
pub fn edit_link(
    kind: Internal,
    version: &str,
    state: Option<&[u8]>,
    pad: usize,
    link: Option<&SampleLink>,
    relinking: bool,
) -> Option<Vec<u8>> {
    if pad >= pad_count(kind) {
        return None;
    }
    let initial;
    let state = if let Some(state) = state {
        state
    } else {
        initial = pinned_clap_state::encode(version, Map::new(), Map::new());
        &initial
    };
    edit_state(state, |wrapper, assets| {
        let updated = links::write(Some(assets.as_bytes()), pad, link);
        let updated = reset_region(wrapper, updated, kind, pad, link.is_some(), relinking)?;
        if link.is_some() {
            let mut updated: Value = serde_json::from_slice(&updated).ok()?;
            links::touch(&mut updated, pad);
            Some(updated.to_string())
        } else {
            String::from_utf8(updated).ok()
        }
    })
}

fn reset_trim_parameters(
    kind: Internal,
    pad: usize,
    attaching: bool,
    relinking: bool,
) -> impl Iterator<Item = &'static ParamInfo> {
    let general = pad_count(kind) == 1;
    let reset = should_reset_trim(kind, attaching, relinking);
    kind.params()
        .iter()
        .enumerate()
        .filter_map(move |(wire, parameter)| {
            let trim = if general {
                matches!(parameter.id.as_str(), "start" | "end")
            } else {
                [PadField::Start.wire(pad), PadField::End.wire(pad)].contains(&wire)
            };
            (reset && trim).then_some(parameter)
        })
}

fn should_reset_trim(kind: Internal, attaching: bool, relinking: bool) -> bool {
    attaching && (pad_count(kind) != 1 || !relinking)
}

fn remove_reset_trim_bases(
    parameter_bases: &mut BTreeMap<u32, f64>,
    kind: Internal,
    pad: usize,
    attaching: bool,
    relinking: bool,
    author_ids: Option<&BTreeMap<String, u32>>,
) -> Option<()> {
    if !parameter_bases.is_empty() {
        for parameter in reset_trim_parameters(kind, pad, attaching, relinking) {
            parameter_bases.remove(author_ids?.get(parameter.id.as_str())?);
        }
    }
    Some(())
}

/// Edit a legacy embedded file link and discard only the bases replaced by the trim reset.
pub fn edit_saved_link(
    device: &Device,
    pad: usize,
    link: Option<&SampleLink>,
    relinking: bool,
    author_ids: Option<&BTreeMap<String, u32>>,
) -> Option<SavedPluginState> {
    let kind = kind(&device.plugin)?;
    let bytes = edit_link(
        kind,
        &device.plugin.version,
        device.state.as_deref(),
        pad,
        link,
        relinking,
    )?;
    let mut parameter_bases = device.parameter_bases.clone();
    remove_reset_trim_bases(
        &mut parameter_bases,
        kind,
        pad,
        link.is_some(),
        relinking,
        author_ids,
    )?;
    Some(SavedPluginState {
        bytes: bytes.into(),
        parameter_bases,
    })
}

fn reset_region(
    wrapper: &mut Value,
    assets: Vec<u8>,
    kind: Internal,
    pad: usize,
    attaching: bool,
    relinking: bool,
) -> Option<Vec<u8>> {
    let mut trim = reset_trim_parameters(kind, pad, attaching, relinking).peekable();
    if trim.peek().is_some() {
        let params = wrapper.get_mut("params")?.as_object_mut()?;
        for parameter in trim {
            params.insert(
                parameter.id.as_str().into(),
                pinned_clap_state::parameter(parameter, parameter.default).0,
            );
        }
    }
    Some(reset_slices(assets, kind, relinking))
}

fn reset_slices(assets: Vec<u8>, kind: Internal, relinking: bool) -> Vec<u8> {
    if pad_count(kind) == 1 && !relinking {
        komponist_general_sampler::write_settings(Some(&assets), &Settings::default())
    } else {
        assets
    }
}

fn waveform_trim_defaults(
    layout: &NativeLayout,
    params: &[ClapParamInfo],
    slot: u32,
) -> Option<[(u32, f64); 2]> {
    let mut bindings = layout
        .pages
        .iter()
        .flat_map(|page| &page.cells)
        .filter_map(|cell| cell.graph.as_ref())
        .filter(|graph| graph.kind == GraphKind::Waveform && graph.asset_slot == Some(slot))
        .map(|graph| ["start", "end"].map(|name| graph.slots.get(name).copied()));
    let [Some(start), Some(end)] = bindings.next()? else {
        return None;
    };
    if start == end || bindings.any(|ids| ids != [Some(start), Some(end)]) {
        return None;
    }
    let [start, end] = [start, end].map(|id| {
        let info = params.iter().find(|info| info.id == id)?;
        (info.default.is_finite() && (info.min..=info.max).contains(&info.default))
            .then_some((id, info.default))
    });
    Some([start?, end?])
}

/// Prepare the bundled sampler's stored trim and slice state for a user file edit.
/// The file link itself is stored in the project and is synced independently by the host.
pub(crate) fn edit_native_regions(
    device: &Device,
    changes: &[crate::AssetEdit],
    layout: Option<&NativeLayout>,
    params: &[ClapParamInfo],
) -> Option<SavedPluginState> {
    let kind = kind(&device.plugin)?;
    let current_layout = layout.filter(|layout| layout.schema_version == 2);
    let initial;
    let state = if let Some(state) = device.state.as_deref() {
        state
    } else {
        initial = pinned_clap_state::encode(&device.plugin.version, Map::new(), Map::new());
        &initial
    };
    let mut parameter_bases = device.parameter_bases.clone();
    let bytes = edit_state(state, |wrapper, assets| {
        let mut updated = assets.as_bytes().to_vec();
        for change in changes {
            let pad = change.slot as usize;
            if pad >= pad_count(kind) {
                return None;
            }
            let defaults = current_layout
                .filter(|_| should_reset_trim(kind, change.attaching, change.relinking))
                .and_then(|layout| waveform_trim_defaults(layout, params, change.slot))
                .filter(|defaults| {
                    let additional_bases = defaults
                        .iter()
                        .filter(|(id, _)| !parameter_bases.contains_key(id))
                        .count();
                    parameter_bases.len() + additional_bases <= MAX_STATE_PARAMETER_BASES
                });
            if let Some(defaults) = defaults {
                // Reapply the live CLAP defaults through the saved state on both host routes.
                parameter_bases.extend(defaults);
                updated = reset_slices(updated, kind, change.relinking);
            } else {
                // Older bundles and edits without room for the overrides can use the stored trim.
                remove_reset_trim_bases(
                    &mut parameter_bases,
                    kind,
                    pad,
                    change.attaching,
                    change.relinking,
                    layout.and_then(NativeLayout::author_ids),
                )?;
                updated = reset_region(
                    wrapper,
                    updated,
                    kind,
                    pad,
                    change.attaching,
                    change.relinking,
                )?;
            }
        }
        String::from_utf8(updated).ok()
    })?;
    if parameter_bases.len() > MAX_STATE_PARAMETER_BASES {
        return None;
    }
    Some(SavedPluginState {
        bytes: bytes.into(),
        parameter_bases,
    })
}

/// Replace the General Sampler slice map without changing the source or CLAP parameters.
pub fn edit_slices(state: &[u8], settings: &Settings) -> Option<Vec<u8>> {
    edit_state(state, |_, assets| {
        let updated = komponist_general_sampler::write_settings(Some(assets.as_bytes()), settings);
        (assets.as_bytes() != updated).then(|| String::from_utf8(updated).ok())?
    })
}

/// Initialize slice markers in a prepared sampler state.
pub fn edit_slice_markers(state: &[u8], times: impl IntoIterator<Item = f64>) -> Option<Vec<u8>> {
    edit_slices(state, &Settings::from_markers(times))
}

/// Rebase the device's legacy links for the destination, including deferred CLAP snapshots.
pub fn rebase_embedded_links(
    device: &Device,
    snapshot: Option<&[u8]>,
    old_dir: Option<&Path>,
    new_dir: &Path,
    destination: impl Fn(PathBuf) -> PathBuf,
) -> Option<Vec<u8>> {
    let rebase = |state: &[u8]| links::rebase_with(Some(state), old_dir, new_dir, &destination);
    match EmbeddedLinkState::from_device(device, snapshot)? {
        // Only the CLAP host supplies a separate saved snapshot.
        EmbeddedLinkState::Legacy(state) => snapshot.is_none().then(|| rebase(state)).flatten(),
        EmbeddedLinkState::Bundled(state) => update_assets(state, |assets| {
            String::from_utf8(rebase(assets.as_bytes())?).ok()
        }),
    }
}

/// Refresh only the bundled asset field before a runtime restore.
pub(super) fn runtime_state(state: &[u8], project_dir: &Path) -> Option<Vec<u8>> {
    update_assets(state, |assets| {
        links::refresh_absolute_fallbacks(assets, project_dir)
    })
}

fn update_assets(state: &[u8], update: impl FnOnce(&str) -> Option<String>) -> Option<Vec<u8>> {
    edit_state(state, |_, assets| {
        let updated = update(assets)?;
        (updated != assets).then_some(updated)
    })
}

fn edit_state(
    state: &[u8],
    edit: impl FnOnce(&mut Value, &str) -> Option<String>,
) -> Option<Vec<u8>> {
    let mut wrapper = decode(state)?;
    let fields = wrapper.get("fields")?.as_object()?;
    let existing = if fields.contains_key(SAMPLE_ASSETS_FIELD) {
        asset_field(&wrapper)?
    } else {
        "{}"
    }
    .to_owned();
    let updated = edit(&mut wrapper, &existing)?;
    if updated.len() > MAX_SAMPLE_ASSET_BYTES {
        return None;
    }
    wrapper
        .get_mut("fields")?
        .as_object_mut()?
        .insert(SAMPLE_ASSETS_FIELD.into(), Value::String(updated));
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

    fn bundled_general_sampler() -> Device {
        Device {
            id: komponist_model::DeviceId(1),
            role: komponist_model::DeviceRole::Instrument,
            plugin: PluginRef {
                format: komponist_model::PluginFormat::Clap,
                id: "org.klang-studio.komponist.general-sampler".into(),
                name: "General Sampler".into(),
                vendor: String::new(),
                version: "0.1.0".into(),
                module: crate::catalog::bundled_module("general-sampler.clap"),
            },
            bypass: false,
            state: None,
            parameter_bases: Default::default(),
            asset_links: Default::default(),
            sidechain: None,
        }
    }

    #[test]
    fn editing_an_empty_bundled_sampler_starts_with_linked_state_and_uses_normalized_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let link = SampleLink::new(&temp.path().join("sample.wav"), Some(temp.path()));
        let kind = Internal::GeneralSampler;
        let first = edit_link(kind, "0.1.0", None, 0, Some(&link), false).unwrap();
        let data = assets(&first).unwrap();
        assert_eq!(links::read(Some(data.as_bytes())), [(0, link.clone())]);
        let mut wrapper = decode(&first).unwrap();
        assert_eq!(wrapper["params"]["start"], json!({"f32": 0.0}));
        assert_eq!(wrapper["params"]["end"], json!({"f32": 1.0}));
        assert!(edit_link(kind, "", Some(&first), 1, Some(&link), false).is_none());
        let settings = Settings {
            slices: vec![komponist_sampler::settings::Slice {
                time: 0.5,
                options: Default::default(),
            }],
            ..Settings::default()
        };
        wrapper["params"]["start"] = json!({"f32": 0.25});
        let payload = serde_json::to_vec(&wrapper).unwrap();
        let first = (payload.len() as u64)
            .to_le_bytes()
            .into_iter()
            .chain(payload)
            .collect::<Vec<_>>();
        let sliced = edit_slices(&first, &settings).unwrap();
        let relinked = edit_link(kind, "", Some(&sliced), 0, Some(&link), true).unwrap();
        assert_eq!(
            decode(&relinked).unwrap()["params"]["start"],
            json!({"f32": 0.25})
        );
        assert_eq!(links::revisions(&assets(&relinked).unwrap())[0], 2);
        assert_eq!(
            komponist_general_sampler::read_settings(Some(assets(&relinked).unwrap().as_bytes())),
            settings
        );
        let cleared = edit_link(kind, "", Some(&relinked), 0, None, false).unwrap();
        let data = assets(&cleared).unwrap();
        assert!(links::read(Some(data.as_bytes())).is_empty());
        assert_eq!(
            komponist_general_sampler::read_settings(Some(data.as_bytes())),
            Settings::default()
        );
    }

    #[test]
    fn older_sampler_replacement_clears_only_the_reset_trim_bases() {
        let device = Device {
            parameter_bases: [(7, 0.25), (9, 0.75), (31, 0.4)].into(),
            ..bundled_general_sampler()
        };
        let layout = NativeLayout::parse(
            br#"{
                "schema_version": 1, "default_locale": "en", "locales": ["en"],
                "author_ids": {"start": 7, "end": 9},
                "pages": [{
                    "key": "sample", "labels": {"en": "Sample"}, "columns": 2,
                    "cells": [
                        {"widget": "knob", "col": 0, "row": 0, "cols": 1, "rows": 1, "param_id": 7},
                        {"widget": "knob", "col": 1, "row": 0, "cols": 1, "rows": 1, "param_id": 9}
                    ]
                }]
            }"#,
        )
        .unwrap();
        let ids = layout.author_ids();
        let link = SampleLink::new(std::path::Path::new("/samples/tone.wav"), None);
        for relinking in [false, true] {
            let native = edit_native_regions(
                &device,
                &[crate::AssetEdit {
                    slot: 0,
                    attaching: true,
                    relinking,
                }],
                Some(&layout),
                &[],
            )
            .unwrap();
            let embedded = edit_saved_link(&device, 0, Some(&link), relinking, ids).unwrap();
            for after in [native, embedded] {
                if relinking {
                    assert_eq!(after.parameter_bases, device.parameter_bases);
                } else {
                    assert_eq!(after.parameter_bases, [(31, 0.4)].into());
                    let state = decode(&after.bytes).unwrap();
                    assert_eq!(state["params"]["start"], json!({"f32": 0.0}));
                    assert_eq!(state["params"]["end"], json!({"f32": 1.0}));
                }
            }
        }
        let linked = edit_saved_link(&device, 0, Some(&link), true, ids).unwrap();
        let device = Device {
            state: Some(linked.bytes),
            ..device
        };
        let cleared = edit_saved_link(&device, 0, None, false, None).unwrap();
        assert_eq!(cleared.parameter_bases, device.parameter_bases);
    }

    #[test]
    fn native_sample_edits_use_clap_trim_defaults_for_the_asset_slot() {
        let mut document: Value = serde_json::from_str(
            r#"{
                "schema_version": 2, "default_locale": "en", "locales": ["en"],
                "pages": [{
                    "key": "sample", "labels": {"en": "Sample"}, "columns": 6,
                    "cells": [
                        {
                            "widget": "graph", "col": 0, "row": 0, "cols": 3, "rows": 3,
                            "graph": {
                                "kind": "waveform", "name": {"en": "Other"},
                                "description": {"en": "Other asset"}, "asset_slot": 1,
                                "slots": {"start": 44, "end": 51}
                            }
                        },
                        {
                            "widget": "graph", "col": 3, "row": 0, "cols": 3, "rows": 3,
                            "graph": {
                                "kind": "waveform", "name": {"en": "Waveform"},
                                "description": {"en": "Sample range"}, "asset_slot": 0,
                                "slots": {"start": 7, "end": 9}
                            }
                        }
                    ]
                }]
            }"#,
        )
        .unwrap();
        let parse =
            |document: &Value| NativeLayout::parse(&serde_json::to_vec(document).unwrap()).unwrap();
        let layout = parse(&document);
        assert!(layout.author_ids().is_none());
        let params =
            [(7, 0.1), (9, 0.9), (44, 0.2), (51, 0.8)].map(|(id, default)| ClapParamInfo {
                id,
                name: id.to_string(),
                module: String::new(),
                min: 0.0,
                max: 1.0,
                default,
                step_count: 0,
                flags: 0,
            });
        layout.validate_params(&params).unwrap();
        let opaque_trim = Map::from_iter([
            ("start".into(), json!({"f32": 0.3})),
            ("end".into(), json!({"f32": 0.6})),
        ]);
        let opaque = pinned_clap_state::encode("0.1.0", opaque_trim.clone(), Map::new());
        let sliced = edit_slices(&opaque, &Settings::from_markers([0.25, 0.5])).unwrap();
        let cleared = edit_slices(&sliced, &Settings::default()).unwrap();
        let device = Device {
            state: Some(sliced.clone().into()),
            parameter_bases: [(7, 0.35), (31, 0.4)].into(),
            ..bundled_general_sampler()
        };
        for (attaching, relinking) in [(true, false), (true, true), (false, false)] {
            let after = edit_native_regions(
                &device,
                &[crate::AssetEdit {
                    slot: 0,
                    attaching,
                    relinking,
                }],
                Some(&layout),
                &params,
            )
            .unwrap();
            let expected_bases = if attaching && !relinking {
                [(7, 0.1), (9, 0.9), (31, 0.4)].into()
            } else {
                device.parameter_bases.clone()
            };
            assert_eq!(after.parameter_bases, expected_bases);
            assert_eq!(decode(&after.bytes).unwrap()["params"], json!(opaque_trim));
            let expected = if relinking { &sliced } else { &cleared };
            assert_eq!(assets(&after.bytes), assets(expected));
        }

        document["author_ids"] = json!({"start": 7, "end": 9, "other_start": 44, "other_end": 51});
        let with_ids = parse(&document);
        let full_bases: BTreeMap<_, _> = (1_000..)
            .take(MAX_STATE_PARAMETER_BASES)
            .map(|id| (id, 0.4))
            .collect();
        let full = Device {
            parameter_bases: full_bases.clone(),
            ..device.clone()
        };
        document["pages"][0]["cells"][1]["graph"]["slots"]["end"] = 7.into();
        let duplicate = parse(&document);
        let change = crate::AssetEdit {
            slot: 0,
            attaching: true,
            relinking: false,
        };
        for (device, layout, expected_bases) in [
            (&full, &with_ids, full_bases),
            (&device, &duplicate, [(31, 0.4)].into()),
        ] {
            let after =
                edit_native_regions(device, std::slice::from_ref(&change), Some(layout), &params)
                    .unwrap();
            assert_eq!(after.parameter_bases, expected_bases);
            assert_eq!(
                decode(&after.bytes).unwrap()["params"],
                json!({"start": {"f32": 0.0}, "end": {"f32": 1.0}})
            );
            assert_eq!(assets(&after.bytes), assets(&cleared));
        }
    }

    #[test]
    fn save_as_keeps_the_original_link_even_when_the_destination_has_a_matching_name() {
        use komponist_model::plugin_assets::AssetLink;

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
        let device = Device {
            asset_links: [(0, AssetLink::new(&unrelated, Some(&new)))].into(),
            ..bundled_general_sampler()
        };
        let rebased =
            rebase_embedded_links(&device, Some(&state), Some(&old), &new, |path| path).unwrap();
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
        let relinked = edit_link(
            Internal::GeneralSampler,
            "0.1.0",
            Some(&rebased),
            0,
            Some(&SampleLink::new(&unrelated, Some(&new))),
            true,
        )
        .unwrap();
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
