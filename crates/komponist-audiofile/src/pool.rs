//! The file pool: one [`PoolFile`] per audio file on disk, shared by everyone who uses it.
//!
//! Key: the file's identity ([`FileId`]: canonical path, size, modification time), so two links
//! to the same file (a symlink, `./x` and `x`, two Samplers, a Sampler pad and an audio clip) are
//! one entry, and a file that changed on disk is a new entry. The pool holds its entries weakly:
//! an entry lives as long as somebody holds the `Arc<PoolFile>` (the refcount) and goes away with
//! the last user, taking its memory (decoded audio, derived data, peaks) with it. Hold the
//! `Arc<PoolFile>` for as long as you use the file.
//!
//! What an entry shares, each computed once however many users ask:
//! - the header ([`PoolFile::info`], read when the entry is created);
//! - the decoded audio ([`PoolFile::decoded`], weak: it is in memory while somebody holds the
//!   `Arc<Decoded>`) and data derived from it ([`PoolFile::derived`], strong, e.g. the Sampler's
//!   `Arc<Sample>`, so two Samplers with one file share one buffer);
//! - the waveform peaks ([`PoolFile::peaks`], [`PoolFile::request_peaks`]): in memory, then in
//!   the disk cache if the pool has a cache folder.
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
use std::time::UNIX_EPOCH;

use crate::AudioError;
use crate::decode::{AudioFile, AudioInfo, Decoded, decode_all};
use crate::peaks::{PeakData, cache};

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
    /// Modification time in nanoseconds since the Unix epoch (0 if the OS does not say).
    pub mtime_ns: u64,
}

impl FileId {
    /// The identity of the file at `path`.
    pub fn of(path: &Path) -> Result<FileId, AudioError> {
        let path = std::fs::canonicalize(path)?;
        let meta = std::fs::metadata(&path)?;
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
        Ok(FileId {
            path,
            len: meta.len(),
            mtime_ns,
        })
    }

    /// A hash that is the same in every run and build (FNV-1a), for file names of the disk cache.
    pub fn stable_hash(&self) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut eat = |bytes: &[u8]| {
            for b in bytes {
                h ^= u64::from(*b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        };
        eat(self.path.to_string_lossy().as_bytes());
        eat(&self.len.to_le_bytes());
        eat(&self.mtime_ns.to_le_bytes());
        h
    }
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

struct Job {
    file: Arc<PoolFile>,
}

struct PoolInner {
    entries: Mutex<HashMap<FileId, Weak<PoolFile>>>,
    cache_dir: Mutex<Option<PathBuf>>,
    /// The peak worker's queue, started with the first request.
    worker: Mutex<Option<Sender<Job>>>,
    /// Test hook: the next peak computation panics.
    panic_next: AtomicBool,
}

/// Starts the peak worker: one thread, ends when the pool and all entries are gone and the queue
/// is empty.
fn spawn_worker() -> Sender<Job> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        .name("komponist-peaks".into())
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                job.file.compute_peaks();
            }
        })
        .expect("the peak worker starts");
    tx
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
                worker: Mutex::new(None),
                panic_next: AtomicBool::new(false),
            }),
        }
    }

    /// The process-wide pool: the one the Sampler's loader and the audio clips share.
    pub fn global() -> &'static FilePool {
        static GLOBAL: OnceLock<FilePool> = OnceLock::new();
        GLOBAL.get_or_init(FilePool::new)
    }

    /// Sets the folder of the on-disk peak cache (`None`: memory only). It is the caller's
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

    /// The peak cache folder, if one is set.
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

    /// Asks the pool's worker thread for the peaks (disk cache first, else a streaming decode)
    /// and returns the state now. `on_done` runs on the worker thread when they are ready or
    /// have failed (not if they already were); the UI passes a repaint request. Many callers share
    /// one computation; the worker handles one file at a time.
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
        let mut worker = lock(&self.pool.worker);
        let mut job = Job {
            file: Arc::clone(self),
        };
        // A worker that died (its receiver is gone) is replaced once; if the new one is gone
        // too, the request fails instead of staying pending forever.
        for _ in 0..2 {
            let tx = worker.get_or_insert_with(spawn_worker);
            match tx.send(job) {
                Ok(()) => return PeakState::Pending,
                Err(mpsc::SendError(back)) => {
                    job = back;
                    *worker = None;
                }
            }
        }
        drop(worker);
        let err = AudioError::Malformed("peak worker unavailable".into());
        self.finish_peaks(Err(err.clone()));
        PeakState::Failed(err)
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
        for callback in callbacks {
            callback();
        }
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
