//! Filesystem placement for Collect and Save. Callers resolve project links before collection.

use std::collections::HashMap;
use std::fs::File;
use std::hash::Hasher;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::paths::{absolute, normalize};

/// What the collector did for one file.
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
    /// The samples folder.
    samples_dir: PathBuf,
    /// Content of every file the folder holds or got this run: (size, hash) → paths.
    known: HashMap<(u64, u64), Vec<PathBuf>>,
    scanned: bool,
    /// Resolved source → target, so one file is handled once.
    done: HashMap<PathBuf, PathBuf>,
    done_assets: HashMap<PathBuf, PathBuf>,
    /// The log of this run.
    pub log: Vec<Collected>,
    /// Files that were **not** collected because they are not audio (see [`is_audio_file`]); their
    /// links stay as they were. A crafted project must not make "Sammeln und speichern" copy an
    /// arbitrary file (a key, a document) next to the project.
    pub skipped: Vec<PathBuf>,
    /// Plugin assets outside the collection folder that were not approved for copying.
    pub unapproved_assets: Vec<PathBuf>,
}

/// File name extensions of audio files that may be collected (lower case).
const AUDIO_EXTENSIONS: [&str; 8] = ["wav", "wave", "aif", "aiff", "flac", "ogg", "oga", "mp3"];

/// Whether `path` is an audio file worth copying: its extension is on the allowlist
/// (case-insensitive) **and** its first bytes are a known audio signature (RIFF/WAVE, FORM/AIFF or
/// AIFC, fLaC, OggS, ID3 or an MPEG frame sync). A tiny check on purpose; it decodes nothing.
pub fn is_audio_file(path: &Path) -> bool {
    let allowed = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()));
    if !allowed {
        return false;
    }
    let mut head = [0u8; 12];
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut n = 0;
    while n < head.len() {
        match file.read(&mut head[n..]) {
            Ok(0) | Err(_) => break,
            Ok(k) => n += k,
        }
    }
    let head = &head[..n];
    let riff = head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WAVE";
    let aiff =
        head.len() >= 12 && &head[..4] == b"FORM" && matches!(&head[8..12], b"AIFF" | b"AIFC");
    let mpeg = head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0;
    riff || aiff
        || mpeg
        || head.starts_with(b"fLaC")
        || head.starts_with(b"OggS")
        || head.starts_with(b"ID3")
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
            samples_dir,
            known: HashMap::new(),
            scanned: false,
            done: HashMap::new(),
            done_assets: HashMap::new(),
            log: Vec::new(),
            skipped: Vec::new(),
            unapproved_assets: Vec::new(),
        }
    }

    /// The samples folder.
    pub fn samples_dir(&self) -> &Path {
        &self.samples_dir
    }

    /// Collects an audio file and returns its destination. Reuses a file already in
    /// the samples folder or an identical audio copy there.
    pub fn collect_file(&mut self, file: &Path) -> io::Result<PathBuf> {
        self.place(file, false)
    }

    /// Collects a plugin-declared file, preserving its extension for formats the plugin chooses.
    pub fn collect_asset_file(&mut self, file: &Path) -> io::Result<PathBuf> {
        self.place(file, true)
    }

    /// The destination recorded by [`Collector::collect_file`], if any.
    pub fn placed(&self, file: &Path) -> Option<PathBuf> {
        self.done.get(&normalize(file)).cloned()
    }

    /// Where a generic asset went, using its own extension-preserving placement.
    pub fn placed_asset(&self, file: &Path) -> Option<PathBuf> {
        self.done_assets.get(&normalize(file)).cloned()
    }

    /// The path `file` has in the samples folder after collecting (copied if needed).
    fn place(&mut self, file: &Path, generic_asset: bool) -> io::Result<PathBuf> {
        let file = normalize(file);
        let done = if generic_asset {
            &self.done_assets
        } else {
            &self.done
        };
        if let Some(target) = done.get(&file) {
            return Ok(target.clone());
        }
        if !generic_asset && !is_audio_file(&file) {
            // Not copied, not changed: the link keeps naming the file where it is.
            self.skipped.push(file.clone());
            self.done.insert(file.clone(), file.clone());
            return Ok(file);
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
                .find(|p| {
                    let supported = if generic_asset {
                        p.extension() == file.extension()
                    } else {
                        is_audio_file(p)
                    };
                    supported && same_bytes(p, &file).unwrap_or(false)
                })
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
        if generic_asset {
            self.done_assets.insert(file, target.clone());
        } else {
            self.done.insert(file, target.clone());
        }
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
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let mut remaining = length;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let mut buffer = [0; 16 * 1024];
    while remaining != 0 {
        let size = remaining.min(buffer.len() as u64) as usize;
        file.read_exact(&mut buffer[..size])?;
        h.write(&buffer[..size]);
        remaining -= size as u64;
    }
    Ok((length, h.finish()))
}

fn same_bytes(a: &Path, b: &Path) -> io::Result<bool> {
    let (mut first, mut second) = (File::open(a)?, File::open(b)?);
    let mut remaining = first.metadata()?.len();
    if remaining != second.metadata()?.len() {
        return Ok(false);
    }
    let mut a = [0; 16 * 1024];
    let mut b = [0; 16 * 1024];
    while remaining != 0 {
        let size = remaining.min(a.len() as u64) as usize;
        first.read_exact(&mut a[..size])?;
        second.read_exact(&mut b[..size])?;
        if a[..size] != b[..size] {
            return Ok(false);
        }
        remaining -= size as u64;
    }
    Ok(first.read(&mut a[..1])? == 0 && second.read(&mut b[..1])? == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh scratch folder under the system's temp folder.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kp-collection-{name}-{}", std::process::id()));
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
    fn generic_collection_preserves_extensions_even_after_audio_deduplication() {
        let dir = scratch("generic-extension");
        let project = dir.join("Set.komponist");
        let existing = file(&dir, "Set Samples/tone.wav", &wavb(b"same"));
        let source = file(&dir, "outside/tone.wave", &wavb(b"same"));
        let mut collector = Collector::new(&project);
        assert_eq!(collector.collect_file(&source).unwrap(), existing);
        let generic = collector.collect_asset_file(&source).unwrap();
        assert_eq!(generic, dir.join("Set Samples/tone.wave"));
        assert_eq!(collector.placed(&source), Some(existing));
        assert_eq!(collector.placed_asset(&source), Some(generic));
    }

    #[test]
    fn audio_signatures() {
        let dir = scratch("audio-magic");
        for (name, bytes, ok) in [
            ("a.flac", &b"fLaC\0\0\0\0"[..], true),
            ("a.ogg", b"OggS\0\x02", true),
            ("a.mp3", b"ID3\x04\0\0", true),
            ("b.mp3", &[0xFF, 0xFB, 0x90, 0x00][..], true),
            ("a.aiff", b"FORM\0\0\0\0AIFF", true),
            ("a.aif", b"FORM\0\0\0\0AIFC", true),
            ("a.wav", b"RIFF\0\0\0\0AVI ", false),
            ("a.txt", b"RIFF\0\0\0\0WAVE", false),
            ("empty.wav", b"", false),
        ] {
            assert_eq!(is_audio_file(&file(&dir, name, bytes)), ok, "{name}");
        }
        assert!(!is_audio_file(&dir.join("missing.wav")));
    }
}
