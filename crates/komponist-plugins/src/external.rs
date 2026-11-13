//! The engine-side stand-in for a plugin running in a bridge process (ARCHITECTURE §8.1).
//!
//! Implements `komponist_engine::Processor` on top of `komponist_ipc::BridgeClient`. Per call:
//! convert the events to `WireEvent`s and the transport to `WireTransport`, `begin_block`, then
//! `wait_done` with a deadline of about 60 % of the block period (at the sample rate) minus the time
//! already spent in this callback. Fallbacks (passthrough for effects, silence for instruments):
//! while the bridge loads (`Loading`), when a block is late or the bridge busy (counted as "late"),
//! and permanently once the bridge died (then `ProcessStatus::Failed`). Undelivered note-offs are
//! kept and sent with the next block that reaches the bridge.
//!
//! Created only by [`crate::PluginManager`]; lives on the audio thread; dropped on the UI thread
//! (engine garbage).

use komponist_engine::{Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo};

/// A [`Processor`] whose work happens in a bridge process.
#[derive(Debug)]
pub struct ExternalProcessor {
    _private: (),
}

impl Processor for ExternalProcessor {
    /// Non-RT (engine out of the callback, e.g. inside `Engine::set_sample_rate`). If the rate or
    /// block size changed, asks the plugin manager (through a queue it polls) to re-activate the
    /// bridge, and returns at once; until the bridge is `Active` again the processor uses the
    /// fallback.
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        todo!("WP-45")
    }

    /// RT: one shared-memory round trip with deadline, or the fallback.
    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        todo!("WP-45")
    }

    /// RT: the latency the bridge published in shared memory.
    fn latency(&self) -> u32 {
        todo!("WP-45")
    }

    /// RT: sets `BLOCK_FLAG_RESET` on the next block.
    fn reset(&mut self) {
        todo!("WP-45")
    }
}
