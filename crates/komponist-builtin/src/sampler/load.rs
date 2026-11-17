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
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use komponist_audiofile::{AudioError, Decoded, FilePool, PoolFile};
use rtrb::{Consumer, Producer, RingBuffer};
use sampler_core::Sample;

use super::PADS;
use super::links::SampleLink;

/// Buckets of a pad's peak display.
pub const PEAK_BUCKETS: usize = 512;

/// Longest sample the sampler loads, in seconds (a safety limit for memory).
pub const MAX_SECONDS: f64 = 600.0;

/// Capacity of the queues between the threads.
const QUEUE: usize = 64;

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
pub(super) struct Loaded {
    pub pad: usize,
    pub sample: Option<Arc<Sample>>,
}

/// The audio thread's ends of the queues.
pub(super) struct AudioSide {
    pub incoming: Consumer<Loaded>,
    pub garbage: Producer<Option<Arc<Sample>>>,
}

/// State shared by the handle, the loader and the audio thread.
struct Shared {
    views: Mutex<Vec<PadView>>,
    /// Playhead of each pad as `f32` bits: a fraction of the sample, negative while silent.
    playheads: [AtomicU32; PADS],
    /// `counter << 4 | pad` of the last note that hit a pad; the counter starts at 1.
    hit: AtomicU32,
    /// Requests the loader has not finished.
    pending: AtomicUsize,
}

impl Shared {
    fn update(&self, pad: usize, f: impl FnOnce(&mut PadView)) {
        let mut views = self.views.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut views[pad]);
    }
}

/// The audio thread's side of the shared atomics.
#[derive(Clone)]
pub(super) struct Meters(Arc<Shared>);

impl Meters {
    pub(super) fn playhead(&self, pad: usize, fraction: f32) {
        self.0.playheads[pad].store(fraction.to_bits(), Ordering::Relaxed);
    }

    pub(super) fn hit(&self, pad: usize) {
        let counter = (self.0.hit.load(Ordering::Relaxed) >> 4).wrapping_add(1);
        self.0
            .hit
            .store(counter << 4 | pad as u32, Ordering::Relaxed);
    }
}

enum Request {
    Load {
        pad: usize,
        link: SampleLink,
        project_dir: Option<PathBuf>,
    },
    Clear {
        pad: usize,
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
    let shared = Arc::new(Shared {
        views: Mutex::new(vec![PadView::empty(); PADS]),
        playheads: std::array::from_fn(|_| AtomicU32::new((-1.0f32).to_bits())),
        hit: AtomicU32::new(0),
        pending: AtomicUsize::new(0),
    });
    let (to_audio, incoming) = RingBuffer::<Loaded>::new(QUEUE);
    let (garbage, from_audio) = RingBuffer::<Option<Arc<Sample>>>::new(QUEUE);
    let (requests, rx) = mpsc::channel();
    let worker = Arc::clone(&shared);
    std::thread::Builder::new()
        .name("komponist-sampler-loader".into())
        .spawn(move || run_loader(&worker, &rx, to_audio, from_audio))
        .expect("the loader thread starts");
    (
        SamplerHandle {
            shared: Arc::clone(&shared),
            requests,
        },
        AudioSide { incoming, garbage },
        Meters(shared),
    )
}

impl SamplerHandle {
    /// Loads the sample `link` names (resolved against `project_dir`) into `pad`. The pad shows
    /// "Lädt" until the loader is done, then ready, missing or failed.
    pub fn load_link(&self, pad: usize, link: SampleLink, project_dir: Option<PathBuf>) {
        if pad >= PADS {
            return;
        }
        self.shared.update(pad, |v| {
            *v = PadView {
                status: PadStatus::Loading,
                name: link.name.clone(),
                link: Some(link.clone()),
                length_s: 0.0,
                peaks: Arc::from([]),
            };
        });
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.requests.send(Request::Load {
            pad,
            link,
            project_dir,
        });
    }

    /// Empties `pad`.
    pub fn clear(&self, pad: usize) {
        if pad >= PADS {
            return;
        }
        self.shared.update(pad, |v| *v = PadView::empty());
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.requests.send(Request::Clear { pad });
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

    /// Whether the loader has nothing left to do.
    pub fn is_idle(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) == 0
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
) {
    // The pool entry of every pad that has a sample: it lives while the pad shows the sample.
    let mut held: Vec<Option<Arc<PoolFile>>> = vec![None; PADS];
    loop {
        // Dropping the old samples is this thread's job.
        while from_audio.pop().is_ok() {}
        let request = match requests.recv_timeout(Duration::from_millis(100)) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match request {
            Request::Clear { pad } => {
                held[pad] = None;
                send(&mut to_audio, &mut from_audio, Loaded { pad, sample: None });
            }
            Request::Load {
                pad,
                link,
                project_dir,
            } => match link.resolve(project_dir.as_deref()) {
                None => shared.update(pad, |v| v.status = PadStatus::Missing),
                Some(path) => match open_sample(&path) {
                    Ok((file, sample)) => {
                        let (length_s, peaks) = (sample.duration_secs() as f64, peaks_of(&sample));
                        held[pad] = Some(file);
                        send(
                            &mut to_audio,
                            &mut from_audio,
                            Loaded {
                                pad,
                                sample: Some(sample),
                            },
                        );
                        shared.update(pad, |v| {
                            // A newer request for the pad may have replaced the view meanwhile.
                            if v.link.as_ref() == Some(&link) {
                                v.status = PadStatus::Ready;
                                v.length_s = length_s;
                                v.peaks = peaks;
                            }
                        });
                    }
                    Err(reason) => {
                        shared.update(pad, |v| {
                            if v.link.as_ref() == Some(&link) {
                                v.status = PadStatus::Failed(reason);
                            }
                        });
                    }
                },
            },
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
) {
    loop {
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
