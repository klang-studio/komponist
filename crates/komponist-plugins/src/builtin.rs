//! The built-in instruments of the catalog (ARCHITECTURE §8.5): pisynth, in process, and klopfer,
//! shown as "klopfer · INTERN": the bundled `Klopfer.clap` hosted through the normal bridge (crash
//! isolation, its own GUI; decision D5).
//!
//! - Where the bundle is looked for: [`klopfer_bundle_candidates`] (installed, development, user
//!   folder), the first that exists wins ([`find_klopfer_bundle`]).
//! - A scanned Klopfer CLAP (the same plugin from the user's CLAP folders) is not listed again while
//!   the built-in exists, and a device that references it resolves to the built-in
//!   ([`crate::Catalog::resolve`]).
//! - Pads: klopfer has [`KLOPFER_PADS`] pads; pad `n` (1-based) plays note
//!   `KLOPFER_FIRST_NOTE + n − 1` by default ([`klopfer_pad`]).
//! - Kits: a host sets klopfer's samples through its plugin state; [`klopfer_kit_state`] turns a
//!   `.klopfer` kit file into such a state.

use std::path::{Path, PathBuf};

use komponist_ipc::{PluginFormat as BundleFormat, PluginKind};
use komponist_model::project::{BUILTIN_KLOPFER, BUILTIN_PISYNTH};
use komponist_model::{PluginFormat, PluginRef};
use serde_json::{Map, Value};

use crate::catalog::{BuiltinKind, CatalogEntry, PluginLocation};

/// CLAP id of klopfer (`../klopfer/src/lib.rs`), loaded from the bundled `Klopfer.clap`.
pub const KLOPFER_CLAP_ID: &str = "com.klopfer.drum-sampler";

/// File name of klopfer's CLAP bundle.
pub const KLOPFER_BUNDLE: &str = "Klopfer.clap";

/// Folder of the bundled plugins relative to the install prefix (`<prefix>/lib/komponist/plugins/`;
/// the app binary lies in `<prefix>/bin/`).
pub const PLUGIN_INSTALL_DIR: &str = "lib/komponist/plugins";

/// Number of klopfer pads (two banks of 16).
pub const KLOPFER_PADS: u8 = 32;

/// Default note of klopfer's first pad (C1 in the General MIDI drum map, klopfer's `BASE_NOTE`).
pub const KLOPFER_FIRST_NOTE: u8 = 36;

/// Plugin state fields of klopfer that belong to the session, not to a kit (klopfer's `kit.rs`).
const SESSION_FIELDS: [&str; 2] = ["editor-state", "midi-map"];

/// The built-in entries: pisynth always, klopfer if `klopfer_bundle` is `Some`.
pub(crate) fn entries(klopfer_bundle: Option<PathBuf>) -> Vec<CatalogEntry> {
    let mut entries = vec![CatalogEntry {
        plugin: builtin_ref(BUILTIN_PISYNTH, "pisynth", "klang-studio"),
        kind: PluginKind::Instrument,
        category: None,
        features: vec!["instrument".into(), "synthesizer".into()],
        location: PluginLocation::InProcess(BuiltinKind::Pisynth),
    }];
    if let Some(path) = klopfer_bundle {
        entries.push(CatalogEntry {
            // klopfer's CLAP vendor (`../klopfer/src/lib.rs`), as in the demo project.
            plugin: builtin_ref(BUILTIN_KLOPFER, "klopfer", "Klopfer"),
            kind: PluginKind::Instrument,
            category: None,
            features: vec!["instrument".into(), "drum".into(), "sampler".into()],
            location: PluginLocation::Bundle {
                format: BundleFormat::Clap,
                path,
                plugin_id: KLOPFER_CLAP_ID.into(),
            },
        });
    }
    entries
}

/// The reference of a built-in ("INTERN").
fn builtin_ref(id: &str, name: &str, vendor: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: id.into(),
        name: name.into(),
        vendor: vendor.into(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

/// Whether `plugin` is klopfer: the built-in, or the same plugin referenced as CLAP.
pub fn is_klopfer(plugin: &PluginRef) -> bool {
    match plugin.format {
        PluginFormat::Builtin => plugin.id == BUILTIN_KLOPFER,
        PluginFormat::Clap => plugin.id == KLOPFER_CLAP_ID,
        PluginFormat::Vst3 => false,
    }
}

/// The klopfer pad (1-based) that plays `note` with klopfer's default pad notes, if any.
pub fn klopfer_pad(note: u8) -> Option<u8> {
    note.checked_sub(KLOPFER_FIRST_NOTE)
        .filter(|&i| i < KLOPFER_PADS)
        .map(|i| i + 1)
}

/// Where the app looks for klopfer's bundle, in this order:
///
/// 1. installed: `<exe_dir>/../lib/komponist/plugins/Klopfer.clap` ([`PLUGIN_INSTALL_DIR`]);
/// 2. development: the sibling checkout's `../klopfer/target/bundled/Klopfer.clap`, where
///    `scripts/build-klopfer.sh` puts it (relative to this source tree);
/// 3. the user's CLAP folder: `~/.clap/Klopfer.clap`.
pub fn klopfer_bundle_candidates(exe_dir: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let installed = exe_dir
        .join("..")
        .join(PLUGIN_INSTALL_DIR)
        .join(KLOPFER_BUNDLE);
    let dev = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../klopfer/target/bundled")
        .join(KLOPFER_BUNDLE);
    let user = home.map(|h| h.join(".clap").join(KLOPFER_BUNDLE));
    [Some(installed), Some(dev), user]
        .into_iter()
        .flatten()
        .collect()
}

/// The first of [`klopfer_bundle_candidates`] that exists, canonicalized.
pub fn find_klopfer_bundle(exe_dir: &Path, home: Option<&Path>) -> Option<PathBuf> {
    klopfer_bundle_candidates(exe_dir, home)
        .into_iter()
        .find_map(|p| std::fs::canonicalize(p).ok())
}

/// klopfer's plugin state for the kit file `kit` (`.klopfer`, as klopfer's "Kit speichern" or its
/// `tools/testkit` write it), for `Device::state`.
///
/// A host has no other way to set klopfer's samples: there is no kit-path parameter, and the state
/// is the only channel. A kit file is klopfer's state as JSON with sample paths relative to the kit
/// folder, so this does what klopfer's own kit loading does (`kit::load`): it resolves the sample
/// paths against the kit folder (a missing absolute path falls back to the same file name next to
/// the kit), migrates single-sample paths of old kits into layer 1, resets the samples and names of
/// pads the kit does not mention, drops the session fields (editor state, MIDI learn) and sets the
/// kit name to the file stem.
///
/// The result is in nice-plug's CLAP state format ([`clap_state`]), for the built-in klopfer (a
/// CLAP plugin). klopfer loads the samples synchronously on activation, or in its next process
/// call when the state arrives while it is active.
pub fn klopfer_kit_state(kit: &Path) -> std::io::Result<Vec<u8>> {
    let invalid = |what: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, what);
    let mut state: Value = serde_json::from_slice(&std::fs::read(kit)?)
        .map_err(|e| invalid(&format!("not a Klopfer kit: {e}")))?;
    let base = kit.parent().unwrap_or(Path::new(""));
    let fields = state
        .get_mut("fields")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid("not a Klopfer kit: no fields"))?;
    for key in SESSION_FIELDS {
        fields.remove(key);
    }
    let stem = kit
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    fields.insert("kit-name".into(), json_string(&Value::String(stem)));
    for pad in 1..=KLOPFER_PADS {
        fields
            .entry(format!("name_{pad}"))
            .or_insert_with(|| json_string(&Value::String(String::new())));
        let layers = pad_layers(fields, pad, base);
        fields.insert(format!("layers_{pad}"), json_string(&layers));
        fields.insert(format!("path_{pad}"), json_string(&Value::Null));
    }
    let json = serde_json::to_vec(&state).map_err(|e| invalid(&e.to_string()))?;
    Ok(clap_state(&json))
}

/// A nice-plug plugin's CLAP state for the state object `json`: its length as a little-endian
/// `u64`, then the JSON (CLAP streams have no length, so nice-plug prepends it; plugins built
/// without nice-plug's `zstd` feature, like klopfer, store the JSON uncompressed).
pub fn clap_state(json: &[u8]) -> Vec<u8> {
    let mut state = Vec::with_capacity(8 + json.len());
    state.extend_from_slice(&(json.len() as u64).to_le_bytes());
    state.extend_from_slice(json);
    state
}

/// A persistent field's value: klopfer stores each one as a JSON text inside the state's JSON.
fn json_string(value: &Value) -> Value {
    Value::String(value.to_string())
}

/// The parsed JSON text of field `key`, if present and valid.
fn json_field(fields: &Map<String, Value>, key: &str) -> Option<Value> {
    fields
        .get(key)
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str(text).ok())
}

/// Pad `pad`'s layer configuration with resolved sample paths (see [`klopfer_kit_state`]). A pad
/// without one gets klopfer's default: no samples.
fn pad_layers(fields: &Map<String, Value>, pad: u8, base: &Path) -> Value {
    let mut config = json_field(fields, &format!("layers_{pad}"))
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({ "mode": "Velocity", "layers": [] }));
    if !config.get("layers").is_some_and(Value::is_array) {
        config["layers"] = Value::Array(Vec::new());
    }
    let layers = config["layers"].as_array_mut().expect("layers is an array");
    // Kits from before layers existed have a single path per pad.
    if let Some(Value::String(legacy)) = json_field(fields, &format!("path_{pad}")) {
        match layers.first_mut() {
            Some(first) if first.get("path").is_some_and(|p| !p.is_null()) => {}
            Some(first) => first["path"] = Value::String(legacy),
            None => layers.push(serde_json::json!({ "path": legacy })),
        }
    }
    for layer in layers.iter_mut() {
        if let Some(path) = layer.get("path").and_then(Value::as_str) {
            let resolved = resolve_sample(path, base);
            layer["path"] = Value::String(resolved);
        }
    }
    config
}

/// A sample path from a kit in folder `base` as a local absolute path: relative paths are relative
/// to the kit; an absolute path that does not exist falls back to the same file name next to the
/// kit, if that exists (samples that moved together with the kit).
fn resolve_sample(path: &str, base: &Path) -> String {
    let p = Path::new(path);
    let resolved = if p.is_relative() {
        base.join(p)
    } else if !p.exists() {
        p.file_name()
            .map(|name| base.join(name))
            .filter(|candidate| candidate.exists())
            .unwrap_or_else(|| p.to_path_buf())
    } else {
        p.to_path_buf()
    };
    resolved.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kp-builtin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn klopfer_is_the_builtin_or_its_clap_plugin() {
        let mut plugin = builtin_ref(BUILTIN_KLOPFER, "klopfer", "Klopfer");
        assert!(is_klopfer(&plugin));
        plugin.format = PluginFormat::Clap;
        assert!(!is_klopfer(&plugin));
        plugin.id = KLOPFER_CLAP_ID.into();
        assert!(is_klopfer(&plugin));
        plugin.format = PluginFormat::Vst3;
        assert!(!is_klopfer(&plugin));
        assert!(!is_klopfer(&builtin_ref(BUILTIN_PISYNTH, "pisynth", "")));
    }

    #[test]
    fn pads_are_numbered_from_note_36() {
        assert_eq!(klopfer_pad(35), None);
        assert_eq!(klopfer_pad(36), Some(1));
        assert_eq!(klopfer_pad(43), Some(8));
        assert_eq!(klopfer_pad(67), Some(32));
        assert_eq!(klopfer_pad(68), None);
        assert_eq!(klopfer_pad(0), None);
    }

    #[test]
    fn bundle_candidates_are_installed_then_dev_then_user() {
        let candidates =
            klopfer_bundle_candidates(Path::new("/opt/komponist/bin"), Some(Path::new("/home/x")));
        assert_eq!(candidates.len(), 3);
        assert_eq!(
            candidates[0],
            Path::new("/opt/komponist/bin/../lib/komponist/plugins/Klopfer.clap")
        );
        assert!(candidates[1].ends_with("klopfer/target/bundled/Klopfer.clap"));
        assert_eq!(candidates[2], Path::new("/home/x/.clap/Klopfer.clap"));
        assert_eq!(klopfer_bundle_candidates(Path::new("/b"), None).len(), 2);

        // The installed bundle wins over the others.
        let prefix = scratch("prefix");
        let plugins = prefix.join(PLUGIN_INSTALL_DIR);
        std::fs::create_dir_all(&plugins).unwrap();
        std::fs::write(plugins.join(KLOPFER_BUNDLE), b"bundle").unwrap();
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let found = find_klopfer_bundle(&prefix.join("bin"), None).unwrap();
        assert_eq!(
            found,
            std::fs::canonicalize(plugins.join(KLOPFER_BUNDLE)).unwrap()
        );
        std::fs::remove_dir_all(&prefix).unwrap();
    }

    /// The parsed JSON text of `fields[key]`.
    fn field(state: &Value, key: &str) -> Value {
        serde_json::from_str(state["fields"][key].as_str().unwrap()).unwrap()
    }

    #[test]
    fn a_kit_becomes_a_state_with_absolute_sample_paths() {
        let dir = scratch("kit");
        std::fs::create_dir_all(dir.join("samples")).unwrap();
        std::fs::write(dir.join("samples/kick.wav"), b"").unwrap();
        std::fs::write(dir.join("moved.wav"), b"").unwrap();
        let layers = |paths: &[&str]| {
            let layers: Vec<Value> = paths
                .iter()
                .map(|p| serde_json::json!({ "path": p, "vel_lo": 1, "vel_hi": 127 }))
                .collect();
            serde_json::json!({ "mode": "RoundRobin", "layers": layers }).to_string()
        };
        let kit = serde_json::json!({
            "version": "0.1.0",
            "params": { "choke_5": { "i32": 1 } },
            "fields": {
                "layers_1": layers(&["samples/kick.wav"]),
                "layers_2": layers(&["/gone/moved.wav", "/gone/nowhere.wav"]),
                "path_3": "\"samples/legacy.wav\"",
                "name_1": "\"Kick\"",
                "editor-state": "{}",
                "midi-map": "[]",
                "kit-name": "\"Old\"",
            },
        });
        let file = dir.join("My Kit.klopfer");
        std::fs::write(&file, serde_json::to_vec_pretty(&kit).unwrap()).unwrap();

        let blob = klopfer_kit_state(&file).unwrap();
        let (length, json) = blob.split_at(8);
        assert_eq!(
            u64::from_le_bytes(length.try_into().unwrap()),
            json.len() as u64
        );
        let state: Value = serde_json::from_slice(json).unwrap();
        assert_eq!(state["version"], "0.1.0");
        assert_eq!(state["params"], kit["params"]);
        let fields = state["fields"].as_object().unwrap();
        assert!(!fields.contains_key("editor-state") && !fields.contains_key("midi-map"));
        assert_eq!(field(&state, "kit-name"), "My Kit");
        assert_eq!(field(&state, "name_1"), "Kick");
        assert_eq!(field(&state, "name_32"), "");

        let pad1 = field(&state, "layers_1");
        assert_eq!(pad1["mode"], "RoundRobin");
        assert_eq!(
            pad1["layers"][0]["path"],
            dir.join("samples/kick.wav").to_str().unwrap()
        );
        assert_eq!(pad1["layers"][0]["vel_lo"], 1);
        let pad2 = field(&state, "layers_2");
        assert_eq!(
            pad2["layers"][0]["path"],
            dir.join("moved.wav").to_str().unwrap()
        );
        assert_eq!(pad2["layers"][1]["path"], "/gone/nowhere.wav");
        // The single path of an old kit becomes layer 1; the old field is cleared.
        let pad3 = field(&state, "layers_3");
        assert_eq!(
            pad3["layers"][0]["path"],
            dir.join("samples/legacy.wav").to_str().unwrap()
        );
        assert_eq!(field(&state, "path_3"), Value::Null);
        // Pads the kit does not mention lose their samples.
        assert_eq!(
            field(&state, "layers_32"),
            serde_json::json!({ "mode": "Velocity", "layers": [] })
        );

        std::fs::write(&file, b"{ not json").unwrap();
        let error = klopfer_kit_state(&file).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(klopfer_kit_state(&dir.join("missing.klopfer")).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
