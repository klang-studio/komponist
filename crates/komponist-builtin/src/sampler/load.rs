//! Sample loading for the sampler: decoding on a worker thread, handed to the audio thread
//! lock-free (WP-87).
//!
//! ```text
//!  UI thread                    loader thread                       audio thread
//!  SamplerHandle::load_link ──► decode (hound), peaks ──► rtrb ──► SamplerDsp::process swaps the
//!  (mpsc request)               fills the pad's PadView            slot, sends the old Arc back
//!                               drops what comes back ◄── rtrb ◄── (never frees on the audio thread)
//! ```
//!
//! The audio thread never allocates, locks or frees: a loaded sample arrives as an `Arc<Sample>`
//! through a wait-free queue, the slot's old `Arc` goes back through a second one, and the loader
//! thread drops it. What the UI shows of a pad (name, length, peaks, status) lives in a
//! [`PadView`] the loader fills under a mutex only the UI and loader threads take; the audio
//! thread writes the playheads and the last hit into atomics.
//!
//! Formats: WAV through `hound` (8/16/24/32-bit integer and 32-bit float, mono, stereo, or the
//! first two channels of more). No other decoder crate is in the dependency tree, so other
//! formats are refused with a message; any sample rate is played back resampled (the voice
//! interpolates), nothing is converted here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    pub generation: u64,
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
pub(super) struct Meters(Arc<Shared>);

impl Meters {
    pub(super) fn is_current(&self, pad: usize, generation: u64) -> bool {
        self.0.generations[pad].load(Ordering::Acquire) == generation
    }

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
    let shared = Arc::new(Shared {
        views: Mutex::new(vec![PadView::empty(); PADS]),
        generations: std::array::from_fn(|_| AtomicU64::new(0)),
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
        let generation = self.shared.update(pad, |v| {
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
            *v = PadView::empty();
            self.shared.generations[pad]
                .fetch_add(1, Ordering::AcqRel)
                .wrapping_add(1)
        });
        self.shared.pending.fetch_add(1, Ordering::SeqCst);
        let _ = self.requests.send(Request::Clear { pad, generation });
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
    loop {
        // Dropping the old samples is this thread's job.
        while from_audio.pop().is_ok() {}
        let request = match requests.recv_timeout(Duration::from_millis(100)) {
            Ok(r) => r,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let (pad, generation, load) = match request {
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
            if let Some((link, project_dir)) = load {
                let (status, length_s, peaks) = match link.resolve(project_dir.as_deref()) {
                    None => (PadStatus::Missing, 0.0, Arc::from([])),
                    Some(path) => match decode(&path) {
                        Ok(decoded) => {
                            let length = decoded.duration_secs() as f64;
                            let peaks = peaks_of(&decoded);
                            sample = Some(Arc::new(decoded));
                            (PadStatus::Ready, length, peaks)
                        }
                        Err(reason) => (PadStatus::Failed(reason), 0.0, Arc::from([])),
                    },
                };
                shared.update(pad, |view| {
                    // Check under the same lock used to publish a new request's view.
                    if shared.generations[pad].load(Ordering::Acquire) == generation {
                        view.status = status;
                        view.length_s = length_s;
                        view.peaks = peaks;
                    }
                });
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

/// Reads a WAV file as a sampler sample; the error is the reason in German.
pub fn decode(path: &Path) -> Result<Sample, String> {
    let name = path
        .file_name()
        .map_or_else(String::new, |f| f.to_string_lossy().into_owned());
    let fail = |why: &str| format!("„{name}“ lässt sich nicht laden: {why}");
    let mut reader = hound::WavReader::open(path).map_err(|e| match e {
        hound::Error::IoError(e) => fail(&e.to_string()),
        _ => fail("kein WAV oder ein Format, das komponist nicht liest"),
    })?;
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let rate = spec.sample_rate;
    if rate == 0 {
        return Err(fail("Abtastrate fehlt"));
    }
    if f64::from(reader.duration()) / f64::from(rate) > MAX_SECONDS {
        return Err(fail("länger als zehn Minuten"));
    }
    let interleaved: Vec<f32> = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<_, _>>()
            .map_err(|e| fail(&e.to_string()))?,
        (hound::SampleFormat::Int, bits @ (8 | 16 | 24 | 32)) => {
            let scale = 1.0 / (1i64 << (bits - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| fail(&e.to_string()))?
        }
        _ => return Err(fail("Sample-Format wird nicht unterstützt")),
    };
    let frames = interleaved.len() / channels;
    if frames == 0 {
        return Err(fail("die Datei ist leer"));
    }
    let channel =
        |c: usize| -> Vec<f32> { (0..frames).map(|i| interleaved[i * channels + c]).collect() };
    Ok(if channels == 1 {
        Sample::mono(rate as f32, channel(0))
    } else {
        Sample::stereo(rate as f32, channel(0), channel(1))
    })
}
