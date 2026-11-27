//! Realtime audio and MIDI processing contracts shared by hosts and devices.

mod meter;
pub mod processor;
pub mod tap;

pub use meter::DeviceMeter;
pub use processor::{
    Event, EventBuffer, EventKind, EventProcessor, EventSink, NOTE_ID_NONE, ProcessContext,
    ProcessStatus, Processor, ProcessorError, StereoIo, TransportInfo,
};
pub use tap::SampleTap;

/// Maximum number of events per engine processor call. The engine prioritizes note-offs
/// when events exceed the available capacity.
pub const MAX_EVENTS_PER_BLOCK: usize = 1024;

#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
