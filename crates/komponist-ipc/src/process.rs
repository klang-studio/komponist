//! Crash detection for bridge processes (ARCHITECTURE §8.1, "Crash").
//!
//! The plugin manager spawns a bridge, wraps its pid in a [`ProcessWatcher`] at once and checks it
//! every UI frame with [`ProcessWatcher::try_exit`] (one non-blocking wait), or puts the watcher's
//! fd (`AsFd`) into a `poll()` set: it turns readable the moment the process dies. The watcher
//! reaps the child, so nobody else may wait for it (drop the `std::process::Child` without calling
//! `wait`, or do not create one). Per OS:
//!
//! - Linux: a pidfd. `waitid(P_PIDFD)` reaps, signals go through the pidfd.
//! - macOS: a kqueue with an `EVFILT_PROC` / `NOTE_EXIT` filter on the pid (the kqueue fd is
//!   readable once the exit event is pending; nobody consumes it). `waitpid` reaps, `kill` signals.
//! - Windows: a process handle (`OpenProcess`; `AsHandle` gives it, signalled once the process has
//!   exited, for `WaitForMultipleObjects`, and with `PROCESS_DUP_HANDLE`, so the app can duplicate
//!   handles into the bridge through it). `WaitForSingleObject` and `GetExitCodeProcess` read the
//!   exit, `TerminateProcess` kills. There is nothing to reap.
//!
//! Linux and macOS are safe against pid reuse: the child stays unreaped (a zombie keeps its pid)
//! until the watcher reaps it, and after that the watcher never touches the pid again. On Windows
//! a pid stays taken while any handle to the process is open: create the watcher while the
//! `std::process::Child` (which holds one) is still alive; from then on the watcher's own handle
//! keeps it.
//!
//! [`ProcessExit`] on Windows, which has no signals: an exit code whose two top bits are set
//! (NTSTATUS severity "error", `0xC000_0000..`) is a crash and goes into `signal` as the NTSTATUS
//! (as `i32`): `0xC000_0005` access violation, `0xC000_0409` fast fail (`std::process::abort`,
//! `panic = "abort"`, stack buffer overrun), `0xC000_001D` illegal instruction, `0xC000_00FD` stack
//! overflow. A process ended by [`ProcessWatcher::kill`] reports `signal: Some(9)` as on Unix
//! (SIGKILL). Every other exit code is `code`. (A program that exits with such a code on purpose,
//! `std::process::exit(-1)` for one, reads as a crash too.)
//!
//! Non-RT: the engine never touches processes; a dead bridge just makes its blocks late.

use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(windows)]
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, poll};
#[cfg(unix)]
use rustix::io::Errno;
#[cfg(unix)]
use rustix::process::Pid;
#[cfg(unix)]
use rustix::time::Timespec;

/// How a watched process ended; the fields of `komponist_plugins::FailReason::Exited`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessExit {
    /// Exit code, if it exited normally.
    pub code: Option<i32>,
    /// Terminating signal, if it was killed (SIGSEGV = 11, SIGABRT = 6, SIGKILL = 9). Windows: the
    /// NTSTATUS of a crash as `i32` (e.g. `0xC000_0005_u32 as i32`), or 9 after
    /// [`ProcessWatcher::kill`] (module docs).
    pub signal: Option<i32>,
}

/// A watched child process. Owned by the plugin manager (UI thread).
#[derive(Debug)]
pub struct ProcessWatcher {
    /// Linux: the pidfd. macOS: the kqueue watching the pid.
    #[cfg(unix)]
    fd: OwnedFd,
    #[cfg(unix)]
    pid: Pid,
    /// Windows: the process handle.
    #[cfg(windows)]
    process: OwnedHandle,
    #[cfg(windows)]
    pid: u32,
    /// Windows: [`Self::kill`] terminated the process (its exit then reads as signal 9).
    #[cfg(windows)]
    killed: AtomicBool,
    exit: Option<ProcessExit>,
}

impl ProcessWatcher {
    /// Watches child `pid` (e.g. `std::process::Child::id`). Call right after spawning: the
    /// process must be a child of this process that nobody has reaped yet (an exited, unreaped
    /// child is fine, its pid cannot be reused). Windows: while the `Child` is still alive (its
    /// handle keeps the pid from being reused).
    pub fn new(pid: u32) -> io::Result<ProcessWatcher> {
        #[cfg(unix)]
        {
            let raw =
                i32::try_from(pid).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            let pid = Pid::from_raw(raw).ok_or(io::ErrorKind::InvalidInput)?;
            let (fd, exit) = sys::watch(pid)?;
            Ok(ProcessWatcher { fd, pid, exit })
        }
        #[cfg(windows)]
        {
            Ok(ProcessWatcher {
                process: sys::open(pid)?,
                pid,
                killed: AtomicBool::new(false),
                exit: None,
            })
        }
    }

    /// The process id.
    pub fn pid(&self) -> u32 {
        #[cfg(unix)]
        {
            self.pid.as_raw_pid() as u32
        }
        #[cfg(windows)]
        {
            self.pid
        }
    }

    /// Non-blocking: `Some` once the process has exited. The first detection reaps it; later
    /// calls return the same value.
    pub fn try_exit(&mut self) -> io::Result<Option<ProcessExit>> {
        if self.exit.is_none() {
            #[cfg(unix)]
            {
                self.exit = sys::try_reap(self.fd.as_fd(), self.pid)?;
            }
            #[cfg(windows)]
            {
                let killed = self.killed.load(Ordering::Acquire);
                self.exit = sys::try_exit(self.process.as_handle(), killed)?;
            }
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
            #[cfg(unix)]
            {
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
            #[cfg(windows)]
            sys::wait(self.process.as_handle(), remaining)?;
        }
    }

    /// Sends SIGKILL (Windows: `TerminateProcess`). A no-op once the exit was observed; `Ok` if
    /// the process is already gone.
    pub fn kill(&self) -> io::Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        #[cfg(unix)]
        match sys::kill(self.fd.as_fd(), self.pid) {
            Ok(()) | Err(Errno::SRCH) => Ok(()),
            Err(e) => Err(e.into()),
        }
        #[cfg(windows)]
        {
            if sys::kill(self.process.as_handle())? {
                self.killed.store(true, Ordering::Release);
            }
            Ok(())
        }
    }
}

#[cfg(unix)]
impl AsFd for ProcessWatcher {
    /// For `poll()`: readable once the process has exited (the pidfd on Linux, the kqueue on
    /// macOS).
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(windows)]
impl AsHandle for ProcessWatcher {
    /// The process handle: signalled once the process has exited (for `WaitForMultipleObjects`),
    /// and a target for `DuplicateHandle` (`PROCESS_DUP_HANDLE`, for `Channel::send_region`).
    fn as_handle(&self) -> BorrowedHandle<'_> {
        self.process.as_handle()
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

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_DUP_HANDLE,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        TerminateProcess, WaitForSingleObject,
    };

    use super::ProcessExit;

    /// The exit code [`kill`] gives the process: 128 + SIGKILL, what a Unix shell reports for a
    /// killed process.
    pub(super) const KILL_EXIT_CODE: u32 = 137;

    /// NTSTATUS severity "error" (the two top bits): an unhandled exception or a fast fail.
    const NTSTATUS_ERROR: u32 = 0xC000_0000;

    /// A handle to process `pid`: wait for it, read its exit code, terminate it, and duplicate
    /// handles into it.
    pub(super) fn open(pid: u32) -> io::Result<OwnedHandle> {
        let access = PROCESS_SYNCHRONIZE
            | PROCESS_QUERY_LIMITED_INFORMATION
            | PROCESS_TERMINATE
            | PROCESS_DUP_HANDLE;
        // SAFETY: plain FFI call; the handle is not inheritable.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle that nothing else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }

    /// Waits until the process has exited or `timeout` passed (rounded up to whole ms; this is
    /// not an RT path). A signalled process ends the wait at once, whatever the clock tick.
    pub(super) fn wait(process: BorrowedHandle<'_>, timeout: Option<Duration>) -> io::Result<()> {
        let ms = timeout.map_or(INFINITE, |t| {
            t.as_nanos()
                .div_ceil(1_000_000)
                .min(u128::from(INFINITE - 1)) as u32
        });
        // SAFETY: a valid process handle.
        match unsafe { WaitForSingleObject(process.as_raw_handle(), ms) } {
            WAIT_OBJECT_0 | WAIT_TIMEOUT => Ok(()),
            _ => Err(io::Error::last_os_error()),
        }
    }

    /// The exit once the process has ended (`killed`: [`kill`] terminated it).
    pub(super) fn try_exit(
        process: BorrowedHandle<'_>,
        killed: bool,
    ) -> io::Result<Option<ProcessExit>> {
        // SAFETY: a valid process handle; a zero timeout only polls.
        match unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } {
            WAIT_TIMEOUT => return Ok(None),
            WAIT_OBJECT_0 => {}
            _ => return Err(io::Error::last_os_error()),
        }
        let mut code = 0u32;
        // SAFETY: a valid process handle and an out pointer.
        if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Some(exit_from_code(code, killed)))
    }

    /// Maps a Windows exit code to [`ProcessExit`] (module docs).
    pub(super) fn exit_from_code(code: u32, killed: bool) -> ProcessExit {
        if killed && code == KILL_EXIT_CODE {
            ProcessExit {
                code: None,
                signal: Some(9),
            }
        } else if code & NTSTATUS_ERROR == NTSTATUS_ERROR {
            ProcessExit {
                code: None,
                signal: Some(code as i32),
            }
        } else {
            ProcessExit {
                code: Some(code as i32),
                signal: None,
            }
        }
    }

    /// `TerminateProcess` with [`KILL_EXIT_CODE`]. `Ok(true)` if it terminated the process,
    /// `Ok(false)` if the process had already exited.
    pub(super) fn kill(process: BorrowedHandle<'_>) -> io::Result<bool> {
        // SAFETY: a valid process handle with PROCESS_TERMINATE.
        if unsafe { TerminateProcess(process.as_raw_handle(), KILL_EXIT_CODE) } != 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        // An exited process refuses termination (ERROR_ACCESS_DENIED): it is gone, fine.
        // SAFETY: a valid process handle; a zero timeout only polls.
        if unsafe { WaitForSingleObject(process.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
compile_error!("komponist-ipc: no process-watcher backend (process.rs) for this OS");

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    /// A child to watch.
    #[derive(Clone, Copy)]
    enum Kid {
        /// Exits at once with this code.
        Exit(i32),
        /// Sleeps 30 s (to be killed).
        Sleep,
        /// Windows: reads an unmapped address (access violation).
        #[cfg(windows)]
        Segv,
        /// Windows: `std::process::abort` (a fast fail).
        #[cfg(windows)]
        Abort,
    }

    #[cfg(unix)]
    fn spawn(kid: Kid) -> Child {
        match kid {
            Kid::Exit(code) => Command::new("sh")
                .args(["-c", &format!("exit {code}")])
                .spawn(),
            Kid::Sleep => Command::new("sleep").arg("30").spawn(),
        }
        .unwrap()
    }

    /// Windows: what [`child_entry`] does in a child (this test binary, run again).
    #[cfg(windows)]
    const KID_ENV: &str = "KOMPONIST_IPC_TEST_KID";

    /// Windows has no `sh` or `sleep` to rely on: the child is this test binary running only
    /// [`child_entry`].
    #[cfg(windows)]
    fn spawn(kid: Kid) -> Child {
        let what = match kid {
            Kid::Exit(code) => format!("exit:{code}"),
            Kid::Sleep => "sleep".into(),
            Kid::Segv => "segv".into(),
            Kid::Abort => "abort".into(),
        };
        Command::new(std::env::current_exe().unwrap())
            .args(["process::tests::child_entry", "--exact", "--test-threads=1"])
            .env(KID_ENV, what)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap()
    }

    /// The child side on Windows; does nothing in a normal test run.
    #[test]
    #[cfg(windows)]
    fn child_entry() {
        use windows_sys::Win32::System::Diagnostics::Debug::{
            SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
        };
        let Ok(what) = std::env::var(KID_ENV) else {
            return;
        };
        // No Windows Error Reporting dialog for the crashes below (this process only).
        // SAFETY: plain FFI call.
        unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
        match what.as_str() {
            "sleep" => std::thread::sleep(Duration::from_secs(30)),
            // SAFETY: none, on purpose: address 8 is never mapped, so the read faults.
            "segv" => unsafe {
                core::ptr::read_volatile(core::ptr::without_provenance::<u8>(8));
            },
            "abort" => std::process::abort(),
            exit => std::process::exit(exit["exit:".len()..].parse().unwrap()),
        }
        std::process::exit(0);
    }

    #[test]
    fn exit_code_is_reported() {
        let child = spawn(Kid::Exit(3));
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
        let child = spawn(Kid::Sleep);
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
    #[cfg(unix)]
    fn readable(watcher: &ProcessWatcher, timeout: Duration) -> bool {
        let ts = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(watcher, PollFlags::IN)];
        poll(&mut fds, Some(&ts)).unwrap() == 1
    }

    /// Windows: whether the watcher's process handle is signalled within `timeout`.
    #[cfg(windows)]
    fn readable(watcher: &ProcessWatcher, timeout: Duration) -> bool {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
        use windows_sys::Win32::System::Threading::WaitForSingleObject;
        let ms = timeout.as_millis() as u32;
        // SAFETY: a valid process handle.
        unsafe { WaitForSingleObject(watcher.as_handle().as_raw_handle(), ms) == WAIT_OBJECT_0 }
    }

    #[test]
    fn the_fd_turns_readable_on_exit_and_stays_so() {
        let child = spawn(Kid::Sleep);
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
        let child = spawn(Kid::Exit(5));
        // Give the child ample time to exit, so it is a zombie (exited, not reaped) before it is
        // watched (macOS: EVFILT_PROC does not attach to a zombie, the watcher falls back).
        // Windows: `child` still holds a handle, so the pid is still the child's.
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
        // pid 1 is not our child: pidfd_open works, waitid refuses (ECHILD). (Windows has no
        // reaping, so any process could be watched there.)
        #[cfg(unix)]
        if let Ok(mut watcher) = ProcessWatcher::new(1) {
            assert!(watcher.try_exit().is_err());
        }
        assert!(ProcessWatcher::new(0).is_err());
        assert!(ProcessWatcher::new(u32::MAX).is_err());
    }

    /// Windows: an unhandled access violation and a fast fail (`abort`) count as crashes, with
    /// their NTSTATUS in `signal`.
    #[test]
    #[cfg(windows)]
    fn crashes_are_reported_with_their_ntstatus() {
        for (kid, status) in [(Kid::Segv, 0xC000_0005_u32), (Kid::Abort, 0xC000_0409)] {
            let child = spawn(kid);
            let mut watcher = ProcessWatcher::new(child.id()).unwrap();
            drop(child);
            let exit = watcher.wait_exit(Some(Duration::from_secs(20))).unwrap();
            assert_eq!(
                exit,
                Some(ProcessExit {
                    code: None,
                    signal: Some(status as i32),
                }),
                "{status:#x}"
            );
        }
    }

    #[test]
    #[cfg(windows)]
    fn windows_exit_codes_map_to_code_or_signal() {
        use sys::{KILL_EXIT_CODE, exit_from_code};
        let code = |c: i32| ProcessExit {
            code: Some(c),
            signal: None,
        };
        let signal = |s: u32| ProcessExit {
            code: None,
            signal: Some(s as i32),
        };
        assert_eq!(exit_from_code(0, false), code(0));
        assert_eq!(exit_from_code(3, false), code(3));
        assert_eq!(
            exit_from_code(0x8000_0003, false),
            code(0x8000_0003_u32 as i32)
        );
        for status in [0xC000_0005, 0xC000_0409, 0xC000_001D, 0xC000_00FD] {
            assert_eq!(exit_from_code(status, false), signal(status));
            assert_eq!(exit_from_code(status, true), signal(status));
        }
        // Killed by the watcher: SIGKILL, as on Unix; the same code without `kill` is a code.
        assert_eq!(exit_from_code(KILL_EXIT_CODE, true), signal(9));
        assert_eq!(exit_from_code(KILL_EXIT_CODE, false), code(137));
        // The process beat `kill` to it with its own exit code.
        assert_eq!(exit_from_code(1, true), code(1));
    }
}
