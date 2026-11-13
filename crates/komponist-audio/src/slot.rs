//! Engine handoff between streams (ARCHITECTURE §7, R1).
//!
//! The stream callback closure owns an [`EngineSlot`]: the boxed [`AudioProcess`] plus the MIDI
//! receiver. The UI side keeps the matching [`SlotHandle`]. When cpal tears the stream down, the
//! closure and with it the slot are dropped on a non-RT thread; the slot's `Drop` sends the engine
//! and the receiver back through the handle. Detach (export) moves the engine out while the stream
//! keeps running; attach moves one in. Both moves go through single-slot lock-free queues, so the
//! callback never allocates, frees or blocks.

use std::sync::Arc;

use crate::midi::MidiReceiver;
use crate::process::{AudioProcess, CallbackInfo, OutputBuffer};
use crate::system::StreamStats;

/// The callback's side: owns the engine while the stream runs.
pub struct EngineSlot<P: AudioProcess> {
    _engine: core::marker::PhantomData<P>,
}

/// The UI's side of an [`EngineSlot`].
pub struct SlotHandle<P: AudioProcess> {
    _engine: core::marker::PhantomData<P>,
}

impl<P: AudioProcess> EngineSlot<P> {
    /// Non-RT. A slot holding `engine` (already prepared for the stream) and `midi`, recording
    /// callback load and xruns into `stats`.
    pub fn new(
        engine: Box<P>,
        midi: MidiReceiver,
        stats: Arc<StreamStats>,
    ) -> (EngineSlot<P>, SlotHandle<P>) {
        todo!("WP-13")
    }

    /// RT, called by the backend callback. Picks up an attached engine and serves a detach request
    /// first; then runs [`AudioProcess::process`] (with FTZ/DAZ set and the load measured) or, with
    /// no engine, writes silence.
    pub fn process(&mut self, out: &mut OutputBuffer<'_>, info: &CallbackInfo) {
        todo!("WP-13")
    }
}

impl<P: AudioProcess> SlotHandle<P> {
    /// Asks the callback to hand the engine out at its next run.
    pub fn request_detach(&self) {
        todo!("WP-13")
    }

    /// The engine, once handed out by a detach or by the slot's drop at stream teardown.
    pub fn try_take_engine(&mut self) -> Option<Box<P>> {
        todo!("WP-13")
    }

    /// The MIDI receiver, once the slot was dropped at stream teardown.
    pub fn try_take_midi(&mut self) -> Option<MidiReceiver> {
        todo!("WP-13")
    }

    /// Hands a prepared engine to the running slot; returns it if the slot already holds one or is
    /// gone.
    pub fn attach(&mut self, engine: Box<P>) -> Result<(), Box<P>> {
        todo!("WP-13")
    }

    /// Whether the slot was dropped (the stream is gone).
    pub fn is_closed(&self) -> bool {
        todo!("WP-13")
    }
}

impl<P: AudioProcess> core::fmt::Debug for EngineSlot<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineSlot").finish_non_exhaustive()
    }
}

impl<P: AudioProcess> core::fmt::Debug for SlotHandle<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SlotHandle").finish_non_exhaustive()
    }
}
