//! Project files `*.komponist` (ARCHITECTURE §5.5, decision D7).
//!
//! One JSON file: `{ "format": "komponist", "version": 5, "project": { … } }`. Plugin states are
//! base64 strings. A missing plugin keeps its state byte-identical through load and save (S06).
//! Older versions are upgraded on load by a chain of `migrate_vN_to_vN+1` functions on the JSON
//! value, one per version step, each with its own test.
//!
//! Versions:
//! - 1: up to WP-100; every track plays into the master.
//! - 2 (WP-101, wave 9): routing. Every track has an `output` (`"master"` or `{"bus": id}`);
//!   optional `sends`, `folder`, `folded` on tracks and `sidechain` on devices; track kind `"bus"`.
//!   `migrate_v1_to_v2` gives every track `"output": "master"`.
//! - 3 (#9, wave 10): audio tracks. Track kind `"audio"`, optional `audio` (input and monitoring)
//!   on tracks, clip content `{"kind": "audio", …}` ([`crate::AudioClip`]). Everything is new
//!   optional data, so `migrate_v2_to_v3` changes nothing; the bump keeps builds from before audio
//!   from opening a project that may hold audio clips as if it were complete.
//! - 4 (#65): optional track and master automation and MIDI/step clip envelopes.
//! - 5 (macros): optional `macros` on tracks, a fixed array of eight slots with names, values and
//!   mappings to automation targets. Tracks without it get the default slots, so
//!   `migrate_v4_to_v5` changes nothing.
//!
//! Loading never trusts the file: after parsing, [`crate::validate`] repairs what has an obvious
//! safe value, drops what is unusable and rejects the rest, so [`Project::validate`] holds for
//! every project that loads. The repairs come back with the project ([`load`]).
//!
//! Thread: any non-RT thread (loading and saving run on workers).

use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::project::{
    BUILTIN_HEXA, BUILTIN_SAMPLER, BUILTIN_VOLT, DRUM_SAMPLER_NAME, HEXA_NAME, PRESSE_NAME,
    Project, VOLT_NAME,
};
use crate::validate::{Repairs, collect_deserialize_repairs, repair_loaded};

/// Value of the `format` field.
pub const FORMAT_TAG: &str = "komponist";

/// Version written by this build; files with a higher version are rejected.
pub const FORMAT_VERSION: u32 = 5;

/// File name extension, without the dot.
pub const FILE_EXTENSION: &str = "komponist";

/// Why a project could not be loaded or saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistError {
    /// Not JSON, or JSON that does not match the schema; the message comes from serde.
    Malformed(String),
    /// The `format` field is missing or not [`FORMAT_TAG`].
    NotAProject,
    /// The file was written by a newer komponist.
    TooNew {
        /// Version in the file.
        found: u32,
    },
    /// A migration step failed.
    Migration {
        /// Version the failed step started from.
        from: u32,
        /// What went wrong.
        message: String,
    },
}

impl fmt::Display for PersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PersistError::Malformed(m) => write!(f, "malformed project file: {m}"),
            PersistError::NotAProject => write!(f, "not a komponist project file"),
            PersistError::TooNew { found } => {
                write!(
                    f,
                    "project file version {found} is newer than {FORMAT_VERSION}"
                )
            }
            PersistError::Migration { from, message } => {
                write!(f, "migration from version {from} failed: {message}")
            }
        }
    }
}

impl std::error::Error for PersistError {}

/// Serialises `project` into the file format (pretty-printed JSON, stable field order).
pub fn to_json(project: &Project) -> Result<String, PersistError> {
    serde_json::to_string_pretty(&FileOut {
        format: FORMAT_TAG,
        version: FORMAT_VERSION,
        project,
    })
    .map_err(|e| PersistError::Malformed(e.to_string()))
}

/// A loaded project and what loading repaired in it.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    /// The project; [`Project::validate`] holds.
    pub project: Project,
    /// What differed from the file (empty for a valid file).
    pub repairs: Repairs,
}

/// Parses a project file of any supported version, migrating it to the current schema, repairing
/// it ([`crate::validate`]) and dropping the repair notes. Use [`load`] to show them.
pub fn from_json(text: &str) -> Result<Project, PersistError> {
    load(text).map(|loaded| loaded.project)
}

/// Parses a project file of any supported version, migrating it to the current schema and
/// repairing it ([`crate::validate`]). The result passes [`Project::validate`]; what was repaired
/// is in [`Loaded::repairs`]. A file that cannot be repaired is an error.
pub fn load(text: &str) -> Result<Loaded, PersistError> {
    let malformed = |e: serde_json::Error| PersistError::Malformed(e.to_string());
    let file: Value = serde_json::from_str(text).map_err(malformed)?;
    let Value::Object(mut file) = file else {
        return Err(PersistError::NotAProject);
    };
    if file.get("format").and_then(Value::as_str) != Some(FORMAT_TAG) {
        return Err(PersistError::NotAProject);
    }
    let version = file
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| PersistError::Malformed("missing or invalid version".to_owned()))?;
    let version = u32::try_from(version)
        .map_err(|_| PersistError::Malformed(format!("version {version} out of range")))?;
    if version > FORMAT_VERSION {
        return Err(PersistError::TooNew { found: version });
    }
    let project = file
        .remove("project")
        .ok_or_else(|| PersistError::Malformed("missing project".to_owned()))?;
    let mut project = migrate(project, version)?;
    rename_builtins(&mut project);
    let (project, mut repairs) =
        collect_deserialize_repairs(|| serde_json::from_value::<Project>(project));
    let mut project = project.map_err(malformed)?;
    let repaired = repair_loaded(&mut project).map_err(PersistError::Malformed)?;
    for (kind, count) in repaired.iter() {
        repairs.add(kind, count);
    }
    Ok(Loaded { project, repairs })
}

/// Upgrades the `project` JSON value of a file with version `from` to [`FORMAT_VERSION`] by
/// running the migration steps in order. Identity for `from == FORMAT_VERSION`.
pub fn migrate(project: serde_json::Value, from: u32) -> Result<serde_json::Value, PersistError> {
    if from > FORMAT_VERSION {
        return Err(PersistError::TooNew { found: from });
    }
    if from == 0 {
        return Err(PersistError::Migration {
            from,
            message: "there is no version 0".to_owned(),
        });
    }
    let mut project = project;
    for (from, step) in (from..).zip(&MIGRATIONS[from as usize - 1..]) {
        project = step(project).map_err(|message| PersistError::Migration { from, message })?;
    }
    Ok(project)
}

/// The file envelope as written.
#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    version: u32,
    project: &'a Project,
}

/// Built-in devices whose display name changed: `(stored id, old name, new name)`. The stored id stays (so
/// every existing project loads); the stored display name of such a plugin reference is replaced on load,
/// without a format version bump, so every view shows the new name and a later save writes it.
/// "pisynth" became hexa, "knurr" became volt; since 2026-11-16 the names of Hexa, Volt and Presse start with
/// a capital (the lowercase names of earlier files are replaced).
const RENAMED_BUILTINS: [(&str, &str, &str); 6] = [
    (BUILTIN_HEXA, "pisynth", HEXA_NAME),
    (BUILTIN_VOLT, "knurr", VOLT_NAME),
    (BUILTIN_HEXA, "hexa", HEXA_NAME),
    (BUILTIN_VOLT, "volt", VOLT_NAME),
    ("presse", "presse", PRESSE_NAME),
    (BUILTIN_SAMPLER, "Sampler", DRUM_SAMPLER_NAME),
];

fn rename_builtins(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("format").and_then(Value::as_str) == Some("builtin") {
                let id = map.get("id").and_then(Value::as_str);
                let name = map.get("name").and_then(Value::as_str);
                let renamed = RENAMED_BUILTINS
                    .iter()
                    .find(|(rid, old, _)| Some(*rid) == id && Some(*old) == name);
                if let Some((_, _, new)) = renamed {
                    map.insert("name".to_owned(), Value::from(*new));
                }
            }
            map.values_mut().for_each(rename_builtins);
        }
        Value::Array(items) => items.iter_mut().for_each(rename_builtins),
        _ => {}
    }
}

/// A migration step on the `project` value: `MIGRATIONS[i]` upgrades version `i + 1` to `i + 2`.
/// Add `migrate_vN_to_vN+1` functions here (with a test each) when [`FORMAT_VERSION`] grows.
type MigrationStep = fn(Value) -> Result<Value, String>;

/// All migration steps, oldest first.
const MIGRATIONS: [MigrationStep; FORMAT_VERSION as usize - 1] = [
    migrate_v1_to_v2,
    migrate_v2_to_v3,
    migrate_v3_to_v4,
    migrate_v4_to_v5,
];

/// Version 1 → 2 (wave 9, routing): every track outputs to the master. Sends, groups and
/// sidechains are optional fields and absent in version 1, which means none. A malformed track
/// list is left alone for the schema check to report.
fn migrate_v1_to_v2(mut project: Value) -> Result<Value, String> {
    let tracks = project.get_mut("tracks").and_then(Value::as_array_mut);
    for track in tracks
        .into_iter()
        .flatten()
        .filter_map(Value::as_object_mut)
    {
        track
            .entry("output")
            .or_insert_with(|| Value::String("master".to_owned()));
    }
    Ok(project)
}

/// Version 2 → 3 (wave 10, audio tracks): nothing to rewrite. Audio tracks, their `audio` settings
/// and audio clips are optional additions, absent in version 2, which means none.
fn migrate_v2_to_v3(project: Value) -> Result<Value, String> {
    Ok(project)
}

/// Version 3 → 4 (automation): all automation fields have empty defaults.
fn migrate_v3_to_v4(project: Value) -> Result<Value, String> {
    Ok(project)
}

/// Version 4 → 5 (track macros): tracks without slots deserialize to the default array.
fn migrate_v4_to_v5(project: Value) -> Result<Value, String> {
    Ok(project)
}

/// serde helper for [`crate::Device::state`]: `Option<Arc<[u8]>>` as an optional standard base64
/// string (with padding). `None` is written as `null`; a missing field reads as `None`.
pub mod base64_state {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer, Serializer};

    /// Writes the state as base64 or `null`.
    pub fn serialize<S: Serializer>(
        state: &Option<Arc<[u8]>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match state {
            Some(bytes) => serializer.serialize_some(&STANDARD.encode(bytes)),
            None => serializer.serialize_none(),
        }
    }

    /// Reads a base64 string or `null`.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Arc<[u8]>>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|text| {
                STANDARD
                    .decode(text)
                    .map(Arc::from)
                    .map_err(D::Error::custom)
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::edits::{self, NewClip, NewClipContent, NewNote};
    use crate::fixtures::{demo_project, empty_project};
    use crate::project::ClipContent;
    use crate::{DeviceId, Tick};

    fn file(project: Value) -> String {
        serde_json::json!({ "format": "komponist", "version": 1, "project": project }).to_string()
    }

    fn demo_value() -> Value {
        serde_json::to_value(demo_project()).unwrap()
    }

    #[test]
    fn round_trips() {
        for project in [demo_project(), empty_project()] {
            let text = to_json(&project).unwrap();
            assert_eq!(from_json(&text).unwrap(), project);
            assert_eq!(
                to_json(&from_json(&text).unwrap()).unwrap(),
                text,
                "stable output"
            );
        }
    }

    #[test]
    fn file_layout() {
        let text = to_json(&demo_project()).unwrap();
        assert!(
            text.starts_with(
                "{\n  \"format\": \"komponist\",\n  \"version\": 5,\n  \"project\": {"
            )
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        let track = &v["project"]["tracks"][0];
        assert_eq!(track["name"], "Drums");
        assert_eq!(track["hue"], 45);
        assert_eq!(track["kind"], "midi");
        assert_eq!(track["input"], "all");
        assert_eq!(track["midi_channel"], "omni");
        assert_eq!(track["output"], "master");
        for absent in ["sends", "folder", "folded", "macros"] {
            assert!(
                track.get(absent).is_none(),
                "{absent} is written only when set"
            );
        }
        assert!(track["channel"]["devices"][0].get("sidechain").is_none());
        assert_eq!(
            track["channel"]["devices"][0]["plugin"]["format"],
            "builtin"
        );
        assert_eq!(track["channel"]["devices"][0]["state"], Value::Null);
        assert_eq!(track["clips"][0]["content"]["kind"], "step");
        assert_eq!(
            v["project"]["tracks"][1]["clips"][0]["content"]["kind"],
            "midi"
        );
        assert_eq!(v["project"]["loop_range"]["start"], 30_720);
    }

    #[test]
    fn device_state_is_base64_and_survives_unchanged() {
        // A state of a plugin nobody has installed (S06): kept byte for byte.
        let bytes: Arc<[u8]> = (0..=255u8).chain([0, 0xff, 0x80]).collect();
        let device = DeviceId(2);
        let project =
            edits::set_device_state(&demo_project(), device, Some(bytes.clone())).unwrap();
        let text = to_json(&project).unwrap();
        let v: Value = serde_json::from_str(&text).unwrap();
        let state = v["project"]["tracks"][0]["channel"]["devices"][0]["state"]
            .as_str()
            .unwrap();
        use base64::Engine as _;
        assert_eq!(
            state,
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        );
        let loaded = from_json(&text).unwrap();
        assert_eq!(loaded.find_device(device).unwrap().2.state, Some(bytes));
        assert_eq!(to_json(&loaded).unwrap(), text);
    }

    #[test]
    fn small_state_encoding() {
        let mut v = demo_value();
        v["tracks"][0]["channel"]["devices"][0]["state"] = "AAEC/w==".into();
        let p = from_json(&file(v.clone())).unwrap();
        let state = p.tracks[0].channel.devices[0].state.clone();
        assert_eq!(state.as_deref(), Some(&[0u8, 1, 2, 255][..]));
        // A missing field reads as no state.
        v["tracks"][0]["channel"]["devices"][0]
            .as_object_mut()
            .unwrap()
            .remove("state");
        assert_eq!(
            from_json(&file(v.clone())).unwrap().tracks[0]
                .channel
                .devices[0]
                .state,
            None
        );
        v["tracks"][0]["channel"]["devices"][0]["state"] = "not base64!".into();
        assert!(matches!(
            from_json(&file(v)),
            Err(PersistError::Malformed(_))
        ));
    }

    #[test]
    fn optional_clip_fields_default() {
        let mut v = demo_value();
        let clip = v["tracks"][1]["clips"][0].as_object_mut().unwrap();
        clip.remove("transpose");
        clip.remove("velocity_pct");
        assert_eq!(from_json(&file(v)).unwrap(), demo_project());
    }

    #[test]
    fn plugin_module_name_round_trips_and_is_optional() {
        // The demo's Surge XT carries its module file name through save and load.
        let project = demo_project();
        let dev = &project.tracks[3].channel.devices[0];
        assert_eq!(&*dev.plugin.module, "Surge XT.clap");
        let back = from_json(&to_json(&project).unwrap()).unwrap();
        assert_eq!(
            &*back.tracks[3].channel.devices[0].plugin.module,
            "Surge XT.clap"
        );
        // An old file has no "module": it loads with an empty name, and saving writes none.
        let mut v = demo_value();
        let plugin = v["tracks"][3]["channel"]["devices"][0]["plugin"]
            .as_object_mut()
            .unwrap();
        assert_eq!(plugin.remove("module").unwrap(), "Surge XT.clap");
        let old = from_json(&file(v)).unwrap();
        assert_eq!(&*old.tracks[3].channel.devices[0].plugin.module, "");
        let text = to_json(&old).unwrap();
        let again: Value = serde_json::from_str(&text).unwrap();
        assert!(
            again["project"]["tracks"][3]["channel"]["devices"][0]["plugin"]
                .get("module")
                .is_none()
        );
    }

    /// WP-88: a MIDI effect is one more device role in the same chain list. A file saved before it
    /// existed has none and loads and saves unchanged; one with MIDI effects round-trips.
    #[test]
    fn midi_effects_are_part_of_the_chain_and_old_files_load() {
        use crate::{DeviceRole, edits::NewDevice};
        let project = demo_project();
        let old_text = to_json(&project).unwrap();
        assert!(!old_text.contains("midieffect"));
        let old = from_json(&old_text).unwrap();
        assert_eq!(old.tracks[0].channel.midi_effect_count(), 0);
        assert_eq!(to_json(&old).unwrap(), old_text);

        let track = project.tracks[0].id;
        let spec = NewDevice {
            role: DeviceRole::MidiEffect,
            plugin: project.tracks[0].channel.devices[0].plugin.clone(),
            state: Some(Arc::from(&b"{}"[..])),
            sidechain: None,
        };
        let (with, id) =
            edits::insert_device(&project, crate::ChannelRef::Track(track), 0, spec).unwrap();
        let text = to_json(&with).unwrap();
        assert!(text.contains(r#""role": "midieffect""#), "{text}");
        let back = from_json(&text).unwrap();
        assert_eq!(back, with);
        let channel = &back.tracks[0].channel;
        assert_eq!(channel.midi_effect_count(), 1);
        assert_eq!(channel.devices[0].id, id);
        assert_eq!(channel.instrument().unwrap().0, 1);
    }

    #[test]
    fn rejects_foreign_and_broken_files() {
        let not_a_project = [
            "[]",
            "42",
            "{}",
            r#"{"format": "ableton", "version": 1, "project": {}}"#,
            r#"{"format": 1, "version": 1}"#,
        ];
        for text in not_a_project {
            assert_eq!(from_json(text), Err(PersistError::NotAProject), "{text}");
        }
        assert!(matches!(from_json("{"), Err(PersistError::Malformed(_))));
        assert!(matches!(from_json(""), Err(PersistError::Malformed(_))));
        let malformed = [
            r#"{"format": "komponist", "project": {}}"#,
            r#"{"format": "komponist", "version": "1", "project": {}}"#,
            r#"{"format": "komponist", "version": 1}"#,
            r#"{"format": "komponist", "version": 1, "project": {"name": "x"}}"#,
            r#"{"format": "komponist", "version": 99999999999, "project": {}}"#,
        ];
        for text in malformed {
            assert!(
                matches!(from_json(text), Err(PersistError::Malformed(_))),
                "{text}"
            );
        }
        let version = FORMAT_VERSION + 1;
        let too_new =
            serde_json::json!({ "format": FORMAT_TAG, "version": version, "project": {} });
        assert_eq!(
            from_json(&too_new.to_string()),
            Err(PersistError::TooNew { found: version })
        );
        assert!(matches!(
            from_json(r#"{"format": "komponist", "version": 0, "project": {}}"#),
            Err(PersistError::Migration { from: 0, .. })
        ));
    }

    #[test]
    fn rejects_what_the_program_cannot_use() {
        let broken: [(&str, Value); 5] = [
            ("/meter/den", 3.into()),
            ("/tempo/bpm", 0.5.into()),
            ("/tracks/1/clips/0/content/active_take", 1.into()),
            ("/tracks/1/clips/0/content/takes", serde_json::json!([])),
            ("/tracks/0/clips/0/content/length_steps", 33.into()),
        ];
        for (pointer, value) in broken {
            let mut v = demo_value();
            *v.pointer_mut(pointer).unwrap() = value;
            let result = from_json(&file(v));
            assert!(
                matches!(result, Err(PersistError::Malformed(_))),
                "{pointer}: {result:?}"
            );
        }
    }

    #[test]
    fn rejects_exhausted_identifiers_without_panicking() {
        for pointer in [
            "/next_id",
            "/tracks/0/id",
            "/tracks/0/channel/devices/0/id",
            "/tracks/0/clips/0/id",
            "/tracks/1/clips/0/content/takes/0/notes/0/id",
        ] {
            let mut value = demo_value();
            *value.pointer_mut(pointer).expect("fixture field") = u64::MAX.into();
            assert!(
                matches!(from_json(&file(value)), Err(PersistError::Malformed(message))
                if message.contains("identifiers exhausted")),
                "{pointer}"
            );
        }
        let mut project = routed_project();
        let track = project
            .tracks
            .iter_mut()
            .find(|t| !t.sends.is_empty())
            .unwrap();
        Arc::make_mut(track).sends[0].id = crate::SendId(u64::MAX);
        assert!(from_json(&to_json(&project).unwrap()).is_err());
        let mut project = routed_project();
        Arc::make_mut(&mut Arc::make_mut(&mut project.master).devices[0]).id =
            crate::DeviceId(u64::MAX);
        assert!(from_json(&to_json(&project).unwrap()).is_err());
    }

    #[test]
    fn large_valid_identifiers_repair_and_round_trip() {
        let mut value = demo_value();
        value["tracks"][0]["id"] = (u64::MAX - 2).into();
        value["next_id"] = 1.into();
        let project = from_json(&file(value)).unwrap();
        assert_eq!(project.next_id, u64::MAX - 1);
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
    }

    #[test]
    fn repairs_ids_and_order() {
        let mut v = demo_value();
        v["next_id"] = 5.into();
        v["tracks"][1]["clips"].as_array_mut().unwrap().reverse();
        v["tracks"][2]["clips"][0]["content"]["takes"][0]["notes"]
            .as_array_mut()
            .unwrap()
            .reverse();
        let p = from_json(&file(v)).unwrap();
        assert_eq!(p, demo_project());
    }

    #[test]
    fn a_stored_pisynth_name_loads_as_hexa_with_the_same_id() {
        let file = |name: &str, id: &str| serde_json::json!({"plugin": {"format": "builtin", "id": id, "name": name}});
        let mut v =
            serde_json::json!({"a": [file("pisynth", "pisynth"), file("pisynth", "sampler")]});
        rename_builtins(&mut v);
        assert_eq!(v["a"][0]["plugin"]["name"], "Hexa");
        assert_eq!(v["a"][0]["plugin"]["id"], "pisynth");
        // Only the built-in hexa is renamed; anything else keeps what it stored.
        assert_eq!(v["a"][1]["plugin"]["name"], "pisynth");
    }

    #[test]
    fn a_stored_knurr_name_loads_as_volt_with_the_same_id() {
        let file = |name: &str, id: &str| serde_json::json!({"plugin": {"format": "builtin", "id": id, "name": name}});
        let mut v = serde_json::json!({"a": [file("knurr", "knurr"), file("knurr", "sampler"), file("Volt", "knurr")]});
        rename_builtins(&mut v);
        assert_eq!(v["a"][0]["plugin"]["name"], "Volt");
        assert_eq!(v["a"][0]["plugin"]["id"], "knurr");
        assert_eq!(v["a"][1]["plugin"]["name"], "knurr");
        assert_eq!(v["a"][2]["plugin"]["name"], "Volt");
    }

    #[test]
    fn a_stored_drum_sampler_name_loads_as_klopfer_with_the_same_id() {
        let mut value = demo_value();
        value["tracks"][0]["channel"]["devices"][0]["plugin"]["name"] = "Sampler".into();
        let bytes = file(value);
        let project = from_json(&bytes).unwrap();
        let plugin = &project.tracks[0].channel.devices[0].plugin;
        assert_eq!(plugin.id, BUILTIN_SAMPLER);
        assert_eq!(plugin.name, DRUM_SAMPLER_NAME);
    }

    #[test]
    fn lowercase_device_names_of_earlier_files_load_capitalised_with_the_same_ids() {
        let file = |name: &str, id: &str| serde_json::json!({"plugin": {"format": "builtin", "id": id, "name": name}});
        let mut v = serde_json::json!({"a": [
            file("hexa", "pisynth"), file("volt", "knurr"), file("presse", "presse"),
            file("presse", "sampler"), file("Hexa", "pisynth")]});
        rename_builtins(&mut v);
        let shown = |i: usize| {
            (
                v["a"][i]["plugin"]["id"].clone(),
                v["a"][i]["plugin"]["name"].clone(),
            )
        };
        assert_eq!(shown(0), ("pisynth".into(), "Hexa".into()));
        assert_eq!(shown(1), ("knurr".into(), "Volt".into()));
        assert_eq!(shown(2), ("presse".into(), "Presse".into()));
        // Only the built-ins are renamed; a name that is already right stays.
        assert_eq!(shown(3), ("sampler".into(), "presse".into()));
        assert_eq!(shown(4), ("pisynth".into(), "Hexa".into()));
    }

    #[test]
    fn migration_hook() {
        let v = demo_value();
        assert_eq!(migrate(v.clone(), FORMAT_VERSION).unwrap(), v);
        assert_eq!(
            migrate(v.clone(), FORMAT_VERSION + 1),
            Err(PersistError::TooNew {
                found: FORMAT_VERSION + 1
            })
        );
        assert!(matches!(
            migrate(v, 0),
            Err(PersistError::Migration { from: 0, .. })
        ));
        assert_eq!(MIGRATIONS.len() as u32, FORMAT_VERSION - 1);
    }

    #[test]
    fn edited_projects_round_trip() {
        let p = demo_project();
        let lead = p.tracks[2].id;
        let (p, clip) = edits::add_clip(
            &p,
            lead,
            NewClip {
                start: Tick(1),
                length: Tick(7),
                name: "Take 1".into(),
                content: NewClipContent::Midi {
                    notes: vec![NewNote {
                        start: Tick(3),
                        length: Tick(1),
                        pitch: 61,
                        velocity: 1,
                    }],
                    take_name: "Take 1".into(),
                },
            },
        )
        .unwrap();
        let p = edits::set_clip_loop(&p, clip, Some(Tick(5)), Tick(7)).unwrap();
        let p = edits::set_master_prop(&p, edits::ChannelProp::Pan(-0.123_456_79)).unwrap();
        let p = edits::set_track_prop(
            &p,
            &[lead],
            edits::TrackProp::Input(crate::MidiInputSel::Device("Arturia „KeyStep“".into())),
        )
        .unwrap();
        assert_eq!(from_json(&to_json(&p).unwrap()).unwrap(), p);
    }

    /// The demo with routing: Pad and Keys send to an effect bus, Drums and Bass form a folded
    /// group, Bass's first effect listens to Drums, a master limiter listens to the group.
    fn routed_project() -> Project {
        use crate::project::{Sidechain, Tap};
        let p = demo_project();
        let t: Vec<_> = p.tracks.iter().map(|t| t.id).collect();
        let (p, hall) = edits::add_bus(&p, edits::NewBus::effect(vec![t[3], t[4]])).unwrap();
        let (p, group) = edits::add_bus(&p, edits::NewBus::group(vec![t[0], t[1]])).unwrap();
        let p = edits::set_track_prop(&p, &[group], edits::TrackProp::Folded(true)).unwrap();
        let send = p.track(t[3]).unwrap().sends[0].id;
        let p = edits::set_send(&p, send, edits::SendProp::Tap(Tap::Pre)).unwrap();
        let bass_fx = p.track(t[1]).unwrap().channel.devices[1].id;
        let p = edits::set_sidechain(&p, bass_fx, Some(Sidechain::new(t[0]))).unwrap();
        let spec = edits::NewDevice {
            role: crate::DeviceRole::Effect,
            plugin: p.track(t[0]).unwrap().channel.devices[0].plugin.clone(),
            state: None,
            sidechain: None,
        };
        let (p, limiter) = edits::insert_device(&p, crate::ChannelRef::Master, 0, spec).unwrap();
        let sc = Sidechain {
            source: group,
            tap: Tap::Post,
        };
        let p = edits::set_sidechain(&p, limiter, Some(sc)).unwrap();
        assert_eq!(p.track(hall).unwrap().kind, crate::TrackKind::Bus);
        p
    }

    #[test]
    fn routing_round_trips() {
        let p = routed_project();
        let text = to_json(&p).unwrap();
        let back = from_json(&text).unwrap();
        assert_eq!(back, p);
        assert_eq!(to_json(&back).unwrap(), text, "stable output");
        let v: Value = serde_json::from_str(&text).unwrap();
        let tracks = v["project"]["tracks"].as_array().unwrap();
        let group = &tracks[0];
        assert_eq!(group["kind"], "bus");
        assert_eq!(group["name"], "Gruppe 1");
        assert_eq!(group["folder"], true);
        assert_eq!(group["folded"], true);
        assert_eq!(
            tracks[1]["output"],
            serde_json::json!({ "bus": group["id"] })
        );
        let bus = tracks.last().unwrap();
        assert_eq!(bus["kind"], "bus");
        assert_eq!(bus["output"], "master");
        assert!(bus.get("folder").is_none());
        let pad = &tracks[4];
        assert_eq!(pad["name"], "Pad");
        let send = &pad["sends"][0];
        assert_eq!(send["target"], bus["id"]);
        assert_eq!(send["level_db"], -6.0);
        assert_eq!(send["tap"], "pre");
        assert_eq!(send["enabled"], true);
        assert_eq!(
            tracks[2]["channel"]["devices"][1]["sidechain"],
            serde_json::json!({ "source": tracks[1]["id"], "tap": "pre" })
        );
        assert_eq!(
            v["project"]["master"]["devices"][0]["sidechain"]["tap"],
            "post"
        );
    }

    /// A file from before wave 9 (version 1, no `output`) loads as every track into the master,
    /// without sends, and equals the project it was saved from.
    #[test]
    fn version_1_files_load_into_the_master() {
        let mut v = demo_value();
        for track in v["tracks"].as_array_mut().unwrap() {
            assert_eq!(
                track.as_object_mut().unwrap().remove("output").unwrap(),
                "master"
            );
        }
        let migrated = migrate(v.clone(), 1).unwrap();
        assert!(
            migrated["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .all(|t| t["output"] == "master")
        );
        let old = from_json(&file(v.clone())).unwrap();
        assert_eq!(old, demo_project());
        assert!(
            old.tracks
                .iter()
                .all(|t| t.output == crate::TrackOutput::Master && t.sends.is_empty() && !t.folder)
        );
        // The same project as version 2 must name the output.
        let v2 = serde_json::json!({ "format": "komponist", "version": 2, "project": v });
        assert!(matches!(
            from_json(&v2.to_string()),
            Err(PersistError::Malformed(_))
        ));
        // A broken track list is left to the schema check.
        assert!(migrate_v1_to_v2(serde_json::json!({ "tracks": [1, "x"] })).is_ok());
        assert!(migrate_v1_to_v2(serde_json::json!({})).is_ok());
    }

    #[test]
    fn repairs_broken_routing_references() {
        let p = routed_project();
        let mut v = serde_json::to_value(&p).unwrap();
        let ids: Vec<u64> = v["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["id"].as_u64().unwrap())
            .collect();
        // Lead outputs to a missing bus, Keys sends to a MIDI track, Pad sends twice to the bus,
        // Bass's sidechain comes from a missing track, Lead is a folder and folded, the bus armed.
        v["tracks"][3]["output"] = serde_json::json!({ "bus": 99_999 });
        v["tracks"][3]["folder"] = true.into();
        v["tracks"][3]["folded"] = true.into();
        v["tracks"][5]["sends"][0]["target"] = ids[3].into();
        let first = v["tracks"][4]["sends"][0].clone();
        let mut second = first.clone();
        second["id"] = 88_888.into();
        v["tracks"][4]["sends"] = serde_json::json!([first, second]);
        v["tracks"][2]["channel"]["devices"][1]["sidechain"]["source"] = 77_777.into();
        let last = ids.len() - 1;
        v["tracks"][last]["arm"] = true.into();
        // The group's members out of order: Drums moves to the end.
        let tracks = v["tracks"].as_array_mut().unwrap();
        let drums = tracks.remove(1);
        tracks.push(drums);
        let q = from_json(&file(v)).unwrap();
        crate::routing::check(&q).unwrap();
        let lead = &q.tracks[q.track_index(crate::TrackId(ids[3])).unwrap()];
        assert_eq!(lead.output, crate::TrackOutput::Master);
        assert!(!lead.folder && !lead.folded);
        assert!(
            q.tracks[q.track_index(crate::TrackId(ids[5])).unwrap()]
                .sends
                .is_empty()
        );
        assert_eq!(
            q.tracks[q.track_index(crate::TrackId(ids[4])).unwrap()]
                .sends
                .len(),
            1
        );
        let bass = &q.tracks[q.track_index(crate::TrackId(ids[2])).unwrap()];
        assert_eq!(bass.channel.devices[1].sidechain, None);
        assert!(!q.tracks.last().unwrap().arm);
        let names: Vec<&str> = q.tracks.iter().map(|t| t.name.as_str()).collect();
        // Drums is still a member: it moves back behind the group, after Bass.
        assert_eq!(
            &names[..3],
            ["Gruppe 1", "Bass", "Drums"],
            "members behind the group"
        );
        assert!(q.next_id > 88_888, "send ids count for next_id");
    }

    #[test]
    fn rejects_routing_cycles_and_bus_content() {
        let p = routed_project();
        let v = serde_json::to_value(&p).unwrap();
        let tracks = v["tracks"].as_array().unwrap();
        let (group, bus) = (
            tracks[0]["id"].clone(),
            tracks.last().unwrap()["id"].clone(),
        );
        let last = tracks.len() - 1;
        let last_output = format!("/tracks/{last}/output");
        let last_clips = format!("/tracks/{last}/clips");
        let broken = [
            // Group → bus → group.
            vec![
                ("/tracks/0/output", serde_json::json!({ "bus": bus })),
                (last_output.as_str(), serde_json::json!({ "bus": group })),
            ],
            // A bus with a clip.
            vec![(last_clips.as_str(), tracks[1]["clips"].clone())],
        ];
        for changes in broken {
            let mut v = v.clone();
            for (pointer, value) in &changes {
                *v.pointer_mut(pointer).unwrap() = value.clone();
            }
            let result = from_json(&file(v));
            assert!(
                matches!(result, Err(PersistError::Malformed(_))),
                "{changes:?}: {result:?}"
            );
        }
    }

    #[test]
    fn errors_display() {
        assert_eq!(
            PersistError::TooNew { found: 6 }.to_string(),
            "project file version 6 is newer than 5"
        );
    }

    // --- Version 3: audio tracks and clips (#9) -----------------------------------------------

    use crate::audio::{AudioFile, AudioInput, ClipTimeMode, HardwareInput, MonitorMode};
    use crate::edits::{
        AudioClipProp, NewAudioTrack, TrackProp, add_audio_track, add_bus, set_audio_clip_prop,
        set_track_prop,
    };

    fn audio_file() -> AudioFile {
        AudioFile {
            abs: "/t/take 1.wav".into(),
            rel: "Unbenannt Samples/take 1.wav".into(),
            frames: 480_000,
            sample_rate: 48_000,
            channels: 2,
        }
    }

    /// An audio track with a clip (gain, fades, Re-Pitch), a second one that records a bus, and
    /// a bus.
    fn audio_project() -> Project {
        let p = Project::new("Audio");
        let (p, bus) = add_bus(&p, crate::edits::NewBus::default()).unwrap();
        let (p, a) = add_audio_track(&p, NewAudioTrack::default()).unwrap();
        let (p, b) = add_audio_track(&p, NewAudioTrack::default()).unwrap();
        let (p, clip) = edits::add_clip(
            &p,
            a,
            NewClip {
                start: crate::Tick(3840),
                length: crate::Tick(7680),
                name: "take 1".into(),
                content: NewClipContent::Audio {
                    file: audio_file(),
                    offset: 48_000,
                },
            },
        )
        .unwrap();
        let p = set_audio_clip_prop(&p, clip, AudioClipProp::GainDb(-3.5)).unwrap();
        let p = set_audio_clip_prop(&p, clip, AudioClipProp::FadeIn(crate::Tick(120))).unwrap();
        let p =
            set_audio_clip_prop(&p, clip, AudioClipProp::TimeMode(ClipTimeMode::RePitch)).unwrap();
        let hw = AudioInput::Hardware(HardwareInput {
            first: 0,
            stereo: true,
        });
        let p = set_track_prop(&p, &[a], TrackProp::AudioInput(hw)).unwrap();
        let p = set_track_prop(&p, &[a], TrackProp::MonitorMode(MonitorMode::Auto)).unwrap();
        let p = set_track_prop(&p, &[a], TrackProp::InputGain(120)).unwrap();
        set_track_prop(&p, &[b], TrackProp::AudioInput(AudioInput::Bus(bus))).unwrap()
    }

    #[test]
    fn audio_projects_round_trip() {
        let p = audio_project();
        let text = to_json(&p).unwrap();
        assert!(text.contains("\"version\": 5"));
        assert!(text.contains("\"kind\": \"audio\""));
        assert!(text.contains("\"time_mode\": \"repitch\""));
        assert_eq!(from_json(&text).unwrap(), p);
        // The default audio settings and an empty relative path are not written.
        let plain = to_json(&demo_project()).unwrap();
        assert!(!plain.contains("\"audio\""));
        let mut q = p.clone();
        for t in &mut q.tracks {
            Arc::make_mut(t).audio = crate::AudioIo::default();
        }
        assert!(!to_json(&q).unwrap().contains("\"input\": \"none\""));
    }

    #[test]
    fn a_version_2_file_loads_as_it_is_and_migration_changes_nothing() {
        let v = demo_value();
        assert_eq!(migrate_v2_to_v3(v.clone()).unwrap(), v);
        let v2 = serde_json::json!({ "format": "komponist", "version": 2, "project": v });
        let loaded = from_json(&v2.to_string()).unwrap();
        assert_eq!(loaded, demo_project());
        assert!(loaded.tracks.iter().all(|t| !t.is_audio()));
        // Saving writes the current format version.
        assert!(to_json(&loaded).unwrap().contains("\"version\": 5"));
        // Audio clip fields that are missing read as the defaults (Fixed, 0 dB, no fades).
        let mut av = serde_json::to_value(audio_project()).unwrap();
        let clip = &mut av["tracks"][1]["clips"][0]["content"];
        for k in ["gain_db", "fade_in", "fade_out", "time_mode"] {
            clip.as_object_mut().unwrap().remove(k);
        }
        let p: Project = serde_json::from_value(av).unwrap();
        let ClipContent::Audio(a) = &p.tracks[1].clips[0].content else {
            panic!("audio clip");
        };
        assert_eq!(
            (a.gain_db, a.fade_in, a.fade_out, a.time_mode),
            (0.0, crate::Tick(0), crate::Tick(0), ClipTimeMode::Fixed)
        );
    }

    #[test]
    fn loading_repairs_audio_files_that_break_and_rejects_what_cannot_work() {
        let value = |p: &Project| serde_json::to_value(p).unwrap();
        let load = |v: serde_json::Value| {
            from_json(&format!(
                r#"{{"format":"komponist","version":3,"project":{v}}}"#
            ))
        };
        // Clip 1 is track 1's clip.
        let mut v = value(&audio_project());
        v["tracks"][1]["clips"][0]["length"] = 99_999_999.into();
        v["tracks"][1]["clips"][0]["content"]["fade_out"] = 99_999_999.into();
        v["tracks"][1]["clips"][0]["content"]["gain_db"] = 500.into();
        let p = load(v).unwrap();
        let clip = &p.tracks[1].clips[0];
        let ClipContent::Audio(a) = &clip.content else {
            panic!("audio clip");
        };
        // RePitch at 120 bpm original: 9 s left behind the offset = 17 280 ticks.
        assert_eq!(clip.length, crate::Tick(17_280));
        assert_eq!(a.gain_db, crate::CLIP_GAIN_DB_MAX);
        assert!(a.fade_in + a.fade_out <= clip.length);
        // An offset behind the end, a file without frames: malformed.
        let mut v = value(&audio_project());
        v["tracks"][1]["clips"][0]["content"]["offset"] = 480_000.into();
        assert!(matches!(load(v), Err(PersistError::Malformed(_))));
        let mut v = value(&audio_project());
        v["tracks"][1]["clips"][0]["content"]["file"]["frames"] = 0.into();
        assert!(matches!(load(v), Err(PersistError::Malformed(_))));
        // An audio clip on a MIDI track is rejected.
        let mut v = value(&audio_project());
        v["tracks"][1]["kind"] = "midi".into();
        assert!(matches!(load(v), Err(PersistError::Malformed(_))));
        // Audio settings on a bus are cleared; an input that is not a bus is cleared.
        let mut v = value(&audio_project());
        v["tracks"][0]["audio"] = serde_json::json!({ "monitor": "on" });
        let own = v["tracks"][2]["id"].clone();
        v["tracks"][2]["audio"] = serde_json::json!({ "input": { "bus": own } });
        let p = load(v).unwrap();
        assert!(p.tracks[0].audio.is_default());
        assert_eq!(p.tracks[2].audio.input, AudioInput::None);
        // An input cycle is rejected: the track records a bus it outputs into.
        let mut v = value(&audio_project());
        let bus = v["tracks"][0]["id"].clone();
        v["tracks"][2]["output"] = serde_json::json!({ "bus": bus });
        assert!(matches!(load(v), Err(PersistError::Malformed(_))));
        // Overlapping audio clips are cut on load: the later one wins.
        let mut v = value(&audio_project());
        let mut second = v["tracks"][1]["clips"][0].clone();
        second["id"] = 9000.into();
        second["start"] = 5760.into();
        v["tracks"][1]["clips"].as_array_mut().unwrap().push(second);
        v["next_id"] = 9001.into();
        let p = load(v).unwrap();
        let clips = &p.tracks[1].clips;
        assert_eq!(clips.len(), 2);
        assert!(clips[0].end() <= clips[1].start);
    }

    #[test]
    fn audio_take_lists_round_trip_legacy_sources_and_reject_invalid_shapes() {
        let p = audio_project();
        let clip = p.tracks[1].clips[0].id;
        let legacy = serde_json::to_value(&p).unwrap();
        assert!(
            legacy["tracks"][1]["clips"][0]["content"]
                .get("takes")
                .is_none()
        );
        let (p, _) = edits::add_audio_take(
            &p,
            clip,
            crate::AudioTake {
                name: "Take 2".into(),
                file: AudioFile {
                    abs: "/t/second.wav".into(),
                    ..audio_file()
                },
                offset: 96000,
            },
        )
        .unwrap();
        assert_eq!(from_json(&to_json(&p).unwrap()).unwrap(), p);
        let value = serde_json::to_value(&p).unwrap();
        let content = &value["tracks"][1]["clips"][0]["content"];
        assert_eq!(content["active_take"], 1);
        assert_eq!(content["file"], content["takes"][1]["file"]);
        assert_eq!(content["offset"], content["takes"][1]["offset"]);
        for malformed in [
            {
                let mut v = value.clone();
                v["tracks"][1]["clips"][0]["content"]["takes"] = serde_json::json!([]);
                v
            },
            {
                let mut v = value.clone();
                v["tracks"][1]["clips"][0]["content"]["active_take"] = 200.into();
                v
            },
            {
                let mut v = value.clone();
                v["tracks"][1]["clips"][0]["content"]["offset"] = 1.into();
                v
            },
        ] {
            assert!(serde_json::from_value::<Project>(malformed).is_err());
        }
        // Structural checks run in Deserialize, before field Deref can index an empty take list.
        let mut empty = value;
        empty["tracks"][1]["clips"][0]["content"]["takes"] = serde_json::json!([]);
        let envelope = serde_json::json!({"format":"komponist", "version":3, "project":empty});
        assert!(matches!(
            from_json(&envelope.to_string()),
            Err(PersistError::Malformed(_))
        ));
    }

    #[test]
    fn inactive_exhausted_audio_takes_survive_save_and_invalid_metadata_is_rejected() {
        let p = audio_project();
        let clip = p.tracks[1].clips[0].id;
        let (p, _) = edits::add_audio_take(
            &p,
            clip,
            crate::AudioTake {
                name: "Short".into(),
                file: AudioFile {
                    frames: 24000,
                    ..audio_file()
                },
                offset: 0,
            },
        )
        .unwrap();
        let p = edits::select_audio_take(&p, clip, 0).unwrap();
        let p = edits::set_clip_length(&p, clip, crate::Tick(7680)).unwrap();
        let (p, _) = edits::split_clips(&p, &[clip], crate::Tick(7680)).unwrap();
        assert_eq!(from_json(&to_json(&p).unwrap()).unwrap(), p);
        let mut value = serde_json::to_value(&p).unwrap();
        value["tracks"][1]["clips"][0]["content"]["takes"][1]["file"]["sample_rate"] = 0.into();
        let envelope = serde_json::json!({"format":"komponist", "version":3, "project":value});
        assert!(matches!(
            from_json(&envelope.to_string()),
            Err(PersistError::Malformed(_))
        ));
    }
    #[test]
    fn projects_without_automation_keep_their_wire_content() {
        let p = audio_project();
        let current = to_json(&p).unwrap();
        let previous = current.replacen("\"version\": 5", "\"version\": 3", 1);
        assert!(!current.contains("automation"));
        assert_eq!(
            migrate_v3_to_v4(serde_json::to_value(&p).unwrap()).unwrap(),
            serde_json::to_value(&p).unwrap()
        );
        assert_eq!(from_json(&previous).unwrap(), p);
        assert_eq!(to_json(&from_json(&previous).unwrap()).unwrap(), current);
    }

    #[test]
    fn version_4_projects_have_default_macros_after_migration() {
        let project = crate::fixtures::automation_project();
        let value = serde_json::to_value(&project).unwrap();
        assert_eq!(migrate_v4_to_v5(value.clone()).unwrap(), value);
        let old = serde_json::json!({ "format": FORMAT_TAG, "version": 4, "project": value });
        let loaded = from_json(&old.to_string()).unwrap();
        assert_eq!(loaded, project);
        assert!(!to_json(&loaded).unwrap().contains("\"macros\""));
    }

    #[test]
    fn macros_round_trip_with_defaults_omitted_inside_the_fixed_array() {
        let mut project = demo_project();
        let track = Arc::make_mut(&mut project.tracks[0]);
        let targets = [
            serde_json::json!({ "kind": "volume" }),
            serde_json::json!({ "kind": "pan" }),
            serde_json::json!({ "kind": "send", "send": 998 }),
            serde_json::json!({ "kind": "param", "device": 999, "param": "cutoff" }),
        ];
        let slot = &mut track.macros.0[0];
        slot.name = "Open".into();
        slot.value = 0.4;
        slot.mappings = targets
            .into_iter()
            .enumerate()
            .map(|(index, target)| crate::macros::Mapping {
                target: serde_json::from_value(target).unwrap(),
                label: "Saved target".into(),
                min: 0.2,
                max: 0.9,
                invert: index == 3,
                bend: if index == 0 { 0.4 } else { 0.0 },
            })
            .collect();

        let text = to_json(&project).unwrap();
        let json: Value = serde_json::from_str(&text).unwrap();
        let slots = json["project"]["tracks"][0]["macros"].as_array().unwrap();
        assert_eq!(slots.len(), crate::macros::MACRO_COUNT);
        assert!(slots[1..].iter().all(|slot| slot == &serde_json::json!({})));
        assert!(slots[0]["mappings"][0].get("invert").is_none());
        assert!(slots[0]["mappings"][3].get("bend").is_none());
        let loaded = from_json(&text).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(to_json(&loaded).unwrap(), text);
    }

    #[test]
    fn macro_loading_repairs_ranges_conflicts_and_array_length() {
        use crate::automation::AutomationTarget;
        use crate::macros::{MACRO_COUNT, MAX_MAPPINGS};

        let mut value = demo_value();
        let track = &mut value["tracks"][0];
        let mut mappings = vec![
            serde_json::json!({ "target": { "kind": "pan" }, "label": "Pan", "min": 2.0, "max": -1.0, "bend": 5.0 }),
            serde_json::json!({ "target": { "kind": "macro", "index": 0 }, "label": "Recursive", "min": 0.0, "max": 1.0 }),
        ];
        let missing = |id| {
            serde_json::json!({
                "target": { "kind": "param", "device": id, "param": "cutoff" },
                "label": "Missing",
                "min": 0.0,
                "max": 1.0
            })
        };
        mappings.extend((500..500 + MAX_MAPPINGS).map(missing));
        track["macros"] = serde_json::json!([
            { "value": 2.0, "mappings": mappings },
            { "value": -1.0, "mappings": [
                { "target": { "kind": "pan" }, "label": "Duplicate", "min": 0.0, "max": 1.0 }
            ] }
        ]);
        let loaded = from_json(&file(value.clone())).unwrap();
        let slots = &loaded.tracks[0].macros;
        assert_eq!(slots.0[0].value, 1.0);
        assert_eq!(slots.0[0].mappings.len(), MAX_MAPPINGS + 1);
        assert_eq!(slots.0[0].mappings[0].min, 0.0);
        assert_eq!(slots.0[0].mappings[0].max, 1.0);
        assert_eq!(slots.0[0].mappings[0].bend, 1.0);
        assert_eq!(slots.driver(&AutomationTarget::Pan), Some(0));
        assert_eq!(
            slots.driver(&slots.0[0].mappings[MAX_MAPPINGS].target),
            None
        );
        assert!(slots.0[1..].iter().all(|slot| slot == &Default::default()));

        value["tracks"][0]["macros"] = Value::Array(vec![serde_json::json!({}); MACRO_COUNT + 1]);
        assert!(matches!(
            from_json(&file(value)),
            Err(PersistError::Malformed(_))
        ));
    }

    #[test]
    fn all_automation_shapes_targets_owners_and_modes_round_trip() {
        use crate::automation::{Point, Points};
        let project = crate::fixtures::automation_project();
        let text = to_json(&project).unwrap();
        assert!(text.contains("[[0,0.25],[960,0.5,0.5],[1920,0.75,\"hold\"],[3840,0.5]]"));
        assert!(!text.contains("\"mode\": \"read\""));
        let loaded = from_json(&text).unwrap();
        assert_eq!(loaded, project);
        assert_eq!(to_json(&loaded).unwrap(), text);
        let points = Points::new(
            (0..2_000)
                .map(|i| Point::linear(Tick(i * 20), (i % 100) as f32 / 100.0))
                .collect(),
        );
        assert!(serde_json::to_string_pretty(&points).unwrap().len() < 20 * 2_000);
    }

    #[test]
    fn loading_repairs_duplicate_lanes_and_points_but_keeps_an_unresolved_target() {
        use crate::automation::{AutomationMode, AutomationTarget, LANE_HEIGHT_MAX};
        let p = crate::fixtures::automation_project();
        let mut serialized: Value = serde_json::to_value(&p).unwrap();
        let lanes = serialized["tracks"][2]["automation"]["lanes"]
            .as_array_mut()
            .unwrap();
        lanes[0]["height"] = 999.into();
        lanes[0]["target"]["device"] = 999999.into();
        lanes[0]["points"] = serde_json::json!([[50, 2.0], [10, -1.0, 5.0], [10, 0.3], [10, 0.5]]);
        let duplicate = lanes[0].clone();
        let id = lanes[0]["id"].clone();
        lanes.push(duplicate);
        let file =
            serde_json::json!({ "format": "komponist", "version": 4, "project": serialized });
        let loaded = from_json(&file.to_string()).unwrap();
        let repaired = &loaded.tracks[2].automation.lanes[0];
        assert_eq!(repaired.id.0, id.as_u64().unwrap());
        assert_eq!(repaired.mode, AutomationMode::Read);
        assert_eq!(repaired.height, LANE_HEIGHT_MAX);
        assert!(
            matches!(repaired.target, AutomationTarget::Param { device, .. } if device.0 == 999999)
        );
        assert_eq!(repaired.points.as_slice().len(), 3);
        assert_eq!(repaired.points.as_slice()[0].value, 0.0);
        assert_eq!(repaired.points.as_slice()[1].value, 0.5);
        assert_eq!(repaired.points.as_slice()[2].value, 1.0);
        assert_eq!(
            loaded.tracks[2]
                .automation
                .lanes
                .iter()
                .filter(|lane| lane.target == repaired.target)
                .count(),
            1
        );
        // The original clip target is still live, so load gives its envelope a Read lane.
        assert!(
            loaded.tracks[2]
                .automation
                .lane(&loaded.tracks[2].clips[0].automation[0].target)
                .is_some()
        );
    }
}
