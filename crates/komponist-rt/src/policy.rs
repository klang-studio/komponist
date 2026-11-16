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
//! - Windows: MMCSS ("Pro Audio" task, `AVRT_PRIORITY_CRITICAL`; windows-wake spike S1, internal
//!   notes: the registration keeps wake-ups at a few µs under load), with
//!   `THREAD_PRIORITY_TIME_CRITICAL` as the fallback when MMCSS refuses (service stopped, policy).
//!   MMCSS needs no privilege. A second `AvSetMmThreadCharacteristicsW` on a registered thread
//!   fails although the thread stays registered, so each thread registers once
//!   ([`set_current_thread_mmcss`] remembers it in a thread-local).

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
    #[cfg(windows)]
    {
        let _ = (period, fifo_priority);
        set_current_thread_mmcss()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (period, fifo_priority);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(windows)]
thread_local! {
    /// Whether this thread is registered with MMCSS already (see [`set_current_thread_mmcss`]).
    /// A `const` initialiser of a type without destructor: no allocation.
    static MMCSS_REGISTERED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// Non-RT. Registers the calling thread with MMCSS as "Pro Audio" at `AVRT_PRIORITY_CRITICAL`; if
/// MMCSS refuses, raises it to `THREAD_PRIORITY_TIME_CRITICAL` instead (reported as success: the
/// thread has the highest priority of the normal class). Called again on a registered thread it
/// returns `Ok` without a second registration (which Windows would refuse). An error carries the
/// MMCSS `GetLastError` code when both ways fail, or says that the MMCSS priority was refused
/// (the thread is registered then, at normal MMCSS priority).
#[cfg(windows)]
pub fn set_current_thread_mmcss() -> io::Result<()> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::System::Threading::{
        AVRT_PRIORITY_CRITICAL, AvSetMmThreadCharacteristicsW, AvSetMmThreadPriority,
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
    };

    if MMCSS_REGISTERED.get() {
        return Ok(());
    }
    // "Pro Audio" as a NUL-terminated UTF-16 string.
    const TASK: [u16; 10] = {
        let ascii = *b"Pro Audio\0";
        let mut wide = [0u16; 10];
        let mut i = 0;
        while i < wide.len() {
            wide[i] = ascii[i] as u16;
            i += 1;
        }
        wide
    };
    // The task index must be 0 for a new task.
    let mut task_index = 0u32;
    // SAFETY: `TASK` is NUL-terminated and `task_index` a valid `u32`, both alive for the call.
    let task = unsafe { AvSetMmThreadCharacteristicsW(TASK.as_ptr(), &mut task_index) };
    if !task.is_null() {
        MMCSS_REGISTERED.set(true);
        // SAFETY: `task` is the handle the call above returned.
        if unsafe { AvSetMmThreadPriority(task, AVRT_PRIORITY_CRITICAL) } != 0 {
            return Ok(());
        }
        // SAFETY: no preconditions.
        let code = unsafe { GetLastError() };
        return Err(io::Error::other(format!(
            "MMCSS priority refused (error {code})"
        )));
    }
    // SAFETY: no preconditions.
    let mmcss_code = unsafe { GetLastError() };
    // SAFETY: `GetCurrentThread` returns a pseudo handle that needs no closing.
    if unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) } != 0 {
        return Ok(());
    }
    Err(io::Error::from_raw_os_error(mmcss_code as i32))
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

/// Shortest period [`set_current_thread_time_constraint`] sets: the kernel refuses computations
/// below 50 µs (measured on macOS 27, M1 Pro: 41.5 µs refused, 50 µs accepted). Shorter callback
/// periods (16 frames at 192 kHz: 83 µs) get this one.
#[cfg(target_os = "macos")]
pub const MIN_TIME_CONSTRAINT_PERIOD: Duration = Duration::from_micros(100);

/// Non-RT. Sets the time-constraint policy for work every `period` (at least
/// [`MIN_TIME_CONSTRAINT_PERIOD`]) on the calling thread: computation 50 % of the period,
/// constraint = the period, preemptible. An error carries the kernel's `kern_return_t`
/// (4 = `KERN_INVALID_ARGUMENT`: a period the kernel does not accept).
#[cfg(target_os = "macos")]
pub fn set_current_thread_time_constraint(period: Duration) -> io::Result<()> {
    let period = period.max(MIN_TIME_CONSTRAINT_PERIOD);
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

    /// MMCSS (or the fallback priority) is applied, or both are refused with an error; asking
    /// twice on one thread is fine.
    #[cfg(windows)]
    #[test]
    fn mmcss_is_obtained_or_refused_cleanly_and_twice_is_fine() {
        let (first, second, registered) = std::thread::spawn(|| {
            let first = set_current_thread_realtime(Duration::from_millis(5), 80);
            let second = set_current_thread_realtime(Duration::from_millis(5), 80);
            (first, second, MMCSS_REGISTERED.get())
        })
        .join()
        .unwrap();
        match (first, second) {
            (Ok(()), Ok(())) => {}
            // MMCSS registered but refused the priority, or neither way worked: the retry
            // succeeds (registered) or fails again; it never panics or hangs.
            (Err(first), second) => {
                eprintln!("RT priority not fully obtained here: {first}");
                assert!(registered || second.is_err(), "{second:?}");
            }
            (Ok(()), Err(e)) => panic!("the second call must not fail: {e}"),
        }
    }

    /// The registration is per thread: a new thread registers on its own.
    #[cfg(windows)]
    #[test]
    fn mmcss_registration_is_per_thread() {
        let run = || std::thread::spawn(|| (set_current_thread_mmcss(), MMCSS_REGISTERED.get()));
        let first = run().join().unwrap();
        let second = run().join().unwrap();
        assert_eq!(first.0.is_ok(), second.0.is_ok());
        assert_eq!(first.1, second.1);
    }

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

    /// The longest period komponist asks for: a whole 2048-frame block at 44.1 kHz (the bridge's
    /// activated maximum, 46 ms), and the shortest: 16 frames at 192 kHz (raised to the minimum).
    #[cfg(target_os = "macos")]
    #[test]
    fn the_periods_komponist_uses_are_accepted() {
        for period in [Duration::from_micros(46_440), Duration::from_micros(83)] {
            std::thread::spawn(move || set_current_thread_time_constraint(period))
                .join()
                .unwrap()
                .unwrap_or_else(|e| panic!("{period:?}: {e}"));
        }
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
