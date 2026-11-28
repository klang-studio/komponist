//! Control-thread core for synchronizing a project document and its devices to the audio engine.

pub mod device_host;
pub mod engine_sync;
pub mod project_io;
mod session;

pub use session::Session;

#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;
