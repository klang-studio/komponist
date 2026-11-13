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

use komponist_model::{Meter, TickRange};

/// MIDI note id meaning "no id" (live input, or when the sender does not track notes).
pub const NOTE_ID_NONE: u32 = u32::MAX;

/// One device in a chain, driven by the engine.
///
/// `Send` because it is created on the UI thread and moved to the audio thread. Methods marked RT
/// run on the audio thread and must not allocate, free, lock, log, do IO or block (CLAUDE.md
/// rule 4). Tests wrap them in `assert_no_alloc`.
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

    /// RT-safe and cheap (a cached value). Latency the processor adds, in frames at the active
    /// sample rate. The app passes the same value to the compiler for delay compensation
    /// (`compile::CompileEnv::device_latency`).
    fn latency(&self) -> u32;

    /// RT. Silences all voices and clears internal state (tails, delay lines) without
    /// deallocating. Called on stop, on locate while playing and when bypass turns on. Not on a loop
    /// wrap: there only the sequenced notes end, so tails keep sounding across the loop.
    fn reset(&mut self);
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
