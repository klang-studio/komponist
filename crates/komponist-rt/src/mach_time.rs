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

/// RT. `deadline` as an absolute `mach_absolute_time`. `Instant::now()` is read before
/// `mach_absolute_time()`, so the result is never earlier than `deadline`; a past deadline gives
/// the current tick count.
pub fn deadline(deadline: Instant) -> u64 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    now().saturating_add(ticks(remaining))
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
