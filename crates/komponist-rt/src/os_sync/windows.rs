//! Windows: `WaitOnAddress` / `WakeByAddressSingle` / `WakeByAddressAll`, within one process.
//!
//! `WaitOnAddress` takes a relative timeout in whole milliseconds, which runs on the coarse system
//! clock: a 1 ms timeout took a median of 12 ms at the default 15.6 ms timer resolution (~+1 to
//! 2 ms under `timeBeginPeriod(1)`), and it may also end up to a tick early (spike windows-wake S1,
//! internal notes). So each wait asks for the remaining time rounded up, and a timeout counts only
//! once `Instant` has reached the deadline; before that the caller waits again. No spin and no
//! waitable timer: the engine's workers, the only users on Windows, need the deadline only to count
//! a late job (the real deadline, the audio callback, is the device's). The wake round trip is
//! fast (p50 0.5 µs, max ~1 to 4 µs for MMCSS threads, spike S1). Nothing here calls
//! `timeBeginPeriod`.

use core::ffi::c_void;
use core::sync::atomic::AtomicU32;
use std::time::Instant;

use windows_sys::Win32::Foundation::{ERROR_TIMEOUT, GetLastError};
use windows_sys::Win32::System::Threading::{
    INFINITE, WaitOnAddress, WakeByAddressAll, WakeByAddressSingle,
};

use super::{Scope, Wait};

/// `Instant` itself: `WaitOnAddress` takes a relative timeout, computed per wait.
pub(super) type Deadline = Instant;

/// RT. The deadline as it is.
pub(super) fn deadline(at: Instant) -> Instant {
    at
}

/// RT. One `WaitOnAddress` while `word == current`, with the remaining time rounded up to whole
/// milliseconds (`None`: no timeout).
pub(super) fn wait(
    word: &AtomicU32,
    current: u32,
    deadline: Option<Deadline>,
    // Only `Scope::Process` exists on Windows.
    _scope: Scope,
) -> Wait {
    let timeout = match deadline {
        None => INFINITE,
        Some(deadline) => {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Wait::TimedOut;
            }
            // Below INFINITE (u32::MAX), which would mean "no timeout".
            remaining
                .as_nanos()
                .div_ceil(1_000_000)
                .min(u128::from(INFINITE - 1)) as u32
        }
    };
    // SAFETY: both pointers are valid for the 4 bytes compared for the whole call (`word.as_ptr()`
    // points into the caller's atomic, `current` is a local); `word` is 4-byte aligned like every
    // `AtomicU32`.
    let woken = unsafe {
        WaitOnAddress(
            word.as_ptr().cast::<c_void>(),
            (&raw const current).cast::<c_void>(),
            size_of::<u32>(),
            timeout,
        )
    };
    if woken != 0 {
        return Wait::Retry;
    }
    // SAFETY: no preconditions; reads the calling thread's last error.
    match unsafe { GetLastError() } {
        ERROR_TIMEOUT if deadline.is_some_and(|d| Instant::now() >= d) => Wait::TimedOut,
        ERROR_TIMEOUT => Wait::Retry,
        // Not expected for an aligned 4-byte word.
        _ => Wait::Failed,
    }
}

/// RT. Wakes up to `count` waiters (`usize::MAX`: all). Returns 0: Windows does not report how
/// many it woke.
pub(super) fn wake(word: &AtomicU32, count: usize, _scope: Scope) -> usize {
    let address = word.as_ptr().cast_const().cast::<c_void>();
    // SAFETY: `address` points to a live atomic; the calls only look the address up in the
    // process's wait table, and waking an address nobody waits on is a no-op.
    unsafe {
        if count == usize::MAX {
            WakeByAddressAll(address);
        } else {
            // The callers wake at most a pool's size (≤ 15) apart from "all".
            for _ in 0..count {
                WakeByAddressSingle(address);
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::os_sync::{WaitOutcome, wait_while};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// The waiter's resume time after a wake, excluding thread teardown and the caller's join
    /// latency.
    fn wake_round_trip(
        wait: fn(&AtomicU32, u32, Instant) -> bool,
        signal: fn(&AtomicU32),
    ) -> Duration {
        let word = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&word);
        let (ready, started) = std::sync::mpsc::sync_channel(0);
        let waiter = std::thread::spawn(move || {
            ready.send(()).unwrap();
            assert!(wait(&other, 0, Instant::now() + Duration::from_secs(10)));
            assert_eq!(other.load(Ordering::Acquire), 1);
            Instant::now()
        });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        // Let the waiter reach the kernel; identical for the wrapper and the native reference.
        std::thread::sleep(Duration::from_millis(1));
        let sent = Instant::now();
        word.store(1, Ordering::Release);
        signal(&word);
        let latency = waiter
            .join()
            .unwrap()
            .checked_duration_since(sent)
            .expect("wait returned before the word changed");
        assert!(
            latency < Duration::from_secs(1),
            "wake did not interrupt the long wait: {latency:?}"
        );
        latency
    }

    /// An independent Win32 reference, without the wrapper, on the same loaded machine.
    fn native_wait_on_address(word: &AtomicU32, current: u32, deadline: Instant) -> bool {
        while word.load(Ordering::Acquire) == current {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let millis = remaining
                .as_nanos()
                .div_ceil(1_000_000)
                .min(u128::from(u32::MAX - 1)) as u32;
            // SAFETY: valid, aligned word and comparison value for the duration of the wait.
            unsafe {
                WaitOnAddress(
                    word.as_ptr().cast(),
                    (&raw const current).cast(),
                    size_of::<u32>(),
                    millis,
                );
            }
        }
        true
    }

    fn native_wake_by_address(word: &AtomicU32) {
        // SAFETY: the word is valid and aligned for the whole call.
        unsafe { WakeByAddressSingle(word.as_ptr().cast()) };
    }

    fn wrapper_wait(word: &AtomicU32, current: u32, deadline: Instant) -> bool {
        wait_while(word, current, Some(deadline), Scope::Process) != WaitOutcome::TimedOut
    }

    fn wrapper_wake(word: &AtomicU32) {
        super::super::wake(word, 1, Scope::Process);
    }

    /// Every wake interrupts the pending wait. Compares the median latency with the native
    /// primitive in interleaved rounds: a shared VM can delay either thread by a scheduler
    /// quantum. The wrapper's 1 ms overhead budget still catches a coarse-timer poll (about
    /// 15.6 ms) without treating thread teardown or runner scheduling as wrapper latency.
    #[test]
    fn the_wake_round_trip_is_as_fast_as_the_native_one() {
        let mut wrapper = Vec::new();
        let mut native = Vec::new();
        for round in 0..31 {
            for reference in [round % 2 == 0, round % 2 != 0] {
                if reference {
                    native.push(wake_round_trip(
                        native_wait_on_address,
                        native_wake_by_address,
                    ));
                } else {
                    wrapper.push(wake_round_trip(wrapper_wait, wrapper_wake));
                }
            }
        }
        wrapper.sort_unstable();
        native.sort_unstable();
        let wrapper_median = wrapper[wrapper.len() / 2];
        let native_median = native[native.len() / 2];
        eprintln!("wake round trip: wrapper {wrapper_median:?}, native {native_median:?}");
        assert!(
            wrapper_median <= native_median + Duration::from_millis(1),
            "wrapper {wrapper_median:?}, native {native_median:?}"
        );
    }
}
