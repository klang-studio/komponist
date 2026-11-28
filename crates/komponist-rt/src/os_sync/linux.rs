//! Linux: a futex (`FUTEX_WAIT_BITSET` with an absolute `CLOCK_MONOTONIC` deadline, `FUTEX_WAKE`),
//! private for [`Scope::Process`], shared for [`Scope::Shared`].

use core::num::NonZeroU32;
use core::sync::atomic::AtomicU32;
use std::time::Instant;

use rustix::io::Errno;
use rustix::thread::futex;
use rustix::time::{ClockId, Timespec, clock_gettime};

use super::{Scope, Wait};

/// `FUTEX_BITSET_MATCH_ANY`: wait for any wake-up on the word.
const MATCH_ANY: NonZeroU32 = NonZeroU32::MAX;

/// An absolute `CLOCK_MONOTONIC` time.
pub(super) type Deadline = Timespec;

/// RT. `at` as an absolute `CLOCK_MONOTONIC` time (the clock `Instant` reads on Linux), never
/// earlier than `at`: `Instant` is read first, so the clock read after it adds the time between
/// the two reads. Two vDSO clock reads, no syscall, no allocation.
pub(super) fn deadline(at: Instant) -> Timespec {
    let remaining = at.saturating_duration_since(Instant::now());
    let now = clock_gettime(ClockId::Monotonic);
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

fn flags(scope: Scope) -> futex::Flags {
    match scope {
        Scope::Process => futex::Flags::PRIVATE,
        // No FUTEX_PRIVATE_FLAG: the other side may sit in another process.
        Scope::Shared => futex::Flags::empty(),
    }
}

/// RT. One `FUTEX_WAIT_BITSET` while `word == current`, until `deadline` (`None`: none).
pub(super) fn wait(
    word: &AtomicU32,
    current: u32,
    deadline: Option<Deadline>,
    scope: Scope,
) -> Wait {
    match futex::wait_bitset(word, flags(scope), current, deadline.as_ref(), MATCH_ANY) {
        Ok(()) | Err(Errno::AGAIN | Errno::INTR) => Wait::Retry,
        Err(Errno::TIMEDOUT) => Wait::TimedOut,
        // EFAULT, EINVAL, ENOSYS.
        Err(_) => Wait::Failed,
    }
}

/// RT. `FUTEX_WAKE` of up to `count` waiters; the number woken.
pub(super) fn wake(word: &AtomicU32, count: usize, scope: Scope) -> usize {
    // The kernel reads the count as a signed int: more than `i32::MAX` would wake one thread.
    let count = count.min(i32::MAX as usize) as u32;
    futex::wake(word, flags(scope), count).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn the_monotonic_deadline_matches_instant() {
        let now = clock_gettime(ClockId::Monotonic);
        let ts = deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (ts.tv_sec - now.tv_sec) as f64 + (ts.tv_nsec - now.tv_nsec) as f64 * 1e-9;
        assert!((1.499..1.6).contains(&diff), "{diff}");
        assert!((0..1_000_000_000).contains(&ts.tv_nsec));
    }
}
