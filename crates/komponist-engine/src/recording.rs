//! Bounded audio capture: fixed chunks cross per-track SPSC queues; a worker alone writes WAVs.
//! Preparation and session polling are non-RT. Engine jobs never allocate, lock, perform IO, or
//! destroy a recorder. The command/garbage protocol returns retired recorders to the UI thread.
use komponist_model::{AudioInput, TempoMap, Tick, TrackId};
use std::cell::UnsafeCell;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const QUEUE_CHUNKS: usize = 32;
const WAVE_BUCKETS: usize = 4096;
const BASE_WAVE_FRAMES: u64 = 128;
static SESSION: AtomicU64 = AtomicU64::new(1);

/// Recording file format. Source files are written directly, without a later conversion pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordingFormat {
    /// Signed 24-bit PCM (default).
    #[default]
    Pcm24,
    /// IEEE 32-bit floating point.
    Float32,
}
/// A source prepared before capture starts. Input changes during capture are explicit failures.
#[derive(Debug, Clone)]
pub struct RecordingTrack {
    /// Track to capture.
    pub track: TrackId,
    /// Human-readable take base, retained for the UI.
    pub name: String,
    /// One or two channels.
    pub channels: u16,
    /// The input at preparation time.
    pub input: AudioInput,
}
/// Non-RT preparation parameters.
#[derive(Debug, Clone)]
pub struct RecordingConfig {
    /// Project Takes folder, or the unsaved project's provisional recording folder.
    pub directory: PathBuf,
    /// Engine rate in Hz.
    pub sample_rate: u32,
    /// PCM24 or float32.
    pub format: RecordingFormat,
    /// Prepared tracks; each appears at most once.
    pub tracks: Vec<RecordingTrack>,
}
/// One finalized recording pass.
#[derive(Debug, Clone)]
pub struct RecordedTake {
    /// Owning track.
    pub track: TrackId,
    /// Final WAV path.
    pub path: PathBuf,
    /// Frames written.
    pub frames: u64,
    /// Common cycle window while a checkpoint is still a growing prefix; final results are padded.
    pub window_frames: Option<u64>,
    /// Native file rate.
    pub sample_rate: u32,
    /// One or two channels.
    pub channels: u16,
    /// First source frame on the song timeline, after latency correction.
    pub start: Tick,
    /// Zero-based loop pass.
    pub lap: u32,
}
/// Finished session; every file has a finalized WAV header before this becomes available.
#[derive(Debug, Clone)]
pub struct RecordingResult {
    /// Captured passes in lap/track order.
    pub takes: Vec<RecordedTake>,
}
/// Explicit session failure. Partial files remain in the recording folder for recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordingError {
    /// Samples did not fit in a bounded queue.
    Overrun,
    /// The input bridge reported missing samples or discontinuous timestamps.
    Discontinuity,
    /// A selected input vanished.
    InputUnavailable,
    /// The engine rate changed.
    SampleRateChanged,
    /// The selected track/input changed while capturing.
    SourceChanged,
    /// The user discarded the session; its provisional files are deleted.
    Cancelled,
    /// Creating, writing or finalizing a file failed.
    Io(String),
}
impl std::fmt::Display for RecordingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Overrun => f.write_str("audio recording queue overflow"),
            Self::Discontinuity => f.write_str("audio input interrupted"),
            Self::InputUnavailable => f.write_str("audio input unavailable"),
            Self::SampleRateChanged => f.write_str("audio sample rate changed while recording"),
            Self::SourceChanged => f.write_str("recording track or input changed"),
            Self::Cancelled => f.write_str("recording discarded"),
            Self::Io(e) => f.write_str(e),
        }
    }
}
impl std::error::Error for RecordingError {}
/// A measured, bounded waveform summary of the live take.
#[derive(Debug, Clone, Copy)]
pub struct RecordingPeak {
    /// Minimum sample value.
    pub min: f32,
    /// Maximum sample value.
    pub max: f32,
    /// Measured RMS (all source channels).
    pub rms: f32,
    /// Number of measured source samples, including a partial bucket. For a combined
    /// envelope divide by the progress channel count to obtain frame duration; for a
    /// native channel envelope this count already equals frame duration.
    pub samples: u64,
}
/// Current pass telemetry. UI polling may allocate; the audio thread never does.
#[derive(Debug, Clone)]
pub struct RecordingProgress {
    /// Owning track.
    pub track: TrackId,
    /// Current pass WAV (header is flushed before progress publication).
    pub path: PathBuf,
    /// Start of the current pass on the song timeline.
    pub start: Tick,
    /// File channels.
    pub channels: u16,
    /// Frames of the current lap consumed by the writer.
    pub frames: u64,
    /// Current zero-based pass.
    pub lap: u32,
    /// Peak since the previous progress call.
    pub peak: f32,
    /// At most 4096 energy-weighted buckets covering the current take.
    pub waveform: Vec<RecordingPeak>,
    /// One measured envelope per native channel (mono or left/right), each bounded to
    /// 4096 buckets. Here each bucket's `samples` is its duration in frames.
    pub channel_waveforms: Vec<Vec<RecordingPeak>>,
}
#[derive(Clone, Copy)]
struct Chunk {
    samples: [f32; crate::MAX_BLOCK * 2],
    frames: usize,
    at: i64,
    start: Tick,
    lap: u32,
    window: Option<(i64, i64, Tick)>,
}
impl Chunk {
    fn new(at: i64, start: Tick, lap: u32) -> Self {
        Self {
            samples: [0.0; crate::MAX_BLOCK * 2],
            frames: 0,
            at,
            start,
            lap,
            window: None,
        }
    }
}
#[derive(Clone, Copy)]
struct Bucket {
    min: f32,
    max: f32,
    energy: f64,
    count: u64,
}
impl Default for Bucket {
    fn default() -> Self {
        Self {
            min: f32::INFINITY,
            max: f32::NEG_INFINITY,
            energy: 0.0,
            count: 0,
        }
    }
}
impl Bucket {
    fn merge(self, other: Self) -> Self {
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
            energy: self.energy + other.energy,
            count: self.count + other.count,
        }
    }
    fn peak(self) -> RecordingPeak {
        RecordingPeak {
            min: self.min,
            max: self.max,
            rms: (self.energy / self.count.max(1) as f64).sqrt() as f32,
            samples: self.count,
        }
    }
}
struct Wave {
    buckets: Vec<Bucket>,
    current: Bucket,
    stride: u64,
    filled: u64,
}
impl Wave {
    fn new() -> Self {
        Self {
            buckets: Vec::with_capacity(WAVE_BUCKETS),
            current: Bucket::default(),
            stride: BASE_WAVE_FRAMES,
            filled: 0,
        }
    }
    fn push(&mut self, frame: &[f32]) {
        for &sample in frame {
            self.current.min = self.current.min.min(sample);
            self.current.max = self.current.max.max(sample);
            self.current.energy += f64::from(sample).powi(2);
            self.current.count += 1;
        }
        self.filled += 1;
        if self.filled == self.stride {
            if self.buckets.len() == WAVE_BUCKETS - 2 {
                for i in 0..(WAVE_BUCKETS - 2) / 2 {
                    self.buckets[i] = self.buckets[2 * i].merge(self.buckets[2 * i + 1]);
                }
                self.buckets.truncate((WAVE_BUCKETS - 2) / 2);
                self.stride *= 2;
                // The current old-stride bucket is the first half of the next larger one.
                return;
            }
            self.buckets.push(self.current);
            self.current = Bucket::default();
            self.filled = 0;
        }
    }
    fn snapshot(&self) -> Vec<RecordingPeak> {
        self.buckets
            .iter()
            .copied()
            .chain((self.current.count > 0).then_some(self.current))
            .map(Bucket::peak)
            .collect()
    }
}
// Worker-only builders share the same frame boundaries and compaction schedule. The
// combined envelope retains all-channel energy; each native channel retains its own energy.
struct LiveWave {
    combined: Wave,
    channels: Vec<Wave>,
}
impl LiveWave {
    fn new(channels: u16) -> Self {
        Self {
            combined: Wave::new(),
            channels: (0..channels).map(|_| Wave::new()).collect(),
        }
    }
    fn push(&mut self, frame: &[f32]) {
        self.combined.push(frame);
        for (channel, &sample) in self.channels.iter_mut().zip(frame) {
            channel.push(&[sample]);
        }
    }
}
struct Progress {
    track: TrackId,
    path: Mutex<PathBuf>,
    start: AtomicI64,
    channels: u16,
    frames: AtomicU64,
    lap: AtomicU32,
    peak: AtomicU32,
    wave: Mutex<LiveWave>,
}
impl Progress {
    fn snapshot(&self) -> RecordingProgress {
        // Publication and polling both acquire wave before path. Keep the waveform guard
        // through the metadata reads so a lap change cannot produce a mixed snapshot.
        let wave = self.wave.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
        RecordingProgress {
            track: self.track,
            path,
            start: Tick(self.start.load(Ordering::Acquire)),
            channels: self.channels,
            frames: self.frames.load(Ordering::Acquire),
            lap: self.lap.load(Ordering::Acquire),
            peak: f32::from_bits(self.peak.swap(0, Ordering::Relaxed)),
            waveform: wave.combined.snapshot(),
            channel_waveforms: wave.channels.iter().map(Wave::snapshot).collect(),
        }
    }
}
struct Shared {
    ended: AtomicBool,
    cancelled: AtomicBool,
    failure: AtomicU8,
    progress: Vec<Arc<Progress>>,
}
struct Lane {
    spec: RecordingTrack,
    // SAFETY: only the one engine job for this track may access its producer, once per segment.
    tx: UnsafeCell<rtrb::Producer<Chunk>>,
}
/// Audio-thread endpoint. Install it with `Command::SetRecorder`, retire through engine garbage.
pub struct Recorder {
    lanes: Vec<Lane>,
    shared: Arc<Shared>,
    pub(crate) rate: u32,
    pub(crate) start: Option<i64>,
}
// SAFETY: the engine schedules one job per track once per segment; producers are disjoint by
// TrackId, and a track job is the only caller of `capture` for that lane. Shared flags are atomic.
unsafe impl Sync for Recorder {}
impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("tracks", &self.lanes.len())
            .finish()
    }
}
/// UI endpoint. Drop/cancel/join and waveform locks are confined to the non-RT owner.
pub struct RecordingSession {
    control: mpsc::Sender<Control>,
    shared: Arc<Shared>,
    rx: mpsc::Receiver<Result<RecordingResult, RecordingError>>,
    worker: Option<JoinHandle<()>>,
    finished: bool,
}
type Checkpoint = Result<Vec<RecordedTake>, RecordingError>;
enum Control {
    Checkpoint(mpsc::SyncSender<Checkpoint>),
    Relocate(PathBuf, mpsc::SyncSender<Checkpoint>),
}
impl RecordingSession {
    /// Non-RT. Flushes all WAV headers and returns a consistent, saveable prefix of each take.
    /// Includes all samples queued before the request.
    /// Capture continues into bounded queues while the worker handles this request.
    pub fn checkpoint(&self) -> Checkpoint {
        let (tx, rx) = mpsc::sync_channel(1);
        self.control
            .send(Control::Checkpoint(tx))
            .map_err(|_| RecordingError::Io("recording writer already stopped".into()))?;
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|_| RecordingError::Io("recording checkpoint timed out".into()))?
    }
    /// Non-RT. Closes file handles, transactionally stages hard links on the same filesystem, and reopens
    /// append. A failed move restores old paths and capture continues; cross-device moves are an
    /// explicit error and can be copied after stopping. No files are silently overwritten.
    pub fn relocate(&self, directory: &std::path::Path) -> Checkpoint {
        let (tx, rx) = mpsc::sync_channel(1);
        self.control
            .send(Control::Relocate(directory.to_path_buf(), tx))
            .map_err(|_| RecordingError::Io("recording writer already stopped".into()))?;
        rx.recv()
            .map_err(|_| RecordingError::Io("recording writer disconnected".into()))?
    }
    /// Receives only a completely drained/finalized result. Never blocks awaiting capture.
    pub fn poll(&mut self) -> Option<Result<RecordingResult, RecordingError>> {
        let result = match self.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) if self.finished => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(RecordingError::Io(
                "recording writer terminated unexpectedly".into(),
            )),
        };
        self.finished = true;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        Some(result)
    }
    /// Requests discard; the engine observes cancellation before sending more chunks.
    pub fn cancel(&self) {
        self.shared.cancelled.store(true, Ordering::Release);
        self.shared.ended.store(true, Ordering::Release);
    }
    /// Current live waveforms. At most one small summary per prepared track.
    pub fn progress(&self) -> Vec<RecordingProgress> {
        self.shared.progress.iter().map(|p| p.snapshot()).collect()
    }
}
impl Drop for RecordingSession {
    fn drop(&mut self) {
        if !self.finished {
            self.cancel();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish();
    }
}
impl Recorder {
    pub(crate) fn validate(&self, tracks: &[Arc<crate::program::TrackProgram>]) {
        if self.lanes.iter().any(|lane| {
            !tracks.iter().any(|t| {
                t.id == lane.spec.track
                    && t.source.is_audio()
                    && t.source.audio.input == lane.spec.input
            })
        }) {
            self.fail(5);
        }
    }
    pub(crate) fn input(&self, track: TrackId) -> Option<AudioInput> {
        self.lanes
            .iter()
            .find(|l| l.spec.track == track)
            .map(|l| l.spec.input)
    }
    pub(crate) fn has_hardware(&self) -> bool {
        self.lanes
            .iter()
            .any(|l| matches!(l.spec.input, AudioInput::Hardware(_)))
    }
    pub(crate) fn active(&self) -> bool {
        !self.shared.ended.load(Ordering::Acquire) && !self.shared.cancelled.load(Ordering::Acquire)
    }
    pub(crate) fn finish(&self) {
        self.shared.ended.store(true, Ordering::Release);
    }
    pub(crate) fn fail(&self, code: u8) {
        let _ = self
            .shared
            .failure
            .compare_exchange(0, code, Ordering::AcqRel, Ordering::Relaxed);
        self.finish();
    }
    /// Called by exactly one job for this track, per engine segment.
    pub(crate) fn capture(&self, track: TrackId, l: &[f32], r: &[f32], window: CaptureWindow) {
        if !self.active() {
            return;
        }
        let Some(lane) = self.lanes.iter().find(|lane| lane.spec.track == track) else {
            return;
        };
        let first = self.start.unwrap_or(window.at);
        let mut chunk = Chunk::new(0, Tick::ZERO, window.lap);
        for (i, (&l, &r)) in l.iter().zip(r).enumerate() {
            let mut at = window.at.saturating_add(i as i64);
            let mut lap = window.lap;
            if let Some((lo, hi)) = window.lp {
                let span = hi - lo;
                if span > 0 {
                    if at < lo && lap > 0 {
                        let wraps = ((lo - at - 1) / span + 1).min(i64::from(lap));
                        at = at.saturating_add(wraps.saturating_mul(span));
                        lap -= wraps as u32;
                    }
                    if at >= hi {
                        let wraps = (at - lo) / span;
                        at = lo + (at - lo).rem_euclid(span);
                        lap = lap.saturating_add(wraps as u32);
                    }
                }
            }
            if lap == 0 && at < first {
                continue;
            }
            if chunk.frames > 0 && (chunk.lap != lap || chunk.at + chunk.frames as i64 != at) {
                if !self.push(lane, chunk) {
                    return;
                }
                chunk = Chunk::new(0, Tick::ZERO, lap);
            }
            if chunk.frames == 0 {
                chunk.at = at;
                chunk.start = window.map.sample_to_tick(at, f64::from(self.rate));
                chunk.lap = lap;
                chunk.window = window
                    .lp
                    .map(|(lo, hi)| (lo, hi, window.map.sample_to_tick(lo, f64::from(self.rate))));
            }
            let sample = |x: f32| if x.is_finite() { x } else { 0.0 };
            let left = sample(l);
            let right = sample(r);
            let channels = usize::from(lane.spec.channels);
            chunk.samples[chunk.frames * channels] = left;
            if channels == 2 {
                chunk.samples[chunk.frames * 2 + 1] = right;
            }
            chunk.frames += 1;
            if chunk.frames == crate::MAX_BLOCK {
                if !self.push(lane, chunk) {
                    return;
                }
                chunk = Chunk::new(0, Tick::ZERO, lap);
            }
        }
        if chunk.frames > 0 {
            self.push(lane, chunk);
        }
    }
    fn push(&self, lane: &Lane, chunk: Chunk) -> bool {
        // SAFETY: `capture`'s scheduling contract keeps this lane's producer exclusive.
        if unsafe { &mut *lane.tx.get() }.push(chunk).is_err() {
            self.fail(1);
            false
        } else {
            true
        }
    }
}
#[derive(Clone, Copy)]
pub(crate) struct CaptureWindow {
    pub(crate) at: i64,
    pub(crate) lap: u32,
    pub(crate) lp: Option<(i64, i64)>,
    pub(crate) map: TempoMap,
}
type Writer = hound::WavWriter<BufWriter<File>>;
struct Output {
    writer: Option<Writer>,
    path: PathBuf,
    frames: u64,
    next: i64,
    start: Tick,
    window: Option<(i64, i64)>,
    closed: bool,
}
fn create(path: PathBuf, config: &RecordingConfig, channels: u16) -> io::Result<Output> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    let spec = hound::WavSpec {
        channels,
        sample_rate: config.sample_rate,
        bits_per_sample: if config.format == RecordingFormat::Pcm24 {
            24
        } else {
            32
        },
        sample_format: if config.format == RecordingFormat::Pcm24 {
            hound::SampleFormat::Int
        } else {
            hound::SampleFormat::Float
        },
    };
    let writer = hound::WavWriter::new(BufWriter::new(file), spec).map_err(io::Error::other)?;
    Ok(Output {
        writer: Some(writer),
        path,
        frames: 0,
        next: 0,
        start: Tick::ZERO,
        window: None,
        closed: false,
    })
}
fn pad_output(
    output: &mut Output,
    frames: u64,
    format: RecordingFormat,
) -> Result<(), RecordingError> {
    if frames == 0 {
        return Ok(());
    }
    let writer = output
        .writer
        .as_mut()
        .ok_or_else(|| RecordingError::Io("recording writer closed".into()))?;
    let channels = u64::from(writer.spec().channels);
    let bytes = u64::from(writer.spec().bits_per_sample / 8);
    if output
        .frames
        .saturating_add(frames)
        .saturating_mul(channels)
        .saturating_mul(bytes)
        > u64::from(u32::MAX) - 128
    {
        return Err(RecordingError::Io(
            "recording exceeds WAV length limit".into(),
        ));
    }
    for _ in 0..frames * channels {
        let result = match format {
            RecordingFormat::Pcm24 => writer.write_sample(0_i32),
            RecordingFormat::Float32 => writer.write_sample(0.0_f32),
        };
        result.map_err(|e| RecordingError::Io(e.to_string()))?;
    }
    output.frames += frames;
    output.next = output.next.saturating_add(frames as i64);
    Ok(())
}
fn close_output(output: &mut Output, format: RecordingFormat) -> Result<(), RecordingError> {
    if let Some((_, hi)) = output.window {
        pad_output(output, hi.saturating_sub(output.next).max(0) as u64, format)?;
    }
    if let Some(writer) = output.writer.take() {
        writer
            .finalize()
            .map_err(|e| RecordingError::Io(e.to_string()))?;
    }
    output.closed = true;
    Ok(())
}
fn take_path(config: &RecordingConfig, id: &str, track: TrackId, lap: u32) -> PathBuf {
    config
        .directory
        .join(format!("take-{}-{id}-{}.wav", track.0, u64::from(lap) + 1))
}
fn error(code: u8) -> RecordingError {
    match code {
        1 => RecordingError::Overrun,
        2 => RecordingError::Discontinuity,
        3 => RecordingError::InputUnavailable,
        4 => RecordingError::SampleRateChanged,
        _ => RecordingError::SourceChanged,
    }
}
fn flush_all(all: &mut [BTreeMap<u32, Output>]) -> Result<(), RecordingError> {
    for output in all.iter_mut().flat_map(|p| p.values_mut()) {
        if let Some(writer) = output.writer.as_mut() {
            writer
                .flush()
                .map_err(|e| RecordingError::Io(e.to_string()))?;
        } else if !output.closed {
            return Err(RecordingError::Io("recording writer closed".into()));
        }
    }
    Ok(())
}
fn snapshot_outputs(all: &[BTreeMap<u32, Output>], config: &RecordingConfig) -> Vec<RecordedTake> {
    let mut takes: Vec<_> = all
        .iter()
        .enumerate()
        .flat_map(|(i, passes)| {
            passes
                .iter()
                .filter(|(_, o)| o.frames > 0)
                .map(move |(&lap, o)| RecordedTake {
                    track: config.tracks[i].track,
                    path: o.path.clone(),
                    frames: o.frames,
                    window_frames: o.window.map(|(lo, hi)| (hi - lo) as u64),
                    sample_rate: config.sample_rate,
                    channels: config.tracks[i].channels,
                    start: o.start,
                    lap,
                })
        })
        .collect();
    takes.sort_by_key(|t| (t.lap, t.track.0));
    takes
}
fn relocate_outputs(
    all: &mut [BTreeMap<u32, Output>],
    directory: &std::path::Path,
) -> Result<(), RecordingError> {
    relocate_outputs_with(all, directory, |path| hound::WavWriter::append(path))
}

// Staging hard links instead of replacing names keeps originals intact until all destination
// writers are open. Creating a link is atomic and never overwrites a concurrent destination.
// The injected opener exercises late failures without filesystem permissions or system changes.
fn relocate_outputs_with(
    all: &mut [BTreeMap<u32, Output>],
    directory: &std::path::Path,
    mut open: impl FnMut(&std::path::Path) -> Result<Writer, hound::Error>,
) -> Result<(), RecordingError> {
    flush_all(all)?;
    std::fs::create_dir_all(directory).map_err(|e| RecordingError::Io(e.to_string()))?;
    let mut outputs: Vec<_> = all.iter_mut().flat_map(|p| p.values_mut()).collect();
    let paths: Vec<_> = outputs
        .iter()
        .map(|output| {
            let name = output
                .path
                .file_name()
                .ok_or_else(|| RecordingError::Io("recording path has no name".into()))?;
            Ok((output.path.clone(), directory.join(name)))
        })
        .collect::<Result<_, RecordingError>>()?;
    if paths.iter().all(|(from, to)| from == to) {
        return Ok(());
    }
    let mut failure = None;
    for output in &mut outputs {
        if let Some(writer) = output.writer.take()
            && let Err(e) = writer.finalize()
        {
            failure = Some(e.to_string());
        }
    }
    let mut linked = Vec::new();
    if failure.is_none() {
        for (from, to) in &paths {
            if from == to {
                continue;
            }
            match std::fs::hard_link(from, to) {
                Ok(()) => linked.push((from.clone(), to.clone())),
                Err(e) => {
                    failure = Some(format!("stage {}: {e}", to.display()));
                    break;
                }
            }
        }
    }
    let mut reopened = Vec::with_capacity(outputs.len());
    if failure.is_none() {
        for (output, (_, to)) in outputs.iter().zip(&paths) {
            if output.closed {
                reopened.push(None);
                continue;
            }
            match open(to) {
                Ok(writer) => reopened.push(Some(writer)),
                Err(e) => {
                    failure = Some(format!("reopen {}: {e}", to.display()));
                    break;
                }
            }
        }
    }
    // Commit the name change only after every new append handle exists. If unlinking an old
    // name fails, restore previously removed names while both paths still share the same data.
    let mut removed = Vec::new();
    if failure.is_none() {
        for (from, to) in &linked {
            match std::fs::remove_file(from) {
                Ok(()) => removed.push((from, to)),
                Err(e) => {
                    failure = Some(format!("remove {}: {e}", from.display()));
                    break;
                }
            }
        }
    }
    if let Some(mut error) = failure {
        for writer in reopened.into_iter().flatten() {
            let _ = writer.finalize();
        }
        for (from, to) in removed {
            if let Err(e) = std::fs::hard_link(to, from) {
                error = format!("restore {} from {}: {e}", from.display(), to.display());
            }
        }
        for (from, to) in &linked {
            if from.exists() {
                let _ = std::fs::remove_file(to);
            }
            // If even restoring the original name failed, keep the surviving destination;
            // fail explicitly rather than unlinking the final copy of captured material.
            else if let Some(output) = outputs.iter_mut().find(|o| o.path == *from) {
                output.path = to.clone();
            }
        }
        for output in &mut outputs {
            if output.closed {
                continue;
            }
            match open(&output.path) {
                Ok(writer) => output.writer = Some(writer),
                Err(e) => error = format!("restore append {}: {e}", output.path.display()),
            }
        }
        return Err(RecordingError::Io(error));
    }
    for ((output, (_, to)), writer) in outputs.into_iter().zip(paths).zip(reopened) {
        output.path = to;
        output.writer = writer;
    }
    Ok(())
}

/// Preallocates bounded queues and opens the initial WAVs before starting the writer thread.
/// Every path is unique and created exclusively; names supplied by the UI never become paths.
pub fn prepare(config: RecordingConfig) -> io::Result<(Box<Recorder>, RecordingSession)> {
    if config.sample_rate == 0
        || config.tracks.is_empty()
        || config.tracks.len() > crate::MAX_TRACKS
        || config
            .tracks
            .iter()
            .any(|t| !(1..=2).contains(&t.channels) || t.input == AudioInput::None)
        || config
            .tracks
            .iter()
            .enumerate()
            .any(|(i, t)| config.tracks[..i].iter().any(|p| p.track == t.track))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recording configuration",
        ));
    }
    std::fs::create_dir_all(&config.directory)?;
    let id = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SESSION.fetch_add(1, Ordering::Relaxed)
    );
    let rate = config.sample_rate;
    let mut lanes = Vec::new();
    let mut readers = Vec::new();
    let mut outputs = Vec::new();
    let mut progress = Vec::new();
    for spec in &config.tracks {
        let initial = match create(
            take_path(&config, &id, spec.track, 0),
            &config,
            spec.channels,
        ) {
            Ok(o) => o,
            Err(e) => {
                for o in outputs {
                    let o: Output = o;
                    let _ = std::fs::remove_file(o.path);
                }
                return Err(e);
            }
        };
        let (tx, rx) = rtrb::RingBuffer::new(QUEUE_CHUNKS);
        lanes.push(Lane {
            spec: spec.clone(),
            tx: UnsafeCell::new(tx),
        });
        readers.push(rx);
        outputs.push(initial);
        progress.push(Arc::new(Progress {
            track: spec.track,
            path: Mutex::new(take_path(&config, &id, spec.track, 0)),
            start: AtomicI64::new(0),
            channels: spec.channels,
            frames: AtomicU64::new(0),
            lap: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            wave: Mutex::new(LiveWave::new(spec.channels)),
        }));
    }
    let shared = Arc::new(Shared {
        ended: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        failure: AtomicU8::new(0),
        progress,
    });
    let (result_tx, rx) = mpsc::sync_channel(1);
    let (control, control_rx) = mpsc::channel();
    let writer_shared = shared.clone();
    let worker = std::thread::Builder::new()
        .name("komponist-record".into())
        .spawn(move || {
            let mut config = config;
            let shared = writer_shared;
            let mut all: Vec<BTreeMap<u32, Output>> = outputs
                .into_iter()
                .map(|o| BTreeMap::from([(0, o)]))
                .collect();
            let mut failure = None;
            loop {
                // Collect requests before draining: a checkpoint must include samples queued
                // before the request, even if they arrived just after the previous drain.
                let controls: Vec<_> = control_rx.try_iter().collect();
                let ended = shared.ended.load(Ordering::Acquire);
                let mut worked = false;
                for (i, reader) in readers.iter_mut().enumerate() {
                    while let Ok(chunk) = reader.pop() {
                        worked = true;
                        if shared.cancelled.load(Ordering::Acquire) || failure.is_some() {
                            continue;
                        }
                        // A later timestamped lap proves this lane has no more input for earlier
                        // laps, even with latency correction. Close them now to bound open FDs.
                        for (_, old) in all[i].range_mut(..chunk.lap) {
                            if !old.closed
                                && let Err(e) = close_output(old, config.format)
                            {
                                failure = Some(e);
                                shared.ended.store(true, Ordering::Release);
                            }
                        }
                        if failure.is_some() {
                            continue;
                        }
                        let spec = &config.tracks[i];
                        if let std::collections::btree_map::Entry::Vacant(entry) =
                            all[i].entry(chunk.lap)
                        {
                            match create(
                                take_path(&config, &id, spec.track, chunk.lap),
                                &config,
                                spec.channels,
                            ) {
                                Ok(output) => {
                                    entry.insert(output);
                                }
                                Err(e) => {
                                    failure = Some(RecordingError::Io(e.to_string()));
                                    shared.ended.store(true, Ordering::Release);
                                    continue;
                                }
                            }
                        }
                        let output = all[i].get_mut(&chunk.lap).expect("created pass exists");
                        if output.frames == 0 {
                            if let Some((lo, hi, start)) = chunk.window {
                                output.start = start;
                                output.next = lo;
                                output.window = Some((lo, hi));
                                if let Err(e) =
                                    pad_output(output, (chunk.at - lo).max(0) as u64, config.format)
                                {
                                    failure = Some(e);
                                    shared.ended.store(true, Ordering::Release);
                                    continue;
                                }
                            } else {
                                output.start = chunk.start;
                                output.next = chunk.at;
                            }
                        }
                        if output.next != chunk.at {
                            failure = Some(RecordingError::Discontinuity);
                            shared.ended.store(true, Ordering::Release);
                            continue;
                        }
                        let channels = usize::from(spec.channels);
                        let mut write_error = None;
                        let writer = output.writer.as_mut().expect("active WAV writer");
                        let bytes_per_frame = u64::from(writer.spec().channels)
                            * u64::from(writer.spec().bits_per_sample / 8);
                        if output
                            .frames
                            .saturating_add(chunk.frames as u64)
                            .saturating_mul(bytes_per_frame)
                            > u64::from(u32::MAX) - 128
                        {
                            failure = Some(RecordingError::Io(
                                "recording exceeds WAV length limit".into(),
                            ));
                            shared.ended.store(true, Ordering::Release);
                            continue;
                        }
                        for &sample in &chunk.samples[..chunk.frames * channels] {
                            let result = match config.format {
                                RecordingFormat::Float32 => writer.write_sample(sample),
                                RecordingFormat::Pcm24 => writer.write_sample(
                                    (f64::from(sample).clamp(-1.0, 1.0) * 8388608.0)
                                        .round()
                                        .clamp(-8388608.0, 8388607.0)
                                        as i32,
                                ),
                            };
                            if let Err(e) = result {
                                write_error = Some(e);
                                break;
                            }
                        }
                        if let Some(e) = write_error {
                            failure = Some(RecordingError::Io(e.to_string()));
                            shared.ended.store(true, Ordering::Release);
                            continue;
                        }
                        if let Err(e) = writer.flush() {
                            failure = Some(RecordingError::Io(e.to_string()));
                            shared.ended.store(true, Ordering::Release);
                            continue;
                        }
                        output.frames += chunk.frames as u64;
                        output.next += chunk.frames as i64;
                        let p = &shared.progress[i];
                        let mut wave = p.wave.lock().unwrap_or_else(|e| e.into_inner());
                        if p.lap.load(Ordering::Relaxed) != chunk.lap
                            || p.frames.load(Ordering::Relaxed) == 0
                        {
                            *wave = LiveWave::new(p.channels);
                            let prefix = output.frames.saturating_sub(chunk.frames as u64);
                            for _ in 0..prefix {
                                wave.push(&[0.0; 2][..channels]);
                            }
                        }
                        for frame in chunk.samples[..chunk.frames * channels].chunks_exact(channels)
                        {
                            wave.push(frame);
                        }
                        *p.path.lock().unwrap_or_else(|e| e.into_inner()) = output.path.clone();
                        p.start.store(output.start.0, Ordering::Release);
                        p.frames.store(output.frames, Ordering::Release);
                        p.lap.store(chunk.lap, Ordering::Release);
                        let peak = chunk.samples[..chunk.frames * channels]
                            .iter()
                            .fold(0.0_f32, |a, &s| a.max(s.abs()));
                        p.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
                    }
                }
                for control in controls {
                    match control {
                        Control::Checkpoint(reply) => {
                            let result =
                                flush_all(&mut all).map(|()| snapshot_outputs(&all, &config));
                            let _ = reply.send(result);
                        }
                        Control::Relocate(directory, reply) => {
                            let result = relocate_outputs(&mut all, &directory).map(|()| {
                                config.directory = directory;
                                for (i, passes) in all.iter().enumerate() {
                                    let progress = &shared.progress[i];
                                    let _wave =
                                        progress.wave.lock().unwrap_or_else(|e| e.into_inner());
                                    let lap = progress.lap.load(Ordering::Acquire);
                                    if let Some(output) = passes.get(&lap) {
                                        *progress.path.lock().unwrap_or_else(|e| e.into_inner()) =
                                            output.path.clone();
                                    }
                                }
                                snapshot_outputs(&all, &config)
                            });
                            if all
                                .iter()
                                .any(|p| p.values().any(|o| !o.closed && o.writer.is_none()))
                            {
                                failure = Some(RecordingError::Io(
                                    "recording file could not be reopened".into(),
                                ));
                                shared.ended.store(true, Ordering::Release);
                            }
                            let _ = reply.send(result);
                        }
                    }
                }
                if ended && !worked {
                    break;
                }
                if !worked {
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            let mut takes = Vec::new();
            for (i, passes) in all.into_iter().enumerate() {
                for (lap, mut output) in passes {
                    if output.frames > 0
                        && !shared.cancelled.load(Ordering::Acquire)
                        && let Some((_, hi)) = output.window
                    {
                        let remaining = hi.saturating_sub(output.next).max(0) as u64;
                        if let Err(e) = pad_output(&mut output, remaining, config.format) {
                            failure = Some(e);
                        }
                    }
                    let window_frames = output.window.map(|(lo, hi)| (hi - lo) as u64);
                    let Output {
                        writer,
                        path,
                        frames,
                        start,
                        ..
                    } = output;
                    if let Some(writer) = writer
                        && let Err(e) = writer.finalize()
                    {
                        failure = Some(RecordingError::Io(e.to_string()));
                    }
                    if frames == 0 || shared.cancelled.load(Ordering::Acquire) {
                        let _ = std::fs::remove_file(&path);
                    } else {
                        takes.push(RecordedTake {
                            track: config.tracks[i].track,
                            path,
                            frames,
                            window_frames,
                            sample_rate: config.sample_rate,
                            channels: config.tracks[i].channels,
                            start,
                            lap,
                        });
                    }
                }
            }
            takes.sort_by_key(|t| (t.lap, t.track.0));
            let code = shared.failure.load(Ordering::Acquire);
            let result = if shared.cancelled.load(Ordering::Acquire) {
                Err(RecordingError::Cancelled)
            } else if let Some(e) = failure {
                Err(e)
            } else if code != 0 {
                Err(error(code))
            } else {
                Ok(RecordingResult { takes })
            };
            let _ = result_tx.send(result);
        })?;
    Ok((
        Box::new(Recorder {
            lanes,
            shared: shared.clone(),
            rate,
            start: None,
        }),
        RecordingSession {
            control,
            shared,
            rx,
            worker: Some(worker),
            finished: false,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{HardwareInput, Meter, Tempo};
    #[test]
    fn progress_polling_uses_publication_lock_order_and_keeps_lap_metadata_together() {
        let progress = Arc::new(Progress {
            track: TrackId(1),
            path: Mutex::new(PathBuf::from("0.wav")),
            start: AtomicI64::new(0),
            channels: 1,
            frames: AtomicU64::new(1),
            lap: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            wave: Mutex::new(LiveWave::new(1)),
        });
        // Holding path forces the reader to stop between its two acquisitions. Observing
        // wave locked proves the lock order without relying on thread scheduling or sleeps.
        let path = progress.path.lock().unwrap();
        let (sent, received) = std::sync::mpsc::channel();
        let reader = progress.clone();
        let polling = std::thread::spawn(move || sent.send(reader.snapshot()).unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let ordered = loop {
            if matches!(
                progress.wave.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ) {
                break true;
            }
            if std::time::Instant::now() >= deadline {
                break false;
            }
            std::thread::yield_now();
        };
        drop(path);
        received.recv_timeout(Duration::from_secs(2)).unwrap();
        polling.join().unwrap();
        assert!(ordered, "polling must acquire wave before waiting for path");

        let publisher = progress.clone();
        let (sent, received) = std::sync::mpsc::channel();
        let publishing = std::thread::spawn(move || {
            for lap in 1..1000_u32 {
                let mut wave = publisher.wave.lock().unwrap();
                *wave = LiveWave::new(1);
                wave.push(&[lap as f32]);
                *publisher.path.lock().unwrap() = PathBuf::from(format!("{lap}.wav"));
                publisher.start.store(i64::from(lap), Ordering::Release);
                publisher
                    .frames
                    .store(u64::from(lap) + 1, Ordering::Release);
                publisher.lap.store(lap, Ordering::Release);
                drop(wave);
                std::thread::yield_now();
            }
            sent.send(()).unwrap();
        });
        let reader = progress.clone();
        let (sent, snapshots) = std::sync::mpsc::channel();
        let polling = std::thread::spawn(move || {
            for _ in 0..1000 {
                let snapshot = reader.snapshot();
                assert_eq!(
                    snapshot.path,
                    PathBuf::from(format!("{}.wav", snapshot.lap))
                );
                assert_eq!(snapshot.start.0, i64::from(snapshot.lap));
                assert_eq!(snapshot.frames, u64::from(snapshot.lap) + 1);
                if snapshot.lap > 0 {
                    assert_eq!(snapshot.waveform[0].max, snapshot.lap as f32);
                    assert_eq!(snapshot.channel_waveforms.len(), 1);
                    assert_eq!(snapshot.channel_waveforms[0][0].max, snapshot.lap as f32);
                }
            }
            sent.send(()).unwrap();
        });
        received.recv_timeout(Duration::from_secs(5)).unwrap();
        snapshots.recv_timeout(Duration::from_secs(5)).unwrap();
        publishing.join().unwrap();
        polling.join().unwrap();
    }
    #[test]
    fn the_live_waveform_is_bounded_and_preserves_weighted_energy() {
        let mut wave = Wave::new();
        let frames = (WAVE_BUCKETS * BASE_WAVE_FRAMES as usize * 3) + 17;
        for _ in 0..frames {
            wave.push(&[0.5, -0.5]);
        }
        let summary = wave.snapshot();
        assert!(summary.len() <= WAVE_BUCKETS);
        assert!(wave.stride > BASE_WAVE_FRAMES);
        assert!(
            summary
                .iter()
                .all(|p| p.min == -0.5 && p.max == 0.5 && (p.rms - 0.5).abs() < 1e-6)
        );
        let count: u64 = wave.buckets.iter().map(|b| b.count).sum::<u64>() + wave.current.count;
        assert_eq!(count, frames as u64 * 2);
        assert_eq!(
            summary.iter().map(|peak| peak.samples).sum::<u64>() / 2,
            frames as u64
        );
        assert_eq!(summary.last().unwrap().samples, wave.current.count);
        assert!(wave.filled > 0 && wave.filled < wave.stride);
    }
    #[test]
    fn native_channel_waveforms_preserve_asymmetry_and_partial_compacted_durations() {
        let mut wave = LiveWave::new(2);
        let frames = WAVE_BUCKETS as u64 * BASE_WAVE_FRAMES * 3 + 17;
        for _ in 0..frames {
            wave.push(&[0.25, 0.75]);
        }
        assert!(wave.combined.stride > BASE_WAVE_FRAMES);
        for (channel, expected) in wave.channels.iter().zip([0.25, 0.75]) {
            let summary = channel.snapshot();
            assert!(summary.len() <= WAVE_BUCKETS);
            assert_eq!(summary.iter().map(|peak| peak.samples).sum::<u64>(), frames);
            assert!(summary.iter().all(|peak| {
                peak.min == expected && peak.max == expected && (peak.rms - expected).abs() < 1e-6
            }));
            assert_eq!(summary.last().unwrap().samples, channel.filled);
            assert!(channel.filled > 0 && channel.filled < channel.stride);
        }
        let combined = wave.combined.snapshot();
        assert_eq!(
            combined.iter().map(|peak| peak.samples).sum::<u64>(),
            frames * 2
        );
        let rms = ((0.25_f32.powi(2) + 0.75_f32.powi(2)) / 2.0).sqrt();
        assert!(combined.iter().all(|peak| (peak.rms - rms).abs() < 1e-6));
        assert_eq!(wave.channels[0].snapshot().len(), combined.len());
        assert_eq!(wave.channels[1].snapshot().len(), combined.len());
    }
    #[test]
    fn live_bucket_sample_counts_preserve_a_short_partial_mono_window() {
        let mut wave = Wave::new();
        let frames = BASE_WAVE_FRAMES + 17;
        for _ in 0..frames {
            wave.push(&[0.25]);
        }
        let summary = wave.snapshot();
        assert_eq!(summary.len(), 2);
        assert_eq!(summary[0].samples, BASE_WAVE_FRAMES);
        assert_eq!(summary[1].samples, 17);
        assert_eq!(summary.iter().map(|peak| peak.samples).sum::<u64>(), frames);
    }
    #[test]
    fn a_full_capture_queue_fails_explicitly_without_allocating_or_freeing() {
        let (tx, rx) = rtrb::RingBuffer::new(2);
        let shared = Arc::new(Shared {
            ended: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            failure: AtomicU8::new(0),
            progress: Vec::new(),
        });
        let recorder = Recorder {
            lanes: vec![Lane {
                spec: RecordingTrack {
                    track: TrackId(1),
                    name: "Take".into(),
                    channels: 2,
                    input: AudioInput::Hardware(HardwareInput {
                        first: 0,
                        stereo: true,
                    }),
                },
                tx: UnsafeCell::new(tx),
            }],
            shared: shared.clone(),
            rate: 48000,
            start: Some(0),
        };
        let window = CaptureWindow {
            at: 0,
            lap: 0,
            lp: None,
            map: TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR),
        };
        assert_no_alloc::assert_no_alloc(|| {
            for _ in 0..3 {
                recorder.capture(TrackId(1), &[0.5; 32], &[0.25; 32], window);
            }
        });
        assert_eq!(shared.failure.load(Ordering::Acquire), 1);
        assert!(shared.ended.load(Ordering::Acquire));
        drop(rx);
    }

    #[test]
    fn a_late_destination_append_failure_restores_original_paths_and_all_writers() {
        let root = std::env::temp_dir().join(format!(
            "komponist-reopen-{}-{}",
            std::process::id(),
            SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&root).unwrap();
        let config = RecordingConfig {
            directory: root.clone(),
            sample_rate: 48000,
            format: RecordingFormat::Float32,
            tracks: Vec::new(),
        };
        let paths = [root.join("first.wav"), root.join("second.wav")];
        let mut all: Vec<_> = paths
            .iter()
            .map(|path| {
                let mut output = create(path.clone(), &config, 1).unwrap();
                output
                    .writer
                    .as_mut()
                    .unwrap()
                    .write_sample(0.25_f32)
                    .unwrap();
                output.frames = 1;
                output.next = 1;
                BTreeMap::from([(0, output)])
            })
            .collect();
        let target = root.join("Takes");
        let mut attempts = 0;
        let result = relocate_outputs_with(&mut all, &target, |path| {
            if path.parent() == Some(target.as_path()) {
                attempts += 1;
                if attempts == 2 {
                    return Err(hound::Error::IoError(io::Error::other(
                        "injected append failure",
                    )));
                }
            }
            hound::WavWriter::append(path)
        });
        assert!(result.is_err());
        assert_eq!(attempts, 2);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        for (pass, original) in all.iter_mut().zip(&paths) {
            let output = pass.get_mut(&0).unwrap();
            assert_eq!(&output.path, original);
            assert!(original.is_file());
            output
                .writer
                .as_mut()
                .unwrap()
                .write_sample(0.5_f32)
                .unwrap();
            output.frames += 1;
            output.next += 1;
            close_output(output, RecordingFormat::Float32).unwrap();
            let samples = hound::WavReader::open(original)
                .unwrap()
                .samples::<f32>()
                .map(Result::unwrap)
                .collect::<Vec<_>>();
            assert_eq!(
                samples,
                [0.25, 0.5],
                "capture continues through the restored writer"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}
