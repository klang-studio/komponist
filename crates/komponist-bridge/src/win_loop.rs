//! The bridge main loop on Windows (ports W5, W6): [`WinEventLoop`] (the plugin's [`HostLoop`]
//! and the wake relay) and [`ControlReader`] (the control pipe, read on a thread of its own).
//!
//! Windows plugins need a message pump on their main thread even without a visible GUI: they
//! create hidden windows, `SetTimer` timers and COM objects there and post work to them. So the
//! main thread does nothing but pump window messages ([`WinEventLoop::run`], a
//! `MsgWaitForMultipleObjectsEx(QS_ALLINPUT)` through `komponist_hosting::host_loop::wake::wait`),
//! and the bridge's work reaches it as a window message of its own.
//!
//! # The wake relay (port W6)
//!
//! A relay thread ([`WinEventLoop::start_relay`]) waits for
//!
//! - the control reader's event (a manual-reset event, set while control messages are queued),
//! - the notification event of [`crate::notify::BridgeNotify`] (set by plugin threads and the RT
//!   thread with one `SetEvent`, never blocking),
//! - a high-resolution waitable timer armed for the next due host timer or deadline (a
//!   millisecond timeout would end on the 15.6 ms clock tick),
//!
//! and posts one `WM_APP` message to a message-only window of the main thread. Its window
//! procedure calls the dispatcher the bridge installed ([`WinEventLoop::set_dispatch`]), which
//! handles timers, notifications and control messages outside every borrow of this loop, then
//! arms the timer for the next deadline and lets the relay wait again ([`WinEventLoop::rearm`]).
//! The relay waits for that hand-back before it looks at the events again, so an event that stays
//! set until the main thread drains it never makes it spin (the role of the suspended dispatch
//! sources on macOS, `cf_loop.rs`).
//!
//! **Why a window message:** whoever pumps the main thread's messages dispatches it, not only this
//! loop: eframe's (winit's) message loop while the bridge's plugin frame is open (`frame`), the
//! window manager's move and size loop while the user drags the frame, and a plugin's own modal
//! loop (a file dialog or message box opened from its GUI). Control messages, notifications and
//! timers keep being served in all of them, as the common modes do on macOS. A wake that finds the
//! bridge busy (a modal loop inside a plugin call the bridge made, such as `gui_open`) is retried
//! with a 5 ms `SetTimer` until the call returns: messages then wait, as during a long plugin call
//! on Linux.
//!
//! **Why a reader thread:** a pipe handle cannot be waited on for "readable", and the channel's
//! overlapped reads are internal to `komponist-ipc`. The reader blocks in `Channel::recv` and
//! queues whole messages (at most [`QUEUE_CAPACITY`]; when the main thread falls behind, the
//! reader stops reading and the app's writer blocks, as with a full socket on Linux). The main
//! thread sends on the same channel at the same time; the overlapped pipe allows that.
//!
//! There is no fd registry: CLAP posix-fd-support and VST3 `IRunLoop` are not offered on Windows.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle, BorrowedHandle, OwnedHandle};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::wake::{self, DeadlineTimer, WakeEvent};
use komponist_hosting::{HostLoop, HostingError, TimerId};
use komponist_ipc::{BridgeChannel, ChannelError, ControlMsg};
use windows_sys::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WAIT_FAILED, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, HWND_MESSAGE, KillTimer, PostMessageW,
    RegisterClassExW, SetTimer, WM_APP, WM_TIMER, WNDCLASSEXW,
};

/// Control messages the reader queues before it waits for the main thread.
pub const QUEUE_CAPACITY: usize = 256;

/// The relay's wake message to the main thread's wake window.
const WM_WAKE: u32 = WM_APP + 0x4B50;

/// The retry timer of a wake that found the bridge busy.
const RETRY_TIMER: usize = 1;

/// How often a busy bridge is tried again.
const RETRY: Duration = Duration::from_millis(5);

/// Longest the relay sleeps without a deadline (the bridge looks around once a second, as the W5
/// loop did).
const IDLE: Duration = Duration::from_secs(1);

#[derive(Debug)]
struct TimerEntry {
    id: TimerId,
    period: Duration,
    next: Instant,
}

#[derive(Debug, Default)]
struct LoopState {
    next_timer: u32,
    timers: Vec<TimerEntry>,
}

/// What the wake window calls on the main thread: `false` if the bridge was busy (the wake is
/// retried, module docs).
pub type Dispatch = Rc<dyn Fn() -> bool>;

thread_local! {
    /// The dispatcher of this thread's loop, for the wake window's procedure.
    static DISPATCH: RefCell<Option<Dispatch>> = const { RefCell::new(None) };
}

/// See the module docs. Main thread only; shared with the plugin as `Rc<dyn HostLoop>`.
pub struct WinEventLoop {
    state: RefCell<LoopState>,
    timer: DeadlineTimer,
    relay: RefCell<Option<Relay>>,
    stopped: Cell<bool>,
}

impl std::fmt::Debug for WinEventLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WinEventLoop")
            .field("state", &self.state)
            .field("relay", &self.relay.borrow().is_some())
            .finish()
    }
}

impl WinEventLoop {
    /// A loop with its deadline timer and without a relay yet.
    pub fn new() -> io::Result<Rc<WinEventLoop>> {
        Ok(Rc::new(WinEventLoop {
            state: RefCell::new(LoopState::default()),
            timer: DeadlineTimer::new()?,
            relay: RefCell::new(None),
            stopped: Cell::new(false),
        }))
    }

    /// Number of registered timers.
    #[cfg(test)]
    pub fn timer_count(&self) -> usize {
        self.state.borrow().timers.len()
    }

    /// Whether `timer` is still registered.
    pub fn timer_registered(&self, timer: TimerId) -> bool {
        self.state.borrow().timers.iter().any(|t| t.id == timer)
    }

    /// One wait over `extra` until one is signalled, a window message arrives (it is dispatched),
    /// `max_wait` passes, or a timer is due; returns the due timers ([`Self::take_due`]). The
    /// bridge waits through the relay; this is the loop's own wait, for tests.
    #[cfg(test)]
    pub fn wait(&self, extra: &[BorrowedHandle<'_>], max_wait: Duration) -> Vec<TimerId> {
        let limit = Instant::now() + max_wait;
        let deadline = self.next_timer().map_or(limit, |t| t.min(limit));
        // No borrow is held here: plugin window procedures run inside.
        wake::wait(&self.timer, extra, Some(deadline));
        self.take_due()
    }

    /// When the next timer is due.
    pub fn next_timer(&self) -> Option<Instant> {
        self.state.borrow().timers.iter().map(|t| t.next).min()
    }

    /// The timers that are due now; each moves on by its period (missed periods are skipped
    /// instead of firing a burst).
    pub fn take_due(&self) -> Vec<TimerId> {
        let now = Instant::now();
        let mut state = self.state.borrow_mut();
        let mut due = Vec::new();
        for timer in state.timers.iter_mut().filter(|t| t.next <= now) {
            timer.next += timer.period;
            if timer.next <= now {
                timer.next = now + timer.period;
            }
            due.push(timer.id);
        }
        due
    }

    /// Starts the relay thread over `events` (the control reader's and the notification event;
    /// duplicated for the thread) and creates the wake window (module docs).
    pub fn start_relay(&self, events: &[BorrowedHandle<'_>]) -> io::Result<()> {
        let events = events
            .iter()
            .map(|e| e.try_clone_to_owned())
            .collect::<io::Result<Vec<OwnedHandle>>>()?;
        let shared = Arc::new(RelayShared {
            deadline: DeadlineTimer::new()?,
            go: WakeEvent::new()?,
            quit: WakeEvent::new()?,
        });
        let window = wake_window()?;
        let thread = shared.clone();
        let target = window as isize;
        let spawned = std::thread::Builder::new()
            .name("kp-bridge-relay".into())
            .spawn(move || relay_loop(&events, &thread, target));
        if let Err(e) = spawned {
            // SAFETY: the window this call created.
            unsafe { DestroyWindow(window) };
            return Err(e);
        }
        *self.relay.borrow_mut() = Some(Relay { window, shared });
        Ok(())
    }

    /// Installs (or removes) the dispatcher the wake window calls (module docs).
    pub fn set_dispatch(&self, dispatch: Option<Dispatch>) {
        DISPATCH.with(|d| *d.borrow_mut() = dispatch);
    }

    /// After a dispatch: arms the relay's timer for `due` (or [`IDLE`] from now) and lets the
    /// relay wait again.
    pub fn rearm(&self, due: Option<Instant>) {
        if let Some(relay) = self.relay.borrow().as_ref() {
            relay
                .shared
                .deadline
                .arm(due.unwrap_or_else(|| Instant::now() + IDLE));
            relay.shared.go.signal();
        }
    }

    /// Pumps the thread's window messages until [`Self::stop`]; the bridge's work runs inside,
    /// from the wake window.
    pub fn run(&self) {
        while !self.stopped.replace(false) {
            wake::wait(&self.timer, &[], None);
        }
    }

    /// Makes [`Self::run`] return after the message it is dispatching (also when it is not
    /// running: then the next `run` returns at once).
    pub fn stop(&self) {
        self.stopped.set(true);
    }
}

/// The relay's half on the main thread.
struct Relay {
    window: HWND,
    shared: Arc<RelayShared>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.shared.quit.signal();
        // SAFETY: the wake window this thread created.
        unsafe { DestroyWindow(self.window) };
    }
}

/// What the relay thread and the main thread share.
struct RelayShared {
    /// Armed by the main thread for its next deadline.
    deadline: DeadlineTimer,
    /// Signalled by the main thread when it handled a wake.
    go: WakeEvent,
    /// Ends the relay thread.
    quit: WakeEvent,
}

/// The relay thread (module docs): waits for `events` or the deadline, posts the wake, waits for
/// the hand-back.
fn relay_loop(events: &[OwnedHandle], shared: &RelayShared, window: isize) {
    let mut watched: Vec<HANDLE> = vec![shared.quit.as_handle().as_raw_handle()];
    watched.extend(events.iter().map(|e| e.as_raw_handle()));
    watched.push(shared.deadline.as_handle().as_raw_handle());
    let back = [
        shared.quit.as_handle().as_raw_handle(),
        shared.go.as_handle().as_raw_handle(),
    ];
    loop {
        // SAFETY: handles that live as long as this thread (owned by it or by `shared`).
        let woke =
            unsafe { WaitForMultipleObjects(watched.len() as u32, watched.as_ptr(), 0, INFINITE) };
        if woke == 0 {
            return;
        }
        if woke == WAIT_FAILED {
            // A broken handle: do not spin.
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        // SAFETY: posting to a window of another thread; fails once the window is gone.
        if unsafe { PostMessageW(window as HWND, WM_WAKE, 0, 0) } == 0 {
            return;
        }
        // SAFETY: as above.
        let woke = unsafe { WaitForMultipleObjects(2, back.as_ptr(), 0, INFINITE) };
        if woke == 0 {
            return;
        }
        shared.go.drain();
    }
}

/// Makes the bridge per-monitor-v2 DPI aware, as `komponist.exe`'s manifest does (port W6): the
/// frame works in physical pixels and passes the DPI scale to the plugin. Must run before the
/// process's first window: Windows fixes a process's awareness when it creates one, after which
/// winit's own call fails and `SetWindowPos` sizes would be scaled behind the frame's back.
/// A process that is aware already (the manifest) keeps its setting.
pub fn per_monitor_dpi() {
    use windows_sys::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: a process-wide setting; failing (already set) changes nothing.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// The main thread's message-only wake window.
fn wake_window() -> io::Result<HWND> {
    let class: Vec<u16> = "komponist-bridge-wake\0".encode_utf16().collect();
    // SAFETY: the module handle of this executable.
    let instance = unsafe { GetModuleHandleW(std::ptr::null()) };
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wake_proc),
        hInstance: instance,
        lpszClassName: class.as_ptr(),
        // SAFETY: all-zero is valid for the rest of this plain C struct.
        ..unsafe { std::mem::zeroed() }
    };
    // SAFETY: a valid class description; registering twice fails harmlessly (tests start several
    // loops in one process).
    unsafe { RegisterClassExW(&wc) };
    // SAFETY: a message-only window of a registered class.
    let window = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            std::ptr::null(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            instance,
            std::ptr::null(),
        )
    };
    if window.is_null() {
        return Err(io::Error::last_os_error());
    }
    Ok(window)
}

/// The wake window's procedure: a wake (or its retry) calls the dispatcher.
unsafe extern "system" fn wake_proc(
    window: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let wake = msg == WM_WAKE || (msg == WM_TIMER && wparam == RETRY_TIMER);
    if !wake {
        // SAFETY: the default procedure with the arguments Windows passed.
        return unsafe { DefWindowProcW(window, msg, wparam, lparam) };
    }
    if msg == WM_TIMER {
        // SAFETY: this window's own timer.
        unsafe { KillTimer(window, RETRY_TIMER) };
    }
    // Cloned out: the dispatcher may install another one.
    let dispatch = DISPATCH.with(|d| d.borrow().clone());
    let handled = dispatch.is_none_or(|dispatch| dispatch());
    if !handled {
        // SAFETY: a timer of this window; replaces a pending one with the same id.
        unsafe { SetTimer(window, RETRY_TIMER, RETRY.as_millis() as u32, None) };
    }
    0
}

impl HostLoop for WinEventLoop {
    fn register_timer(&self, period: Duration) -> Result<TimerId, HostingError> {
        let mut state = self.state.borrow_mut();
        state.next_timer = state.next_timer.wrapping_add(1);
        let id = TimerId(state.next_timer);
        let period = period.max(Duration::from_millis(1));
        state.timers.push(TimerEntry {
            id,
            period,
            next: Instant::now() + period,
        });
        Ok(id)
    }

    fn unregister_timer(&self, timer: TimerId) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        let before = state.timers.len();
        state.timers.retain(|t| t.id != timer);
        if state.timers.len() == before {
            return Err(HostingError::HostLoop(format!("unknown timer {}", timer.0)));
        }
        Ok(())
    }
}

/// What the reader thread received: a message, or the error that ended (`Closed`, `Io`) or
/// skipped (`Codec`) one.
pub type Received = Result<ControlMsg, ChannelError>;

#[derive(Debug, Default)]
struct Queue {
    /// After a `Closed` or `Io` entry nothing follows: the reader has stopped.
    messages: VecDeque<Received>,
}

#[derive(Debug)]
struct Shared {
    queue: Mutex<Queue>,
    /// Signalled when the main thread made room.
    room: Condvar,
    /// Set while `queue.messages` is not empty.
    event: WakeEvent,
}

/// The control pipe's reader thread and its queue (module docs).
#[derive(Debug)]
pub struct ControlReader {
    shared: Arc<Shared>,
}

impl ControlReader {
    /// Starts the reader thread on `channel`. The thread is never joined: it blocks in `recv`
    /// until the app closes the pipe, and the bridge ends with `_exit` semantics anyway.
    pub fn start(channel: Arc<BridgeChannel>) -> io::Result<ControlReader> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue::default()),
            room: Condvar::new(),
            event: WakeEvent::new()?,
        });
        let reader = shared.clone();
        std::thread::Builder::new()
            .name("kp-bridge-control".into())
            .spawn(move || read_loop(&channel, &reader))?;
        Ok(ControlReader { shared })
    }

    /// The event that is signalled while messages are queued.
    pub fn event(&self) -> BorrowedHandle<'_> {
        self.shared.event.as_handle()
    }

    /// The next queued message, if any. Resets the event once the queue is empty (under the
    /// queue's lock, so a message queued right after sets it again).
    pub fn try_next(&self) -> Option<Received> {
        let mut queue = self
            .shared
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let next = queue.messages.pop_front();
        if queue.messages.is_empty() {
            self.shared.event.drain();
        }
        drop(queue);
        if next.is_some() {
            self.shared.room.notify_one();
        }
        next
    }
}

/// The reader thread: receives until the channel closes or breaks.
fn read_loop(channel: &BridgeChannel, shared: &Shared) {
    loop {
        let received = channel.recv();
        let last = matches!(received, Err(ChannelError::Closed | ChannelError::Io(_)));
        let mut queue = shared.queue.lock().unwrap_or_else(PoisonError::into_inner);
        while queue.messages.len() >= QUEUE_CAPACITY {
            queue = shared
                .room
                .wait(queue)
                .unwrap_or_else(PoisonError::into_inner);
        }
        queue.messages.push_back(received);
        shared.event.signal();
        drop(queue);
        if last {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_ipc::control::socket_pair;
    use komponist_ipc::{AppChannel, BridgeChannel};

    fn pair() -> (AppChannel, Arc<BridgeChannel>) {
        let (app, bridge) = socket_pair().unwrap();
        (app, Arc::new(BridgeChannel::from_handle(bridge)))
    }

    /// The relay wakes the message loop for an event and for the deadline; the dispatcher runs
    /// inside `run` (from the wake window), and a wake that finds the bridge busy is retried.
    #[test]
    fn the_relay_dispatches_events_and_deadlines_from_the_message_loop() {
        let lp = WinEventLoop::new().unwrap();
        let event = Arc::new(WakeEvent::new().unwrap());
        lp.start_relay(&[event.as_handle()]).unwrap();
        let calls = Rc::new(Cell::new(0u32));
        let busy = Rc::new(Cell::new(1u32));
        let weak = Rc::downgrade(&lp);
        let (counted, refusals, drained) = (calls.clone(), busy.clone(), event.clone());
        lp.set_dispatch(Some(Rc::new(move || {
            if refusals.get() > 0 {
                refusals.set(refusals.get() - 1);
                return false;
            }
            drained.drain();
            counted.set(counted.get() + 1);
            let lp = weak.upgrade().unwrap();
            // The first wake asks for a deadline 30 ms on, the second for none.
            let due = (counted.get() == 1).then(|| Instant::now() + Duration::from_millis(30));
            lp.rearm(due);
            lp.stop();
            true
        })));

        let signal = event.clone();
        let start = Instant::now();
        let setter = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            signal.signal();
        });
        lp.run();
        setter.join().unwrap();
        let first = start.elapsed();
        assert_eq!((calls.get(), busy.get()), (1, 0));
        assert!(first >= Duration::from_millis(15), "{first:?}");
        assert!(first < Duration::from_secs(1), "{first:?}");

        // Nothing is signalled: the deadline wakes the loop.
        let start = Instant::now();
        lp.run();
        let second = start.elapsed();
        assert_eq!(calls.get(), 2);
        assert!(second >= Duration::from_millis(20), "{second:?}");
        assert!(second < Duration::from_millis(500), "{second:?}");
        lp.set_dispatch(None);
    }

    #[test]
    fn timers_fire_periodically_and_stop() {
        let lp = WinEventLoop::new().unwrap();
        let id = lp.register_timer(Duration::from_millis(10)).unwrap();
        let end = Instant::now() + Duration::from_millis(105);
        let mut fired = 0;
        while Instant::now() < end {
            fired += lp
                .wait(&[], end.saturating_duration_since(Instant::now()))
                .iter()
                .filter(|t| **t == id)
                .count();
        }
        assert!((7..=11).contains(&fired), "{fired} timer calls");
        assert!(lp.timer_registered(id));
        lp.unregister_timer(id).unwrap();
        assert!(!lp.timer_registered(id) && lp.unregister_timer(id).is_err());
        assert!(lp.wait(&[], Duration::from_millis(30)).is_empty());
        assert_eq!(lp.timer_count(), 0);
    }

    /// Control messages wake the wait through the reader's event, in order; a closed pipe ends
    /// with `Closed`.
    #[test]
    fn the_reader_queues_messages_and_wakes_the_loop() {
        let lp = WinEventLoop::new().unwrap();
        let (app, bridge) = pair();
        let reader = ControlReader::start(bridge).unwrap();
        let start = Instant::now();
        lp.wait(&[reader.event()], Duration::from_millis(30));
        assert!(start.elapsed() >= Duration::from_millis(25));
        assert!(reader.try_next().is_none());

        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            for seq in 0..3 {
                app.send(&ControlMsg::Ping { seq }).unwrap();
            }
            app
        });
        let start = Instant::now();
        let mut got = Vec::new();
        while got.len() < 3 && start.elapsed() < Duration::from_secs(5) {
            lp.wait(&[reader.event()], Duration::from_secs(5));
            while let Some(received) = reader.try_next() {
                got.push(received.unwrap());
            }
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        let seqs: Vec<u32> = got
            .iter()
            .map(|m| match m {
                ControlMsg::Ping { seq } => *seq,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(seqs, [0, 1, 2]);
        drop(sender.join().unwrap());
        let start = Instant::now();
        loop {
            lp.wait(&[reader.event()], Duration::from_secs(5));
            if let Some(received) = reader.try_next() {
                assert_eq!(received, Err(ChannelError::Closed));
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }

    /// The queue is bounded: a reader that is ahead waits until the main thread takes messages.
    #[test]
    fn the_queue_is_bounded() {
        let (app, bridge) = pair();
        let reader = ControlReader::start(bridge).unwrap();
        let total = QUEUE_CAPACITY as u32 + 20;
        let sender = std::thread::spawn(move || {
            for seq in 0..total {
                app.send(&ControlMsg::Ping { seq }).unwrap();
            }
            app
        });
        let start = Instant::now();
        while reader.shared.queue.lock().unwrap().messages.len() < QUEUE_CAPACITY {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::yield_now();
        }
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            reader.shared.queue.lock().unwrap().messages.len(),
            QUEUE_CAPACITY
        );
        let mut seen = 0;
        while seen < total {
            match reader.try_next() {
                Some(Ok(ControlMsg::Ping { seq })) => {
                    assert_eq!(seq, seen);
                    seen += 1;
                }
                Some(other) => panic!("{other:?}"),
                None => {
                    assert!(start.elapsed() < Duration::from_secs(5));
                    std::thread::yield_now();
                }
            }
        }
        drop(sender.join().unwrap());
    }
}
