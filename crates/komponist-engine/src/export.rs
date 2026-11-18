//! WAV export (ARCHITECTURE §6.7, screen S08): offline render of a frame range plus a tail, peak,
//! optional normalisation, TPDF dither and PCM quantisation, written with `hound`.
//!
//! [`export`] runs one [`ExportJob`] on a worker thread, faster than real time:
//!
//! 1. Queues `Stop` twice (the second is the hard cut: no live tail leaks into the file; the
//!    `SetProgram` behind it completes the cut in the same command drain, so no fade ramp reaches
//!    the render),
//!    `SetProgram` with an export variant of the caller's program, `Locate` and
//!    `Play` on the engine's command queue. The export program has the loop and the metronome
//!    off, no note starting at or after the range end, notes still sounding there cut at it (so
//!    the tail only lets reverbs, delays and releases ring out), and fresh track meters (the UI's
//!    meters do not see the export).
//! 2. Renders with [`Engine::render`] (`offline = true`, no deadline) into an in-memory f32
//!    buffer: first a pre-roll of less than one tick when the range start lies between two ticks
//!    (`Locate` takes ticks), plus the project latency ([`Engine::project_latency`]: lookahead
//!    of presse or Limiter, plugin latency, delay compensation), all of it discarded, so the sound
//!    of song frame `f` is file frame `f − range.start`; then range + tail. A project without
//!    latency renders exactly as without that skip. After every render
//!    call it reports [`Progress`]; before every call it checks the cancel flag.
//! 3. Peak → optional normalisation ([`normalize_gain`]) → TPDF dither ([`tpdf`], 16 and 24 bit
//!    only; float is never dithered) → quantisation ([`quantize`]) → WAV. The file is written as
//!    an exclusively created file in a private staging directory next to the target. It is
//!    renamed onto the target when complete. Cancellation or errors remove the staging directory
//!    and preserve any existing export. Staging is created before step 1.
//! 4. Queues `Stop` twice (hard cut: the export's tail never rings on), `SetProgram` with the caller's program and `Locate` to the playhead from
//!    before the export, whatever the outcome. They take effect at the engine's next call, so the
//!    transport is stopped where it was (ARCHITECTURE §6.7: the transport stops during export).
//!
//! The caller's duties **before** [`export`]:
//! - Take the engine out of the audio callback (`komponist-audio`'s detach) and move the
//!   `Box<Engine>`, its [`EngineHandle`] and the program to the worker thread.
//! - Set the export rate with [`Engine::set_sample_rate`] when it differs from the device rate
//!   ([`ExportJob::sample_rate`] must equal the engine's rate; this re-activates every processor).
//! - Wait until every external device reports `Ok` again (after the re-activation, and after a
//!   project load): a device still loading renders silence (PLAN WP-45). `komponist-plugins`
//!   reports a bridge `Loading` from the moment `set_sample_rate` asked it to re-activate until it
//!   runs at the new rate and its warm-up passed (`komponist_plugins::WARM_UP`), so this one
//!   status wait covers both; no state round trip as a barrier is needed. Show which devices are
//!   missing or crashed instead of exporting without them. Bridges use their offline deadline
//!   (2 s per block) because render calls carry no [`crate::ProcessContext::deadline`].
//! - A range that starts inside a long note renders it from the first frame: `Play` chases the
//!   notes sounding at the range start (see [`crate::engine`]).
//! - Compile the project again with [`recompile`] once the devices run at the export rate (the
//!   latency skip reads the processors, so the program's delay compensation and routing latency
//!   must come from them too, in frames of the export rate), and pass that program.
//!
//! **After** [`export`] (on success, cancel and error alike):
//! - Restore the device rate with [`Engine::set_sample_rate`] if it was changed.
//! - Queue the program the engine played before ([`export`] queued the one it was given).
//! - Put the engine back into the audio callback and the handle back on the UI thread.
//! - Poll the handle as usual: both replaced programs come back as garbage; a
//!   `ProcessorFailed` from the export means a device dropped out of the render.
//!
//! Range mapping (S08 "Bereich"): Song = ticks `0 .. Program::song_end`, Loop = the loop range,
//! Selection = the selection's time range, each converted with [`ExportJob::frames_of`]; the tail
//! ("Nachlauf") with [`ExportJob::tail_frames`]; "Normalisieren" = [`NORMALIZE_TARGET_DB`].

use std::fmt;
use std::fs;
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use klang_dsp::buffer::scale;
use klang_dsp::rng::Rng;
use klang_dsp::units::{MIN_GAIN, db_to_gain, gain_to_db};
use komponist_model::{DeviceId, Project, TempoMap, Tick, TickRange};

use crate::MAX_BLOCK;
use crate::api::{Command, EngineHandle, TrackMeter};
use crate::audio::SourceStatus;
use crate::compile::{self, CompileEnv};
use crate::engine::Engine;
use crate::program::{MidiPort, Program, SeqNote, TrackProgram};

mod track;
pub use track::TrackExport;

/// Peak level of "Normalisieren" (S08) in dBFS.
pub const NORMALIZE_TARGET_DB: f32 = -1.0;

/// Default "Nachlauf" (S08) in seconds.
pub const DEFAULT_TAIL_SECONDS: f32 = 2.0;

/// Maximum tail for a flattened track, including sustained or self-oscillating effects.
pub const TRACK_TAIL_SECONDS: f32 = 30.0;

/// Leave this much padding after the last signal so whole-tick clip lengths preserve the decay.
const TRACK_END_PADDING_SECONDS: f32 = 0.1;

/// Seed of the dither noise, so the same render always gives the same file.
const DITHER_SEED: u32 = 0x5eed_d17e;

/// Interleaved samples written between two checks of the cancel flag.
const WRITE_CHUNK: usize = 1 << 16;

/// Commands one export queues: five before the render, four after it.
const COMMANDS: usize = 9;

/// Sample format of the file (S08 "Bittiefe"); maps 1:1 to the app's `BitDepth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitDepth {
    /// 16-bit PCM.
    Int16,
    /// 24-bit PCM.
    Int24,
    /// 32-bit IEEE float: the rendered samples as they are (never dithered, not clipped).
    Float32,
}

impl BitDepth {
    /// Bits per sample.
    pub fn bits(self) -> u16 {
        match self {
            BitDepth::Int16 => 16,
            BitDepth::Int24 => 24,
            BitDepth::Float32 => 32,
        }
    }
}

/// What to export (S08). The app maps its `ExportSettings` field by field.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportJob {
    /// The WAV file to write. An existing file is replaced only when the export completes.
    pub path: PathBuf,
    /// Sample rate of the file in Hz; must equal the engine's rate.
    pub sample_rate: u32,
    /// Song frames of the range at `sample_rate` (frame 0 = tick 0), from
    /// [`ExportJob::frames_of`]. `end < start` is an error; an empty range exports only the tail.
    pub range: Range<u64>,
    /// Frames rendered after the range ("Nachlauf"), from [`ExportJob::tail_frames`]. No note
    /// starts in them.
    pub tail: u64,
    /// Sample format.
    pub bit_depth: BitDepth,
    /// Normalise the sample peak to this level in dBFS ([`NORMALIZE_TARGET_DB`]); `None` keeps
    /// the level.
    pub normalize: Option<f32>,
    /// TPDF dither of ±1 LSB before quantising to 16 or 24 bit; ignored for float.
    pub dither: bool,
}

impl ExportJob {
    /// Frames in the file: range + tail.
    pub fn frames(&self) -> u64 {
        self.range.end.saturating_sub(self.range.start) + self.tail
    }

    /// The song frames of `ticks` at `sample_rate`: each end at the sample where an event at that
    /// tick is rendered ([`TempoMap::tick_to_sample`]). Negative positions count as 0; an empty
    /// tick range gives an empty frame range.
    pub fn frames_of(map: &TempoMap, ticks: TickRange, sample_rate: u32) -> Range<u64> {
        let frame = |tick| map.tick_to_sample(tick, f64::from(sample_rate)).max(0) as u64;
        let start = frame(ticks.start);
        start..frame(ticks.end).max(start)
    }

    /// `seconds` (negative counts as 0) as frames at `sample_rate`, rounded.
    pub fn tail_frames(seconds: f32, sample_rate: u32) -> u64 {
        (f64::from(seconds.max(0.0)) * f64::from(sample_rate)).round() as u64
    }
}

/// Progress of the render, reported after every render call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    /// Rendered share of range + tail, `0.0 ..= 1.0`. Normalising and writing follow at 1.0
    /// (they take a small fraction of the render time; the cancel flag still counts).
    pub fraction: f32,
    /// Highest sample peak rendered so far in dBFS, before normalisation (−120 for silence).
    pub peak_db: f32,
}

/// The result of a completed export.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExportReport {
    /// Frames written: range + tail.
    pub frames: u64,
    /// Sample peak of the render before normalisation in dBFS (−120 for silence).
    pub peak_db: f32,
    /// Gain the normalisation applied in dB (0 without normalisation or for silence).
    pub gain_db: f32,
}

/// Why an export ended without a file.
#[derive(Debug)]
pub enum ExportError {
    /// The cancel flag was set. No file was written.
    Cancelled,
    /// The job's range ends before it starts. Nothing was changed.
    InvalidRange,
    /// The requested MIDI track has no clips or could not be compiled on its own.
    InvalidTrack,
    /// A device required for a track render was missing or failed while processing.
    DeviceFailed(DeviceId),
    /// The engine runs at another rate than the job asks for. Nothing was changed.
    SampleRate {
        /// The engine's rate in Hz.
        engine: f32,
        /// The job's rate in Hz.
        job: u32,
    },
    /// The engine's command queue has no room for the export's commands. Nothing was changed.
    QueueFull,
    /// An audio clip in the range plays a file that is missing or cannot be read (the path of the
    /// link). Nothing was changed: an export without it would be silent where the clip is.
    MissingAudio(PathBuf),
    /// A worker could not render a warped clip. No silent/partial export is committed.
    Stretch {
        clip: komponist_model::ClipId,
        reason: crate::audio::StretchError,
    },
    /// The target cannot be created (checked before anything changes), the render does not fit
    /// into memory, or writing failed. No partial file is left.
    Io(io::Error),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Cancelled => f.write_str("export cancelled"),
            ExportError::InvalidRange => f.write_str("export range ends before it starts"),
            ExportError::InvalidTrack => f.write_str("MIDI track could not be rendered"),
            ExportError::DeviceFailed(device) => {
                write!(f, "render processor {} unavailable", device.0)
            }
            ExportError::SampleRate { engine, job } => write!(
                f,
                "the engine runs at {engine} Hz, the export asks for {job} Hz"
            ),
            ExportError::QueueFull => f.write_str("engine command queue full"),
            ExportError::MissingAudio(path) => {
                write!(f, "audio file missing or unreadable: {}", path.display())
            }
            ExportError::Stretch { clip, reason } => write!(f, "audio clip {}: {reason}", clip.0),
            ExportError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for ExportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Io(error) => Some(error),
            ExportError::Stretch { reason, .. } => Some(reason),
            _ => None,
        }
    }
}

impl From<io::Error> for ExportError {
    fn from(error: io::Error) -> ExportError {
        ExportError::Io(error)
    }
}

/// Non-RT, on a worker thread. Renders `job` with `engine` and writes the WAV file (see the
/// module docs for the steps and for the caller's duties before and after).
///
/// `handle` is the engine's own handle (the export sends its commands through it and leaves the
/// engine's events in it). `program` is the program the engine plays now; the export plays a
/// variant of it and queues it again at the end. `cancel` is checked before every render call
/// and while writing; once set, the export ends with [`ExportError::Cancelled`] and no file.
/// `progress` is called after every render call.
pub fn export(
    engine: &mut Engine,
    handle: &mut EngineHandle,
    program: &Program,
    job: &ExportJob,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<ExportReport, ExportError> {
    export_inner(engine, handle, program, job, None, cancel, progress)
}

/// Renders one MIDI track using its live processor slots, before the channel strip. Processes
/// its sidechain dependencies as control signals, and fails if a required processor drops out.
/// Queues the live program again after rendering on success, cancellation or error.
pub fn export_track(
    engine: &mut Engine,
    handle: &mut EngineHandle,
    program: &Program,
    track: (&TrackExport, &Project),
    job: &ExportJob,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<ExportReport, ExportError> {
    let (track, project) = track;
    let compiled = track.compile(engine, project, program)?;
    export_inner(
        engine,
        handle,
        &compiled,
        job,
        Some((track, program)),
        cancel,
        progress,
    )
}

fn export_inner(
    engine: &mut Engine,
    handle: &mut EngineHandle,
    program: &Program,
    job: &ExportJob,
    track_render: Option<(&TrackExport, &Program)>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<ExportReport, ExportError> {
    if job.range.end < job.range.start {
        return Err(ExportError::InvalidRange);
    }
    if engine.sample_rate() != job.sample_rate as f32 {
        return Err(ExportError::SampleRate {
            engine: engine.sample_rate(),
            job: job.sample_rate,
        });
    }
    if job.range.is_empty() && job.tail == 0 {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let output = Output::create(job)?;
        progress(Progress {
            fraction: 1.0,
            peak_db: gain_to_db(0.0),
        });
        return output.write(job, Vec::new(), 0.0, cancel);
    }
    if handle.commands.slots() < COMMANDS {
        return Err(ExportError::QueueFull);
    }
    let (track, restore) =
        track_render.map_or((None, program), |(track, live)| (Some(track), live));
    let map = &program.tempo_map;
    let rate = f64::from(job.sample_rate);
    let start = track.map_or(job.range.start, |track| {
        (map.tick_to_sample(track.preroll(), rate).max(0) as u64).min(job.range.start)
    });
    let (from, preroll) = locate_point(map, rate, start);
    // The sound of song frame f leaves the master at f + latency: skip that many more frames, so
    // the file lines up with the song timeline and the delayed tail is rendered in full.
    let preroll = preroll + (job.range.start - start) + engine.project_latency(program);
    let end = end_tick(map, rate, job.range.end);
    let playhead = handle.telemetry().playhead();
    let exporting = export_program(program, end, job.range.end as i64, track);
    let render_frames = start as i64..(job.range.end + job.tail) as i64;
    check_audio_files(
        if track.is_some() { &exporting } else { program },
        &render_frames,
    )?;
    let output = Output::create(job)?;
    let devices = track
        .into_iter()
        .flat_map(TrackExport::devices)
        .filter_map(|id| program.slots.slot(id).map(|slot| (id, slot)))
        .map(|(id, slot)| (id, slot, engine.processor_offline_failures(slot)))
        .collect::<Vec<_>>();
    let failures: Vec<_> = exporting
        .tracks
        .iter()
        .filter_map(|t| t.audio.as_ref())
        .flat_map(|plan| {
            plan.segments()
                .iter()
                .enumerate()
                .filter(|(_, s)| s.start < render_frames.end && s.end > render_frames.start)
                .map(|(i, s)| (Arc::clone(plan), i, s.clip))
                .collect::<Vec<_>>()
        })
        .collect();
    let check = |engine: &Engine| {
        for &(device, slot, initial) in &devices {
            if initial.is_none() || engine.processor_offline_failures(slot) != initial {
                return Err(ExportError::DeviceFailed(device));
            }
        }
        for (plan, i, clip) in &failures {
            if plan.failure_count(*i) != 0 {
                return Err(ExportError::Stretch {
                    clip: *clip,
                    reason: plan
                        .failure_reason(*i)
                        .unwrap_or(crate::audio::StretchError::Backend),
                });
            }
        }
        Ok(())
    };
    check(engine)?;
    send_all(
        handle,
        [
            // Two Stops: the second is the hard cut, so no live tail bleeds into the file.
            Command::Stop,
            Command::Stop,
            Command::SetProgram(Box::new(exporting)),
            Command::Locate(from),
            Command::Play,
        ],
    )?;
    let result = render(
        engine,
        job,
        preroll,
        cancel,
        &mut progress,
        check,
        track.is_some(),
    )
    .and_then(|(samples, peak)| output.write(job, samples, peak, cancel));
    let restored = send_all(
        handle,
        [
            // The export's tail is not for the speakers: hard cut.
            Command::Stop,
            Command::Stop,
            Command::SetProgram(Box::new(restore.clone())),
            Command::Locate(playhead),
        ],
    );
    // The master meter saw the export; the UI's ballistics should not.
    handle.telemetry().master_meter().take();
    let report = result?;
    restored.map(|()| report)
}

/// Non-RT, on the export worker, after the engine got its export rate and before [`export`]. The
/// program the export renders with: `project` compiled again for the processors that sit in the
/// engine now, so its delay compensation and routing latency are in frames of the engine's rate and
/// come from exactly the latencies [`Engine::project_latency`] reads (a program compiled earlier,
/// at the device rate or before a processor published its latency, would not line up with the
/// skip). The engine's queued commands are applied first ([`Engine::settle`]).
///
/// Slots stay as `program` has them (same project). A track bound to a named MIDI input compiles
/// as if the port were absent: the export plays clips only. Pass `program` to [`export`] as the
/// program to restore, and queue it again afterwards: the live engine plays what the app compiled.
pub fn recompile(engine: &mut Engine, project: &Project, program: &Program) -> Program {
    engine.settle();
    let env = EngineEnv {
        engine,
        slots: program,
    };
    compile::build(project, Some(program), &env)
}

/// The compiler's view of the engine on the export worker: latencies from the processors.
struct EngineEnv<'a> {
    engine: &'a Engine,
    slots: &'a Program,
}

impl CompileEnv for EngineEnv<'_> {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.slots
            .slots
            .slot(device)
            .map_or(0, |slot| self.engine.processor_latency(slot))
    }

    fn automation_parameter(
        &self,
        device: &komponist_model::Device,
        param: &komponist_model::params::ParamId,
    ) -> Option<crate::automation::Parameter> {
        use komponist_model::automation::AutomationTarget;
        self.slots
            .tracks
            .iter()
            .flat_map(|track| track.automation.iter())
            .chain(self.slots.master.automation.iter())
            .find_map(|target| match &target.model {
                AutomationTarget::Param {
                    device: id,
                    param: key,
                } if *id == device.id && key == param => target.parameter,
                _ => None,
            })
    }

    fn sample_rate(&self) -> f64 {
        f64::from(self.engine.sample_rate())
    }

    fn project_dir(&self) -> Option<PathBuf> {
        // The program the app compiled knows the folder its audio links resolve against.
        self.slots
            .tracks
            .iter()
            .filter_map(|t| t.audio.as_ref())
            .find_map(|plan| plan.dir().map(Into::into))
    }
}

/// The audio files the clips in the render's range play must be there (looked for right now, no
/// retry delay): an export would be silent where a clip is, and the user should hear about it
/// before, not find out in the file.
fn check_audio_files(program: &Program, frames: &Range<i64>) -> Result<(), ExportError> {
    for plan in program.tracks.iter().filter_map(|t| t.audio.as_ref()) {
        for segment in plan.segments() {
            if segment.start < frames.end && segment.end > frames.start {
                let source = &plan.sources()[segment.source as usize];
                if source.open_now() != SourceStatus::Ready {
                    return Err(ExportError::MissingAudio(source.link().abs.clone()));
                }
            }
        }
    }
    Ok(())
}

/// Queues `commands` in order.
fn send_all<const N: usize>(
    handle: &mut EngineHandle,
    commands: [Command; N],
) -> Result<(), ExportError> {
    for command in commands {
        handle.send(command).map_err(|_| ExportError::QueueFull)?;
    }
    Ok(())
}

/// The tick to locate to for song frame `start` (the last tick rendered at or before it) and the
/// frames from that tick's sample to `start` (the pre-roll, shorter than one tick).
fn locate_point(map: &TempoMap, rate: f64, start: u64) -> (Tick, u64) {
    let start = start as i64;
    let sample = |tick: i64| map.tick_to_sample(Tick(tick), rate);
    let mut tick = map.samples_to_ticks(start as f64, rate).floor() as i64;
    while tick > 0 && sample(tick) > start {
        tick -= 1;
    }
    while sample(tick + 1) <= start {
        tick += 1;
    }
    (Tick(tick), (start - sample(tick)) as u64)
}

/// The first tick rendered at or after song frame `end`: exactly the notes starting before it
/// start inside the range.
fn end_tick(map: &TempoMap, rate: f64, end: u64) -> Tick {
    let end = end as i64;
    let sample = |tick: i64| map.tick_to_sample(Tick(tick), rate);
    let mut tick = map.samples_to_ticks(end as f64, rate).ceil() as i64;
    while sample(tick) < end {
        tick += 1;
    }
    while tick > 0 && sample(tick - 1) >= end {
        tick -= 1;
    }
    Tick(tick)
}

/// The program the export plays: `program` with loop and metronome off, without notes starting
/// at or after `end`, notes sounding at `end` cut there, and fresh track meters.
fn export_program(
    program: &Program,
    end: Tick,
    audio_end: i64,
    target: Option<&TrackExport>,
) -> Program {
    let tracks = program
        .tracks
        .iter()
        .map(|track| {
            // Dependency tracks may continue feeding the target's sidechains during its tail.
            let limit = target.is_none_or(|target| track.id == target.id());
            let inside = |n: &SeqNote| n.start.0 + n.length.0 <= end.0;
            let notes = if !limit || track.notes.iter().all(inside) {
                Arc::clone(&track.notes)
            } else {
                track
                    .notes
                    .iter()
                    .filter(|n| n.start < end)
                    .map(|n| SeqNote {
                        length: Tick(n.length.0.min(end.0 - n.start.0)),
                        ..*n
                    })
                    .collect()
            };
            // Audio is cut at the exact sample boundary. Its offline plan owns a fresh ring.
            let audio = track
                .audio
                .as_ref()
                .map(|plan| plan.for_export(if limit { audio_end } else { i64::MAX }));
            Arc::new(TrackProgram {
                notes,
                meter: Arc::new(TrackMeter::new()),
                audio,
                ..TrackProgram::clone(track)
            })
        })
        .collect();
    Program {
        loop_on: false,
        metronome: false,
        tracks,
        ..program.clone()
    }
}

/// One `Engine::render` call inside `assert_no_alloc`: the render loop is realtime code on a
/// worker thread (rule 4), and every binary or test that installs `AllocDisabler` checks it.
fn render_block(engine: &mut Engine, frames: usize) -> &[f32] {
    assert_no_alloc::assert_no_alloc(move || engine.render(frames))
}

/// Renders the pre-roll (discarded), then range + tail. Returns the interleaved samples
/// (non-finite ones replaced by 0) and their peak.
fn render(
    engine: &mut Engine,
    job: &ExportJob,
    preroll: u64,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
    check: impl Fn(&Engine) -> Result<(), ExportError>,
    track_tail: bool,
) -> Result<(Vec<f32>, f32), ExportError> {
    let too_long = || io::Error::new(io::ErrorKind::OutOfMemory, "export too long for memory");
    let len = usize::try_from(job.frames())
        .ok()
        .and_then(|frames| frames.checked_mul(2))
        .ok_or_else(too_long)?;
    let mut samples = Vec::new();
    samples.try_reserve_exact(len).map_err(|_| too_long())?;
    let mut skip = preroll;
    while skip > 0 {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = skip.min(MAX_BLOCK as u64) as usize;
        render_block(engine, n);
        check(engine)?;
        skip -= n as u64;
    }
    let mut peak = 0.0_f32;
    let body = ((job.range.end - job.range.start) as usize) * 2;
    let mut last_signal = body;
    while samples.len() < len {
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let n = MAX_BLOCK.min((len - samples.len()) / 2);
        let first = samples.len();
        samples.extend_from_slice(render_block(engine, n));
        check(engine)?;
        for (i, s) in samples[first..].iter_mut().enumerate() {
            if !s.is_finite() {
                *s = 0.0;
            }
            peak = peak.max(s.abs());
            if track_tail && *s != 0.0 {
                last_signal = last_signal.max(first + i + 1);
            }
        }
        progress(Progress {
            fraction: (samples.len() as f64 / len as f64) as f32,
            peak_db: gain_to_db(peak),
        });
    }
    if track_tail {
        // Effects can produce sound after a silent gap. Inspect the whole budget before trimming.
        let padding =
            ExportJob::tail_frames(TRACK_END_PADDING_SECONDS, job.sample_rate) as usize * 2;
        samples.truncate(last_signal.next_multiple_of(2).saturating_add(padding));
    }
    Ok((samples, peak))
}

/// The open temporary file of an export.
struct Output {
    // Dropped first: the writer closes its file before the directory is removed.
    writer: hound::WavWriter<io::BufWriter<fs::File>>,
    staging: tempfile::TempDir,
}

const STAGING_WAV: &str = "audio.wav";

impl Output {
    /// Creates and holds the staging file before writing the WAV header.
    fn create(job: &ExportJob) -> io::Result<Output> {
        if job.path.file_name().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "export path has no file name",
            ));
        }
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: job.sample_rate,
            bits_per_sample: job.bit_depth.bits(),
            sample_format: match job.bit_depth {
                BitDepth::Float32 => hound::SampleFormat::Float,
                BitDepth::Int16 | BitDepth::Int24 => hound::SampleFormat::Int,
            },
        };
        let parent = job
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let mut builder = tempfile::Builder::new();
        builder.prefix(".komponist-export-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let staging = builder.tempdir_in(parent)?;
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(staging.path().join(STAGING_WAV))?;
        let writer = hound::WavWriter::new(io::BufWriter::new(file), spec).map_err(wav_error)?;
        Ok(Output { writer, staging })
    }

    /// Normalises, dithers, quantises and writes `samples` (peak `peak`), then renames the staged
    /// file onto the target.
    fn write(
        mut self,
        job: &ExportJob,
        mut samples: Vec<f32>,
        peak: f32,
        cancel: &AtomicBool,
    ) -> Result<ExportReport, ExportError> {
        let frames = samples.len() as u64 / 2;
        let gain = job.normalize.map_or(1.0, |db| normalize_gain(peak, db));
        if gain != 1.0 {
            scale(&mut samples, gain);
        }
        let mut rng = Rng::new(DITHER_SEED);
        let bits = u32::from(job.bit_depth.bits());
        let writer = &mut self.writer;
        for chunk in samples.chunks(WRITE_CHUNK) {
            if cancel.load(Ordering::Relaxed) {
                return Err(ExportError::Cancelled);
            }
            for &s in chunk {
                let written = match job.bit_depth {
                    BitDepth::Float32 => writer.write_sample(s),
                    BitDepth::Int16 | BitDepth::Int24 => {
                        let dither = if job.dither { tpdf(&mut rng) } else { 0.0 };
                        let value = quantize(s, bits, dither);
                        if bits == 16 {
                            writer.write_sample(value as i16)
                        } else {
                            writer.write_sample(value)
                        }
                    }
                };
                written.map_err(wav_error)?;
            }
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        let Output { writer, staging } = self;
        writer.finalize().map_err(wav_error)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(ExportError::Cancelled);
        }
        fs::rename(staging.path().join(STAGING_WAV), &job.path)?;
        Ok(ExportReport {
            frames,
            peak_db: gain_to_db(peak),
            gain_db: if gain == 1.0 { 0.0 } else { gain_to_db(gain) },
        })
    }
}

/// `hound`'s error as an IO error.
fn wav_error(error: hound::Error) -> io::Error {
    match error {
        hound::Error::IoError(error) => error,
        other => io::Error::other(other),
    }
}

/// The gain that brings a sample peak `peak` (linear) to `target_db` dBFS. 1.0 when there is
/// nothing to normalise: a peak at or below −120 dBFS ([`MIN_GAIN`], silence) or not finite.
pub fn normalize_gain(peak: f32, target_db: f32) -> f32 {
    if peak > MIN_GAIN && peak.is_finite() {
        db_to_gain(target_db) / peak
    } else {
        1.0
    }
}

/// One TPDF dither value in LSB: the difference of two independent uniform values in `[0, 1)`,
/// triangular over `(−1, 1)`, mean 0, variance 1/6 (RMS 1/√6 ≈ 0.408 LSB).
///
/// Added before rounding, it makes the total error of [`quantize`] independent of the signal in
/// mean and power: mean 0, variance 1/6 + 1/12 = 1/4 (RMS 0.5 LSB), white, without harmonics.
pub fn tpdf(rng: &mut Rng) -> f32 {
    rng.unipolar() - rng.unipolar()
}

/// `x` (full scale ±1.0) as a signed `bits`-bit integer (16 or 24): `x × 2^(bits−1) + dither`
/// rounded to the nearest integer (halves away from zero) and clipped to
/// `−2^(bits−1) ..= 2^(bits−1) − 1`. `dither` is in LSB ([`tpdf`], or 0). NaN gives 0.
pub fn quantize(x: f32, bits: u32, dither: f32) -> i32 {
    let full = f64::from(1_u32 << (bits - 1));
    let value = (f64::from(x) * full + f64::from(dither)).round();
    value.clamp(-full, full - 1.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    use komponist_model::{Meter, Tempo};

    /// 124 bpm: 48 kHz gives 24.19… samples per tick, so most ticks fall between samples.
    fn map() -> TempoMap {
        TempoMap::constant(Tempo::new(124.0), Meter::FOUR_FOUR)
    }

    #[test]
    fn tpdf_is_triangular_over_one_lsb_with_zero_mean() {
        let mut rng = Rng::new(3);
        let n = 400_000;
        let values: Vec<f64> = (0..n).map(|_| f64::from(tpdf(&mut rng))).collect();
        assert!(values.iter().all(|v| v.abs() < 1.0));
        let mean = values.iter().sum::<f64>() / n as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
        // Standard error of the mean: √(1/6 / n) ≈ 0.00065.
        assert!(mean.abs() < 0.003, "mean {mean}");
        assert!((variance - 1.0 / 6.0).abs() < 0.002, "variance {variance}");
        // Triangular density 1 − |x|: P(|x| < 0.5) = 3/4, P(|x| < 0.25) = 7/16, and the four
        // quarters of (−1, 1) hold 1/8, 3/8, 3/8, 1/8.
        let share =
            |f: &dyn Fn(f64) -> bool| values.iter().filter(|&&v| f(v)).count() as f64 / n as f64;
        assert!((share(&|v| v.abs() < 0.5) - 0.75).abs() < 0.005);
        assert!((share(&|v| v.abs() < 0.25) - 7.0 / 16.0).abs() < 0.005);
        let quarters = [
            (-1.0, -0.5, 0.125),
            (-0.5, 0.0, 0.375),
            (0.0, 0.5, 0.375),
            (0.5, 1.0, 0.125),
        ];
        for (lo, hi, expected) in quarters {
            let got = share(&|v| lo <= v && v < hi);
            assert!((got - expected).abs() < 0.005, "{lo}..{hi}: {got}");
        }
        // White: neighbouring values are uncorrelated.
        let lag1 = values.windows(2).map(|w| w[0] * w[1]).sum::<f64>() / n as f64;
        assert!((lag1 / variance).abs() < 0.01, "lag-1 correlation {lag1}");
    }

    #[test]
    fn quantize_rounds_and_clips_16_bit() {
        let lsb = 1.0 / 32768.0;
        assert_eq!(quantize(0.0, 16, 0.0), 0);
        assert_eq!(quantize(0.5, 16, 0.0), 16384);
        assert_eq!(quantize(-0.5, 16, 0.0), -16384);
        assert_eq!(quantize(0.49 * lsb, 16, 0.0), 0);
        assert_eq!(quantize(0.51 * lsb, 16, 0.0), 1);
        assert_eq!(quantize(-0.51 * lsb, 16, 0.0), -1);
        assert_eq!(quantize(0.0, 16, 0.6), 1);
        assert_eq!(quantize(0.0, 16, -0.6), -1);
        assert_eq!(quantize(0.0, 16, 0.4), 0);
        // Full scale: +1.0 clips to the largest code, −1.0 is the smallest exactly.
        assert_eq!(quantize(1.0, 16, 0.0), 32767);
        assert_eq!(quantize(1.0 - lsb, 16, 0.0), 32767);
        assert_eq!(quantize(1.0 - lsb, 16, 0.9), 32767);
        assert_eq!(quantize(-1.0, 16, 0.0), -32768);
        assert_eq!(quantize(-1.0, 16, -0.9), -32768);
        assert_eq!(quantize(3.5, 16, 0.0), 32767);
        assert_eq!(quantize(-3.5, 16, 0.0), -32768);
        assert_eq!(quantize(f32::INFINITY, 16, 0.0), 32767);
        assert_eq!(quantize(f32::NEG_INFINITY, 16, 0.0), -32768);
        assert_eq!(quantize(f32::NAN, 16, 0.0), 0);
    }

    #[test]
    fn quantize_rounds_and_clips_24_bit() {
        let lsb = 1.0 / 8_388_608.0;
        assert_eq!(quantize(0.5, 24, 0.0), 4_194_304);
        assert_eq!(quantize(0.25 + 3.0 * lsb, 24, 0.0), 2_097_155);
        assert_eq!(quantize(-0.25 - 3.0 * lsb, 24, 0.0), -2_097_155);
        assert_eq!(quantize(0.0, 24, 0.7), 1);
        assert_eq!(quantize(1.0, 24, 0.0), 8_388_607);
        assert_eq!(quantize(-1.0, 24, 0.0), -8_388_608);
        assert_eq!(quantize(2.0, 24, 0.0), 8_388_607);
        assert_eq!(quantize(-2.0, 24, 0.0), -8_388_608);
        // The dither reaches the LSB even near full scale (computed in f64).
        assert_eq!(quantize(0.75, 24, 0.6), 6_291_457);
    }

    #[test]
    fn normalize_gain_brings_the_peak_to_the_target() {
        let target = db_to_gain(NORMALIZE_TARGET_DB);
        for peak in [0.5_f32, 0.001, 1.0, 1.7] {
            let gain = normalize_gain(peak, NORMALIZE_TARGET_DB);
            assert!((peak * gain - target).abs() < 1e-6, "{peak}");
            assert!((gain_to_db(peak * gain) - NORMALIZE_TARGET_DB).abs() < 1e-4);
        }
        assert_eq!(normalize_gain(0.0, -1.0), 1.0);
        assert_eq!(normalize_gain(MIN_GAIN, -1.0), 1.0);
        assert_eq!(normalize_gain(f32::INFINITY, -1.0), 1.0);
        assert_eq!(normalize_gain(f32::NAN, -1.0), 1.0);
        assert!((normalize_gain(0.25, -6.0) - db_to_gain(-6.0) * 4.0).abs() < 1e-6);
    }

    /// Power of `signal` at `cycles` cycles over its length (one DFT bin), normalised so that a
    /// sine of amplitude `a` on the bin gives `a² / 2`.
    fn bin_power(signal: &[f64], cycles: f64) -> f64 {
        let n = signal.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in signal.iter().enumerate() {
            let phase = core::f64::consts::TAU * cycles * i as f64 / n;
            re += x * phase.cos();
            im += x * phase.sin();
        }
        2.0 * (re * re + im * im) / (n * n)
    }

    #[test]
    fn dither_turns_quantisation_distortion_into_white_noise() {
        // A sine of 3 LSB at 16 bit, exactly 101 cycles in 2^16 samples, quantised with and
        // without dither. Error in LSB.
        let n = 1 << 16;
        let cycles = 101.0;
        let amplitude = 3.0 / 32768.0;
        let sine: Vec<f32> = (0..n)
            .map(|i| {
                let phase = core::f64::consts::TAU * cycles * i as f64 / n as f64;
                (amplitude * phase.sin()) as f32
            })
            .collect();
        let error = |dither: &mut dyn FnMut() -> f32| -> Vec<f64> {
            sine.iter()
                .map(|&x| f64::from(quantize(x, 16, dither())) - f64::from(x) * 32768.0)
                .collect()
        };
        let plain = error(&mut || 0.0);
        let mut rng = Rng::new(11);
        let dithered = error(&mut || tpdf(&mut rng));
        let rms = |e: &[f64]| (e.iter().map(|x| x * x).sum::<f64>() / e.len() as f64).sqrt();
        let mean = |e: &[f64]| e.iter().sum::<f64>() / e.len() as f64;
        // Total error of TPDF-dithered rounding: variance 1/6 + 1/12 = 1/4.
        assert!(
            (rms(&dithered) - 0.5).abs() < 0.01,
            "rms {}",
            rms(&dithered)
        );
        assert!(mean(&dithered).abs() < 0.01);
        // Undithered, the error of this low-level sine is periodic: its odd harmonics stand far
        // above the white floor (expected bin power of white noise with variance σ²: 2σ²/n;
        // measured: ≈ 250×).
        let floor = 2.0 * 0.25 / n as f64;
        let harmonic_peak = |e: &[f64]| {
            [3.0, 5.0, 7.0]
                .iter()
                .map(|h| bin_power(e, h * cycles))
                .fold(0.0, f64::max)
        };
        assert!(
            harmonic_peak(&plain) > 100.0 * floor,
            "{}",
            harmonic_peak(&plain)
        );
        // Dithered, the harmonics vanish into the floor (an exponential bin exceeds 10× its mean
        // with probability e^−10).
        assert!(
            harmonic_peak(&dithered) < 10.0 * floor,
            "{}",
            harmonic_peak(&dithered)
        );
        // And the signal itself comes through unchanged on average.
        let signal_power = bin_power(
            &sine
                .iter()
                .map(|&x| f64::from(x) * 32768.0)
                .collect::<Vec<_>>(),
            cycles,
        );
        let restored: Vec<f64> = dithered
            .iter()
            .zip(&sine)
            .map(|(e, &x)| e + f64::from(x) * 32768.0)
            .collect();
        assert!((bin_power(&restored, cycles) / signal_power - 1.0).abs() < 0.02);
    }

    #[test]
    fn locate_point_is_the_last_tick_at_or_before_the_start() {
        let map = map();
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            for start in [0_u64, 1, 23, 24, 25, 48_000, 96_011, 1_234_567] {
                let (tick, preroll) = locate_point(&map, rate, start);
                let at = map.tick_to_sample(tick, rate);
                assert_eq!(at as u64 + preroll, start, "{rate} Hz, frame {start}");
                assert!(map.tick_to_sample(Tick(tick.0 + 1), rate) > start as i64);
                assert!((preroll as f64) < map.samples_per_tick(rate));
            }
            // A frame on a tick needs no pre-roll.
            let on = map.tick_to_sample(Tick(3840 + 7), rate) as u64;
            assert_eq!(locate_point(&map, rate, on), (Tick(3840 + 7), 0));
        }
    }

    #[test]
    fn end_tick_is_the_first_tick_at_or_after_the_end() {
        let map = map();
        for rate in [44_100.0, 48_000.0] {
            for end in [0_u64, 1, 24, 25, 96_011, 1_234_567] {
                let tick = end_tick(&map, rate, end);
                assert!(
                    map.tick_to_sample(tick, rate) >= end as i64,
                    "{rate} Hz, {end}"
                );
                assert!(
                    tick == Tick::ZERO || map.tick_to_sample(Tick(tick.0 - 1), rate) < end as i64
                );
            }
            let on = map.tick_to_sample(Tick(2 * 3840), rate) as u64;
            assert_eq!(end_tick(&map, rate, on), Tick(2 * 3840));
        }
    }

    #[test]
    fn frames_of_maps_tick_ranges_like_the_engine() {
        let map = map();
        let range = TickRange::new(Tick(3840), Tick(4 * 3840));
        let frames = ExportJob::frames_of(&map, range, 48_000);
        assert_eq!(
            frames.start as i64,
            map.tick_to_sample(Tick(3840), 48_000.0)
        );
        assert_eq!(
            frames.end as i64,
            map.tick_to_sample(Tick(4 * 3840), 48_000.0)
        );
        let empty = ExportJob::frames_of(&map, TickRange::new(Tick(3840), Tick(0)), 48_000);
        assert!(empty.is_empty());
        let negative = ExportJob::frames_of(&map, TickRange::new(Tick(-3840), Tick(0)), 48_000);
        assert_eq!(negative, 0..0);
        assert_eq!(ExportJob::tail_frames(DEFAULT_TAIL_SECONDS, 48_000), 96_000);
        assert_eq!(ExportJob::tail_frames(2.0, 44_100), 88_200);
        assert_eq!(ExportJob::tail_frames(-1.0, 44_100), 0);
    }

    #[test]
    fn the_export_program_plays_nothing_past_the_end() {
        use crate::program::{MasterProgram, SlotMap};
        use komponist_model::{
            Channel, Hue, MidiChannelSel, MidiInputSel, Track, TrackId, TrackKind,
        };

        let note = |start: i64, length: i64, key: u8| SeqNote {
            start: Tick(start),
            length: Tick(length),
            key,
            velocity: 100,
            probability: 100,
            channel: 0,
        };
        let source = Arc::new(Track {
            id: TrackId(1),
            name: "Spur".to_owned(),
            hue: Hue::RED,
            kind: TrackKind::Midi,
            channel: Channel::default(),
            arm: false,
            monitor: false,
            input: MidiInputSel::All,
            midi_channel: MidiChannelSel::Omni,
            audio: komponist_model::AudioIo::default(),
            height: 56,
            clips: Vec::new(),
            output: komponist_model::TrackOutput::Master,
            sends: Vec::new(),
            folder: false,
            folded: false,
            automation: Default::default(),
        });
        let track = |notes: Vec<SeqNote>| {
            Arc::new(TrackProgram {
                automation: Default::default(),
                chain_latency: 0,
                id: TrackId(1),
                source: Arc::clone(&source),
                notes: notes.into(),
                chain: Vec::new(),
                gain: 1.0,
                pan: 0.0,
                mute: false,
                solo: false,
                arm: false,
                monitor: false,
                input: crate::program::InputFilter {
                    port: None,
                    channel: None,
                    port_missing: false,
                },
                pdc_delay: 0,
                meter: Arc::new(TrackMeter::new()),
                audio: None,
            })
        };
        let program = Program {
            tempo_map: map(),
            loop_range: TickRange::new(Tick(0), Tick(960)),
            loop_on: true,
            metronome: true,
            count_in_bars: 1,
            song_end: Tick(4000),
            tracks: vec![
                track(vec![
                    note(0, 100, 60),
                    note(900, 200, 61),
                    note(1000, 10, 62),
                ]),
                track(vec![note(0, 1000, 64)]),
            ],
            master: MasterProgram {
                automation: Default::default(),
                automation_source: Default::default(),
                chain_latency: 0,
                chain: Vec::new(),
                gain: 1.0,
                pan: 0.0,
                mute: false,
            },
            slots: Arc::new(SlotMap::default()),
            routing: Default::default(),
        };
        let export = export_program(&program, Tick(1000), 25_000, None);
        assert!(!export.loop_on && !export.metronome);
        assert_eq!(export.song_end, program.song_end);
        assert_eq!(
            &*export.tracks[0].notes,
            &[note(0, 100, 60), note(900, 100, 61)]
        );
        // Untouched notes are shared; meters are new.
        assert!(Arc::ptr_eq(
            &export.tracks[1].notes,
            &program.tracks[1].notes
        ));
        assert!(!Arc::ptr_eq(
            &export.tracks[1].meter,
            &program.tracks[1].meter
        ));
        assert!(
            export_program(&program, Tick(0), 0, None)
                .tracks
                .iter()
                .all(|t| t.notes.is_empty())
        );
    }
}
