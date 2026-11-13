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

    /// Compile-time check of `N`, evaluated wherever `push` or `pop` is instantiated.
    const CHECK: () = assert!(
        N.is_power_of_two() && N <= 1 << 31,
        "ShmRing: N must be a power of two, at most 2^31"
    );

    /// Appends `item`; returns it back if the ring is full. RT-safe.
    ///
    /// # Safety
    ///
    /// The caller must be the only producer of this ring across all processes and threads, for the
    /// whole time the ring is in use.
    pub unsafe fn push(&self, item: T) -> Result<(), T> {
        const { Self::CHECK };
        // Only this producer writes `tail`.
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        // `>=` rather than `==`: a peer that corrupted `head` must not make us overwrite slots
        // the consumer may still read.
        if tail.wrapping_sub(head) as usize >= N {
            return Err(item);
        }
        let slot = self.slots[tail as usize & (N - 1)].get();
        // SAFETY: the slot lies outside the consumer's window `head .. tail` (checked above), and
        // the caller is the only producer, so nobody else accesses it until `tail` is published.
        unsafe { slot.write(item) };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Removes the oldest item, if any. RT-safe.
    ///
    /// # Safety
    ///
    /// The caller must be the only consumer of this ring across all processes and threads, for the
    /// whole time the ring is in use.
    pub unsafe fn pop(&self) -> Option<T> {
        const { Self::CHECK };
        // Only this consumer writes `head`.
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let len = tail.wrapping_sub(head) as usize;
        // A fill level above `N` can only come from a corrupted peer: treat it as empty rather than
        // read slots the producer may be writing.
        if len == 0 || len > N {
            return None;
        }
        let slot = self.slots[head as usize & (N - 1)].get();
        // SAFETY: the producer published this slot with the release store of `tail` (acquired
        // above) and does not touch it again until `head` moves past it; the caller is the only
        // consumer. `T` is `Copy`, so reading leaves nothing to drop.
        let item = unsafe { slot.read() };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Some(item)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A zeroed ring on the heap, as in a fresh memfd.
    fn zeroed<T: Copy, const N: usize>() -> Box<ShmRing<T, N>> {
        let layout = std::alloc::Layout::new::<ShmRing<T, N>>();
        // SAFETY: all-zero is a valid empty ring for the POD `T`s used here; the layout is that of
        // the boxed type.
        unsafe {
            let p = std::alloc::alloc_zeroed(layout) as *mut ShmRing<T, N>;
            assert!(!p.is_null());
            Box::from_raw(p)
        }
    }

    #[test]
    fn push_pop_in_order_until_full() {
        let ring = zeroed::<u32, 8>();
        assert!(ring.is_empty());
        // SAFETY: single thread, one producer and one consumer.
        unsafe {
            for i in 0..8 {
                assert_eq!(ring.push(i), Ok(()));
            }
            assert_eq!(ring.push(99), Err(99));
            assert_eq!(ring.len(), 8);
            for i in 0..8 {
                assert_eq!(ring.pop(), Some(i));
            }
            assert_eq!(ring.pop(), None);
        }
        assert!(ring.is_empty());
    }

    #[test]
    fn counters_wrap_around_u32() {
        let ring = zeroed::<u64, 4>();
        let start = u32::MAX - 5;
        ring.head.store(start, Ordering::Relaxed);
        ring.tail.store(start, Ordering::Relaxed);
        // SAFETY: single thread.
        unsafe {
            for round in 0..10u64 {
                for i in 0..4 {
                    assert_eq!(ring.push(round * 10 + i), Ok(()));
                }
                assert_eq!(ring.push(0), Err(0));
                assert_eq!(ring.len(), 4);
                for i in 0..4 {
                    assert_eq!(ring.pop(), Some(round * 10 + i));
                }
                assert_eq!(ring.pop(), None);
            }
        }
        assert!(
            ring.tail.load(Ordering::Relaxed) < start,
            "the counter wrapped"
        );
    }

    #[test]
    fn corrupted_indices_stay_in_bounds() {
        let ring = zeroed::<u32, 4>();
        // A peer claims 1000 items: the consumer refuses, the producer sees a full ring.
        ring.tail.store(1000, Ordering::Relaxed);
        // SAFETY: single thread.
        unsafe {
            assert_eq!(ring.pop(), None);
            ring.tail.store(0, Ordering::Relaxed);
            ring.head.store(1000, Ordering::Relaxed);
            assert_eq!(ring.push(1), Err(1));
        }
    }

    #[test]
    fn spsc_across_threads_without_allocation() {
        const ITEMS: u64 = 200_000;
        let ring: Arc<ShmRing<u64, 64>> = Arc::from(zeroed::<u64, 64>());
        let producer_ring = Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            assert_no_alloc::assert_no_alloc(|| {
                let mut next = 0;
                while next < ITEMS {
                    // SAFETY: the only producer.
                    if unsafe { producer_ring.push(next) }.is_ok() {
                        next += 1;
                    } else {
                        std::hint::spin_loop();
                    }
                }
            })
        });
        // No assertions inside `assert_no_alloc`: a failing one would allocate its message.
        let (sum, out_of_order) = assert_no_alloc::assert_no_alloc(|| {
            let mut expected = 0;
            let mut sum = 0u64;
            let mut out_of_order = 0u64;
            while expected < ITEMS {
                // SAFETY: the only consumer.
                match unsafe { ring.pop() } {
                    Some(v) => {
                        out_of_order += u64::from(v != expected);
                        sum += v;
                        expected += 1;
                    }
                    None => std::hint::spin_loop(),
                }
            }
            (sum, out_of_order)
        });
        producer.join().unwrap();
        assert_eq!(out_of_order, 0);
        assert_eq!(sum, ITEMS * (ITEMS - 1) / 2);
        assert!(ring.is_empty());
    }
}
