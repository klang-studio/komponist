//! The few OS and CPU interfaces the worker pool needs (WP-65, #45): a process-private wait with
//! a deadline and its wake, the floating-point control register, and realtime set-up of a worker
//! thread ([`set_current_thread_realtime`], [`join_workgroup`]).
//!
//! The wait per OS:
//! - Linux: a private futex (`FUTEX_WAIT` with a relative timeout, `FUTEX_WAKE`).
//! - macOS (14.4+): `os_sync_wait_on_address_with_deadline` / `os_sync_wake_by_address_*` without
//!   the `SHARED` flag, through `komponist_rt::os_sync`, which arms each wait at 8/9 of the
//!   remaining time against the kernel's timer leeway (spike #43).
//!
//! The engine knows no IPC, so it does not use `komponist-ipc`'s waits (which work on shared,
//! cross-process words).

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use crate::workers::WorkerRt;

/// RT. Blocks while `word == current`, until another thread changes it and calls [`wake`], or
/// until `deadline`. Returns `false` if the deadline passed with the word unchanged. Spurious
/// wake-ups and `EINTR` are handled inside; the only blocking syscall is the kernel wait.
#[cfg(target_os = "linux")]
pub(crate) fn wait_while(word: &AtomicU32, current: u32, deadline: Instant) -> bool {
    use rustix::io::Errno;
    use rustix::thread::futex;

    loop {
        if word.load(Ordering::Acquire) != current {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let timeout = futex::Timespec {
            tv_sec: remaining.as_secs() as i64,
            tv_nsec: i64::from(remaining.subsec_nanos()),
        };
        match futex::wait(word, futex::Flags::PRIVATE, current, Some(&timeout)) {
            // Woken (maybe spuriously), changed before the kernel looked, interrupted, or timed
            // out: the loop looks at the word and the clock again.
            Ok(()) | Err(Errno::AGAIN | Errno::INTR | Errno::TIMEDOUT) => {}
            // Not expected for an aligned word (EFAULT, EINVAL, ENOSYS); never spin on it.
            Err(_) => return word.load(Ordering::Acquire) != current,
        }
    }
}

/// RT. Wakes up to `count` threads waiting on `word`. Never blocks.
#[cfg(target_os = "linux")]
pub(crate) fn wake(word: &AtomicU32, count: usize) {
    use rustix::thread::futex;

    // The kernel reads the count as a signed int: more than `i32::MAX` would wake one thread.
    let count = count.min(i32::MAX as usize) as u32;
    let _ = futex::wake(word, futex::Flags::PRIVATE, count);
}

/// RT. Blocks while `word == current`, until another thread changes it and calls [`wake`], or
/// until `deadline`. Returns `false` if the deadline passed with the word unchanged. Spurious
/// wake-ups, `EINTR` and early timer expiries are handled inside; the only blocking syscall is
/// `os_sync_wait_on_address_with_deadline`.
#[cfg(target_os = "macos")]
pub(crate) fn wait_while(word: &AtomicU32, current: u32, deadline: Instant) -> bool {
    use komponist_rt::mach_time;
    use komponist_rt::os_sync::{self, Scope, Wait};

    // Once, so retries keep the same deadline.
    let deadline = mach_time::deadline(deadline);
    loop {
        if word.load(Ordering::Acquire) != current {
            return true;
        }
        match os_sync::wait(word, current, Some(deadline), Scope::Process) {
            Wait::Retry => {}
            // Timed out, or an unexpected error (EINVAL); never spin on it.
            Wait::TimedOut | Wait::Failed => return word.load(Ordering::Acquire) != current,
        }
    }
}

/// RT. Wakes up to `count` threads waiting on `word` (`usize::MAX`: all). Never blocks.
#[cfg(target_os = "macos")]
pub(crate) fn wake(word: &AtomicU32, count: usize) {
    use komponist_rt::os_sync::{self, Scope};

    // os_sync wakes one or all; `count` is at most the pool size (≤ 15) apart from "all", so
    // wake one at a time and stop when nobody is left.
    if count == usize::MAX {
        os_sync::wake_all(word, Scope::Process);
        return;
    }
    for _ in 0..count {
        if !os_sync::wake_one(word, Scope::Process) {
            break;
        }
    }
}

/// RT. The calling thread's floating-point control state (MXCSR on x86_64, FPCR on aarch64, 0
/// elsewhere): rounding mode, flush-to-zero, denormals-are-zero. Workers copy the dispatching
/// thread's state, so a track computes the same bits on every thread.
pub(crate) fn fp_state() -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        let mut csr: u32 = 0;
        // SAFETY: `stmxcsr` stores the calling thread's SSE control register into `csr`, a valid
        // local. Nothing else is touched.
        unsafe {
            core::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags));
        }
        u64::from(csr)
    }
    #[cfg(target_arch = "aarch64")]
    {
        let fpcr: u64;
        // SAFETY: reads only the calling thread's floating-point control register.
        unsafe {
            core::arch::asm!("mrs {}, fpcr", out(reg) fpcr, options(nomem, nostack, preserves_flags));
        }
        fpcr
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        0
    }
}

/// RT. Loads a state read by [`fp_state`] (on the same architecture) into the calling thread.
pub(crate) fn set_fp_state(state: u64) {
    #[cfg(target_arch = "x86_64")]
    {
        let csr = state as u32;
        // SAFETY: loads a value `stmxcsr` produced into the calling thread's SSE control
        // register; reserved bits are therefore zero.
        unsafe {
            core::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, preserves_flags));
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: writes back a value read from FPCR; only the calling thread's floating-point
        // control register changes.
        unsafe {
            core::arch::asm!("msr fpcr, {}", in(reg) state, options(nomem, nostack, preserves_flags));
        }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        let _ = state;
    }
}

/// Non-RT, on a new worker thread. Realtime scheduling per `rt`: `SCHED_FIFO` at
/// [`WorkerRt::fifo_priority`] on Linux, the time-constraint policy for [`WorkerRt::period`] on
/// macOS (`komponist_rt::set_current_thread_realtime`).
pub(crate) fn set_current_thread_realtime(rt: &WorkerRt) -> std::io::Result<()> {
    komponist_rt::set_current_thread_realtime(rt.period, rt.fifo_priority)
}

/// Non-RT, on a new worker thread. Joins [`WorkerRt::workgroup`] (macOS: the device's IO
/// workgroup). `None` without a workgroup; the worker leaves it with the token before it ends.
pub(crate) fn join_workgroup(
    rt: &WorkerRt,
) -> Option<Result<(komponist_rt::Workgroup, komponist_rt::JoinToken), i32>> {
    let workgroup = rt.workgroup.clone()?;
    Some(workgroup.join().map(|token| (workgroup, token)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn wait_returns_at_once_when_changed_and_times_out_otherwise() {
        let word = AtomicU32::new(1);
        assert!(wait_while(&word, 0, Instant::now()));
        let t0 = Instant::now();
        assert!(!wait_while(&word, 1, t0 + Duration::from_millis(10)));
        assert!(t0.elapsed() >= Duration::from_millis(10));
    }

    #[test]
    fn wake_from_another_thread() {
        let word = Arc::new(AtomicU32::new(0));
        let other = Arc::clone(&word);
        let waker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(5));
            other.store(1, Ordering::Release);
            wake(&other, 1);
        });
        assert!(wait_while(
            &word,
            0,
            Instant::now() + Duration::from_secs(5)
        ));
        waker.join().unwrap();
    }

    #[test]
    fn wake_all_wakes_every_waiter() {
        let word = Arc::new(AtomicU32::new(0));
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let word = Arc::clone(&word);
                std::thread::spawn(move || {
                    let t0 = Instant::now();
                    wait_while(&word, 0, t0 + Duration::from_secs(10));
                    t0.elapsed()
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(20));
        word.store(1, Ordering::Release);
        wake(&word, usize::MAX);
        for waiter in waiters {
            assert!(waiter.join().unwrap() < Duration::from_secs(5));
        }
    }

    #[test]
    fn fp_state_round_trips_on_another_thread() {
        let state = fp_state();
        let seen = std::thread::spawn(move || {
            set_fp_state(state);
            fp_state()
        })
        .join()
        .unwrap();
        assert_eq!(seen, state);
    }

    #[test]
    fn waits_and_wakes_allocate_nothing() {
        let word = AtomicU32::new(3);
        let outcome = assert_no_alloc::assert_no_alloc(|| {
            let changed = wait_while(&word, 2, Instant::now());
            let timed_out = wait_while(&word, 3, Instant::now() + Duration::from_millis(1));
            wake(&word, 1);
            wake(&word, 3);
            wake(&word, usize::MAX);
            (changed, timed_out)
        });
        assert_eq!(outcome, (true, false));
    }

    /// Two of four waiters woken: (at least) two return (on macOS one `wake_one` each).
    #[test]
    fn wake_count_wakes_that_many() {
        let word = Arc::new(AtomicU32::new(0));
        let waiters: Vec<_> = (0..4)
            .map(|_| {
                let word = Arc::clone(&word);
                std::thread::spawn(move || {
                    wait_while(&word, 0, Instant::now() + Duration::from_secs(10))
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(50));
        word.store(1, Ordering::Release);
        wake(&word, 2);
        let t0 = Instant::now();
        while waiters.iter().filter(|w| w.is_finished()).count() < 2 {
            assert!(t0.elapsed() < Duration::from_secs(5), "fewer than two woke");
            std::thread::sleep(Duration::from_millis(1));
        }
        // The rest, so the test ends.
        wake(&word, usize::MAX);
        for waiter in waiters {
            assert!(waiter.join().unwrap());
        }
    }
}
