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
//! - **How** ([`Writer`]): the UI thread hands the project (an `Arc`, no copy) to a writer thread
//!   through a latest-wins slot: a newer project replaces a queued one. Deletions queue in order
//!   behind it. The writer serialises and writes with `project_io::write_atomic` (staging file,
//!   fsync, rename, fsync of the folder). The UI thread never serialises or writes a copy, and the
//!   realtime threads never do any file IO, also not while panicking.
//! - **Panic hook** ([`install_panic_hook`], chained to the previous hook): on the UI thread it takes
//!   the last published project with `try_lock` (it never blocks on it) and writes the copy itself,
//!   with `ended_by = panic`; on any other thread it only queues `ended_by` for the writer thread,
//!   which rewrites the meta. Signals (a crash inside an in-process plugin, a kill, a power loss) run
//!   no hook: the periodic copy covers them.
//! - Discarded copies move to `discarded/` and are deleted after [`KEEP_DISCARDED`] (7 days), without
//!   UI.
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
use komponist_model::{DeviceId, Project, Repairs};

use crate::project_io::{ProjectIoError, load_project_repaired, write_atomic};

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
    /// `<id>.lock`: held while the document is open.
    pub lock: PathBuf,
}

impl EntryPaths {
    fn new(root: &Path, id: &Id) -> EntryPaths {
        let file = |extension: &str| root.join(format!("{id}.{extension}"));
        EntryPaths {
            copy: file(COPY_EXTENSION),
            meta: file(META_EXTENSION),
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
        }
    }

    /// When the copy was written.
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

/// `write_atomic` into the existing recovery folder; a failed folder flush after the rename only
/// goes to the log (the copy is in place).
fn write_published(path: &Path, text: &str) -> io::Result<()> {
    if let Some(error) = write_atomic(path, text, true)? {
        eprintln!(
            "komponist: {}: not flushed to disk: {error}",
            path.display()
        );
    }
    Ok(())
}

/// Writes the project and then its meta (the meta last: a copy without a meta is still offered,
/// a meta without its copy says the copy is unreadable).
fn write_entry(paths: &EntryPaths, project: &Project, meta: &RecoveryMeta) -> io::Result<()> {
    let json = persist::to_json(project).map_err(io::Error::other)?;
    write_published(&paths.copy, &json)?;
    meta.write(&paths.meta)
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
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

/// Opens (creating it) and locks `path`: `Ok(Some(file))` when the lock is ours, `Ok(None)` when
/// another process (or another handle of this one) holds it.
fn take_lock(path: &Path) -> io::Result<Option<File>> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

impl Store {
    /// The store in folder `root` (created when the first entry is).
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

    /// A new entry with a fresh id and its lock taken (no copy yet).
    pub fn create_entry(&self) -> io::Result<Entry> {
        fs::create_dir_all(&self.root)?;
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

    /// The copies other sessions left behind, newest first: every entry with a copy or a meta whose
    /// lock this call can take (the locks stay held by the candidates). Entries locked by a live
    /// session are skipped. Lock files without a copy are removed; discarded copies older than
    /// [`KEEP_DISCARDED`] are deleted.
    pub fn scan(&self) -> Vec<Candidate> {
        self.prune_discarded(SystemTime::now());
        let Ok(dir) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut ids: Vec<Id> = dir
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                let extension = path.extension()?.to_str()?;
                if ![COPY_EXTENSION, META_EXTENSION, LOCK_EXTENSION].contains(&extension) {
                    return None;
                }
                Id::parse(path.file_stem()?.to_str()?)
            })
            .collect();
        ids.sort();
        ids.dedup();
        let mut found: Vec<Candidate> = ids
            .into_iter()
            .filter_map(|id| self.candidate(id))
            .collect();
        found.sort_by_key(|c| std::cmp::Reverse(c.written()));
        found
    }

    fn candidate(&self, id: Id) -> Option<Candidate> {
        let paths = self.paths(&id);
        let lock = match take_lock(&paths.lock) {
            Ok(Some(lock)) => lock,
            Ok(None) => return None,
            Err(error) => {
                eprintln!("komponist: recovery: {}: {error}", paths.lock.display());
                return None;
            }
        };
        let has_copy = paths.copy.exists();
        let meta = RecoveryMeta::read(&paths.meta);
        if !has_copy && !paths.meta.exists() {
            // A session that ended while its document was clean.
            let _ = fs::remove_file(&paths.lock);
            return None;
        }
        let loaded = load_project_repaired(&paths.copy).map(|(p, r)| (Arc::new(p), r));
        let written = meta.as_ref().map_or_else(
            || {
                fs::metadata(&paths.copy)
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH)
            },
            RecoveryMeta::written,
        );
        Some(Candidate {
            id,
            paths,
            meta,
            loaded,
            written,
            lock: Some(lock),
        })
    }

    /// Moves `candidate`'s copy and meta to `discarded/` (kept [`KEEP_DISCARDED`]) and releases it.
    pub fn discard(&self, candidate: Candidate) -> io::Result<()> {
        let folder = self.root.join(DISCARDED);
        fs::create_dir_all(&folder)?;
        let now = SystemTime::now();
        for path in [&candidate.paths.copy, &candidate.paths.meta] {
            let Some(name) = path.file_name() else {
                continue;
            };
            let target = folder.join(name);
            match fs::rename(path, &target) {
                Ok(()) => {
                    // The 7 days start now, not when the copy was written.
                    if let Ok(file) = OpenOptions::new().write(true).open(&target) {
                        let _ = file.set_modified(now);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let lock = candidate.paths.lock.clone();
        drop(candidate);
        let _ = fs::remove_file(lock);
        Ok(())
    }

    /// Deletes the discarded copies older than [`KEEP_DISCARDED`] at `now`.
    pub fn prune_discarded(&self, now: SystemTime) {
        let Ok(dir) = fs::read_dir(self.root.join(DISCARDED)) else {
            return;
        };
        for entry in dir.filter_map(Result::ok) {
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
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
    /// When the copy was written.
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
/// after the last change, or [`MAX_AGE`] after the first change the copy does not hold yet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Schedule {
    first: Option<Instant>,
    last: Option<Instant>,
}

impl Schedule {
    /// The document changed at `now`.
    pub fn changed(&mut self, now: Instant) {
        self.first.get_or_insert(now);
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

    /// The copy was handed to the writer: nothing pending.
    pub fn written(&mut self) {
        *self = Schedule::default();
    }
}

// ------------------------------------------------------------------------------------------------
// The writer thread
// ------------------------------------------------------------------------------------------------

/// What the panic hook needs: the entry and the last published project, and the meta of the last
/// copy on disk. Shared by the UI thread, the writer thread and the hook.
#[derive(Default)]
pub struct Shared {
    state: Mutex<HookState>,
    /// Held while a copy is being written, so the hook does not race the writer thread.
    io: Mutex<()>,
}

#[derive(Default)]
struct HookState {
    entry: Option<EntryPaths>,
    published: Option<(Arc<Project>, MetaBase)>,
    written: Option<RecoveryMeta>,
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
    fn publish(&self, entry: &EntryPaths, project: Arc<Project>, base: MetaBase) {
        let mut state = lock(&self.state);
        state.entry = Some(entry.clone());
        state.published = Some((project, base));
    }

    fn unpublish(&self) {
        let mut state = lock(&self.state);
        state.published = None;
    }

    fn forget(&self, entry: &EntryPaths) {
        let mut state = lock(&self.state);
        if state.entry.as_ref() == Some(entry) {
            *state = HookState::default();
        }
    }
}

enum Job {
    /// Write the copy (latest wins).
    Write {
        paths: EntryPaths,
        project: Arc<Project>,
        base: MetaBase,
    },
    /// Delete copy and meta; with a lock, also release it and delete the lock file.
    Delete {
        paths: EntryPaths,
        lock: Option<File>,
    },
    /// A panic on a thread that must not write: put `ended_by` into the meta.
    MarkPanic(EndedBy),
}

#[derive(Default)]
struct Slot {
    /// The newest write; a newer one replaces it.
    write: Option<Job>,
    /// Deletions and panic marks, in order, before the write.
    queue: VecDeque<Job>,
    busy: bool,
    stop: bool,
}

struct WriterInner {
    slot: Mutex<Slot>,
    wake: Condvar,
    idle: Condvar,
    shared: Arc<Shared>,
    #[cfg(test)]
    before_write: Mutex<Option<Box<dyn Fn() + Send>>>,
}

/// What a write came to (for the status bar: a failure is reported once per session).
#[derive(Debug)]
pub enum Outcome {
    /// The copy of `paths` is on disk.
    Written(EntryPaths),
    /// Writing failed; retried with the next change.
    Failed(io::Error),
}

/// The writer thread and its latest-wins slot (module docs).
pub struct Writer {
    inner: Arc<WriterInner>,
    thread: Option<JoinHandle<()>>,
    outcomes: mpsc::Receiver<Outcome>,
}

impl Writer {
    /// Starts the thread.
    pub fn start(shared: Arc<Shared>) -> Writer {
        let inner = Arc::new(WriterInner {
            slot: Mutex::default(),
            wake: Condvar::new(),
            idle: Condvar::new(),
            shared,
            #[cfg(test)]
            before_write: Mutex::new(None),
        });
        let (send, outcomes) = mpsc::channel();
        let worker = Arc::clone(&inner);
        let thread = std::thread::Builder::new()
            .name("komponist-recovery".to_owned())
            .spawn(move || run(&worker, &send))
            .ok();
        if thread.is_none() {
            eprintln!("komponist: recovery: the writer thread did not start");
        }
        Writer {
            inner,
            thread,
            outcomes,
        }
    }

    fn post(&self, put: impl FnOnce(&mut Slot)) {
        let mut slot = lock(&self.inner.slot);
        put(&mut slot);
        self.inner.wake.notify_one();
    }

    /// Writes `project` as the copy of `paths` (replaces a queued write).
    pub fn write(&self, paths: &EntryPaths, project: Arc<Project>, base: MetaBase) {
        let paths = paths.clone();
        self.post(|slot| {
            slot.write = Some(Job::Write {
                paths,
                project,
                base,
            })
        });
    }

    /// Deletes the copy of `paths`; with `lock`, also the lock file (the document closed). A
    /// queued write of the same entry is dropped.
    pub fn delete(&self, paths: &EntryPaths, lock: Option<File>) {
        let paths = paths.clone();
        self.post(|slot| {
            if matches!(&slot.write, Some(Job::Write { paths: queued, .. }) if *queued == paths) {
                slot.write = None;
            }
            slot.queue.push_back(Job::Delete { paths, lock });
        });
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

    /// The handle the panic hook queues `ended_by` with.
    fn hook_handle(&self) -> Arc<WriterInner> {
        Arc::clone(&self.inner)
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

fn run(inner: &WriterInner, outcomes: &mpsc::Sender<Outcome>) {
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
            Job::Write {
                paths,
                project,
                base,
            } => {
                #[cfg(test)]
                if let Some(hook) = &*lock(&inner.before_write) {
                    hook();
                }
                let meta = RecoveryMeta::at(&base, SystemTime::now());
                let written = {
                    let _io = lock(&shared.io);
                    write_entry(&paths, &project, &meta)
                };
                let outcome = match written {
                    Ok(()) => {
                        let mut state = lock(&shared.state);
                        if state.entry.as_ref() == Some(&paths) {
                            state.written = Some(meta);
                        }
                        Outcome::Written(paths)
                    }
                    Err(error) => {
                        eprintln!("komponist: recovery copy not written: {error}");
                        Outcome::Failed(error)
                    }
                };
                let _ = outcomes.send(outcome);
            }
            Job::Delete { paths, lock: held } => {
                {
                    let _io = lock(&shared.io);
                    for path in [&paths.copy, &paths.meta] {
                        if let Err(error) = remove_if_present(path) {
                            eprintln!("komponist: recovery: {}: {error}", path.display());
                        }
                    }
                }
                {
                    let mut state = lock(&shared.state);
                    if state.entry.as_ref() == Some(&paths) {
                        state.written = None;
                    }
                }
                if let Some(file) = held {
                    drop(file);
                    let _ = fs::remove_file(&paths.lock);
                }
            }
            Job::MarkPanic(ended_by) => {
                let (entry, meta) = {
                    let state = lock(&shared.state);
                    (state.entry.clone(), state.written.clone())
                };
                if let (Some(entry), Some(mut meta)) = (entry, meta) {
                    meta.ended_by = ended_by;
                    let _io = lock(&shared.io);
                    if entry.copy.exists()
                        && let Err(error) = meta.write(&entry.meta)
                    {
                        eprintln!("komponist: recovery: {}: {error}", entry.meta.display());
                    }
                }
            }
        }
    }
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
    pub deferred: &'a [(DeviceId, Arc<[u8]>)],
    /// Its project file.
    pub path: Option<&'a Path>,
}

/// The recovery of one app session: the store, the writer, this document's entry and the copies
/// other sessions left. Disabled ([`Recovery::disabled`], the default) it does nothing at all, so
/// tests never touch a real folder.
pub struct Recovery {
    store: Option<Store>,
    shared: Arc<Shared>,
    writer: Option<Writer>,
    clock: Clock,
    schedule: Schedule,
    entry: Option<Entry>,
    /// The project and deferred-state count last seen ([`Recovery::update`] compares).
    seen: Option<(Arc<Project>, usize)>,
    /// A copy of this entry is on disk or queued.
    has_copy: bool,
    /// A write failure was reported in this session.
    reported: bool,
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
            entry: None,
            seen: None,
            has_copy: false,
            reported: false,
            candidates: Vec::new(),
        }
    }

    /// Recovery in `store`, on `clock`.
    pub fn new(store: Store, clock: Clock) -> Recovery {
        let shared = Arc::<Shared>::default();
        let writer = Writer::start(Arc::clone(&shared));
        Recovery {
            store: Some(store),
            shared,
            writer: Some(writer),
            clock,
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

    /// This document's entry, once it has one.
    pub fn entry(&self) -> Option<&Entry> {
        self.entry.as_ref()
    }

    /// Reads the folder for copies of other sessions (module docs); replaces the list.
    pub fn scan(&mut self) {
        let own = self.entry.as_ref().map(|e| e.id.clone());
        self.candidates = self
            .store
            .as_ref()
            .map(Store::scan)
            .unwrap_or_default()
            .into_iter()
            .filter(|c| Some(&c.id) != own.as_ref())
            .collect();
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

    /// The candidate of project file `path`, if one is waiting.
    pub fn candidate_for(&self, path: &Path) -> Option<&Candidate> {
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        self.candidates
            .iter()
            .find(|c| c.project_path().is_some_and(|p| same(p, path)))
    }

    /// Moves candidate `id` to `discarded/`.
    pub fn discard(&mut self, id: &Id) -> io::Result<()> {
        let (Some(store), Some(candidate)) = (self.store.clone(), self.take_candidate(id)) else {
            return Ok(());
        };
        store.discard(candidate)
    }

    /// One frame: publishes a changed document for the panic hook, hands a due copy to the writer,
    /// deletes the copy of a clean document. Returns the write failure to report (once per
    /// session) and how long until the next copy is due.
    pub fn update(&mut self, doc: DocumentState<'_>) -> (Option<io::Error>, Option<Duration>) {
        let failure = self.poll();
        let Some(store) = self.store.clone() else {
            return (None, None);
        };
        if !doc.dirty {
            if self.has_copy
                && let (Some(entry), Some(writer)) = (&self.entry, &self.writer)
            {
                writer.delete(&entry.paths, None);
            }
            self.has_copy = false;
            self.schedule.written();
            self.seen = None;
            self.shared.unpublish();
            return (failure, None);
        }
        if self.entry.is_none() {
            match store.create_entry() {
                Ok(entry) => self.entry = Some(entry),
                Err(error) => {
                    eprintln!("komponist: recovery: {}: {error}", store.root().display());
                    return (failure.or_else(|| self.report(error)), None);
                }
            }
        }
        let (Some(entry), Some(writer)) = (&self.entry, &self.writer) else {
            return (failure, None);
        };
        let base = MetaBase {
            project_path: doc.path.map(Path::to_path_buf),
            name: doc.project.name.clone(),
        };
        let now = (self.clock)();
        let changed = self.seen.as_ref().is_none_or(|(project, deferred)| {
            !Arc::ptr_eq(project, doc.project) || *deferred != doc.deferred.len()
        });
        if changed {
            self.schedule.changed(now);
            self.seen = Some((Arc::clone(doc.project), doc.deferred.len()));
            self.shared
                .publish(&entry.paths, Arc::clone(doc.project), base.clone());
        }
        if self.schedule.due(now) {
            writer.write(&entry.paths, with_deferred(doc.project, doc.deferred), base);
            self.has_copy = true;
            self.schedule.written();
        }
        let due = self.schedule.due_at();
        (failure, due.map(|at| at.saturating_duration_since(now)))
    }

    /// The writer's failures since the last frame; the first one of the session is returned.
    fn poll(&mut self) -> Option<io::Error> {
        let outcomes = self.writer.as_ref().map(Writer::outcomes)?;
        let mut first = None;
        for outcome in outcomes {
            if let Outcome::Failed(error) = outcome {
                first = first.or_else(|| self.report(error));
            }
        }
        first
    }

    fn report(&mut self, error: io::Error) -> Option<io::Error> {
        (!std::mem::replace(&mut self.reported, true)).then_some(error)
    }

    /// The open document is replaced (another project opened, a new one, the start screen after
    /// „Nicht speichern“): its copy and lock go. `adopt` is the entry of a recovered copy that the
    /// new document continues.
    pub fn document_replaced(&mut self, adopt: Option<(Entry, Arc<Project>)>) {
        if let Some(entry) = self.entry.take() {
            self.shared.forget(&entry.paths);
            match &self.writer {
                Some(writer) => writer.delete(&entry.paths, Some(entry.lock)),
                None => drop(entry),
            }
        }
        self.has_copy = false;
        self.schedule.written();
        self.seen = None;
        if let Some((entry, project)) = adopt {
            // The copy on disk holds this project already: written again after the next change.
            self.seen = Some((project, 0));
            self.has_copy = true;
            self.entry = Some(entry);
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

    /// What [`install_panic_hook`] needs; `None` while recovery is off.
    pub fn panic_handle(&self) -> Option<PanicHandle> {
        Some(PanicHandle {
            shared: Arc::clone(&self.shared),
            writer: self.writer.as_ref()?.hook_handle(),
            ui: std::thread::current().id(),
        })
    }
}

/// `project` with the deferred plugin states put into their devices (as a save does).
fn with_deferred(project: &Arc<Project>, deferred: &[(DeviceId, Arc<[u8]>)]) -> Arc<Project> {
    let mut out: Option<Project> = None;
    for (device, state) in deferred {
        let base = out.as_ref().unwrap_or(project);
        let current = base.find_device(*device).map(|(_, _, d)| d.state.clone());
        if current.flatten().as_ref() == Some(state) {
            continue;
        }
        if let Ok(next) =
            komponist_model::edits::set_device_state(base, *device, Some(Arc::clone(state)))
        {
            out = Some(next);
        }
    }
    out.map_or_else(|| Arc::clone(project), Arc::new)
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

/// The UI thread panicked: writes the last published project as the copy, marked `ended_by`.
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
    let Some(entry) = entry else {
        return;
    };
    // Let a write in progress finish, so it cannot land after this one.
    let deadline = Instant::now() + HOOK_IO_WAIT;
    let _io = loop {
        if let Some(guard) = try_lock(&shared.io) {
            break Some(guard);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let result = match (published, written) {
        (Some((project, base)), _) => {
            let mut meta = RecoveryMeta::at(&base, SystemTime::now());
            meta.ended_by = ended_by;
            write_entry(&entry, &project, &meta)
        }
        // Clean when it panicked, but a copy is still on disk: mark it.
        (None, Some(mut meta)) if entry.copy.exists() => {
            meta.ended_by = ended_by;
            meta.write(&entry.meta)
        }
        (None, _) => Ok(()),
    };
    if let Err(error) = result {
        eprintln!("komponist: recovery copy not written while panicking: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        schedule.written();
        assert_eq!(schedule.due_at(), None);
    }

    #[test]
    fn a_dirty_document_gets_a_copy_after_two_seconds_and_a_clean_one_loses_it() {
        let dir = scratch("debounce");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let p1 = named("Night Drive");
        // Clean: no entry, nothing written.
        r.update(doc(false, &p1));
        assert!(r.entry().is_none());
        // Dirty: an entry and its lock at once, the copy 2 s later.
        let (_, due) = r.update(doc(true, &p1));
        let paths = r.entry().unwrap().paths.clone();
        assert!(paths.lock.exists());
        assert_eq!(due, Some(QUIET));
        advance(&now, Duration::from_millis(1900));
        r.update(doc(true, &p1));
        assert!(r.flush(Duration::from_secs(5)));
        assert!(!paths.copy.exists(), "not before 2 s");
        advance(&now, Duration::from_millis(100));
        r.update(doc(true, &p1));
        assert!(r.flush(Duration::from_secs(5)));
        let (loaded, _) = load_project_repaired(&paths.copy).unwrap();
        assert_eq!(loaded, *p1);
        let meta = RecoveryMeta::read(&paths.meta).unwrap();
        assert_eq!(meta.name, "Night Drive");
        assert_eq!(meta.ended_by, EndedBy::Unknown);
        assert_eq!(meta.app_version, env!("CARGO_PKG_VERSION"));
        // Nothing new: nothing written again.
        advance(&now, Duration::from_secs(60));
        assert_eq!(r.update(doc(true, &p1)).1, None);
        // Clean again (saved, or undone to the saved state): the copy goes, the lock stays.
        r.update(doc(false, &p1));
        assert!(r.flush(Duration::from_secs(5)));
        assert!(!paths.copy.exists() && !paths.meta.exists());
        assert!(paths.lock.exists());
        // Closing the document removes the lock file too.
        r.document_replaced(None);
        assert!(r.flush(Duration::from_secs(5)));
        assert!(!paths.lock.exists());
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn continuous_changes_are_written_every_thirty_seconds() {
        let dir = scratch("max-age");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let mut writes = 0;
        let mut last_copy = None;
        for i in 0..62 {
            let p = named(&format!("Take {i}"));
            r.update(doc(true, &p));
            assert!(r.flush(Duration::from_secs(5)));
            let paths = r.entry().unwrap().paths.clone();
            if let Ok(meta) = fs::read_to_string(&paths.meta)
                && Some(&meta) != last_copy.as_ref()
            {
                writes += 1;
                last_copy = Some(meta);
            }
            advance(&now, Duration::from_secs(1));
        }
        // A change every second from 0 to 61 s: never two quiet seconds, so the copies come at
        // 30 s (30 s after the change at 0 s) and at 61 s (30 s after the change at 31 s).
        assert_eq!(writes, 2);
        r.shutdown();
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_slow_writer_writes_only_the_latest_project() {
        let dir = scratch("latest");
        let store = Store::new(&dir);
        let entry = store.create_entry().unwrap();
        let writer = Writer::start(Arc::default());
        let (started_tx, started) = mpsc::channel();
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let count = Arc::new(Mutex::new(0));
        let counted = Arc::clone(&count);
        writer.set_before_write(move || {
            *counted.lock().unwrap() += 1;
            let _ = started_tx.send(());
            let _ = gate.lock().unwrap().recv_timeout(Duration::from_secs(5));
        });
        writer.write(&entry.paths, named("v1"), base("v1"));
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        // v1 is being written; v2 and v3 queue, v3 replaces v2.
        writer.write(&entry.paths, named("v2"), base("v2"));
        writer.write(&entry.paths, named("v3"), base("v3"));
        release.send(()).unwrap();
        release.send(()).unwrap();
        assert!(writer.wait_idle(Duration::from_secs(5)));
        assert_eq!(*count.lock().unwrap(), 2, "v2 was never written");
        let (loaded, _) = load_project_repaired(&entry.paths.copy).unwrap();
        assert_eq!(loaded.name, "v3");
        // A delete drops a queued write of the same entry.
        let (release_again, gate_again) = mpsc::channel::<()>();
        let gate_again = Mutex::new(gate_again);
        writer.set_before_write(move || {
            let _ = gate_again
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5));
        });
        writer.write(&entry.paths, named("v4"), base("v4"));
        writer.delete(&entry.paths, None);
        release_again.send(()).unwrap();
        assert!(writer.wait_idle(Duration::from_secs(5)));
        assert!(!entry.paths.copy.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_panic_on_another_thread_only_marks_the_meta_through_the_writer() {
        let dir = scratch("mark");
        let (clock, now) = fake_clock();
        let mut r = Recovery::new(Store::new(&dir), clock);
        let p = named("Night Drive");
        r.update(doc(true, &p));
        advance(&now, QUIET);
        r.update(doc(true, &p));
        assert!(r.flush(Duration::from_secs(5)));
        let paths = r.entry().unwrap().paths.clone();
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
        assert!(r.flush(Duration::from_secs(5)));
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
        first.update(doc(true, &p));
        advance(&now, QUIET);
        first.update(doc(true, &p));
        assert!(first.flush(Duration::from_secs(5)));
        // A second store (another komponist) does not see the copy of the live session.
        let second = Store::new(&dir);
        assert!(second.scan().is_empty());
        // The session ends without deleting its copy (a crash): now the copy is offered.
        let id = first.entry().unwrap().id.clone();
        let Recovery { writer, entry, .. } = first;
        drop(writer);
        drop(entry);
        let found = second.scan();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, id);
        assert_eq!(found[0].name(), Some("Hafenlicht"));
        assert_eq!(found[0].file_state(), FileState::Never);
        assert_eq!(found[0].project().unwrap().name, "Hafenlicht");
        // While the first scan holds the candidate, a third one does not see it either.
        assert!(Store::new(&dir).scan().is_empty());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn candidates_are_sorted_newest_first_and_stale_locks_go() {
        let dir = scratch("scan");
        let store = Store::new(&dir);
        for (name, ms) in [("Alt", 1_000), ("Neu", 3_000), ("Mitte", 2_000)] {
            let entry = store.create_entry().unwrap();
            let mut meta = RecoveryMeta::at(&base(name), SystemTime::now());
            meta.written_ms = ms;
            write_entry(&entry.paths, &Project::new(name), &meta).unwrap();
        }
        // A lock without a copy: a session that ended clean.
        let stale = store.create_entry().unwrap();
        let stale_lock = stale.paths.lock.clone();
        drop(stale);
        let names: Vec<_> = store
            .scan()
            .iter()
            .map(|c| c.name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["Neu", "Mitte", "Alt"]);
        assert!(!stale_lock.exists());
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
        let state = |store: &Store| store.scan().remove(0).file_state();
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
        let found = store.scan();
        let reason = found[0]
            .loaded
            .as_ref()
            .unwrap_err()
            .reason(komponist_i18n::Lang::De);
        assert_eq!(reason, "Dateiversion 99 ist neuer als dieses komponist");
        assert_eq!(found[0].name(), Some("Night Drive"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn discarded_copies_wait_seven_days_in_their_folder() {
        let dir = scratch("discard");
        let store = Store::new(&dir);
        let entry = store.create_entry().unwrap();
        let id = entry.id.clone();
        write_entry(
            &entry.paths,
            &Project::new("Weg"),
            &RecoveryMeta::at(&base("Weg"), SystemTime::now()),
        )
        .unwrap();
        drop(entry);
        let candidate = store.scan().remove(0);
        store.discard(candidate).unwrap();
        assert!(store.scan().is_empty());
        let kept = dir.join(DISCARDED).join(format!("{id}.komponist"));
        assert!(kept.exists());
        store.prune_discarded(SystemTime::now() + KEEP_DISCARDED - Duration::from_secs(60));
        assert!(kept.exists());
        store.prune_discarded(SystemTime::now() + KEEP_DISCARDED + Duration::from_secs(60));
        assert!(!kept.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn deferred_plugin_states_go_into_the_copy() {
        let project = komponist_model::fixtures::demo_project();
        let device = project.tracks[0].channel.devices[0].id;
        let project =
            Arc::new(komponist_model::edits::set_device_state(&project, device, None).unwrap());
        let state: Arc<[u8]> = Arc::from(&b"state"[..]);
        let out = with_deferred(&project, &[(device, Arc::clone(&state))]);
        assert_eq!(
            out.find_device(device).unwrap().2.state.as_deref(),
            Some(&b"state"[..])
        );
        // Nothing deferred: the same project, no copy.
        assert!(Arc::ptr_eq(&with_deferred(&project, &[]), &project));
    }
}
