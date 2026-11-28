//! Bounded realtime capture queues and their non-realtime read endpoint.

use std::cell::UnsafeCell;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use komponist_model::{AudioInput, TempoMap, Tick, TickRange, TrackId};

// Buffer half a second of actual audio frames, within a fixed per-track memory bound.
// Metadata capacity also protects against pathological sub-eight-frame segments.
const MAX_QUEUE_FRAMES: usize = 65_536;

/// Maximum number of frames queued per track at this rate.
pub fn queue_frames(sample_rate: u32) -> usize {
    (sample_rate as usize / 2).clamp(8_192, MAX_QUEUE_FRAMES)
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
/// A loop window in frames and exact song ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CycleWindow {
    /// Start frame of the cycle.
    pub lo: i64,
    /// Exclusive end frame of the cycle.
    pub hi: i64,
    /// Song ticks of the cycle.
    pub ticks: TickRange,
    /// Number of frames in the completed take.
    pub file_frames: u64,
}

/// Timestamp and layout of a captured batch.
#[derive(Debug, Clone, Copy)]
pub struct Chunk {
    /// Number of captured frames.
    pub frames: usize,
    /// First frame on the song timeline.
    pub at: i64,
    /// Song tick of the batch start.
    pub start: Tick,
    /// Zero-based pass.
    pub lap: u32,
    /// Loop window for a cycle pass.
    pub window: Option<CycleWindow>,
}
impl Chunk {
    fn new(at: i64, start: Tick, lap: u32) -> Self {
        Self {
            frames: 0,
            at,
            start,
            lap,
            window: None,
        }
    }
}

#[derive(Default)]
struct Signals {
    ended: AtomicBool,
    cancelled: AtomicBool,
    failure: AtomicU8,
}

/// Shared flags for the producer, drain worker and control thread.
#[derive(Clone, Default)]
pub struct CaptureSignals(Arc<Signals>);

impl CaptureSignals {
    /// Whether the producer has finished, so the drain can consume the final batch.
    pub fn ended(&self) -> bool {
        self.0.ended.load(Ordering::Acquire)
    }

    /// Whether the control thread discarded the capture.
    pub fn cancelled(&self) -> bool {
        self.0.cancelled.load(Ordering::Acquire)
    }

    /// Stops the producer after a writer failure.
    pub fn finish(&self) {
        self.0.ended.store(true, Ordering::Release);
    }

    /// Discards the capture and stops the producer.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
        self.finish();
    }

    /// Producer failure code, or zero if the producer reported no failure.
    pub fn failure(&self) -> u8 {
        self.0.failure.load(Ordering::Acquire)
    }
}

/// A track's bounded read endpoint, consumed by one worker.
pub struct LaneReader {
    chunks: rtrb::Consumer<Chunk>,
    samples: rtrb::Consumer<f32>,
    channels: usize,
}

impl LaneReader {
    /// Takes a snapshot of the number of available batches.
    pub fn queued_chunks(&self) -> usize {
        self.chunks.slots()
    }

    /// Reads one batch into a buffer of at least `MAX_BLOCK * channels` samples.
    pub fn read<'a>(
        &mut self,
        buffer: &'a mut [f32; crate::MAX_BLOCK * 2],
    ) -> Option<(Chunk, &'a [f32])> {
        let chunk = self.chunks.pop().ok()?;
        let samples = &mut buffer[..chunk.frames * self.channels];
        self.samples
            .pop_entire_slice(samples)
            .expect("published chunk has samples");
        Some((chunk, samples))
    }
}

/// The producer and the corresponding track readers in preparation order.
pub struct CaptureEndpoints {
    /// Endpoint sent to the audio thread.
    pub recorder: Box<Recorder>,
    /// Read endpoints consumed by the worker.
    pub readers: Vec<LaneReader>,
    /// Shared lifecycle signals.
    pub signals: CaptureSignals,
}

/// Preallocates the capture queues without opening files or starting a thread.
pub fn prepare_capture(
    tracks: &[RecordingTrack],
    sample_rate: u32,
) -> io::Result<CaptureEndpoints> {
    if sample_rate == 0
        || tracks.is_empty()
        || tracks.len() > crate::MAX_TRACKS
        || tracks
            .iter()
            .any(|t| !(1..=2).contains(&t.channels) || t.input == AudioInput::None)
        || tracks
            .iter()
            .enumerate()
            .any(|(i, t)| tracks[..i].iter().any(|p| p.track == t.track))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid recording configuration",
        ));
    }
    let capacity = queue_frames(sample_rate);
    let mut lanes = Vec::with_capacity(tracks.len());
    let mut readers = Vec::with_capacity(tracks.len());
    for spec in tracks {
        let channels = usize::from(spec.channels);
        let (chunk_tx, chunk_rx) = rtrb::RingBuffer::new(capacity / 8);
        let (sample_tx, sample_rx) = rtrb::RingBuffer::new(capacity * channels);
        lanes.push(Lane {
            spec: spec.clone(),
            writer: UnsafeCell::new(LaneWriter {
                chunks: chunk_tx,
                samples: sample_tx,
                scratch: [0.0; crate::MAX_BLOCK * 2],
            }),
        });
        readers.push(LaneReader {
            chunks: chunk_rx,
            samples: sample_rx,
            channels,
        });
    }
    let signals = CaptureSignals::default();
    Ok(CaptureEndpoints {
        recorder: Box::new(Recorder {
            lanes,
            shared: signals.clone(),
            rate: sample_rate,
            start: None,
        }),
        readers,
        signals,
    })
}

struct LaneWriter {
    chunks: rtrb::Producer<Chunk>,
    samples: rtrb::Producer<f32>,
    scratch: [f32; crate::MAX_BLOCK * 2],
}
struct Lane {
    spec: RecordingTrack,
    // SAFETY: only the one engine job for this track may access its producer, once per segment.
    writer: UnsafeCell<LaneWriter>,
}
/// Audio-thread endpoint. Install it with `Command::SetRecorder`, retire through engine garbage.
pub struct Recorder {
    lanes: Vec<Lane>,
    shared: CaptureSignals,
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
    /// A recording at or beyond the loop end follows the transport linearly.
    pub(crate) fn cycle_window(&self, loop_range: Option<(i64, i64)>) -> Option<(i64, i64)> {
        let first = self.start?;
        loop_range.filter(|&(_, end)| first < end)
    }

    pub(crate) fn active(&self) -> bool {
        !self.shared.0.ended.load(Ordering::Acquire)
            && !self.shared.0.cancelled.load(Ordering::Acquire)
    }
    pub(crate) fn finish(&self) {
        self.shared.0.ended.store(true, Ordering::Release);
    }
    pub(crate) fn fail(&self, code: u8) {
        let _ =
            self.shared
                .0
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
        // SAFETY: capture's scheduling contract keeps this lane's producer and scratch exclusive.
        let writer = unsafe { &mut *lane.writer.get() };
        let channels = usize::from(lane.spec.channels);
        let first = self.start.unwrap_or(window.at);
        let cycle = window.lp.map(|(lo, hi)| {
            let ticks = window.loop_range;
            // The transport rounds each boundary to the nearest frame. Pad the WAV by
            // at most one extra frame when the floored source duration would lose a tick.
            let minimum = window
                .map
                .tick_to_samples(ticks.len(), f64::from(self.rate))
                .ceil() as u64;
            CycleWindow {
                lo,
                hi,
                ticks,
                file_frames: ((hi - lo) as u64).max(minimum),
            }
        });
        let has_lead_in = cycle.is_some_and(|cycle| first < cycle.lo);
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
            // A take that begins before the loop keeps its lead-in as a linear clip.
            // Assign a separate lap to the first cycle after correcting for latency.
            let cycle = cycle.filter(|cycle| at >= cycle.lo);
            if has_lead_in && cycle.is_some() {
                lap = lap.saturating_add(1);
            }
            if chunk.frames > 0 && (chunk.lap != lap || chunk.at + chunk.frames as i64 != at) {
                if !self.push(writer, chunk, channels) {
                    return;
                }
                chunk = Chunk::new(0, Tick::ZERO, lap);
            }
            if chunk.frames == 0 {
                chunk.at = at;
                chunk.start = window.map.sample_to_tick(at, f64::from(self.rate));
                chunk.lap = lap;
                chunk.window = cycle;
            }
            let sample = |x: f32| if x.is_finite() { x } else { 0.0 };
            let left = sample(l);
            let right = sample(r);
            writer.scratch[chunk.frames * channels] = left;
            if channels == 2 {
                writer.scratch[chunk.frames * 2 + 1] = right;
            }
            chunk.frames += 1;
            if chunk.frames == crate::MAX_BLOCK {
                if !self.push(writer, chunk, channels) {
                    return;
                }
                chunk = Chunk::new(0, Tick::ZERO, lap);
            }
        }
        if chunk.frames > 0 {
            self.push(writer, chunk, channels);
        }
    }
    fn push(&self, writer: &mut LaneWriter, chunk: Chunk, channels: usize) -> bool {
        let samples = &writer.scratch[..chunk.frames * channels];
        if writer.chunks.is_full() || writer.samples.slots() < samples.len() {
            self.fail(1);
            return false;
        }
        // The consumer only reads samples after receiving a descriptor. Once both checks
        // pass it can only free more space before we publish the two entries.
        writer
            .samples
            .push_entire_slice(samples)
            .expect("sample slots checked");
        writer.chunks.push(chunk).expect("chunk slot checked");
        true
    }
}
#[derive(Clone, Copy)]
#[doc(hidden)]
pub struct CaptureWindow {
    pub at: i64,
    pub lap: u32,
    pub lp: Option<(i64, i64)>,
    pub loop_range: TickRange,
    pub map: TempoMap,
}

#[cfg(feature = "recording-test-support")]
impl Recorder {
    #[doc(hidden)]
    pub fn set_test_start(&mut self, start: i64) {
        self.start = Some(start);
    }

    #[doc(hidden)]
    pub fn capture_for_test(
        &mut self,
        track: TrackId,
        l: &[f32],
        r: &[f32],
        window: CaptureWindow,
    ) {
        self.capture(track, l, r, window);
    }

    #[doc(hidden)]
    pub fn finish_for_test(&mut self) {
        self.finish();
    }

    #[doc(hidden)]
    pub fn active_for_test(&self) -> bool {
        self.active()
    }
}
