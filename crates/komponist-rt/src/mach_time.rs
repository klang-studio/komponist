//! The clock behind `std::time::Instant` on macOS, in Mach ticks (`mach_absolute_time`).
//!
//! `Instant` reads `CLOCK_UPTIME_RAW`, which is `mach_absolute_time` converted to nanoseconds
//! (verified in spike #43), so an `Instant` deadline becomes a Mach deadline by adding the
//! remaining time in ticks to the current tick count. Mach ticks are the clock of
//! `os_sync_wait_on_address_with_deadline` (`OS_CLOCK_MACH_ABSOLUTE_TIME`), of the time-constraint
//! policy and of workgroup intervals. All functions are RT: two commpage reads, no syscall, no
//! allocation.

use std::time::{Duration, Instant};

/// `struct mach_timebase_info` (`<mach/mach_time.h>`): nanoseconds per tick as a fraction.
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

// Declared here: libc marks its Mach bindings deprecated in favour of the `mach2` crate, which
// would be a dependency for two functions.
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    fn mach_wait_until(deadline: u64) -> i32;
}

/// RT. The current `mach_absolute_time` (ticks since boot, not counting sleep).
pub fn now() -> u64 {
    // SAFETY: no arguments, no side effects; reads the commpage.
    unsafe { mach_absolute_time() }
}

/// RT. `duration` in Mach ticks, rounded up (so a wait never ends before its deadline).
pub fn ticks(duration: Duration) -> u64 {
    let mut timebase = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: writes the two fields of a valid struct. Reads the commpage: no syscall, no lock, no
    // allocation (std's own deadline conversion calls it the same way).
    unsafe { mach_timebase_info(&mut timebase) };
    if timebase.numer == 0 || timebase.denom == 0 {
        // Never seen; 1:1 is the timebase of Intel Macs.
        timebase = MachTimebaseInfo { numer: 1, denom: 1 };
    }
    let ticks =
        (duration.as_nanos() * u128::from(timebase.denom)).div_ceil(u128::from(timebase.numer));
    u64::try_from(ticks).unwrap_or(u64::MAX)
}

/// RT. Sleeps until the absolute tick count `deadline` (returns at once if it has passed) with
/// `mach_wait_until`, a timed wait the kernel barely delays on a time-constraint thread (macOS 27,
/// M1 Pro: 1–16 µs late for 25 µs–100 ms, where an `os_sync` wait ends 12.5 % of the wait, up to
/// 1 ms, late). Nothing wakes it early, so [`crate::os_sync`] uses it only in short slices.
pub fn wait_until(deadline: u64) {
    // SAFETY: a plain blocking wait until an absolute tick count; no memory is touched.
    unsafe { mach_wait_until(deadline) };
}

/// RT. `deadline` as an absolute `mach_absolute_time`. `Instant::now()` is read before
/// `mach_absolute_time()`, so the result is never earlier than `deadline`; a past deadline gives
/// the current tick count.
pub fn deadline(deadline: Instant) -> u64 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    now().saturating_add(ticks(remaining))
}

/// Non-RT, for the leeway tests (here and in `komponist-ipc`). The median of 40 overshoots of
/// `wait` past a deadline 1 ms ahead; `wait` gets the deadline and must not return before it.
pub fn median_overshoot(mut wait: impl FnMut(Instant)) -> Duration {
    let mut late: Vec<Duration> = (0..40)
        .map(|_| {
            let deadline = Instant::now() + Duration::from_millis(1);
            wait(deadline);
            Instant::now().saturating_duration_since(deadline)
        })
        .collect();
    late.sort();
    late[late.len() / 2]
}

/// Non-RT, for the leeway tests. [`median_overshoot`] of `mach_wait_until`, the plainest timed
/// wait, on the calling thread: how precise this machine's timers are at best. On Apple silicon a
/// time-constraint thread gets a few microseconds; a virtual machine's timers can be far coarser
/// (the GitHub macOS runner, #94), and a leeway check means nothing there.
pub fn timer_baseline() -> Duration {
    median_overshoot(|at| wait_until(deadline(at)))
}

/// Above this [`timer_baseline`] the leeway compensation cannot be told from the timers' own
/// imprecision.
pub const COARSE_TIMERS: Duration = Duration::from_micros(100);

/// Non-RT, for the leeway tests: `(baseline, measure())` with the [`timer_baseline`] taken first on
/// the calling thread, or `None` (and a note on stderr) when the timers are too coarse.
pub fn against_baseline(measure: impl FnOnce() -> Duration) -> Option<(Duration, Duration)> {
    let baseline = timer_baseline();
    if baseline > COARSE_TIMERS {
        eprintln!(
            "skipped: timers on this machine are too coarse to check the leeway compensation \
             (mach_wait_until overshoots {baseline:?})"
        );
        return None;
    }
    let measured = measure();
    eprintln!("median overshoot {measured:?}, mach_wait_until {baseline:?}");
    Some((baseline, measured))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_matches_instant() {
        // One second in ticks, converted back: the timebase round trip is exact to a tick.
        let second = ticks(Duration::from_secs(1));
        let per_tick = 1e9 / second as f64;
        assert!(
            per_tick > 0.0 && per_tick <= 1000.0,
            "{per_tick} ns per tick"
        );
        assert_eq!(ticks(Duration::ZERO), 0);
        assert_eq!(ticks(Duration::from_nanos(1)), 1, "rounded up");
        assert_eq!(ticks(Duration::MAX), u64::MAX);
        let before = now();
        let at = deadline(Instant::now() + Duration::from_millis(1500));
        let diff = (at - before) as f64 * per_tick * 1e-9;
        assert!((1.499..1.6).contains(&diff), "{diff}");
        // A past deadline is "now", not an underflow.
        let past = deadline(Instant::now() - Duration::from_secs(1));
        assert!(past <= now());
    }
}
