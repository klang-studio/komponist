//! The file pool: one [`PoolFile`] per audio file on disk, shared by everyone who uses it.
//!
//! Key: the file's identity ([`FileId`]: canonical path, size, modification time), so two links
//! to the same file (a symlink, `./x` and `x`, two Samplers, a Sampler pad and an audio clip) are
//! one entry, and a file that changed on disk is a new entry. The pool holds its entries weakly:
//! an entry lives as long as somebody holds the `Arc<PoolFile>` (the refcount) and goes away with
//! the last user, taking its memory (decoded audio, derived data, peaks, tempo) with it. Hold the
//! `Arc<PoolFile>` for as long as you use the file.
//!
//! What an entry shares, each computed once however many users ask:
//! - the header ([`PoolFile::info`], read when the entry is created);
//! - the decoded audio ([`PoolFile::decoded`], weak: it is in memory while somebody holds the
//!   `Arc<Decoded>`) and data derived from it ([`PoolFile::derived`], strong, e.g. the Sampler's
//!   `Arc<Sample>`, so two Samplers with one file share one buffer);
//! - the waveform peaks ([`PoolFile::peaks`], [`PoolFile::request_peaks`]): in memory, then in
//!   the disk cache if the pool has a cache folder;
//! - the tempo estimate ([`PoolFile::request_tempo`]) with the same memory/disk cache lifetime.
//!
//! Streaming users (#11) open their own [`AudioFile`] on [`PoolFile::path`]: a decoder has a
//! position, so it is not shared.
//!
//! Threads: every method may be called from any thread. [`PoolFile::decoded`] and
//! [`PoolFile::derived`] block while another thread decodes the same file, then return its result.
//! None of this is for the audio thread.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::AudioError;
use crate::decode::{AudioFile, AudioInfo, Decoded, decode_all};
use crate::peaks::{PeakData, cache};
use crate::tempo::{self, TempoEstimate};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The identity of an audio file on disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileId {
    /// Canonical path (symlinks resolved).
    pub path: PathBuf,
    /// Size in bytes.
    pub len: u64,
    /// Signed nanoseconds relative to the Unix epoch, if available.
    pub mtime_ns: Option<i128>,
}

impl FileId {
    /// The identity of the file at `path`.
    pub fn of(path: &Path) -> Result<FileId, AudioError> {
        let path = std::fs::canonicalize(path)?;
        let meta = std::fs::metadata(&path)?;
        let mtime_ns = meta.modified().ok().map(signed_nanos);
        Ok(FileId {
            path,
            len: meta.len(),
            mtime_ns,
        })
    }

    /// Platform-stable FNV-1a hash of the native path and the full metadata identity.
    pub fn stable_hash(&self) -> u64 {
        let path = self.path.as_os_str();
        #[cfg(unix)]
        let path_bytes = {
            use std::os::unix::ffi::OsStrExt;
            path.as_bytes().iter().copied()
        };
        #[cfg(windows)]
        let path_bytes = {
            use std::os::windows::ffi::OsStrExt;
            path.encode_wide().flat_map(u16::to_le_bytes)
        };
        #[cfg(not(any(unix, windows)))]
        let path_bytes = path.as_encoded_bytes().iter().copied();

        extend_hash(
            FNV_OFFSET,
            path_bytes
                .chain(self.len.to_le_bytes())
                .chain(self.mtime_bytes()),
        )
    }

    /// The timestamp representation used by the current analysis caches.
    pub(crate) fn mtime_bytes(&self) -> [u8; 16] {
        // A SystemTime's i128 nanoseconds never reach the sentinel.
        self.mtime_ns.unwrap_or(i128::MIN).to_le_bytes()
    }

    /// Legacy KPK2 keys collapsed non-Unicode paths and pre-epoch or missing times.
    /// Only fall back for identities that cannot have one of those aliases.
    pub(crate) fn legacy_peak_key(&self) -> Option<(u64, u64)> {
        let path = self.path.to_str()?;
        let mtime = u64::try_from(self.mtime_ns?).ok()?;
        if path.contains(char::REPLACEMENT_CHARACTER) || mtime == 0 {
            return None;
        }
        let hash = extend_hash(
            FNV_OFFSET,
            path.bytes()
                .chain(self.len.to_le_bytes())
                .chain(mtime.to_le_bytes()),
        );
        Some((hash, mtime))
    }
}

fn signed_nanos(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(before_epoch) => -(before_epoch.duration().as_nanos() as i128),
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn extend_hash(seed: u64, bytes: impl IntoIterator<Item = u8>) -> u64 {
    bytes.into_iter().fold(seed, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Where the waveform peaks of a [`PoolFile`] are.
#[derive(Debug, Clone)]
pub enum PeakState {
    /// Nobody asked yet ([`PoolFile::request_peaks`]).
    Idle,
    /// Being computed on the worker thread.
    Pending,
    /// Done.
    Ready(Arc<PeakData>),
    /// Could not be computed.
    Failed(AudioError),
}

type Callback = Box<dyn FnOnce() + Send>;

enum PeakSlot {
    Idle,
    Pending(Vec<Callback>),
    Ready(Arc<PeakData>),
    Failed(AudioError),
}

/// State of off-thread tempo analysis. A ready result can have no credible beat period.
#[derive(Debug, Clone)]
pub enum TempoState {
    Idle,
    Pending,
    Ready(Option<TempoEstimate>),
    Failed(AudioError),
}

enum TempoSlot {
    Idle,
    Pending(Vec<Callback>),
    Done(Result<Option<TempoEstimate>, AudioError>),
}

struct PoolInner {
    entries: Mutex<HashMap<FileId, Weak<PoolFile>>>,
    cache_dir: Mutex<Option<PathBuf>>,
    peak_worker: Worker,
    tempo_worker: Worker,
    /// Test hook: the next peak computation panics.
    panic_next: AtomicBool,
}

/// A lazy FIFO lane. The worker exits after the pool drops the sender and the queue drains.
struct Worker {
    name: &'static str,
    compute: fn(&PoolFile),
    sender: Mutex<Option<Sender<Arc<PoolFile>>>>,
    #[cfg(test)]
    fail_next_spawn: AtomicBool,
}

impl Worker {
    fn new(name: &'static str, compute: fn(&PoolFile)) -> Self {
        Self {
            name,
            compute,
            sender: Mutex::new(None),
            #[cfg(test)]
            fail_next_spawn: AtomicBool::new(false),
        }
    }

    fn enqueue(&self, mut file: Arc<PoolFile>) -> Result<(), AudioError> {
        let mut sender = lock(&self.sender);
        // If a receiver has gone away, replace it once and retry the rejected send.
        for _ in 0..2 {
            let tx = match &mut *sender {
                Some(tx) => tx,
                None => sender.insert(self.spawn()?),
            };
            match tx.send(file) {
                Ok(()) => return Ok(()),
                Err(mpsc::SendError(back)) => {
                    file = back;
                    *sender = None;
                }
            }
        }
        Err(AudioError::Io(format!("{} worker unavailable", self.name)))
    }

    fn spawn(&self) -> Result<Sender<Arc<PoolFile>>, AudioError> {
        #[cfg(test)]
        if self.fail_next_spawn.swap(false, Ordering::SeqCst) {
            return Err(AudioError::Io("injected worker startup failure".into()));
        }

        let compute = self.compute;
        let (tx, rx) = mpsc::channel::<Arc<PoolFile>>();
        std::thread::Builder::new()
            .name(self.name.into())
            .spawn(move || {
                while let Ok(file) = rx.recv() {
                    compute(&file);
                }
            })
            .map_err(|err| AudioError::Io(format!("could not start {}: {err}", self.name)))?;
        Ok(tx)
    }
}

fn notify(callbacks: Vec<Callback>) {
    for callback in callbacks {
        // A callback belongs to its caller. Keep the remaining callbacks and queued jobs alive.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback));
    }
}

/// The pool. Cheap to clone (a handle); the process-wide one is [`FilePool::global`].
#[derive(Clone)]
pub struct FilePool {
    inner: Arc<PoolInner>,
}

impl Default for FilePool {
    fn default() -> Self {
        FilePool::new()
    }
}

impl std::fmt::Debug for FilePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FilePool({} files)", self.live_files())
    }
}

impl FilePool {
    /// An empty pool without a disk cache. Tests use their own; the app uses [`FilePool::global`].
    pub fn new() -> FilePool {
        FilePool {
            inner: Arc::new(PoolInner {
                entries: Mutex::new(HashMap::new()),
                cache_dir: Mutex::new(None),
                peak_worker: Worker::new("komponist-peaks", PoolFile::compute_peaks),
                tempo_worker: Worker::new("komponist-tempo", PoolFile::compute_tempo),
                panic_next: AtomicBool::new(false),
            }),
        }
    }

    /// The process-wide pool: the one the Sampler's loader and the audio clips share.
    pub fn global() -> &'static FilePool {
        static GLOBAL: OnceLock<FilePool> = OnceLock::new();
        GLOBAL.get_or_init(FilePool::new)
    }

    /// Sets the folder of the on-disk analysis cache (`None`: memory only). It is the caller's
    /// choice, normally a folder in the user's cache directory; the pool creates it on the first write.
    pub fn set_cache_dir(&self, dir: Option<PathBuf>) {
        *lock(&self.inner.cache_dir) = dir;
    }

    /// Makes the next peak computation panic, to test that the worker survives one. Not for use
    /// outside tests.
    #[doc(hidden)]
    pub fn inject_peak_panic_for_tests(&self) {
        self.inner.panic_next.store(true, Ordering::SeqCst);
    }

    /// The analysis cache folder, if one is set.
    pub fn cache_dir(&self) -> Option<PathBuf> {
        lock(&self.inner.cache_dir).clone()
    }

    /// The entry for the file at `path`: the existing one if somebody holds it, else a new one
    /// whose header is read now (cheap). Errors for a file that is missing or not audio.
    pub fn open(&self, path: &Path) -> Result<Arc<PoolFile>, AudioError> {
        let id = FileId::of(path)?;
        if let Some(file) = self.live(&id) {
            return Ok(file);
        }
        // Read the header outside the lock; another thread may have inserted the entry meanwhile.
        let info = *AudioFile::open(&id.path)?.info();
        let mut entries = lock(&self.inner.entries);
        entries.retain(|_, w| w.strong_count() > 0);
        if let Some(file) = entries.get(&id).and_then(Weak::upgrade) {
            return Ok(file);
        }
        let file = Arc::new(PoolFile {
            id: id.clone(),
            info,
            pool: Arc::clone(&self.inner),
            decoded: Mutex::new(Weak::new()),
            decodes: AtomicU32::new(0),
            derived: Mutex::new(HashMap::new()),
            peaks: Mutex::new(PeakSlot::Idle),
            tempo: Mutex::new(TempoSlot::Idle),
        });
        entries.insert(id, Arc::downgrade(&file));
        Ok(file)
    }

    fn live(&self, id: &FileId) -> Option<Arc<PoolFile>> {
        lock(&self.inner.entries).get(id).and_then(Weak::upgrade)
    }

    /// Entries somebody still holds.
    pub fn live_files(&self) -> usize {
        lock(&self.inner.entries)
            .values()
            .filter(|w| w.strong_count() > 0)
            .count()
    }
}

/// One audio file in the pool. See the module docs.
pub struct PoolFile {
    id: FileId,
    info: AudioInfo,
    pool: Arc<PoolInner>,
    decoded: Mutex<Weak<Decoded>>,
    decodes: AtomicU32,
    derived: Mutex<HashMap<TypeId, Arc<dyn Any + Send + Sync>>>,
    peaks: Mutex<PeakSlot>,
    tempo: Mutex<TempoSlot>,
}

impl std::fmt::Debug for PoolFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PoolFile")
            .field("path", &self.id.path)
            .field("info", &self.info)
            .finish()
    }
}

impl PoolFile {
    /// The file's identity.
    pub fn id(&self) -> &FileId {
        &self.id
    }

    /// The canonical path; open an [`AudioFile`] on it to stream.
    pub fn path(&self) -> &Path {
        &self.id.path
    }

    /// The header read when the entry was created.
    pub fn info(&self) -> &AudioInfo {
        &self.info
    }

    /// How often this entry decoded the whole file (for tests and diagnostics).
    pub fn decode_count(&self) -> u32 {
        self.decodes.load(Ordering::Relaxed)
    }

    /// The whole file, decoded at its own sample rate. Shared while somebody holds the `Arc`;
    /// otherwise decoded again. `max_seconds` refuses longer files ([`AudioError::TooLong`]).
    pub fn decoded(&self, max_seconds: Option<f64>) -> Result<Arc<Decoded>, AudioError> {
        if max_seconds.is_some_and(|m| self.info.duration_secs() > m) {
            return Err(AudioError::TooLong);
        }
        let mut slot = lock(&self.decoded);
        if let Some(d) = slot.upgrade() {
            return Ok(d);
        }
        let d = Arc::new(decode_all(&self.id.path, max_seconds)?);
        self.decodes.fetch_add(1, Ordering::Relaxed);
        *slot = Arc::downgrade(&d);
        Ok(d)
    }

    /// Data of type `T` built from the decoded file by `build`, once per entry however often and
    /// from whichever thread it is asked for; later calls return the same `Arc`. The decoded
    /// audio is handed to `build` as an `Arc` that is unique unless somebody else holds it, so
    /// `Arc::try_unwrap` moves the buffers instead of copying them. A different `build` for the
    /// same `T` is ignored once `T` exists: use one type per kind of data (the Sampler's `Sample`).
    pub fn derived<T: Any + Send + Sync>(
        &self,
        max_seconds: Option<f64>,
        build: impl FnOnce(Arc<Decoded>) -> T,
    ) -> Result<Arc<T>, AudioError> {
        let mut map = lock(&self.derived);
        if let Some(any) = map.get(&TypeId::of::<T>())
            && let Ok(t) = Arc::clone(any).downcast::<T>()
        {
            return Ok(t);
        }
        let value = Arc::new(build(self.decoded(max_seconds)?));
        map.insert(TypeId::of::<T>(), value.clone());
        Ok(value)
    }

    /// Where the waveform peaks are, without waiting.
    pub fn peaks_state(&self) -> PeakState {
        match &*lock(&self.peaks) {
            PeakSlot::Idle => PeakState::Idle,
            PeakSlot::Pending(_) => PeakState::Pending,
            PeakSlot::Ready(p) => PeakState::Ready(Arc::clone(p)),
            PeakSlot::Failed(e) => PeakState::Failed(e.clone()),
        }
    }

    /// The peaks if they are in memory, else `None`.
    pub fn peaks_ready(&self) -> Option<Arc<PeakData>> {
        match self.peaks_state() {
            PeakState::Ready(p) => Some(p),
            _ => None,
        }
    }

    /// Allows the next peak request to retry after the caller rechecks file availability.
    /// Successful and in-progress computations remain shared.
    pub fn allow_peak_retry(&self) {
        let mut slot = lock(&self.peaks);
        if matches!(*slot, PeakSlot::Failed(_)) {
            *slot = PeakSlot::Idle;
        }
    }

    /// Asks the pool's worker thread for the peaks (disk cache first, else a streaming decode)
    /// and returns the state now. `on_done` runs on completion, normally on the worker or on this
    /// thread if scheduling fails. Many callers share one computation; the waveform worker handles
    /// one file at a time independently of tempo analysis.
    pub fn request_peaks(self: &Arc<Self>, on_done: impl FnOnce() + Send + 'static) -> PeakState {
        {
            let mut slot = lock(&self.peaks);
            match &mut *slot {
                PeakSlot::Ready(p) => return PeakState::Ready(Arc::clone(p)),
                PeakSlot::Failed(e) => return PeakState::Failed(e.clone()),
                PeakSlot::Pending(callbacks) => {
                    callbacks.push(Box::new(on_done));
                    return PeakState::Pending;
                }
                PeakSlot::Idle => *slot = PeakSlot::Pending(vec![Box::new(on_done)]),
            }
        }
        match self.pool.peak_worker.enqueue(Arc::clone(self)) {
            Ok(()) => PeakState::Pending,
            Err(err) => {
                self.finish_peaks(Err(err.clone()));
                PeakState::Failed(err)
            }
        }
    }

    /// Return the original-tempo estimate without waiting or starting analysis.
    pub fn tempo_state(&self) -> TempoState {
        match &*lock(&self.tempo) {
            TempoSlot::Idle => TempoState::Idle,
            TempoSlot::Pending(_) => TempoState::Pending,
            TempoSlot::Done(Ok(estimate)) => TempoState::Ready(*estimate),
            TempoSlot::Done(Err(error)) => TempoState::Failed(error.clone()),
        }
    }

    /// Analyze the active file on the tempo worker. Requests share one result. A new request
    /// retries a previous failure. `on_done` runs on completion, on this thread if scheduling fails.
    pub fn request_tempo(self: &Arc<Self>, on_done: impl FnOnce() + Send + 'static) -> TempoState {
        {
            let mut slot = lock(&self.tempo);
            match &mut *slot {
                TempoSlot::Idle | TempoSlot::Done(Err(_)) => {
                    *slot = TempoSlot::Pending(vec![Box::new(on_done)]);
                }
                TempoSlot::Pending(callbacks) => {
                    callbacks.push(Box::new(on_done));
                    return TempoState::Pending;
                }
                TempoSlot::Done(Ok(estimate)) => return TempoState::Ready(*estimate),
            }
        }
        match self.pool.tempo_worker.enqueue(Arc::clone(self)) {
            Ok(()) => TempoState::Pending,
            Err(err) => {
                self.finish_tempo(Err(err.clone()));
                TempoState::Failed(err)
            }
        }
    }

    fn compute_tempo(&self) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir = lock(&self.pool.cache_dir).clone();
            if FileId::of(self.path())? != self.id {
                return Err(AudioError::Io(
                    "audio source changed before tempo analysis".into(),
                ));
            }
            if let Some(cached) = dir.as_ref().and_then(|dir| tempo::cached(dir, &self.id)) {
                return Ok(cached);
            }
            let estimate = tempo::estimate(&self.id)?;
            if let Some(dir) = &dir {
                let _ = tempo::cache(dir, &self.id, estimate);
            }
            Ok(estimate)
        }))
        .unwrap_or_else(|_| Err(AudioError::Malformed("tempo analysis panicked".into())));
        self.finish_tempo(result);
    }

    fn finish_tempo(&self, result: Result<Option<TempoEstimate>, AudioError>) {
        let old = std::mem::replace(&mut *lock(&self.tempo), TempoSlot::Done(result));
        if let TempoSlot::Pending(callbacks) = old {
            notify(callbacks);
        }
    }

    /// The peaks, waiting for them (computes them on the calling thread if nobody did). For
    /// worker threads and tests; the UI uses [`PoolFile::request_peaks`].
    pub fn peaks(&self) -> Result<Arc<PeakData>, AudioError> {
        match self.peaks_state() {
            PeakState::Ready(p) => return Ok(p),
            PeakState::Failed(e) => return Err(e),
            _ => {}
        }
        let result = self.build_peaks();
        self.finish_peaks(result.clone());
        result
    }

    /// Runs on the worker: a panic in a decoder fails this file's peaks, not the worker.
    fn compute_peaks(&self) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if self.pool.panic_next.swap(false, Ordering::SeqCst) {
                panic!("injected peak panic");
            }
            self.build_peaks()
        }))
        .unwrap_or_else(|_| Err(AudioError::Malformed("decoder panicked".into())));
        self.finish_peaks(result);
    }

    fn finish_peaks(&self, result: Result<Arc<PeakData>, AudioError>) {
        let callbacks = {
            let mut slot = lock(&self.peaks);
            let new = match result {
                Ok(p) => PeakSlot::Ready(p),
                Err(e) => PeakSlot::Failed(e),
            };
            match std::mem::replace(&mut *slot, new) {
                PeakSlot::Pending(callbacks) => callbacks,
                _ => Vec::new(),
            }
        };
        notify(callbacks);
    }

    fn build_peaks(&self) -> Result<Arc<PeakData>, AudioError> {
        let dir = lock(&self.pool.cache_dir).clone();
        if let Some(dir) = &dir
            && let Some(p) = cache::read(dir, &self.id)
        {
            return Ok(Arc::new(p));
        }
        let peaks = match lock(&self.decoded).upgrade() {
            Some(d) => PeakData::from_decoded(&d),
            None => PeakData::from_file(&mut AudioFile::open(&self.id.path)?)?,
        };
        if let Some(dir) = &dir {
            // Best effort: a cache that cannot be written is a cache that stays cold.
            let _ = cache::write(dir, &self.id, &peaks);
        }
        Ok(Arc::new(peaks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[cfg(any(unix, windows))]
    #[test]
    fn native_path_hash_preserves_distinct_invalid_unicode() {
        #[cfg(unix)]
        let paths = {
            use std::os::unix::ffi::OsStringExt;
            [vec![0xfe], vec![0xff]].map(std::ffi::OsString::from_vec)
        };
        #[cfg(windows)]
        let paths = {
            use std::os::windows::ffi::OsStringExt;
            [0xd800, 0xd801].map(|unit| std::ffi::OsString::from_wide(&[unit]))
        };
        let [a, b] = paths.map(|path| FileId {
            path: path.into(),
            len: 100,
            mtime_ns: Some(10),
        });
        assert_eq!(a.path.to_string_lossy(), b.path.to_string_lossy());
        assert_ne!(a.stable_hash(), b.stable_hash());
        assert!(a.legacy_peak_key().is_none());
        assert!(b.legacy_peak_key().is_none());
    }

    #[test]
    fn signed_mtimes_distinguish_missing_epoch_and_pre_epoch() {
        let base = FileId {
            path: "source.wav".into(),
            len: 100,
            mtime_ns: None,
        };
        let examples = [
            base.clone(),
            FileId {
                mtime_ns: Some(signed_nanos(UNIX_EPOCH)),
                ..base.clone()
            },
            FileId {
                mtime_ns: Some(signed_nanos(UNIX_EPOCH - Duration::from_secs(60))),
                ..base.clone()
            },
            FileId {
                mtime_ns: Some(signed_nanos(UNIX_EPOCH - Duration::from_secs(120))),
                ..base
            },
        ];
        assert_eq!(examples[1].mtime_ns, Some(0));
        assert_eq!(examples[2].mtime_ns, Some(-60_000_000_000));
        assert_eq!(examples[3].mtime_ns, Some(-120_000_000_000));
        assert_eq!(
            examples
                .iter()
                .map(FileId::stable_hash)
                .collect::<std::collections::HashSet<_>>()
                .len(),
            examples.len()
        );
    }

    #[test]
    fn worker_startup_failures_complete_pending_requests_and_tempo_can_retry() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stereo-440-880.flac");
        let pool = FilePool::new();
        let file = pool.open(&path).unwrap();
        let (tx, rx) = mpsc::channel();
        let notify_peaks = tx.clone();
        let notify_tempo = tx.clone();
        pool.inner
            .peak_worker
            .fail_next_spawn
            .store(true, Ordering::SeqCst);
        pool.inner
            .tempo_worker
            .fail_next_spawn
            .store(true, Ordering::SeqCst);
        assert!(matches!(
            file.request_peaks(move || notify_peaks.send("peaks failed").unwrap()),
            PeakState::Failed(AudioError::Io(why)) if why.contains("startup failure")
        ));
        assert_eq!(rx.try_recv().unwrap(), "peaks failed");
        assert!(matches!(file.peaks_state(), PeakState::Failed(_)));
        assert!(matches!(
            file.request_tempo(move || notify_tempo.send("tempo failed").unwrap()),
            TempoState::Failed(AudioError::Io(why)) if why.contains("startup failure")
        ));
        assert_eq!(rx.try_recv().unwrap(), "tempo failed");
        assert!(matches!(file.tempo_state(), TempoState::Failed(_)));
        assert!(matches!(
            file.request_tempo(move || tx.send("tempo retried").unwrap()),
            TempoState::Pending
        ));
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "tempo retried"
        );
        assert!(matches!(file.tempo_state(), TempoState::Ready(_)));
    }
}
