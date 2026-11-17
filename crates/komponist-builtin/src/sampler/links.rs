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
//! - **Collect** ([`Collector`]): copies every sample outside `<project name> Samples/` into that
//!   folder next to the project file and rewrites the links. The same content (size, hash and then
//!   the bytes) is copied once, whichever pads or devices use it; a different file with the same
//!   name gets a numbered name. This is the only place that creates a folder; the app calls it on
//!   an explicit user action.
//!
//! Everything here is plain `std`, non-realtime.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use komponist_model::paths::normalize;
pub use komponist_model::paths::{absolute, relative};
use serde_json::{Map, Value};

use super::PADS;

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
            (1..=PADS)
                .contains(&pad)
                .then(|| SampleLink::from_json(v).map(|l| (pad - 1, l)))
                .flatten()
        })
        .collect();
    links.sort_by_key(|(pad, _)| *pad);
    links
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
    let links = read(state);
    if links.is_empty() {
        return None;
    }
    let rebased: Vec<(usize, SampleLink)> = links
        .iter()
        .map(|(pad, link)| {
            let moved = match link.resolve(old_dir) {
                Some(found) => SampleLink {
                    rel: relative(&found, new_dir),
                    abs: found,
                    name: link.name.clone(),
                },
                None => link.clone(),
            };
            (*pad, moved)
        })
        .collect();
    (rebased != links).then(|| write_all(state, &rebased))
}

/// What [`Collector::collect`] did for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collected {
    /// Where the sample was.
    pub from: PathBuf,
    /// Where it is now.
    pub to: PathBuf,
    /// Whether the file was copied (`false`: it already was in the samples folder, or an identical
    /// copy was there).
    pub copied: bool,
}

/// Collects samples into `<project name> Samples/` next to the project file ([`Collector::new`]).
#[derive(Debug)]
pub struct Collector {
    /// The folder of the project file.
    project_dir: PathBuf,
    /// The samples folder.
    samples_dir: PathBuf,
    /// Content of every file the folder holds or got this run: (size, hash) → paths.
    known: HashMap<(u64, u64), Vec<PathBuf>>,
    scanned: bool,
    /// Resolved source → target, so one file is handled once.
    done: HashMap<PathBuf, PathBuf>,
    /// The log of this run.
    pub log: Vec<Collected>,
}

/// The name of the samples folder of project file `project_file`: `<stem> Samples`.
pub fn samples_folder_name(project_file: &Path) -> String {
    let stem = project_file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("{stem} Samples")
}

impl Collector {
    /// A collector for the project file `project_file`; creates nothing yet.
    pub fn new(project_file: &Path) -> Collector {
        let project_dir = absolute(project_file.parent().unwrap_or(Path::new("")));
        let samples_dir = project_dir.join(samples_folder_name(project_file));
        Collector {
            project_dir,
            samples_dir,
            known: HashMap::new(),
            scanned: false,
            done: HashMap::new(),
            log: Vec::new(),
        }
    }

    /// The samples folder.
    pub fn samples_dir(&self) -> &Path {
        &self.samples_dir
    }

    /// Collects every sample of `state` (resolved against `old_dir`) and returns the state with
    /// the rewritten links, `None` if nothing changed. Missing samples keep their links.
    pub fn collect(
        &mut self,
        state: Option<&[u8]>,
        old_dir: Option<&Path>,
    ) -> io::Result<Option<Vec<u8>>> {
        let links = read(state);
        let mut changed = false;
        let mut out = Vec::with_capacity(links.len());
        for (pad, link) in &links {
            let Some(found) = link.resolve(old_dir) else {
                out.push((*pad, link.clone()));
                continue;
            };
            let target = self.place(&found)?;
            let new = SampleLink {
                rel: relative(&target, &self.project_dir),
                abs: target,
                name: link.name.clone(),
            };
            changed |= new != *link;
            out.push((*pad, new));
        }
        Ok(changed.then(|| write_all(state, &out)))
    }

    /// `state` with its links pointed where this collector put their files, without copying: a
    /// link to a file collected in this run gets its place in the samples folder, another link
    /// that resolves (against `old_dir`) is rebased to the project's folder as by [`rebase`], a
    /// missing one stays. `None` if nothing changes.
    ///
    /// For the device states of the undo history after collecting the current ones (#86): they
    /// follow the collected project, and a sample only an earlier step uses is not copied.
    pub fn relink(&self, state: Option<&[u8]>, old_dir: Option<&Path>) -> Option<Vec<u8>> {
        let links = read(state);
        if links.is_empty() {
            return None;
        }
        let relinked: Vec<(usize, SampleLink)> = links
            .iter()
            .map(|(pad, link)| {
                let moved = match link.resolve(old_dir) {
                    Some(found) => {
                        let abs = self.done.get(&normalize(&found)).cloned().unwrap_or(found);
                        SampleLink {
                            rel: relative(&abs, &self.project_dir),
                            abs,
                            name: link.name.clone(),
                        }
                    }
                    None => link.clone(),
                };
                (*pad, moved)
            })
            .collect();
        (relinked != links).then(|| write_all(state, &relinked))
    }

    /// Collects one file (resolved already) into the samples folder, like [`Collector::collect`]
    /// does for the links of a state: copied unless it lies in the folder or an identical copy does.
    /// Returns where it is now. Audio clips use it (#9).
    pub fn collect_file(&mut self, file: &Path) -> io::Result<PathBuf> {
        self.place(file)
    }

    /// Where `file` went in this run ([`Collector::collect_file`], [`Collector::collect`]); `None`
    /// if it was not collected. Does not copy.
    pub fn placed(&self, file: &Path) -> Option<PathBuf> {
        self.done.get(&normalize(file)).cloned()
    }

    /// The folder of the project file the collector works for.
    pub fn project_dir(&self) -> &Path {
        &self.project_dir
    }

    /// The path `file` has in the samples folder after collecting (copied if needed).
    fn place(&mut self, file: &Path) -> io::Result<PathBuf> {
        let file = normalize(file);
        if let Some(target) = self.done.get(&file) {
            return Ok(target.clone());
        }
        let inside = file.starts_with(&self.samples_dir);
        if !self.scanned {
            self.scan()?;
        }
        let key = fingerprint(&file)?;
        let existing = self.known.get(&key).and_then(|paths| {
            // A file already in the folder wins over a copy elsewhere; the bytes must match.
            paths
                .iter()
                .find(|p| same_bytes(p, &file).unwrap_or(false))
                .cloned()
        });
        let (target, copied) = if inside {
            (file.clone(), false)
        } else if let Some(existing) = existing {
            (existing, false)
        } else {
            std::fs::create_dir_all(&self.samples_dir)?;
            let target = self.free_name(&file);
            std::fs::copy(&file, &target)?;
            self.known.entry(key).or_default().push(target.clone());
            (target, true)
        };
        self.log.push(Collected {
            from: file.clone(),
            to: target.clone(),
            copied,
        });
        self.done.insert(file, target.clone());
        Ok(target)
    }

    /// Reads what the samples folder holds (it may not exist yet).
    fn scan(&mut self) -> io::Result<()> {
        self.scanned = true;
        let Ok(entries) = std::fs::read_dir(&self.samples_dir) else {
            return Ok(());
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file()
                && let Ok(key) = fingerprint(&path)
            {
                self.known.entry(key).or_default().push(path);
            }
        }
        Ok(())
    }

    /// A name in the samples folder for `file`: its own name, else `name 2.ext`, `name 3.ext` …
    fn free_name(&self, file: &Path) -> PathBuf {
        let stem = file
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "sample".to_owned());
        let ext = file
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        let mut n = 1;
        loop {
            let name = if n == 1 {
                format!("{stem}{ext}")
            } else {
                format!("{stem} {n}{ext}")
            };
            let candidate = self.samples_dir.join(name);
            if !candidate.exists() {
                return candidate;
            }
            n += 1;
        }
    }
}

/// (size, content hash) of a file.
fn fingerprint(path: &Path) -> io::Result<(u64, u64)> {
    let bytes = std::fs::read(path)?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    Ok((bytes.len() as u64, h.finish()))
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    Ok(std::fs::read(a)? == std::fs::read(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let kick = file(&dir, "lib/kick.wav", b"kick-bytes");
        let kick_copy = file(&dir, "other/kick-again.wav", b"kick-bytes");
        let other_kick = file(&dir, "third/kick.wav", b"different");
        let snare = file(&dir, "proj/Hafenlicht Samples/snare.wav", b"snare");
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
        let new = c.collect(state.as_deref(), None).unwrap().unwrap();
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
            b"different"
        );
        assert_eq!(c.log.iter().filter(|e| e.copied).count(), 2);
        // The originals stay.
        assert!(kick.is_file() && kick_copy.is_file() && other_kick.is_file());
        // A second run copies nothing and changes nothing.
        let mut again = Collector::new(&project);
        assert!(again.collect(Some(&new), None).unwrap().is_none());
        assert!(again.log.iter().all(|e| !e.copied));
    }

    #[test]
    fn relinking_follows_the_collected_files_without_copying_others() {
        let dir = scratch("relink");
        let project = dir.join("proj/Hafenlicht.komponist");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        let kick = file(&dir, "lib/kick.wav", b"kick-bytes");
        let snare = file(&dir, "lib/snare.wav", b"snare-bytes");
        let current = write(None, 0, Some(&SampleLink::new(&kick, None)));
        let mut c = Collector::new(&project);
        c.collect(Some(&current), None).unwrap().unwrap();
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
        let relinked = c.relink(Some(&earlier), None).unwrap();
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
        assert!(c.relink(Some(&relinked), None).is_none());
    }
}
