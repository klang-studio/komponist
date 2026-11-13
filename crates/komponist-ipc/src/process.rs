//! Crash detection for bridge processes through pidfds (ARCHITECTURE §8.1, "Crash").
//!
//! The plugin manager spawns a bridge, wraps its pid in a [`ProcessWatcher`] at once and checks it
//! every UI frame with [`ProcessWatcher::try_exit`] (one non-blocking `waitid`), or puts the
//! pidfd ([`AsFd`]) into a `poll()` set: it turns readable the moment the process dies. The watcher
//! reaps the child, so nobody else may wait for it (drop the `std::process::Child` without calling
//! `wait`, or do not create one). Signals go through the pidfd, which cannot hit a recycled pid.
//!
//! Non-RT: the engine never touches processes; a dead bridge just makes its blocks late.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use rustix::process::{
    Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, pidfd_open, pidfd_send_signal, waitid,
};
use rustix::time::Timespec;

/// How a watched process ended; the fields of `komponist_plugins::FailReason::Exited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessExit {
    /// Exit code, if it exited normally.
    pub code: Option<i32>,
    /// Terminating signal, if it was killed (SIGSEGV = 11, SIGABRT = 6, SIGKILL = 9).
    pub signal: Option<i32>,
}

/// A child process watched through its pidfd. Owned by the plugin manager (UI thread).
#[derive(Debug)]
pub struct ProcessWatcher {
    pidfd: OwnedFd,
    pid: u32,
    exit: Option<ProcessExit>,
}

impl ProcessWatcher {
    /// Watches child `pid` (e.g. `std::process::Child::id`). Call right after spawning: the
    /// process must be a child of this process that nobody has reaped yet (an exited, unreaped
    /// child is fine, its pid cannot be reused).
    pub fn new(pid: u32) -> io::Result<ProcessWatcher> {
        let raw = i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let pid_t = Pid::from_raw(raw).ok_or(io::ErrorKind::InvalidInput)?;
        let pidfd = pidfd_open(pid_t, PidfdFlags::empty())?;
        Ok(ProcessWatcher {
            pidfd,
            pid,
            exit: None,
        })
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Non-blocking: `Some` once the process has exited. The first detection reaps it; later
    /// calls return the same value.
    pub fn try_exit(&mut self) -> io::Result<Option<ProcessExit>> {
        if self.exit.is_some() {
            return Ok(self.exit);
        }
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG;
        let status = loop {
            match waitid(WaitId::PidFd(self.pidfd.as_fd()), options) {
                Err(Errno::INTR) => continue,
                other => break other?,
            }
        };
        // `waitid` with `WNOHANG` returns no status (or a zeroed one) while the child runs.
        let Some(status) = status else {
            return Ok(None);
        };
        let exit = if status.exited() {
            ProcessExit {
                code: status.exit_status(),
                signal: None,
            }
        } else if status.killed() || status.dumped() {
            ProcessExit {
                code: None,
                signal: status.terminating_signal(),
            }
        } else {
            // Stopped or continued: not reported without WSTOPPED / WCONTINUED.
            return Ok(None);
        };
        self.exit = Some(exit);
        Ok(self.exit)
    }

    /// Blocks until the process exits or `timeout` passes (`None` = no timeout); `None` on
    /// timeout. For shutdown ("wait a moment, then kill") and tests.
    pub fn wait_exit(&mut self, timeout: Option<Duration>) -> io::Result<Option<ProcessExit>> {
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            if let Some(exit) = self.try_exit()? {
                return Ok(Some(exit));
            }
            let remaining = match deadline {
                Some(d) => {
                    let now = Instant::now();
                    if now >= d {
                        return Ok(None);
                    }
                    Some(d - now)
                }
                None => None,
            };
            let ts = remaining.map(|r| Timespec {
                tv_sec: r.as_secs() as i64,
                tv_nsec: i64::from(r.subsec_nanos()),
            });
            let mut fds = [PollFd::new(&self.pidfd, PollFlags::IN)];
            match poll(&mut fds, ts.as_ref()) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Sends SIGKILL through the pidfd. A no-op once the exit was observed; `Ok` if the process is
    /// already gone.
    pub fn kill(&self) -> io::Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        match pidfd_send_signal(&self.pidfd, Signal::KILL) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl AsFd for ProcessWatcher {
    /// The pidfd, for `poll()`: readable once the process has exited.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pidfd.as_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn exit_code_is_reported() {
        let child = Command::new("sh").args(["-c", "exit 3"]).spawn().unwrap();
        let mut watcher = ProcessWatcher::new(child.id()).unwrap();
        drop(child);
        let exit = watcher.wait_exit(Some(Duration::from_secs(10))).unwrap();
        let expected = ProcessExit {
            code: Some(3),
            signal: None,
        };
        assert_eq!(exit, Some(expected));
        // Reaped once, reported again.
        assert_eq!(watcher.try_exit().unwrap(), Some(expected));
        assert!(watcher.kill().is_ok());
    }

    #[test]
    fn kill_is_reported_as_signal() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let mut watcher = ProcessWatcher::new(child.id()).unwrap();
        assert_eq!(watcher.pid(), child.id());
        drop(child);
        assert_eq!(watcher.try_exit().unwrap(), None);
        assert_eq!(
            watcher.wait_exit(Some(Duration::from_millis(20))).unwrap(),
            None
        );
        watcher.kill().unwrap();
        let exit = watcher.wait_exit(Some(Duration::from_secs(10))).unwrap();
        assert_eq!(
            exit,
            Some(ProcessExit {
                code: None,
                signal: Some(9),
            })
        );
    }

    #[test]
    fn non_child_is_refused_or_never_reaped() {
        // pid 1 is not our child: pidfd_open works, waitid refuses (ECHILD).
        if let Ok(mut watcher) = ProcessWatcher::new(1) {
            assert!(watcher.try_exit().is_err());
        }
        assert!(ProcessWatcher::new(0).is_err());
        assert!(ProcessWatcher::new(u32::MAX).is_err());
    }
}
