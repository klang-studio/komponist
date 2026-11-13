//! Lock-free single-producer single-consumer rings that live in shared memory.
//!
//! The ring is a `#[repr(C)]` struct placed inside the memfd mapping ([`crate::shm::ShmLayout`]),
//! so producer and consumer may sit in different processes. Indices are free-running `u32`
//! counters (`tail − head` = fill level, wrapping arithmetic); the producer publishes a slot with a
//! release store of `tail`, the consumer frees it with a release store of `head`. No allocation, no
//! syscalls: RT-safe on both sides.
//!
//! A freshly created memfd is zero-filled, which is a valid empty ring for any `T` whose all-zero
//! bit pattern is valid (true for the POD types of [`crate::shm`]).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

/// An SPSC ring of `N` slots of `T` (`N` a power of two, at most 2^31) for shared memory.
///
/// The roles are not enforced by the type, because both ends see the same mapping. Exactly one
/// thread (in any process) may call [`ShmRing::push`] and exactly one may call [`ShmRing::pop`];
/// `BridgeClient` and `BridgeServer` (in [`crate::shm`]) uphold this by owning one role each.
#[repr(C)]
pub struct ShmRing<T: Copy, const N: usize> {
    /// Free-running count of popped items; written by the consumer.
    head: AtomicU32,
    /// Free-running count of pushed items; written by the producer.
    tail: AtomicU32,
    /// Item storage, index = counter % N.
    slots: [UnsafeCell<T>; N],
}

// SAFETY: the ring is designed for concurrent use by one producer and one consumer (possibly in
// other processes); slots are handed over with acquire/release on `head` and `tail`, and the
// callers of the unsafe `push`/`pop` promise the single-producer / single-consumer discipline.
unsafe impl<T: Copy + Send, const N: usize> Sync for ShmRing<T, N> {}

impl<T: Copy, const N: usize> ShmRing<T, N> {
    /// Number of slots.
    pub const CAPACITY: usize = N;

    /// Appends `item`; returns it back if the ring is full. RT-safe.
    ///
    /// # Safety
    ///
    /// The caller must be the only producer of this ring across all processes and threads, for the
    /// whole time the ring is in use.
    pub unsafe fn push(&self, item: T) -> Result<(), T> {
        todo!("WP-16")
    }

    /// Removes the oldest item, if any. RT-safe.
    ///
    /// # Safety
    ///
    /// The caller must be the only consumer of this ring across all processes and threads, for the
    /// whole time the ring is in use.
    pub unsafe fn pop(&self) -> Option<T> {
        todo!("WP-16")
    }

    /// Items currently in the ring (a snapshot; may be stale by the time it is used).
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head) as usize
    }

    /// Whether the ring is empty (snapshot).
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
