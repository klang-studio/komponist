//! Crash detection for bridge processes (ARCHITECTURE §8.1, "Crash").
//!
//! The plugin manager spawns a bridge, wraps its pid in a [`ProcessWatcher`] at once and checks it
//! every UI frame with [`ProcessWatcher::try_exit`] (one non-blocking wait), or puts the watcher's
//! fd ([`AsFd`]) into a `poll()` set: it turns readable the moment the process dies. The watcher
//! reaps the child, so nobody else may wait for it (drop the `std::process::Child` without calling
//! `wait`, or do not create one). Per OS:
//!
//! - Linux: a pidfd. `waitid(P_PIDFD)` reaps, signals go through the pidfd.
//! - macOS: a kqueue with an `EVFILT_PROC` / `NOTE_EXIT` filter on the pid (the kqueue fd is
//!   readable once the exit event is pending; nobody consumes it). `waitpid` reaps, `kill` signals.
//!
//! Both are safe against pid reuse: the child stays unreaped (a zombie keeps its pid) until the
//! watcher reaps it, and after that the watcher never touches the pid again.
//!
//! Non-RT: the engine never touches processes; a dead bridge just makes its blocks late.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, poll};
use rustix::io::Errno;
use rustix::process::Pid;
use rustix::time::Timespec;

/// How a watched process ended; the fields of `komponist_plugins::FailReason::Exited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessExit {
    /// Exit code, if it exited normally.
    pub code: Option<i32>,
    /// Terminating signal, if it was killed (SIGSEGV = 11, SIGABRT = 6, SIGKILL = 9).
    pub signal: Option<i32>,
}

/// A watched child process. Owned by the plugin manager (UI thread).
#[derive(Debug)]
pub struct ProcessWatcher {
    /// Linux: the pidfd. macOS: the kqueue watching the pid.
    fd: OwnedFd,
    pid: Pid,
    exit: Option<ProcessExit>,
}

impl ProcessWatcher {
    /// Watches child `pid` (e.g. `std::process::Child::id`). Call right after spawning: the
    /// process must be a child of this process that nobody has reaped yet (an exited, unreaped
    /// child is fine, its pid cannot be reused).
    pub fn new(pid: u32) -> io::Result<ProcessWatcher> {
        let raw = i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let pid = Pid::from_raw(raw).ok_or(io::ErrorKind::InvalidInput)?;
        let (fd, exit) = sys::watch(pid)?;
        Ok(ProcessWatcher { fd, pid, exit })
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        self.pid.as_raw_pid() as u32
    }

    /// Non-blocking: `Some` once the process has exited. The first detection reaps it; later
    /// calls return the same value.
    pub fn try_exit(&mut self) -> io::Result<Option<ProcessExit>> {
        if self.exit.is_none() {
            self.exit = sys::try_reap(self.fd.as_fd(), self.pid)?;
        }
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
            let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
            match poll(&mut fds, ts.as_ref()) {
                Ok(_) | Err(Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Sends SIGKILL. A no-op once the exit was observed; `Ok` if the process is already gone.
    pub fn kill(&self) -> io::Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        match sys::kill(self.fd.as_fd(), self.pid) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

impl AsFd for ProcessWatcher {
    /// For `poll()`: readable once the process has exited (the pidfd on Linux, the kqueue on
    /// macOS).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::io;
    use std::os::fd::{BorrowedFd, OwnedFd};

    use rustix::io::Errno;
    use rustix::process::{
        Pid, PidfdFlags, Signal, WaitId, WaitIdOptions, pidfd_open, pidfd_send_signal, waitid,
    };

    use super::ProcessExit;

    /// Opens the pidfd. The exit is never known yet (`waitid` tells).
    pub(super) fn watch(pid: Pid) -> io::Result<(OwnedFd, Option<ProcessExit>)> {
        Ok((pidfd_open(pid, PidfdFlags::empty())?, None))
    }

    /// One non-blocking `waitid` on the pidfd; reaps the child if it has exited.
    pub(super) fn try_reap(pidfd: BorrowedFd<'_>, _pid: Pid) -> io::Result<Option<ProcessExit>> {
        let options = WaitIdOptions::EXITED | WaitIdOptions::NOHANG;
        let status = loop {
            match waitid(WaitId::PidFd(pidfd), options) {
                Err(Errno::INTR) => continue,
                other => break other?,
            }
        };
        // `waitid` with `WNOHANG` returns no status (or a zeroed one) while the child runs.
        let Some(status) = status else {
            return Ok(None);
        };
        Ok(if status.exited() {
            Some(ProcessExit {
                code: status.exit_status(),
                signal: None,
            })
        } else if status.killed() || status.dumped() {
            Some(ProcessExit {
                code: None,
                signal: status.terminating_signal(),
            })
        } else {
            // Stopped or continued: not reported without WSTOPPED / WCONTINUED.
            None
        })
    }

    /// SIGKILL through the pidfd, which cannot hit a recycled pid.
    pub(super) fn kill(pidfd: BorrowedFd<'_>, _pid: Pid) -> Result<(), Errno> {
        pidfd_send_signal(pidfd, Signal::KILL)
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::io;
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
    use std::time::Duration;

    use rustix::event::kqueue::{Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue};
    use rustix::io::Errno;
    use rustix::process::{Pid, Signal, WaitOptions, kill_process, waitpid};

    use super::ProcessExit;

    /// A kqueue with an `EVFILT_PROC` / `NOTE_EXIT` filter on `pid`. The event is never
    /// consumed, so the kqueue stays readable once the process has exited.
    pub(super) fn watch(pid: Pid) -> io::Result<(OwnedFd, Option<ProcessExit>)> {
        let kq = kqueue()?;
        let filter = EventFilter::Proc {
            pid,
            flags: ProcessEvents::EXIT,
        };
        let change = Event::new(filter, EventFlags::ADD, core::ptr::null_mut());
        // SAFETY: the change refers to a pid, not to a file descriptor; the zero-capacity event
        // list makes this a pure registration that returns at once.
        let registered = unsafe {
            kevent::<_, &mut Vec<Event>>(&kq, &[change], &mut Vec::new(), Some(Duration::ZERO))
        };
        match registered {
            Ok(_) => Ok((kq, None)),
            // The process has already exited (an unreaped child is a zombie, which EVFILT_PROC
            // does not attach to). Reap it now if it is ours; the exit is then known and the
            // kqueue is made readable with a user event, as the exit filter would have.
            Err(Errno::SRCH) => match try_reap(kq.as_fd(), pid)? {
                Some(exit) => {
                    trigger(&kq)?;
                    Ok((kq, Some(exit)))
                }
                None => Err(Errno::SRCH.into()),
            },
            Err(e) => Err(e.into()),
        }
    }

    /// Makes `kq` readable for good with a triggered `EVFILT_USER` event.
    fn trigger(kq: &OwnedFd) -> io::Result<()> {
        use rustix::event::kqueue::{UserDefinedFlags, UserFlags};
        let filter = EventFilter::User {
            ident: 0,
            flags: UserFlags::TRIGGER,
            user_flags: UserDefinedFlags::new(0),
        };
        let change = Event::new(filter, EventFlags::ADD, core::ptr::null_mut());
        // SAFETY: a user event, no file descriptor involved.
        unsafe {
            kevent::<_, &mut Vec<Event>>(kq, &[change], &mut Vec::new(), Some(Duration::ZERO))?;
        }
        Ok(())
    }

    /// One non-blocking `waitpid`; reaps the child if it has exited.
    pub(super) fn try_reap(_kq: BorrowedFd<'_>, pid: Pid) -> io::Result<Option<ProcessExit>> {
        let status = loop {
            match waitpid(Some(pid), WaitOptions::NOHANG) {
                Err(Errno::INTR) => continue,
                other => break other?,
            }
        };
        let Some((_, status)) = status else {
            return Ok(None);
        };
        Ok(if status.exited() {
            Some(ProcessExit {
                code: status.exit_status(),
                signal: None,
            })
        } else if status.signaled() {
            Some(ProcessExit {
                code: None,
                signal: status.terminating_signal(),
            })
        } else {
            // Stopped or continued: not reported without WUNTRACED / WCONTINUED.
            None
        })
    }

    /// SIGKILL to the pid. Safe while the child is unreaped: its pid cannot be reused.
    pub(super) fn kill(_kq: BorrowedFd<'_>, pid: Pid) -> Result<(), Errno> {
        kill_process(pid, Signal::KILL)
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

    /// Readiness of the watcher's fd within `timeout` (`poll`, as the plugin manager uses it).
    fn readable(watcher: &ProcessWatcher, timeout: Duration) -> bool {
        let ts = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(watcher, PollFlags::IN)];
        poll(&mut fds, Some(&ts)).unwrap() == 1
    }

    #[test]
    fn the_fd_turns_readable_on_exit_and_stays_so() {
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let mut watcher = ProcessWatcher::new(child.id()).unwrap();
        drop(child);
        assert!(!readable(&watcher, Duration::ZERO));
        assert!(!readable(&watcher, Duration::from_millis(20)));
        let t0 = Instant::now();
        watcher.kill().unwrap();
        assert!(readable(&watcher, Duration::from_secs(10)));
        assert!(t0.elapsed() < Duration::from_secs(1), "{:?}", t0.elapsed());
        // Polling does not consume the readiness: a second poll sees it too.
        assert!(readable(&watcher, Duration::ZERO));
        let exit = watcher.wait_exit(Some(Duration::from_secs(10))).unwrap();
        assert_eq!(exit.and_then(|e| e.signal), Some(9));
    }

    #[test]
    fn an_exited_unreaped_child_is_reported() {
        let child = Command::new("sh").args(["-c", "exit 5"]).spawn().unwrap();
        // Give the child ample time to exit, so it is a zombie (exited, not reaped) before it is
        // watched (macOS: EVFILT_PROC does not attach to a zombie, the watcher falls back).
        std::thread::sleep(Duration::from_millis(300));
        let mut watcher = ProcessWatcher::new(child.id()).unwrap();
        drop(child);
        assert!(readable(&watcher, Duration::from_secs(10)));
        let expected = ProcessExit {
            code: Some(5),
            signal: None,
        };
        assert_eq!(watcher.try_exit().unwrap(), Some(expected));
        assert_eq!(
            watcher.wait_exit(Some(Duration::ZERO)).unwrap(),
            Some(expected)
        );
        assert!(watcher.kill().is_ok());
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
