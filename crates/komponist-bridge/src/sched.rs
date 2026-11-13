//! Realtime scheduling for the bridge RT thread (ARCHITECTURE §8.1: "RT priority is mandatory").
//!
//! std has no API for `SCHED_FIFO`, rustix has none either, and the workspace has no `libc`
//! dependency, so the one pthread call is declared here. std links the C library on Linux, which
//! exports it (glibc and musl alike).

use std::os::raw::c_int;
use std::os::unix::thread::RawPthread;

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

/// Lowest and highest `SCHED_FIFO` priority on Linux.
pub const PRIORITY_RANGE: std::ops::RangeInclusive<i32> = 1..=99;

/// Default priority of the bridge RT thread: below PipeWire's data loops (88 for the server, 83
/// for clients on this machine), the value spike WP-15 measured with (`chrt -f 80`).
pub const DEFAULT_PRIORITY: i32 = 80;

/// Switches the calling thread to `SCHED_FIFO` at `priority`. Fails with `EPERM` when neither
/// `RLIMIT_RTPRIO` nor `CAP_SYS_NICE` allows it.
pub fn set_current_thread_fifo(priority: i32) -> std::io::Result<()> {
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

    /// (policy, rt_priority) of the calling thread from `/proc/thread-self/stat`.
    fn current_policy() -> (u32, u32) {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap();
        let fields: Vec<&str> = stat[stat.rfind(')').unwrap() + 2..]
            .split_whitespace()
            .collect();
        // Fields 40 (rt_priority) and 41 (policy), counted from 1 with pid and comm as 1 and 2.
        (fields[38].parse().unwrap(), fields[37].parse().unwrap())
    }

    #[test]
    fn fifo_is_applied_or_refused_cleanly() {
        let outcome = std::thread::spawn(|| {
            let before = current_policy();
            let result = set_current_thread_fifo(DEFAULT_PRIORITY);
            (before, result.map(|()| current_policy()))
        })
        .join()
        .unwrap();
        assert_eq!(outcome.0, (0, 0), "a new thread starts as SCHED_OTHER");
        match outcome.1 {
            Ok(after) => assert_eq!(after, (1, DEFAULT_PRIORITY as u32)),
            // Allowed: machines without an RT rlimit (the bridge reports it to the app).
            Err(e) => {
                assert_eq!(e.raw_os_error(), Some(1), "{e}");
                eprintln!("SCHED_FIFO not permitted here: {e}");
            }
        }
    }

    #[test]
    fn invalid_priority_is_an_error() {
        let result = std::thread::spawn(|| set_current_thread_fifo(1000))
            .join()
            .unwrap();
        // EINVAL, or EPERM where RT scheduling is not allowed at all.
        assert!(result.is_err());
    }
}
