//! A lock-free sample tap: the audio thread copies samples of a signal into a fixed ring, the UI
//! thread reads the latest ones (WP-92).
//!
//! It is the one-way path from a processor to a display that needs raw samples (the EQ's live
//! spectrum; a spectrum analyzer device or the sampler's waveform can use it the same way), next
//! to the peak values of [`crate::DeviceMeter`].
//!
//! - **Single producer, single consumer.** The processor calls [`SampleTap::begin`] once per block
//!   and, if it returned `true`, [`SampleTap::push`] or [`SampleTap::push_mono`]; the UI calls
//!   [`SampleTap::want`] every frame in which it shows the signal and [`SampleTap::read_latest`]
//!   to fetch a window.
//! - **RT-safe.** The ring is allocated by [`SampleTap::new`] (UI thread, when the processor is
//!   built). The audio side only does relaxed and release atomic stores and one `compare_exchange`:
//!   no allocation, no lock, no syscall, no loop that waits for the UI.
//! - **Costs nothing while nobody looks.** The UI's interest is a *lease*: [`SampleTap::want`]
//!   stores a number of frames, [`SampleTap::begin`] counts it down by the frames of every block
//!   and returns `false` once it ran out. A hidden device therefore stops copying about half a
//!   second after its frame left the screen, without any message to the audio thread.
//! - **Consistent reads without a lock.** A seqlock on the write position: the producer announces
//!   how far it is about to write (`claimed`) before it writes and publishes how far it did write
//!   (`written`) after; [`SampleTap::read_latest`] copies a window and accepts it only if the
//!   producer did not come around the ring onto the oldest sample it copied.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// Capacity [`SampleTap::new`] callers use by default: 16384 samples (64 KiB), a window of 8192
/// plus the largest block with room to spare.
pub const DEFAULT_CAPACITY: usize = 16_384;

/// Seconds of interest one [`SampleTap::want`] call keeps the tap running for (counted in audio
/// frames, so it also stops counting while the audio is stopped).
pub const LEASE_SECONDS: f32 = 0.5;

/// The tap. Shared as `Arc<SampleTap>` between the processor and the UI.
#[derive(Debug)]
pub struct SampleTap {
    /// `f32` bit patterns; written by the audio thread, read by the UI thread.
    ring: Box<[AtomicU32]>,
    mask: u64,
    /// Samples the producer has fully written (total, never wraps in practice).
    written: AtomicU64,
    /// Samples the producer has started to write.
    claimed: AtomicU64,
    /// Frames of interest left (see the module docs).
    wanted: AtomicU32,
    /// Sample rate of the tapped signal as `f32` bits (0 = not set).
    rate: AtomicU32,
}

impl SampleTap {
    /// A tap with room for `capacity` samples (rounded up to a power of two, at least 2). Allocates.
    pub fn new(capacity: usize) -> SampleTap {
        let capacity = capacity.max(2).next_power_of_two();
        SampleTap {
            ring: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
            mask: capacity as u64 - 1,
            written: AtomicU64::new(0),
            claimed: AtomicU64::new(0),
            wanted: AtomicU32::new(0),
            rate: AtomicU32::new(0),
        }
    }

    /// Samples the ring holds.
    pub fn capacity(&self) -> usize {
        self.ring.len()
    }

    // --- Audio thread -------------------------------------------------------------------------

    /// RT. Starts a block of `frames` frames: `true` if the UI is interested, in which case the
    /// block's samples should be pushed. Counts the interest down by `frames`.
    pub fn begin(&self, frames: usize) -> bool {
        let left = self.wanted.load(Ordering::Relaxed);
        if left == 0 {
            return false;
        }
        let used = u32::try_from(frames).unwrap_or(u32::MAX);
        // If the UI refreshed the lease meanwhile the exchange fails, and the fresh value stays.
        let _ = self.wanted.compare_exchange(
            left,
            left.saturating_sub(used),
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
        true
    }

    /// RT. Appends `samples`.
    pub fn push(&self, samples: &[f32]) {
        self.write(samples.len(), |i| samples[i]);
    }

    /// RT. Appends the mono mix `(left + right) / 2` of two channels (the shorter one decides the
    /// length).
    pub fn push_mono(&self, left: &[f32], right: &[f32]) {
        self.write(left.len().min(right.len()), |i| 0.5 * (left[i] + right[i]));
    }

    fn write(&self, n: usize, sample: impl Fn(usize) -> f32) {
        // Only this thread changes `written`, so a relaxed load sees the latest value.
        let start = self.written.load(Ordering::Relaxed);
        self.claimed.store(start + n as u64, Ordering::Relaxed);
        // The claim is visible before any sample store below (the reader's validation relies on it).
        fence(Ordering::Release);
        for i in 0..n {
            let at = ((start + i as u64) & self.mask) as usize;
            self.ring[at].store(sample(i).to_bits(), Ordering::Relaxed);
        }
        self.written.store(start + n as u64, Ordering::Release);
    }

    /// Sets the sample rate of the tapped signal, so the UI can map bins to Hz. Not RT-critical
    /// (call it when the processor activates).
    pub fn set_rate(&self, rate: f32) {
        self.rate.store(rate.to_bits(), Ordering::Relaxed);
    }

    // --- UI thread ----------------------------------------------------------------------------

    /// The sample rate set with [`SampleTap::set_rate`] (0 before).
    pub fn rate(&self) -> f32 {
        f32::from_bits(self.rate.load(Ordering::Relaxed))
    }

    /// UI. Says the signal is shown now: keeps the tap running for [`LEASE_SECONDS`] of audio.
    /// Call it every frame the display is visible. A tap whose rate is not set runs for 0.5 s at
    /// 48 kHz.
    pub fn want(&self) {
        let rate = match self.rate() {
            r if r > 0.0 => r,
            _ => 48_000.0,
        };
        self.wanted
            .store((rate * LEASE_SECONDS) as u32, Ordering::Relaxed);
    }

    /// UI. Whether the producer currently has interest left (for tests and diagnostics).
    pub fn is_wanted(&self) -> bool {
        self.wanted.load(Ordering::Relaxed) > 0
    }

    /// UI. Total samples written so far; a reader that sees the same number twice knows that no
    /// new samples came in between (the audio is stopped or the tap is off).
    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// UI. Copies the latest `out.len()` samples (oldest first) into `out` and returns the total
    /// written count the window ends at. `None` if fewer samples were written so far, if `out` is
    /// longer than half the ring, or if the producer kept overwriting the window (retried a few
    /// times; the caller keeps its previous result).
    pub fn read_latest(&self, out: &mut [f32]) -> Option<u64> {
        let n = out.len() as u64;
        if n > self.ring.len() as u64 / 2 {
            return None;
        }
        let cap = self.ring.len() as u64;
        for _ in 0..4 {
            let end = self.written.load(Ordering::Acquire);
            if end < n {
                return None;
            }
            let start = end - n;
            for (i, slot) in out.iter_mut().enumerate() {
                let at = ((start + i as u64) & self.mask) as usize;
                *slot = f32::from_bits(self.ring[at].load(Ordering::Relaxed));
            }
            fence(Ordering::Acquire);
            // The oldest sample is intact unless the producer claimed a whole ring beyond it.
            if self.claimed.load(Ordering::Relaxed) - start <= cap {
                return Some(end);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_no_alloc::assert_no_alloc;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn nothing_runs_without_interest_and_interest_runs_out() {
        let tap = SampleTap::new(1024);
        tap.set_rate(1000.0);
        assert!(!tap.begin(100), "no interest yet");
        assert_eq!(tap.written(), 0);
        tap.want();
        assert!(tap.is_wanted());
        // 0.5 s at 1 kHz = 500 frames: the blocks that start before it ran out are recorded.
        let mut blocks = 0;
        while tap.begin(100) {
            tap.push(&[0.0; 100]);
            blocks += 1;
            assert!(blocks < 100);
        }
        assert_eq!(blocks, 5);
        assert!(!tap.is_wanted());
        tap.want();
        assert!(tap.begin(10), "a refresh starts it again");
    }

    #[test]
    fn the_audio_side_does_not_allocate() {
        let tap = SampleTap::new(256);
        tap.want();
        let (l, r) = ([0.5_f32; 100], [-0.25_f32; 100]);
        let mut out = [0.0_f32; 64];
        assert_no_alloc(|| {
            for _ in 0..10 {
                if tap.begin(l.len()) {
                    tap.push_mono(&l, &r);
                    tap.push(&l);
                }
            }
            tap.read_latest(&mut out);
        });
        assert_eq!(out[0], 0.5);
    }

    #[test]
    fn it_returns_the_latest_samples_across_the_wrap() {
        let tap = SampleTap::new(8);
        let mut out = [0.0_f32; 4];
        assert_eq!(tap.read_latest(&mut out), None, "not enough yet");
        // 13 samples through a ring of 8: the window wraps several times.
        let samples: Vec<f32> = (0..13).map(|i| i as f32).collect();
        for chunk in samples.chunks(3) {
            tap.push(chunk);
        }
        assert_eq!(tap.read_latest(&mut out), Some(13));
        assert_eq!(out, [9.0, 10.0, 11.0, 12.0]);
        assert_eq!(
            tap.read_latest(&mut [0.0; 5]),
            None,
            "more than half the ring"
        );
    }

    #[test]
    fn mono_is_the_mean_of_both_channels() {
        let tap = SampleTap::new(16);
        tap.push_mono(&[1.0, 0.0, 0.5], &[0.0, 1.0, 0.5]);
        let mut out = [0.0_f32; 3];
        tap.read_latest(&mut out).unwrap();
        assert_eq!(out, [0.5, 0.5, 0.5]);
    }

    /// A producer writes a ramp of ever-growing integers in blocks of varying size through a
    /// small ring while the reader fetches windows: every accepted window must be consecutive.
    #[test]
    fn concurrent_reads_are_never_torn() {
        let tap = Arc::new(SampleTap::new(256));
        let stop = Arc::new(AtomicBool::new(false));
        let producer = {
            let (tap, stop) = (Arc::clone(&tap), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut next = 0_u32;
                let mut block = Vec::new();
                let mut size = 1;
                while !stop.load(Ordering::Relaxed) {
                    size = size % 97 + 1;
                    block.clear();
                    block.extend((0..size).map(|i| (next + i) as f32));
                    next += size;
                    tap.push(&block);
                }
                next
            })
        };
        let (mut accepted, mut rejected) = (0_u32, 0_u32);
        let mut window = [0.0_f32; 100];
        let started = std::time::Instant::now();
        let mut last_end = 0;
        while started.elapsed() < std::time::Duration::from_millis(400) {
            match tap.read_latest(&mut window) {
                Some(end) => {
                    accepted += 1;
                    assert!(end >= last_end);
                    last_end = end;
                    for (i, pair) in window.windows(2).enumerate() {
                        assert_eq!(pair[1], pair[0] + 1.0, "torn window at {i}: {window:?}");
                    }
                    assert_eq!(window[99], (end - 1) as f32);
                }
                None => rejected += 1,
            }
        }
        stop.store(true, Ordering::Relaxed);
        let total = producer.join().unwrap();
        assert!(total > 1000, "the producer ran");
        assert!(accepted > 0, "rejected {rejected} windows, accepted none");
    }
    /// Timing, not a check: `cargo test --release -p komponist-engine --lib tap::tests::cost
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing: run in release with --ignored --nocapture"]
    fn cost_of_pushing_a_block() {
        let tap = SampleTap::new(DEFAULT_CAPACITY);
        tap.want();
        let (l, r) = (vec![0.1_f32; 512], vec![0.2_f32; 512]);
        let runs = 100_000;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            tap.want();
            if tap.begin(512) {
                tap.push_mono(&l, &r);
            }
        }
        println!(
            "begin + push_mono of 512 frames: {:?}",
            started.elapsed() / runs
        );
    }
}
