//! The few OS and CPU interfaces the worker pool needs (WP-65): a process-private futex with a
//! deadline, the floating-point control register and `SCHED_FIFO`.
//!
//! The engine knows no IPC, so it does not use `komponist-ipc`'s futex helpers (which work on
//! shared, cross-process words); the bridge's `SCHED_FIFO` call is copied for the same reason.

use std::os::raw::c_int;
use std::os::unix::thread::RawPthread;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use rustix::io::Errno;
use rustix::thread::futex;

/// RT. Blocks while `word == current`, until another thread changes it and calls [`wake`], or
/// until `deadline`. Returns `false` if the deadline passed with the word unchanged. Spurious
/// wake-ups and `EINTR` are handled inside; the only blocking syscall is `FUTEX_WAIT`.
pub(crate) fn wait_while(word: &AtomicU32, current: u32, deadline: Instant) -> bool {
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
pub(crate) fn wake(word: &AtomicU32, count: usize) {
    // The kernel reads the count as a signed int: more than `i32::MAX` would wake one thread.
    let count = count.min(i32::MAX as usize) as u32;
    let _ = futex::wake(word, futex::Flags::PRIVATE, count);
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

/// `SCHED_FIFO` (`<sched.h>`, the same value on every Linux architecture).
const SCHED_FIFO: c_int = 1;

/// `struct sched_param` (`<sched.h>`): only the priority field on Linux.
#[repr(C)]
struct SchedParam {
    sched_priority: c_int,
}

unsafe extern "C" {
    fn pthread_self() -> RawPthread;
    fn pthread_setschedparam(thread: RawPthread, policy: c_int, param: *const SchedParam) -> c_int;
}

/// Non-RT. Switches the calling thread to `SCHED_FIFO` at `priority` (1 ..= 99). Fails with
/// `EPERM` when neither `RLIMIT_RTPRIO` nor `CAP_SYS_NICE` allows it.
pub(crate) fn set_current_thread_fifo(priority: i32) -> std::io::Result<()> {
    let param = SchedParam {
        sched_priority: priority,
    };
    // SAFETY: `pthread_self` has no preconditions; `param` is a valid `sched_param` that lives for
    // the call. `pthread_setschedparam` returns an errno value instead of setting `errno`.
    let rc = unsafe { pthread_setschedparam(pthread_self(), SCHED_FIFO, &param) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(rc))
    }
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
    fn fifo_is_applied_or_refused_cleanly() {
        let result = std::thread::spawn(|| set_current_thread_fifo(10))
            .join()
            .unwrap();
        if let Err(e) = result {
            // Machines without an RT rlimit refuse it.
            assert_eq!(e.raw_os_error(), Some(1), "{e}");
        }
        let invalid = std::thread::spawn(|| set_current_thread_fifo(1000))
            .join()
            .unwrap();
        assert!(invalid.is_err());
    }
}
