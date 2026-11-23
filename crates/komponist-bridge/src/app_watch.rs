//! macOS: the bridge ends with the app (#218).
//!
//! A bridge leaves its main loop when the app closes the control socket, but only while that loop
//! runs: one stuck in a plugin call (loading, opening the editor) would outlive a crashed app,
//! with its RT thread and locked shared memory. Linux and Windows end such a bridge from the app's
//! side (`PR_SET_PDEATHSIG`, a job object; `komponist_ipc::process`). macOS has neither, so the
//! bridge watches the app itself: the app names its pid in
//! [`PARENT_PID_ENV`](komponist_ipc::process::PARENT_PID_ENV), and a thread waits on a kqueue
//! `EVFILT_PROC` / `NOTE_EXIT` filter for it, then ends the process with `_exit` (no plugin code
//! runs after that; the app is gone, nothing is left to save).
//!
//! Started without that variable (a tool or a test running `komponist-bridge` directly), the
//! bridge watches nothing and ends on socket EOF only, as before.

use std::os::fd::OwnedFd;
use std::time::Duration;

use rustix::buffer::spare_capacity;
use rustix::event::kqueue::{Event, EventFilter, EventFlags, ProcessEvents, kevent, kqueue};
use rustix::io::Errno;
use rustix::process::Pid;

use crate::exit;

/// Starts watching the app named in the environment; ends the process at once if it is gone.
pub(crate) fn start() {
    let Some(app) = std::env::var(komponist_ipc::process::PARENT_PID_ENV)
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .and_then(Pid::from_raw)
    else {
        return;
    };
    let kq = match watch(app) {
        Ok(kq) => kq,
        // The app died before the bridge got here.
        Err(Errno::SRCH) => gone(),
        Err(e) => {
            eprintln!("komponist-bridge: cannot watch the app (pid {app:?}): {e}");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name("kp-app-watch".into())
        .spawn(move || wait(&kq));
    if let Err(e) = spawned {
        eprintln!("komponist-bridge: cannot watch the app: {e}");
    }
}

/// A kqueue with an exit filter on `app`.
fn watch(app: Pid) -> Result<OwnedFd, Errno> {
    let kq = kqueue()?;
    let filter = EventFilter::Proc {
        pid: app,
        flags: ProcessEvents::EXIT,
    };
    let change = Event::new(filter, EventFlags::ADD, core::ptr::null_mut());
    // SAFETY: the change refers to a pid, not to a file descriptor; the zero-capacity event list
    // makes this a pure registration that returns at once.
    unsafe {
        kevent::<_, &mut Vec<Event>>(&kq, &[change], &mut Vec::new(), Some(Duration::ZERO))?;
    }
    Ok(kq)
}

/// Blocks until the app has exited, then ends the bridge.
fn wait(kq: &OwnedFd) -> ! {
    let mut events: Vec<Event> = Vec::with_capacity(1);
    loop {
        events.clear();
        // SAFETY: the only registered filter refers to a pid.
        // The spare capacity is the event list (a `&mut Vec` would be its length, zero here,
        // and make `kevent` return at once).
        match unsafe { kevent(kq, &[], spare_capacity(&mut events), None) } {
            Ok(n) if n > 0 => gone(),
            // EINTR, or a spurious wake-up.
            Ok(_) | Err(Errno::INTR) => {}
            Err(e) => {
                eprintln!("komponist-bridge: watching the app failed: {e}");
                // Park for good: without the watch the bridge still ends on socket EOF.
                loop {
                    std::thread::park();
                }
            }
        }
    }
}

/// The app is gone: end now, whatever the main thread is doing.
fn gone() -> ! {
    // SAFETY: ends the process without running exit handlers or plugin code; the app that would
    // need anything from this bridge is gone.
    unsafe { libc::_exit(i32::from(exit::FAILURE)) }
}
