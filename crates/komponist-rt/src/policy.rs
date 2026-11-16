//! Realtime scheduling of the calling thread.
//!
//! - Linux: `SCHED_FIFO` at a priority (the bridges and the engine's workers use 80, below
//!   PipeWire's data loops). Needs `RLIMIT_RTPRIO` or `CAP_SYS_NICE`.
//! - macOS: the Mach time-constraint policy with the values of spike #43 (internal notes): period =
//!   the callback period, computation = 50 % of it, constraint = the period, preemptible. Needs no
//!   privilege; the kernel refuses values out of range (`KERN_INVALID_ARGUMENT`), and a refused
//!   thread silently stays timeshare, so the result must be checked and reported. On Apple silicon
//!   the policy alone is not enough: the thread must also be in an audio workgroup
//!   ([`crate::workgroup`]).

use std::io;
use std::time::Duration;

/// Non-RT. Gives the calling thread realtime scheduling for work every `period`: `SCHED_FIFO` at
/// `fifo_priority` on Linux ([`set_current_thread_fifo`]), the time-constraint policy for `period`
/// on macOS ([`set_current_thread_time_constraint`]). Each OS ignores the other's argument. An error
/// says why the thread runs without it.
pub fn set_current_thread_realtime(period: Duration, fifo_priority: i32) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let _ = period;
        set_current_thread_fifo(fifo_priority)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = fifo_priority;
        set_current_thread_time_constraint(period)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (period, fifo_priority);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Non-RT. Switches the calling thread to `SCHED_FIFO` at `priority` (1 ..= 99). Fails with
/// `EPERM` when neither `RLIMIT_RTPRIO` nor `CAP_SYS_NICE` allows it, with `EINVAL` for a priority
/// out of range.
#[cfg(target_os = "linux")]
pub fn set_current_thread_fifo(priority: i32) -> io::Result<()> {
    // SAFETY: `sched_param` is plain data; zeroed is a valid value (musl has reserved fields).
    let mut param: libc::sched_param = unsafe { core::mem::zeroed() };
    param.sched_priority = priority;
    // SAFETY: `pthread_self` has no preconditions; `param` is a valid `sched_param` that lives for
    // the call. `pthread_setschedparam` returns an errno value instead of setting `errno`.
    let rc = unsafe { libc::pthread_setschedparam(libc::pthread_self(), libc::SCHED_FIFO, &param) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(rc))
    }
}

/// Non-RT. Sets the time-constraint policy for work every `period` on the calling thread:
/// computation 50 % of `period`, constraint = `period`, preemptible. An error carries the kernel's
/// `kern_return_t` (4 = `KERN_INVALID_ARGUMENT`: a period the kernel does not accept).
#[cfg(target_os = "macos")]
pub fn set_current_thread_time_constraint(period: Duration) -> io::Result<()> {
    let period = u32::try_from(crate::mach_time::ticks(period)).unwrap_or(u32::MAX);
    let mut policy = libc::thread_time_constraint_policy {
        period,
        computation: period / 2,
        constraint: period,
        preemptible: 1,
    };
    // SAFETY: `pthread_mach_thread_np` returns the calling thread's port without taking a new
    // reference (unlike `mach_thread_self`), so nothing leaks. `policy` is a valid
    // `thread_time_constraint_policy` with the matching count, alive for the call.
    let kr = unsafe {
        libc::thread_policy_set(
            libc::pthread_mach_thread_np(libc::pthread_self()),
            libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
            (&raw mut policy).cast(),
            libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
        )
    };
    if kr == libc::KERN_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "time-constraint policy refused (kern_return_t {kr})"
        )))
    }
}

/// The calling thread's time-constraint policy (period, computation, constraint in ticks), `None`
/// while it runs without one.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn current_time_constraint() -> Option<(u32, u32, u32)> {
    let mut policy = libc::thread_time_constraint_policy {
        period: 0,
        computation: 0,
        constraint: 0,
        preemptible: 0,
    };
    let mut count = libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT;
    let mut default = 0;
    // SAFETY: as in `set_current_thread_time_constraint`; the kernel writes at most `count` words
    // into `policy`.
    let kr = unsafe {
        libc::thread_policy_get(
            libc::pthread_mach_thread_np(libc::pthread_self()),
            libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
            (&raw mut policy).cast(),
            &mut count,
            &mut default,
        )
    };
    (kr == libc::KERN_SUCCESS && default == 0).then_some((
        policy.period,
        policy.computation,
        policy.constraint,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (policy, rt_priority) of the calling thread from `/proc/thread-self/stat`.
    #[cfg(target_os = "linux")]
    fn current_policy() -> (u32, u32) {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..]
            .split_whitespace()
            .collect();
        // Fields 40 (rt_priority) and 41 (policy), counted from 1 with pid and comm as 1 and 2.
        (fields[38].parse().unwrap(), fields[37].parse().unwrap())
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fifo_is_applied_or_refused_cleanly() {
        let outcome = std::thread::spawn(|| {
            let before = current_policy();
            let result = set_current_thread_realtime(Duration::from_millis(5), 80);
            (before, result.map(|()| current_policy()))
        })
        .join()
        .unwrap();
        assert_eq!(outcome.0, (0, 0), "a new thread starts as SCHED_OTHER");
        match outcome.1 {
            Ok(after) => assert_eq!(after, (1, 80)),
            // Allowed: machines without an RT rlimit.
            Err(e) => {
                assert_eq!(e.raw_os_error(), Some(libc::EPERM), "{e}");
                eprintln!("SCHED_FIFO not permitted here: {e}");
            }
        }
        let invalid = std::thread::spawn(|| set_current_thread_fifo(1000))
            .join()
            .unwrap();
        // EINVAL, or EPERM where RT scheduling is not allowed at all.
        assert!(invalid.is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn time_constraint_is_applied_with_the_spike_values() {
        let period = Duration::from_micros(2667);
        let (before, result, after) = std::thread::spawn(move || {
            let before = current_time_constraint();
            let result = set_current_thread_realtime(period, 80);
            (before, result, current_time_constraint())
        })
        .join()
        .unwrap();
        assert_eq!(before, None, "a new thread starts as timeshare");
        result.expect("the time-constraint policy needs no privilege");
        let ticks = crate::mach_time::ticks(period) as u32;
        assert_eq!(after, Some((ticks, ticks / 2, ticks)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_refused_time_constraint_is_an_error() {
        // A computation of half an hour is far above the kernel's maximum.
        let result =
            std::thread::spawn(|| set_current_thread_time_constraint(Duration::from_secs(3600)))
                .join()
                .unwrap();
        let error = result.expect_err("the kernel refuses it");
        assert!(error.to_string().contains("kern_return_t 4"), "{error}");
    }
}
