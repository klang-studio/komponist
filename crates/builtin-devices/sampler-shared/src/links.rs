//! How the sampler stores its samples in the device state: as **links**, like Ableton (WP-87,
//! owner decision 2026-11-14).
//!
//! The state is the same JSON as every internal device's (`{"params": {…}}`) plus one key,
//! `"samples"`: an object from the pad number (`"1"` … `"8"`) to a [`SampleLink`] with the absolute
//! path, the path relative to the project file, and the name the pad shows.
//!
//! - **Resolve** ([`SampleLink::resolve`]): the relative path first (against the folder of the
//!   project file, so a project folder that moved as a whole keeps working), then the absolute one.
//! - **Save** ([`rebase`]): every link that resolves gets its absolute path refreshed and its
//!   relative path recomputed for the file being written (also on "Speichern unter"); a link that
//!   does not resolve stays as it is, so nothing is lost while the sample is missing.
//!
//! Everything here is plain `std`, non-realtime.

use std::path::{Path, PathBuf};

use komponist_model::paths::normalize;
pub use komponist_model::paths::{absolute, relative};
use serde_json::{Map, Value};

use super::MAX_PAD_SLOTS;

/// The key of the links in the device state.
const KEY: &str = "samples";

/// One pad's sample: where it is and what the pad calls it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleLink {
    /// The absolute path when the link was made or last saved.
    pub abs: PathBuf,
    /// The path relative to the project file's folder when last saved (empty before the project
    /// has a file).
    pub rel: PathBuf,
    /// The pad's name (the file name without extension).
    pub name: String,
}

impl SampleLink {
    /// A link to `path` (made absolute against the current folder if it is relative), named after
    /// the file, with a relative path if `project_dir` is known.
    pub fn new(path: &Path, project_dir: Option<&Path>) -> SampleLink {
        let abs = absolute(path);
        let rel = project_dir.map(|d| relative(&abs, d)).unwrap_or_default();
        let name = abs
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        SampleLink { abs, rel, name }
    }

    /// The file the link names: the relative path against `project_dir` if that exists, else the
    /// absolute path if that exists, else `None` (the sample is missing).
    pub fn resolve(&self, project_dir: Option<&Path>) -> Option<PathBuf> {
        if let Some(dir) = project_dir
            && !self.rel.as_os_str().is_empty()
        {
            let candidate = normalize(&dir.join(&self.rel));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        (!self.abs.as_os_str().is_empty() && self.abs.is_file()).then(|| self.abs.clone())
    }

    fn to_json(&self) -> Value {
        serde_json::json!({
            "abs": self.abs.to_string_lossy(),
            "rel": self.rel.to_string_lossy(),
            "name": self.name,
        })
    }

    fn from_json(v: &Value) -> Option<SampleLink> {
        let text = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default();
        let (abs, rel) = (text("abs"), text("rel"));
        if abs.is_empty() && rel.is_empty() {
            return None;
        }
        Some(SampleLink {
            abs: PathBuf::from(abs),
            rel: PathBuf::from(rel),
            name: text("name").to_owned(),
        })
    }
}

/// The links in a device state, by pad index (0-based); empty for a state without links or one
/// that is not JSON.
pub fn read(state: Option<&[u8]>) -> Vec<(usize, SampleLink)> {
    let Some(value) = state.and_then(|s| serde_json::from_slice::<Value>(s).ok()) else {
        return Vec::new();
    };
    let Some(map) = value.get(KEY).and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut links: Vec<(usize, SampleLink)> = map
        .iter()
        .filter_map(|(k, v)| {
            let pad: usize = k.parse().ok()?;
            (1..=MAX_PAD_SLOTS)
                .contains(&pad)
                .then(|| SampleLink::from_json(v).map(|l| (pad - 1, l)))
                .flatten()
        })
        .collect();
    links.sort_by_key(|(pad, _)| *pad);
    links
}

const REVISIONS: &str = "sample_revisions";

fn read_revisions(state: &Value) -> [u64; MAX_PAD_SLOTS] {
    let mut revisions = [0; MAX_PAD_SLOTS];
    if let Some(values) = state.get(REVISIONS).and_then(Value::as_array) {
        for (revision, value) in revisions.iter_mut().zip(values) {
            *revision = value.as_u64().unwrap_or_default();
        }
    }
    revisions
}

/// A per-pad revision for explicit reloads whose links remain the same.
pub fn revisions(state: &str) -> [u64; MAX_PAD_SLOTS] {
    serde_json::from_str(state)
        .ok()
        .map(|value| read_revisions(&value))
        .unwrap_or_default()
}

/// Writes a sample edit revision into the asset document. Save/rebase preserves it.
pub fn touch(state: &mut Value, pad: usize) {
    let mut revisions = read_revisions(state);
    if let Some(revision) = revisions.get_mut(pad)
        && let Some(object) = state.as_object_mut()
    {
        *revision = revision.wrapping_add(1);
        object.insert(REVISIONS.into(), serde_json::json!(revisions));
    }
}

/// `state` with the link of `pad` set to `link` (`None` removes it). Everything else in the state
/// stays; a state that is not a JSON object is replaced by one with only the link.
pub fn write(state: Option<&[u8]>, pad: usize, link: Option<&SampleLink>) -> Vec<u8> {
    let mut value = state
        .and_then(|s| serde_json::from_slice::<Value>(s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    let object = value.as_object_mut().expect("an object");
    let links = object
        .entry(KEY)
        .or_insert_with(|| Value::Object(Map::new()));
    if !links.is_object() {
        *links = Value::Object(Map::new());
    }
    let links = links.as_object_mut().expect("an object");
    let key = (pad + 1).to_string();
    match link {
        Some(l) => {
            links.insert(key, l.to_json());
        }
        None => {
            links.remove(&key);
        }
    }
    if links.is_empty() {
        object.remove(KEY);
    }
    serde_json::to_vec(&value).expect("JSON encodes")
}

/// `state` with all links replaced by `links`.
fn write_all(state: Option<&[u8]>, links: &[(usize, SampleLink)]) -> Vec<u8> {
    let mut value = state
        .and_then(|s| serde_json::from_slice::<Value>(s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(object) = value.as_object_mut() {
        object.remove(KEY);
    }
    let mut out = serde_json::to_vec(&value).expect("JSON encodes");
    for (pad, link) in links {
        out = write(Some(&out), *pad, Some(link));
    }
    out
}

/// Removes legacy links for pads whose bindings are managed by the host.
pub fn without_managed_links(assets: &str, managed: impl Fn(usize) -> bool) -> Option<String> {
    let mut value: Value = serde_json::from_str(assets).ok()?;
    let object = value.as_object_mut()?;
    let links = object.get_mut(KEY)?.as_object_mut()?;
    let previous = links.len();
    links.retain(|key, _| {
        !key.parse::<usize>()
            .ok()
            .and_then(|pad| pad.checked_sub(1))
            .is_some_and(&managed)
    });
    if links.len() == previous {
        return None;
    }
    if links.is_empty() {
        object.remove(KEY);
    }
    serde_json::to_string(&value).ok()
}

/// Refresh the absolute fallbacks in a sampler asset object for a host that can only pass
/// absolute sample paths at runtime. Keep the relative paths, unresolved links and all other
/// asset metadata in the state. Returns a new asset object only when a link changes.
pub fn refresh_absolute_fallbacks(assets: &str, project_dir: &Path) -> Option<String> {
    if !project_dir.is_absolute() {
        return None;
    }
    let mut assets: Value = serde_json::from_str(assets).ok()?;
    let mut changed = false;
    for (key, value) in assets.get_mut(KEY)?.as_object_mut()? {
        if !key
            .parse::<usize>()
            .is_ok_and(|pad| (1..=MAX_PAD_SLOTS).contains(&pad))
        {
            continue;
        }
        let Some(link) = SampleLink::from_json(value) else {
            continue;
        };
        if let Some(found) = link.resolve(Some(project_dir))
            && found.is_absolute()
            && found != link.abs
            && let Some(fields) = value.as_object_mut()
        {
            fields.insert("abs".into(), Value::String(found.to_str()?.to_owned()));
            changed = true;
        }
    }
    changed.then(|| assets.to_string())
}

/// The links of `state` that do not resolve against `project_dir`, by pad.
pub fn missing(state: Option<&[u8]>, project_dir: Option<&Path>) -> Vec<(usize, SampleLink)> {
    read(state)
        .into_iter()
        .filter(|(_, l)| l.resolve(project_dir).is_none())
        .collect()
}

/// Rewrites the links of `state` for a project file whose folder is `new_dir`: a link that
/// resolves (against `old_dir`, the folder of the file the project was loaded from) gets its
/// absolute path and a relative path to `new_dir`. `None` if nothing changes.
pub fn rebase(state: Option<&[u8]>, old_dir: Option<&Path>, new_dir: &Path) -> Option<Vec<u8>> {
    rebase_with(state, old_dir, new_dir, |path| path)
}

/// Rebases links and redirects resolved sources that have been copied into the project.
pub fn rebase_with(
    state: Option<&[u8]>,
    old_dir: Option<&Path>,
    new_dir: &Path,
    destination: impl Fn(PathBuf) -> PathBuf,
) -> Option<Vec<u8>> {
    let links = read(state);
    if links.is_empty() {
        return None;
    }
    let rebased: Vec<(usize, SampleLink)> = links
        .iter()
        .map(|(pad, link)| {
            let moved = match link.resolve(old_dir) {
                Some(found) => {
                    let path = destination(found);
                    SampleLink {
                        rel: relative(&path, new_dir),
                        abs: path,
                        name: link.name.clone(),
                    }
                }
                None => link.clone(),
            };
            (*pad, moved)
        })
        .collect();
    (rebased != links).then(|| write_all(state, &rebased))
}

#[cfg(test)]
mod tests {
    use komponist_model::asset_collection::Collector;

    use super::*;

    fn collect_state(
        collector: &mut Collector,
        state: Option<&[u8]>,
        project_dir: &Path,
    ) -> Option<Vec<u8>> {
        for (_, link) in read(state) {
            if let Some(path) = link.resolve(None) {
                collector.collect_file(&path).unwrap();
            }
        }
        relink_state(collector, state, project_dir)
    }

    fn relink_state(
        collector: &Collector,
        state: Option<&[u8]>,
        project_dir: &Path,
    ) -> Option<Vec<u8>> {
        rebase_with(state, None, project_dir, |path| {
            collector.placed(&path).unwrap_or(path)
        })
    }

    /// A fresh scratch folder under the system's temp folder.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kp-links-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn links_round_trip_and_keep_the_rest_of_the_state() {
        let state = br#"{"params":{"kit_tune":2}}"#;
        // `/p` has no drive on Windows, where `new` makes the link's path absolute (`C:\s\kick.wav`):
        // the project folder gets the same drive, so both share a root.
        let project = absolute(Path::new("/p"));
        let link = SampleLink::new(Path::new("/s/kick.wav"), Some(&project));
        assert_eq!(link.rel, Path::new("../s/kick.wav"));
        assert_eq!(link.name, "kick");
        let with = write(Some(state), 2, Some(&link));
        let value: Value = serde_json::from_slice(&with).unwrap();
        assert_eq!(value["params"]["kit_tune"], 2);
        assert_eq!(read(Some(&with)), vec![(2, link.clone())]);
        let without = write(Some(&with), 2, None);
        assert!(read(Some(&without)).is_empty());
        assert!(
            serde_json::from_slice::<Value>(&without)
                .unwrap()
                .get(KEY)
                .is_none()
        );
    }

    #[test]
    fn the_relative_path_wins_and_the_absolute_one_is_the_fallback() {
        let dir = scratch("resolve");
        let moved = dir.join("moved");
        let old = file(&dir, "old/kick.wav", b"a");
        file(&moved, "Samples/kick.wav", b"b");
        let link = SampleLink {
            abs: old.clone(),
            rel: PathBuf::from("Samples/kick.wav"),
            name: "kick".into(),
        };
        // The project folder moved along with its samples: relative first, even though the
        // absolute path still exists.
        assert_eq!(
            link.resolve(Some(&moved)),
            Some(moved.join("Samples/kick.wav"))
        );
        // No project folder, or a relative path that does not exist: the absolute one.
        assert_eq!(link.resolve(None), Some(old.clone()));
        assert_eq!(
            link.resolve(Some(&dir.join("elsewhere"))),
            Some(old.clone())
        );
        std::fs::remove_file(&old).unwrap();
        assert_eq!(link.resolve(Some(&dir.join("elsewhere"))), None);
        assert_eq!(missing(Some(&write(None, 0, Some(&link))), None).len(), 1);
    }

    #[test]
    fn saving_rebases_links_that_resolve_and_keeps_missing_ones() {
        let dir = scratch("rebase");
        let kick = file(&dir, "lib/kick.wav", b"a");
        let (a, b) = (dir.join("a"), dir.join("deeper/b"));
        let state = write(None, 0, Some(&SampleLink::new(&kick, Some(&a))));
        let gone = SampleLink {
            abs: dir.join("gone.wav"),
            rel: PathBuf::from("gone.wav"),
            name: "gone".into(),
        };
        let state = write(Some(&state), 1, Some(&gone));
        let rebased = rebase(Some(&state), Some(&a), &b).unwrap();
        let links = read(Some(&rebased));
        assert_eq!(links[0].1.rel, Path::new("../../lib/kick.wav"));
        assert_eq!(links[0].1.abs, kick);
        assert_eq!(links[1].1, gone);
        // Rebasing for the same folder changes nothing.
        assert!(rebase(Some(&rebased), Some(&b), &b).is_none());
    }

    #[test]
    fn collecting_copies_dedupes_by_content_and_rewrites_the_links() {
        let dir = scratch("collect");
        let project = dir.join("proj/Hafenlicht.komponist");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        let kick = file(&dir, "lib/kick.wav", &wavb(b"kick-bytes"));
        let kick_copy = file(&dir, "other/kick-again.wav", &wavb(b"kick-bytes"));
        let other_kick = file(&dir, "third/kick.wav", &wavb(b"different"));
        let snare = file(&dir, "proj/Hafenlicht Samples/snare.wav", &wavb(b"snare"));
        let mut state = None;
        for (pad, path) in [&kick, &kick_copy, &other_kick, &snare]
            .into_iter()
            .enumerate()
        {
            state = Some(write(
                state.as_deref(),
                pad,
                Some(&SampleLink::new(path, None)),
            ));
        }
        let mut c = Collector::new(&project);
        let new = collect_state(&mut c, state.as_deref(), project.parent().unwrap()).unwrap();
        let links = read(Some(&new));
        let folder = dir.join("proj/Hafenlicht Samples");
        // Identical content → one file; the second "kick" with other content gets a number;
        // the snare was already inside and is not copied again.
        assert_eq!(links[0].1.abs, folder.join("kick.wav"));
        assert_eq!(links[1].1.abs, folder.join("kick.wav"));
        assert_eq!(links[2].1.abs, folder.join("kick 2.wav"));
        assert_eq!(links[3].1.abs, snare);
        assert_eq!(links[0].1.rel, Path::new("Hafenlicht Samples/kick.wav"));
        assert_eq!(
            std::fs::read(folder.join("kick 2.wav")).unwrap(),
            wavb(b"different")
        );
        assert_eq!(c.log.iter().filter(|e| e.copied).count(), 2);
        // The originals stay.
        assert!(kick.is_file() && kick_copy.is_file() && other_kick.is_file());
        // A second run copies nothing and changes nothing.
        let mut again = Collector::new(&project);
        assert!(collect_state(&mut again, Some(&new), project.parent().unwrap()).is_none());
        assert!(again.log.iter().all(|e| !e.copied));
    }

    #[test]
    fn relinking_follows_the_collected_files_without_copying_others() {
        let dir = scratch("relink");
        let project = dir.join("proj/Hafenlicht.komponist");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        let kick = file(&dir, "lib/kick.wav", &wavb(b"kick-bytes"));
        let snare = file(&dir, "lib/snare.wav", &wavb(b"snare-bytes"));
        let current = write(None, 0, Some(&SampleLink::new(&kick, None)));
        let mut c = Collector::new(&project);
        collect_state(&mut c, Some(&current), project.parent().unwrap()).unwrap();
        // An earlier state: the kick on pad 1, the snare on pad 2, a missing sample on pad 3, and
        // its parameters.
        let gone = SampleLink {
            abs: dir.join("gone.wav"),
            rel: PathBuf::new(),
            name: "gone".into(),
        };
        let earlier = br#"{"params":{"pad1_tune":12.0}}"#;
        let earlier = write(Some(earlier), 0, Some(&SampleLink::new(&kick, None)));
        let earlier = write(Some(&earlier), 1, Some(&SampleLink::new(&snare, None)));
        let earlier = write(Some(&earlier), 2, Some(&gone));
        let relinked = relink_state(&c, Some(&earlier), project.parent().unwrap()).unwrap();
        let links = read(Some(&relinked));
        let folder = dir.join("proj/Hafenlicht Samples");
        assert_eq!(
            links[0].1.abs,
            folder.join("kick.wav"),
            "collected: follows"
        );
        assert_eq!(links[0].1.rel, Path::new("Hafenlicht Samples/kick.wav"));
        assert_eq!(links[1].1.abs, snare, "only the history has it: not copied");
        assert_eq!(links[1].1.rel, Path::new("../lib/snare.wav"));
        assert!(!folder.join("snare.wav").exists());
        assert_eq!(links[2].1, gone, "missing: as it was");
        let value: Value = serde_json::from_slice(&relinked).unwrap();
        assert_eq!(
            value["params"]["pad1_tune"], 12.0,
            "the rest of the state stays"
        );
        // Relinking a relinked state changes nothing.
        assert!(relink_state(&c, Some(&relinked), project.parent().unwrap()).is_none());
    }

    /// A WAV header followed by `payload` (enough for the audio check; the tests copy bytes only).
    fn wavb(payload: &[u8]) -> Vec<u8> {
        let mut b = wav_bytes();
        b.extend_from_slice(payload);
        b
    }

    /// The smallest valid WAV: header only.
    fn wav_bytes() -> Vec<u8> {
        let mut b = b"RIFF".to_vec();
        b.extend_from_slice(&36u32.to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b
    }

    #[test]
    fn only_real_audio_files_are_collected() {
        let dir = scratch("audio-only");
        let project = dir.join("proj");
        let real = file(&dir, "lib/Kick.WAV", &wav_bytes());
        let fake = file(
            &dir,
            "lib/secret.wav",
            b"-----BEGIN OPENSSH PRIVATE KEY-----",
        );
        let other = file(&dir, "lib/id_ed25519", &wav_bytes());
        let mut c = Collector::new(&project.join("Song.komponist"));
        let copied = c.collect_file(&real).unwrap();
        assert_eq!(copied, c.samples_dir().join("Kick.WAV"));
        assert!(copied.is_file());
        // A file that only has the extension, and one that only has the content: left alone.
        for path in [&fake, &other] {
            assert_eq!(&c.collect_file(path).unwrap(), path);
        }
        assert_eq!(c.skipped, vec![fake.clone(), other.clone()]);
        let names: Vec<_> = std::fs::read_dir(c.samples_dir())
            .unwrap()
            .flatten()
            .collect();
        assert_eq!(names.len(), 1, "only the real WAV was copied");
        // The Sampler's links go through the same check.
        let state = write(None, 0, Some(&SampleLink::new(&fake, None)));
        let other_project = dir.join("proj2");
        let mut c = Collector::new(&other_project.join("Song.komponist"));
        collect_state(&mut c, Some(&state), &other_project);
        assert_eq!(c.skipped, vec![fake]);
        assert!(!c.samples_dir().exists());
    }
}
