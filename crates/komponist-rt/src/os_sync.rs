//! Wait and wake on a 32-bit word with `os_sync_wait_on_address` (macOS 14.4+), the counterpart
//! of a Linux futex. The engine's worker pool waits on process-private words, komponist-ipc on
//! words in shared memory ([`Scope`]).
//!
//! **Timer leeway:** a timed-out `os_sync_wait_on_address_with_deadline` returns late, also on
//! time-constraint threads, by an amount that is kernel policy and changes with the machine's
//! state and the OS version. On macOS 27 (M1 Pro) it is 12.5 % of the wait up to 1 ms (spike #43,
//! measured again for #101), 25 % while nobody uses the desktop (the kernel coalesces timers more
//! while the user is idle, towards its maximum). The macOS 15 CI runner, a VM nobody uses, ended
//! 1 ms waits armed 750 µs ahead some 675 µs late (#101): a leeway that does not shrink with the
//! wait. [`wait`] therefore assumes no particular leeway:
//!
//! - A kernel wait is armed at 3/4 of the remaining time, which ends in time for any leeway up to
//!   a third of the wait, but at least the learned leeway (and a quarter of it, for jitter) before
//!   the deadline ([`plan`]); the caller waits again for the rest (an early timeout is reported as
//!   [`Wait::Retry`]).
//! - Every kernel wait that times out reports how late it ended. A lateness of more than a third
//!   of its wait is learned, per [`Scope`], as a decaying maximum in an atomic ([`learned`]);
//!   the rest let it decay, so the long idle waits of the engine's workers (1 s, up to 1 ms late)
//!   do not raise it.
//! - The last stretch, [`FINAL_POLL`] plus that margin, is not left to the kernel's timer: the
//!   waiter sleeps in slices of at most [`POLL_SLICE`] with `mach_wait_until`
//!   ([`mach_time::wait_until`], which the kernel does not delay on a time-constraint thread) and
//!   looks at the word in between.
//!
//! So a timeout is never reported early, and once the leeway is learned (from one late wait) it
//! comes within the precision of `mach_wait_until`. The cost: a wake-up in the polled stretch is
//! seen up to [`POLL_SLICE`] later (never after the deadline), and the stretch costs one short
//! sleep per slice. Where the leeway stays within a third of the wait (macOS 27 in use), nothing is
//! learned and only the last [`FINAL_POLL`] is polled; a wake-up before the polled stretch, the
//! normal path, always ends a kernel wait.

use core::ffi::c_void;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::mach_time;

/// Size of the waited-on word in bytes.
const WORD: usize = size_of::<AtomicU32>();

/// The last stretch before a deadline that is always polled (plus the margin of the learned
/// leeway): a kernel wait armed this close may end a whole leeway late, which can be far longer.
pub const FINAL_POLL: Duration = Duration::from_micros(50);

/// The longest sleep of the polled stretch: the longest a wake-up there goes unnoticed.
pub const POLL_SLICE: Duration = Duration::from_micros(25);

/// Upper bound of the learned leeway, so that a preempted waiter (which looks like a long leeway)
/// cannot make the waits poll for long. The kernel's own maximum is 1 ms on macOS 27.
pub const MAX_LEEWAY: Duration = Duration::from_millis(2);

/// Who may wake a waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    /// Threads of this process only (the word is in private memory).
    Process,
    /// Any process that maps the word (`OS_SYNC_*_SHARED`). Waiters and wakers must agree.
    Shared,
}

/// The learned leeway of each scope in Mach ticks ([`learned`]); the kernel may treat the scopes'
/// timers differently. Relaxed loads and stores: a lost update from a concurrent waiter only
/// delays the learning by one wait.
static LEEWAY: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

impl Scope {
    fn leeway(self) -> &'static AtomicU64 {
        &LEEWAY[self as usize]
    }
}

/// What one call of [`wait`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Wait {
    /// Woken (possibly spuriously), the word changed before the kernel looked, a signal ended the
    /// wait, or the armed time or a poll slice passed before the deadline: look at the word (and
    /// the clock) again.
    Retry,
    /// The deadline passed.
    TimedOut,
    /// An unexpected error (bad arguments); retrying would spin.
    Failed,
}

/// What [`wait`] does next on the way to a deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// A kernel wait on the word, armed at this absolute time (before the deadline).
    Kernel(u64),
    /// A `mach_wait_until` to this absolute time (at most the deadline), then a look at the word.
    Poll(u64),
}

/// RT. The next step at `now` towards `deadline` (after `now`) with the learned `leeway` (ticks).
/// With `margin` = the leeway and a quarter of it: a poll slice in the last [`FINAL_POLL`] +
/// `margin`, else a kernel wait armed a quarter of the remaining time or `margin` before the
/// deadline, whichever is earlier. So the wait ends in time if its leeway is at most a third of
/// its length or at most `margin`.
fn plan(now: u64, deadline: u64, leeway: u64) -> Step {
    let remaining = deadline - now;
    let margin = leeway.saturating_add(leeway / 4);
    if remaining <= margin.saturating_add(mach_time::ticks(FINAL_POLL)) {
        Step::Poll(deadline.min(now.saturating_add(mach_time::ticks(POLL_SLICE))))
    } else {
        Step::Kernel(deadline - margin.max(remaining / 4))
    }
}

/// RT. The learned leeway after a kernel wait armed `interval` ahead timed out `late` after its
/// armed time: `late` if that is more than a third of the interval (more than the 3/4 arming
/// absorbs), else nothing; or the old `leeway` less 1/16 if that is more. A decaying maximum: one
/// late wait raises it at once, a few dozen waits that end in time let it fall. At most
/// [`MAX_LEEWAY`].
fn learned(leeway: u64, interval: u64, late: u64) -> u64 {
    let observed = if late > interval / 3 { late } else { 0 };
    observed
        .max(leeway - leeway / 16)
        .min(mach_time::ticks(MAX_LEEWAY))
}

/// RT. The learned leeway after one poll slice: 1/4096 less. Without this a leeway long enough to
/// make every wait a poll from its start would never meet a kernel wait to let it decay; with it,
/// some 2800 slices (about 70 such waits of 1 ms) halve it. Where the kernel's leeway really is
/// that long, the first kernel wait after the decay ends up to [`FINAL_POLL`] late and is learned
/// again.
fn decayed(leeway: u64) -> u64 {
    leeway - leeway / 4096
}

/// RT. The learned leeway of `scope` (see the module documentation), for diagnostics.
pub fn learned_leeway(scope: Scope) -> Duration {
    let ticks = scope.leeway().load(Ordering::Relaxed);
    let per_ms = mach_time::ticks(Duration::from_millis(1)).max(1);
    Duration::from_nanos(ticks.saturating_mul(1_000_000) / per_ms)
}

/// RT. One step of a wait while `word == current`, until a wake-up or the absolute Mach
/// `deadline` (`None`: no deadline, a plain kernel wait). Returns [`Wait::TimedOut`] only once the
/// deadline has passed (at once if it already has). Blocks only in
/// `os_sync_wait_on_address[_with_deadline]` or, in the last stretch before the deadline, in one
/// `mach_wait_until` of at most [`POLL_SLICE`].
pub fn wait(word: &AtomicU32, current: u32, deadline: Option<u64>, scope: Scope) -> Wait {
    let addr = word.as_ptr().cast::<c_void>();
    let flags = wait_flags(scope);
    let Some(deadline) = deadline else {
        // SAFETY: `addr` points to a live, 4-byte aligned word for the duration of the call; the
        // kernel only reads it.
        let rc = unsafe { libc::os_sync_wait_on_address(addr, u64::from(current), WORD, flags) };
        return if rc >= 0 { Wait::Retry } else { failure() };
    };
    let now = mach_time::now();
    if now >= deadline {
        return Wait::TimedOut;
    }
    let leeway = scope.leeway();
    let armed = match plan(now, deadline, leeway.load(Ordering::Relaxed)) {
        Step::Kernel(armed) => armed,
        Step::Poll(until) => {
            leeway.store(decayed(leeway.load(Ordering::Relaxed)), Ordering::Relaxed);
            if word.load(Ordering::Acquire) != current {
                return Wait::Retry;
            }
            mach_time::wait_until(until);
            return if mach_time::now() < deadline {
                Wait::Retry
            } else {
                Wait::TimedOut
            };
        }
    };
    // SAFETY: as above. The armed time is after `now`, so never 0.
    let rc = unsafe {
        libc::os_sync_wait_on_address_with_deadline(
            addr,
            u64::from(current),
            WORD,
            flags,
            libc::OS_CLOCK_MACH_ABSOLUTE_TIME,
            armed,
        )
    };
    if rc >= 0 {
        return Wait::Retry;
    }
    // SAFETY: the calling thread's errno; reading it allocates nothing.
    if unsafe { *libc::__error() } != libc::ETIMEDOUT {
        return failure();
    }
    let end = mach_time::now();
    let late = end.saturating_sub(armed);
    leeway.store(
        learned(leeway.load(Ordering::Relaxed), armed - now, late),
        Ordering::Relaxed,
    );
    if end < deadline {
        Wait::Retry
    } else {
        Wait::TimedOut
    }
}

/// RT. What a failed kernel wait other than a timeout means, from `errno`.
fn failure() -> Wait {
    // SAFETY: the calling thread's errno; reading it allocates nothing.
    match unsafe { *libc::__error() } {
        // Interrupted or spurious, or transient kernel conditions the header says to retry (the
        // word itself is always a valid address here).
        libc::EINTR | libc::EFAULT | libc::ENOMEM => Wait::Retry,
        // EINVAL; ETIMEDOUT cannot come without a deadline.
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

fn wait_flags(scope: Scope) -> libc::os_sync_wait_on_address_flags_t {
    match scope {
        Scope::Process => libc::OS_SYNC_WAIT_ON_ADDRESS_NONE,
        Scope::Shared => libc::OS_SYNC_WAIT_ON_ADDRESS_SHARED,
    }
}

fn wake_flags(scope: Scope) -> libc::os_sync_wake_by_address_flags_t {
    // The flag must match the waiters' flag.
    match scope {
        Scope::Process => libc::OS_SYNC_WAKE_BY_ADDRESS_NONE,
        Scope::Shared => libc::OS_SYNC_WAKE_BY_ADDRESS_SHARED,
    }
}

/// Non-RT, for the leeway tests and their CI report (#101): the [`mach_time::median_overshoot`]
/// of a bare kernel wait on a word nobody changes, armed at the deadline itself and repeated only
/// if it ends early. What [`wait`] would do without any compensation; it neither reads nor
/// changes the learned leeway.
pub fn kernel_overshoot(scope: Scope) -> Duration {
    let flags = wait_flags(scope);
    let word = AtomicU32::new(0);
    mach_time::median_overshoot(|at| {
        let deadline = mach_time::deadline(at);
        while mach_time::now() < deadline {
            // SAFETY: as in `wait`; the deadline is not 0.
            unsafe {
                libc::os_sync_wait_on_address_with_deadline(
                    word.as_ptr().cast::<c_void>(),
                    0,
                    WORD,
                    flags,
                    libc::OS_CLOCK_MACH_ABSOLUTE_TIME,
                    deadline,
                )
            };
        }
    })
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

    fn us(micros: u64) -> u64 {
        mach_time::ticks(Duration::from_micros(micros))
    }

    #[test]
    fn without_a_learned_leeway_a_wait_is_armed_at_three_quarters_and_the_last_50_us_polled() {
        let now = 1_000_000;
        let long = us(1000);
        assert_eq!(
            plan(now, now + long, 0),
            Step::Kernel(now + long - long / 4)
        );
        // The final stretch: slices of POLL_SLICE, the last one ends at the deadline.
        assert_eq!(
            plan(now, now + us(50), 0),
            Step::Poll(now + mach_time::ticks(POLL_SLICE))
        );
        assert_eq!(plan(now, now + us(10), 0), Step::Poll(now + us(10)));
        assert!(matches!(plan(now, now + us(51), 0), Step::Kernel(_)));
    }

    #[test]
    fn a_learned_leeway_arms_the_kernel_wait_earlier_and_lengthens_the_polled_stretch() {
        let now = 1_000_000;
        let leeway = us(400);
        let margin = leeway + leeway / 4;
        assert!(matches!(
            plan(now, now + margin + us(50), leeway),
            Step::Poll(_)
        ));
        // Close to the deadline: armed the margin before it.
        let deadline = now + margin + us(100);
        assert_eq!(plan(now, deadline, leeway), Step::Kernel(deadline - margin));
        // Far from it: the 3/4 arming leaves more than the margin.
        let deadline = now + us(10_000);
        assert_eq!(
            plan(now, deadline, leeway),
            Step::Kernel(deadline - us(10_000) / 4)
        );
    }

    #[test]
    fn the_leeway_is_learned_at_once_and_decays_slowly() {
        // Late by up to a third of the wait: covered by the 3/4 arming, nothing to learn.
        assert_eq!(learned(0, us(750), us(250)), 0);
        // Later: learned at once.
        assert_eq!(learned(0, us(750), us(675)), us(675));
        assert_eq!(learned(us(800), us(750), us(675)), us(800) - us(800) / 16);
        // Waits that end in time let it fall by 1/16 each, a poll slice by 1/4096.
        assert_eq!(learned(1600, us(750), 0), 1500);
        assert_eq!(decayed(40960), 40950);
        // Never above MAX_LEEWAY (a preempted waiter looks like a huge leeway).
        let max = mach_time::ticks(MAX_LEEWAY);
        assert_eq!(learned(0, us(100), us(100_000)), max);
    }

    /// One wait of `length` ticks against a simulated kernel whose timed waits end
    /// `kernel(armed interval)` late and that never wakes the waiter; `mach_wait_until` is exact.
    /// The leeway is learned as in [`wait`]. Returns the overshoot and the number of kernel waits
    /// and poll slices.
    fn simulate(leeway: &mut u64, length: u64, kernel: &dyn Fn(u64) -> u64) -> (u64, u32, u32) {
        let (mut waits, mut polls) = (0, 0);
        let mut now = 1_000_000;
        let deadline = now + length;
        while now < deadline {
            match plan(now, deadline, *leeway) {
                Step::Kernel(armed) => {
                    assert!(now < armed && armed < deadline);
                    let end = armed + kernel(armed - now);
                    *leeway = learned(*leeway, armed - now, end - armed);
                    now = end;
                    waits += 1;
                }
                Step::Poll(until) => {
                    assert!(now < until && until <= deadline);
                    *leeway = decayed(*leeway);
                    now = until;
                    polls += 1;
                }
            }
        }
        (now - deadline, waits, polls)
    }

    /// Models of the kernel's leeway the compensation must handle without knowing them: a share
    /// of the wait up to 1 ms (12.5 % on macOS 27, 25 % with the user idle); that share scaled
    /// towards the 1 ms the more idle the machine is (64 % of the way gives the 424 µs overshoot
    /// of 1 ms waits the macOS 15 CI runner showed, #101); 1 ms for every wait.
    #[test]
    fn a_simulated_leeway_is_learned_from_the_first_late_wait_and_no_wait_ends_early() {
        let cap = us(1000);
        let share = move |shift: u32| move |i: u64| (i >> shift).min(cap);
        let idle = move |i: u64| {
            let base = (i >> 3).min(cap);
            base + (cap - base) * 64 / 100
        };
        let slice = mach_time::ticks(POLL_SLICE);
        type Model = Box<dyn Fn(u64) -> u64>;
        let models: [(&str, Model, bool); 4] = [
            ("12.5 %", Box::new(share(3)), false),
            ("25 %", Box::new(share(2)), false),
            ("idle-scaled", Box::new(idle), true),
            ("1 ms", Box::new(move |_| cap), true),
        ];
        let lengths = [1000, 1000, 3000, 1000, 20_000, 500, 1000];
        for (name, kernel, learns) in &models {
            let mut leeway = 0;
            let mut late_waits = 0;
            for (n, length) in lengths.into_iter().chain([1000; 100]).enumerate() {
                let (late, waits, polls) = simulate(&mut leeway, us(length), kernel);
                assert!(waits <= 4, "{name}: {waits} kernel waits");
                if !learns {
                    // Nothing to learn: on time, only the final stretch polled.
                    assert_eq!(late, 0, "{name}: wait {n}");
                    assert!(polls <= 2, "{name}: {polls} polls");
                    continue;
                }
                // After the first wait, lateness only comes from the slow decay while every
                // wait is polled (the 1 ms model): one kernel wait now and then, armed just
                // before the polled stretch, late by about that stretch.
                if n > 0 {
                    let most = us(50) + slice;
                    assert!(late < most, "{name}: wait {n} late by {late}");
                }
                late_waits += u32::from(late > 0);
                // The polled stretch is at most the margin of MAX_LEEWAY plus FINAL_POLL.
                let most = (mach_time::ticks(MAX_LEEWAY) * 5 / 4 + us(50)) / slice + 1;
                assert!(u64::from(polls) <= most, "{name}: {polls} polls");
            }
            if *learns {
                assert!(leeway > us(600), "{name}: leeway {leeway}");
                assert!(late_waits <= 6, "{name}: {late_waits} late waits");
            } else {
                assert_eq!(leeway, 0, "{name}");
            }
        }
        // A long idle wait (the engine's workers wait 1 s) times out a full 1 ms late and must
        // not raise the leeway when that is within a third of the wait.
        let mut leeway = 0;
        let (late, _, _) = simulate(&mut leeway, us(1_000_000), &share(3));
        assert_eq!((late, leeway), (0, 0));
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
    /// time-constraint thread, a 1 ms wait that times out ends at most 50 µs later than the
    /// machine's plainest timed wait does ([`mach_time::timer_baseline`], a few µs on Apple
    /// silicon; a bare kernel wait: 125–260 µs more on macOS 27, 425 µs more on the macOS 15 CI
    /// runner, #101), in both scopes (the bridges wait in the shared one). Medians of 40 waits,
    /// so a preemption on a busy machine (or the first wait, before the leeway is learned) does
    /// not decide it. Prints the numbers for the CI log (`--nocapture`). Skipped if the kernel
    /// refuses the policy, or if the machine's timers are too coarse for the check (a virtual
    /// machine, #94).
    #[test]
    fn a_timed_out_wait_ends_close_to_its_deadline() {
        for scope in [Scope::Process, Scope::Shared] {
            let overshoot = std::thread::spawn(move || {
                if let Err(e) = crate::set_current_thread_realtime(Duration::from_millis(2), 80) {
                    eprintln!("skipped: {e}");
                    return None;
                }
                let word = AtomicU32::new(0);
                mach_time::against_baseline(|| {
                    let bare = kernel_overshoot(scope);
                    let compensated = mach_time::median_overshoot(|at| {
                        let outcome = wait_until(&word, 0, mach_time::deadline(at), scope);
                        assert_eq!(outcome, Wait::TimedOut);
                    });
                    eprintln!(
                        "{scope:?}: bare kernel wait {bare:?}, compensated {compensated:?}, \
                         learned leeway {:?}",
                        learned_leeway(scope)
                    );
                    compensated
                })
            })
            .join()
            .unwrap();
            if let Some((baseline, median)) = overshoot {
                assert!(
                    median < baseline + Duration::from_micros(50),
                    "{scope:?}: median overshoot {median:?}, the machine's timers {baseline:?}"
                );
            }
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
        // Kernel waits, the learning and the polled final stretch, in both scopes.
        let outcome = assert_no_alloc::assert_no_alloc(|| {
            let mut waited = [Wait::Failed; 2];
            for (scope, waited) in [Scope::Process, Scope::Shared].into_iter().zip(&mut waited) {
                let deadline = mach_time::deadline(Instant::now() + Duration::from_millis(1));
                *waited = wait_until(&word, 4, deadline, scope);
                let _ = learned_leeway(scope);
            }
            (
                waited,
                wake_one(&word, Scope::Process),
                wake_all(&word, Scope::Shared),
            )
        });
        assert_eq!(outcome, ([Wait::TimedOut; 2], false, false));
    }
}
