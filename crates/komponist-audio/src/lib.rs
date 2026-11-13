//! Audio and MIDI I/O of komponist.
//!
//! cpal backends (PipeWire, JACK, ALSA) and a null backend for tests and "Kein Audio-Gerät", the
//! stream lifecycle with an engine handoff that survives a stream rebuild, midir MIDI inputs with
//! timestamps, and CPU load and xrun statistics.
//!
//! This crate does not depend on `komponist-engine`. It drives any [`AudioProcess`]; the app
//! implements that trait for a wrapper around the engine.
//!
//! Module map (ARCHITECTURE §7):
//! - [`process`]: the [`AudioProcess`] callback trait, [`OutputBuffer`], [`CallbackInfo`],
//!   [`MidiIn`].
//! - [`system`]: [`AudioSystem`], [`HostKind`], [`DeviceInfo`], [`StreamConfig`], [`AudioState`],
//!   [`StreamStats`].
//! - [`slot`]: [`EngineSlot`], the engine handoff between streams.
//! - [`midi`]: [`MidiInputHub`], [`MidiStamped`], [`MidiPortId`].
//!
//! Threads: `AudioSystem` and `MidiInputHub` live on the UI thread; `EngineSlot::process` and
//! `AudioProcess::process` run on the audio thread; midir callbacks on MIDI threads.

// Contract stage (WP-01): bodies are `todo!()` until WP-13 implements them, so parameters are
// unused for now (and boxed engines are not yet moved anywhere). Remove these allows when the
// implementation lands.
#![allow(unused_variables, clippy::boxed_local)]

pub mod midi;
pub mod process;
pub mod slot;
pub mod system;

pub use midi::{MidiError, MidiInputHub, MidiPortId, MidiPortInfo, MidiReceiver, MidiStamped};
pub use process::{AudioProcess, CallbackInfo, MidiIn, OutputBuffer, StreamParams};
pub use slot::{EngineSlot, SlotHandle};
pub use system::{
    AudioError, AudioState, AudioSystem, DeviceInfo, HostKind, StreamConfig, StreamStats,
};

/// RT-safe. Sets flush-to-zero and denormals-are-zero for the calling thread (MXCSR on x86_64,
/// FPCR on aarch64, no-op elsewhere). The audio callback calls it before running the engine
/// (ARCHITECTURE §6.1).
pub fn enable_flush_to_zero() {
    todo!("WP-13")
}
