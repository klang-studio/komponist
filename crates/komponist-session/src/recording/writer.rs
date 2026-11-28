//! WAV file writer for bounded engine capture. File operations run on its worker thread.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufWriter};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use komponist_engine::recording::{
    CaptureEndpoints, CaptureSignals, Chunk, CycleWindow, Recorder, RecordingTrack,
    prepare_capture, queue_frames,
};
use komponist_model::{Tick, TrackId};

const WRITE_BATCH_PERIOD: Duration = Duration::from_millis(8);
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
    /// Exact end tick of the loop for a cycle take.
    pub window_end: Option<Tick>,
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
    /// The capture has reached finalization. Poll for the completed take.
    WriterStopped,
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
            Self::WriterStopped => f.write_str("recording writer stopped"),
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
/// Current waveform and at most two completed overlays for a track.
pub struct TrackRecordingProgress {
    /// The pass receiving samples.
    pub current: RecordingProgress,
    /// The linear lead-in and the most recent completed cycle, in that order.
    pub completed: [Option<Arc<RecordingProgress>>; 2],
}

struct ProgressWave {
    current: LiveWave,
    current_is_cycle: bool,
    completed: [Option<Arc<RecordingProgress>>; 2],
}

impl ProgressWave {
    fn new(channels: u16) -> Self {
        Self {
            current: LiveWave::new(channels),
            current_is_cycle: false,
            completed: [None, None],
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
    wave: Mutex<ProgressWave>,
}
impl Progress {
    fn snapshot(&self) -> RecordingProgress {
        let wave = self.wave.lock().unwrap_or_else(|e| e.into_inner());
        self.snapshot_locked(&wave.current, self.peak.swap(0, Ordering::Relaxed))
    }

    fn visible(&self) -> TrackRecordingProgress {
        let wave = self.wave.lock().unwrap_or_else(|e| e.into_inner());
        TrackRecordingProgress {
            current: self.snapshot_locked(&wave.current, self.peak.swap(0, Ordering::Relaxed)),
            completed: wave.completed.clone(),
        }
    }

    fn snapshot_locked(&self, wave: &LiveWave, peak: u32) -> RecordingProgress {
        // Publication and polling both acquire wave before path. Keep the waveform guard
        // through the metadata reads so a lap change cannot produce a mixed snapshot.
        let path = self.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
        RecordingProgress {
            track: self.track,
            path,
            start: Tick(self.start.load(Ordering::Acquire)),
            channels: self.channels,
            frames: self.frames.load(Ordering::Acquire),
            lap: self.lap.load(Ordering::Acquire),
            peak: f32::from_bits(peak),
            waveform: wave.combined.snapshot(),
            channel_waveforms: wave.channels.iter().map(Wave::snapshot).collect(),
        }
    }
}
/// UI endpoint. Drop/cancel/join and waveform locks are confined to the non-RT owner.
pub struct RecordingSession {
    control: Arc<Mutex<Option<mpsc::Sender<Control>>>>,
    signals: CaptureSignals,
    progress: Arc<Vec<Arc<Progress>>>,
    rx: mpsc::Receiver<WriterOutcome>,
    worker: Option<JoinHandle<()>>,
    finished: bool,
}
// Keep finalized files owned by the worker until the control thread consumes the result.
struct WriterOutcome {
    result: Option<Result<RecordingResult, RecordingError>>,
    paths: Vec<PathBuf>,
}

impl WriterOutcome {
    fn accept(mut self) -> Result<RecordingResult, RecordingError> {
        self.result.take().expect("writer result is consumed once")
    }
}

impl Drop for WriterOutcome {
    fn drop(&mut self) {
        if self.result.is_some() {
            for path in &self.paths {
                let _ = std::fs::remove_file(path);
            }
        }
    }
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
        self.request(Control::Checkpoint(tx))?;
        rx.recv_timeout(Duration::from_secs(2))
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Disconnected => RecordingError::WriterStopped,
                mpsc::RecvTimeoutError::Timeout => {
                    RecordingError::Io("recording checkpoint timed out".into())
                }
            })?
    }
    /// Non-RT. Closes file handles, transactionally stages hard links on the same filesystem, and reopens
    /// append. A failed move restores old paths and capture continues; cross-device moves are an
    /// explicit error and can be copied after stopping. No files are silently overwritten.
    pub fn relocate(&self, directory: &std::path::Path) -> Checkpoint {
        let (tx, rx) = mpsc::sync_channel(1);
        self.request(Control::Relocate(directory.to_path_buf(), tx))?;
        rx.recv().map_err(|_| RecordingError::WriterStopped)?
    }
    fn request(&self, request: Control) -> Result<(), RecordingError> {
        let control = self
            .control
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        control
            .as_ref()
            .ok_or(RecordingError::WriterStopped)?
            .send(request)
            .map_err(|_| RecordingError::WriterStopped)
    }

    /// Receives only a completely drained/finalized result. Never blocks awaiting capture.
    pub fn poll(&mut self) -> Option<Result<RecordingResult, RecordingError>> {
        let received = match self.rx.try_recv() {
            Ok(result) => Ok(result),
            Err(mpsc::TryRecvError::Empty) => return None,
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        self.received(received)
    }
    /// Non-RT. [`RecordingSession::poll`] that waits up to `timeout` for the writer to finish,
    /// on its completion signal instead of polling (#221).
    pub fn poll_timeout(
        &mut self,
        timeout: Duration,
    ) -> Option<Result<RecordingResult, RecordingError>> {
        let received = match self.rx.recv_timeout(timeout) {
            Ok(result) => Ok(result),
            Err(mpsc::RecvTimeoutError::Timeout) => return None,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(()),
        };
        self.received(received)
    }
    /// `Err(())`: the writer hung up without a result.
    fn received(
        &mut self,
        received: Result<WriterOutcome, ()>,
    ) -> Option<Result<RecordingResult, RecordingError>> {
        let result = match received {
            Ok(_) if self.signals.cancelled() => Err(RecordingError::Cancelled),
            Ok(outcome) => outcome.accept(),
            Err(()) if self.finished => return None,
            Err(()) => Err(RecordingError::Io(
                "recording writer terminated unexpectedly".into(),
            )),
        };
        self.finished = true;
        // The writer sent its result as its last act; reap it only if it has already exited, so
        // a caller with a deadline never waits for a thread's exit (#221).
        if let Some(worker) = self.worker.take_if(|worker| worker.is_finished()) {
            let _ = worker.join();
        }
        Some(result)
    }
    /// Stops capture and discards WAVs whose result has not been accepted.
    pub fn cancel(&self) {
        self.signals.cancel();
    }
    /// Current live waveforms. At most one small summary per prepared track.
    pub fn progress(&self) -> Vec<RecordingProgress> {
        self.progress.iter().map(|p| p.snapshot()).collect()
    }

    /// Atomically pairs each current pass with its shared, bounded completed overlays.
    pub fn visible_progress(&self) -> Vec<TrackRecordingProgress> {
        self.progress.iter().map(|p| p.visible()).collect()
    }
}
impl Drop for RecordingSession {
    /// A cancelled writer is joined: it deletes its files before the session is gone. A writer
    /// that has delivered its result is left to exit on its own (dropping its handle detaches it).
    fn drop(&mut self) {
        if !self.finished {
            self.cancel();
        }
        if let Some(worker) = self.worker.take()
            && !self.finished
        {
            let _ = worker.join();
        }
    }
}
type Writer = hound::WavWriter<BufWriter<File>>;
struct Output {
    writer: Option<Writer>,
    path: PathBuf,
    frames: u64,
    next: i64,
    start: Tick,
    window: Option<CycleWindow>,
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
    let writer = match hound::WavWriter::new(BufWriter::new(file), spec) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return Err(io::Error::other(error));
        }
    };
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
fn starts_next_pass(previous_lap: u32, previous: &Output, chunk: &Chunk) -> bool {
    let Some(window) = chunk.window else {
        return false;
    };
    if previous_lap.checked_add(1) != Some(chunk.lap) || chunk.at != window.lo {
        return false;
    }
    match previous.window {
        None => previous.next == window.lo,
        Some(prior) => prior == window && previous.next == window.hi,
    }
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
    if let Some(window) = output.window {
        pad_output(
            output,
            window.file_frames.saturating_sub(output.frames),
            format,
        )?;
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
                    window_frames: o.window.map(|window| window.file_frames),
                    window_end: o.window.map(|window| window.ticks.end),
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

fn publish_batch(
    output: &mut Output,
    progress: &Progress,
    lap: u32,
    samples: &[f32],
) -> Result<(), RecordingError> {
    output
        .writer
        .as_mut()
        .expect("active WAV writer")
        .flush()
        .map_err(|error| RecordingError::Io(error.to_string()))?;
    let channels = usize::from(progress.channels);
    let mut wave = progress.wave.lock().unwrap_or_else(|e| e.into_inner());
    let old_frames = progress.frames.load(Ordering::Relaxed);
    if old_frames == 0 || progress.lap.load(Ordering::Relaxed) != lap {
        if old_frames != 0 {
            let old =
                progress.snapshot_locked(&wave.current, progress.peak.load(Ordering::Relaxed));
            let index = usize::from(wave.current_is_cycle);
            wave.completed[index] = Some(Arc::new(old));
        }
        wave.current = LiveWave::new(progress.channels);
        wave.current_is_cycle = output.window.is_some();
        let prefix = output
            .frames
            .saturating_sub((samples.len() / channels) as u64);
        for _ in 0..prefix {
            wave.current.push(&[0.0; 2][..channels]);
        }
    }
    for frame in samples.chunks_exact(channels) {
        wave.current.push(frame);
    }
    *progress.path.lock().unwrap_or_else(|e| e.into_inner()) = output.path.clone();
    progress.start.store(output.start.0, Ordering::Release);
    progress.frames.store(output.frames, Ordering::Release);
    progress.lap.store(lap, Ordering::Release);
    let peak = samples
        .iter()
        .fold(0.0_f32, |a, &sample| a.max(sample.abs()));
    progress.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
    Ok(())
}

/// Preallocates bounded queues and opens the initial WAVs before starting the writer thread.
/// Every path is unique and created exclusively; names supplied by the UI never become paths.
pub fn prepare(config: RecordingConfig) -> io::Result<(Box<Recorder>, RecordingSession)> {
    let CaptureEndpoints {
        recorder,
        mut readers,
        signals,
    } = prepare_capture(&config.tracks, config.sample_rate)?;
    std::fs::create_dir_all(&config.directory)?;
    let id = format!(
        "{}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        SESSION.fetch_add(1, Ordering::Relaxed)
    );
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
                    let Output { writer, path, .. } = o;
                    drop(writer);
                    let _ = std::fs::remove_file(path);
                }
                return Err(e);
            }
        };
        outputs.push(initial);
        progress.push(Arc::new(Progress {
            track: spec.track,
            path: Mutex::new(take_path(&config, &id, spec.track, 0)),
            start: AtomicI64::new(0),
            channels: spec.channels,
            frames: AtomicU64::new(0),
            lap: AtomicU32::new(0),
            peak: AtomicU32::new(0),
            wave: Mutex::new(ProgressWave::new(spec.channels)),
        }));
    }
    let initial_paths: Vec<_> = outputs.iter().map(|output| output.path.clone()).collect();
    let progress = Arc::new(progress);
    let (result_tx, rx) = mpsc::sync_channel(1);
    let (control, control_rx) = mpsc::channel();
    let control = Arc::new(Mutex::new(Some(control)));
    let writer_control = Arc::clone(&control);
    let writer_signals = signals.clone();
    let writer_progress = Arc::clone(&progress);
    let trace = std::env::var_os("KOMPONIST_JACK_CAPTURE_TRACE").as_deref()
        == Some(std::ffi::OsStr::new("1"));
    let worker = std::thread::Builder::new()
        .name("komponist-record".into())
        .spawn(move || {
            let mut config = config;
            let signals = writer_signals;
            let progress = writer_progress;
            let mut all: Vec<BTreeMap<u32, Output>> = outputs
                .into_iter()
                .map(|o| BTreeMap::from([(0, o)]))
                .collect();
            let mut failure = None;
            let mut input = [0.0; komponist_engine::MAX_BLOCK * 2];
            let mut pending_wave = Vec::with_capacity(queue_frames(config.sample_rate) * 2);
            loop {
                // Collect requests before draining: a checkpoint must include samples queued
                // before the request, even if they arrived just after the previous drain.
                let mut controls: Vec<_> = control_rx.try_iter().collect();
                let ended = signals.ended();
                let mut worked = false;
                for (i, reader) in readers.iter_mut().enumerate() {
                    let spec = &config.tracks[i];
                    let mut batch_lap = None;
                    pending_wave.clear();
                    // Drain a finite snapshot. Both producers can keep queuing during disk IO.
                    for _ in 0..reader.queued_chunks() {
                        let Some((chunk, samples)) = reader.read(&mut input) else { break };
                        worked = true;
                        if signals.cancelled() || failure.is_some() {
                            continue;
                        }
                        if batch_lap.is_some_and(|lap| lap != chunk.lap) {
                            let lap = batch_lap.take().expect("batch lap");
                            if let Err(error) = publish_batch(
                                all[i].get_mut(&lap).expect("batch output"),
                                &progress[i],
                                lap,
                                &pending_wave,
                            ) {
                                failure = Some(error);
                                signals.finish();
                                continue;
                            }
                            pending_wave.clear();
                        }
                        if let Some((&previous_lap, previous)) = all[i].last_key_value()
                            && previous.frames > 0
                            && chunk.lap != previous_lap
                            && !starts_next_pass(previous_lap, previous, &chunk)
                        {
                            if trace {
                                eprintln!(
                                    "komponist: recording trace writer-pass-gap track={} previous_lap={previous_lap} next_frame={} lap={} actual_frame={}",
                                    spec.track.0, previous.next, chunk.lap, chunk.at,
                                );
                            }
                            failure = Some(RecordingError::Discontinuity);
                            signals.finish();
                            continue;
                        }
                        // The preceding pass has ended. Close it now to bound open FDs.
                        if let Some((_, old)) = all[i].range_mut(..chunk.lap).next_back()
                            && !old.closed
                            && let Err(error) = close_output(old, config.format)
                        {
                            failure = Some(error);
                            signals.finish();
                        }
                        if failure.is_some() {
                            continue;
                        }
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
                                    signals.finish();
                                    continue;
                                }
                            }
                        }
                        let output = all[i].get_mut(&chunk.lap).expect("created pass exists");
                        if output.frames == 0 {
                            if let Some(window) = chunk.window {
                                output.start = window.ticks.start;
                                output.next = window.lo;
                                output.window = Some(window);
                                if let Err(e) = pad_output(
                                    output,
                                    (chunk.at - window.lo).max(0) as u64,
                                    config.format,
                                )
                                {
                                    failure = Some(e);
                                    signals.finish();
                                    continue;
                                }
                            } else {
                                output.start = chunk.start;
                                output.next = chunk.at;
                            }
                        }
                        if output.next != chunk.at {
                            if trace {
                                eprintln!(
                                    "komponist: recording trace writer-timestamp-gap track={} lap={} expected_frame={} actual_frame={}",
                                    spec.track.0, chunk.lap, output.next, chunk.at,
                                );
                            }
                            failure = Some(RecordingError::Discontinuity);
                            signals.finish();
                            continue;
                        }
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
                            signals.finish();
                            continue;
                        }
                        for &sample in samples.iter() {
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
                            signals.finish();
                            continue;
                        }
                        output.frames += chunk.frames as u64;
                        output.next += chunk.frames as i64;
                        batch_lap = Some(chunk.lap);
                        pending_wave.extend_from_slice(samples);
                    }
                    if let Some(lap) = batch_lap
                        && let Err(error) = publish_batch(
                            all[i].get_mut(&lap).expect("batch output"),
                            &progress[i],
                            lap,
                            &pending_wave,
                        )
                    {
                        failure = Some(error);
                        signals.finish();
                    }
                }
                let drained = ended && !worked;
                if drained {
                    // Close requests under the same lock used by senders, then serve any requests
                    // accepted during the final drain before finishing the WAVs.
                    writer_control.lock().unwrap_or_else(|error| error.into_inner()).take();
                    controls.extend(control_rx.try_iter());
                }
                for control in controls {
                    match control {
                        Control::Checkpoint(reply) => {
                            let result = if signals.cancelled() {
                                Err(RecordingError::Cancelled)
                            } else if let Some(error) = &failure {
                                Err(error.clone())
                            } else {
                                match signals.failure() {
                                    0 => flush_all(&mut all)
                                        .map(|()| snapshot_outputs(&all, &config)),
                                    code => Err(error(code)),
                                }
                            };
                            let _ = reply.send(result);
                        }
                        Control::Relocate(directory, reply) => {
                            let result = relocate_outputs(&mut all, &directory).map(|()| {
                                config.directory = directory;
                                for (i, passes) in all.iter().enumerate() {
                                    let progress = &progress[i];
                                    let mut wave =
                                        progress.wave.lock().unwrap_or_else(|e| e.into_inner());
                                    for completed in wave.completed.iter_mut().flatten() {
                                        if let Some(output) = passes.get(&completed.lap) {
                                            Arc::make_mut(completed).path = output.path.clone();
                                        }
                                    }
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
                                signals.finish();
                            }
                            let _ = reply.send(result);
                        }
                    }
                }
                if drained {
                    break;
                }
                if !ended {
                    std::thread::sleep(WRITE_BATCH_PERIOD);
                }
            }
            let mut takes = Vec::new();
            let mut paths = Vec::new();
            for (i, passes) in all.into_iter().enumerate() {
                for (lap, mut output) in passes {
                    if output.frames > 0
                        && !signals.cancelled()
                        && let Some(window) = output.window
                    {
                        let remaining = window.file_frames.saturating_sub(output.frames);
                        if let Err(e) = pad_output(&mut output, remaining, config.format) {
                            failure = Some(e);
                        }
                    }
                    let window_frames = output.window.map(|window| window.file_frames);
                    let window_end = output.window.map(|window| window.ticks.end);
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
                    if frames == 0 {
                        let _ = std::fs::remove_file(&path);
                    } else {
                        paths.push(path.clone());
                        takes.push(RecordedTake {
                            track: config.tracks[i].track,
                            path,
                            frames,
                            window_frames,
                            window_end,
                            sample_rate: config.sample_rate,
                            channels: config.tracks[i].channels,
                            start,
                            lap,
                        });
                    }
                }
            }
            takes.sort_by_key(|t| (t.lap, t.track.0));
            let code = signals.failure();
            if trace {
                match code {
                    1 => eprintln!("komponist: recording trace engine-queue-overrun code=1"),
                    2 => eprintln!("komponist: recording trace engine-input-discontinuity code=2"),
                    3 => eprintln!("komponist: recording trace engine-input-unavailable code=3"),
                    _ => {}
                }
            }
            let result = if signals.cancelled() {
                Err(RecordingError::Cancelled)
            } else if let Some(e) = failure {
                Err(e)
            } else if code != 0 {
                Err(error(code))
            } else {
                Ok(RecordingResult { takes })
            };
            let outcome = WriterOutcome { result: Some(result), paths };
            if signals.cancelled() {
                drop(outcome);
                let _ = result_tx.send(WriterOutcome {
                    result: Some(Err(RecordingError::Cancelled)),
                    paths: Vec::new(),
                });
            } else {
                let _ = result_tx.send(outcome);
            }
        });
    let worker = match worker {
        Ok(worker) => worker,
        Err(error) => {
            // A failed spawn drops the closure and closes its output handles before returning.
            for path in initial_paths {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
    };
    Ok((
        recorder,
        RecordingSession {
            control,
            signals,
            progress,
            rx,
            worker: Some(worker),
            finished: false,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_engine::recording::CaptureWindow;
    use komponist_model::{AudioInput, TempoMap, TickRange};
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
            wave: Mutex::new(ProgressWave::new(1)),
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
                wave.current = LiveWave::new(1);
                wave.current.push(&[lap as f32]);
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
    const PERIOD: usize = 64;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            loop {
                let directory = std::env::temp_dir().join(format!(
                    "komponist-64-frame-recording-{}-{}",
                    std::process::id(),
                    SESSION.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&directory) {
                    Ok(()) => return Self(directory),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("could not create recording fixture: {error}"),
                }
            }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stalled_writer_fixture(track_count: u64) -> (Scratch, Box<Recorder>, RecordingSession) {
        let directory = Scratch::new();
        let config = RecordingConfig {
            directory: directory.0.clone(),
            sample_rate: 48000,
            format: RecordingFormat::Pcm24,
            tracks: (1..=track_count)
                .map(|id| RecordingTrack {
                    track: TrackId(id),
                    name: format!("Track {id}"),
                    channels: id as u16,
                    input: AudioInput::Hardware(HardwareInput {
                        first: 0,
                        stereo: id == 2,
                    }),
                })
                .collect(),
        };
        let (mut recorder, session) = prepare(config).unwrap();
        recorder.set_test_start(0);
        (directory, recorder, session)
    }

    fn completed_session(
        session: &mut RecordingSession,
    ) -> Result<RecordingResult, RecordingError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(result) = session.poll() {
                return result;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the writer did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn assert_pcm24(path: &std::path::Path, channels: u16, frames: u64, captured: u64) {
        let mut reader = hound::WavReader::open(path).unwrap();
        let spec = reader.spec();
        assert_eq!(spec.bits_per_sample, 24);
        assert_eq!(spec.sample_rate, 48000);
        assert_eq!(spec.channels, channels);
        assert_eq!(u64::from(reader.duration()), frames);
        for (sample, value) in reader.samples::<i32>().enumerate() {
            let expected = if (sample / usize::from(channels)) as u64 >= captured {
                0
            } else if channels == 2 && sample % 2 == 1 {
                -4_194_304
            } else {
                2_097_152
            };
            assert_eq!(
                value.unwrap(),
                expected,
                "sample {sample} from {}",
                path.display()
            );
        }
    }

    #[test]
    fn a_stalled_writer_keeps_64_frame_capture_loop_overlays_and_relocated_pcm24() {
        // 32 frames per tick makes both synthetic sample boundaries exact ticks.
        let map = TempoMap::constant(Tempo::new(93.75), Meter::FOUR_FOUR);
        let (lo, hi) = (1280_i64, 3200_i64);
        let span = hi - lo;
        for track_count in [1, 2] {
            let (directory, mut recorder, mut session) = stalled_writer_fixture(track_count);
            let relocated = directory.0.join("project-takes");
            // A writer which has dequeued samples must stop at progress publication. Push enough
            // callbacks and lap changes to exceed the old 32-entry queue, even on its first drain.
            let stalled = session.progress[0].wave.lock().unwrap();
            for block in 0..100 {
                let frame = (block * PERIOD) as i64;
                let (at, lap) = if frame < hi {
                    (frame, 0)
                } else {
                    (lo + (frame - hi) % span, (1 + (frame - hi) / span) as u32)
                };
                let window = CaptureWindow {
                    at,
                    lap,
                    lp: Some((lo, hi)),
                    loop_range: TickRange::new(
                        map.sample_to_tick(lo, 48_000.0),
                        map.sample_to_tick(hi, 48_000.0),
                    ),
                    map,
                };
                assert_no_alloc::assert_no_alloc(|| {
                    for track in 1..=track_count {
                        recorder.capture_for_test(
                            TrackId(track),
                            &[0.25; PERIOD],
                            &[-0.5; PERIOD],
                            window,
                        );
                    }
                });
            }
            drop(stalled);
            assert!(recorder.active_for_test());
            let checkpoint = session.checkpoint().unwrap();
            assert_eq!(checkpoint.len(), track_count as usize * 4);
            let visible = session.visible_progress();
            let repeated = session.visible_progress();
            for (track, again) in visible.iter().zip(repeated.iter()) {
                assert_eq!((track.current.lap, track.current.frames), (3, 1280));
                assert_eq!(
                    track
                        .current
                        .waveform
                        .iter()
                        .map(|bucket| bucket.samples)
                        .sum::<u64>(),
                    track.current.frames * u64::from(track.current.channels)
                );
                for ((progress, same), (lap, frames)) in track
                    .completed
                    .iter()
                    .zip(&again.completed)
                    .zip([(0, 1280), (2, 1920)])
                {
                    let (progress, same) = (progress.as_ref().unwrap(), same.as_ref().unwrap());
                    assert_eq!((progress.lap, progress.frames), (lap, frames));
                    assert!(Arc::ptr_eq(progress, same));
                }
            }

            session.relocate(&relocated).unwrap();
            for track in session.visible_progress() {
                assert!(track.current.path.starts_with(&relocated));
                for progress in track.completed.iter().flatten() {
                    assert!(progress.path.starts_with(&relocated));
                    assert_pcm24(
                        &progress.path,
                        progress.channels,
                        progress.frames,
                        progress.frames,
                    );
                }
            }

            recorder.finish_for_test();
            let result = completed_session(&mut session).unwrap();
            assert_eq!(result.takes.len(), track_count as usize * 4);
            for take in result.takes {
                let captured = if take.lap == 0 || take.lap == 3 {
                    1280
                } else {
                    span as u64
                };
                let expected_file = if take.lap == 0 { captured } else { span as u64 };
                assert!(take.path.starts_with(&relocated));
                assert_eq!(take.frames, expected_file);
                assert_pcm24(&take.path, take.channels, expected_file, captured);
            }
        }
    }

    #[test]
    fn a_full_capture_queue_fails_explicitly_and_preserves_its_written_prefix() {
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        let (_directory, mut recorder, mut session) = stalled_writer_fixture(1);
        let stalled = session.progress[0].wave.lock().unwrap();
        let mut accepted = 0;
        assert_no_alloc::assert_no_alloc(|| {
            for i in 0..3 * queue_frames(48000) / PERIOD {
                recorder.capture_for_test(
                    TrackId(1),
                    &[0.25; PERIOD],
                    &[-0.5; PERIOD],
                    CaptureWindow {
                        at: (i * PERIOD) as i64,
                        lap: 0,
                        lp: None,
                        loop_range: TickRange::default(),
                        map,
                    },
                );
                if !recorder.active_for_test() {
                    break;
                }
                accepted += PERIOD as u64;
            }
        });
        drop(stalled);
        assert!(!recorder.active_for_test());
        assert!(accepted >= queue_frames(48000) as u64);
        assert_eq!(
            completed_session(&mut session).unwrap_err(),
            RecordingError::Overrun
        );
        let path = session.progress()[0].path.clone();
        assert_pcm24(&path, 1, accepted, accepted);
    }

    #[test]
    fn the_writer_detects_a_gap_at_loop_entry_and_wrap() {
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        let lp = Some((128, 256));
        for (first, next, lap) in [(0, 144, 0), (128, 128, 1)] {
            let (_directory, mut recorder, mut session) = stalled_writer_fixture(1);
            let stalled = session.progress[0].wave.lock().unwrap();
            let loop_range = TickRange::new(
                map.sample_to_tick(128, 48_000.0),
                map.sample_to_tick(256, 48_000.0),
            );
            for window in [
                CaptureWindow {
                    at: first,
                    lap: 0,
                    lp,
                    loop_range,
                    map,
                },
                CaptureWindow {
                    at: next,
                    lap,
                    lp,
                    loop_range,
                    map,
                },
            ] {
                recorder.capture_for_test(TrackId(1), &[0.25; PERIOD], &[0.0; PERIOD], window);
            }
            let (tx, rx) = mpsc::sync_channel(1);
            session.request(Control::Checkpoint(tx)).unwrap();
            drop(stalled);
            assert_eq!(
                rx.recv_timeout(Duration::from_secs(2))
                    .unwrap()
                    .unwrap_err(),
                RecordingError::Discontinuity
            );
            assert_eq!(
                completed_session(&mut session).unwrap_err(),
                RecordingError::Discontinuity
            );
            let path = session.progress()[0].path.clone();
            assert_pcm24(&path, 1, if first == 0 { 64 } else { 128 }, 64);
        }
    }

    #[test]
    fn a_cycle_take_keeps_exact_loop_ticks_and_pads_to_the_required_frame_count() {
        let map = TempoMap::constant(Tempo::new(122.0), Meter::FOUR_FOUR);
        let loop_range = TickRange::new(Tick(3840), Tick(7680));
        let at = map.tick_to_sample(loop_range.start, 48_000.0);
        let end = map.tick_to_sample(loop_range.end, 48_000.0);
        assert_eq!(map.sample_to_tick(at, 48_000.0), Tick(3839));
        assert_eq!(end - at, 94_426);

        let (_directory, mut recorder, mut session) = stalled_writer_fixture(1);
        recorder.set_test_start(at);
        recorder.capture_for_test(
            TrackId(1),
            &[0.25; PERIOD],
            &[0.0; PERIOD],
            CaptureWindow {
                at,
                lap: 0,
                lp: Some((at, end)),
                loop_range,
                map,
            },
        );
        let checkpoint = session.checkpoint().unwrap();
        assert_eq!(checkpoint[0].start, loop_range.start);
        assert_eq!(checkpoint[0].window_end, Some(loop_range.end));
        assert_eq!(checkpoint[0].window_frames, Some(94_427));
        assert_eq!(session.progress()[0].start, loop_range.start);
        recorder.finish_for_test();
        let result = completed_session(&mut session).unwrap();
        assert_eq!(result.takes[0].start, loop_range.start);
        assert_eq!(result.takes[0].window_end, Some(loop_range.end));
        assert_pcm24(&result.takes[0].path, 1, 94_427, PERIOD as u64);
    }

    /// #221: a save waits for the writer on its completion signal, bounded by a timeout.
    #[test]
    fn poll_timeout_waits_for_the_result_and_gives_up_at_the_timeout() {
        let (_directory, mut recorder, mut session) = stalled_writer_fixture(1);
        let started = std::time::Instant::now();
        assert!(session.poll_timeout(Duration::from_millis(30)).is_none());
        assert!(started.elapsed() >= Duration::from_millis(30));
        let finisher = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            recorder.finish_for_test();
            recorder
        });
        let result = session.poll_timeout(Duration::from_secs(5));
        assert!(
            matches!(result, Some(Ok(_))),
            "{:?}",
            result.map(|r| r.err())
        );
        assert!(
            session.poll_timeout(Duration::ZERO).is_none(),
            "received once"
        );
        drop(finisher.join().unwrap());
    }

    #[test]
    fn checkpoint_after_writer_shutdown_reports_the_final_take_is_available() {
        let (_directory, mut recorder, mut session) = stalled_writer_fixture(1);
        recorder.finish_for_test();
        session.worker.take().unwrap().join().unwrap();
        assert_eq!(
            session.checkpoint().unwrap_err(),
            RecordingError::WriterStopped
        );
        assert!(session.poll().unwrap().unwrap().takes.is_empty());
    }

    #[test]
    fn a_queued_two_track_result_is_cleaned_up_only_if_it_is_discarded() {
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        for discard in [false, true] {
            let (directory, mut recorder, mut session) = stalled_writer_fixture(2);
            for track in [TrackId(1), TrackId(2)] {
                recorder.capture_for_test(
                    track,
                    &[0.25; PERIOD],
                    &[0.5; PERIOD],
                    CaptureWindow {
                        at: 0,
                        lap: 0,
                        lp: None,
                        loop_range: TickRange::default(),
                        map,
                    },
                );
            }
            recorder.finish_for_test();
            session.worker.take().unwrap().join().unwrap();
            assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 2);
            if !discard {
                assert_eq!(session.poll().unwrap().unwrap().takes.len(), 2);
            }
            drop(session);
            assert_eq!(
                std::fs::read_dir(&directory.0).unwrap().count(),
                if discard { 0 } else { 2 },
            );
        }
    }

    #[test]
    fn a_late_destination_append_failure_restores_original_paths_and_all_writers() {
        let scratch = Scratch::new();
        let root = scratch.0.clone();
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
    }
}
