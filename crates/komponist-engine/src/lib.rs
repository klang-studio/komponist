//! The realtime engine of komponist.
//!
//! Transport, sequencer, mixer, metronome and the `Processor` trait run inside `Engine::process` on
//! the audio thread, which never allocates, locks or blocks. The program compiler turns a project
//! into the immutable program the engine plays. Offline render and WAV export live here too. The
//! engine knows no UI, no IPC, no plugin format and no audio backend.
//!
//! Module map (ARCHITECTURE §6):
//! - [`processor`]: the [`Processor`] trait, the [`EventProcessor`] stage of MIDI effects, events,
//!   [`ProcessContext`], [`StereoIo`].
//! - [`program`]: the compiled [`Program`] the engine plays.
//! - [`audio`]: audio clips: the [`AudioPlan`] of an audio track, disk streaming, the voice.
//! - [`compile`]: project → program, with processor-slot mapping and the routing graph.
//! - [`route`]: the compiled routing graph of a program (buses, sends, sidechains, gates).
//! - [`api`]: [`Command`], [`EngineEvent`], [`Garbage`], [`Telemetry`], [`TrackMeter`], [`DeviceMeter`],
//!   [`EngineHandle`].
//! - [`engine`]: [`Engine::new`], [`Engine::process`], [`Engine::render`].
//! - [`export`]: WAV export of a frame range ([`export::export`]): render, normalise, dither, write.
//! - [`testing`]: test processors ([`testing::TestSine`], [`testing::TestGain`]).
//! - [`workers`]: the worker pool for parallel track processing ([`Engine::set_workers`]).
//!
//! Engine internals: `transport` (position, loop, count-in), `sequencer` (active notes,
//! probability, live input), `stage` (MIDI effects and the note guard), `slots` (processor table, chains), `mixer` (fader, balance, solo,
//! delay compensation), `metronome`, `garbage` (event queue with the garbage holding list), `sys`
//! (futex, floating-point mode and `SCHED_FIFO` for the workers).
//!
//! Threads: `Engine` lives on the audio thread (or a render worker); `EngineHandle`, the compiler
//! and processor creation run on the UI thread. Everything crossing between them goes through the
//! rtrb queues of [`api`] or atomics. With workers ([`Engine::set_workers`]), the track chains of
//! a segment also run on the engine's worker threads (see [`workers`]).

pub mod api;
pub mod audio;
pub mod automation;
pub mod compile;
pub mod engine;
pub mod export;
mod garbage;
mod graph;
mod macros;
mod metronome;
mod mixer;
mod note_schedule;
pub mod processor;
pub mod program;
pub mod recording;
pub mod route;
mod sequencer;
mod slots;
mod stage;
mod sys;
pub mod tap;
pub mod testing;
mod transport;
pub mod workers;

// Unit tests run RT paths inside `assert_no_alloc`, which needs its allocator (debug builds only;
// in release builds `assert_no_alloc` is a no-op).
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub use api::{
    Command, DeviceMeter, EngineEvent, EngineHandle, Garbage, Telemetry, TrackMeter, TransportState,
};
pub use audio::{AudioPlan, ClipSource, Segment, SourceStatus};
pub use engine::{
    AudioInputBuffer, AudioInputRoutes, Engine, EngineConfig, LiveInput, MacroInput, MidiInEvent,
};
pub use processor::{
    Event, EventBuffer, EventKind, EventProcessor, EventSink, NOTE_ID_NONE, ProcessContext,
    ProcessStatus, Processor, ProcessorError, StereoIo, TransportInfo,
};
pub use program::{MidiPort, Program, SlotId};
pub use tap::SampleTap;

/// Largest number of frames per processor call. Device buffers above it are processed in chunks;
/// shared memory to the bridges is sized for it (`komponist_ipc::shm::MAX_BLOCK` must be equal).
pub const MAX_BLOCK: usize = 2048;

/// Most tracks a program holds; the engine preallocates per-track state (active notes, RNG, PDC
/// delay, gain ramp) for this many.
pub const MAX_TRACKS: usize = 128;

/// Most MIDI effects in front of one instrument; the compiler drops further ones.
pub const MAX_STAGES: usize = 8;

/// Distinct parameters (`Command::SetParam`) that can wait for one slot until its next processor
/// call; further ones are dropped and counted. A sender that diffs a whole state into parameters
/// (`EngineSync::sync_device_states`) replaces the processor instead when the diff is larger.
pub const MAX_PENDING_PARAMS: usize = 16;

/// Size of the engine's processor slot table ([`SlotId`] `0 .. MAX_SLOTS`).
pub const MAX_SLOTS: usize = 1024;

/// Most events one processor call receives; further events of that call are dropped (note-offs
/// first in line) and counted in [`Telemetry::dropped_events`].
pub const MAX_EVENTS_PER_BLOCK: usize = 1024;

/// Most commands the engine applies per block; the rest wait for the next block.
pub const MAX_COMMANDS_PER_BLOCK: usize = 64;

/// Capacity of the UI → engine command queue.
pub const COMMAND_CAPACITY: usize = 1024;

/// Ordinary capacity of the engine → UI event queue (events plus garbage).
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
