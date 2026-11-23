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
//! Dropping a watcher never blocks and never leaves a zombie (#218): a child that has not been
//! reaped yet goes to a reaper thread, which reaps it once it has exited.
//!
//! **Children that die with the app** (#218): [`spawn_tied`] starts a child that does not outlive
//! this process, even when the app crashes or is killed while the child is stuck in a plugin call
//! (and so never reads the end of its control socket). Per OS:
//!
//! - Linux: `PR_SET_PDEATHSIG(SIGKILL)` between fork and exec, then a `getppid` check against this
//!   process (the app may have died before the `prctl`). The kernel sends the signal when the
//!   *thread* that spawned the child ends: spawn from a thread that lives as long as the child
//!   should (the plugin manager's UI thread; a scan worker waits for its helper).
//! - macOS: there is no such kernel feature. The child learns this process's pid from
//!   [`PARENT_PID_ENV`] and watches it itself (`komponist-bridge`: kqueue `EVFILT_PROC` /
//!   `NOTE_EXIT`, then `_exit`). Spawns also hold the spawn lock that socket pairs are created
//!   under (`control`), since macOS has no atomic close-on-exec for them.
//! - Windows: the child goes into a job object of the app with `KILL_ON_JOB_CLOSE`; the job's
//!   handle is never closed, so the system closes it, and ends the child, when the app ends,
//!   whichever way. The job allows silent breakaway: processes the child starts are not tied,
//!   as on Linux and macOS. The child is created suspended, joins the job and only then runs, so
//!   it cannot run (or start anything) outside it. Stable Rust's `Command` cannot pass a job list
//!   to `CreateProcess` (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), which would close the last gap: an app
//!   that dies between creating the child and putting it into the job leaves the child suspended,
//!   never running a line of its own or plugin code, until someone ends it.
//!
//! [`ProcessGroup::spawn`] does the same and puts the child into a group of its own with
//! everything it starts (Unix: a process group, Windows: a job object of its own, nested in the
//! app's job), so a scan helper that times out is killed with the helper processes its plugin
//! started. The group is killed only on request: what a finished child leaves running (a
//! background service a plugin started) lives on. One difference on Windows: the nested job
//! allows no breakaway (or the group could not be killed whole), so such leftovers stay in the
//! app's job too and end with the app; on Unix they outlive it.
//!
//! Non-RT: the engine never touches processes; a dead bridge just makes its blocks late.

use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::{AsHandle, BorrowedHandle, OwnedHandle};
use std::process::{Child, Command};
#[cfg(windows)]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "macos")]
use std::sync::{PoisonError, RwLock};
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

impl Drop for ProcessWatcher {
    /// Hands a child that has not been reaped yet to the reaper thread (module docs). Windows has
    /// nothing to reap: closing the handle is all.
    fn drop(&mut self) {
        #[cfg(unix)]
        if matches!(self.try_exit(), Ok(None)) {
            reaper::adopt(self.pid);
        }
    }
}

/// The environment variable that tells a child started by [`spawn_tied`] the pid of the process
/// it must not outlive (macOS, module docs).
pub const PARENT_PID_ENV: &str = "KOMPONIST_PARENT_PID";

/// macOS: socket pairs are created (and marked close-on-exec) under the write side, children are
/// spawned under the read side, so no child inherits a socket of another bridge (module docs).
#[cfg(target_os = "macos")]
pub(crate) static SPAWN_LOCK: RwLock<()> = RwLock::new(());

/// Starts `command` as a child that does not outlive this process (module docs). Watch it with a
/// [`ProcessWatcher`] and drop the `Child` without waiting, as for any other child.
///
/// Call it from a thread that lives as long as the child should: on Linux the child is killed
/// when the spawning *thread* ends (`PR_SET_PDEATHSIG`). Today that is the UI thread, which owns
/// the plugin manager, and a scan worker, which outlives its helper.
pub fn spawn_tied(command: &mut Command) -> io::Result<Child> {
    #[cfg(target_os = "linux")]
    {
        let app = rustix::process::getpid();
        // SAFETY: runs between fork and exec: two async-signal-safe syscalls and no allocation
        // (an `io::Error` made from an errno holds the number only).
        unsafe {
            command.pre_exec(move || {
                rustix::process::set_parent_process_death_signal(Some(
                    rustix::process::Signal::KILL,
                ))?;
                // The app died before the `prctl`: the signal will never come.
                if rustix::process::getppid() != Some(app) {
                    return Err(rustix::io::Errno::SRCH.into());
                }
                Ok(())
            });
        }
        command.spawn()
    }
    #[cfg(target_os = "macos")]
    {
        command.env(PARENT_PID_ENV, std::process::id().to_string());
        let _spawning = SPAWN_LOCK.read().unwrap_or_else(PoisonError::into_inner);
        command.spawn()
    }
    #[cfg(windows)]
    {
        // No app job (it could not be made): the child runs untied, as before #218. A job that
        // exists but cannot take the child fails the spawn (`spawn_in`).
        let app = job::app();
        job::spawn_in(command, app.as_slice())
    }
}

/// A child and everything it starts, killed together (module docs).
#[derive(Debug)]
pub struct ProcessGroup {
    /// Unix: the group leader, whose pid is the group id.
    #[cfg(unix)]
    leader: Pid,
    /// Windows: the group's job object, nested in the app's job.
    #[cfg(windows)]
    job: OwnedHandle,
}

impl ProcessGroup {
    /// Starts `command` as [`spawn_tied`] does, as the first member of a new group: on Unix the
    /// leader of a process group of its own, on Windows the first process of a job object of its
    /// own, which nests in the app's job (the child is in that one already; module docs).
    pub fn spawn(command: &mut Command) -> io::Result<(Child, ProcessGroup)> {
        #[cfg(unix)]
        {
            command.process_group(0);
            let child = spawn_tied(command)?;
            let raw = i32::try_from(child.id()).map_err(|_| io::ErrorKind::InvalidInput)?;
            let leader = Pid::from_raw(raw).ok_or(io::ErrorKind::InvalidInput)?;
            Ok((child, ProcessGroup { leader }))
        }
        #[cfg(windows)]
        {
            // No limits: no breakaway (everything the child starts stays in the group), and
            // closing the handle kills nothing. The child joins the app's job first, then this
            // one, which so nests in it; both before it runs (see `spawn_tied`).
            let group = job::create(0)?;
            let mut jobs = job::app().into_iter().collect::<Vec<_>>();
            jobs.push(group.as_handle());
            let child = job::spawn_in(command, &jobs)?;
            Ok((child, ProcessGroup { job: group }))
        }
    }

    /// SIGKILL to every process in the group (Windows: `TerminateJobObject`); `leader` watches the
    /// group's first process. `Ok` if the group is already gone.
    ///
    /// Unix: the group id is the leader's pid, which may be reused once the leader is reaped. So
    /// once `leader` has reaped it (its exit is known), this does nothing: the rest of the group
    /// is out of reach then. Kill the group before the watcher can see the exit.
    pub fn kill(&self, leader: &ProcessWatcher) -> io::Result<()> {
        #[cfg(unix)]
        {
            debug_assert_eq!(leader.pid, self.leader, "not this group's leader");
            if leader.exit.is_some() {
                return Ok(());
            }
            match rustix::process::kill_process_group(self.leader, rustix::process::Signal::KILL) {
                Ok(()) | Err(Errno::SRCH) => Ok(()),
                Err(e) => Err(e.into()),
            }
        }
        #[cfg(windows)]
        {
            // A job has no id to reuse: it can be terminated whatever the leader did.
            let _ = leader;
            job::terminate(self.job.as_handle())
        }
    }
}

/// Reaps the children of dropped watchers (module docs): one thread for the process, started
/// with the first such child; it looks at its children every [`reaper::POLL`] and sleeps while it
/// has none.
#[cfg(unix)]
mod reaper {
    use std::sync::OnceLock;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
    use std::time::Duration;

    use rustix::io::Errno;
    use rustix::process::{Pid, WaitOptions, waitpid};

    /// How often the reaper looks at its children.
    pub(super) const POLL: Duration = Duration::from_millis(50);

    /// Hands `pid`, an unreaped child of this process that nobody else waits for, to the reaper.
    pub(super) fn adopt(pid: Pid) {
        static REAPER: OnceLock<Option<Sender<Pid>>> = OnceLock::new();
        let reaper = REAPER.get_or_init(|| {
            let (tx, rx) = mpsc::channel();
            std::thread::Builder::new()
                .name("kp-reaper".into())
                .spawn(move || run(&rx))
                .ok()
                .map(|_| tx)
        });
        // Without a reaper thread the child stays a zombie until the app ends.
        if let Some(reaper) = reaper {
            let _ = reaper.send(pid);
        }
    }

    fn run(rx: &Receiver<Pid>) {
        let mut children: Vec<Pid> = Vec::new();
        loop {
            let next = if children.is_empty() {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                rx.recv_timeout(POLL)
            };
            match next {
                Ok(pid) => children.push(pid),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            children.extend(rx.try_iter());
            // Keep the ones still running; an error means the child is not ours (any more).
            children.retain(|&pid| {
                matches!(
                    waitpid(Some(pid), WaitOptions::NOHANG),
                    Ok(None) | Err(Errno::INTR)
                )
            });
        }
    }
}

/// Windows: job objects for [`spawn_tied`] and [`ProcessGroup`].
#[cfg(windows)]
mod job {
    use std::io;
    use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle};
    use std::process::Child;
    use std::sync::OnceLock;

    use std::os::windows::process::CommandExt;
    use std::process::Command;

    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    };

    use windows_sys::Win32::System::Threading::{
        CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
    };

    use super::sys::KILL_EXIT_CODE;

    /// Spawns `command` suspended, puts it into `jobs` in order (a later one nests in the earlier
    /// ones) and only then lets it run, so it cannot run or start anything outside them (module
    /// docs). A job it cannot join fails the spawn: the suspended child is killed and reaped
    /// before it ran, so no child ever runs untied. Overrides the command's creation flags
    /// (nobody sets any).
    pub(super) fn spawn_in(
        command: &mut Command,
        jobs: &[BorrowedHandle<'_>],
    ) -> io::Result<Child> {
        command.creation_flags(CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        let joined = jobs.iter().try_for_each(|job| assign(*job, &child));
        if let Err(e) = joined.and_then(|()| resume(child.id())) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
        Ok(child)
    }

    /// Resumes the threads of process `pid`: a process created suspended has its main thread
    /// only (std keeps no handle to it, so it is found by a thread snapshot). The caller holds a
    /// handle to the process, so `pid` cannot have been reused. Resuming a running thread does
    /// nothing.
    fn resume(pid: u32) -> io::Result<()> {
        // SAFETY: plain FFI call; the snapshot is closed below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a plain-old-data struct; `dwSize` is set as the API requires.
        let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = false;
        let mut error = None;
        // SAFETY: a valid snapshot handle and an entry with `dwSize` set, alive across the calls.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: plain FFI calls; the thread handle is checked and closed.
                unsafe {
                    let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                    if thread.is_null() {
                        error = Some(io::Error::last_os_error());
                    } else {
                        if ResumeThread(thread) == u32::MAX {
                            error = Some(io::Error::last_os_error());
                        } else {
                            resumed = true;
                        }
                        CloseHandle(thread);
                    }
                }
            }
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        // SAFETY: the snapshot handle from above, closed once.
        unsafe { CloseHandle(snapshot) };
        match (resumed, error) {
            (true, _) => Ok(()),
            (false, Some(e)) => Err(e),
            (false, None) => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no thread of the new process to resume",
            )),
        }
    }

    /// A new unnamed job object with `limits`.
    pub(super) fn create(limits: JOB_OBJECT_LIMIT) -> io::Result<OwnedHandle> {
        // SAFETY: plain FFI call; default security, no name, the handle is not inheritable.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh handle that nothing else owns.
        let job = unsafe { OwnedHandle::from_raw_handle(job) };
        // SAFETY: a plain-old-data struct, all zero is "no limits".
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        info.BasicLimitInformation.LimitFlags = limits;
        // SAFETY: a valid job handle and a struct of the size given, matching the class.
        let set = unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts `child` into `job`.
    pub(super) fn assign(job: BorrowedHandle<'_>, child: &Child) -> io::Result<()> {
        // SAFETY: a valid job handle and the child's process handle (std opens it with all
        // access, which includes PROCESS_SET_QUOTA and PROCESS_TERMINATE).
        if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ends every process in `job` with the exit code of [`super::ProcessWatcher::kill`].
    pub(super) fn terminate(job: BorrowedHandle<'_>) -> io::Result<()> {
        // SAFETY: a valid job handle (created with all access).
        if unsafe { TerminateJobObject(job.as_raw_handle(), KILL_EXIT_CODE) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// The app's job (module docs), made on first use; `None` if it could not be made. Never
    /// closed: the system closes it when the process ends.
    pub(super) fn app() -> Option<BorrowedHandle<'static>> {
        static APP: OnceLock<Option<OwnedHandle>> = OnceLock::new();
        APP.get_or_init(|| {
            create(JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK).ok()
        })
        .as_ref()
        .map(OwnedHandle::as_handle)
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
    #[cfg(unix)]
    use rustix::process::Signal;
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

    /// Whether `pid` still runs (Unix): not a zombie, which `kill(pid, 0)` would count. A
    /// reparented zombie may wait for an init that does not reap (a container's).
    #[cfg(unix)]
    fn running(pid: u32) -> bool {
        #[cfg(target_os = "linux")]
        {
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                return false;
            };
            // "pid (comm) state …"
            let state = stat
                .rfind(')')
                .and_then(|i| stat[i + 1..].trim_start().chars().next());
            state.is_some_and(|s| s != 'Z' && s != 'X')
        }
        #[cfg(target_os = "macos")]
        {
            let out = Command::new("ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("run ps");
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            !stat.is_empty() && !stat.starts_with('Z')
        }
    }

    /// Whether `pid` exists at all (Unix; a zombie counts, it is not reaped yet).
    #[cfg(unix)]
    fn exists(pid: u32) -> bool {
        let pid = Pid::from_raw(pid as i32).unwrap();
        rustix::process::test_kill_process(pid).is_ok()
    }

    /// Waits up to 10 s for `done`.
    #[cfg(unix)]
    fn eventually(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// #218: dropping a watcher does not wait, and its child is reaped later all the same, killed
    /// just before or still running.
    #[test]
    #[cfg(unix)]
    fn a_dropped_watcher_leaves_no_zombie() {
        let child = spawn(Kid::Sleep);
        let pid = child.id();
        let watcher = ProcessWatcher::new(pid).unwrap();
        drop(child);
        watcher.kill().unwrap();
        let t0 = Instant::now();
        drop(watcher);
        assert!(
            t0.elapsed() < Duration::from_millis(100),
            "{:?}",
            t0.elapsed()
        );
        eventually("the killed child is reaped", || !exists(pid));

        // Still running when the watcher goes: reaped once it ends.
        let child = spawn(Kid::Sleep);
        let pid = child.id();
        drop(ProcessWatcher::new(pid).unwrap());
        drop(child);
        assert!(running(pid));
        // Ours and not reaped, so the pid cannot have been reused.
        rustix::process::kill_process(Pid::from_raw(pid as i32).unwrap(), Signal::KILL).unwrap();
        eventually("the running child is reaped", || !exists(pid));
    }

    /// #218, Linux: a tied child gets SIGKILL when the thread that spawned it ends (the kernel's
    /// "parent" is that thread), as it does when the app dies.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_tied_child_is_killed_when_its_spawner_ends() {
        let mut watcher = std::thread::spawn(|| {
            let child = spawn_tied(Command::new("sleep").arg("30")).unwrap();
            let watcher = ProcessWatcher::new(child.id()).unwrap();
            drop(child);
            watcher
        })
        .join()
        .unwrap();
        let exit = watcher.wait_exit(Some(Duration::from_secs(10))).unwrap();
        assert_eq!(exit.and_then(|e| e.signal), Some(9), "{exit:?}");
    }

    /// #218, macOS: a tied child is told whom to watch.
    #[test]
    #[cfg(target_os = "macos")]
    fn a_tied_child_learns_the_app_pid() {
        let mut command = Command::new("sh");
        command
            .args(["-c", &format!("echo ${PARENT_PID_ENV}")])
            .stdout(std::process::Stdio::piped());
        let out = spawn_tied(&mut command)
            .unwrap()
            .wait_with_output()
            .unwrap();
        let pid = String::from_utf8_lossy(&out.stdout);
        assert_eq!(pid.trim(), std::process::id().to_string());
    }

    /// #218, Windows: a tied child is in the app's job.
    #[test]
    #[cfg(windows)]
    fn a_tied_child_is_in_the_app_job() {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::IsProcessInJob;
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["process::tests::child_entry", "--exact", "--test-threads=1"])
            .env(KID_ENV, "sleep")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = spawn_tied(&mut command).unwrap();
        let mut member = 0;
        // SAFETY: valid process and job handles and an out pointer.
        let ok = unsafe {
            IsProcessInJob(
                child.as_raw_handle(),
                job::app().unwrap().as_raw_handle(),
                &mut member,
            )
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(ok != 0 && member != 0);
    }

    /// #218: killing a group ends the processes its leader started too.
    #[test]
    #[cfg(unix)]
    fn a_group_is_killed_with_what_its_leader_started() {
        use std::io::BufRead;
        let mut command = Command::new("sh");
        command
            .args(["-c", "sleep 30 & echo $!; wait"])
            .stdout(std::process::Stdio::piped());
        let (mut child, group) = ProcessGroup::spawn(&mut command).unwrap();
        let mut watcher = ProcessWatcher::new(child.id()).unwrap();
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        drop(child);
        let helper: u32 = line.trim().parse().unwrap();
        assert!(running(helper));
        group.kill(&watcher).unwrap();
        let exit = watcher.wait_exit(Some(Duration::from_secs(10))).unwrap();
        assert_eq!(exit.and_then(|e| e.signal), Some(9), "{exit:?}");
        eventually("the helper is killed with its group", || !running(helper));
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
