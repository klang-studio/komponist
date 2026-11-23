//! Recording folders and conservative migration of finalized takes. Originals survive every
//! failed copy/save; callers unlink them only after updating the document and its history.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Unsaved takes live in `<music>/komponist/Recordings` (the folder name is language-neutral; the
/// Music folder comes from `crate::dirs::music_dir`). Documents reference takes by absolute path,
/// so takes left in an older `komponist/Aufnahmen` folder keep working and are never moved.
pub(super) fn provisional_root() -> Option<PathBuf> {
    crate::dirs::recordings_dir()
}

pub(super) fn directory(root: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    let name: String = name
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        .take(64)
        .collect();
    let name = if name.trim().is_empty() {
        "komponist"
    } else {
        name.trim()
    };
    let date = crate::dialogs::format::local(SystemTime::now()).map_or_else(
        || "recording".into(),
        |date| format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
    );
    for index in 0..10_000 {
        let suffix = if index == 0 {
            String::new()
        } else {
            format!("-{index}")
        };
        let path = root.join(format!("{name}-{date}{suffix}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "recording folder names exhausted",
    ))
}

/// Creates Takes in an existing project directory. Save must never create the project folder.
fn ensure_takes_folder(target: &Path) -> io::Result<()> {
    match fs::create_dir(target) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && target.is_dir() => Ok(()),
        result => result,
    }
}

/// Copies only explicitly owned finalized takes, never overwriting another recording. A failed
/// transaction deletes its own new copies and leaves all source paths usable.
pub(super) fn stage(paths: &[PathBuf], target: &Path) -> io::Result<BTreeMap<PathBuf, PathBuf>> {
    ensure_takes_folder(target)?;
    let mut copies = BTreeMap::new();
    let result = (|| {
        for source in paths {
            if source.parent() == Some(target) || copies.contains_key(source) {
                continue;
            }
            let stem = source.file_stem().unwrap_or_default().to_string_lossy();
            let extension = source.extension().unwrap_or_default().to_string_lossy();
            let mut placed = false;
            for index in 0..10_000 {
                let suffix = if index == 0 {
                    String::new()
                } else {
                    format!("-{index}")
                };
                let name = if extension.is_empty() {
                    format!("{stem}{suffix}")
                } else {
                    format!("{stem}{suffix}.{extension}")
                };
                let destination = target.join(name);
                let mut output = match OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&destination)
                {
                    Ok(output) => output,
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                };
                copies.insert(source.clone(), destination);
                let mut input = fs::File::open(source)?;
                io::copy(&mut input, &mut output)?;
                output.sync_all()?;
                placed = true;
                break;
            }
            if !placed {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "recording file names exhausted",
                ));
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in copies.values() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    Ok(copies)
}

/// Hidden folders in `Takes` that hold copies a staging worker made but nobody accepted yet.
const STAGING_PREFIX: &str = ".komponist-staging-";

/// Copies of finished takes in a private folder inside `target` (`Takes`), so nothing sees them
/// under a final name before the app accepts them (#221): `(private folder, source → copy)`.
/// [`publish`] gives them their final names. On an error the private folder is gone.
pub(super) fn stage_private(
    paths: &[PathBuf],
    target: &Path,
) -> io::Result<(PathBuf, BTreeMap<PathBuf, PathBuf>)> {
    ensure_takes_folder(target)?;
    let private = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(target)?
        .keep();
    match stage(paths, &private) {
        Ok(copies) => Ok((private, copies)),
        Err(error) => {
            let _ = fs::remove_dir_all(&private);
            Err(error)
        }
    }
}

/// Moves privately staged copies into `target` under final names (`source → final path`), never
/// replacing a file there. On an error, the files published so far are removed again and the
/// remaining private copies stay where they are.
pub(super) fn publish(
    copies: &BTreeMap<PathBuf, PathBuf>,
    target: &Path,
) -> io::Result<BTreeMap<PathBuf, PathBuf>> {
    let mut published = BTreeMap::new();
    let result = (|| {
        for (source, copy) in copies {
            let stem = source.file_stem().unwrap_or_default().to_string_lossy();
            let extension = source.extension().unwrap_or_default().to_string_lossy();
            let mut placed = None;
            for index in 0..10_000 {
                let suffix = if index == 0 {
                    String::new()
                } else {
                    format!("-{index}")
                };
                let name = if extension.is_empty() {
                    format!("{stem}{suffix}")
                } else {
                    format!("{stem}{suffix}.{extension}")
                };
                let destination = target.join(name);
                // A hard link fails instead of replacing an existing file. Filesystems without
                // links get a rename after a check.
                match fs::hard_link(copy, &destination) {
                    Ok(()) => {
                        fs::remove_file(copy)?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(_) if destination.exists() => continue,
                    Err(_) => fs::rename(copy, &destination)?,
                }
                placed = Some(destination);
                break;
            }
            let destination = placed.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "recording file names exhausted",
                )
            })?;
            published.insert(source.clone(), destination);
        }
        Ok(())
    })();
    if let Err(error) = result {
        for path in published.values() {
            let _ = fs::remove_file(path);
        }
        return Err(error);
    }
    Ok(published)
}

/// Deletes private staging folders that a staging worker left in `target` (`Takes`) when the
/// app ended before it could clean up (#221).
pub(crate) fn reclaim_abandoned(target: &Path) {
    let Ok(entries) = fs::read_dir(target) else {
        return;
    };
    for entry in entries.flatten() {
        let abandoned = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX));
        if abandoned && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// A link may reach an owned source via a symlink rather than the writer's original spelling.
pub(super) fn canonical_copies(copies: &BTreeMap<PathBuf, PathBuf>) -> BTreeMap<PathBuf, PathBuf> {
    copies
        .iter()
        .filter_map(|(source, copy)| Some((fs::canonicalize(source).ok()?, copy.clone())))
        .collect()
}

/// Saves independent checkpoint assets. The capture worker continues appending to its own
/// files; Esc may delete those files without invalidating an already saved project. Rebuilding
/// the header and copying exactly the flushed prefix avoids saving a concurrently changing
/// RIFF length. Short cycle prefixes receive the same silent tail as finalized takes.
pub(super) fn checkpoint(
    takes: &[komponist_engine::recording::RecordedTake],
    target: &Path,
) -> io::Result<Vec<komponist_engine::recording::RecordedTake>> {
    ensure_takes_folder(target)?;
    let mut created = Vec::new();
    let result = (|| {
        let mut snapshots = Vec::new();
        for take in takes.iter().filter(|take| take.frames > 0) {
            let mut reader = hound::WavReader::open(&take.path).map_err(io::Error::other)?;
            let spec = reader.spec();
            if spec.channels != take.channels || spec.sample_rate != take.sample_rate {
                return Err(io::Error::other("recording checkpoint format changed"));
            }
            let frames = take.window_frames.unwrap_or(take.frames).max(take.frames);
            let bytes = frames
                .checked_mul(u64::from(spec.channels))
                .and_then(|n| n.checked_mul(u64::from(spec.bits_per_sample.div_ceil(8))))
                .ok_or_else(|| io::Error::other("checkpoint size overflow"))?;
            if bytes > u64::from(u32::MAX) - 128 {
                return Err(io::Error::other("checkpoint exceeds WAV size limit"));
            }
            let stem = take.path.file_stem().unwrap_or_default().to_string_lossy();
            let mut destination = None;
            for index in 0..10_000 {
                let path = target.join(format!("checkpoint-{stem}-{index}.wav"));
                match OpenOptions::new().create_new(true).write(true).open(&path) {
                    Ok(file) => {
                        destination = Some((path, file));
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
            let (path, file) =
                destination.ok_or_else(|| io::Error::other("checkpoint names exhausted"))?;
            created.push(path.clone());
            let mut writer = hound::WavWriter::new(std::io::BufWriter::new(file), spec)
                .map_err(io::Error::other)?;
            let samples = take.frames * u64::from(spec.channels);
            match spec.sample_format {
                hound::SampleFormat::Int => {
                    let mut input = reader.samples::<i32>();
                    for _ in 0..samples {
                        let value = input
                            .next()
                            .ok_or_else(|| io::Error::other("short recording checkpoint"))?
                            .map_err(io::Error::other)?;
                        writer.write_sample(value).map_err(io::Error::other)?;
                    }
                    for _ in samples..frames * u64::from(spec.channels) {
                        writer.write_sample(0_i32).map_err(io::Error::other)?;
                    }
                }
                hound::SampleFormat::Float => {
                    let mut input = reader.samples::<f32>();
                    for _ in 0..samples {
                        let value = input
                            .next()
                            .ok_or_else(|| io::Error::other("short recording checkpoint"))?
                            .map_err(io::Error::other)?;
                        writer.write_sample(value).map_err(io::Error::other)?;
                    }
                    for _ in samples..frames * u64::from(spec.channels) {
                        writer.write_sample(0.0_f32).map_err(io::Error::other)?;
                    }
                }
            }
            writer.finalize().map_err(io::Error::other)?;
            let mut snapshot = take.clone();
            snapshot.path = path;
            snapshot.frames = frames;
            snapshots.push(snapshot);
        }
        Ok(snapshots)
    })();
    if result.is_err() {
        for path in created {
            let _ = fs::remove_file(path);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komponist-recording-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
    #[test]
    fn migration_never_overwrites_existing_takes_and_keeps_originals_until_commit() {
        let root = scratch();
        let source = root.join("Take.wav");
        let target = root.join("Takes");
        fs::create_dir_all(&target).unwrap();
        fs::write(&source, b"recording").unwrap();
        fs::write(target.join("Take.wav"), b"previous recording").unwrap();
        let copies = stage(std::slice::from_ref(&source), &target).unwrap();
        assert_eq!(fs::read(&source).unwrap(), b"recording");
        assert_eq!(
            fs::read(target.join("Take.wav")).unwrap(),
            b"previous recording"
        );
        assert_eq!(fs::read(&copies[&source]).unwrap(), b"recording");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn failed_copy_rolls_back_only_new_files_and_preserves_sources() {
        let root = scratch();
        let source = root.join("Take.wav");
        let target = root.join("Takes");
        fs::write(&source, b"recording").unwrap();
        assert!(stage(&[source.clone(), root.join("missing.wav")], &target).is_err());
        assert!(source.is_file());
        assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn private_copies_get_final_names_only_when_published() {
        let root = scratch();
        let source = root.join("Take.wav");
        let target = root.join("Takes");
        fs::create_dir_all(&target).unwrap();
        fs::write(&source, b"recording").unwrap();
        fs::write(target.join("Take.wav"), b"previous recording").unwrap();
        let (private, copies) = stage_private(std::slice::from_ref(&source), &target).unwrap();
        assert!(private.starts_with(&target));
        assert!(!target.join("Take-1.wav").exists(), "no final name yet");
        let published = publish(&copies, &target).unwrap();
        assert_eq!(published[&source], target.join("Take-1.wav"));
        assert_eq!(fs::read(target.join("Take-1.wav")).unwrap(), b"recording");
        assert_eq!(
            fs::read(target.join("Take.wav")).unwrap(),
            b"previous recording"
        );
        // Left behind by an app that ended: reclaimed, published files stay.
        fs::write(private.join("Take.wav"), b"abandoned").unwrap();
        reclaim_abandoned(&target);
        assert!(!private.exists());
        assert!(target.join("Take-1.wav").is_file());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn recording_directories_are_unique_and_cannot_escape_the_root() {
        let root = scratch();
        let first = directory(&root, "../../Song/").unwrap();
        let second = directory(&root, "../../Song/").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.parent(), Some(root.as_path()));
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn checkpoint_survives_discard_of_the_live_file_and_pads_a_partial_loop() {
        let root = scratch();
        let path = root.join("live.wav");
        let mut writer = hound::WavWriter::create(
            &path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 24,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for _ in 0..64 {
            writer.write_sample(123_i32).unwrap();
        }
        writer.flush().unwrap();
        let take = komponist_engine::recording::RecordedTake {
            track: komponist_model::TrackId(1),
            path: path.clone(),
            frames: 64,
            window_frames: Some(128),
            window_end: None,
            sample_rate: 48_000,
            channels: 1,
            start: komponist_model::Tick::ZERO,
            lap: 0,
        };
        let snapshots = checkpoint(&[take], &root.join("Takes")).unwrap();
        writer.finalize().unwrap();
        fs::remove_file(path).unwrap();
        let mut reader = hound::WavReader::open(&snapshots[0].path).unwrap();
        let samples: Vec<_> = reader.samples::<i32>().map(Result::unwrap).collect();
        assert_eq!(samples.len(), 128);
        assert!(samples[..64].iter().all(|sample| *sample == 123));
        assert!(samples[64..].iter().all(|sample| *sample == 0));
        fs::remove_dir_all(root).unwrap();
    }
}
