//! Realtime scheduling for the bridge RT thread (ARCHITECTURE §8.1: "RT priority is mandatory").
//!
//! The calls themselves are in `komponist-rt` (shared with the engine's workers): `SCHED_FIFO` at
//! `--rt-priority` on Linux; on macOS (#45) the time-constraint policy for the activation's block
//! period, plus an own audio work interval that the RT loop starts and finishes per block with the
//! app's deadline ([`crate::rt`]); on Windows MMCSS ("Pro Audio", `THREAD_PRIORITY_TIME_CRITICAL` as
//! the fallback; `--rt-priority` does not apply).

/// Lowest and highest `SCHED_FIFO` priority on Linux.
pub const PRIORITY_RANGE: std::ops::RangeInclusive<i32> = 1..=99;

/// Default priority of the bridge RT thread: below PipeWire's data loops (88 for the server, 83
/// for clients on this machine), the value spike WP-15 measured with (`chrt -f 80`).
pub const DEFAULT_PRIORITY: i32 = 80;

/// The realtime scheduling this system gives the RT thread, for log lines.
pub fn policy_name(priority: i32) -> String {
    if cfg!(target_os = "macos") {
        "the time-constraint policy".to_owned()
    } else if cfg!(windows) {
        "MMCSS (Pro Audio)".to_owned()
    } else {
        format!("SCHED_FIFO {priority}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_policy_is_named_per_system() {
        let name = policy_name(DEFAULT_PRIORITY);
        if cfg!(target_os = "macos") {
            assert_eq!(name, "the time-constraint policy");
        } else if cfg!(windows) {
            assert_eq!(name, "MMCSS (Pro Audio)");
        } else {
            assert_eq!(name, "SCHED_FIFO 80");
        }
    }
}
