//! Crash recovery (#227; crash-recovery proposal, internal notes, §2): the **recovery copy**
//! ("Sicherungskopie") of the open document, written in the background while it has unsaved
//! changes, and offered at the next start after an abnormal end. No egui in here: the prompt is
//! `dialogs::recovery`, the app's side `app::recovery`.
//!
//! - **Where** ([`default_root`]): a per-user state folder, `$XDG_STATE_HOME/komponist/recovery`
//!   (else `~/.local/state/komponist/recovery`) on Linux, `~/Library/Application Support/komponist/Recovery`
//!   on macOS, `%LOCALAPPDATA%\komponist\Recovery` on Windows. Not the cache folder (cleaners empty
//!   it), not next to the project (an untitled project has no folder).
//! - **What**: per open document `<id>.komponist` (the project, `persist::to_json`, the format of a
//!   project file, loaded through the same `persist::load` with its validation), `<id>.json`
//!   ([`RecoveryMeta`]) and `<id>.lock`, with a random 64-bit [`Id`] per document and session. The
//!   session holds an advisory lock on the `.lock` file (`File::try_lock`: `flock` on Unix,
//!   `LockFileEx` on Windows) while the document is open, so another komponist never offers the copy
//!   of a project that is still open ([`Store::scan`] only returns entries whose lock it can take).
//! - **When** ([`Schedule`]): [`QUIET`] (2 s) after the last change, but at least every [`MAX_AGE`]
//!   (30 s) while changes go on; never while the document is clean. A clean document (saved, or
//!   undone back to the saved state) deletes the copy; so do closing the document and quitting.
//! - **How** ([`Writer`]): the UI thread hands the project (an `Arc`, no copy) and the deferred
//!   plugin states to a writer thread through a latest-wins slot: a newer project replaces a queued
//!   one. Creating the entry, deleting and closing queue in order before it. The writer thread owns
//!   the entry; it writes the meta and then the copy ([`write_entry`], one generation: a copy on
//!   disk always has its document's meta) with `komponist_session::project_io::write_atomic` (staging file, fsync,
//!   rename, fsync of the folder). The UI thread never touches the folder for a copy, and the
//!   realtime threads never do any file IO, also not while panicking. A failed write is tried again
//!   after 2 s, 4 s … up to 30 s while the document stays dirty, and reported once per session.
//! - **Private**: the folder is `0700` and its files `0600` on Unix (an existing folder is narrowed
//!   by the next scan); on Windows the per-user Local AppData keeps others out.
//! - **Panic hook** ([`install_panic_hook`], chained to the previous hook): on the UI thread it takes
//!   the last published document with `try_lock` (it never blocks on it), writes the copy itself
//!   with `ended_by = panic` and seals the store, so the writer thread cannot put older work over it;
//!   if the writer holds the folder longer than 2 s, the previous complete copy stays. On any other
//!   thread it only queues `ended_by` for the writer thread, which rewrites the meta. Signals (a
//!   crash inside an in-process plugin, a kill, a power loss) run no hook: the periodic copy covers
//!   them.
//! - Discarded copies move to `discarded/` as a pair, with the time of the discard in their names,
//!   and are deleted [`KEEP_DISCARDED`] (7 days) later, without UI.
//!
//! Links (samples, audio files) inside the copy stay relative to the original project folder; the
//! app opens a recovered copy with the original path, so they resolve as before. Provisional
//! recordings a copy refers to are never removed by this module (discarding removes nothing but the
//! copy), and a session only ever removes recordings it made itself.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError, mpsc};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use komponist_model::persist;
use komponist_model::{Project, Repairs};

use crate::param_undo::{DeferredPluginEdit, replay_deferred_plugins};
use komponist_session::project_io::{ProjectIoError, write_atomic};

/// Version of [`RecoveryMeta`]; a meta with a higher version is treated as unreadable.
pub const META_VERSION: u32 = 1;

/// Quiet time after the last change before the copy is written (owner decision Q2).
pub const QUIET: Duration = Duration::from_secs(2);

/// Most time a change waits for the copy while changes go on (owner decision Q2).
pub const MAX_AGE: Duration = Duration::from_secs(30);

/// How long discarded copies are kept in `discarded/` (owner decision Q6).
pub const KEEP_DISCARDED: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The folder of discarded copies inside the recovery folder.
pub const DISCARDED: &str = "discarded";

const COPY_EXTENSION: &str = persist::FILE_EXTENSION;
const META_EXTENSION: &str = "json";
const LOCK_EXTENSION: &str = "lock";
const NEXT_META_EXTENSION: &str = "json.next";

/// How long the panic hook waits for the writer thread to finish a write in progress before it
/// writes the newest copy over it.
const HOOK_IO_WAIT: Duration = Duration::from_secs(2);

/// The recovery folder of this user (module docs), `None` without a home folder.
pub fn default_root() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::dirs::local_app_data().map(|d| d.join("komponist").join("Recovery"))
    }
    #[cfg(not(windows))]
    {
        let xdg = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        let home = crate::dirs::home_dir();
        root_from(cfg!(target_os = "macos"), xdg.as_deref(), home.as_deref())
    }
}

/// [`default_root`] for given inputs (Linux and macOS); `macos` picks the platform's convention.
#[cfg(not(windows))]
fn root_from(macos: bool, xdg_state: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let home = home.filter(|p| p.is_absolute());
    if macos {
        return Some(home?.join("Library/Application Support/komponist/Recovery"));
    }
    let state = xdg_state
        .filter(|p| p.is_absolute())
        .map(Path::to_path_buf)
        .or_else(|| home.map(|h| h.join(".local/state")))?;
    Some(state.join("komponist").join("recovery"))
}

// ------------------------------------------------------------------------------------------------
// Ids, paths, meta
// ------------------------------------------------------------------------------------------------

/// The id of one document's recovery entry: 16 lower-case hex digits (64 random bits).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(String);

impl Id {
    /// A new random id. `RandomState` is seeded from the system's random source once per process
    /// and per thread; the time and a counter make every call different.
    pub fn random() -> Id {
        use std::hash::{BuildHasher, Hasher};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u32(std::process::id());
        hasher.write_u128(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        Id(format!("{:016x}", hasher.finish()))
    }

    /// The id of a file stem, if it is one.
    pub fn parse(stem: &str) -> Option<Id> {
        (stem.len() == 16 && stem.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Id(stem.to_owned()))
    }

    /// The 16 hex digits.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The three files of an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPaths {
    /// `<id>.komponist`: the project.
    pub copy: PathBuf,
    /// `<id>.json`: the [`RecoveryMeta`].
    pub meta: PathBuf,
    /// `<id>.json.next`: the meta of a generation being written ([`write_entry`]).
    pub next_meta: PathBuf,
    /// `<id>.lock`: held while the document is open.
    pub lock: PathBuf,
}

impl EntryPaths {
    fn new(root: &Path, id: &Id) -> EntryPaths {
        let file = |extension: &str| root.join(format!("{id}.{extension}"));
        EntryPaths {
            copy: file(COPY_EXTENSION),
            meta: file(META_EXTENSION),
            next_meta: file(NEXT_META_EXTENSION),
            lock: file(LOCK_EXTENSION),
        }
    }
}

/// The modification time and length of a project file: whether it changed since the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    /// Modification time in nanoseconds since the Unix epoch (0 if the system has none).
    pub modified_ns: u64,
    /// Length in bytes.
    pub len: u64,
}

impl Fingerprint {
    /// The fingerprint of the file at `path` now, `None` if there is none.
    pub fn of(path: &Path) -> Option<Fingerprint> {
        let meta = fs::metadata(path).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(Fingerprint {
            modified_ns: meta.modified().ok().map_or(0, unix_ns),
            len: meta.len(),
        })
    }

    /// The modification time.
    pub fn modified(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_nanos(self.modified_ns)
    }
}

fn unix_ns(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn unix_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// How the session that wrote a copy ended, as far as it could tell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EndedBy {
    /// Nothing ran at the end: a crash by signal, a kill, a power loss, or a session still open.
    #[default]
    Unknown,
    /// A Rust panic; message and location are for the log (#240), the prompt does not show them
    /// (owner decision Q9).
    Panic {
        /// The panic message.
        message: String,
        /// `file:line:column`.
        location: String,
    },
}

/// The sidecar `<id>.json` of a recovery copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryMeta {
    /// [`META_VERSION`] when written.
    pub version: u32,
    /// The project file the document belongs to; `None`: never saved.
    pub project_path: Option<PathBuf>,
    /// The project name.
    pub name: String,
    /// When the copy was written, in milliseconds since the Unix epoch.
    pub written_ms: u64,
    /// The project file's fingerprint when the copy was written (`None`: there was no file).
    pub file: Option<Fingerprint>,
    /// The komponist version that wrote the copy.
    pub app_version: String,
    /// How the session ended ([`EndedBy::Unknown`] until a panic hook says otherwise).
    #[serde(default)]
    pub ended_by: EndedBy,
    /// The copy this meta belongs to: its length and checksum (one generation, [`write_entry`]).
    #[serde(default)]
    pub copy: Option<CopyCheck>,
}

/// Length and FNV-1a 64 checksum of a copy's bytes: which meta belongs to which copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyCheck {
    /// Length in bytes.
    pub len: u64,
    /// FNV-1a 64 of the bytes, as 16 hex digits (stable across builds, unlike `std`'s hasher).
    pub fnv: String,
}

impl CopyCheck {
    /// The check of `bytes`.
    pub fn of(bytes: &[u8]) -> CopyCheck {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        CopyCheck {
            len: bytes.len() as u64,
            fnv: format!("{hash:016x}"),
        }
    }
}

impl RecoveryMeta {
    /// The meta of a copy of `base` written at `now`, with the project file's fingerprint as it is
    /// now.
    fn at(base: &MetaBase, now: SystemTime) -> RecoveryMeta {
        RecoveryMeta {
            version: META_VERSION,
            project_path: base.project_path.clone(),
            name: base.name.clone(),
            written_ms: unix_ms(now),
            file: base.project_path.as_deref().and_then(Fingerprint::of),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            ended_by: EndedBy::Unknown,
            copy: None,
        }
    }

    /// When the meta was written.
    pub fn written(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_millis(self.written_ms)
    }

    fn read(path: &Path) -> Option<RecoveryMeta> {
        let text = fs::read_to_string(path).ok()?;
        let meta: RecoveryMeta = serde_json::from_str(&text).ok()?;
        (meta.version <= META_VERSION).then_some(meta)
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        write_published(path, &text)
    }
}

/// What the document says about itself in the meta: path and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaBase {
    /// The project file (`None`: untitled).
    pub project_path: Option<PathBuf>,
    /// The project name.
    pub name: String,
}

/// Creates `dir` (with its parents) and makes it private to the user.
fn ensure_private(dir: &Path) -> io::Result<()> {
    // Durably: `write_atomic` into it does not create folders, and a new store must survive a
    // power loss on file systems that need the parent flushed.
    komponist_session::project_io::create_dirs_durably(dir)?;
    restrict(dir)
}

/// Unix: the recovery folders are `0700`, so no other local user can read a copy (they hold the
/// user's unsaved music); an existing folder with wider permissions is narrowed. Windows: the
/// per-user Local AppData folder's ACL already keeps others out.
#[cfg(unix)]
fn restrict(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_: &Path) -> io::Result<()> {
    Ok(())
}

/// A file of an entry: `0600` on Unix (inside a `0700` folder from the start, so it is never
/// readable by others in between).
#[cfg(unix)]
fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Narrows a file an older build left readable by others to `0600` (Unix).
#[cfg(unix)]
fn narrow_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let meta = fs::symlink_metadata(path)?;
    if meta.is_file() && meta.permissions().mode() & 0o077 != 0 {
        restrict_file(path)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn narrow_file(_: &Path) -> io::Result<()> {
    Ok(())
}

/// `write_atomic` into the existing recovery folder, owner-only; a failed folder flush after the
/// rename only goes to the log (the file is in place).
fn write_published(path: &Path, text: &str) -> io::Result<()> {
    if let Some(error) = write_atomic(path, text, false)? {
        eprintln!(
            "komponist: {}: not flushed to disk: {error}",
            path.display()
        );
    }
    restrict_file(path)
}

/// Writes one generation of an entry as a committed pair: the new meta (with the copy's
/// [`CopyCheck`]) goes to `<id>.json.next`, then the copy is published, then the meta is renamed
/// over `<id>.json`. An interruption anywhere leaves a pair the scan can match by the check: the
/// previous copy with `<id>.json`, or (after the copy, before the rename) the new copy with
/// `<id>.json.next` ([`Store::scan`]). A copy is never read with the meta of another generation, so
/// its link base (the original path) is always its own, also after Save As moved the document.
fn write_entry(paths: &EntryPaths, project: &Project, meta: &RecoveryMeta) -> io::Result<()> {
    let json = persist::to_json(project).map_err(io::Error::other)?;
    let mut meta = meta.clone();
    meta.copy = Some(CopyCheck::of(json.as_bytes()));
    // A pending meta of the copy on disk (its commit failed last time) is that copy's only meta:
    // commit it before the new generation's meta takes its name, or write nothing.
    if let Ok(bytes) = fs::read(&paths.copy) {
        let check = CopyCheck::of(&bytes);
        if RecoveryMeta::read(&paths.next_meta).is_some_and(|m| m.copy.as_ref() == Some(&check)) {
            commit_meta(paths)?;
        }
    }
    meta.write(&paths.next_meta)?;
    write_published(&paths.copy, &json)?;
    commit_meta(paths)
}

/// The last step of [`write_entry`]: `<id>.json.next` becomes `<id>.json`.
fn commit_meta(paths: &EntryPaths) -> io::Result<()> {
    fs::rename(&paths.next_meta, &paths.meta)
}

/// Deletes the copy, then its metas (the reverse of [`write_entry`]). If the copy cannot be
/// deleted, its meta stays too (a copy without its meta would lose its link base) and the error
/// comes back; a file that is not there counts as deleted.
fn delete_entry(paths: &EntryPaths) -> io::Result<()> {
    remove_if_present(&paths.copy)?;
    remove_if_present(&paths.next_meta)?;
    remove_if_present(&paths.meta)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Renames `from` to `to`; `Ok(false)` if `from` is not there.
fn rename_if_present(from: &Path, to: &Path) -> io::Result<bool> {
    match fs::rename(from, to) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

// ------------------------------------------------------------------------------------------------
// The store: entries, locks, scan, discard
// ------------------------------------------------------------------------------------------------

/// The recovery folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Store {
    root: PathBuf,
}

/// An entry this session owns: its id, files and the held lock.
#[derive(Debug)]
pub struct Entry {
    /// The id.
    pub id: Id,
    /// Its files.
    pub paths: EntryPaths,
    lock: File,
}

/// Opens (creating it, owner-only) and locks `path`: `Ok(Some(file))` when the lock is ours,
/// `Ok(None)` when another process (or another handle of this one) holds it.
fn take_lock(path: &Path) -> io::Result<Option<File>> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// When a file in `discarded/` was discarded: the stamp in its name (`<id>-<unix ms>.<ext>`), which
/// is there before the file is (a rename), else its modification time.
fn discarded_at(entry: &fs::DirEntry) -> Option<SystemTime> {
    let path = entry.path();
    let stamp = path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.rsplit_once('-'))
        .and_then(|(_, ms)| ms.parse::<u64>().ok());
    match stamp {
        Some(ms) => Some(UNIX_EPOCH + Duration::from_millis(ms)),
        None => entry.metadata().and_then(|m| m.modified()).ok(),
    }
}

impl Store {
    /// The store in folder `root` (created, owner-only, when the first entry is).
    pub fn new(root: impl Into<PathBuf>) -> Store {
        Store { root: root.into() }
    }

    /// The folder.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The files of entry `id`.
    pub fn paths(&self, id: &Id) -> EntryPaths {
        EntryPaths::new(&self.root, id)
    }

    /// A new entry with a fresh id and its lock taken (no copy yet). Runs on the writer thread.
    pub fn create_entry(&self) -> io::Result<Entry> {
        ensure_private(&self.root)?;
        for _ in 0..8 {
            let id = Id::random();
            let paths = self.paths(&id);
            if paths.copy.exists() || paths.meta.exists() || paths.lock.exists() {
                continue;
            }
            if let Some(lock) = take_lock(&paths.lock)? {
                return Ok(Entry { id, paths, lock });
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free recovery id",
        ))
    }

    /// The copies other sessions left behind, newest first: every entry with a copy whose lock
    /// this call can take (the locks stay held by the candidates). Entries locked by a live session
    /// are skipped; a meta or lock without its copy (a session that ended clean, or a crash before
    /// its first copy) is removed; discarded copies older than [`KEEP_DISCARDED`] are deleted. A
    /// missing folder is an empty store; any other failure to read it comes back with the
    /// candidates that could be read.
    pub fn scan(&self) -> (Vec<Candidate>, Option<io::Error>) {
        let dir = match fs::read_dir(&self.root) {
            Ok(dir) => dir,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return (Vec::new(), None),
            Err(error) => return (Vec::new(), Some(error)),
        };
        // Stores of older builds may be readable by others.
        if let Err(error) = restrict(&self.root) {
            eprintln!("komponist: recovery: {}: {error}", self.root.display());
        }
        self.prune_discarded(SystemTime::now());
        let mut failure = None;
        let mut ids = Vec::new();
        for entry in dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    failure.get_or_insert(error);
                    continue;
                }
            };
            let path = entry.path();
            // Files of older builds may be readable by others.
            if let Err(error) = narrow_file(&path) {
                eprintln!("komponist: recovery: {}: {error}", path.display());
            }
            let id = path
                .extension()
                .and_then(|e| e.to_str())
                .filter(|e| [COPY_EXTENSION, META_EXTENSION, LOCK_EXTENSION].contains(e))
                .and_then(|_| Id::parse(path.file_stem()?.to_str()?));
            ids.extend(id);
        }
        ids.sort();
        ids.dedup();
        let mut found = Vec::new();
        for id in ids {
            match self.candidate(id) {
                Ok(Some(candidate)) => found.push(candidate),
                Ok(None) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        found.sort_by_key(|c| std::cmp::Reverse(c.written()));
        (found, failure)
    }

    fn candidate(&self, id: Id) -> io::Result<Option<Candidate>> {
        let paths = self.paths(&id);
        let Some(lock) = take_lock(&paths.lock)? else {
            return Ok(None);
        };
        let bytes = match fs::read(&paths.copy) {
            Ok(bytes) => Some(Ok(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => Some(Err(error)),
        };
        let Some(bytes) = bytes else {
            // Nothing to recover: a session that ended clean, or one that crashed before its
            // first copy.
            remove_if_present(&paths.next_meta)?;
            remove_if_present(&paths.meta)?;
            drop(lock);
            let _ = fs::remove_file(&paths.lock);
            return Ok(None);
        };
        let meta = bytes
            .as_ref()
            .ok()
            .and_then(|bytes| pair_meta(&paths, bytes));
        let loaded = match bytes {
            Ok(bytes) => String::from_utf8(bytes)
                .map_err(|_| {
                    ProjectIoError::Format(paths.copy.clone(), persist::PersistError::NotAProject)
                })
                .and_then(|text| {
                    persist::load(&text)
                        .map(|loaded| (Arc::new(loaded.project), loaded.repairs))
                        .map_err(|e| ProjectIoError::Format(paths.copy.clone(), e))
                }),
            Err(error) => Err(ProjectIoError::Io(paths.copy.clone(), error)),
        };
        let written = fs::metadata(&paths.copy)
            .and_then(|m| m.modified())
            .ok()
            .or_else(|| meta.as_ref().map(RecoveryMeta::written))
            .unwrap_or(UNIX_EPOCH);
        Ok(Some(Candidate {
            id,
            paths,
            meta,
            loaded,
            written,
            lock: Some(lock),
        }))
    }

    /// Moves `candidate`'s copy and meta to `discarded/` as one pair (kept [`KEEP_DISCARDED`],
    /// counted from now: the time is in their new names). If either move fails, the pair stays where
    /// it was and the error comes back.
    pub fn discard(&self, candidate: &Candidate) -> io::Result<()> {
        let folder = self.root.join(DISCARDED);
        ensure_private(&folder)?;
        let stamp = unix_ms(SystemTime::now());
        let target = |extension: &str| folder.join(format!("{}-{stamp}.{extension}", candidate.id));
        discard_to(
            &candidate.paths,
            &target(COPY_EXTENSION),
            &target(META_EXTENSION),
        )
    }

    /// Deletes the discarded copies older than [`KEEP_DISCARDED`] at `now`.
    pub fn prune_discarded(&self, now: SystemTime) {
        let folder = self.root.join(DISCARDED);
        let Ok(dir) = fs::read_dir(&folder) else {
            return;
        };
        if let Err(error) = restrict(&folder) {
            eprintln!("komponist: recovery: {}: {error}", folder.display());
        }
        for entry in dir.filter_map(Result::ok) {
            if let Err(error) = narrow_file(&entry.path()) {
                eprintln!("komponist: recovery: {}: {error}", entry.path().display());
            }
            let old = discarded_at(&entry)
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|age| age > KEEP_DISCARDED);
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    /// Takes over `candidate` as this session's entry (after „Wiederherstellen“): same id, lock
    /// held, copy in place.
    pub fn adopt(candidate: Candidate) -> Option<Entry> {
        let mut candidate = candidate;
        let lock = candidate.lock.take()?;
        Some(Entry {
            id: candidate.id.clone(),
            paths: candidate.paths.clone(),
            lock,
        })
    }
}

/// The meta of the copy `bytes`: the committed `<id>.json` or, after an interruption between the
/// copy and the meta's rename, `<id>.json.next`, whichever was written for these bytes
/// ([`write_entry`]). A meta of an older build without a check pairs as before. A pending meta of
/// another generation is left over from an interrupted write and goes.
fn pair_meta(paths: &EntryPaths, bytes: &[u8]) -> Option<RecoveryMeta> {
    let check = CopyCheck::of(bytes);
    let matches = |meta: &RecoveryMeta| meta.copy.as_ref().is_none_or(|c| *c == check);
    let next = RecoveryMeta::read(&paths.next_meta);
    if let Some(next) = next.filter(|m| m.copy.as_ref() == Some(&check)) {
        // The copy of the interrupted write made it: finish the commit.
        if let Err(error) = commit_meta(paths) {
            eprintln!("komponist: recovery: {}: {error}", paths.meta.display());
        }
        return Some(next);
    }
    let _ = remove_if_present(&paths.next_meta);
    RecoveryMeta::read(&paths.meta).filter(matches)
}

/// Moves copy and meta of `paths` to `copy_to` and `meta_to`; rolls the copy back if the meta
/// cannot follow.
fn discard_to(paths: &EntryPaths, copy_to: &Path, meta_to: &Path) -> io::Result<()> {
    discard_with(paths, copy_to, meta_to, |from, to| fs::rename(from, to))
}

/// [`discard_to`] with the rename the copy is rolled back with (tests make it fail).
///
/// If the meta cannot follow and the copy cannot go back either, the discard has happened for the
/// copy (it is in `discarded/`, as the user asked): the meta left behind is deleted, so no later
/// scan pairs it with anything, and the discard counts as done. A meta that cannot be deleted
/// either is a leftover without a copy, which the next scan removes ([`Store::scan`]); it is never
/// offered as a copy.
fn discard_with(
    paths: &EntryPaths,
    copy_to: &Path,
    meta_to: &Path,
    roll_back: impl Fn(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    // Nothing changes before the copy has moved: a failed move leaves the pair as it was, also a
    // pending meta the scan could not commit ([`pair_meta`]).
    let moved = rename_if_present(&paths.copy, copy_to)?;
    // The meta that belongs to the copy: the pending one if it was written for these bytes (its
    // commit failed), else the committed one. The other one is stale.
    let pending = moved
        && fs::read(copy_to).is_ok_and(|bytes| {
            let check = CopyCheck::of(&bytes);
            RecoveryMeta::read(&paths.next_meta).is_some_and(|m| m.copy.as_ref() == Some(&check))
        });
    let (own, stale) = if pending {
        (&paths.next_meta, &paths.meta)
    } else {
        (&paths.meta, &paths.next_meta)
    };
    let Err(error) = rename_if_present(own, meta_to) else {
        if let Err(error) = remove_if_present(stale) {
            eprintln!("komponist: recovery: {}: {error}", stale.display());
        }
        return Ok(());
    };
    if !moved {
        return Err(error);
    }
    match roll_back(copy_to, &paths.copy) {
        // Back in place: still a candidate, with its meta.
        Ok(()) => Err(error),
        Err(back) => {
            eprintln!(
                "komponist: recovery: {} not moved back ({back}); it stays discarded",
                copy_to.display()
            );
            for lone in [&paths.next_meta, &paths.meta] {
                if let Err(error) = remove_if_present(lone) {
                    eprintln!("komponist: recovery: {}: {error}", lone.display());
                }
            }
            Ok(())
        }
    }
}

/// The state of a candidate's project file, compared with the copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// The project was never saved (variant b).
    Never,
    /// The file is there; `changed` when it was saved after the copy (variant c: the
    /// fingerprint differs from the one in the meta).
    Saved {
        /// The file's modification time.
        modified: SystemTime,
        /// Written after the copy.
        changed: bool,
    },
    /// The file is gone (variant d).
    Gone,
}

/// A copy another session left behind ([`Store::scan`]); holds its lock until dropped.
#[derive(Debug)]
pub struct Candidate {
    /// The id.
    pub id: Id,
    /// Its files.
    pub paths: EntryPaths,
    /// The meta (`None`: missing or unreadable).
    pub meta: Option<RecoveryMeta>,
    /// The copy as loaded (variant e when it fails).
    pub loaded: Result<(Arc<Project>, Repairs), ProjectIoError>,
    written: SystemTime,
    lock: Option<File>,
}

impl Candidate {
    /// When the copy was written (its modification time).
    pub fn written(&self) -> SystemTime {
        self.written
    }

    /// The project file, if the project had one.
    pub fn project_path(&self) -> Option<&Path> {
        self.meta.as_ref()?.project_path.as_deref()
    }

    /// The project name: the meta's, else the copy's; `None` when neither can be read.
    pub fn name(&self) -> Option<&str> {
        self.meta
            .as_ref()
            .map(|m| m.name.as_str())
            .or_else(|| self.loaded.as_ref().ok().map(|(p, _)| p.name.as_str()))
    }

    /// The project, if the copy could be read.
    pub fn project(&self) -> Option<&Arc<Project>> {
        self.loaded.as_ref().ok().map(|(p, _)| p)
    }

    /// The project file now, compared with the copy.
    pub fn file_state(&self) -> FileState {
        let Some(path) = self.project_path() else {
            return FileState::Never;
        };
        match Fingerprint::of(path) {
            None => FileState::Gone,
            Some(now) => FileState::Saved {
                modified: now.modified(),
                changed: self.meta.as_ref().and_then(|m| m.file) != Some(now),
            },
        }
    }
}

// ------------------------------------------------------------------------------------------------
// When to write
// ------------------------------------------------------------------------------------------------

/// The debounce of the writes (owner decision Q2), on a clock the caller passes in: due [`QUIET`]
/// after the last change, or [`MAX_AGE`] after the copy began to fall behind: the first change it
/// does not hold, or, while changes go on without a quiet [`QUIET`], the last submission (so
/// continuous edits get a copy at 30 s, 60 s, 90 s …).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Schedule {
    first: Option<Instant>,
    last: Option<Instant>,
    submitted: Option<Instant>,
}

impl Schedule {
    /// The document changed at `now`.
    pub fn changed(&mut self, now: Instant) {
        if self.first.is_none() {
            let continuous = self
                .submitted
                .filter(|at| now.saturating_duration_since(*at) < QUIET);
            self.first = Some(continuous.unwrap_or(now));
        }
        self.last = Some(now);
    }

    /// When the copy is due, `None` with nothing to write.
    pub fn due_at(&self) -> Option<Instant> {
        Some((self.last? + QUIET).min(self.first? + MAX_AGE))
    }

    /// Whether the copy is due at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.due_at().is_some_and(|at| now >= at)
    }

    /// The copy was handed to the writer at `now`: nothing pending.
    pub fn submitted(&mut self, now: Instant) {
        *self = Schedule {
            submitted: Some(now),
            ..Schedule::default()
        };
    }

    /// The document is clean or another one: nothing pending, nothing submitted.
    pub fn reset(&mut self) {
        *self = Schedule::default();
    }
}

// ------------------------------------------------------------------------------------------------
// The writer thread
// ------------------------------------------------------------------------------------------------

/// Deferred plugin states: newer than the document's, put into the copy (as a save does).
pub type Deferred = Vec<DeferredPluginEdit>;

/// What the UI thread publishes of the open document: for the writer and for the panic hook.
#[derive(Clone)]
struct Published {
    /// The document (its generation, [`Recovery::document_replaced`]).
    generation: u64,
    project: Arc<Project>,
    deferred: Deferred,
    base: MetaBase,
}

impl Published {
    /// The project as the copy holds it: with the deferred plugin states.
    fn project(&self) -> Arc<Project> {
        with_deferred(&self.project, &self.deferred)
    }
}

/// What the panic hook needs: the entry and the last published document, and the meta of the last
/// copy on disk. Shared by the UI thread, the writer thread and the hook.
#[derive(Default)]
pub struct Shared {
    state: Mutex<HookState>,
    /// Held while a copy is written or deleted; `sealed` once the panic hook has written its copy,
    /// after which the writer thread writes nothing (it could only write older work).
    io: Mutex<Io>,
}

#[derive(Default)]
struct HookState {
    /// The writer's entry and the generation it belongs to.
    entry: Option<(EntryPaths, u64)>,
    published: Option<Published>,
    written: Option<RecoveryMeta>,
}

#[derive(Default)]
struct Io {
    sealed: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn try_lock<T>(mutex: &Mutex<T>) -> Option<MutexGuard<'_, T>> {
    match mutex.try_lock() {
        Ok(guard) => Some(guard),
        Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner()),
        Err(TryLockError::WouldBlock) => None,
    }
}

impl Shared {
    fn publish(&self, published: Published) {
        lock(&self.state).published = Some(published);
    }

    fn unpublish(&self) {
        lock(&self.state).published = None;
    }
}

enum Job {
    /// Create the entry of document `generation` (the document became dirty).
    Prepare { generation: u64 },
    /// Continue a recovered copy's entry for document `generation`.
    Adopt { entry: Entry, generation: u64 },
    /// Write the copy (latest wins); creates the entry first if there is none.
    Write(Published),
    /// The document is clean: delete copy and meta, keep the entry.
    DeleteCopy,
    /// The document closed: delete copy, meta and lock file, release the entry.
    Close,
    /// A panic on a thread that must not write: put `ended_by` into the meta.
    MarkPanic(EndedBy),
}

#[derive(Default)]
struct Slot {
    /// The newest write; a newer one replaces it.
    write: Option<Job>,
    /// The other jobs, in order, before the write.
    queue: VecDeque<Job>,
    busy: bool,
    stop: bool,
}

struct WriterInner {
    store: Store,
    /// Wakes the UI after an outcome, so a failed write is retried in an idle app too.
    waker: Mutex<Option<Waker>>,
    slot: Mutex<Slot>,
    wake: Condvar,
    idle: Condvar,
    shared: Arc<Shared>,
    #[cfg(test)]
    before_write: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// Tests: how many of the next writes fail.
    #[cfg(test)]
    fail_writes: std::sync::atomic::AtomicUsize,
}

/// Wakes the UI thread (`egui::Context::request_repaint`).
pub type Waker = Box<dyn Fn() + Send + Sync>;

/// What a job came to, for document `generation` (the UI ignores older documents).
#[derive(Debug)]
pub enum Outcome {
    /// The copy is on disk.
    Written {
        /// The document.
        generation: u64,
    },
    /// Creating the entry or writing failed; retried with a backoff.
    Failed {
        /// The document.
        generation: u64,
        /// Why.
        error: io::Error,
    },
}

/// The writer thread and its latest-wins slot (module docs). The thread owns this session's entry.
pub struct Writer {
    inner: Arc<WriterInner>,
    thread: Option<JoinHandle<()>>,
    outcomes: mpsc::Receiver<Outcome>,
}

impl Writer {
    /// Starts the thread; an error when the system cannot start one.
    pub fn start(store: Store, shared: Arc<Shared>) -> io::Result<Writer> {
        let inner = Arc::new(WriterInner {
            store,
            waker: Mutex::new(None),
            slot: Mutex::default(),
            wake: Condvar::new(),
            idle: Condvar::new(),
            shared,
            #[cfg(test)]
            before_write: Mutex::new(None),
            #[cfg(test)]
            fail_writes: std::sync::atomic::AtomicUsize::new(0),
        });
        let (send, outcomes) = mpsc::channel();
        let worker = Arc::clone(&inner);
        let thread = std::thread::Builder::new()
            .name("komponist-recovery".to_owned())
            .spawn(move || run(&worker, &send))?;
        Ok(Writer {
            inner,
            thread: Some(thread),
            outcomes,
        })
    }

    fn post(&self, put: impl FnOnce(&mut Slot)) {
        let mut slot = lock(&self.inner.slot);
        put(&mut slot);
        self.inner.wake.notify_one();
    }

    fn queue(&self, job: Job) {
        let drops_write = matches!(job, Job::DeleteCopy | Job::Close);
        self.post(|slot| {
            // A queued write is older than a deletion or a close.
            if drops_write {
                slot.write = None;
            }
            slot.queue.push_back(job);
        });
    }

    fn write(&self, published: Published) {
        self.post(|slot| slot.write = Some(Job::Write(published)));
    }

    /// The outcomes since the last call.
    pub fn outcomes(&self) -> Vec<Outcome> {
        self.outcomes.try_iter().collect()
    }

    /// Waits until everything posted so far is done; `false` after `timeout`.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut slot = lock(&self.inner.slot);
        while slot.busy || slot.write.is_some() || !slot.queue.is_empty() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.thread.is_none() {
                return false;
            }
            slot = self
                .inner
                .idle
                .wait_timeout(slot, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Finishes what is queued and ends the thread.
    pub fn stop(&mut self) {
        self.post(|slot| slot.stop = true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    #[cfg(test)]
    fn set_before_write(&self, hook: impl Fn() + Send + 'static) {
        *lock(&self.inner.before_write) = Some(Box::new(hook));
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The writer thread's own state: the entry and its document.
struct Owned {
    entry: Option<(Entry, u64)>,
}

impl Owned {
    /// The entry of document `generation`, created if there is none.
    fn ensure(&mut self, inner: &WriterInner, generation: u64) -> io::Result<&EntryPaths> {
        if self.entry.as_ref().is_none_or(|(_, g)| *g != generation) {
            self.close(inner);
            let entry = inner.store.create_entry()?;
            lock(&inner.shared.state).entry = Some((entry.paths.clone(), generation));
            self.entry = Some((entry, generation));
        }
        Ok(&self.entry.as_ref().expect("an entry").0.paths)
    }

    fn close(&mut self, inner: &WriterInner) {
        let Some((entry, _)) = self.entry.take() else {
            return;
        };
        {
            let io = lock(&inner.shared.io);
            // A sealed store keeps the panic hook's copy (the session is ending anyway).
            if !io.sealed
                && let Err(error) = delete_entry(&entry.paths)
            {
                eprintln!(
                    "komponist: recovery: {}: {error}",
                    entry.paths.copy.display()
                );
            }
        }
        {
            let mut state = lock(&inner.shared.state);
            state.entry = None;
            state.written = None;
        }
        let Entry { paths, lock, .. } = entry;
        drop(lock);
        let _ = fs::remove_file(&paths.lock);
    }
}

fn run(inner: &WriterInner, outcomes: &mpsc::Sender<Outcome>) {
    let mut owned = Owned { entry: None };
    loop {
        let job = {
            let mut slot = lock(&inner.slot);
            slot.busy = false;
            loop {
                if let Some(job) = slot.queue.pop_front().or_else(|| slot.write.take()) {
                    slot.busy = true;
                    break job;
                }
                inner.idle.notify_all();
                if slot.stop {
                    // Not a close: the files stay for the next start (a crash, a kill in tests).
                    return;
                }
                slot = inner
                    .wake
                    .wait(slot)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let shared = &inner.shared;
        match job {
            Job::Prepare { generation } => {
                if let Err(error) = owned.ensure(inner, generation) {
                    eprintln!(
                        "komponist: recovery: {}: {error}",
                        inner.store.root().display()
                    );
                    let _ = outcomes.send(Outcome::Failed { generation, error });
                    wake(inner);
                }
            }
            Job::Adopt { entry, generation } => {
                owned.close(inner);
                let mut state = lock(&shared.state);
                state.entry = Some((entry.paths.clone(), generation));
                state.written = RecoveryMeta::read(&entry.paths.meta);
                drop(state);
                owned.entry = Some((entry, generation));
            }
            Job::Write(published) => {
                let generation = published.generation;
                let outcome = match write_job(inner, &mut owned, &published) {
                    Ok(()) => Outcome::Written { generation },
                    Err(error) => {
                        eprintln!("komponist: recovery copy not written: {error}");
                        Outcome::Failed { generation, error }
                    }
                };
                let _ = outcomes.send(outcome);
                wake(inner);
            }
            Job::DeleteCopy => {
                if let Some((entry, _)) = &owned.entry {
                    let io = lock(&shared.io);
                    // After the panic hook's copy nothing older may remove it.
                    if !io.sealed {
                        match delete_entry(&entry.paths) {
                            Ok(()) => lock(&shared.state).written = None,
                            Err(error) => eprintln!(
                                "komponist: recovery: {}: {error}",
                                entry.paths.copy.display()
                            ),
                        }
                    }
                }
            }
            Job::Close => owned.close(inner),
            Job::MarkPanic(ended_by) => {
                let (entry, meta) = {
                    let state = lock(&shared.state);
                    (state.entry.clone(), state.written.clone())
                };
                if let (Some((entry, _)), Some(mut meta)) = (entry, meta) {
                    meta.ended_by = ended_by;
                    let io = lock(&shared.io);
                    // The hook's copy has its own, newer meta.
                    if !io.sealed
                        && entry.copy.exists()
                        && let Err(error) = meta.write(&entry.meta)
                    {
                        eprintln!("komponist: recovery: {}: {error}", entry.meta.display());
                    }
                }
            }
        }
    }
}

fn wake(inner: &WriterInner) {
    if let Some(waker) = &*lock(&inner.waker) {
        waker();
    }
}

fn write_job(inner: &WriterInner, owned: &mut Owned, published: &Published) -> io::Result<()> {
    #[cfg(test)]
    if let Some(hook) = &*lock(&inner.before_write) {
        hook();
    }
    let paths = owned.ensure(inner, published.generation)?.clone();
    let project = published.project();
    let meta = RecoveryMeta::at(&published.base, SystemTime::now());
    let io = lock(&inner.shared.io);
    if io.sealed {
        // The panic hook wrote the newest copy; this work is older.
        return Ok(());
    }
    #[cfg(test)]
    {
        use std::sync::atomic::Ordering;
        let fail = &inner.fail_writes;
        if fail
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(io::Error::other("test: the write fails"));
        }
    }
    write_entry(&paths, &project, &meta)?;
    drop(io);
    let mut state = lock(&inner.shared.state);
    if state.entry.as_ref().is_some_and(|(p, _)| *p == paths) {
        state.written = Some(meta);
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------------
// The session's side
// ------------------------------------------------------------------------------------------------

/// The clock the schedule reads (tests pass a fake one).
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// What [`Recovery::update`] needs to know about the open document in this frame.
pub struct DocumentState<'a> {
    /// The document has unsaved changes worth a copy (and is shown).
    pub dirty: bool,
    /// Its current project.
    pub project: &'a Arc<Project>,
    /// Plugin states that are newer than the document's (deferred bundled states).
    pub deferred: &'a [DeferredPluginEdit],
    /// Its project file.
    pub path: Option<&'a Path>,
}

/// The recovery of one app session: the store, the writer, this document's state and the copies
/// other sessions left. Disabled ([`Recovery::disabled`], the default) it does nothing at all, so
/// tests never touch a real folder.
pub struct Recovery {
    store: Option<Store>,
    shared: Arc<Shared>,
    writer: Option<Writer>,
    clock: Clock,
    schedule: Schedule,
    /// The open document; every [`Recovery::document_replaced`] counts up.
    generation: u64,
    /// The writer was asked for this document's entry.
    prepared: bool,
    /// The project and deferred states last seen ([`Recovery::update`] compares).
    seen: Option<(Arc<Project>, Deferred)>,
    /// A copy of this document is on disk or queued.
    has_copy: bool,
    /// A failed write is tried again at this time (`backoff` doubles up to [`MAX_AGE`]).
    retry: Option<Instant>,
    backoff: Duration,
    /// A failure was reported in this session.
    reported: bool,
    /// The writer thread could not start: reported once; scanning still works.
    unavailable: Option<io::Error>,
    /// Reading the folder failed in the last scan.
    scan_error: Option<io::Error>,
    /// The copies other sessions left, newest first ([`Recovery::scan`]).
    pub candidates: Vec<Candidate>,
}

impl Default for Recovery {
    fn default() -> Self {
        Recovery::disabled()
    }
}

impl Recovery {
    /// No recovery: nothing is written, scanned or locked.
    pub fn disabled() -> Recovery {
        Recovery {
            store: None,
            shared: Arc::default(),
            writer: None,
            clock: Arc::new(Instant::now),
            schedule: Schedule::default(),
            generation: 0,
            prepared: false,
            seen: None,
            has_copy: false,
            retry: None,
            backoff: Duration::ZERO,
            reported: false,
            unavailable: None,
            scan_error: None,
            candidates: Vec::new(),
        }
    }

    /// Recovery in `store`, on `clock`. If the writer thread cannot start, copies of this session
    /// are off (reported in the first frame) but the copies of other sessions are still offered.
    pub fn new(store: Store, clock: Clock) -> Recovery {
        let shared = Arc::<Shared>::default();
        let (writer, unavailable) = match Writer::start(store.clone(), Arc::clone(&shared)) {
            Ok(writer) => (Some(writer), None),
            Err(error) => {
                eprintln!("komponist: recovery: the writer thread did not start: {error}");
                (None, Some(error))
            }
        };
        Recovery {
            store: Some(store),
            shared,
            writer,
            clock,
            unavailable,
            ..Recovery::disabled()
        }
    }

    /// Whether recovery is on.
    pub fn enabled(&self) -> bool {
        self.store.is_some()
    }

    /// The store, if recovery is on.
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// Lets the writer wake the UI thread after a write (once; later calls do nothing).
    pub fn set_waker(&self, waker: impl Fn() + Send + Sync + 'static) {
        if let Some(writer) = &self.writer {
            let mut slot = lock(&writer.inner.waker);
            if slot.is_none() {
                *slot = Some(Box::new(waker));
            }
        }
    }

    /// Whether the writer can wake the UI thread.
    pub fn has_waker(&self) -> bool {
        self.writer
            .as_ref()
            .is_some_and(|w| lock(&w.inner.waker).is_some())
    }

    /// The files of this document's entry, once the writer has created it.
    pub fn entry_paths(&self) -> Option<EntryPaths> {
        lock(&self.shared.state)
            .entry
            .as_ref()
            .map(|(paths, _)| paths.clone())
    }

    /// Reads the folder for copies of other sessions (module docs); replaces the list. A failure
    /// to read the folder is kept for [`Recovery::take_scan_error`].
    pub fn scan(&mut self) {
        let Some(store) = &self.store else {
            return;
        };
        let (found, error) = store.scan();
        if let Some(error) = &error {
            eprintln!("komponist: recovery: {}: {error}", store.root().display());
        }
        self.candidates = found;
        self.scan_error = error;
    }

    /// Why the last scan could not read (all of) the folder, once.
    pub fn take_scan_error(&mut self) -> Option<io::Error> {
        self.scan_error.take()
    }

    /// The candidate with id `id`.
    pub fn candidate(&self, id: &Id) -> Option<&Candidate> {
        self.candidates.iter().find(|c| c.id == *id)
    }

    /// Removes the candidate `id` from the list.
    pub fn take_candidate(&mut self, id: &Id) -> Option<Candidate> {
        let index = self.candidates.iter().position(|c| c.id == *id)?;
        Some(self.candidates.remove(index))
    }

    /// The newest candidate of project file `path`, if one is waiting.
    pub fn candidate_for(&self, path: &Path) -> Option<&Candidate> {
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        self.candidates
            .iter()
            .find(|c| c.project_path().is_some_and(|p| same(p, path)))
    }

    /// Moves candidate `id` to `discarded/` and releases it; on an error it stays a candidate.
    pub fn discard(&mut self, id: &Id) -> io::Result<()> {
        let (Some(store), Some(candidate)) = (&self.store, self.candidate(id)) else {
            return Ok(());
        };
        store.discard(candidate)?;
        if let Some(candidate) = self.take_candidate(id) {
            let lock = candidate.paths.lock.clone();
            drop(candidate);
            let _ = fs::remove_file(lock);
        }
        Ok(())
    }

    /// One frame: publishes a changed document for the panic hook, hands a due copy to the writer
    /// (and a failed one again after a backoff), deletes the copy of a clean document. Returns the
    /// failure to report (once per session) and how long until the next copy is due.
    pub fn update(&mut self, doc: DocumentState<'_>) -> (Option<io::Error>, Option<Duration>) {
        if self.store.is_none() {
            return (None, None);
        }
        let now = (self.clock)();
        let failure = self.poll(now);
        let Some(writer) = &self.writer else {
            let unavailable = self.unavailable.take();
            return (unavailable.and_then(|e| self.report(e)), None);
        };
        if !doc.dirty {
            if self.has_copy {
                writer.queue(Job::DeleteCopy);
            }
            self.has_copy = false;
            self.schedule.reset();
            self.retry = None;
            self.backoff = Duration::ZERO;
            self.seen = None;
            self.shared.unpublish();
            return (failure, None);
        }
        if !self.prepared {
            writer.queue(Job::Prepare {
                generation: self.generation,
            });
            self.prepared = true;
        }
        let changed = self.seen.as_ref().is_none_or(|(project, deferred)| {
            !Arc::ptr_eq(project, doc.project)
                || deferred.len() != doc.deferred.len()
                || deferred
                    .iter()
                    .zip(doc.deferred)
                    .any(|(previous, current)| !previous.same_update(current))
        });
        let published = Published {
            generation: self.generation,
            project: Arc::clone(doc.project),
            deferred: doc.deferred.to_vec(),
            base: MetaBase {
                project_path: doc.path.map(Path::to_path_buf),
                name: doc.project.name.clone(),
            },
        };
        if changed {
            self.schedule.changed(now);
            self.seen = Some((Arc::clone(doc.project), doc.deferred.to_vec()));
            self.shared.publish(published.clone());
        }
        let retry_due = self.retry.is_some_and(|at| now >= at);
        if self.schedule.due(now) || retry_due {
            writer.write(published);
            self.has_copy = true;
            self.schedule.submitted(now);
            self.retry = None;
        }
        let due = match (self.schedule.due_at(), self.retry) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        (failure, due.map(|at| at.saturating_duration_since(now)))
    }

    /// The writer's outcomes since the last frame: a failure of the open document is tried again
    /// after a backoff; the first failure of the session is returned for the status bar.
    fn poll(&mut self, now: Instant) -> Option<io::Error> {
        let outcomes = self.writer.as_ref().map(Writer::outcomes)?;
        let mut first = None;
        for outcome in outcomes {
            match outcome {
                Outcome::Written { generation } if generation == self.generation => {
                    self.backoff = Duration::ZERO;
                }
                Outcome::Failed { generation, error } => {
                    if generation == self.generation && self.prepared {
                        self.backoff = if self.backoff.is_zero() {
                            QUIET
                        } else {
                            (self.backoff * 2).min(MAX_AGE)
                        };
                        self.retry = Some(now + self.backoff);
                    }
                    first = first.or_else(|| self.report(error));
                }
                Outcome::Written { .. } => {}
            }
        }
        first
    }

    fn report(&mut self, error: io::Error) -> Option<io::Error> {
        (!std::mem::replace(&mut self.reported, true)).then_some(error)
    }

    /// The open document is replaced (another project opened, a new one) or closed (quit): its
    /// copy and lock go. `adopt` is the entry of a recovered copy that the new document continues.
    pub fn document_replaced(&mut self, adopt: Option<(Entry, Arc<Project>)>) {
        if (self.prepared || self.has_copy)
            && let Some(writer) = &self.writer
        {
            writer.queue(Job::Close);
        }
        self.generation += 1;
        self.prepared = false;
        self.has_copy = false;
        self.schedule.reset();
        self.retry = None;
        self.backoff = Duration::ZERO;
        self.seen = None;
        self.shared.unpublish();
        if let Some((entry, project)) = adopt
            && let Some(writer) = &self.writer
        {
            writer.queue(Job::Adopt {
                entry,
                generation: self.generation,
            });
            // The copy on disk holds this project already: written again after the next change.
            self.seen = Some((project, Vec::new()));
            self.prepared = true;
            self.has_copy = true;
        }
    }

    /// Quitting normally: the copy goes, the writer finishes and stops, the candidates' locks are
    /// released.
    pub fn shutdown(&mut self) {
        self.document_replaced(None);
        if let Some(mut writer) = self.writer.take() {
            writer.stop();
        }
        self.candidates.clear();
    }

    /// Waits until the writer has done everything asked so far (tests, quitting).
    pub fn flush(&self, timeout: Duration) -> bool {
        self.writer.as_ref().is_none_or(|w| w.wait_idle(timeout))
    }

    /// What [`install_panic_hook`] needs; `None` while recovery is off or the writer did not start.
    pub fn panic_handle(&self) -> Option<PanicHandle> {
        Some(PanicHandle {
            shared: Arc::clone(&self.shared),
            writer: Arc::clone(&self.writer.as_ref()?.inner),
            ui: std::thread::current().id(),
        })
    }
}

/// `project` with the deferred plugin states put into their devices (as a save does).
fn with_deferred(project: &Arc<Project>, deferred: &[DeferredPluginEdit]) -> Arc<Project> {
    replay_deferred_plugins(project, deferred).map_or_else(|| Arc::clone(project), Arc::new)
}

// ------------------------------------------------------------------------------------------------
// The panic hook
// ------------------------------------------------------------------------------------------------

/// What the panic hook of a session works with ([`Recovery::panic_handle`]).
pub struct PanicHandle {
    shared: Arc<Shared>,
    writer: Arc<WriterInner>,
    /// The UI thread: the only thread on which the hook writes.
    ui: ThreadId,
}

/// Installs the recovery panic hook in front of the current one (which still runs afterwards and
/// prints the message). Called once by `main` on the UI thread.
pub fn install_panic_hook(handle: PanicHandle) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let ended_by = EndedBy::Panic {
            message: info.payload_as_str().unwrap_or("").to_owned(),
            location: info.location().map(ToString::to_string).unwrap_or_default(),
        };
        if std::thread::current().id() == handle.ui {
            write_from_hook(&handle.shared, ended_by);
        } else if let Some(mut slot) = try_lock(&handle.writer.slot) {
            // Never file IO on this thread (it may be a realtime thread): the writer does it.
            slot.queue.push_back(Job::MarkPanic(ended_by));
            handle.writer.wake.notify_one();
        }
        previous(info);
    }));
}

/// The UI thread panicked: writes the last published document (with its deferred plugin states)
/// as the copy, marked `ended_by`, and seals the store so the writer thread cannot put older work
/// over it. Without the IO lock within [`HOOK_IO_WAIT`] it writes nothing: the previous complete
/// copy stays.
fn write_from_hook(shared: &Shared, ended_by: EndedBy) {
    let (entry, published, written) = {
        let Some(state) = try_lock(&shared.state) else {
            return;
        };
        (
            state.entry.clone(),
            state.published.clone(),
            state.written.clone(),
        )
    };
    let Some((paths, generation)) = entry else {
        return;
    };
    let deadline = Instant::now() + HOOK_IO_WAIT;
    let mut io = loop {
        if let Some(guard) = try_lock(&shared.io) {
            break guard;
        }
        if Instant::now() >= deadline {
            eprintln!("komponist: recovery: the writer is busy; the last copy stays");
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if io.sealed {
        return;
    }
    let result = match (published, written) {
        // The entry belongs to the published document (not to one closed a moment ago).
        (Some(published), _) if published.generation == generation => {
            let mut meta = RecoveryMeta::at(&published.base, SystemTime::now());
            meta.ended_by = ended_by;
            write_entry(&paths, &published.project(), &meta)
        }
        // Clean when it panicked, but a copy is still on disk: mark it (not sealed: the writer
        // may still delete that copy of a saved document).
        (_, Some(mut meta)) if paths.copy.exists() => {
            meta.ended_by = ended_by;
            if let Err(error) = meta.write(&paths.meta) {
                eprintln!("komponist: recovery: {}: {error}", paths.meta.display());
            }
            return;
        }
        _ => return,
    };
    // Only a copy that is on disk seals the store; after a failure the writer may still write.
    match result {
        Ok(()) => io.sealed = true,
        Err(error) => eprintln!("komponist: recovery copy not written while panicking: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_session::project_io::load_project_repaired;

    const WAIT: Duration = Duration::from_secs(5);

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komponist-recovery-{name}-{}-{}",
            std::process::id(),
            Id::random()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn named(name: &str) -> Arc<Project> {
        Arc::new(Project::new(name))
    }

    fn base(name: &str) -> MetaBase {
        MetaBase {
            project_path: None,
            name: name.to_owned(),
        }
    }

    fn published(generation: u64, name: &str) -> Published {
        Published {
            generation,
            project: named(name),
            deferred: Vec::new(),
            base: base(name),
        }
    }

    /// A clock the test moves by hand.
    fn fake_clock() -> (Clock, Arc<Mutex<Instant>>) {
        let now = Arc::new(Mutex::new(Instant::now()));
        let read = Arc::clone(&now);
        (Arc::new(move || *read.lock().unwrap()), now)
    }

    fn advance(clock: &Mutex<Instant>, by: Duration) {
        *clock.lock().unwrap() += by;
    }

    fn doc<'a>(dirty: bool, project: &'a Arc<Project>) -> DocumentState<'a> {
        DocumentState {
            dirty,
            project,
            deferred: &[],
            path: None,
        }
    }

    /// One frame and the writer done.
    fn frame(r: &mut Recovery, state: DocumentState<'_>) -> (Option<io::Error>, Option<Duration>) {
        let out = r.update(state);
        assert!(r.flush(WAIT));
        out
    }

    fn loaded_name(paths: &EntryPaths) -> String {
        load_project_repaired(&paths.copy).unwrap().0.name
    }

    #[cfg(not(windows))]
    #[test]
    fn the_folder_follows_the_platform() {
        let home = Path::new("/home/u");
        assert_eq!(
            root_from(false, None, Some(home)),
            Some("/home/u/.local/state/komponist/recovery".into())
        );
        assert_eq!(
            root_from(false, Some(Path::new("/state")), Some(home)),
            Some("/state/komponist/recovery".into())
        );
        // A relative XDG variable does not count.
        assert_eq!(
            root_from(false, Some(Path::new("state")), Some(home)),
            Some("/home/u/.local/state/komponist/recovery".into())
        );
        assert_eq!(
            root_from(true, Some(Path::new("/state")), Some(Path::new("/Users/u"))),
            Some("/Users/u/Library/Application Support/komponist/Recovery".into())
        );
        assert_eq!(root_from(false, None, None), None);
    }

    #[test]
    fn ids_are_random_hex_and_parse_back() {
        let a = Id::random();
        let b = Id::random();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 16);
        assert_eq!(Id::parse(a.as_str()), Some(a));
        assert_eq!(Id::parse("discarded"), None);
        assert_eq!(Id::parse("0123456789ABCDEF"), None);
    }

    #[test]
    fn the_schedule_writes_after_two_quiet_seconds_and_at_least_every_thirty() {
        let t0 = Instant::now();
        let s = |secs: f32| t0 + Duration::from_secs_f32(secs);
        let mut schedule = Schedule::default();
        assert!(!schedule.due(s(100.0)), "nothing changed");
        schedule.changed(s(0.0));
        assert!(!schedule.due(s(1.9)));
        assert!(schedule.due(s(2.0)));
        // Changes every second: never two quiet seconds, so the 30 s bound writes.
        let mut schedule = Schedule::default();
        let mut t = 0.0;
        while t < 29.5 {
            schedule.changed(s(t));
            assert!(!schedule.due(s(t + 0.5)), "at {t}");
            t += 1.0;
        }
        assert_eq!(schedule.due_at(), Some(s(30.0)));
        assert!(schedule.due(s(30.0)));
        schedule.submitted(s(30.0));
        assert_eq!(schedule.due_at(), None);
        // The changes go on: the next copy 30 s after the last submission, not after the next
        // change.
        let mut t = 30.5;
        while t < 59.5 {
            schedule.changed(s(t));
            assert!(!schedule.due(s(t + 0.5)), "at {t}");
            t += 1.0;
        }
        assert_eq!(schedule.due_at(), Some(s(60.0)));
        // After a pause of more than 2 s a new change waits its 2 quiet seconds again.
        schedule.submitted(s(60.0));
        schedule.changed(s(70.0));
        assert_eq!(schedule.due_at(), Some(s(72.0)));
        schedule.reset();
        assert_eq!(schedule.due_at(), None);
    }

    #[test]
    fn a_dirty_document_gets_a_copy_after_two_seconds_and_a_clean_one_loses_it() {
        let dir = scratch("debounce");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let p1 = named("Night Drive");
        // Clean: no entry, nothing written.
        frame(&mut r, doc(false, &p1));
        assert!(r.entry_paths().is_none());
        // Dirty: the writer creates the entry and its lock at once, the copy 2 s later.
        let (_, due) = frame(&mut r, doc(true, &p1));
        let paths = r.entry_paths().expect("an entry");
        assert!(paths.lock.exists());
        assert_eq!(due, Some(QUIET));
        advance(&now, Duration::from_millis(1900));
        frame(&mut r, doc(true, &p1));
        assert!(!paths.copy.exists(), "not before 2 s");
        advance(&now, Duration::from_millis(100));
        frame(&mut r, doc(true, &p1));
        assert_eq!(loaded_name(&paths), "Night Drive");
        let meta = RecoveryMeta::read(&paths.meta).unwrap();
        assert_eq!(meta.name, "Night Drive");
        assert_eq!(meta.ended_by, EndedBy::Unknown);
        assert_eq!(meta.app_version, env!("CARGO_PKG_VERSION"));
        // Nothing new: nothing written again.
        advance(&now, Duration::from_secs(60));
        assert_eq!(frame(&mut r, doc(true, &p1)).1, None);
        // Clean again (saved, or undone to the saved state): the copy goes, the lock stays.
        frame(&mut r, doc(false, &p1));
        assert!(!paths.copy.exists() && !paths.meta.exists());
        assert!(paths.lock.exists());
        // Closing the document removes the lock file too.
        r.document_replaced(None);
        assert!(r.flush(WAIT));
        assert!(!paths.lock.exists());
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn the_store_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let root = dir.join("recovery");
        // A store an older build left readable for others, with its files, is narrowed by the
        // next scan.
        let store = Store::new(&root);
        let old = write_copy(&store, "Alt", None);
        // Widened after the store made it private, so only the scan can narrow it again.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o755);
        let discarded = root.join(DISCARDED);
        fs::create_dir_all(&discarded).unwrap();
        let thrown = discarded.join(format!(
            "{}-{}.json",
            Id::random(),
            unix_ms(SystemTime::now())
        ));
        fs::write(&thrown, "{}").unwrap();
        fs::set_permissions(&discarded, fs::Permissions::from_mode(0o755)).unwrap();
        let wide = fs::Permissions::from_mode(0o644);
        for path in [&old.copy, &old.meta, &old.lock, &thrown] {
            fs::set_permissions(path, wide.clone()).unwrap();
        }
        let found = store.scan().0;
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&discarded), 0o700);
        for path in [&old.copy, &old.meta, &old.lock, &thrown] {
            assert_eq!(mode(path), 0o600, "{}", path.display());
        }
        drop(found);
        // A new store is created owner-only.
        let fresh = dir.join("state/komponist/recovery");
        Store::new(&fresh).create_entry().unwrap();
        assert_eq!(mode(&fresh), 0o700);
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(store, clock);
        let p = named("Privat");
        frame(&mut r, doc(true, &p));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &p));
        let paths = r.entry_paths().unwrap();
        assert_eq!(mode(&paths.copy), 0o600);
        assert_eq!(mode(&paths.meta), 0o600);
        assert_eq!(mode(&paths.lock), 0o600);
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn continuous_changes_are_written_every_thirty_seconds() {
        let dir = scratch("max-age");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let mut writes = Vec::new();
        let mut last_copy = None;
        for i in 0..62 {
            let p = named(&format!("Take {i}"));
            frame(&mut r, doc(true, &p));
            let paths = r.entry_paths().unwrap();
            if let Ok(meta) = fs::read_to_string(&paths.meta)
                && Some(&meta) != last_copy.as_ref()
            {
                writes.push(i);
                last_copy = Some(meta);
            }
            advance(&now, Duration::from_secs(1));
        }
        // A change every second from 0 to 61 s: never two quiet seconds, so the copies come every
        // 30 s from the last one: at 30 s and 60 s.
        assert_eq!(writes, [30, 60]);
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_replaced_deferred_state_is_a_change_and_goes_into_the_copy() {
        let dir = scratch("deferred");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let project = komponist_model::fixtures::demo_project();
        let device = project.tracks[0].channel.devices[0].id;
        let project = Arc::new(project);
        let first: Deferred = vec![DeferredPluginEdit::snapshot(
            device,
            Arc::<[u8]>::from(&b"one"[..]).into(),
        )];
        fn state<'a>(project: &'a Arc<Project>, deferred: &'a Deferred) -> DocumentState<'a> {
            DocumentState {
                dirty: true,
                project,
                deferred: deferred.as_slice(),
                path: None,
            }
        }
        frame(&mut r, state(&project, &first));
        advance(&now, QUIET);
        frame(&mut r, state(&project, &first));
        let paths = r.entry_paths().unwrap();
        let stored = |paths: &EntryPaths| {
            let (p, _) = load_project_repaired(&paths.copy).unwrap();
            p.find_device(device).unwrap().2.state.clone()
        };
        assert_eq!(stored(&paths).as_deref(), Some(&b"one"[..]));
        // Same device, same count, a new state (a gesture goes on): a change.
        let second: Deferred = vec![DeferredPluginEdit::snapshot(
            device,
            Arc::<[u8]>::from(&b"two"[..]).into(),
        )];
        let (_, due) = frame(&mut r, state(&project, &second));
        assert_eq!(due, Some(QUIET));
        advance(&now, QUIET);
        frame(&mut r, state(&project, &second));
        assert_eq!(stored(&paths).as_deref(), Some(&b"two"[..]));
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_write_is_tried_again_without_another_edit() {
        let dir = scratch("retry");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        r.writer
            .as_ref()
            .unwrap()
            .inner
            .fail_writes
            .store(2, std::sync::atomic::Ordering::Relaxed);
        let woken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&woken);
        r.set_waker(move || {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
        let p = named("Wieder");
        frame(&mut r, doc(true, &p));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &p));
        let paths = r.entry_paths().unwrap();
        assert!(!paths.copy.exists());
        // The writer wakes the UI, so an idle app still gets the frame that schedules the retry.
        assert!(woken.load(std::sync::atomic::Ordering::Relaxed) > 0);
        // The failure comes back in the next frame (reported once) with a retry after 2 s.
        let (failure, due) = frame(&mut r, doc(true, &p));
        assert!(failure.is_some());
        assert_eq!(due, Some(QUIET));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &p));
        // Failed again: reported no more, the backoff doubles.
        let (failure, due) = frame(&mut r, doc(true, &p));
        assert!(failure.is_none());
        assert_eq!(due, Some(QUIET * 2));
        advance(&now, QUIET * 2);
        frame(&mut r, doc(true, &p));
        assert_eq!(
            loaded_name(&paths),
            "Wieder",
            "written without another edit"
        );
        // A clean document is not tried again.
        r.writer
            .as_ref()
            .unwrap()
            .inner
            .fail_writes
            .store(1, std::sync::atomic::Ordering::Relaxed);
        let q = named("Nochmal");
        frame(&mut r, doc(true, &q));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &q));
        frame(&mut r, doc(false, &q));
        assert_eq!(frame(&mut r, doc(false, &q)).1, None);
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_slow_writer_writes_only_the_latest_project() {
        let dir = scratch("latest");
        let shared = Arc::<Shared>::default();
        let writer = Writer::start(Store::new(&dir), Arc::clone(&shared)).unwrap();
        let (started_tx, started) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let count = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&count);
        writer.set_before_write(move || {
            *counted.lock().unwrap() += 1;
            let _ = started_tx.send(());
            let _ = gate.lock().unwrap().recv_timeout(WAIT);
        });
        writer.write(published(0, "v1"));
        started.recv_timeout(WAIT).unwrap();
        // v1 is being written; v2 and v3 queue, v3 replaces v2.
        writer.write(published(0, "v2"));
        writer.write(published(0, "v3"));
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(writer.wait_idle(WAIT));
        assert_eq!(*count.lock().unwrap(), 2, "v2 was never written");
        let paths = lock(&shared.state).entry.clone().unwrap().0;
        assert_eq!(loaded_name(&paths), "v3");
        // A deletion drops a queued write.
        let (release_again, gate_again) = mpsc::channel::<()>();
        let gate_again = Mutex::new(gate_again);
        writer.set_before_write(move || {
            let _ = gate_again.lock().unwrap().recv_timeout(WAIT);
        });
        writer.write(published(0, "v4"));
        writer.queue(Job::DeleteCopy);
        release_again.send(()).unwrap();
        assert!(writer.wait_idle(WAIT));
        assert!(!paths.copy.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_panic_copy_is_not_overwritten_by_older_queued_work() {
        let dir = scratch("sealed");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let a = named("A");
        frame(&mut r, doc(true, &a));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &a));
        let paths = r.entry_paths().unwrap();
        // B is published (an edit), not yet written; the UI thread panics.
        let b = named("B");
        r.update(doc(true, &b));
        write_from_hook(
            &r.shared,
            EndedBy::Panic {
                message: "boom".into(),
                location: "app.rs:1:1".into(),
            },
        );
        assert_eq!(loaded_name(&paths), "B");
        // Older work the writer still had cannot land on top of it.
        r.writer.as_ref().unwrap().write(Published {
            generation: r.generation,
            ..published(0, "A")
        });
        assert!(r.flush(WAIT));
        assert_eq!(loaded_name(&paths), "B");
        let meta = RecoveryMeta::read(&paths.meta).unwrap();
        assert!(matches!(meta.ended_by, EndedBy::Panic { .. }));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_sealed_store_keeps_the_panic_copy_against_a_later_delete_and_mark() {
        let dir = scratch("sealed-delete");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let a = named("A");
        frame(&mut r, doc(true, &a));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &a));
        let paths = r.entry_paths().unwrap();
        // Saved, edited again, and the UI thread panics before the writer deletes the old copy.
        let b = named("B");
        r.update(doc(true, &b));
        let hook = EndedBy::Panic {
            message: "boom".into(),
            location: "app.rs:1:1".into(),
        };
        write_from_hook(&r.shared, hook.clone());
        let writer = r.writer.as_ref().unwrap();
        writer.queue(Job::DeleteCopy);
        writer.queue(Job::MarkPanic(EndedBy::Panic {
            message: "later".into(),
            location: "engine.rs:1:1".into(),
        }));
        assert!(r.flush(WAIT));
        assert_eq!(loaded_name(&paths), "B", "the copy stays");
        assert_eq!(RecoveryMeta::read(&paths.meta).unwrap().ended_by, hook);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_hook_that_writes_nothing_does_not_seal_the_store() {
        let dir = scratch("unsealed");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let p = named("Später");
        // An entry, but a clean document: the hook has nothing to write.
        frame(&mut r, doc(true, &p));
        frame(&mut r, doc(false, &p));
        write_from_hook(&r.shared, EndedBy::Unknown);
        assert!(!lock(&r.shared.io).sealed);
        // The writer still writes the next copy.
        frame(&mut r, doc(true, &p));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &p));
        assert_eq!(loaded_name(&r.entry_paths().unwrap()), "Später");
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_copy_that_cannot_be_deleted_keeps_its_meta() {
        let dir = scratch("undeletable");
        let store = Store::new(&dir);
        let paths = write_copy(&store, "Fest", None);
        // A non-empty folder where the copy is: `remove_file` fails.
        fs::remove_file(&paths.copy).unwrap();
        fs::create_dir_all(paths.copy.join("inside")).unwrap();
        assert!(delete_entry(&paths).is_err());
        assert!(paths.meta.exists(), "the meta stays with its copy");
        // Nothing there counts as deleted.
        fs::remove_dir_all(&paths.copy).unwrap();
        assert!(delete_entry(&paths).is_ok());
        assert!(!paths.meta.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn an_interrupted_write_keeps_a_matching_pair() {
        let dir = scratch("generation");
        let store = Store::new(&dir);
        let entry = store.create_entry().unwrap();
        let paths = entry.paths.clone();
        let at = |path: &str| MetaBase {
            project_path: Some(dir.join(path)),
            name: "Night Drive".into(),
        };
        // Generation 1 in folder A, complete.
        write_entry(
            &paths,
            &Project::new("Night Drive"),
            &RecoveryMeta::at(&at("a/Night Drive.komponist"), SystemTime::now()),
        )
        .unwrap();
        drop(entry);
        // Generation 2 after Save As to folder B: the meta is written, the copy is not (a crash,
        // a failed write). The old copy keeps the old meta.
        let mut project = Project::new("Night Drive");
        project.tempo.bpm = 99.0;
        let json = persist::to_json(&project).unwrap();
        let mut next = RecoveryMeta::at(&at("b/Night Drive.komponist"), SystemTime::now());
        next.copy = Some(CopyCheck::of(json.as_bytes()));
        next.write(&paths.next_meta).unwrap();
        let found = store.scan().0;
        assert_eq!(
            found[0].project_path(),
            Some(dir.join("a/Night Drive.komponist").as_path())
        );
        assert_eq!(found[0].project().unwrap().tempo.bpm, 120.0);
        assert!(!paths.next_meta.exists(), "the stale meta goes");
        drop(found);
        // Generation 2 again: the copy is published, the meta not yet committed. The new copy
        // pairs with the new meta, which the scan commits.
        next.write(&paths.next_meta).unwrap();
        write_published(&paths.copy, &json).unwrap();
        let found = store.scan().0;
        assert_eq!(
            found[0].project_path(),
            Some(dir.join("b/Night Drive.komponist").as_path())
        );
        assert_eq!(found[0].project().unwrap().tempo.bpm, 99.0);
        assert!(!paths.next_meta.exists());
        assert_eq!(
            RecoveryMeta::read(&paths.meta).unwrap().project_path,
            Some(dir.join("b/Night Drive.komponist"))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_panic_copy_holds_the_deferred_plugin_states() {
        let dir = scratch("hook-deferred");
        let (clock, _now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let project = komponist_model::fixtures::demo_project();
        let device = project.tracks[0].channel.devices[0].id;
        let project = Arc::new(project);
        let deferred: Deferred = vec![DeferredPluginEdit::snapshot(
            device,
            Arc::<[u8]>::from(&b"late"[..]).into(),
        )];
        frame(
            &mut r,
            DocumentState {
                dirty: true,
                project: &project,
                deferred: &deferred,
                path: None,
            },
        );
        write_from_hook(&r.shared, EndedBy::Unknown);
        let paths = r.entry_paths().unwrap();
        let (copy, _) = load_project_repaired(&paths.copy).unwrap();
        assert_eq!(
            copy.find_device(device).unwrap().2.state.as_deref(),
            Some(&b"late"[..])
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_panic_on_another_thread_only_marks_the_meta_through_the_writer() {
        let dir = scratch("mark");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let p = named("Night Drive");
        frame(&mut r, doc(true, &p));
        advance(&now, QUIET);
        frame(&mut r, doc(true, &p));
        let paths = r.entry_paths().unwrap();
        // What the hook does on a thread that is not the UI thread: queue, never write itself.
        let handle = r.panic_handle().unwrap();
        let ended_by = EndedBy::Panic {
            message: "boom".into(),
            location: "engine.rs:1:1".into(),
        };
        {
            let mut slot = try_lock(&handle.writer.slot).unwrap();
            slot.queue.push_back(Job::MarkPanic(ended_by.clone()));
            handle.writer.wake.notify_one();
        }
        assert!(r.flush(WAIT));
        let meta = RecoveryMeta::read(&paths.meta).unwrap();
        assert_eq!(meta.ended_by, ended_by);
        assert_eq!(meta.name, "Night Drive");
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_live_entry_is_not_offered_until_its_session_ends() {
        let dir = scratch("lock");
        let (clock, now) = fake_clock();
        let mut first = Recovery::new(Store::new(&dir), clock);
        let p = named("Hafenlicht");
        frame(&mut first, doc(true, &p));
        advance(&now, QUIET);
        frame(&mut first, doc(true, &p));
        // A second store (another komponist) does not see the copy of the live session.
        let second = Store::new(&dir);
        assert!(second.scan().0.is_empty());
        // The session ends without deleting its copy (a crash): now the copy is offered.
        let paths = first.entry_paths().unwrap();
        drop(first.writer.take());
        let (found, error) = second.scan();
        assert!(error.is_none());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].paths, paths);
        assert_eq!(found[0].name(), Some("Hafenlicht"));
        assert_eq!(found[0].file_state(), FileState::Never);
        assert_eq!(found[0].project().unwrap().name, "Hafenlicht");
        // While the first scan holds the candidate, a third one does not see it either.
        assert!(Store::new(&dir).scan().0.is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    fn write_copy(store: &Store, name: &str, modified: Option<SystemTime>) -> EntryPaths {
        let entry = store.create_entry().unwrap();
        write_entry(
            &entry.paths,
            &Project::new(name),
            &RecoveryMeta::at(&base(name), SystemTime::now()),
        )
        .unwrap();
        if let Some(modified) = modified {
            OpenOptions::new()
                .write(true)
                .open(&entry.paths.copy)
                .unwrap()
                .set_modified(modified)
                .unwrap();
        }
        entry.paths
    }

    #[test]
    fn candidates_are_sorted_newest_first_and_leftovers_without_a_copy_go() {
        let dir = scratch("scan");
        let store = Store::new(&dir);
        let at = |s: u64| Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + s));
        write_copy(&store, "Alt", at(1));
        write_copy(&store, "Neu", at(3));
        write_copy(&store, "Mitte", at(2));
        // A lock without a copy: a session that ended clean.
        let stale = store.create_entry().unwrap();
        let stale_lock = stale.paths.lock.clone();
        drop(stale);
        // A meta without its copy: a crash between the first meta and the first copy.
        let early = store.create_entry().unwrap();
        RecoveryMeta::at(&base("Früh"), SystemTime::now())
            .write(&early.paths.meta)
            .unwrap();
        let early_paths = early.paths.clone();
        drop(early);
        let (found, error) = store.scan();
        assert!(error.is_none());
        let names: Vec<_> = found.iter().map(|c| c.name().unwrap().to_owned()).collect();
        assert_eq!(names, ["Neu", "Mitte", "Alt"]);
        assert_eq!(found[0].written(), at(3).unwrap(), "the copy's own time");
        assert!(!stale_lock.exists());
        assert!(!early_paths.meta.exists() && !early_paths.lock.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_missing_folder_is_empty_and_an_unreadable_one_says_so() {
        let dir = scratch("unreadable-folder");
        let (found, error) = Store::new(dir.join("nothing")).scan();
        assert!(found.is_empty() && error.is_none());
        // A file where the folder should be: not a missing folder.
        fs::write(dir.join("file"), "x").unwrap();
        let (found, error) = Store::new(dir.join("file")).scan();
        assert!(found.is_empty());
        assert!(error.is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn the_file_state_compares_fingerprints() {
        let dir = scratch("fingerprint");
        let store = Store::new(&dir);
        let file = dir.join("Night Drive.komponist");
        fs::write(&file, "x").unwrap();
        let entry = store.create_entry().unwrap();
        let based = MetaBase {
            project_path: Some(file.clone()),
            name: "Night Drive".into(),
        };
        write_entry(
            &entry.paths,
            &Project::new("Night Drive"),
            &RecoveryMeta::at(&based, SystemTime::now()),
        )
        .unwrap();
        drop(entry);
        let state = |store: &Store| store.scan().0.remove(0).file_state();
        assert!(matches!(
            state(&store),
            FileState::Saved { changed: false, .. }
        ));
        // Saved after the copy (another length): variant c.
        fs::write(&file, "xy").unwrap();
        assert!(matches!(
            state(&store),
            FileState::Saved { changed: true, .. }
        ));
        fs::remove_file(&file).unwrap();
        assert_eq!(state(&store), FileState::Gone);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn unreadable_copies_are_candidates_with_a_reason() {
        let dir = scratch("unreadable");
        let store = Store::new(&dir);
        let entry = store.create_entry().unwrap();
        fs::write(&entry.paths.copy, r#"{"format":"komponist","version":99}"#).unwrap();
        RecoveryMeta::at(&base("Night Drive"), SystemTime::now())
            .write(&entry.paths.meta)
            .unwrap();
        drop(entry);
        let found = store.scan().0;
        let reason = crate::project_io::project_io_reason(
            found[0].loaded.as_ref().unwrap_err(),
            komponist_i18n::Lang::De,
        );
        assert_eq!(reason, "Dateiversion 99 ist neuer als dieses komponist");
        assert_eq!(found[0].name(), Some("Night Drive"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn discarded_copies_wait_seven_days_from_the_discard() {
        let dir = scratch("discard");
        let store = Store::new(&dir);
        // Written long ago: the 7 days still count from the discard.
        let paths = write_copy(&store, "Weg", Some(SystemTime::now() - KEEP_DISCARDED * 2));
        let (clock, _) = fake_clock();
        let mut r = Recovery::new(store.clone(), clock);
        r.scan();
        let id = r.candidates[0].id.clone();
        r.discard(&id).unwrap();
        assert!(r.candidates.is_empty());
        assert!(!paths.copy.exists() && !paths.meta.exists() && !paths.lock.exists());
        let kept: Vec<PathBuf> = fs::read_dir(dir.join(DISCARDED))
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert_eq!(kept.len(), 2, "copy and meta");
        // A scan right away (another komponist) keeps them.
        let _ = store.scan();
        assert!(kept.iter().all(|p| p.exists()));
        store.prune_discarded(SystemTime::now() + KEEP_DISCARDED - Duration::from_secs(60));
        assert!(kept.iter().all(|p| p.exists()));
        store.prune_discarded(SystemTime::now() + KEEP_DISCARDED + Duration::from_secs(60));
        assert!(kept.iter().all(|p| !p.exists()));
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_discard_that_cannot_move_the_meta_leaves_the_pair_in_place() {
        let dir = scratch("discard-fails");
        let store = Store::new(&dir);
        let paths = write_copy(&store, "Bleibt", None);
        let copy_to = dir.join("moved.komponist");
        // A non-empty folder where the meta should go: the rename fails.
        let meta_to = dir.join("blocked");
        fs::create_dir_all(meta_to.join("inside")).unwrap();
        assert!(discard_to(&paths, &copy_to, &meta_to).is_err());
        assert!(paths.copy.exists() && paths.meta.exists());
        assert!(!copy_to.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_discard_whose_rollback_fails_stays_discarded_and_is_not_offered() {
        let dir = scratch("discard-rollback");
        let store = Store::new(&dir);
        let paths = write_copy(&store, "Weg", None);
        let folder = dir.join(DISCARDED);
        fs::create_dir_all(&folder).unwrap();
        let copy_to = folder.join("weg.komponist");
        // The meta cannot follow (a non-empty folder in its way), the copy cannot go back.
        let meta_to = dir.join("blocked");
        fs::create_dir_all(meta_to.join("inside")).unwrap();
        let result = discard_with(&paths, &copy_to, &meta_to, |_, _| {
            Err(io::Error::other("test: no way back"))
        });
        assert!(result.is_ok(), "the copy is discarded");
        assert!(copy_to.exists(), "the copy is kept in discarded/");
        assert!(!paths.copy.exists() && !paths.meta.exists());
        // The next start offers nothing, not an unreadable copy.
        let (found, error) = store.scan();
        assert!(found.is_empty() && error.is_none());
        assert!(copy_to.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_discard_keeps_the_pending_meta_of_the_copy() {
        let dir = scratch("discard-pending");
        let store = Store::new(&dir);
        let entry = store.create_entry().unwrap();
        let paths = entry.paths.clone();
        let at = |path: &str| MetaBase {
            project_path: Some(dir.join(path)),
            name: "Night Drive".into(),
        };
        write_entry(
            &paths,
            &Project::new("Night Drive"),
            &RecoveryMeta::at(&at("a/Night Drive.komponist"), SystemTime::now()),
        )
        .unwrap();
        drop(entry);
        // A newer generation in folder b: the copy is published, the meta is still pending, and
        // the scan cannot commit it (a non-empty folder where `<id>.json` goes).
        let mut project = Project::new("Night Drive");
        project.tempo.bpm = 99.0;
        let json = persist::to_json(&project).unwrap();
        let mut next = RecoveryMeta::at(&at("b/Night Drive.komponist"), SystemTime::now());
        next.copy = Some(CopyCheck::of(json.as_bytes()));
        next.write(&paths.next_meta).unwrap();
        write_published(&paths.copy, &json).unwrap();
        fs::remove_file(&paths.meta).unwrap();
        fs::create_dir_all(paths.meta.join("inside")).unwrap();
        let b = dir.join("b/Night Drive.komponist");
        let found = store.scan().0;
        assert_eq!(found[0].project_path(), Some(b.as_path()));
        // The discard cannot move the copy (a non-empty folder in its way): nothing changes.
        let copy_to = dir.join("blocked-copy");
        fs::create_dir_all(copy_to.join("inside")).unwrap();
        assert!(discard_to(&paths, &copy_to, &dir.join("meta-to")).is_err());
        assert!(paths.next_meta.exists() && paths.copy.exists());
        drop(found);
        // The next scan still has the original path of that copy.
        let found = store.scan().0;
        assert_eq!(found[0].project_path(), Some(b.as_path()));
        assert_eq!(found[0].project().unwrap().tempo.bpm, 99.0);
        drop(found);
        // A discard that works takes the pending meta along with the copy.
        let discarded = dir.join("gone");
        fs::create_dir_all(&discarded).unwrap();
        let (copy_to, meta_to) = (discarded.join("c.komponist"), discarded.join("c.json"));
        discard_to(&paths, &copy_to, &meta_to).unwrap();
        assert_eq!(
            RecoveryMeta::read(&meta_to).unwrap().project_path,
            Some(b.clone())
        );
        assert!(!paths.copy.exists() && !paths.next_meta.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_pending_meta_is_committed_before_the_next_write_replaces_it() {
        let dir = scratch("pending-next");
        let store = Store::new(&dir);
        let entry = store.create_entry().unwrap();
        let paths = entry.paths.clone();
        let at = |path: &str, bpm: f64| {
            let mut project = Project::new("Night Drive");
            project.tempo.bpm = bpm;
            let base = MetaBase {
                project_path: Some(dir.join(path)),
                name: "Night Drive".into(),
            };
            (project, RecoveryMeta::at(&base, SystemTime::now()))
        };
        // Generation A, complete.
        let (a, meta_a) = at("a/Night Drive.komponist", 100.0);
        write_entry(&paths, &a, &meta_a).unwrap();
        // Generation B: the copy is published, the commit fails (a folder where `<id>.json` goes).
        let (b, mut meta_b) = at("b/Night Drive.komponist", 110.0);
        let json_b = persist::to_json(&b).unwrap();
        meta_b.copy = Some(CopyCheck::of(json_b.as_bytes()));
        meta_b.write(&paths.next_meta).unwrap();
        write_published(&paths.copy, &json_b).unwrap();
        fs::remove_file(&paths.meta).unwrap();
        fs::create_dir_all(paths.meta.join("inside")).unwrap();
        // Generation C cannot take the pending name while B's meta cannot be committed.
        let (c, meta_c) = at("c/Night Drive.komponist", 120.0);
        assert!(write_entry(&paths, &c, &meta_c).is_err());
        assert_eq!(
            RecoveryMeta::read(&paths.next_meta).unwrap().project_path,
            Some(dir.join("b/Night Drive.komponist")),
            "B's meta is kept"
        );
        // Once the commit works, C commits B first; a crash before C's copy leaves B's pair.
        fs::remove_dir_all(&paths.meta).unwrap();
        let json_c = persist::to_json(&c).unwrap();
        let mut pending_c = meta_c.clone();
        pending_c.copy = Some(CopyCheck::of(json_c.as_bytes()));
        // What `write_entry` does up to the crash: commit B, write C's meta, no copy yet.
        let bytes = fs::read(&paths.copy).unwrap();
        assert!(
            RecoveryMeta::read(&paths.next_meta)
                .is_some_and(|m| m.copy == Some(CopyCheck::of(&bytes)))
        );
        commit_meta(&paths).unwrap();
        pending_c.write(&paths.next_meta).unwrap();
        drop(entry);
        let found = store.scan().0;
        assert_eq!(
            found[0].project_path(),
            Some(dir.join("b/Night Drive.komponist").as_path())
        );
        assert_eq!(found[0].project().unwrap().tempo.bpm, 110.0);
        drop(found);
        // And a full write of C after the commit is a complete pair.
        let entry = store.scan().0.remove(0);
        let paths = entry.paths.clone();
        write_entry(&paths, &c, &meta_c).unwrap();
        assert!(!paths.next_meta.exists());
        assert_eq!(
            RecoveryMeta::read(&paths.meta).unwrap().project_path,
            Some(dir.join("c/Night Drive.komponist"))
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn recovery_copy_keeps_a_deferred_preset_and_its_state_together() {
        use komponist_model::{ClapPresetLocation, ClapPresetSource, DevicePreset};

        let project = Arc::new(komponist_model::fixtures::demo_project());
        let device = project
            .tracks
            .iter()
            .flat_map(|track| &track.channel.devices)
            .find(|device| device.plugin.name == "Surge XT")
            .unwrap()
            .id;
        let source = ClapPresetSource {
            location: ClapPresetLocation::Plugin,
            load_key: Some(b"Warm".to_vec()),
        };
        let mut report = DeferredPluginEdit::reported(device, source.clone());
        // The label waits while the bytes are unknown.
        assert!(Arc::ptr_eq(
            &with_deferred(&project, &[report.clone()]),
            &project
        ));
        report.set_snapshot(&Arc::<[u8]>::from(&b"loaded"[..]).into());
        let recovered = with_deferred(&project, &[report]);
        let recovered = recovered.find_device(device).unwrap().2;
        assert_eq!(recovered.preset, Some(DevicePreset::loaded(source)));
        assert_eq!(recovered.state.as_deref(), Some(&b"loaded"[..]));
    }

    #[test]
    fn deferred_plugin_states_go_into_the_copy() {
        let project = komponist_model::fixtures::demo_project();
        let device = project.tracks[0].channel.devices[0].id;
        let project =
            Arc::new(komponist_model::edits::set_device_state(&project, device, None).unwrap());
        let state: Arc<[u8]> = Arc::from(&b"state"[..]);
        let out = with_deferred(
            &project,
            &[DeferredPluginEdit::snapshot(
                device,
                Arc::clone(&state).into(),
            )],
        );
        assert_eq!(
            out.find_device(device).unwrap().2.state.as_deref(),
            Some(&b"state"[..])
        );
        // Nothing deferred: the same project, no copy.
        assert!(Arc::ptr_eq(&with_deferred(&project, &[]), &project));
    }
}
