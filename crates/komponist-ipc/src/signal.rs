//! Futex signalling on shared 32-bit words, with deadlines (ARCHITECTURE §8.1).
//!
//! Both words of the block handshake (`request_seq`, `done_seq`) live in the shared memory of one
//! bridge, so the futexes are *shared* (not `FUTEX_PRIVATE`). Waits use an absolute
//! `CLOCK_MONOTONIC` deadline (`FUTEX_WAIT_BITSET`), which is what `std::time::Instant` measures on
//! Linux, so a deadline computed once per block stays valid across spurious wake-ups.
//!
//! RT use: [`wait_while`] is the only blocking syscall the engine's audio thread may make, always
//! with a deadline; [`wake`] never blocks.

use core::num::NonZeroU32;
use core::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use rustix::io::Errno;
use rustix::thread::futex;
use rustix::time::{ClockId, Timespec, clock_gettime};

/// `FUTEX_BITSET_MATCH_ANY`: wait for any wake-up on the word.
const MATCH_ANY: NonZeroU32 = NonZeroU32::MAX;

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
    let value = word.load(Ordering::Acquire);
    if value != current {
        return WaitOutcome::Changed(value);
    }
    // Translate the deadline once into an absolute CLOCK_MONOTONIC time; FUTEX_WAIT_BITSET takes
    // it as is, so retries after spurious wake-ups or EINTR keep the same deadline.
    let timeout = deadline.map(monotonic_deadline);
    loop {
        match futex::wait_bitset(
            word,
            futex::Flags::empty(),
            current,
            timeout.as_ref(),
            MATCH_ANY,
        ) {
            // Woken (possibly spuriously), the word changed before the kernel looked (EAGAIN), or a
            // signal interrupted the wait: look at the word again.
            Ok(()) | Err(Errno::AGAIN) | Err(Errno::INTR) => {}
            Err(Errno::TIMEDOUT) => {
                let value = word.load(Ordering::Acquire);
                return if value != current {
                    WaitOutcome::Changed(value)
                } else {
                    WaitOutcome::TimedOut
                };
            }
            // Not expected for an aligned word and a valid timeout (EFAULT, EINVAL, ENOSYS). Do not
            // spin: with a deadline report a timeout, without one fall back to yielding.
            Err(_) => {
                let value = word.load(Ordering::Acquire);
                if value != current {
                    return WaitOutcome::Changed(value);
                }
                if deadline.is_some() {
                    return WaitOutcome::TimedOut;
                }
                std::thread::yield_now();
            }
        }
        let value = word.load(Ordering::Acquire);
        if value != current {
            return WaitOutcome::Changed(value);
        }
    }
}

/// Wakes every waiter on `word` (in any process). Call after storing the new value with release
/// ordering. Returns the number of woken waiters. Never blocks.
pub fn wake(word: &AtomicU32) -> usize {
    // A shared futex (no FUTEX_PRIVATE_FLAG): the waiter may sit in another process.
    futex::wake(word, futex::Flags::empty(), i32::MAX as u32).unwrap_or(0)
}

/// `deadline` as an absolute `CLOCK_MONOTONIC` time (the clock `Instant` reads on Linux). Two vDSO
/// clock reads, no syscall, no allocation.
fn monotonic_deadline(deadline: Instant) -> Timespec {
    let now = clock_gettime(ClockId::Monotonic);
    let remaining = deadline.saturating_duration_since(Instant::now());
    let mut sec = now.tv_sec.saturating_add(remaining.as_secs() as i64);
    let mut nsec = now.tv_nsec + i64::from(remaining.subsec_nanos());
    if nsec >= 1_000_000_000 {
        nsec -= 1_000_000_000;
        sec = sec.saturating_add(1);
    }
    Timespec {
        tv_sec: sec,
        tv_nsec: nsec,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn returns_at_once_if_the_word_differs() {
        let word = AtomicU32::new(5);
        assert_eq!(wait_while(&word, 4, None), WaitOutcome::Changed(5));
        assert_eq!(
            wait_while(&word, 4, Some(Instant::now() - Duration::from_millis(1))),
            WaitOutcome::Changed(5)
        );
    }

    #[test]
    fn past_deadline_times_out_at_once() {
        let word = AtomicU32::new(1);
        let t0 = Instant::now();
        assert_eq!(wait_while(&word, 1, Some(t0)), WaitOutcome::TimedOut);
        assert!(t0.elapsed() < Duration::from_millis(5));
    }

    #[test]
    fn times_out_at_the_deadline() {
        let word = AtomicU32::new(1);
        let t0 = Instant::now();
        let deadline = t0 + Duration::from_millis(20);
        assert_eq!(wait_while(&word, 1, Some(deadline)), WaitOutcome::TimedOut);
        let now = Instant::now();
        assert!(now >= deadline, "woke {:?} early", deadline - now);
        assert!(now - deadline < Duration::from_millis(50), "woke too late");
    }

    #[test]
    fn wake_from_another_thread() {
        let word = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            other.store(7, Ordering::Release);
            wake(&other)
        });
        let outcome = wait_while(&word, 0, Some(Instant::now() + Duration::from_secs(5)));
        assert_eq!(outcome, WaitOutcome::Changed(7));
        assert!(waker.join().unwrap() <= 1);
    }

    #[test]
    fn wait_without_deadline_returns_on_change() {
        let word = Arc::new(AtomicU32::new(3));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            other.store(4, Ordering::Release);
            wake(&other);
        });
        assert_eq!(wait_while(&word, 3, None), WaitOutcome::Changed(4));
        waker.join().unwrap();
    }

    #[test]
    fn wake_without_waiters_wakes_none() {
        let word = AtomicU32::new(0);
        assert_eq!(wake(&word), 0);
    }

    #[test]
    fn monotonic_deadline_matches_instant() {
        let now = clock_gettime(ClockId::Monotonic);
        let ts = monotonic_deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (ts.tv_sec - now.tv_sec) as f64 + (ts.tv_nsec - now.tv_nsec) as f64 * 1e-9;
        assert!((1.499..1.6).contains(&diff), "{diff}");
        assert!((0..1_000_000_000).contains(&ts.tv_nsec));
    }
}
