//! Realtime thread set-up shared by the engine's workers, the audio backend and the bridge RT
//! thread (#45), which do not depend on each other.
//!
//! Module map:
//! - [`policy`]: [`set_current_thread_realtime`]: `SCHED_FIFO` on Linux, the Mach time-constraint
//!   policy on macOS, MMCSS ("Pro Audio") with a `TIME_CRITICAL` fallback on Windows.
//! - [`workgroup`]: [`Workgroup`] (a device's audio workgroup, joined by threads that work to its
//!   deadline) and [`WorkInterval`] (a workgroup a thread creates and drives itself), with the
//!   [`JoinToken`] of a join. They exist on every OS so callers need no `cfg`, but only macOS can
//!   create them; elsewhere `Option<Workgroup>` is always `None`.
//! - [`topology`]: [`topology::performance_cores`] for the worker count on Apple silicon.
//! - macOS only: [`mach_time`] (the clock behind `Instant` in Mach ticks) and [`os_sync`] (wait and
//!   wake on an address with a deadline, compensating the kernel's timer leeway).
//!
//! Why macOS needs more than a priority (realtime spike #43, internal notes): on Apple silicon the
//! time-constraint policy alone leaves a lightly loaded machine running RT threads on efficiency
//! cores or down-clocked performance cores (2–16 % late blocks); every thread that works to the
//! block deadline must also be in an audio workgroup (0–0.13 %).
//!
//! RT rules: functions marked "RT" allocate nothing, take no lock and make no blocking call;
//! "non-RT" ones run before or after a realtime loop.

pub mod policy;
pub mod topology;
pub mod workgroup;

#[cfg(target_os = "macos")]
pub mod mach_time;
#[cfg(target_os = "macos")]
pub mod os_sync;

// Unit tests run the RT paths inside `assert_no_alloc`, which needs its allocator (debug builds
// only; in release builds `assert_no_alloc` is a no-op).
#[cfg(all(test, debug_assertions))]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub use policy::set_current_thread_realtime;
pub use workgroup::{JoinToken, WorkInterval, Workgroup};
