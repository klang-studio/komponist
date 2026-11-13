//! A small `poll()`-based [`HostLoop`] plus a matching [`HostNotify`]: the reference
//! implementation of the bridge main loop's plugin side, used by this crate's tests.
//!
//! The bridge (WP-31) owns the real main loop, which also serves the control socket and the
//! watchdog. It can reuse [`PollLoop`] or copy its policies; either way it should keep these rules:
//!
//! - **One `poll()` per tick** over a wake fd ([`LoopNotify`]'s eventfd), every registered plugin
//!   fd, and a timeout that ends at the next due timer. No X11 connection of its own.
//! - **Callbacks run outside every borrow:** a plugin registers and unregisters fds and timers from
//!   inside `on_fd` / `on_timer` / `on_main_thread`, so the loop first collects what is ready, then
//!   dispatches, and checks before each dispatch that the fd or timer is still registered.
//! - **Write interest policy** (spike WP-15 §1.7). JUCE plugins (Dexed) register their X11 socket
//!   with `READ | WRITE | ERROR`. A socket is almost always writable, so a level-triggered `POLLOUT`
//!   makes `poll()` return at once and the loop spins at 100 % CPU (about 10 million `on_fd` calls in
//!   15 s in the spike). Therefore:
//!   1. `READ` interest is polled as `POLLIN`; `ERROR` (`POLLERR`, `POLLHUP`, `POLLNVAL`) is always
//!      reported and needs no interest.
//!   2. `WRITE` interest is polled as `POLLOUT` only once the fd's write cooldown has expired.
//!      After an `on_fd` call that reported `WRITE`, the fd gets no `POLLOUT` for
//!      [`PollLoop::write_interval`] (default [`DEFAULT_WRITE_INTERVAL`], 50 ms). So an
//!      always-writable fd costs at most one extra wake-up and callback per interval, while a plugin
//!      that really waits for a full socket buffer to drain still learns about it within that
//!      interval. `None` turns write polling off entirely (the spike's `CLAP_CHILD_IGNORE_WRITE`).
//!   3. An fd that reports `POLLNVAL` (closed without unregistering) is reported once with `ERROR`
//!      and then parked until the plugin modifies or unregisters it, so a stale fd cannot spin
//!      either.
//! - **Notifications** ([`HostNotification`]) arrive from any thread through [`LoopNotify`]: an
//!   atomic bit per kind plus an eventfd wake, no allocation and no lock. The loop handles
//!   `MainThreadCallback` (calls [`HostedPlugin::on_main_thread`]) and `ParamFlushRequested` (calls
//!   [`HostedPlugin::flush_params`] while the plugin is inactive) itself and records everything
//!   else for the caller ([`PollLoop::take_observed`]); the bridge forwards those to the app.

use std::cell::{Cell, RefCell};
use std::os::fd::{AsFd, BorrowedFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use komponist_ipc::{GuiSize, LogLevel, WireEvent};
use rustix::event::{EventfdFlags, PollFd, PollFlags, Timespec};

use crate::host::{FdEvents, HostContext, HostLoop, HostNotification, HostNotify, TimerId};
use crate::plugin::{HostedPlugin, HostingError};

/// Default cooldown between two `WRITE` reports for the same fd (see the module docs).
pub const DEFAULT_WRITE_INTERVAL: Duration = Duration::from_millis(50);

/// Bits of [`HostNotification`] kinds in [`LoopNotify`] and [`Observed`].
pub mod notification_bits {
    /// [`super::HostNotification::MainThreadCallback`].
    pub const MAIN_THREAD_CALLBACK: u32 = 1 << 0;
    /// [`super::HostNotification::RestartRequested`].
    pub const RESTART_REQUESTED: u32 = 1 << 1;
    /// [`super::HostNotification::ProcessRequested`].
    pub const PROCESS_REQUESTED: u32 = 1 << 2;
    /// [`super::HostNotification::ParamFlushRequested`].
    pub const PARAM_FLUSH_REQUESTED: u32 = 1 << 3;
    /// [`super::HostNotification::ParamsChanged`].
    pub const PARAMS_CHANGED: u32 = 1 << 4;
    /// [`super::HostNotification::ParamEdited`].
    pub const PARAM_EDITED: u32 = 1 << 5;
    /// [`super::HostNotification::LatencyChanged`].
    pub const LATENCY_CHANGED: u32 = 1 << 6;
    /// [`super::HostNotification::StateDirty`].
    pub const STATE_DIRTY: u32 = 1 << 7;
    /// [`super::HostNotification::NoteNamesChanged`].
    pub const NOTE_NAMES_CHANGED: u32 = 1 << 8;
    /// [`super::HostNotification::GuiResizeRequested`].
    pub const GUI_RESIZE_REQUESTED: u32 = 1 << 9;
    /// [`super::HostNotification::GuiClosed`].
    pub const GUI_CLOSED: u32 = 1 << 10;
}

use notification_bits as bits;

/// The bit of a notification kind.
pub fn notification_bit(notification: &HostNotification) -> u32 {
    match notification {
        HostNotification::MainThreadCallback => bits::MAIN_THREAD_CALLBACK,
        HostNotification::RestartRequested => bits::RESTART_REQUESTED,
        HostNotification::ProcessRequested => bits::PROCESS_REQUESTED,
        HostNotification::ParamFlushRequested => bits::PARAM_FLUSH_REQUESTED,
        HostNotification::ParamsChanged => bits::PARAMS_CHANGED,
        HostNotification::ParamEdited { .. } => bits::PARAM_EDITED,
        HostNotification::LatencyChanged => bits::LATENCY_CHANGED,
        HostNotification::StateDirty => bits::STATE_DIRTY,
        HostNotification::NoteNamesChanged => bits::NOTE_NAMES_CHANGED,
        HostNotification::GuiResizeRequested(_) => bits::GUI_RESIZE_REQUESTED,
        HostNotification::GuiClosed => bits::GUI_CLOSED,
    }
}

const NO_SIZE: u64 = u64::MAX;

fn pack_size(size: GuiSize) -> u64 {
    (u64::from(size.width) << 32) | u64::from(size.height)
}

fn unpack_size(raw: u64) -> Option<GuiSize> {
    (raw != NO_SIZE).then_some(GuiSize {
        width: (raw >> 32) as u32,
        height: raw as u32,
    })
}

/// A [`HostNotify`] that coalesces notifications into atomic bits and wakes a [`PollLoop`] through
/// an eventfd. RT-safe: [`HostNotify::notify`] does atomics and at most one non-blocking `write`.
///
/// Coalescing: one bit per kind, the latest GUI size request, and for `ParamEdited` (VST3 only)
/// only the latest id and value plus a counter; a bridge that forwards VST3 GUI edits needs a ring
/// instead.
#[derive(Debug)]
pub struct LoopNotify {
    pending: AtomicU32,
    gui_size: AtomicU64,
    edited_id: AtomicU32,
    edited_value: AtomicU64,
    edited_count: AtomicU32,
    log_lines: [AtomicU32; 4],
    echo_log: bool,
    wake: OwnedFd,
}

impl LoopNotify {
    /// Creates the eventfd. `echo_log` prints plugin log lines to stderr (tests; never on the RT
    /// thread of a real bridge).
    pub fn new(echo_log: bool) -> Result<LoopNotify, HostingError> {
        let wake = rustix::event::eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK)
            .map_err(|e| HostingError::HostLoop(format!("eventfd: {e}")))?;
        Ok(LoopNotify {
            pending: AtomicU32::new(0),
            gui_size: AtomicU64::new(NO_SIZE),
            edited_id: AtomicU32::new(0),
            edited_value: AtomicU64::new(0),
            edited_count: AtomicU32::new(0),
            log_lines: Default::default(),
            echo_log,
            wake,
        })
    }

    /// The fd that becomes readable when a notification is pending.
    pub fn wake_fd(&self) -> BorrowedFd<'_> {
        self.wake.as_fd()
    }

    /// Clears the wake fd, then takes the pending bits and the latest GUI size request. Draining
    /// first means a notification that races with this call wakes the next `poll()`.
    pub fn take(&self) -> (u32, Option<GuiSize>) {
        let mut buf = [0u8; 8];
        // Non-blocking eventfd: one read resets the counter; EAGAIN means nothing was pending.
        let _ = rustix::io::read(&self.wake, &mut buf);
        let pending = self.pending.swap(0, Ordering::AcqRel);
        let size = if pending & bits::GUI_RESIZE_REQUESTED != 0 {
            unpack_size(self.gui_size.load(Ordering::Acquire))
        } else {
            None
        };
        (pending, size)
    }

    /// Number of `ParamEdited` notifications so far and the latest one.
    pub fn param_edits(&self) -> (u32, Option<(u32, f64)>) {
        let count = self.edited_count.load(Ordering::Acquire);
        let last = (count > 0).then(|| {
            (
                self.edited_id.load(Ordering::Acquire),
                f64::from_bits(self.edited_value.load(Ordering::Acquire)),
            )
        });
        (count, last)
    }

    /// Log lines received per level (debug, info, warning, error).
    pub fn log_counts(&self) -> [u32; 4] {
        [0, 1, 2, 3].map(|i| self.log_lines[i].load(Ordering::Relaxed))
    }
}

impl HostNotify for LoopNotify {
    fn notify(&self, notification: HostNotification) {
        match notification {
            HostNotification::GuiResizeRequested(size) => {
                self.gui_size.store(pack_size(size), Ordering::Release);
            }
            HostNotification::ParamEdited { id, value } => {
                self.edited_id.store(id, Ordering::Release);
                self.edited_value.store(value.to_bits(), Ordering::Release);
                self.edited_count.fetch_add(1, Ordering::AcqRel);
            }
            _ => {}
        }
        let bit = notification_bit(&notification);
        // Only the first notification since the last `take` needs to wake the loop.
        if self.pending.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            let _ = rustix::io::write(&self.wake, &1u64.to_ne_bytes());
        }
    }

    fn log(&self, level: LogLevel, message: &str) {
        let index = match level {
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warning => 2,
            LogLevel::Error => 3,
        };
        self.log_lines[index].fetch_add(1, Ordering::Relaxed);
        if self.echo_log {
            eprintln!("[plugin {level:?}] {message}");
        }
    }
}

/// What a [`PollLoop`] saw since the last [`PollLoop::take_observed`]: notification bits the loop
/// did not handle itself, the latest GUI size request, and the parameter events a
/// `ParamFlushRequested` produced (GUI edits while the plugin is inactive).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    /// [`notification_bits`] of every notification received, including those the loop handled.
    pub notifications: u32,
    /// The latest `GuiResizeRequested` size.
    pub gui_size: Option<GuiSize>,
    /// Output of the `flush_params` calls the loop made.
    pub flushed: Vec<WireEvent>,
}

impl Observed {
    /// Whether a notification of the kind `bit` ([`notification_bits`]) arrived.
    pub fn has(&self, bit: u32) -> bool {
        self.notifications & bit != 0
    }
}

/// Counters of a [`PollLoop`], for tests and diagnostics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopStats {
    /// Ticks (`poll()` calls).
    pub ticks: u64,
    /// `on_timer` calls.
    pub timer_calls: u64,
    /// `on_fd` calls.
    pub fd_calls: u64,
    /// `on_fd` calls that reported `WRITE`.
    pub write_calls: u64,
    /// `on_main_thread` calls.
    pub main_thread_calls: u64,
}

#[derive(Debug)]
struct TimerEntry {
    id: TimerId,
    period: Duration,
    next: Instant,
}

#[derive(Debug)]
struct FdEntry {
    fd: RawFd,
    interest: FdEvents,
    /// No `POLLOUT` before this instant (write cooldown).
    write_after: Instant,
    /// Reported `POLLNVAL`; not polled until modified.
    parked: bool,
}

#[derive(Debug, Default)]
struct LoopState {
    next_timer: u32,
    timers: Vec<TimerEntry>,
    fds: Vec<FdEntry>,
}

/// A single-threaded `poll()` loop implementing [`HostLoop`] for one plugin instance (see the
/// module docs for its policies). Main thread only.
#[derive(Debug)]
pub struct PollLoop {
    state: RefCell<LoopState>,
    notify: Arc<LoopNotify>,
    write_interval: Option<Duration>,
    observed: RefCell<Observed>,
    stats: Cell<LoopStats>,
}

impl PollLoop {
    /// A loop with the default write interval. `echo_log` prints plugin log lines to stderr.
    pub fn new(echo_log: bool) -> Result<Rc<PollLoop>, HostingError> {
        PollLoop::with_write_interval(echo_log, Some(DEFAULT_WRITE_INTERVAL))
    }

    /// A loop with an explicit write cooldown; `None` never polls for `WRITE`.
    pub fn with_write_interval(
        echo_log: bool,
        write_interval: Option<Duration>,
    ) -> Result<Rc<PollLoop>, HostingError> {
        Ok(Rc::new(PollLoop {
            state: RefCell::new(LoopState::default()),
            notify: Arc::new(LoopNotify::new(echo_log)?),
            write_interval,
            observed: RefCell::new(Observed::default()),
            stats: Cell::new(LoopStats::default()),
        }))
    }

    /// The write cooldown (see the module docs).
    pub fn write_interval(&self) -> Option<Duration> {
        self.write_interval
    }

    /// The notification sink that wakes this loop.
    pub fn notify(&self) -> &Arc<LoopNotify> {
        &self.notify
    }

    /// A [`HostContext`] serving the plugin through this loop.
    pub fn context(self: &Rc<Self>) -> HostContext {
        HostContext {
            host_loop: self.clone(),
            notify: self.notify.clone(),
            host_name: "komponist",
            host_version: env!("CARGO_PKG_VERSION"),
        }
    }

    /// Number of registered timers and fds.
    pub fn registrations(&self) -> (usize, usize) {
        let state = self.state.borrow();
        (state.timers.len(), state.fds.len())
    }

    /// The interest registered for `fd`.
    pub fn fd_interest(&self, fd: RawFd) -> Option<FdEvents> {
        let state = self.state.borrow();
        state.fds.iter().find(|e| e.fd == fd).map(|e| e.interest)
    }

    /// Counters since creation.
    pub fn stats(&self) -> LoopStats {
        self.stats.get()
    }

    /// Takes what the loop observed since the last call.
    pub fn take_observed(&self) -> Observed {
        std::mem::take(&mut *self.observed.borrow_mut())
    }

    /// Runs ticks until `duration` has passed.
    pub fn run_for(&self, plugin: &mut dyn HostedPlugin, duration: Duration) {
        let end = Instant::now() + duration;
        loop {
            let now = Instant::now();
            if now >= end {
                break;
            }
            self.run_once(plugin, end - now);
        }
    }

    /// Runs ticks until `done` returns true or `timeout` passes; returns whether `done` held.
    pub fn run_until(
        &self,
        plugin: &mut dyn HostedPlugin,
        timeout: Duration,
        mut done: impl FnMut(&mut dyn HostedPlugin, &PollLoop) -> bool,
    ) -> bool {
        let end = Instant::now() + timeout;
        loop {
            if done(plugin, self) {
                return true;
            }
            let now = Instant::now();
            if now >= end {
                return false;
            }
            self.run_once(plugin, (end - now).min(Duration::from_millis(10)));
        }
    }

    /// One tick: waits up to `max_wait` (less if a timer or write cooldown is due), then dispatches
    /// ready fds, due timers and pending notifications to `plugin`.
    pub fn run_once(&self, plugin: &mut dyn HostedPlugin, max_wait: Duration) {
        let mut stats = self.stats.get();
        stats.ticks += 1;

        // 1. Build the poll set and the timeout.
        let now = Instant::now();
        let mut deadline = now + max_wait;
        let mut wanted: Vec<(RawFd, PollFlags)> = Vec::new();
        {
            let state = self.state.borrow();
            for timer in &state.timers {
                deadline = deadline.min(timer.next);
            }
            for entry in &state.fds {
                if entry.parked {
                    continue;
                }
                let mut flags = PollFlags::empty();
                if entry.interest.contains(FdEvents::READ) {
                    flags |= PollFlags::IN;
                }
                if entry.interest.contains(FdEvents::WRITE) && self.write_interval.is_some() {
                    if now >= entry.write_after {
                        flags |= PollFlags::OUT;
                    } else {
                        deadline = deadline.min(entry.write_after);
                    }
                }
                wanted.push((entry.fd, flags));
            }
        }
        let timeout = deadline.saturating_duration_since(now);
        let mut pollfds = Vec::with_capacity(wanted.len() + 1);
        pollfds.push(PollFd::from_borrowed_fd(
            self.notify.wake_fd(),
            PollFlags::IN,
        ));
        for &(fd, flags) in &wanted {
            // SAFETY: the plugin registered `fd` and keeps it open until it unregisters it; a
            // closed fd only yields POLLNVAL.
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            pollfds.push(PollFd::from_borrowed_fd(borrowed, flags));
        }
        let timespec = Timespec {
            tv_sec: timeout.as_secs() as i64,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let polled = rustix::event::poll(&mut pollfds, Some(&timespec)).is_ok();

        // 2. Collect ready fds; update cooldowns and parking while the state is borrowed.
        let mut ready: Vec<(RawFd, FdEvents)> = Vec::new();
        let now = Instant::now();
        if polled {
            let mut state = self.state.borrow_mut();
            for (pollfd, &(fd, _)) in pollfds[1..].iter().zip(&wanted) {
                let revents = pollfd.revents();
                if revents.is_empty() {
                    continue;
                }
                let Some(entry) = state.fds.iter_mut().find(|e| e.fd == fd) else {
                    continue;
                };
                let mut events = FdEvents::NONE;
                if revents.contains(PollFlags::IN) && entry.interest.contains(FdEvents::READ) {
                    events = events.union(FdEvents::READ);
                }
                if revents.contains(PollFlags::OUT) && entry.interest.contains(FdEvents::WRITE) {
                    events = events.union(FdEvents::WRITE);
                    if let Some(interval) = self.write_interval {
                        entry.write_after = now + interval;
                    }
                }
                if revents.intersects(PollFlags::ERR | PollFlags::HUP | PollFlags::NVAL) {
                    events = events.union(FdEvents::ERROR);
                }
                if revents.contains(PollFlags::NVAL) {
                    entry.parked = true;
                }
                if events != FdEvents::NONE {
                    ready.push((fd, events));
                }
            }
        }
        drop(pollfds);

        // 3. Dispatch fds, each only if still registered (a callback may unregister another).
        for (fd, events) in ready {
            if self.fd_interest(fd).is_some() {
                stats.fd_calls += 1;
                if events.contains(FdEvents::WRITE) {
                    stats.write_calls += 1;
                }
                plugin.on_fd(fd, events);
            }
        }

        // 4. Due timers.
        let now = Instant::now();
        let due: Vec<TimerId> = {
            let mut state = self.state.borrow_mut();
            state
                .timers
                .iter_mut()
                .filter(|t| t.next <= now)
                .map(|t| {
                    // Skip missed periods instead of firing a burst.
                    t.next += t.period;
                    if t.next <= now {
                        t.next = now + t.period;
                    }
                    t.id
                })
                .collect()
        };
        for id in due {
            let registered = self.state.borrow().timers.iter().any(|t| t.id == id);
            if registered {
                stats.timer_calls += 1;
                plugin.on_timer(id);
            }
        }

        // 5. Notifications.
        let (pending, size) = self.notify.take();
        if pending != 0 {
            {
                let mut observed = self.observed.borrow_mut();
                observed.notifications |= pending;
                if size.is_some() {
                    observed.gui_size = size;
                }
            }
            if pending & bits::MAIN_THREAD_CALLBACK != 0 {
                stats.main_thread_calls += 1;
                plugin.on_main_thread();
            }
            if pending & bits::PARAM_FLUSH_REQUESTED != 0 && !plugin.is_active() {
                let mut out = Vec::new();
                plugin.flush_params(&[], &mut out);
                self.observed.borrow_mut().flushed.extend(out);
            }
        }
        self.stats.set(stats);
    }
}

impl HostLoop for PollLoop {
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

    fn register_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        if fd < 0 || state.fds.iter().any(|e| e.fd == fd) {
            return Err(HostingError::HostLoop(format!(
                "fd {fd} is invalid or already registered"
            )));
        }
        state.fds.push(FdEntry {
            fd,
            interest: events,
            write_after: Instant::now(),
            parked: false,
        });
        Ok(())
    }

    fn modify_fd(&self, fd: RawFd, events: FdEvents) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        let entry = state
            .fds
            .iter_mut()
            .find(|e| e.fd == fd)
            .ok_or_else(|| HostingError::HostLoop(format!("unknown fd {fd}")))?;
        entry.interest = events;
        entry.parked = false;
        Ok(())
    }

    fn unregister_fd(&self, fd: RawFd) -> Result<(), HostingError> {
        let mut state = self.state.borrow_mut();
        let before = state.fds.len();
        state.fds.retain(|e| e.fd != fd);
        if state.fds.len() == before {
            return Err(HostingError::HostLoop(format!("unknown fd {fd}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::{ActivateConfig, GuiInfo, HostedProcessor};
    use komponist_ipc::{
        NoteName, ParamInfo, PluginDescriptor, PluginFormat, PluginKind, RenderMode,
    };
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    /// What the fake plugin was called with.
    #[derive(Debug, Clone, PartialEq)]
    enum Call {
        Timer(TimerId),
        Fd(RawFd, FdEvents),
        MainThread,
        Flush,
    }

    /// An action the fake plugin runs from inside its callbacks.
    type OnCall = Box<dyn FnMut(&Call)>;

    /// A plugin that records dispatches and runs an optional action from inside callbacks.
    struct FakePlugin {
        descriptor: PluginDescriptor,
        calls: Vec<Call>,
        on_call: Option<OnCall>,
    }

    impl FakePlugin {
        fn new() -> FakePlugin {
            FakePlugin {
                descriptor: PluginDescriptor {
                    format: PluginFormat::Clap,
                    id: "fake".into(),
                    name: "fake".into(),
                    vendor: String::new(),
                    version: String::new(),
                    kind: PluginKind::Other,
                    features: Vec::new(),
                    bundle: "/fake.clap".into(),
                },
                calls: Vec::new(),
                on_call: None,
            }
        }

        fn record(&mut self, call: Call) {
            if let Some(f) = &mut self.on_call {
                f(&call);
            }
            self.calls.push(call);
        }

        fn count(&self, pred: impl Fn(&Call) -> bool) -> usize {
            self.calls.iter().filter(|c| pred(c)).count()
        }
    }

    impl HostedPlugin for FakePlugin {
        fn descriptor(&self) -> &PluginDescriptor {
            &self.descriptor
        }
        fn has_gui(&self) -> bool {
            false
        }
        fn activate(
            &mut self,
            _: ActivateConfig,
        ) -> Result<Box<dyn HostedProcessor>, HostingError> {
            Err(HostingError::Unsupported("fake"))
        }
        fn deactivate(&mut self, _: Box<dyn HostedProcessor>) {}
        fn is_active(&self) -> bool {
            false
        }
        fn set_render_mode(&mut self, _: RenderMode) -> Result<(), HostingError> {
            Ok(())
        }
        fn params(&self) -> Vec<ParamInfo> {
            Vec::new()
        }
        fn param_value(&self, _: u32) -> Option<f64> {
            None
        }
        fn param_text(&self, _: u32, _: f64) -> Option<String> {
            None
        }
        fn set_param(&mut self, _: u32, _: f64) {}
        fn flush_params(&mut self, _: &[WireEvent], output: &mut Vec<WireEvent>) {
            self.record(Call::Flush);
            output.push(WireEvent {
                kind: 4,
                param_id: 3,
                value: 0.5,
                ..WireEvent::default()
            });
        }
        fn save_state(&mut self) -> Result<Vec<u8>, HostingError> {
            Ok(Vec::new())
        }
        fn load_state(&mut self, _: &[u8]) -> Result<(), HostingError> {
            Ok(())
        }
        fn latency(&self) -> u32 {
            0
        }
        fn note_names(&self) -> Vec<NoteName> {
            Vec::new()
        }
        fn gui_open(&mut self, _: u64, _: f64) -> Result<GuiInfo, HostingError> {
            Err(HostingError::Unsupported("fake"))
        }
        fn gui_set_size(&mut self, _: GuiSize) -> Result<GuiSize, HostingError> {
            Err(HostingError::Unsupported("fake"))
        }
        fn gui_close(&mut self) {}
        fn on_main_thread(&mut self) {
            self.record(Call::MainThread);
        }
        fn on_timer(&mut self, timer: TimerId) {
            self.record(Call::Timer(timer));
        }
        fn on_fd(&mut self, fd: RawFd, events: FdEvents) {
            self.record(Call::Fd(fd, events));
        }
    }

    #[test]
    fn timers_fire_periodically_and_stop() {
        let lp = PollLoop::new(false).unwrap();
        let mut plugin = FakePlugin::new();
        let id = lp.register_timer(Duration::from_millis(10)).unwrap();
        lp.run_for(&mut plugin, Duration::from_millis(105));
        let fired = plugin.count(|c| *c == Call::Timer(id));
        assert!((7..=11).contains(&fired), "{fired} timer calls in 105 ms");
        lp.unregister_timer(id).unwrap();
        assert!(lp.unregister_timer(id).is_err());
        plugin.calls.clear();
        lp.run_for(&mut plugin, Duration::from_millis(30));
        assert!(plugin.calls.is_empty());
        // Periods below 1 ms are raised to 1 ms instead of spinning.
        let fast = lp.register_timer(Duration::ZERO).unwrap();
        lp.run_for(&mut plugin, Duration::from_millis(20));
        assert!(plugin.count(|c| *c == Call::Timer(fast)) <= 21);
    }

    #[test]
    fn a_callback_may_unregister_its_timer() {
        let lp = PollLoop::new(false).unwrap();
        let mut plugin = FakePlugin::new();
        let a = lp.register_timer(Duration::from_millis(5)).unwrap();
        let b = lp.register_timer(Duration::from_millis(5)).unwrap();
        let lp2 = lp.clone();
        // On the first call of either timer, both go away: the other must not fire afterwards.
        plugin.on_call = Some(Box::new(move |call| {
            if let Call::Timer(_) = call {
                let _ = lp2.unregister_timer(a);
                let _ = lp2.unregister_timer(b);
            }
        }));
        lp.run_for(&mut plugin, Duration::from_millis(30));
        assert_eq!(plugin.count(|c| matches!(c, Call::Timer(_))), 1);
        assert_eq!(lp.registrations(), (0, 0));
    }

    #[test]
    fn read_readiness_is_dispatched() {
        let lp = PollLoop::new(false).unwrap();
        let mut plugin = FakePlugin::new();
        let (ours, mut theirs) = UnixStream::pair().unwrap();
        let fd = ours.as_raw_fd();
        lp.register_fd(fd, FdEvents::READ).unwrap();
        assert!(lp.register_fd(fd, FdEvents::READ).is_err());
        lp.run_once(&mut plugin, Duration::from_millis(20));
        assert!(plugin.calls.is_empty());
        theirs.write_all(b"x").unwrap();
        lp.run_once(&mut plugin, Duration::from_millis(200));
        assert_eq!(plugin.calls, vec![Call::Fd(fd, FdEvents::READ)]);
        lp.modify_fd(fd, FdEvents::NONE).unwrap();
        plugin.calls.clear();
        lp.run_once(&mut plugin, Duration::from_millis(20));
        assert!(plugin.calls.is_empty());
        lp.unregister_fd(fd).unwrap();
        assert!(lp.unregister_fd(fd).is_err());
        assert!(lp.modify_fd(fd, FdEvents::READ).is_err());
    }

    /// Spike WP-15 §1.7: an always-writable fd with WRITE interest must not make the loop spin.
    #[test]
    fn write_interest_is_rate_limited() {
        let lp = PollLoop::new(false).unwrap();
        let mut plugin = FakePlugin::new();
        let (ours, _theirs) = UnixStream::pair().unwrap();
        let fd = ours.as_raw_fd();
        lp.register_fd(
            fd,
            FdEvents::READ.union(FdEvents::WRITE).union(FdEvents::ERROR),
        )
        .unwrap();
        lp.run_for(&mut plugin, Duration::from_millis(500));
        let stats = lp.stats();
        // 500 ms / 50 ms = 10 cooldowns; allow for timing slack.
        assert!(
            (5..=12).contains(&stats.write_calls),
            "{} write callbacks in 500 ms",
            stats.write_calls
        );
        assert!(stats.ticks < 40, "{} ticks in 500 ms", stats.ticks);
        assert!(
            plugin
                .calls
                .iter()
                .all(|c| *c == Call::Fd(fd, FdEvents::WRITE))
        );

        // Write polling off: no WRITE callbacks at all.
        let lp = PollLoop::with_write_interval(false, None).unwrap();
        let mut plugin = FakePlugin::new();
        lp.register_fd(fd, FdEvents::READ.union(FdEvents::WRITE))
            .unwrap();
        lp.run_for(&mut plugin, Duration::from_millis(100));
        assert_eq!(lp.stats().write_calls, 0);
        assert!(plugin.calls.is_empty());
    }

    #[test]
    fn a_closed_fd_is_reported_once_and_parked() {
        let lp = PollLoop::new(false).unwrap();
        let mut plugin = FakePlugin::new();
        // Never a valid fd (far above RLIMIT_NOFILE), so no parallel test can reuse the number.
        let fd: RawFd = 999_999;
        lp.register_fd(fd, FdEvents::READ).unwrap();
        lp.run_for(&mut plugin, Duration::from_millis(50));
        assert_eq!(plugin.calls, vec![Call::Fd(fd, FdEvents::ERROR)]);
        // Modifying un-parks it.
        lp.modify_fd(fd, FdEvents::READ).unwrap();
        lp.run_once(&mut plugin, Duration::from_millis(10));
        assert_eq!(plugin.calls.len(), 2);
        lp.unregister_fd(fd).unwrap();
    }

    #[test]
    fn notifications_wake_the_loop_from_other_threads() {
        let lp = PollLoop::new(false).unwrap();
        let mut plugin = FakePlugin::new();
        let notify = lp.notify().clone();
        let sender = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
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
            notify.notify(HostNotification::ParamFlushRequested);
            notify.notify(HostNotification::ParamEdited { id: 4, value: 0.25 });
            notify.log(LogLevel::Warning, "careful");
        });
        let start = Instant::now();
        let woke = lp.run_until(&mut plugin, Duration::from_secs(2), |p, _| {
            p.descriptor();
            lp.stats().main_thread_calls > 0
        });
        sender.join().unwrap();
        assert!(woke && start.elapsed() < Duration::from_secs(1));
        lp.run_once(&mut plugin, Duration::from_millis(10));
        // Coalesced to one call, or two if the loop woke between the sender's two requests.
        let main_thread_calls = plugin.count(|c| *c == Call::MainThread);
        assert!((1..=2).contains(&main_thread_calls), "{main_thread_calls}");
        assert_eq!(plugin.count(|c| *c == Call::Flush), 1);
        let observed = lp.take_observed();
        assert!(observed.has(bits::GUI_RESIZE_REQUESTED) && observed.has(bits::PARAM_EDITED));
        assert_eq!(
            observed.gui_size,
            Some(GuiSize {
                width: 640,
                height: 480
            })
        );
        assert_eq!(observed.flushed.len(), 1);
        assert_eq!(lp.notify().param_edits(), (1, Some((4, 0.25))));
        assert_eq!(lp.notify().log_counts(), [0, 0, 1, 0]);
        assert_eq!(lp.take_observed(), Observed::default());
    }

    #[test]
    fn notify_is_allocation_free() {
        let notify = LoopNotify::new(false).unwrap();
        assert_no_alloc::assert_no_alloc(|| {
            notify.notify(HostNotification::MainThreadCallback);
            notify.notify(HostNotification::GuiResizeRequested(GuiSize {
                width: 1,
                height: 2,
            }));
            notify.log(LogLevel::Info, "from the audio thread");
        });
        let (pending, size) = notify.take();
        assert_eq!(
            pending,
            bits::MAIN_THREAD_CALLBACK | bits::GUI_RESIZE_REQUESTED
        );
        assert_eq!(
            size,
            Some(GuiSize {
                width: 1,
                height: 2
            })
        );
        assert_eq!(notify.take(), (0, None));
    }
}
