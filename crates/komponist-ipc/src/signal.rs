//! Cross-process wait and wake on shared 32-bit words, with deadlines (ARCHITECTURE §8.1).
//!
//! Both words of the block handshake (`request_seq`, `done_seq`) live in the shared memory of one
//! bridge, so the waits are *shared* (they find waiters in other processes). A deadline is turned
//! once into an absolute time of the clock `std::time::Instant` measures, so a deadline computed
//! once per block stays valid across spurious wake-ups. Per OS:
//!
//! - Linux: a shared futex, `FUTEX_WAIT_BITSET` with an absolute `CLOCK_MONOTONIC` deadline
//!   (what `Instant` reads on Linux).
//! - macOS (14.4+): `os_sync_wait_on_address_with_deadline` / `os_sync_wake_by_address_all` with
//!   the `SHARED` flags and an absolute `OS_CLOCK_MACH_ABSOLUTE_TIME` deadline, through
//!   `komponist_rt::os_sync` (#45). `Instant` reads `CLOCK_UPTIME_RAW`, which is
//!   `mach_absolute_time` in nanoseconds, so the deadline is the current `mach_absolute_time` plus
//!   the remaining time in mach ticks (rounded up). The kernel ends timed waits 12.5–25 % of the
//!   wait late (timer leeway, spike #43), so each kernel wait is armed at 3/4 of the remaining
//!   time and repeated for the rest, the last one (under 50 µs) at the deadline.
//!
//! RT use: [`wait_while`] is the only blocking call the engine's audio thread may make, always
//! with a deadline; [`wake`] never blocks. Neither allocates or takes a lock.

use core::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

/// Outcome of [`wait_while`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitOutcome {
    /// The word no longer holds the awaited value; carries the value read.
    Changed(u32),
    /// The deadline passed while the word still held the value.
    TimedOut,
}

/// What one kernel wait returned, independent of the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wait {
    /// Woken (possibly spuriously), the word changed before the kernel looked, or a signal or a
    /// transient condition ended the wait early: look at the word again.
    Retry,
    /// The deadline passed.
    TimedOut,
    /// An unexpected error (bad alignment or arguments); retrying would spin.
    Failed,
}

/// Blocks while `word == current`, until another thread or process changes it and calls
/// [`wake`], or until `deadline` (`None` = no deadline). Returns at once if the word already
/// differs. Handles spurious wake-ups and `EINTR` internally.
pub fn wait_while(word: &AtomicU32, current: u32, deadline: Option<Instant>) -> WaitOutcome {
    let value = word.load(Ordering::Acquire);
    if value != current {
        return WaitOutcome::Changed(value);
    }
    // Translate the deadline once into an absolute time of the kernel's clock, so retries after
    // spurious wake-ups or EINTR keep the same deadline.
    let timeout = deadline.map(sys::absolute_deadline);
    loop {
        match sys::wait(word, current, timeout) {
            Wait::Retry => {}
            Wait::TimedOut => {
                let value = word.load(Ordering::Acquire);
                return if value != current {
                    WaitOutcome::Changed(value)
                } else {
                    WaitOutcome::TimedOut
                };
            }
            // Not expected for an aligned word and a valid deadline. Do not spin: with a deadline
            // report a timeout, without one fall back to yielding.
            Wait::Failed => {
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
/// ordering. Never blocks.
///
/// Returns the number of woken waiters on Linux; on macOS, where the kernel does not report a
/// count, 1 if it woke at least one waiter and 0 if there was none.
pub fn wake(word: &AtomicU32) -> usize {
    sys::wake(word)
}

#[cfg(target_os = "linux")]
mod sys {
    use core::num::NonZeroU32;
    use core::sync::atomic::AtomicU32;
    use std::time::Instant;

    use rustix::io::Errno;
    use rustix::thread::futex;
    use rustix::time::{ClockId, Timespec, clock_gettime};

    use super::Wait;

    /// `FUTEX_BITSET_MATCH_ANY`: wait for any wake-up on the word.
    const MATCH_ANY: NonZeroU32 = NonZeroU32::MAX;

    /// An absolute `CLOCK_MONOTONIC` time.
    pub(super) type Deadline = Timespec;

    /// `deadline` as an absolute `CLOCK_MONOTONIC` time (the clock `Instant` reads on Linux). Two
    /// vDSO clock reads, no syscall, no allocation.
    pub(super) fn absolute_deadline(deadline: Instant) -> Timespec {
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

    /// One `FUTEX_WAIT_BITSET` on a shared futex (no `FUTEX_PRIVATE_FLAG`).
    pub(super) fn wait(word: &AtomicU32, current: u32, deadline: Option<Deadline>) -> Wait {
        match futex::wait_bitset(
            word,
            futex::Flags::empty(),
            current,
            deadline.as_ref(),
            MATCH_ANY,
        ) {
            Ok(()) | Err(Errno::AGAIN) | Err(Errno::INTR) => Wait::Retry,
            Err(Errno::TIMEDOUT) => Wait::TimedOut,
            // EFAULT, EINVAL, ENOSYS.
            Err(_) => Wait::Failed,
        }
    }

    pub(super) fn wake(word: &AtomicU32) -> usize {
        // A shared futex (no FUTEX_PRIVATE_FLAG): the waiter may sit in another process.
        futex::wake(word, futex::Flags::empty(), i32::MAX as u32).unwrap_or(0)
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use core::sync::atomic::AtomicU32;
    use std::time::Instant;

    use komponist_rt::mach_time;
    use komponist_rt::os_sync::{self, Scope};

    use super::Wait;

    /// An absolute `mach_absolute_time` value (ticks).
    pub(super) type Deadline = u64;

    /// `deadline` as an absolute `mach_absolute_time` (the clock behind `Instant`), never earlier
    /// than `deadline`.
    pub(super) fn absolute_deadline(deadline: Instant) -> u64 {
        mach_time::deadline(deadline)
    }

    /// One `os_sync_wait_on_address[_with_deadline]` with the `SHARED` flag (the word may sit in
    /// shared memory and be woken from another process), armed short of the deadline to make up
    /// for the kernel's timer leeway ([`os_sync`]).
    pub(super) fn wait(word: &AtomicU32, current: u32, deadline: Option<Deadline>) -> Wait {
        match os_sync::wait(word, current, deadline, Scope::Shared) {
            os_sync::Wait::Retry => Wait::Retry,
            os_sync::Wait::TimedOut => Wait::TimedOut,
            os_sync::Wait::Failed => Wait::Failed,
        }
    }

    pub(super) fn wake(word: &AtomicU32) -> usize {
        usize::from(os_sync::wake_all(word, Scope::Shared))
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
    #[cfg(target_os = "linux")]
    fn monotonic_deadline_matches_instant() {
        use rustix::time::{ClockId, clock_gettime};
        let now = clock_gettime(ClockId::Monotonic);
        let ts = sys::absolute_deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (ts.tv_sec - now.tv_sec) as f64 + (ts.tv_nsec - now.tv_nsec) as f64 * 1e-9;
        assert!((1.499..1.6).contains(&diff), "{diff}");
        assert!((0..1_000_000_000).contains(&ts.tv_nsec));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn mach_deadline_matches_instant() {
        use komponist_rt::mach_time;
        let second = mach_time::ticks(Duration::from_secs(1)) as f64;
        let now = mach_time::now();
        let deadline = sys::absolute_deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (deadline - now) as f64 / second;
        assert!((1.499..1.6).contains(&diff), "{diff}");
    }

    /// The kernel's timer leeway is compensated (spike #43, Q4): on a time-constraint thread a
    /// 1 ms wait that times out ends less than 60 µs after its deadline (uncompensated: 125–260
    /// µs). The median of 40 waits; skipped if the kernel refuses the policy.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_timed_out_wait_ends_close_to_its_deadline() {
        let median = std::thread::spawn(|| {
            let period = Duration::from_millis(2);
            if let Err(e) = komponist_rt::set_current_thread_realtime(period, 80) {
                eprintln!("skipped: {e}");
                return None;
            }
            let word = AtomicU32::new(0);
            let mut late: Vec<Duration> = (0..40)
                .map(|_| {
                    let deadline = Instant::now() + Duration::from_millis(1);
                    assert_eq!(wait_while(&word, 0, Some(deadline)), WaitOutcome::TimedOut);
                    Instant::now().duration_since(deadline)
                })
                .collect();
            late.sort();
            Some(late[late.len() / 2])
        })
        .join()
        .unwrap();
        if let Some(median) = median {
            assert!(
                median < Duration::from_micros(60),
                "median overshoot {median:?}"
            );
        }
    }

    #[test]
    fn waits_and_wakes_allocate_nothing() {
        let word = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            other.store(1, Ordering::Release);
            wake(&other);
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let outcome = assert_no_alloc::assert_no_alloc(|| {
            let differs = wait_while(&AtomicU32::new(3), 7, Some(Instant::now()));
            let soon = Instant::now() + Duration::from_millis(1);
            let late = wait_while(&AtomicU32::new(2), 2, Some(soon));
            (differs, late, wait_while(&word, 0, Some(deadline)))
        });
        assert_eq!(
            outcome,
            (
                WaitOutcome::Changed(3),
                WaitOutcome::TimedOut,
                WaitOutcome::Changed(1)
            )
        );
        waker.join().unwrap();
    }
}
