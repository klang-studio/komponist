//! The bridge main loop on Windows (port W5): [`WinEventLoop`] (the plugin's [`HostLoop`]) and
//! [`ControlReader`] (the control pipe, read on a thread of its own).
//!
//! Windows plugins need a message pump on their main thread even without a visible GUI: they
//! create hidden windows, `SetTimer` timers and COM objects there and post work to them. So the main
//! thread waits in `MsgWaitForMultipleObjectsEx(QS_ALLINPUT, MWMO_INPUTAVAILABLE)`
//! (`komponist_hosting::host_loop::wake::wait`) over
//!
//! - the control reader's event (a manual-reset event, set while control messages are queued),
//! - the notification event of [`crate::notify::BridgeNotify`] (set by plugin threads and the RT
//!   thread with one `SetEvent`, never blocking),
//! - a high-resolution waitable timer armed for the next due host timer or deadline (a
//!   millisecond timeout would end on the 15.6 ms clock tick),
//!
//! and every window message queued for the thread is dispatched before the wait returns. The
//! caller then dispatches due timers, notifications and control messages, outside every borrow
//! (a plugin registers timers re-entrantly), as on Linux (`event_loop.rs`) and macOS
//! (`cf_loop.rs`). There is no fd registry: CLAP posix-fd-support and VST3 `IRunLoop` are not
//! offered on Windows.
//!
//! **Why a reader thread:** a pipe handle cannot be waited on for "readable", and the channel's
//! overlapped reads are internal to `komponist-ipc`. The reader blocks in `Channel::recv` and
//! queues whole messages (at most [`QUEUE_CAPACITY`]; when the main thread falls behind, the
//! reader stops reading and the app's writer blocks, as with a full socket on Linux). The main
//! thread sends on the same channel at the same time; the overlapped pipe allows that.
//!
//! **Nested loops:** while a plugin call runs its own modal loop (a dialog), or later while eframe
//! runs the frame window (W6), this wait does not run: window messages are still dispatched by
//! that loop, but control messages, notifications and timers wait until it returns, as during a
//! long plugin call on Linux. W6 has to serve them from inside eframe's loop (the seam is
//! `Bridge::run` in `instance.rs`).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::os::windows::io::{AsHandle, BorrowedHandle};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::wake::{self, DeadlineTimer, WakeEvent};
use komponist_hosting::{HostLoop, HostingError, TimerId};
use komponist_ipc::{BridgeChannel, ChannelError, ControlMsg};

/// Control messages the reader queues before it waits for the main thread.
pub const QUEUE_CAPACITY: usize = 256;

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

/// See the module docs. Main thread only; shared with the plugin as `Rc<dyn HostLoop>`.
#[derive(Debug)]
pub struct WinEventLoop {
    state: RefCell<LoopState>,
    timer: DeadlineTimer,
}

impl WinEventLoop {
    /// A loop with its deadline timer.
    pub fn new() -> io::Result<Rc<WinEventLoop>> {
        Ok(Rc::new(WinEventLoop {
            state: RefCell::new(LoopState::default()),
            timer: DeadlineTimer::new()?,
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

    /// One wait over `extra` (the caller's events) until one is signalled, a window message
    /// arrives (it is dispatched), `max_wait` passes, or a timer is due. Returns the due timers
    /// (each moves on by its period; missed periods are skipped instead of firing a burst).
    pub fn wait(&self, extra: &[BorrowedHandle<'_>], max_wait: Duration) -> Vec<TimerId> {
        let mut deadline = Instant::now() + max_wait;
        for timer in &self.state.borrow().timers {
            deadline = deadline.min(timer.next);
        }
        // No borrow is held here: plugin window procedures run inside.
        wake::wait(&self.timer, extra, Some(deadline));
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
