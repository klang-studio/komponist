//! The few OS and CPU interfaces the worker pool needs (WP-65, #45): the floating-point control
//! register and realtime set-up of a worker thread ([`set_current_thread_realtime`],
//! [`join_workgroup`]).
//!
//! The workers' wait and wake is `komponist_rt::os_sync` with [`Scope::Process`] (#243): a private
//! futex on Linux, `os_sync_wait_on_address` with the timer-leeway compensation on macOS,
//! `WaitOnAddress` on Windows. Call sites (`workers.rs`): the dispatcher waits with a deadline of
//! one callback period (about 0.7 to 46 ms) only for jobs a worker is running, and a timeout
//! merely counts `WorkerStats::late` and waits again (the real deadline, the audio callback, is
//! enforced by the device); idle workers wait one second to look at the shutdown flag. A deadline
//! that is a millisecond late therefore changes nothing but the statistic, which is why the coarse
//! millisecond timeout of `WaitOnAddress` suffices on Windows (without `timeBeginPeriod`, `late`
//! is counted up to one timer tick late).
//!
//! [`Scope::Process`]: komponist_rt::os_sync::Scope::Process

use crate::workers::WorkerRt;

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

/// RT. `state` (from [`fp_state`]) with flush-to-zero and denormals-are-zero set: MXCSR bits 15
/// and 6 on x86_64, FPCR.FZ (bit 24, which also flushes denormal inputs) on aarch64.
pub(crate) fn with_flush_to_zero(state: u64) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        state | (1 << 15) | (1 << 6)
    }
    #[cfg(target_arch = "aarch64")]
    {
        state | (1 << 24)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        state
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
}
