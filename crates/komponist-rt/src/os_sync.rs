//! Wait and wake on a 32-bit word with `os_sync_wait_on_address` (macOS 14.4+), the counterpart
//! of a Linux futex. The engine's worker pool waits on process-private words, komponist-ipc on
//! words in shared memory ([`Scope`]).
//!
//! **Timer leeway:** a timed-out `os_sync_wait_on_address_with_deadline` returns a share of the
//! wait late, also on time-constraint threads, and the share varies: 12.5 % in spike #43 (Q4:
//! 103 µs late for 800 µs), 25 % on the same Mac a few hours later with nobody at the desktop
//! (257 µs for 1 ms; the kernel coalesces timers more while the user is idle). A fixed
//! compensation therefore fails. [`wait`] arms the kernel wait at 3/4 of the remaining time
//! ([`armed`]), which ends before the deadline for any leeway up to 33 %, and the caller waits
//! again for the rest (a wait that times out early is reported as [`Wait::Retry`]); once less
//! than [`FINAL_WAIT`] is left it arms the deadline itself. So a timeout is never reported early,
//! and it comes at most a quarter of [`FINAL_WAIT`] late, after two or three kernel waits for a
//! 1 ms wait.

use core::ffi::c_void;
use core::sync::atomic::AtomicU32;
use std::time::Duration;

use crate::mach_time;

/// Size of the waited-on word in bytes.
const WORD: usize = size_of::<AtomicU32>();

/// Below this remaining time a wait is armed at its deadline (and ends up to a quarter of it late).
pub const FINAL_WAIT: Duration = Duration::from_micros(50);

/// Who may wake a waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Threads of this process only (the word is in private memory).
    Process,
    /// Any process that maps the word (`OS_SYNC_*_SHARED`). Waiters and wakers must agree.
    Shared,
}

/// What one kernel wait returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wait {
    /// Woken (possibly spuriously), the word changed before the kernel looked, a signal ended the
    /// wait, or the armed time passed before the deadline: look at the word (and the clock) again.
    Retry,
    /// The deadline passed.
    TimedOut,
    /// An unexpected error (bad arguments); retrying would spin.
    Failed,
}

/// RT. The absolute time to arm a kernel wait at: `now` + 3/4 of the time to `deadline`, so that
/// the kernel's leeway (12.5–25 % of the wait) ends it before the deadline; the deadline itself
/// when less than [`FINAL_WAIT`] is left or it has passed.
pub fn armed(now: u64, deadline: u64) -> u64 {
    let remaining = deadline.saturating_sub(now);
    if remaining <= mach_time::ticks(FINAL_WAIT) {
        deadline
    } else {
        deadline - remaining / 4
    }
}

/// RT. One wait while `word == current`, until a wake-up or the absolute Mach `deadline` (`None`:
/// no deadline). Returns [`Wait::TimedOut`] only once the deadline has passed (at once if it
/// already has). The only blocking call is `os_sync_wait_on_address[_with_deadline]`.
pub fn wait(word: &AtomicU32, current: u32, deadline: Option<u64>, scope: Scope) -> Wait {
    let addr = word.as_ptr().cast::<c_void>();
    let flags = match scope {
        Scope::Process => libc::OS_SYNC_WAIT_ON_ADDRESS_NONE,
        Scope::Shared => libc::OS_SYNC_WAIT_ON_ADDRESS_SHARED,
    };
    let rc = match deadline {
        Some(deadline) => {
            let now = mach_time::now();
            if now >= deadline {
                return Wait::TimedOut;
            }
            // SAFETY: `addr` points to a live, 4-byte aligned word for the duration of the call;
            // the kernel only reads it. The armed time is after `now`, so never 0.
            unsafe {
                libc::os_sync_wait_on_address_with_deadline(
                    addr,
                    u64::from(current),
                    WORD,
                    flags,
                    libc::OS_CLOCK_MACH_ABSOLUTE_TIME,
                    armed(now, deadline),
                )
            }
        }
        // SAFETY: as above.
        None => unsafe { libc::os_sync_wait_on_address(addr, u64::from(current), WORD, flags) },
    };
    if rc >= 0 {
        return Wait::Retry;
    }
    // SAFETY: the calling thread's errno; reading it allocates nothing.
    match unsafe { *libc::__error() } {
        libc::ETIMEDOUT => match deadline {
            Some(deadline) if mach_time::now() < deadline => Wait::Retry,
            _ => Wait::TimedOut,
        },
        // Interrupted or spurious, or transient kernel conditions the header says to retry (the
        // word itself is always a valid address here).
        libc::EINTR | libc::EFAULT | libc::ENOMEM => Wait::Retry,
        // EINVAL.
        _ => Wait::Failed,
    }
}

/// RT. Wakes one waiter on `word`. `true` if there was one. Never blocks.
pub fn wake_one(word: &AtomicU32, scope: Scope) -> bool {
    let addr = word.as_ptr().cast::<c_void>();
    // SAFETY: as in `wait`.
    let rc = unsafe { libc::os_sync_wake_by_address_any(addr, WORD, wake_flags(scope)) };
    // 0: woke a waiter; -1 with ENOENT: nobody waited.
    rc == 0
}

/// RT. Wakes every waiter on `word`. `true` if there was at least one. Never blocks.
pub fn wake_all(word: &AtomicU32, scope: Scope) -> bool {
    let addr = word.as_ptr().cast::<c_void>();
    // SAFETY: as in `wait`.
    let rc = unsafe { libc::os_sync_wake_by_address_all(addr, WORD, wake_flags(scope)) };
    rc == 0
}

fn wake_flags(scope: Scope) -> libc::os_sync_wake_by_address_flags_t {
    // The flag must match the waiters' flag.
    match scope {
        Scope::Process => libc::OS_SYNC_WAKE_BY_ADDRESS_NONE,
        Scope::Shared => libc::OS_SYNC_WAKE_BY_ADDRESS_SHARED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Instant;

    /// Waits until `deadline` the way the callers do: retry until changed or timed out.
    fn wait_until(word: &AtomicU32, current: u32, deadline: u64, scope: Scope) -> Wait {
        loop {
            if word.load(Ordering::Acquire) != current {
                return Wait::Retry;
            }
            match wait(word, current, Some(deadline), scope) {
                Wait::Retry => {}
                other => return other,
            }
        }
    }

    #[test]
    fn armed_time_is_three_quarters_of_the_rest() {
        let final_wait = mach_time::ticks(FINAL_WAIT);
        let long = 100 * final_wait;
        assert_eq!(armed(1000, 1000 + long), 1000 + long * 3 / 4);
        // Little left: the deadline itself.
        assert_eq!(armed(1000, 1000 + final_wait), 1000 + final_wait);
        assert_eq!(armed(500, 400), 400);
    }

    #[test]
    fn times_out_at_the_deadline_never_before() {
        let word = AtomicU32::new(1);
        for scope in [Scope::Process, Scope::Shared] {
            let t0 = Instant::now();
            let deadline = t0 + Duration::from_millis(5);
            assert_eq!(
                wait_until(&word, 1, mach_time::deadline(deadline), scope),
                Wait::TimedOut
            );
            let now = Instant::now();
            assert!(now >= deadline, "woke {:?} early", deadline - now);
            assert!(now - deadline < Duration::from_millis(50), "woke too late");
        }
        // A past deadline times out at once, without a kernel wait.
        assert_eq!(
            wait(&word, 1, Some(mach_time::now()), Scope::Process),
            Wait::TimedOut
        );
    }

    /// The leeway compensation, guarded (the leeway is undocumented kernel policy): on a
    /// time-constraint thread, a 1 ms wait that times out ends less than 60 µs after its deadline
    /// (uncompensated: 125–260 µs). The median of 40 waits, so a preemption on a busy machine does
    /// not decide it. Skipped if the kernel refuses the policy.
    #[test]
    fn a_timed_out_wait_ends_close_to_its_deadline() {
        let overshoot = std::thread::spawn(|| {
            if let Err(e) = crate::set_current_thread_realtime(Duration::from_millis(2), 80) {
                eprintln!("skipped: {e}");
                return None;
            }
            let word = AtomicU32::new(0);
            let mut late: Vec<Duration> = (0..40)
                .map(|_| {
                    let deadline = Instant::now() + Duration::from_millis(1);
                    let outcome =
                        wait_until(&word, 0, mach_time::deadline(deadline), Scope::Process);
                    assert_eq!(outcome, Wait::TimedOut);
                    Instant::now().duration_since(deadline)
                })
                .collect();
            late.sort();
            Some(late[late.len() / 2])
        })
        .join()
        .unwrap();
        if let Some(median) = overshoot {
            assert!(
                median < Duration::from_micros(60),
                "median overshoot {median:?}"
            );
        }
    }

    #[test]
    fn wake_one_and_wake_all() {
        for scope in [Scope::Process, Scope::Shared] {
            let word = Arc::new(AtomicU32::new(0));
            assert!(!wake_one(&word, scope), "nobody waits");
            let waiters: Vec<_> = (0..3)
                .map(|_| {
                    let word = Arc::clone(&word);
                    std::thread::spawn(move || {
                        let deadline = mach_time::deadline(Instant::now() + Duration::from_secs(5));
                        wait_until(&word, 0, deadline, scope)
                    })
                })
                .collect();
            std::thread::sleep(Duration::from_millis(20));
            word.store(1, Ordering::Release);
            assert!(wake_one(&word, scope) | wake_all(&word, scope));
            for waiter in waiters {
                assert_eq!(waiter.join().unwrap(), Wait::Retry);
            }
        }
    }

    #[test]
    fn waits_and_wakes_allocate_nothing() {
        let word = AtomicU32::new(4);
        let outcome = assert_no_alloc::assert_no_alloc(|| {
            let deadline = mach_time::deadline(Instant::now() + Duration::from_millis(1));
            let waited = wait_until(&word, 4, deadline, Scope::Process);
            (
                waited,
                wake_one(&word, Scope::Process),
                wake_all(&word, Scope::Shared),
            )
        });
        assert_eq!(outcome, (Wait::TimedOut, false, false));
    }
}
