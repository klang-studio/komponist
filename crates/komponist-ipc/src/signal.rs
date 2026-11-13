//! Futex signalling on shared 32-bit words, with deadlines (ARCHITECTURE §8.1).
//!
//! Both words of the block handshake (`request_seq`, `done_seq`) live in the shared memory of one
//! bridge, so the futexes are *shared* (not `FUTEX_PRIVATE`). Waits use an absolute
//! `CLOCK_MONOTONIC` deadline (`FUTEX_WAIT_BITSET`), which is what `std::time::Instant` measures on
//! Linux, so a deadline computed once per block stays valid across spurious wake-ups.
//!
//! RT use: [`wait_while`] is the only blocking syscall the engine's audio thread may make, always
//! with a deadline; [`wake`] never blocks.

use core::sync::atomic::AtomicU32;
use std::time::Instant;

/// Outcome of [`wait_while`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitOutcome {
    /// The word no longer holds the awaited value; carries the value read.
    Changed(u32),
    /// The deadline passed while the word still held the value.
    TimedOut,
}

/// Blocks while `word == current`, until another thread or process changes it and calls
/// [`wake`], or until `deadline` (`None` = no deadline). Returns at once if the word already
/// differs. Handles spurious wake-ups and `EINTR` internally.
pub fn wait_while(word: &AtomicU32, current: u32, deadline: Option<Instant>) -> WaitOutcome {
    todo!("WP-16")
}

/// Wakes every waiter on `word` (in any process). Call after storing the new value with release
/// ordering. Returns the number of woken waiters. Never blocks.
pub fn wake(word: &AtomicU32) -> usize {
    todo!("WP-16")
}
