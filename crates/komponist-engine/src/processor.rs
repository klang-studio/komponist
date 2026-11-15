//! The `Processor` trait and the per-block types it sees (ARCHITECTURE §6.2).
//!
//! A processor is one device in a channel chain: an instrument (makes sound from note events) or an
//! effect (processes audio). Implementations: `PisynthProcessor` (`komponist-builtin`),
//! `ExternalProcessor` (`komponist-plugins`, talks to a bridge process), `TestSine` / `TestGain`
//! (engine tests). The metronome is engine-internal and not a processor.
//!
//! Lifecycle: created and [`Processor::activate`]d on a non-RT thread, sent to the engine with
//! `Command::InsertProcessor`, processed on the audio thread, sent back through the garbage queue
//! and dropped on the UI thread. A processor is never dropped on the audio thread.

use std::time::Instant;

use komponist_model::{Meter, TickRange};

/// MIDI note id meaning "no id" (live input, or when the sender does not track notes).
pub const NOTE_ID_NONE: u32 = u32::MAX;

/// Share of the callback period by which every processor of a realtime callback must be done
/// (ARCHITECTURE §8.1): [`ProcessContext::deadline`] = callback start + `DEADLINE_SHARE` × the
/// callback's frames / sample rate.
pub const DEADLINE_SHARE: f64 = 0.6;

/// One device in a chain, driven by the engine.
///
/// `Send` because it is created on the UI thread and moved to the audio thread. Methods marked RT
/// run on the audio thread or on one of the engine's worker threads (never on two threads at
/// once) and must not allocate, free, lock, log, do IO or block (CLAUDE.md rule 4). Tests wrap
/// them in `assert_no_alloc`.
pub trait Processor: Send {
    /// Non-RT. Prepares for `sample_rate` Hz and calls of at most `max_block` frames
    /// (≤ [`crate::MAX_BLOCK`]). Called before the processor goes into the engine and again after
    /// a sample rate change (the engine is then out of the audio callback). May allocate.
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError>;

    /// RT. Processes one call of `ctx.frames` frames (1 ..= `max_block`).
    ///
    /// `events` are sorted by [`Event::offset`] (all `< ctx.frames`; equal offsets keep their send
    /// order, note-offs before note-ons at the same offset). Audio is stereo in and out:
    /// instruments ignore `io.input` and must write every output frame; effects read `io.input`
    /// and write `io.output`. Input and output never alias.
    ///
    /// Returning [`ProcessStatus::Failed`] makes the engine report `EngineEvent::ProcessorFailed`
    /// once and treat the slot as bypassed until the processor is replaced.
    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus;

    /// RT, optional (WP-102). [`Processor::process`] with the device's **sidechain input**: the
    /// signal of the track or bus its sidechain taps (ARCHITECTURE §6.8.2), stereo, `ctx.frames`
    /// samples each, lined up in time with `io.input` by the engine's delay compensation. Pre taps
    /// ignore the source's fader, mute and solo; post taps follow fader, pan and mute.
    ///
    /// The engine calls it instead of `process` on every call of a device whose sidechain is set
    /// (with silence while the source is silent), and `process` otherwise. The default ignores the
    /// sidechain; devices with a sidechain input override it (internal devices: WP-104, plugins
    /// through the bridge: WP-105). Same rules as `process`.
    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        let _ = sidechain;
        self.process(ctx, events, io)
    }

    /// RT, optional (WP-65). Starts the next [`Processor::process`] call early, for processors
    /// whose work runs elsewhere (a bridge process): the engine calls it for the instrument at the
    /// head of a track chain as soon as the track's events are known, before it processes any
    /// chain of the segment, so the bridge works while the engine runs the other tracks.
    ///
    /// `ctx`, `events` and `input` (silence, `ctx.frames` samples each) are exactly what the
    /// following `process` call gets; the engine calls `process` in the same segment, possibly on
    /// another thread. Returns whether the call was started: `process` then completes it (waits
    /// for the result or writes the fallback) instead of starting it. The default does nothing
    /// and returns `false`.
    fn begin(&mut self, ctx: &ProcessContext, events: &[Event], input: [&[f32]; 2]) -> bool {
        let _ = (ctx, events, input);
        false
    }

    /// RT-safe and cheap (a cached value). Latency the processor adds, in frames at the active
    /// sample rate. The app passes the same value to the compiler for delay compensation
    /// (`compile::CompileEnv::device_latency`).
    fn latency(&self) -> u32;

    /// RT. Silences all voices and clears internal state (tails, delay lines) without
    /// deallocating. Called on stop, on locate while playing and when bypass turns on. Not on a loop
    /// wrap: there only the sequenced notes end, so tails keep sounding across the loop.
    fn reset(&mut self);

    /// RT-safe, cheap (WP-88). `Some` for a MIDI effect ([`DeviceRole::MidiEffect`] in the project):
    /// the engine runs its [`EventProcessor`] stage in front of the instrument and never calls
    /// [`Processor::process`]. Everything else returns `None` (the default).
    ///
    /// [`DeviceRole::MidiEffect`]: komponist_model::DeviceRole::MidiEffect
    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor> {
        None
    }
}

/// The event stage of a MIDI effect (WP-88, ARCHITECTURE §6.4.2): events in, events out, in front of
/// the instrument.
///
/// A track chain is `[MIDI effects…] → instrument → [audio effects…]`. Per processor call (segment),
/// on the track's job (so on a worker thread with the worker pool, never on two threads at once),
/// the engine feeds the sequencer's and the live input's events through the active stages in chain
/// order and hands the result to the instrument. A bypassed stage, or one whose slot has no
/// processor, passes its input through. All methods are RT: no allocation, locks, logging or IO.
///
/// What a stage sees: the track's **input** (sequenced notes, including chased ones, and live MIDI
/// of monitored inputs; recording captures the same input before the stage), sorted by offset with
/// note-offs before note-ons at one offset. What it need not do: keep notes from hanging. The engine
/// keeps a table of the notes the instrument received and ends every one on bypass, removal,
/// reordering, stop, locate and panic, and drops note-offs for notes it never started, so a stage
/// only has to be musically right.
pub trait EventProcessor {
    /// Parameter `id` (the device's wire id) now has plain value `value`. Applied before the next
    /// [`EventProcessor::process_events`], in send order.
    fn set_param(&mut self, id: u32, value: f64);

    /// Rewrites one call's events. `input` is sorted (see above) with offsets `< ctx.frames`;
    /// `ctx.transport` is the state at frame 0 (tempo, meter, position in ticks, playing, loop), and
    /// the call covers `ctx.frames` samples of constant tempo. Push the output to `out` with offsets
    /// `< ctx.frames`; pushes beyond the bounded capacity return `false` and are counted as dropped
    /// events. Output note-ons may carry any note id; the engine assigns the ids the instrument
    /// sees.
    fn process_events(&mut self, ctx: &ProcessContext, input: &[Event], out: &mut EventSink<'_>);

    /// Forget held notes, phases and pending note-offs without emitting anything (stop, locate,
    /// panic, bypass on). The engine ends the sounding notes itself.
    fn reset(&mut self);
}

/// Where an [`EventProcessor`] writes its output. Bounded: [`crate::MAX_EVENTS_PER_BLOCK`] events
/// per call, of which the engine keeps room for the note-offs of sounding notes.
#[derive(Debug)]
pub struct EventSink<'a> {
    list: &'a mut crate::sequencer::EventList,
    frames: u32,
}

impl<'a> EventSink<'a> {
    pub(crate) fn new(list: &'a mut crate::sequencer::EventList, frames: u32) -> EventSink<'a> {
        EventSink { list, frames }
    }

    /// RT. Adds an event at `offset` (clamped to the call). `false` when the output is full.
    pub fn push(&mut self, offset: u32, kind: EventKind) -> bool {
        self.list
            .push(offset.min(self.frames.saturating_sub(1)), kind)
    }

    /// Events that still fit.
    pub fn room(&self) -> usize {
        self.list.room()
    }
}

/// A preallocated event buffer for driving an [`EventProcessor`] outside the engine (unit tests of
/// MIDI effects): [`EventBuffer::sink`], then [`EventBuffer::events`] sorted as the engine sorts.
#[derive(Debug)]
pub struct EventBuffer(crate::sequencer::EventList);

impl EventBuffer {
    /// Non-RT. A buffer for at most `capacity` events.
    pub fn new(capacity: usize) -> EventBuffer {
        EventBuffer(crate::sequencer::EventList::new(capacity))
    }

    /// RT. Empties the buffer and opens a sink for a call of `frames` frames.
    pub fn sink(&mut self, frames: u32) -> EventSink<'_> {
        self.0.clear();
        EventSink::new(&mut self.0, frames)
    }

    /// RT. The events pushed since [`EventBuffer::sink`], sorted by offset (note-offs before
    /// note-ons at one offset, otherwise in push order).
    pub fn events(&mut self) -> &[Event] {
        self.0.finish()
    }
}

/// Why activation failed.
#[derive(Debug, Clone, PartialEq)]
pub enum ProcessorError {
    /// The processor cannot run at this sample rate (Hz).
    UnsupportedSampleRate(f32),
    /// `max_block` is larger than the processor supports.
    BlockTooLarge(usize),
    /// Anything else, with a message for the log.
    Failed(String),
}

impl core::fmt::Display for ProcessorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProcessorError::UnsupportedSampleRate(rate) => {
                write!(f, "unsupported sample rate {rate} Hz")
            }
            ProcessorError::BlockTooLarge(n) => write!(f, "block of {n} frames too large"),
            ProcessorError::Failed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ProcessorError {}

/// Result of one [`Processor::process`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessStatus {
    /// Output written normally.
    Continue,
    /// The processor failed permanently (plugin error, dead bridge). Its output for this call has
    /// already been replaced by the fallback (silence for instruments, input copied for effects).
    Failed,
}

/// A sample-accurate event for one processor call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Frame offset within the call, `0 .. ctx.frames`.
    pub offset: u32,
    /// What happens.
    pub kind: EventKind,
}

/// Event payloads. Velocities and keys are MIDI 1.0 values.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EventKind {
    /// Note start. `key` 0 ..= 127, `velocity` 1 ..= 127, `channel` 0 ..= 15.
    NoteOn {
        /// MIDI key.
        key: u8,
        /// MIDI velocity, never 0 (0 is sent as `NoteOff`).
        velocity: u8,
        /// MIDI channel 0 ..= 15.
        channel: u8,
        /// Engine-assigned id pairing on and off (CLAP note id), or [`NOTE_ID_NONE`].
        note_id: u32,
    },
    /// Note end.
    NoteOff {
        /// MIDI key.
        key: u8,
        /// MIDI channel 0 ..= 15.
        channel: u8,
        /// Same id as the matching `NoteOn`, or [`NOTE_ID_NONE`].
        note_id: u32,
    },
    /// Any other MIDI 1.0 channel message as 3 bytes (status, data1, data2; unused bytes 0): CC,
    /// pitch bend, aftertouch from live input. CC lanes come later.
    Midi([u8; 3]),
    /// Sets parameter `id` to `value` in the parameter's plain range (its `min ..= max`). Used for
    /// pisynth macros now and automation later.
    Param {
        /// Parameter id (stable per processor type).
        id: u32,
        /// New value, plain (not normalised).
        value: f64,
    },
    /// Release all sounding notes now (stop, locate while playing, program swap cleanup; not on a
    /// loop wrap).
    AllNotesOff,
}

/// What a processor call knows about time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessContext {
    /// Frames in this call, 1 ..= the activated `max_block`.
    pub frames: u32,
    /// Sample rate in Hz (as activated).
    pub sample_rate: f32,
    /// Transport state at frame 0 of this call.
    pub transport: TransportInfo,
    /// `true` during offline render (export): processors may use higher quality and must not
    /// expect realtime pacing (CLAP render extension, VST3 `kOffline`).
    pub offline: bool,
    /// When this realtime callback's processing must be done: `Engine::process` takes the time once
    /// at entry and adds [`DEADLINE_SHARE`] × the callback period (all of its frames / sample
    /// rate). Every segment and every processor of the callback gets the same value, so processors
    /// that wait (bridges) share one budget instead of each taking their own. A processor that
    /// would wait past it gives up and uses its fallback. `None` for offline render (no pacing)
    /// and for calls outside the engine.
    pub deadline: Option<Instant>,
}

/// Transport snapshot at frame 0 of a processor call (maps onto CLAP's transport event and VST3's
/// `ProcessContext`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransportInfo {
    /// Transport running (also during the count-in).
    pub playing: bool,
    /// Recording (capture active, after the count-in).
    pub recording: bool,
    /// Loop on.
    pub looping: bool,
    /// Tempo in quarter notes per minute.
    pub tempo_bpm: f64,
    /// Meter.
    pub meter: Meter,
    /// Song position in (fractional) ticks at frame 0; negative during the count-in. Quarter notes =
    /// `position_ticks / 960`.
    pub position_ticks: f64,
    /// Loop range in ticks.
    pub loop_range: TickRange,
}

/// Stereo input and output buffers of one processor call.
///
/// All four slices have exactly `ctx.frames` samples. `input` and `output` never alias.
#[derive(Debug)]
pub struct StereoIo<'a> {
    /// Left and right input.
    pub input: [&'a [f32]; 2],
    /// Left and right output.
    pub output: [&'a mut [f32]; 2],
}

impl StereoIo<'_> {
    /// Frames in this call.
    pub fn frames(&self) -> usize {
        self.output[0].len()
    }

    /// RT. Copies input to output (bypassed effect, fallback).
    pub fn passthrough(&mut self) {
        self.output[0].copy_from_slice(self.input[0]);
        self.output[1].copy_from_slice(self.input[1]);
    }

    /// RT. Writes silence (bypassed instrument, fallback).
    pub fn silence(&mut self) {
        self.output[0].fill(0.0);
        self.output[1].fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stereo_io_fallbacks() {
        let (l, r) = ([1.0_f32, 2.0], [3.0_f32, 4.0]);
        let (mut ol, mut or) = ([0.0_f32; 2], [9.0_f32; 2]);
        let mut io = StereoIo {
            input: [&l, &r],
            output: [&mut ol, &mut or],
        };
        assert_eq!(io.frames(), 2);
        io.passthrough();
        assert_eq!((io.output[0][1], io.output[1][0]), (2.0, 3.0));
        io.silence();
        assert!(io.output.iter().all(|c| c.iter().all(|&s| s == 0.0)));
    }
}
