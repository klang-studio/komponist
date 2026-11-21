//! Sample loading for the sampler: decoding on a worker thread, handed to the audio thread
//! lock-free (WP-87).
//!
//! ```text
//!  UI thread                    loader thread                       audio thread
//!  SamplerHandle::load_link ──► file pool: open, decode ──► rtrb ──► SamplerDsp::process swaps the
//!  (mpsc request)               (once per file), peaks      slot, sends the old Arc back
//!                               fills the pad's PadView
//!                               drops what comes back ◄── rtrb ◄── (never frees on the audio thread)
//! ```
//!
//! The audio thread never allocates, locks or frees: a loaded sample arrives as an `Arc<Sample>`
//! through a wait-free queue, the slot's old `Arc` goes back through a second one, and the loader
//! thread drops it. What the UI shows of a pad (name, length, peaks, status) lives in a
//! [`PadView`] the loader fills under a mutex only the UI and loader threads take; the audio
//! thread writes the playheads and the last hit into atomics.
//!
//! Decoding and the files belong to the one file pool ([`komponist_audiofile::FilePool::global`],
//! #10), shared with the audio clips: WAV, FLAC, OGG Vorbis and MP3, mono, stereo, or the first
//! two channels of more. A file is decoded once however many pads and Samplers use it: the
//! pool entry keeps the finished `Arc<Sample>` (`PoolFile::derived`), every pad that loads the
//! file gets that same `Arc`, and the loader thread holds the entry for as long as a pad shows
//! it. Any sample rate is played back resampled (the voice interpolates), nothing is converted
//! here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_audiofile::{AudioError, Decoded, FilePool, PeakData, PoolFile};
use komponist_stretch::{CHUNK_FRAMES, Stretcher, output_length};
use rtrb::{Consumer, Producer, RingBuffer};
use sampler_core::Sample;
use sampler_core::slicing::detect_transients;

use crate::settings::{MAX_SLICES, Settings, Slice, SliceOptions};

use super::PADS;
use super::links::SampleLink;

/// Buckets of a pad's peak display.
pub const PEAK_BUCKETS: usize = 512;

/// Longest sample the sampler loads, in seconds (a safety limit for memory).
pub const MAX_SECONDS: f64 = 600.0;

/// Capacity of the queues between the threads.
const QUEUE: usize = 64;
const LOADER_SHUTDOWN_GRACE: Duration = Duration::from_millis(100);
const NO_SLICE_PREVIEW: u64 = f64::NAN.to_bits();
const SLICE_PREVIEW_RELEASED: u64 = 1 << 63;
const RELEASE_SLICE_PREVIEW: u64 = NO_SLICE_PREVIEW | SLICE_PREVIEW_RELEASED;

/// What the pad is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PadStatus {
    /// No sample.
    Empty,
    /// Decoding.
    Loading,
    /// Loaded and playable.
    Ready,
    /// The link does not resolve to a file.
    Missing,
    /// The file could not be read; the reason in German.
    Failed(String),
}

/// What the UI shows of one pad.
#[derive(Debug, Clone, PartialEq)]
pub struct PadView {
    /// State.
    pub status: PadStatus,
    /// The pad's name (the link's), empty without a sample.
    pub name: String,
    /// The link, if the pad has a sample.
    pub link: Option<SampleLink>,
    /// The sample's real length in seconds (0 while not loaded).
    pub length_s: f64,
    /// Absolute amplitude 0–1 over the sample in [`PEAK_BUCKETS`] buckets (empty while not loaded).
    pub peaks: Arc<[f32]>,
}

impl PadView {
    fn empty() -> PadView {
        PadView {
            status: PadStatus::Empty,
            name: String::new(),
            link: None,
            length_s: 0.0,
            peaks: Arc::from([]),
        }
    }
}

/// A sample for the audio thread (`None`: the pad is empty now).
pub struct Loaded {
    pub pad: usize,
    pub generation: u64,
    pub sample: Option<Arc<Sample>>,
}

/// The audio thread's ends of the queues.
pub struct AudioSide {
    pub incoming: Consumer<Loaded>,
    pub garbage: Producer<Option<Arc<Sample>>>,
    pub stretched: Option<Consumer<Stretched>>,
}

pub struct Stretched {
    pub generation: u64,
    pub ratio: u32,
    pub sample: Option<Arc<Sample>>,
}

pub const UNITY_RATIO: u32 = 1.0_f32.to_bits();
const STRETCH_SUCCEEDED: u64 = 1 << 32;

/// Availability of a general sampler's requested tempo-synced source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncStatus {
    /// The source is at the requested tempo or a stretched buffer has been prepared.
    Ready,
    /// The background worker is preparing the requested tempo.
    Preparing,
    /// Stretching failed or would exceed the preparation buffer limit.
    Unavailable,
}

#[derive(Default)]
struct AtomicSlice {
    time: AtomicU64,
    flags: AtomicU32,
    fade_in: AtomicU32,
    fade_out: AtomicU32,
}

impl AtomicSlice {
    fn store(&self, slice: Slice) {
        let reverse = slice
            .options
            .reverse
            .map_or(0, |reverse| if reverse { 2 } else { 1 });
        let flags = reverse | (u32::from(slice.options.gate) << 2);
        self.time.store(slice.time.to_bits(), Ordering::SeqCst);
        self.flags.store(flags, Ordering::SeqCst);
        self.fade_in.store(
            slice.options.fade_in_ms.unwrap_or(f32::NAN).to_bits(),
            Ordering::SeqCst,
        );
        self.fade_out.store(
            slice.options.fade_out_ms.unwrap_or(f32::NAN).to_bits(),
            Ordering::SeqCst,
        );
    }

    fn load(&self) -> Slice {
        let flags = self.flags.load(Ordering::SeqCst);
        let fade = |value: &AtomicU32| {
            let ms = f32::from_bits(value.load(Ordering::SeqCst));
            ms.is_finite().then_some(ms)
        };
        Slice {
            time: f64::from_bits(self.time.load(Ordering::SeqCst)),
            options: SliceOptions {
                reverse: match flags & 3 {
                    1 => Some(false),
                    2 => Some(true),
                    _ => None,
                },
                gate: flags & 4 != 0,
                fade_in_ms: fade(&self.fade_in),
                fade_out_ms: fade(&self.fade_out),
            },
        }
    }
}

pub enum SlicePreview {
    Start { time: f64, released: bool },
    Release,
}

struct GeneralShared {
    // UI writes serialize with each other. Audio reads the snapshot only if
    // both revisions agree; it never waits for a writer.
    slice_writer: Mutex<()>,
    slice_revision: AtomicU64,
    slices: [AtomicSlice; MAX_SLICES + 1],
    /// Latest press in source seconds. The sign bit retains a release that
    /// arrives before audio consumes the press. A signed empty value means release only.
    slice_preview: AtomicU64,
    transients: Mutex<Vec<f64>>,
    waveform_peaks: Mutex<Option<Arc<PeakData>>>,
    stretch_ratio: AtomicU32,
    stretch_revision: AtomicU64,
    completed: AtomicU64,
    completed_generation: AtomicU64,
    completed_revision: AtomicU64,
}

impl Default for GeneralShared {
    fn default() -> Self {
        Self {
            slice_writer: Mutex::new(()),
            slice_revision: AtomicU64::new(0),
            slices: std::array::from_fn(|_| AtomicSlice::default()),
            slice_preview: AtomicU64::new(NO_SLICE_PREVIEW),
            transients: Mutex::new(Vec::new()),
            waveform_peaks: Mutex::new(None),
            stretch_ratio: AtomicU32::new(UNITY_RATIO),
            stretch_revision: AtomicU64::new(0),
            completed: AtomicU64::new(u64::from(UNITY_RATIO) | STRETCH_SUCCEEDED),
            completed_generation: AtomicU64::new(0),
            completed_revision: AtomicU64::new(0),
        }
    }
}

/// State shared by the handle, the loader and the audio thread.
struct Shared {
    views: Mutex<Vec<PadView>>,
    general: Option<GeneralShared>,
    generations: [AtomicU64; PADS],
    /// Playhead of each pad as `f32` bits: a fraction of the sample, negative while silent.
    playheads: [AtomicU32; PADS],
    /// `counter << 4 | pad` of the last note that hit a pad; the counter starts at 1.
    hit: AtomicU32,
    /// Requests the loader has not finished.
    pending: AtomicUsize,
}

impl Shared {
    fn update<R>(&self, pad: usize, f: impl FnOnce(&mut PadView) -> R) -> R {
        let mut views = self.views.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut views[pad])
    }
}

/// The audio thread's side of the shared atomics.
#[derive(Clone)]
pub struct Meters(Arc<Shared>);

impl Meters {
    pub fn take_slice_preview(&self) -> Option<SlicePreview> {
        let general = self.0.general.as_ref()?;
        if general.slice_preview.load(Ordering::Relaxed) == NO_SLICE_PREVIEW {
            return None;
        }
        match general
            .slice_preview
            .swap(NO_SLICE_PREVIEW, Ordering::Relaxed)
        {
            NO_SLICE_PREVIEW => None,
            RELEASE_SLICE_PREVIEW => Some(SlicePreview::Release),
            bits => {
                let position = f64::from_bits(bits);
                Some(SlicePreview::Start {
                    time: position.abs(),
                    released: position.is_sign_negative(),
                })
            }
        }
    }

    /// One bounded attempt to read a snapshot. A conflicting UI write is
    /// picked up by a later audio call.
    pub fn read_slices(&self, revision: &mut u64, slices: &mut [Slice; MAX_SLICES + 1]) {
        let Some(general) = self.0.general.as_ref() else {
            return;
        };
        let before = general.slice_revision.load(Ordering::SeqCst);
        if before == *revision || !before.is_multiple_of(2) {
            return;
        }
        let candidate = std::array::from_fn(|i| general.slices[i].load());
        if before == general.slice_revision.load(Ordering::SeqCst) {
            *revision = before;
            *slices = candidate;
        }
    }

    pub fn request_stretch(&self, ratio: u32) {
        if let Some(general) = &self.0.general {
            request_stretch(general, ratio);
        }
    }

    pub fn stretch_ratio(&self) -> u32 {
        self.0.general.as_ref().map_or(UNITY_RATIO, |general| {
            general.stretch_ratio.load(Ordering::Acquire)
        })
    }

    pub fn is_current(&self, pad: usize, generation: u64) -> bool {
        self.0.generations[pad].load(Ordering::Acquire) == generation
    }

    pub fn playhead(&self, pad: usize, fraction: f32) {
        self.0.playheads[pad].store(fraction.to_bits(), Ordering::Relaxed);
    }

    pub fn hit(&self, pad: usize) {
        let counter = (self.0.hit.load(Ordering::Relaxed) >> 4).wrapping_add(1);
        self.0
            .hit
            .store(counter << 4 | pad as u32, Ordering::Relaxed);
    }
}

enum Request {
    Stop,
    Load {
        pad: usize,
        generation: u64,
        link: SampleLink,
        project_dir: Option<PathBuf>,
    },
    Clear {
        pad: usize,
        generation: u64,
    },
}

/// The UI side of a sampler: loads and clears samples, reads what the pads show. Cheap to clone;
/// the loader thread ends when the last clone is dropped.
#[derive(Clone)]
pub struct SamplerHandle {
    shared: Arc<Shared>,
    requests: Sender<Request>,
}

impl std::fmt::Debug for SamplerHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SamplerHandle")
    }
}

/// A handle with its loader thread, and the audio thread's ends.
pub(super) fn channel() -> (SamplerHandle, AudioSide, Meters) {
    channel_for(false)
}

pub fn general_channel() -> (SamplerHandle, AudioSide, Meters) {
    channel_for(true)
}

fn channel_for(general: bool) -> (SamplerHandle, AudioSide, Meters) {
    let (handle, audio, meters, _) = channel_with_worker(general, None);
    (handle, audio, meters)
}

/// A loader owned by a CLAP instance. Drop on the control thread before unloading the library.
pub struct ManagedLoader {
    stopped: Arc<AtomicBool>,
    requests: Sender<Request>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ManagedLoader {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        // Wake a loader waiting for work. The flag also stops a loader with queued requests.
        let _ = self.requests.send(Request::Stop);
        if let Some(thread) = self.thread.take() {
            finish_worker(thread, LOADER_SHUTDOWN_GRACE, retain_loader_image);
        }
    }
}

fn finish_worker(
    thread: std::thread::JoinHandle<()>,
    grace: Duration,
    retain_image: impl FnOnce() -> bool,
) {
    let deadline = Instant::now() + grace;
    while !thread.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    if thread.is_finished() || !retain_image() {
        let _ = thread.join();
    }
    // Otherwise the worker owns everything it still uses. It will observe Stop as soon
    // as the filesystem operation returns, while the operating system retains its code.
}

// A stalled I/O syscall cannot be interrupted portably. Keep this native image loaded only
// when a worker outlives plugin destruction. Never load a new image from the resolved path.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn retain_loader_image() -> bool {
    let mut info = std::mem::MaybeUninit::<libc::Dl_info>::uninit();
    let address = retain_loader_image as *const ();
    // SAFETY: The address belongs to this image and the host still holds it during destruction.
    unsafe {
        if libc::dladdr(address.cast(), info.as_mut_ptr()) == 0 {
            return false;
        }
        let name = info.assume_init().dli_fname;
        // Keep the returned reference for process lifetime so the worker can finish safely.
        !name.is_null()
            && !libc::dlopen(name, libc::RTLD_NOLOAD | libc::RTLD_LAZY | libc::RTLD_LOCAL).is_null()
    }
}

#[cfg(windows)]
fn retain_loader_image() -> bool {
    use windows_sys::Win32::System::LibraryLoader::{
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleHandleExW,
    };
    let mut module = std::ptr::null_mut();
    // SAFETY: FROM_ADDRESS interprets the pointer as an address in this live module.
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            (retain_loader_image as *const ()).cast(),
            &mut module,
        ) != 0
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn retain_loader_image() -> bool {
    false
}

/// Creates the usual sampler channels and a worker owner for a dynamically loaded plugin.
pub fn managed_channel(general: bool) -> (SamplerHandle, AudioSide, Meters, ManagedLoader) {
    let stopped = Arc::new(AtomicBool::new(false));
    let (handle, audio, meters, thread) = channel_with_worker(general, Some(stopped.clone()));
    let loader = ManagedLoader {
        stopped,
        requests: handle.requests.clone(),
        thread: Some(thread),
    };
    (handle, audio, meters, loader)
}

fn channel_with_worker(
    general: bool,
    stopped: Option<Arc<AtomicBool>>,
) -> (
    SamplerHandle,
    AudioSide,
    Meters,
    std::thread::JoinHandle<()>,
) {
    let shared = Arc::new(Shared {
        views: Mutex::new(vec![PadView::empty(); PADS]),
        general: general.then(GeneralShared::default),
        generations: std::array::from_fn(|_| AtomicU64::new(0)),
        playheads: std::array::from_fn(|_| AtomicU32::new((-1.0f32).to_bits())),
        hit: AtomicU32::new(0),
        pending: AtomicUsize::new(0),
    });
    let (to_audio, incoming) = RingBuffer::<Loaded>::new(QUEUE);
    let (garbage, from_audio) = RingBuffer::<Option<Arc<Sample>>>::new(QUEUE);
    let (stretch_worker, stretched) = if general {
        let (sender, receiver) = RingBuffer::new(4);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    let (requests, rx) = mpsc::channel();
    let worker = Arc::clone(&shared);
    let thread = std::thread::Builder::new()
        .name("komponist-sampler-loader".into())
        .spawn(move || {
            run_loader(
                &worker,
                &rx,
                to_audio,
                from_audio,
                stretch_worker,
                stopped.as_deref(),
            );
        })
        .expect("the loader thread starts");
    (
        SamplerHandle {
            shared: Arc::clone(&shared),
            requests,
        },
        AudioSide {
            incoming,
            garbage,
            stretched,
        },
        Meters(shared),
        thread,
    )
}

impl SamplerHandle {
    /// Publishes normalized slice boundaries and playback options to the audio thread.
    pub fn set_general_settings(&self, settings: &Settings) {
        let Some(general) = &self.shared.general else {
            return;
        };
        let mut settings = settings.clone();
        settings.normalize();
        let _writer = general
            .slice_writer
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        general.slice_revision.fetch_add(1, Ordering::SeqCst);
        general.slices[0].store(Slice {
            time: 0.0,
            options: settings.first,
        });
        for (slot, slice) in general.slices[1..].iter().zip(
            settings
                .slices
                .into_iter()
                .chain(std::iter::repeat(Slice::default())),
        ) {
            slot.store(slice);
        }
        general.slice_revision.fetch_add(1, Ordering::SeqCst);
    }

    /// Transient markers calculated by the loader for the current source.
    pub fn transients(&self) -> Vec<f64> {
        let Some(general) = &self.shared.general else {
            return Vec::new();
        };
        let _view = self.shared.views.lock().unwrap_or_else(|e| e.into_inner());
        general
            .transients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Multi-resolution source peaks for the general sampler waveform. The
    /// loader has already read or computed them before the handle exposes them.
    pub fn waveform_peaks(&self) -> Option<Arc<PeakData>> {
        let general = self.shared.general.as_ref()?;
        let _view = self.shared.views.lock().unwrap_or_else(|e| e.into_inner());
        general
            .waveform_peaks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Audition the slice containing this position in the source, independent of MIDI mapping.
    pub fn preview_slice(&self, time: f64) {
        if time.is_finite()
            && time >= 0.0
            && let Some(general) = &self.shared.general
        {
            general
                .slice_preview
                .store(time.abs().to_bits(), Ordering::Relaxed);
        }
    }

    /// Releases a held slice preview. One-shots ignore this release in the engine.
    pub fn release_slice_preview(&self) {
        if let Some(general) = &self.shared.general {
            general
                .slice_preview
                .fetch_or(SLICE_PREVIEW_RELEASED, Ordering::Relaxed);
        }
    }

    /// Starts preparing the initial project tempo ahead of realtime or offline playback.
    pub fn prepare_general_sync(&self, source_bpm: f64, project_bpm: f64, enabled: bool) {
        if let Some(general) = &self.shared.general {
            request_stretch(general, sync_ratio(source_bpm, project_bpm, enabled));
        }
    }

    /// Loads the sample `link` names (resolved against `project_dir`) into `pad`. The pad shows
    /// "Lädt" until the loader is done, then ready, missing or failed.
    pub fn load_link(&self, pad: usize, link: SampleLink, project_dir: Option<PathBuf>) {
        if pad >= PADS {
            return;
        }
        let generation = self.shared.update(pad, |v| {
            self.clear_analysis(pad);
            let generation = self.shared.generations[pad]
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1);
            *v = PadView {
                status: PadStatus::Loading,
                name: link.name.clone(),
                link: Some(link.clone()),
                length_s: 0.0,
                peaks: Arc::from([]),
            };
            generation
        });
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.requests.send(Request::Load {
            pad,
            generation,
            link,
            project_dir,
        });
    }

    /// Empties `pad`.
    pub fn clear(&self, pad: usize) {
        if pad >= PADS {
            return;
        }
        let generation = self.shared.update(pad, |v| {
            self.clear_analysis(pad);
            *v = PadView::empty();
            self.shared.generations[pad]
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1)
        });
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.requests.send(Request::Clear { pad, generation });
    }

    fn clear_analysis(&self, pad: usize) {
        if pad == 0
            && let Some(general) = &self.shared.general
        {
            general
                .slice_preview
                .store(NO_SLICE_PREVIEW, Ordering::Relaxed);
            general
                .transients
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            *general
                .waveform_peaks
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// What `pad` shows.
    pub fn view(&self, pad: usize) -> PadView {
        let views = self.shared.views.lock().unwrap_or_else(|e| e.into_inner());
        views.get(pad).cloned().unwrap_or_else(PadView::empty)
    }

    /// What every pad shows.
    pub fn views(&self) -> Vec<PadView> {
        self.shared
            .views
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The playhead of `pad` as a fraction of its sample while a voice plays it, as the audio
    /// thread last reported it.
    pub fn playhead(&self, pad: usize) -> Option<f32> {
        let bits = self.shared.playheads.get(pad)?.load(Ordering::Relaxed);
        Some(f32::from_bits(bits)).filter(|p| *p >= 0.0)
    }

    /// The last note that hit a pad: a counter that grows with every hit (0 before the first) and
    /// the pad (0-based). The UI follows the input with it ("Pad folgt Eingang").
    pub fn last_hit(&self) -> (u32, usize) {
        let v = self.shared.hit.load(Ordering::Relaxed);
        (v >> 4, (v & 15) as usize)
    }

    /// Whether the requested tempo-synced audio is prepared, pending or unavailable.
    /// Returns `None` for a drum sampler.
    pub fn sync_status(&self) -> Option<SyncStatus> {
        let general = self.shared.general.as_ref()?;
        let ratio = general.stretch_ratio.load(Ordering::Acquire);
        if ratio == UNITY_RATIO {
            return Some(SyncStatus::Ready);
        }
        let views = self.shared.views.lock().unwrap_or_else(|e| e.into_inner());
        let status = match views[0].status {
            PadStatus::Loading => SyncStatus::Preparing,
            PadStatus::Ready => {
                let generation = self.shared.generations[0].load(Ordering::Acquire);
                let completed_generation = general.completed_generation.load(Ordering::Acquire);
                let completed = general.completed.load(Ordering::Acquire);
                if generation != completed_generation
                    || completed as u32 != ratio
                    || general.stretch_revision.load(Ordering::Acquire)
                        != general.completed_revision.load(Ordering::Acquire)
                {
                    SyncStatus::Preparing
                } else if completed & STRETCH_SUCCEEDED != 0 {
                    SyncStatus::Ready
                } else {
                    SyncStatus::Unavailable
                }
            }
            _ => SyncStatus::Ready,
        };
        Some(status)
    }

    /// Whether the loader has nothing left to do.
    pub fn is_idle(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) == 0
            && self.sync_status() != Some(SyncStatus::Preparing)
    }

    /// Waits until the loader is idle, at most `timeout`; `true` if it is. For tests and for
    /// callers that need a pad playable now (export).
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while !self.is_idle() {
            if Instant::now() >= end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        true
    }
}

/// The loader thread: requests in, decoded samples to the audio thread, garbage dropped here.
fn run_loader(
    shared: &Shared,
    requests: &Receiver<Request>,
    mut to_audio: Producer<Loaded>,
    mut from_audio: Consumer<Option<Arc<Sample>>>,
    mut stretched: Option<Producer<Stretched>>,
    stopped: Option<&AtomicBool>,
) {
    // Keep the shared file-pool entry alive only for each pad's current sample.
    let mut held: Vec<Option<Arc<PoolFile>>> = vec![None; PADS];
    let mut source: Option<(u64, Arc<Sample>)> = None;

    let interval = if stretched.is_some() { 8 } else { 100 };
    while !is_stopped(stopped) {
        // Dropping the old samples is this thread's job.
        while from_audio.pop().is_ok() {}
        if let (Some(sink), Some(general), Some((generation, sample))) =
            (&mut stretched, &shared.general, &source)
        {
            prepare_stretch(shared, general, *generation, sample, sink, stopped);
        }
        let request = match requests.recv_timeout(Duration::from_millis(interval)) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        if is_stopped(stopped) {
            return;
        }
        let (pad, generation, load) = match request {
            Request::Stop => return,
            Request::Clear { pad, generation } => (pad, generation, None),
            Request::Load {
                pad,
                generation,
                link,
                project_dir,
            } => (pad, generation, Some((link, project_dir))),
        };
        if shared.generations[pad].load(Ordering::Acquire) == generation {
            let mut sample = None;
            let mut pooled = None;
            if let Some((link, project_dir)) = load {
                let (status, length_s, peaks) = match link.resolve(project_dir.as_deref()) {
                    None => (PadStatus::Missing, 0.0, Arc::from([])),
                    Some(_) if is_stopped(stopped) => return,
                    Some(path) => match open_sample(&path) {
                        Ok((file, decoded)) => {
                            if is_stopped(stopped) {
                                return;
                            }
                            let length = decoded.duration_secs() as f64;
                            let peaks = peaks_of(&decoded);
                            sample = Some(decoded);
                            pooled = Some(file);
                            (PadStatus::Ready, length, peaks)
                        }
                        Err(reason) => (PadStatus::Failed(reason), 0.0, Arc::from([])),
                    },
                };
                let analysis = if pad == 0 && shared.general.is_some() {
                    sample
                        .as_deref()
                        .zip(pooled.as_deref())
                        .map(|(sample, file)| (transient_markers(sample), file.peaks().ok()))
                } else {
                    None
                };
                shared.update(pad, |view| {
                    // Check under the same lock used to publish a new request's view.
                    if shared.generations[pad].load(Ordering::Acquire) == generation {
                        view.status = status;
                        view.length_s = length_s;
                        view.peaks = peaks;
                        if let (Some(general), Some((transients, peaks))) =
                            (&shared.general, analysis)
                        {
                            *general.transients.lock().unwrap_or_else(|e| e.into_inner()) =
                                transients;
                            *general
                                .waveform_peaks
                                .lock()
                                .unwrap_or_else(|e| e.into_inner()) = peaks;
                        }
                    }
                });
            }
            if shared.generations[pad].load(Ordering::Acquire) == generation {
                held[pad] = pooled;
                if pad == 0 && stretched.is_some() {
                    source = sample.as_ref().map(|s| (generation, Arc::clone(s)));
                }
            }
            // Failures clear the audio slot too. The audio side rejects results superseded
            // while decoding or while waiting for space in this queue.
            send(
                &mut to_audio,
                &mut from_audio,
                Loaded {
                    pad,
                    generation,
                    sample,
                },
                stopped,
            );
        }
        shared.pending.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Pushes `item` to the audio thread, waiting for room; drops garbage while waiting. Gives up
/// when the audio side is gone.
fn send(
    to_audio: &mut Producer<Loaded>,
    from_audio: &mut Consumer<Option<Arc<Sample>>>,
    mut item: Loaded,
    stopped: Option<&AtomicBool>,
) {
    while !is_stopped(stopped) {
        match to_audio.push(item) {
            Ok(()) => return,
            Err(rtrb::PushError::Full(back)) => {
                if to_audio.is_abandoned() {
                    return;
                }
                item = back;
                while from_audio.pop().is_ok() {}
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
}

fn is_stopped(stopped: Option<&AtomicBool>) -> bool {
    stopped.is_some_and(|flag| flag.load(Ordering::Acquire))
}

fn transient_markers(sample: &Sample) -> Vec<f64> {
    detect_transients(sample, 0.5, 0.05)
        .into_iter()
        .skip(1)
        .take(MAX_SLICES)
        .map(|frame| frame as f64 / f64::from(sample.sample_rate))
        .collect()
}

pub fn sync_ratio(source_bpm: f64, project_bpm: f64, enabled: bool) -> u32 {
    if enabled && source_bpm.is_finite() && project_bpm.is_finite() {
        ((source_bpm.clamp(40.0, 240.0) / project_bpm.clamp(40.0, 240.0)) as f32).to_bits()
    } else {
        UNITY_RATIO
    }
}

fn request_stretch(general: &GeneralShared, ratio: u32) {
    if general.stretch_ratio.load(Ordering::Relaxed) != ratio {
        general.stretch_ratio.store(ratio, Ordering::Release);
        general.stretch_revision.fetch_add(1, Ordering::Release);
    }
}

fn prepare_stretch(
    shared: &Shared,
    general: &GeneralShared,
    generation: u64,
    source: &Sample,
    sink: &mut Producer<Stretched>,
    stopped: Option<&AtomicBool>,
) {
    let revision = general.stretch_revision.load(Ordering::Acquire);
    let bits = general.stretch_ratio.load(Ordering::Acquire);
    if bits == UNITY_RATIO
        || (general.completed_generation.load(Ordering::Relaxed) == generation
            && general.completed.load(Ordering::Relaxed) as u32 == bits
            && general.completed_revision.load(Ordering::Relaxed) == revision)
        || sink.slots() == 0
    {
        return;
    }
    let stale = || {
        is_stopped(stopped)
            || sink.is_abandoned()
            || shared.generations[0].load(Ordering::Acquire) != generation
            || general.stretch_ratio.load(Ordering::Acquire) != bits
            || general.stretch_revision.load(Ordering::Acquire) != revision
    };
    if stale() {
        return;
    }
    let ratio = f32::from_bits(bits);
    // Bound the extra buffer to one sampler source's length limit.
    let sample = if f64::from(source.duration_secs()) * f64::from(ratio) <= MAX_SECONDS {
        stretch_sample(source, f64::from(ratio), &stale).map(Arc::new)
    } else {
        None
    };
    let result = u64::from(bits)
        | if sample.is_some() {
            STRETCH_SUCCEEDED
        } else {
            0
        };
    if !stale()
        && sink
            .push(Stretched {
                generation,
                ratio: bits,
                sample,
            })
            .is_ok()
    {
        general.completed.store(result, Ordering::Release);
        general
            .completed_generation
            .store(generation, Ordering::Release);
        general
            .completed_revision
            .store(revision, Ordering::Release);
    }
}

/// Offline stretch with the same compensated backend as arrangement audio.
fn stretch_sample(source: &Sample, ratio: f64, stale: &dyn Fn() -> bool) -> Option<Sample> {
    let rate = source.sample_rate as u32;
    let channels = usize::from(source.is_stereo()) + 1;
    let length = output_length(source.len() as u64, ratio).ok()? as usize;
    if length == 0 {
        return None;
    }
    let mut backend = Stretcher::new(rate, channels, ratio).ok()?;
    let sources = [&source.left[..], source.right.as_deref().unwrap_or(&[])];
    let mut scratch: [Vec<f32>; 2] = Default::default();
    fill_stretch_input(
        &mut scratch,
        &sources[..channels],
        0,
        backend.prime_frames(),
    );
    let input = [&scratch[0][..], &scratch[1][..]];
    backend.seek(&input[..channels]).ok()?;
    let mut cursor = backend.prime_frames();
    let mut left = vec![0.0; length];
    let mut right = vec![0.0; if channels == 2 { length } else { 0 }];
    for start in (0..length).step_by(CHUNK_FRAMES) {
        if stale() {
            return None;
        }
        let end = length.min(start + CHUNK_FRAMES);
        let frames = backend.input_frames_required(end - start).ok()?;
        fill_stretch_input(&mut scratch, &sources[..channels], cursor, frames);
        let input = [&scratch[0][..], &scratch[1][..]];
        let right_chunk = if channels == 2 {
            &mut right[start..end]
        } else {
            &mut right[..]
        };
        let mut output = [&mut left[start..end], right_chunk];
        backend
            .process(&input[..channels], &mut output[..channels])
            .ok()?;
        cursor += frames;
    }
    Some(if channels == 2 {
        Sample::stereo(source.sample_rate, left, right)
    } else {
        Sample::mono(source.sample_rate, left)
    })
}

fn fill_stretch_input(scratch: &mut [Vec<f32>; 2], sources: &[&[f32]], from: usize, frames: usize) {
    for (target, source) in scratch.iter_mut().zip(sources) {
        target.clear();
        if from < source.len() {
            target.extend_from_slice(&source[from..source.len().min(from + frames)]);
        }
        target.resize(frames, 0.0);
    }
}

fn peaks_of(sample: &Sample) -> Arc<[f32]> {
    sample
        .peaks(PEAK_BUCKETS)
        .into_iter()
        .map(|(lo, hi)| lo.abs().max(hi.abs()).min(1.0))
        .collect()
}

/// Opens `path` in the global file pool and returns the entry with the sample of it (shared:
/// every call for the same file gives the same `Arc<Sample>` while somebody holds the entry); the
/// error is the reason in German.
fn open_sample(path: &Path) -> Result<(Arc<PoolFile>, Arc<Sample>), String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let fail = |e: AudioError| format!("„{name}“ lässt sich nicht laden: {}", reason(&e));
    let file = FilePool::global().open(path).map_err(fail)?;
    let sample = file.derived(Some(MAX_SECONDS), sample_of).map_err(fail)?;
    Ok((file, sample))
}

/// A decoded file as the sampler's `Sample`; moves the buffers when nobody else holds them.
fn sample_of(decoded: Arc<Decoded>) -> Sample {
    let Decoded {
        sample_rate,
        mut channels,
    } = Arc::try_unwrap(decoded).unwrap_or_else(|shared| (*shared).clone());
    let rate = sample_rate as f32;
    match channels.len() {
        1 => Sample::mono(rate, channels.remove(0)),
        _ => {
            let right = channels.remove(1);
            Sample::stereo(rate, channels.remove(0), right)
        }
    }
}

/// Why a file does not load, in German.
fn reason(e: &AudioError) -> String {
    match e {
        AudioError::Io(e) => e.clone(),
        AudioError::Unsupported(_) => {
            "kein WAV, FLAC, OGG oder MP3 oder ein Format, das komponist nicht liest".into()
        }
        AudioError::Malformed(_) => "die Datei ist beschädigt".into(),
        AudioError::Empty => "die Datei ist leer".into(),
        AudioError::TooLong => "länger als zehn Minuten".into(),
        AudioError::Resample(_) => "die Abtastrate wird nicht unterstützt".into(),
    }
}

/// Reads an audio file (WAV, FLAC, OGG, MP3) as a sampler sample through the file pool; the error
/// is the reason in German.
pub fn decode(path: &Path) -> Result<Sample, String> {
    open_sample(path).map(|(_, sample)| (*sample).clone())
}

#[cfg(test)]
mod managed_worker_tests {
    use super::*;

    #[test]
    fn finished_worker_needs_no_retained_module() {
        let worker = std::thread::spawn(|| {});
        let deadline = Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
        finish_worker(worker, Duration::ZERO, || panic!("worker already stopped"));
    }

    #[test]
    fn blocked_worker_can_finish_after_its_owner_is_destroyed() {
        let (release, blocked) = mpsc::channel();
        let (done, wait) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _ = blocked.recv_timeout(Duration::from_secs(5));
            done.send(()).unwrap();
        });
        let retained = AtomicBool::new(false);
        finish_worker(worker, Duration::ZERO, || {
            retained.store(true, Ordering::Relaxed);
            true
        });
        assert!(retained.load(Ordering::Relaxed));
        assert!(matches!(wait.try_recv(), Err(mpsc::TryRecvError::Empty)));
        release.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
    }
}
