//! [`BridgeNotify`]: the bridge's [`HostNotify`], the way plugin threads reach the main loop.
//!
//! Like `komponist_hosting::host_loop::LoopNotify` (same bits, same eventfd wake), plus what a
//! real bridge needs on top:
//!
//! - **Log lines** are queued with their text for the main loop, which forwards them to the app as
//!   `BridgeMsg::Log` (only the main thread sends on the control channel). Lines from the RT
//!   thread are dropped and counted (no allocation there); the queue is bounded.
//! - **`ParamEdited`** (VST3 GUI edits, main thread) is queued per edit instead of keeping only
//!   the latest one, so edits of different parameters are not merged.

use std::cell::Cell;
use std::os::fd::BorrowedFd;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use komponist_hosting::host_loop::wake::WakeFd;
use komponist_hosting::host_loop::{notification_bit, notification_bits};
use komponist_hosting::{GuiSize, HostNotification, HostNotify};
use komponist_ipc::LogLevel;

/// Pending bit for queued log lines (above every `notification_bits` value).
pub const LOG_PENDING: u32 = 1 << 31;

/// Log lines kept between two main-loop ticks; more are dropped and counted.
const MAX_LOG_LINES: usize = 256;

/// Longest log line forwarded, in bytes (longer ones are cut at a char boundary).
const MAX_LOG_LEN: usize = 4096;

/// `ParamEdited` notifications kept between two ticks; more are dropped.
const MAX_EDITS: usize = 1024;

const NO_SIZE: u64 = u64::MAX;

std::thread_local! {
    /// Set on the bridge RT thread: plugin log lines from it are dropped instead of allocating.
    static ON_RT_THREAD: Cell<bool> = const { Cell::new(false) };
}

/// Marks the calling thread as the bridge RT thread (for its whole life).
pub fn mark_rt_thread() {
    ON_RT_THREAD.set(true);
}

fn on_rt_thread() -> bool {
    ON_RT_THREAD.get()
}

/// Log lines taken from the queue by [`BridgeNotify::take_logs`].
#[derive(Debug, Default, PartialEq)]
pub struct Logs {
    /// Queued lines in order.
    pub lines: Vec<(LogLevel, String)>,
    /// Lines dropped because the queue was full.
    pub dropped_full: u32,
    /// Lines dropped because they came from the RT thread.
    pub dropped_rt: u32,
}

#[derive(Debug, Default)]
struct LogQueue {
    lines: Vec<(LogLevel, String)>,
    dropped: u32,
}

/// See the module docs. Shared by the plugin (any thread) and the main loop.
#[derive(Debug)]
pub struct BridgeNotify {
    pending: AtomicU32,
    gui_size: AtomicU64,
    logs: Mutex<LogQueue>,
    dropped_rt_logs: AtomicU32,
    edits: Mutex<Vec<(u32, f64)>>,
    wake: WakeFd,
}

impl BridgeNotify {
    /// Creates the wake fd (eventfd on Linux, pipe on macOS).
    pub fn new() -> std::io::Result<BridgeNotify> {
        let wake = WakeFd::new()?;
        let edits = Mutex::new(Vec::with_capacity(MAX_EDITS));
        // On macOS `std`'s mutex boxes its pthread mutex on first use: lock it once here, so that
        // `notify` (`ParamEdited`) never allocates.
        drop(edits.lock());
        Ok(BridgeNotify {
            pending: AtomicU32::new(0),
            gui_size: AtomicU64::new(NO_SIZE),
            logs: Mutex::new(LogQueue::default()),
            dropped_rt_logs: AtomicU32::new(0),
            edits,
            wake,
        })
    }

    /// Readable while something is pending.
    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake.as_fd()
    }

    /// Sets `bit` and wakes the main loop if it was not set yet. RT-safe.
    fn raise(&self, bit: u32) {
        if self.pending.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            // Non-blocking: never blocks (EAGAIN only when a wake-up is pending anyway).
            self.wake.signal();
        }
    }

    /// Clears the wake fd, then takes the pending bits (`notification_bits` and [`LOG_PENDING`])
    /// and the latest GUI size request. Draining first means a notification that races with this
    /// call wakes the next `poll()`.
    pub fn take(&self) -> (u32, Option<GuiSize>) {
        self.wake.drain();
        let pending = self.pending.swap(0, Ordering::AcqRel);
        let size = if pending & notification_bits::GUI_RESIZE_REQUESTED != 0 {
            let raw = self.gui_size.load(Ordering::Acquire);
            (raw != NO_SIZE).then_some(GuiSize {
                width: (raw >> 32) as u32,
                height: raw as u32,
            })
        } else {
            None
        };
        (pending, size)
    }

    /// Takes only the pending bits in `mask`; the others stay pending (the next `take` gets them).
    pub fn take_bits(&self, mask: u32) -> u32 {
        self.pending.fetch_and(!mask, Ordering::AcqRel) & mask
    }

    /// Takes the queued log lines and the drop counters.
    pub fn take_logs(&self) -> Logs {
        let mut queue = self.logs.lock().unwrap_or_else(PoisonError::into_inner);
        let taken = std::mem::take(&mut *queue);
        drop(queue);
        Logs {
            lines: taken.lines,
            dropped_full: taken.dropped,
            dropped_rt: self.dropped_rt_logs.swap(0, Ordering::AcqRel),
        }
    }

    /// Takes the queued `ParamEdited` values in order.
    pub fn take_edits(&self) -> Vec<(u32, f64)> {
        let mut edits = self.edits.lock().unwrap_or_else(PoisonError::into_inner);
        let taken = edits.clone();
        edits.clear();
        taken
    }
}

impl HostNotify for BridgeNotify {
    fn notify(&self, notification: HostNotification) {
        match notification {
            HostNotification::GuiResizeRequested(size) => {
                let packed = (u64::from(size.width) << 32) | u64::from(size.height);
                self.gui_size.store(packed, Ordering::Release);
            }
            HostNotification::ParamEdited { id, value } => {
                // Main thread only (VST3 `performEdit`). `try_lock` never blocks, and the vector
                // has its capacity from the start, so `push` does not allocate.
                if let Ok(mut edits) = self.edits.try_lock()
                    && edits.len() < MAX_EDITS
                {
                    edits.push((id, value));
                }
            }
            _ => {}
        }
        self.raise(notification_bit(&notification));
    }

    fn log(&self, level: LogLevel, message: &str) {
        if on_rt_thread() {
            self.dropped_rt_logs.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let mut end = message.len().min(MAX_LOG_LEN);
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        let mut queue = self.logs.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.lines.len() < MAX_LOG_LINES {
            queue.lines.push((level, message[..end].to_owned()));
        } else {
            queue.dropped += 1;
        }
        drop(queue);
        self.raise(LOG_PENDING);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notification_bits as bits;

    fn readable(notify: &BridgeNotify) -> bool {
        let mut fds = [rustix::event::PollFd::from_borrowed_fd(
            notify.wake_fd(),
            rustix::event::PollFlags::IN,
        )];
        let zero = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        rustix::event::poll(&mut fds, Some(&zero)).unwrap() == 1
    }

    #[test]
    fn notifications_coalesce_and_wake() {
        let notify = BridgeNotify::new().unwrap();
        assert!(!readable(&notify));
        notify.notify(HostNotification::MainThreadCallback);
        notify.notify(HostNotification::MainThreadCallback);
        notify.notify(HostNotification::GuiResizeRequested(GuiSize {
            width: 300,
            height: 200,
        }));
        notify.notify(HostNotification::GuiResizeRequested(GuiSize {
            width: 640,
            height: 480,
        }));
        assert!(readable(&notify));
        let (pending, size) = notify.take();
        assert_eq!(
            pending,
            bits::MAIN_THREAD_CALLBACK | bits::GUI_RESIZE_REQUESTED
        );
        assert_eq!(
            size,
            Some(GuiSize {
                width: 640,
                height: 480
            })
        );
        assert!(!readable(&notify));
        assert_eq!(notify.take(), (0, None));
    }

    #[test]
    fn edits_are_kept_per_parameter() {
        let notify = BridgeNotify::new().unwrap();
        notify.notify(HostNotification::ParamEdited { id: 1, value: 0.5 });
        notify.notify(HostNotification::ParamEdited { id: 2, value: 0.25 });
        notify.notify(HostNotification::ParamEdited { id: 1, value: 0.75 });
        assert_eq!(notify.take().0, bits::PARAM_EDITED);
        assert_eq!(notify.take_edits(), [(1, 0.5), (2, 0.25), (1, 0.75)]);
        assert!(notify.take_edits().is_empty());
    }

    #[test]
    fn logs_are_queued_bounded_and_dropped_on_the_rt_thread() {
        let notify = std::sync::Arc::new(BridgeNotify::new().unwrap());
        notify.log(LogLevel::Warning, "careful");
        let long = "ä".repeat(MAX_LOG_LEN);
        notify.log(LogLevel::Info, &long);
        let rt = {
            let notify = notify.clone();
            std::thread::spawn(move || {
                mark_rt_thread();
                assert_no_alloc::assert_no_alloc(|| notify.log(LogLevel::Error, "from RT"));
            })
        };
        rt.join().unwrap();
        assert_eq!(notify.take().0, LOG_PENDING);
        let logs = notify.take_logs();
        assert_eq!(logs.lines.len(), 2);
        assert_eq!(logs.lines[0], (LogLevel::Warning, "careful".to_owned()));
        assert!(logs.lines[1].1.len() <= MAX_LOG_LEN && logs.lines[1].1.starts_with('ä'));
        assert_eq!((logs.dropped_full, logs.dropped_rt), (0, 1));

        for i in 0..MAX_LOG_LINES + 5 {
            notify.log(LogLevel::Debug, &i.to_string());
        }
        let logs = notify.take_logs();
        assert_eq!(logs.lines.len(), MAX_LOG_LINES);
        assert_eq!(logs.dropped_full, 5);
        assert_eq!(notify.take_logs(), Logs::default());
    }

    #[test]
    fn notify_is_allocation_free() {
        let notify = BridgeNotify::new().unwrap();
        assert_no_alloc::assert_no_alloc(|| {
            notify.notify(HostNotification::MainThreadCallback);
            notify.notify(HostNotification::ParamFlushRequested);
            notify.notify(HostNotification::ParamEdited { id: 3, value: 1.0 });
            notify.notify(HostNotification::GuiResizeRequested(GuiSize {
                width: 1,
                height: 2,
            }));
        });
        assert_eq!(notify.take_edits(), [(3, 1.0)]);
    }
}
