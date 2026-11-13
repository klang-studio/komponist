//! The realtime engine of komponist.
//!
//! Transport, sequencer, mixer, metronome and the `Processor` trait run inside `Engine::process` on
//! the audio thread, which never allocates, locks or blocks. The program compiler turns a project
//! into the immutable program the engine plays. Offline render and WAV export live here too. The
//! engine knows no UI, no IPC, no plugin format and no audio backend.
//!
//! Module map (ARCHITECTURE §6):
//! - [`processor`]: the [`Processor`] trait, events, [`ProcessContext`], [`StereoIo`].
//! - [`program`]: the compiled [`Program`] the engine plays.
//! - [`compile`]: project → program, with processor-slot mapping.
//! - [`api`]: [`Command`], [`EngineEvent`], [`Garbage`], [`Telemetry`], [`TrackMeter`],
//!   [`EngineHandle`].
//! - [`engine`]: [`Engine::new`], [`Engine::process`], [`Engine::render`].
//!
//! Threads: `Engine` lives on the audio thread (or a render worker); `EngineHandle`, the compiler
//! and processor creation run on the UI thread. Everything crossing between them goes through the
//! rtrb queues of [`api`] or atomics.

// Contract stage (WP-01): bodies are `todo!()` until WP-23 / WP-24 implement them, so parameters
// are unused for now. Remove this allow when the implementation lands.
#![allow(unused_variables)]

pub mod api;
pub mod compile;
pub mod engine;
pub mod processor;
pub mod program;

pub use api::{Command, EngineEvent, EngineHandle, Garbage, Telemetry, TrackMeter, TransportState};
pub use engine::{Engine, EngineConfig, LiveInput, MidiInEvent};
pub use processor::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, ProcessStatus, Processor, ProcessorError,
    StereoIo, TransportInfo,
};
pub use program::{MidiPort, Program, SlotId};

/// Largest number of frames per processor call. Device buffers above it are processed in chunks;
/// shared memory to the bridges is sized for it (`komponist_ipc::shm::MAX_BLOCK` must be equal).
pub const MAX_BLOCK: usize = 2048;

/// Most tracks a program holds; the engine preallocates per-track state (active notes, RNG, PDC
/// delay, gain ramp) for this many.
pub const MAX_TRACKS: usize = 128;

/// Size of the engine's processor slot table ([`SlotId`] `0 .. MAX_SLOTS`).
pub const MAX_SLOTS: usize = 1024;

/// Most events one processor call receives; further events of that call are dropped (note-offs
/// first in line) and counted in [`Telemetry::dropped_events`].
pub const MAX_EVENTS_PER_BLOCK: usize = 1024;

/// Most commands the engine applies per block; the rest wait for the next block.
pub const MAX_COMMANDS_PER_BLOCK: usize = 64;

/// Capacity of the UI → engine command queue.
pub const COMMAND_CAPACITY: usize = 1024;

/// Capacity of the engine → UI event queue (events plus garbage).
pub const EVENT_CAPACITY: usize = 4096;

/// Longest delay-compensation line per track, in frames.
pub const MAX_PDC_FRAMES: usize = 8192;

#[cfg(test)]
mod tests {
    /// Proves that the sibling crate `../klang-dsp` resolves and links.
    #[test]
    fn klang_dsp_links() {
        assert_eq!(klang_dsp::units::db_to_gain(0.0), 1.0);
    }
}
