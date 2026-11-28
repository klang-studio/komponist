//! Wait and wake on a 32-bit word with a deadline, on every OS: the one wait the engine's worker
//! pool and the block handshake of komponist-ipc use on their realtime threads (#243).
//!
//! [`wait_while`] blocks while the word holds a value, until another thread (or, with
//! `Scope::Shared`, process) changes it and calls [`wake`], or until a deadline. The deadline is
//! an [`Instant`], turned once into the kernel's clock, so spurious wake-ups and retries keep it;
//! a timeout is never reported before it. Per OS:
//!
//! - Linux: a futex, `FUTEX_WAIT_BITSET` with an absolute `CLOCK_MONOTONIC` deadline (the clock
//!   `Instant` reads), `FUTEX_PRIVATE_FLAG` for [`Scope::Process`]; `FUTEX_WAKE`.
//! - macOS (14.4+): `os_sync_wait_on_address_with_deadline` / `os_sync_wake_by_address_*` with the
//!   `SHARED` flags for `Scope::Shared`, armed short of the deadline against the kernel's timer
//!   leeway, the last stretch polled in `mach_wait_until` slices of at most 25 µs (`mach`, spike
//!   #43, #101).
//! - Windows: `WaitOnAddress` with the remaining time rounded up to whole milliseconds, which ends
//!   on the system clock tick (up to 15.6 ms late at the default timer resolution; spike
//!   windows-wake S1, internal notes), and `WakeByAddressSingle` / `WakeByAddressAll`. There is no
//!   cross-process address wait on Windows, so `Scope::Shared` does not exist there; the block
//!   handshake uses events and a high-resolution timer instead (komponist-ipc `signal`). The
//!   engine's workers accept the coarse timeout: their deadline only counts a late job.
//!
//! RT: [`wait_while`] with a deadline and [`wake`] allocate nothing and take no lock; the only
//! blocking calls are the kernel wait (and on macOS the `mach_wait_until` slices). Without a
//! deadline a wait may block forever, so RT threads always pass one.

use core::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as sys;

#[cfg(target_os = "macos")]
pub mod mach;
#[cfg(target_os = "macos")]
use mach as sys;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows as sys;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("komponist-rt: no wait/wake backend (os_sync) for this OS");

/// [`wake`]'s count for every waiter.
pub const WAKE_ALL: usize = usize::MAX;

/// Who may wake a waiter. Waiters and wakers of a word must agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Threads of this process only (the word is in private memory).
    Process,
    /// Any process that maps the word (Linux: a shared futex; macOS: `OS_SYNC_*_SHARED`).
    #[cfg(unix)]
    Shared,
}

/// Outcome of [`wait_while`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WaitOutcome {
    /// The word no longer holds the awaited value; carries the value read.
    Changed(u32),
    /// The deadline passed while the word still held the value.
    TimedOut,
}

/// What one kernel wait of a `sys` module returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Wait {
    /// Woken (possibly spuriously), the word changed before the kernel looked, a signal or a
    /// transient condition ended the wait, or (macOS, Windows) a wait armed before the deadline
    /// ended early: look at the word and the clock again.
    Retry,
    /// The deadline passed (never reported before it).
    TimedOut,
    /// An unexpected error (bad alignment or arguments); retrying would spin.
    Failed,
}

/// RT (with a deadline). Blocks while `word == current`, until another thread or process
/// changes it and calls [`wake`], or until `deadline` (`None`: no deadline). Returns at once if the
/// word already differs; otherwise [`WaitOutcome::TimedOut`] only once the deadline has passed.
/// Spurious wake-ups, `EINTR` and early timer expiries are handled inside. An unexpected kernel
/// error is not retried with a deadline (reported as a timeout); without one the thread yields and
/// waits again.
pub fn wait_while(
    word: &AtomicU32,
    current: u32,
    deadline: Option<Instant>,
    scope: Scope,
) -> WaitOutcome {
    let changed = || {
        let value = word.load(Ordering::Acquire);
        (value != current).then_some(value)
    };
    if let Some(value) = changed() {
        return WaitOutcome::Changed(value);
    }
    // Once, so retries keep the same deadline.
    let kernel_deadline = deadline.map(sys::deadline);
    loop {
        let step = sys::wait(word, current, kernel_deadline, scope);
        if let Some(value) = changed() {
            return WaitOutcome::Changed(value);
        }
        match step {
            Wait::Retry => {}
            Wait::TimedOut => return WaitOutcome::TimedOut,
            Wait::Failed if deadline.is_some() => return WaitOutcome::TimedOut,
            Wait::Failed => std::thread::yield_now(),
        }
    }
}

/// RT. Wakes up to `count` waiters on `word` ([`WAKE_ALL`]: every one). Call after storing the new
/// value with release ordering. Never blocks.
///
/// Returns how many waiters it woke as far as the OS tells: on Linux the exact number; on macOS,
/// which wakes one or all, the number of single wakes that found a waiter (with [`WAKE_ALL`]: 1 if
/// it woke any); on Windows, which reports nothing, always 0.
pub fn wake(word: &AtomicU32, count: usize, scope: Scope) -> usize {
    if count == 0 {
        return 0;
    }
    sys::wake(word, count, scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[cfg(unix)]
    const SCOPES: [Scope; 2] = [Scope::Process, Scope::Shared];
    #[cfg(windows)]
    const SCOPES: [Scope; 1] = [Scope::Process];

    /// Spawns `n` threads that wait on `word` (still 0) for up to ten seconds; each returns the
    /// outcome and how long it waited.
    fn waiters(
        word: &Arc<AtomicU32>,
        n: usize,
        scope: Scope,
    ) -> Vec<std::thread::JoinHandle<(WaitOutcome, Duration)>> {
        (0..n)
            .map(|_| {
                let word = Arc::clone(word);
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    let deadline = t0 + Duration::from_secs(10);
                    (wait_while(&word, 0, Some(deadline), scope), t0.elapsed())
                })
            })
            .collect()
    }

    #[test]
    fn returns_at_once_if_the_word_differs() {
        let word = AtomicU32::new(5);
        for scope in SCOPES {
            assert_eq!(wait_while(&word, 4, None, scope), WaitOutcome::Changed(5));
            let past = Instant::now() - Duration::from_millis(1);
            assert_eq!(
                wait_while(&word, 4, Some(past), scope),
                WaitOutcome::Changed(5)
            );
        }
    }

    #[test]
    fn a_past_deadline_times_out_at_once() {
        let word = AtomicU32::new(1);
        for scope in SCOPES {
            let t0 = Instant::now();
            assert_eq!(wait_while(&word, 1, Some(t0), scope), WaitOutcome::TimedOut);
            assert!(t0.elapsed() < Duration::from_millis(5));
        }
    }

    /// Never before the deadline. The upper bound is loose: on Windows a millisecond timeout ends
    /// on the coarse clock tick (up to ~16 ms more, spike S1), and the machine may be busy.
    #[test]
    fn times_out_at_the_deadline_never_before() {
        let word = AtomicU32::new(1);
        for scope in SCOPES {
            for ms in [1, 3, 20] {
                let t0 = Instant::now();
                let deadline = t0 + Duration::from_millis(ms);
                assert_eq!(
                    wait_while(&word, 1, Some(deadline), scope),
                    WaitOutcome::TimedOut
                );
                let now = Instant::now();
                assert!(
                    now >= deadline,
                    "{scope:?}: woke {:?} early",
                    deadline - now
                );
                assert!(
                    now - deadline < Duration::from_millis(100),
                    "{scope:?}: a {ms} ms wait took {:?}",
                    now - t0
                );
            }
        }
    }

    #[test]
    fn a_wake_from_another_thread_ends_the_wait_with_and_without_a_deadline() {
        for scope in SCOPES {
            for deadline in [Some(Duration::from_secs(5)), None] {
                let word = Arc::new(AtomicU32::new(0));
                let other = Arc::clone(&word);
                let waker = std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(10));
                    other.store(7, Ordering::Release);
                    wake(&other, 1, scope)
                });
                let deadline = deadline.map(|d| Instant::now() + d);
                assert_eq!(
                    wait_while(&word, 0, deadline, scope),
                    WaitOutcome::Changed(7),
                    "{scope:?}"
                );
                assert!(waker.join().unwrap() <= 1);
            }
        }
    }

    #[test]
    fn wake_all_wakes_every_waiter() {
        for scope in SCOPES {
            let word = Arc::new(AtomicU32::new(0));
            let waiting = waiters(&word, 4, scope);
            std::thread::sleep(Duration::from_millis(20));
            word.store(1, Ordering::Release);
            wake(&word, WAKE_ALL, scope);
            for waiter in waiting {
                let (outcome, waited) = waiter.join().unwrap();
                assert_eq!(outcome, WaitOutcome::Changed(1), "{scope:?}");
                assert!(waited < Duration::from_secs(5), "{scope:?}: {waited:?}");
            }
        }
    }

    /// Two of four waiters woken: (at least) two return before the others' ten-second deadline.
    #[test]
    fn a_wake_count_wakes_that_many() {
        for scope in SCOPES {
            let word = Arc::new(AtomicU32::new(0));
            let waiting = waiters(&word, 4, scope);
            std::thread::sleep(Duration::from_millis(50));
            word.store(1, Ordering::Release);
            let woken = wake(&word, 2, scope);
            assert!(woken <= 2, "{scope:?}: {woken}");
            let t0 = Instant::now();
            while waiting.iter().filter(|w| w.is_finished()).count() < 2 {
                assert!(t0.elapsed() < Duration::from_secs(5), "fewer than two woke");
                std::thread::sleep(Duration::from_millis(1));
            }
            // The rest, so the test ends.
            wake(&word, WAKE_ALL, scope);
            for waiter in waiting {
                assert_eq!(waiter.join().unwrap().0, WaitOutcome::Changed(1));
            }
        }
    }

    #[test]
    fn a_wake_without_waiters_wakes_none() {
        let word = AtomicU32::new(0);
        for scope in SCOPES {
            assert_eq!(wake(&word, 1, scope), 0);
            assert_eq!(wake(&word, WAKE_ALL, scope), 0);
            assert_eq!(wake(&word, 0, scope), 0);
        }
    }

    #[test]
    fn waits_and_wakes_allocate_nothing() {
        for scope in SCOPES {
            let word = Arc::new(AtomicU32::new(0));
            let other = Arc::clone(&word);
            let waker = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(5));
                assert_no_alloc::assert_no_alloc(|| {
                    other.store(1, Ordering::Release);
                    wake(&other, WAKE_ALL, scope)
                })
            });
            let later = Instant::now() + Duration::from_secs(5);
            let outcome = assert_no_alloc::assert_no_alloc(|| {
                let differs = wait_while(&AtomicU32::new(3), 7, Some(Instant::now()), scope);
                let soon = Instant::now() + Duration::from_millis(1);
                let timed_out = wait_while(&AtomicU32::new(2), 2, Some(soon), scope);
                let nobody = (wake(&AtomicU32::new(0), 1, scope), wake(&word, 3, scope));
                (
                    differs,
                    timed_out,
                    nobody,
                    wait_while(&word, 0, Some(later), scope),
                )
            });
            assert_eq!(
                outcome,
                (
                    WaitOutcome::Changed(3),
                    WaitOutcome::TimedOut,
                    (0, 0),
                    WaitOutcome::Changed(1)
                ),
                "{scope:?}"
            );
            waker.join().unwrap();
        }
    }
}
