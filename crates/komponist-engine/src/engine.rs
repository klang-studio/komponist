//! The realtime engine (ARCHITECTURE §6.1, §6.4).
//!
//! `Engine::process` runs on the audio thread, called by the app's `AudioProcess` implementation
//! (`komponist-audio` drives it; the engine does not know that crate). Per block of at most
//! [`crate::MAX_BLOCK`] frames (larger device buffers are cut into chunks):
//!
//! 1. Drain commands (bounded).
//! 2. Live MIDI input → monitored tracks (and capture while recording).
//! 3. Transport: the block's tick range, split at the loop end and at the end of the count-in;
//!    metronome clicks (accent on beat 1).
//! 4. Per track: sequencer notes from the program (note-offs from the active-notes table) + live
//!    events → processor chain → fader gain ramp → balance → meter → master bus (after the PDC
//!    delay).
//! 5. Master: inserts → master fader → meter → output; the metronome is added after the master
//!    fader with its own level.
//! 6. Telemetry and events.
//!
//! Realtime rules: no allocation, freeing, locking, logging, IO or blocking syscalls inside
//! `process` and `render` (only the futex wait of external processors, with a deadline). Everything
//! retired goes back as `Garbage`. FTZ/DAZ is set by the audio thread (`komponist-audio`).
//!
//! Export and tests run the engine without an audio device through [`Engine::render`]. To export
//! while a stream runs, the app takes the engine out of the stream with `komponist-audio`'s detach
//! and puts it back afterwards (this replaces the `Detach` command of ARCHITECTURE §6.3).

use crate::api::EngineHandle;
use crate::processor::ProcessorError;
use crate::program::{MidiPort, SlotId};

/// Engine settings fixed at creation (the sample rate can change later with
/// [`Engine::set_sample_rate`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EngineConfig {
    /// Sample rate in Hz.
    pub sample_rate: f32,
    /// Largest number of frames per processor call, 1 ..= [`crate::MAX_BLOCK`]; processors are
    /// activated with it.
    pub max_block: usize,
    /// Base seed of the per-track probability RNGs (reseeded from it at every playback start, so
    /// offline renders are reproducible).
    pub seed: u32,
}

impl Default for EngineConfig {
    /// 48 kHz, `MAX_BLOCK`, seed 1.
    fn default() -> EngineConfig {
        EngineConfig {
            sample_rate: 48_000.0,
            max_block: crate::MAX_BLOCK,
            seed: 1,
        }
    }
}

/// A live MIDI message for this callback, already mapped to a frame offset by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MidiInEvent {
    /// Frame offset within the `process` call, `0 .. frames`.
    pub frame: u32,
    /// Port it came from.
    pub port: MidiPort,
    /// MIDI 1.0 channel message (status, data1, data2; unused bytes 0). Note-on with velocity 0 is
    /// a note-off. System messages are not passed in.
    pub data: [u8; 3],
}

/// Live input of one `process` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveInput<'a> {
    /// Live MIDI messages sorted by `frame`.
    pub midi: &'a [MidiInEvent],
    /// Output latency of the stream in frames (from the callback timing). Recorded ticks are
    /// corrected by it, so a note played in time with what the player hears lands on the beat.
    pub output_latency: u32,
}

/// The engine. Lives in a `Box` owned by the audio callback (or a render worker).
pub struct Engine {
    _private: (),
}

impl Engine {
    /// Non-RT. Creates the engine with empty program and slot table (all capacities preallocated:
    /// [`crate::MAX_TRACKS`], [`crate::MAX_SLOTS`], …) and the UI's handle to it.
    pub fn new(config: EngineConfig) -> (Box<Engine>, EngineHandle) {
        todo!("WP-23")
    }

    /// The current sample rate in Hz.
    pub fn sample_rate(&self) -> f32 {
        todo!("WP-23")
    }

    /// Non-RT (engine out of the audio callback). Changes the sample rate and re-activates every
    /// processor. Processors that fail stay in their slot but are treated as failed; their slots
    /// are returned.
    pub fn set_sample_rate(
        &mut self,
        sample_rate: f32,
    ) -> Result<(), Vec<(SlotId, ProcessorError)>> {
        todo!("WP-23")
    }

    /// RT. Renders `out_l.len()` frames (any length; `out_l` and `out_r` have equal length) of the
    /// master output into the two planar buffers, overwriting them. `input` carries this callback's
    /// live MIDI.
    pub fn process(&mut self, out_l: &mut [f32], out_r: &mut [f32], input: &LiveInput<'_>) {
        todo!("WP-23")
    }

    /// RT-safe, for offline use. Processes `frames` (1 ..= [`crate::MAX_BLOCK`]) frames with
    /// `offline = true` and no live input, and returns them interleaved stereo (`[l0, r0, l1, …]`,
    /// `2 × frames` samples) from an internal buffer valid until the next call. Panics if `frames`
    /// is 0 or larger than `MAX_BLOCK`.
    pub fn render(&mut self, frames: usize) -> &[f32] {
        todo!("WP-23")
    }
}

impl core::fmt::Debug for Engine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}
