//! The ring buffer between the read-ahead thread and the audio thread (ARCHITECTURE §6.9.3).
//!
//! One ring per audio track, planar stereo `f32` in atomics (so no `unsafe` and no locks), single
//! producer (the streamer) and single consumer (the audio thread). The content is a **stream** of
//! rendered track audio in playback order; its index counts from 0 at every *epoch*. The audio
//! thread starts an epoch whenever the playhead jumps (playback start from another position,
//! locate, a program that changed the track): it names the epoch in a message, the streamer starts
//! producing from the position in it and publishes `(epoch, frames written)`. Both indices carry
//! the epoch in their high half, so a word that belongs to an older epoch is never mistaken for
//! the current one.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Frames a ring holds (a power of two): the read-ahead is at most this far ahead of the playhead,
/// about 0.7 s at 48 kHz.
pub const RING_FRAMES: usize = 1 << 15;

const MASK: usize = RING_FRAMES - 1;

/// Single-producer single-consumer ring of rendered audio. See the module docs.
pub struct Ring {
    data: [Box<[AtomicU32]>; 2],
    available: Box<[AtomicBool]>,
    /// `epoch << 32 | frames written in the epoch` (the producer's).
    write: AtomicU64,
    /// `epoch << 32 | frames consumed in the epoch` (the consumer's).
    read: AtomicU64,
}

impl std::fmt::Debug for Ring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ring").finish_non_exhaustive()
    }
}

fn word(epoch: u32, index: u32) -> u64 {
    u64::from(epoch) << 32 | u64::from(index)
}

impl Ring {
    /// Non-RT. An empty ring (epoch 0: nothing is valid).
    pub fn new() -> Ring {
        let plane = || (0..RING_FRAMES).map(|_| AtomicU32::new(0)).collect();
        Ring {
            data: [plane(), plane()],
            available: (0..RING_FRAMES).map(|_| AtomicBool::new(false)).collect(),
            write: AtomicU64::new(0),
            read: AtomicU64::new(0),
        }
    }

    // --- Consumer (audio thread) ---------------------------------------------------------------

    /// RT. Frames the producer has written in `epoch`, `None` while it has not started it.
    pub fn written(&self, epoch: u32) -> Option<u32> {
        let w = self.write.load(Ordering::Acquire);
        ((w >> 32) as u32 == epoch).then_some(w as u32)
    }

    /// RT. Announces `epoch` as started with nothing consumed (before the message that asks the
    /// producer for it goes out).
    pub fn begin_read(&self, epoch: u32) {
        self.read.store(word(epoch, 0), Ordering::Release);
    }

    /// RT. Declares the first `index` frames of `epoch` consumed: the producer may reuse them.
    pub fn set_read(&self, epoch: u32, index: u32) {
        self.read.store(word(epoch, index), Ordering::Release);
    }

    /// RT. Copies the frames `from ..` of the stream into `left` and `right` (equal lengths).
    /// The caller has checked that they are written ([`Ring::written`]) and not yet released.
    pub fn copy_out(&self, from: u32, left: &mut [f32], right: &mut [f32]) {
        self.copy_out_available(from, left, right);
    }
    /// RT. Invalid worker frames are silence and return false, preserving underrun telemetry.
    pub(crate) fn copy_out_available(
        &self,
        from: u32,
        left: &mut [f32],
        right: &mut [f32],
    ) -> bool {
        let start = from as usize;
        let mut valid = true;
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let at = (start + i) & MASK;
            let available = self.available[at].load(Ordering::Relaxed);
            valid &= available;
            *l = if available {
                f32::from_bits(self.data[0][at].load(Ordering::Relaxed))
            } else {
                0.0
            };
            *r = if available {
                f32::from_bits(self.data[1][at].load(Ordering::Relaxed))
            } else {
                0.0
            };
        }
        valid
    }

    // --- Producer (streamer) -------------------------------------------------------------------

    /// Starts `epoch`: nothing written yet.
    pub(crate) fn begin_write(&self, epoch: u32) {
        self.write.store(word(epoch, 0), Ordering::Release);
    }

    /// Frames that fit before the consumer's released position of `epoch`.
    pub(crate) fn free(&self, epoch: u32) -> usize {
        let w = self.write.load(Ordering::Relaxed);
        if (w >> 32) as u32 != epoch {
            return 0;
        }
        let r = self.read.load(Ordering::Acquire);
        // Before the consumer announced the epoch, nothing is released.
        let released = if (r >> 32) as u32 == epoch {
            r as u32
        } else {
            0
        };
        // A consumer ahead of the producer (it played what was not there yet) has nothing queued.
        let queued = ((w as u32).wrapping_sub(released) as i32).max(0) as usize;
        RING_FRAMES.saturating_sub(queued)
    }

    /// Appends `left` and `right` (equal lengths, at most [`Ring::free`]) to `epoch`.
    pub(crate) fn push(&self, epoch: u32, left: &[f32], right: &[f32]) {
        self.push_available(epoch, left, right, true);
    }
    pub(crate) fn push_available(&self, epoch: u32, left: &[f32], right: &[f32], available: bool) {
        let w = self.write.load(Ordering::Relaxed);
        debug_assert_eq!((w >> 32) as u32, epoch);
        let start = w as u32;
        for (i, (l, r)) in left.iter().zip(right).enumerate() {
            let at = (start as usize).wrapping_add(i) & MASK;
            self.available[at].store(available, Ordering::Relaxed);
            self.data[0][at].store(l.to_bits(), Ordering::Relaxed);
            self.data[1][at].store(r.to_bits(), Ordering::Relaxed);
        }
        let end = start.wrapping_add(left.len() as u32);
        self.write.store(word(epoch, end), Ordering::Release);
    }
}

impl Default for Ring {
    fn default() -> Ring {
        Ring::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_do_not_mix_and_free_space_follows_the_consumer() {
        let ring = Ring::new();
        assert_eq!(ring.written(1), None);
        ring.begin_read(1);
        ring.begin_write(1);
        assert_eq!(ring.written(1), Some(0));
        assert_eq!(ring.free(1), RING_FRAMES);
        let l: Vec<f32> = (0..100).map(|i| i as f32).collect();
        ring.push(1, &l, &l);
        assert_eq!(ring.written(1), Some(100));
        assert_eq!(ring.free(1), RING_FRAMES - 100);
        let (mut a, mut b) = ([0.0; 10], [0.0; 10]);
        ring.copy_out(90, &mut a, &mut b);
        assert_eq!(a[0], 90.0);
        assert_eq!(b[9], 99.0);
        ring.set_read(1, 100);
        assert_eq!(ring.free(1), RING_FRAMES);
        // A new epoch invalidates the old words.
        ring.begin_read(2);
        assert_eq!(ring.written(1), Some(100));
        ring.begin_write(2);
        assert_eq!(ring.written(1), None);
        assert_eq!(ring.written(2), Some(0));
    }

    #[test]
    fn wraps_around_the_end() {
        let ring = Ring::new();
        ring.begin_read(1);
        ring.begin_write(1);
        let block = vec![0.5f32; RING_FRAMES - 10];
        ring.push(1, &block, &block);
        ring.set_read(1, (RING_FRAMES - 10) as u32);
        let x: Vec<f32> = (0..20).map(|i| i as f32).collect();
        ring.push(1, &x, &x);
        let (mut a, mut b) = ([0.0; 20], [0.0; 20]);
        ring.copy_out((RING_FRAMES - 10) as u32, &mut a, &mut b);
        assert_eq!(a.to_vec(), x);
        assert_eq!(b.to_vec(), x);
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    #[test]
    fn unavailable_frames_are_zero_and_counted_through_wrap_and_new_epochs() {
        let ring = Ring::new();
        ring.begin_read(1);
        ring.begin_write(1);
        let block = vec![0.25; RING_FRAMES - 3];
        ring.push(1, &block, &block);
        ring.set_read(1, (RING_FRAMES - 3) as u32);
        ring.push_available(1, &[9.0; 7], &[9.0; 7], false);
        ring.push(1, &[0.5; 4], &[0.5; 4]);
        let (mut l, mut r) = ([1.0; 11], [1.0; 11]);
        assert!(!ring.copy_out_available((RING_FRAMES - 3) as u32, &mut l, &mut r));
        assert_eq!(&l[..7], &[0.0; 7]);
        assert_eq!(&l[7..], &[0.5; 4]);
        assert_eq!(l, r);
        ring.begin_read(2);
        ring.begin_write(2);
        ring.push(2, &[0.75; 11], &[0.75; 11]);
        assert!(ring.copy_out_available(0, &mut l, &mut r));
        assert_eq!(l, [0.75; 11]);
    }
}
