//! Instance mode: `komponist-bridge --instance --control-fd N` (Windows: `--instance`, the control
//! pipe is stdin) (ARCHITECTURE §8.1–8.2).
//!
//! Start-up (komponist-ipc `control` docs): send `Hello`, receive the memfd and map it, then serve
//! control messages until `Shutdown` or until the app closes the socket. Everything except block
//! processing happens on the main thread, which is also the plugin's main thread:
//!
//! - **Main loop:** on Linux one `poll()` (`event_loop::EventLoop`) over the control socket, the
//!   notification eventfd ([`BridgeNotify`]), the plugin's fds and timers; on macOS the main
//!   thread's CFRunLoop (`cf_loop::CfEventLoop`) with all of them as common-mode sources; on Windows
//!   the main thread's message pump (`win_loop::WinEventLoop`), woken by a relay thread that waits
//!   for the control reader thread's event (`win_loop::ControlReader`), the notification event and
//!   a high-resolution timer and posts to a window of the main thread, without plugin fds. Each tick
//!   dispatches plugin fds and timers, handles notifications, reads control messages and forwards
//!   queued log lines and parameter values. Only this thread sends on the control channel.
//! - **Watchdog:** `Ping` is answered with `Pong` from this loop, in every state, between two
//!   plugin calls; a stalled loop shows as a missing `Pong`. A long plugin call on this thread
//!   (`Load`, `OpenGui`) delays it too; a plugin busy in `process` (RT thread) does not.
//! - **RT thread:** started by `Activate` ([`RtThread`]), stopped by `Deactivate`, restarts, render
//!   mode changes and shutdown. Its realtime outcome (`SCHED_FIFO` on Linux, the time-constraint
//!   policy on macOS) goes to the app as `RtPriority` right before every `Activated`, and as a
//!   `Log` line starting with [`RT_LOG_PREFIX`] when it is new or changed (`Info` on success,
//!   `Warning` on failure; processing goes on either way). On macOS a work interval the thread
//!   could not join is a `Warning` line too.
//! - **Parameters:** `SetParam` reaches the plugin as an event at offset 0 of the next block while
//!   active, through `flush_params` while inactive. Values the app set, values the plugin changed
//!   outside processing (flush output, VST3 edits) and all values after a state load go back as
//!   `ParamValues` with the plugin's text, coalesced to at most one message per
//!   [`VALUES_INTERVAL`]. GUI edits during processing travel through the shared output ring.
//! - **Plugin GUI:** on Linux the plugin attaches to the app's X11 child window (`OpenGui` with
//!   `GuiTarget::X11Child`); on macOS (#52) and Windows (port W6) the bridge opens a
//!   frame window of its own (`GuiTarget::OwnFrame`, the `frame` module), run from the outer loop of
//!   [`Bridge::run`].
//! - **Shutdown** (spike WP-15 §4): close the GUI → stop the RT thread and deactivate → destroy
//!   and unload the plugin → `_exit(0)` (the caller, `crate::run`).

#[cfg(bridge_frame)]
#[path = "frame/mod.rs"]
mod frame;

#[cfg(bridge_frame)]
use std::cell::RefCell;
use std::collections::BTreeMap;
#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(unix)]
use komponist_hosting::FdEvents;
#[cfg(x11_embed)]
use komponist_hosting::GuiParent;
use komponist_hosting::host_loop::notification_bits as bits;
use komponist_hosting::{
    ActivateConfig, HostContext, HostNotify, HostedPlugin, HostingError, PluginKind, RenderMode,
    TimerId,
};
use komponist_ipc::clap_presets::PresetReport;
use komponist_ipc::control::{PROTOCOL_VERSION, StateRestoreError};
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    BridgeChannel, BridgeMsg, BridgeServer, BridgeState, ChannelError, ControlMsg, ErrorContext,
    GuiTarget, LoadedInfo, LogLevel, ParamInfo, ParamValue, ShmRegion, WireEvent, WireEventKind,
};
#[cfg(target_os = "linux")]
use rustix::event::{PollFd, PollFlags};

#[cfg(target_os = "macos")]
use crate::cf_loop::{CfEventLoop as EventLoop, Owner};
#[cfg(target_os = "linux")]
use crate::event_loop::EventLoop;
use crate::notify::{BridgeNotify, CONTROLLER_VALUES_PENDING, LOG_PENDING};
use crate::rt::{
    ControllerValues, DeliveredParams, Fallback, PauseTimeout, RtConfig, RtThread, StartReport,
    same_param_value,
};
#[cfg(windows)]
use crate::win_loop::{ControlReader, WinEventLoop as EventLoop};
use crate::{exit, sched};

/// Every `Log` line about the RT thread's scheduling starts with this, so the app can tell it
/// apart from plugin output (it shows a status-bar warning when the level is `Warning`).
pub const RT_LOG_PREFIX: &str = "RT priority:";

/// At most one `ParamValues` message per interval (about 30 Hz).
pub const VALUES_INTERVAL: Duration = Duration::from_millis(33);
/// Bound VST3 controller sync to sixty updates per second.
const CONTROLLER_INTERVAL: Duration = Duration::from_millis(17);

/// Longest `poll()` when nothing is scheduled; the control socket and the eventfd wake it earlier.
/// (Windows: the relay's idle wait, `win_loop`.)
#[cfg(target_os = "linux")]
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// Control messages handled per tick before plugin fds and timers get their turn again.
const MESSAGES_PER_TICK: usize = 64;

/// Options of instance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Inherited fd of the bridge end of the control socket (Windows: the pipe is stdin).
    #[cfg(unix)]
    pub control_fd: RawFd,
    /// `SCHED_FIFO` priority of the RT thread (Linux).
    pub rt_priority: i32,
}

/// Runs instance mode until shutdown; the process exit code ([`crate::exit`]).
pub fn run(options: Options) -> u8 {
    let channel = match open_channel(&options) {
        Ok(channel) => channel,
        Err(message) => {
            eprintln!("komponist-bridge: {message}");
            return exit::FAILURE;
        }
    };
    let hello = BridgeMsg::Hello {
        protocol: PROTOCOL_VERSION,
        pid: std::process::id(),
    };
    if let Err(e) = channel.send(&hello) {
        eprintln!("komponist-bridge: control channel: {e}");
        return exit::FAILURE;
    }
    let region = match receive_region(&channel) {
        Ok(region) => Arc::new(region),
        Err(message) => {
            let _ = channel.send(&BridgeMsg::Error {
                context: ErrorContext::Other,
                message: format!("shared memory: {message}"),
                fatal: true,
            });
            eprintln!("komponist-bridge: shared memory: {message}");
            return exit::FAILURE;
        }
    };
    match Bridge::new(channel, region, options.rt_priority) {
        Ok(bridge) => bridge.run(),
        Err(e) => {
            eprintln!("komponist-bridge: {e}");
            exit::FAILURE
        }
    }
}

/// The bridge end of the control socket: the inherited fd of `--control-fd`.
#[cfg(unix)]
fn open_channel(options: &Options) -> Result<BridgeChannel, String> {
    // SAFETY: only asks the kernel whether the number is an open fd; nothing is closed or kept.
    let open = rustix::io::fcntl_getfd(unsafe { BorrowedFd::borrow_raw(options.control_fd) });
    if let Err(e) = open {
        return Err(format!("control fd {}: {e}", options.control_fd));
    }
    // SAFETY: the app made this inherited, open fd ours (`--control-fd`); nothing else in this
    // process owns it. A wrong kind of fd fails at the first send.
    let fd = unsafe { OwnedFd::from_raw_fd(options.control_fd) };
    // Plugins may spawn helper processes; they must not inherit the socket, or a dead bridge would
    // never close it for the app.
    let _ = rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC);
    Ok(BridgeChannel::from_fd(fd))
}

/// Windows: the bridge end of the control pipe is stdin; stdin then points at `NUL`
/// (`crate::win_stdio`). A stdin that is no pipe is refused; one that is no overlapped pipe fails
/// at the first send.
#[cfg(windows)]
fn open_channel(_options: &Options) -> Result<BridgeChannel, String> {
    crate::win_stdio::take_control_pipe()
        .map(BridgeChannel::from_handle)
        .map_err(|e| format!("control pipe (stdin): {e}"))
}

/// The shared memory the app sends right after `Hello`: the memfd (Linux) or POSIX shm fd (macOS).
#[cfg(unix)]
fn receive_region(channel: &BridgeChannel) -> Result<ShmRegion, String> {
    let fd = channel.recv_fd().map_err(|e| e.to_string())?;
    ShmRegion::open(fd).map_err(|e| e.to_string())
}

/// Windows: the section and its two events, duplicated into this process by the app.
#[cfg(windows)]
fn receive_region(channel: &BridgeChannel) -> Result<ShmRegion, String> {
    // SAFETY: the peer is the app that started this bridge and duplicated these handles for it
    // (`Channel::send_region`).
    let handles = unsafe { channel.recv_region() }.map_err(|e| e.to_string())?;
    ShmRegion::open(handles).map_err(|e| e.to_string())
}

/// The processing state while active.
struct Active {
    rt: RtThread,
    config: ActivateConfig,
}

/// One bridge instance: the control channel, the shared memory, the main loop and the plugin.
struct Bridge {
    // Drop order: the plugin (and with it the RT thread's processor) before the loop it uses.
    active: Option<Active>,
    plugin: Option<Box<dyn HostedPlugin>>,
    /// Shared with the frame window (macOS, Windows), which reports its keys and header clicks itself, and
    /// with the control reader thread (Windows).
    channel: Arc<BridgeChannel>,
    /// Windows: the thread that reads the control pipe and queues its messages.
    #[cfg(windows)]
    control: ControlReader,
    region: Arc<ShmRegion>,
    event_loop: Rc<EventLoop>,
    notify: Arc<BridgeNotify>,
    rt_priority: i32,
    /// Outcome of the last realtime set-up that was reported (scheduling, work interval; reported
    /// again on change).
    rt_reported: Option<(bool, bool)>,
    /// Values for the next `ParamValues`, by id (latest wins).
    pending_values: BTreeMap<u32, f64>,
    values_due: Option<Instant>,
    values_sent: Option<Instant>,
    params_due: Option<Instant>,
    /// The next state capture must wait for the audio block that applies restored bases.
    verify_restored_bases: bool,
    /// Checked snapshots in a row that a value moved under while audio ran (see
    /// `PAUSED_SNAPSHOT_AFTER`).
    unstable_snapshots: u8,
    /// How long the last checked snapshot's `save_state` took.
    last_save: Duration,
    initial_activation: bool,
    startup_state_restored: bool,
    controller_due: Option<Instant>,
    controller_sent: Option<Instant>,
    shutdown: bool,
    /// The plugin frame window this bridge owns (macOS #52, Windows W6).
    #[cfg(bridge_frame)]
    frame: frame::Slot,
}

impl Bridge {
    fn new(
        channel: BridgeChannel,
        region: Arc<ShmRegion>,
        rt_priority: i32,
    ) -> std::io::Result<Bridge> {
        let channel = Arc::new(channel);
        #[cfg(windows)]
        let control = ControlReader::start(channel.clone())?;
        Ok(Bridge {
            #[cfg(windows)]
            control,
            active: None,
            plugin: None,
            channel,
            region,
            #[cfg(unix)]
            event_loop: EventLoop::new(),
            #[cfg(windows)]
            event_loop: EventLoop::new()?,
            notify: Arc::new(BridgeNotify::new()?),
            rt_priority,
            rt_reported: None,
            pending_values: BTreeMap::new(),
            values_due: None,
            values_sent: None,
            params_due: None,
            verify_restored_bases: false,
            unstable_snapshots: 0,
            last_save: Duration::ZERO,
            initial_activation: true,
            startup_state_restored: false,
            controller_due: None,
            controller_sent: None,
            shutdown: false,
            #[cfg(bridge_frame)]
            frame: frame::Slot::default(),
        })
    }

    /// The main loop (Linux): one `poll()` per tick; returns after `Shutdown` or when the app is
    /// gone.
    #[cfg(target_os = "linux")]
    fn run(mut self) -> u8 {
        while !self.shutdown {
            let now = Instant::now();
            let wait = self
                .value_deadline()
                .map_or(IDLE_WAIT, |due| due.saturating_duration_since(now))
                .min(IDLE_WAIT);
            let (control, ready) = {
                let mut extra = [
                    PollFd::from_borrowed_fd(self.channel.as_fd(), PollFlags::IN),
                    PollFd::from_borrowed_fd(self.notify.wake_fd(), PollFlags::IN),
                ];
                let ready = self.event_loop.poll(&mut extra, wait);
                (extra[0].revents(), ready)
            };
            self.dispatch(ready.fds, ready.timers, !control.is_empty());
        }
        self.close();
        exit::SUCCESS
    }

    /// The main loop (Windows, ports W5 and W6): the main thread pumps window messages
    /// (`WinEventLoop::run`), and the relay thread of `win_loop` wakes it for the control reader's
    /// event, the notification event and the next deadline with a message to its wake window,
    /// whose procedure calls [`Bridge::drain_ready`] (outside every borrow of the loop; a busy
    /// bridge is retried). Returns after `Shutdown` or when the app is gone.
    ///
    /// **The plugin frame (W6):** as on macOS, `OpenGui` records a frame request and stops the
    /// pump; this loop then runs `eframe::run_native` (run-and-return, `frame::run`) right here,
    /// outside every borrow of the bridge, and pumps again when the frame has closed. eframe's
    /// (winit's) message loop dispatches the wake messages too, so control messages, plugin timers
    /// and notifications keep being served while the frame is open, during the user's drags and
    /// inside a plugin's modal dialog loop; protocol work never runs from egui's `ui()`.
    #[cfg(windows)]
    fn run(self) -> u8 {
        let event_loop = self.event_loop.clone();
        // Before the first window of the process (the relay's wake window).
        crate::win_loop::per_monitor_dpi();
        if let Err(e) = event_loop.start_relay(&[self.control.event(), self.notify.wake_handle()]) {
            eprintln!("komponist-bridge: wake relay: {e}");
            return exit::FAILURE;
        }
        let bridge = Rc::new(RefCell::new(self));
        let weak = Rc::downgrade(&bridge);
        event_loop.set_dispatch(Some(Rc::new(move || {
            let Some(bridge) = weak.upgrade() else {
                return true;
            };
            let Ok(mut bridge) = bridge.try_borrow_mut() else {
                return false;
            };
            bridge.drain_ready();
            true
        })));
        while !bridge.borrow().shutdown {
            let request = bridge.borrow_mut().frame.request.take();
            match request {
                Some(request) => frame::run(&bridge, request),
                None => event_loop.run(),
            }
        }
        event_loop.set_dispatch(None);
        bridge.borrow_mut().close();
        exit::SUCCESS
    }

    /// One wake of the relay (Windows): due timers, notifications, control messages, then the
    /// next deadline for the relay.
    #[cfg(windows)]
    fn drain_ready(&mut self) {
        let timers = self.event_loop.take_due();
        // Always look at the control queue: it is cheap, and the event says no more than that.
        self.dispatch(timers, true);
        if self.shutdown {
            // An open frame closes first: its event loop runs inside the outer loop.
            frame::close(self, true);
            self.event_loop.stop();
        }
        self.rearm();
    }

    /// Arms the relay for the next deadline: a plugin timer, `ParamValues`, the frame's debounced
    /// work (Windows).
    #[cfg(windows)]
    fn rearm(&mut self) {
        let due = [
            self.event_loop.next_timer(),
            self.value_deadline(),
            frame::deadline(self),
        ]
        .into_iter()
        .flatten()
        .min();
        self.event_loop.rearm(due);
    }

    /// The frame changed its debounced work (a drag, a move, a new scale): macOS lets the bridge
    /// handle what queued and re-arm its timer ([`Bridge::drain_ready`]); Windows only re-arms the
    /// relay (the frame calls this from eframe's `logic()`, where no control message is handled).
    #[cfg(bridge_frame)]
    fn reschedule(&mut self) {
        #[cfg(target_os = "macos")]
        self.drain_ready();
        #[cfg(windows)]
        self.rearm();
    }

    /// The main loop (macOS): the thread's CFRunLoop ([`EventLoop`] is `CfEventLoop`) with the
    /// control socket and the wake fd as sources; returns after `Shutdown` or when the app is
    /// gone.
    ///
    /// Every callout ends in [`Bridge::drain_ready`] unless the bridge is busy already (a plugin
    /// call running a nested run loop); the busy dispatch then drains what queued up.
    ///
    /// **The plugin frame (#52):** the outer loop leaves `CFRunLoopRun` for the frame window.
    /// `OpenGui` records a frame request and calls `event_loop.stop()`; this loop then runs
    /// `eframe::run_native` (run-and-return, `frame::run`) right here, outside every borrow of the
    /// bridge, and enters `event_loop.run()` again when the frame has closed. All sources stay
    /// installed in the common modes, so control messages, plugin timers and notifications keep
    /// being served inside eframe's event loop and during live resize; protocol work never runs
    /// from egui's `ui()` (eframe skips it for occluded windows, spike #42 Q5).
    #[cfg(target_os = "macos")]
    fn run(self) -> u8 {
        let event_loop = self.event_loop.clone();
        let watched = event_loop
            .watch(self.channel.as_fd(), Owner::Control)
            .and_then(|()| event_loop.watch(self.notify.wake_fd(), Owner::Wake));
        if let Err(e) = watched {
            eprintln!("komponist-bridge: {e}");
            return exit::FAILURE;
        }
        let bridge = Rc::new(RefCell::new(self));
        let weak = Rc::downgrade(&bridge);
        event_loop.set_dispatch(Some(Rc::new(move || {
            if let Some(bridge) = weak.upgrade()
                && let Ok(mut bridge) = bridge.try_borrow_mut()
            {
                bridge.drain_ready();
            }
        })));
        while !bridge.borrow().shutdown {
            let request = bridge.borrow_mut().frame.request.take();
            match request {
                Some(request) => frame::run(&bridge, request),
                None => event_loop.run(),
            }
        }
        event_loop.set_dispatch(None);
        bridge.borrow_mut().close();
        exit::SUCCESS
    }

    /// Dispatches everything the run loop queued until the queue stays empty, then arms the
    /// sources again for the next round (macOS).
    #[cfg(target_os = "macos")]
    fn drain_ready(&mut self) {
        loop {
            let ready = self.event_loop.take_ready();
            if ready.is_empty() {
                break;
            }
            self.dispatch(ready.fds, ready.timers, ready.control);
            if self.shutdown {
                // An open frame closes first: its event loop runs inside this run loop.
                frame::close(self, true);
                self.event_loop.stop();
                return;
            }
        }
        let frame_due = frame::deadline(self);
        let due = match (self.value_deadline(), frame_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.event_loop.rearm(due);
    }

    /// One tick: ready plugin fds and due timers (each only while still registered), plugin
    /// notifications, control messages (if the socket is readable), queued log lines, and the
    /// coalesced `ParamValues` when due.
    fn dispatch(
        &mut self,
        #[cfg(unix)] fds: Vec<(RawFd, FdEvents)>,
        timers: Vec<TimerId>,
        control: bool,
    ) {
        if let Some(plugin) = self.plugin.as_mut() {
            #[cfg(unix)]
            for (fd, events) in fds {
                if self.event_loop.fd_registered(fd) {
                    plugin.on_fd(fd, events);
                }
            }
            for timer in timers {
                if self.event_loop.timer_registered(timer) {
                    plugin.on_timer(timer);
                }
            }
        }
        self.handle_notifications();
        if control {
            self.read_control();
        }
        self.forward_logs();
        if self.params_due.is_some_and(|due| due <= Instant::now()) && self.report_params() {
            self.queue_all_values();
        }
        self.sync_controller_if_due();
        self.send_values_if_due();
        #[cfg(bridge_frame)]
        frame::tick(self);
    }

    // --------------------------------------------------------------------------------------------
    // Sending
    // --------------------------------------------------------------------------------------------

    /// Sends `msg`; a closed channel ends the bridge (the app is gone).
    fn send(&mut self, msg: BridgeMsg) {
        if let Err(e) = self.channel.send(&msg) {
            if e != ChannelError::Closed {
                eprintln!("komponist-bridge: send: {e}");
            }
            self.shutdown = true;
        }
    }

    fn error(&mut self, context: ErrorContext, message: impl Into<String>) {
        self.send(BridgeMsg::Error {
            context,
            message: message.into(),
            fatal: false,
        });
    }

    fn log(&mut self, level: LogLevel, message: impl Into<String>) {
        self.send(BridgeMsg::Log {
            level,
            message: message.into(),
        });
    }

    /// Forwards plugin log lines queued by [`BridgeNotify`].
    fn forward_logs(&mut self) {
        let logs = self.notify.take_logs();
        for (level, message) in logs.lines {
            self.log(level, message);
        }
        if logs.dropped_full > 0 {
            self.log(
                LogLevel::Warning,
                format!(
                    "{} plugin log lines dropped (queue full)",
                    logs.dropped_full
                ),
            );
        }
        if logs.dropped_rt > 0 {
            self.log(
                LogLevel::Debug,
                format!(
                    "{} plugin log lines from the audio thread dropped",
                    logs.dropped_rt
                ),
            );
        }
    }

    // --------------------------------------------------------------------------------------------
    // Parameter values
    // --------------------------------------------------------------------------------------------

    fn value_deadline(&self) -> Option<Instant> {
        self.values_due
            .into_iter()
            .chain(self.params_due)
            .chain(self.controller_due)
            .min()
    }

    fn sync_controller_if_due(&mut self) {
        if self.controller_due.is_some_and(|due| due <= Instant::now()) {
            self.sync_controller();
        }
    }

    fn sync_controller(&mut self) {
        self.controller_due = None;
        if let Some(controller) = self
            .active
            .as_ref()
            .and_then(|active| active.rt.controller())
        {
            self.apply_controller_values(&controller);
        }
    }

    fn apply_controller_values(&mut self, controller: &ControllerValues) {
        self.controller_sent = Some(Instant::now());
        for (id, value) in controller.drain() {
            if let Some(plugin) = self.plugin.as_mut() {
                plugin.sync_controller_param(id, value);
            }
            self.queue_value(id, value);
        }
    }

    /// Queues `value` of parameter `id` for the next `ParamValues`.
    fn queue_value(&mut self, id: u32, value: f64) {
        self.pending_values.insert(id, value);
        if self.values_due.is_none() {
            let now = Instant::now();
            let earliest = self.values_sent.map_or(now, |sent| sent + VALUES_INTERVAL);
            self.values_due = Some(earliest.max(now));
        }
    }

    /// Queues every parameter's current value (after a state load or a rescan).
    fn queue_all_values(&mut self) {
        let Some(plugin) = self.plugin.as_ref() else {
            return;
        };
        let values: Vec<(u32, f64)> = plugin
            .params()
            .iter()
            .filter_map(|p| plugin.param_value(p.id).map(|v| (p.id, v)))
            .collect();
        for (id, value) in values {
            self.queue_value(id, value);
        }
    }

    /// Queues the parameter values among the plugin's flush output events.
    fn queue_flushed(&mut self, events: &[WireEvent]) {
        for event in events {
            if let Some(WireEventKind::ParamValue { id, value }) = event.decode() {
                self.queue_value(id, value);
            }
        }
    }

    /// Every parameter's current value at once, outside the coalesced value updates.
    fn send_loaded_values(&mut self) {
        let Some(plugin) = self.plugin.as_ref() else {
            return;
        };
        let values = plugin
            .params()
            .into_iter()
            .filter_map(|info| {
                let value = plugin.param_value(info.id)?;
                Some(ParamValue {
                    id: info.id,
                    value,
                    text: plugin.param_text(info.id, value).unwrap_or_default(),
                })
            })
            .collect();
        self.pending_values.clear();
        self.values_due = None;
        self.send(BridgeMsg::PresetValues(values));
    }

    fn report_presets(&mut self) {
        let reports = self
            .plugin
            .as_mut()
            .map(|plugin| plugin.take_preset_reports())
            .unwrap_or_default();
        for mut report in reports {
            if let PresetReport::Loaded { startup, .. } = &mut report {
                *startup |= self.initial_activation;
            }
            let changed = matches!(report, PresetReport::Loaded { startup: false, .. });
            self.send(BridgeMsg::PresetReported(report));
            if changed {
                // The values the load set, ahead of any later state answer: the app takes
                // them as the new parameter bases.
                self.send_loaded_values();
            }
        }
    }

    fn send_values_if_due(&mut self) {
        let now = Instant::now();
        if self.values_due.is_none_or(|due| due > now) {
            return;
        }
        self.values_due = None;
        let pending = std::mem::take(&mut self.pending_values);
        let Some(plugin) = self.plugin.as_ref() else {
            return;
        };
        let values = pending
            .into_iter()
            .map(|(id, value)| ParamValue {
                id,
                value,
                text: plugin.param_text(id, value).unwrap_or_default(),
            })
            .collect();
        self.values_sent = Some(now);
        self.send(BridgeMsg::ParamValues(values));
    }

    // --------------------------------------------------------------------------------------------
    // Notifications from the plugin
    // --------------------------------------------------------------------------------------------

    fn handle_notifications(&mut self) {
        let (pending, gui_size) = self.notify.take();
        if pending & CONTROLLER_VALUES_PENDING != 0 && self.controller_due.is_none() {
            let now = Instant::now();
            self.controller_due = Some(
                self.controller_sent
                    .map_or(now, |sent| (sent + CONTROLLER_INTERVAL).max(now)),
            );
        }
        if pending & !LOG_PENDING == 0 {
            return;
        }
        let Some(plugin) = self.plugin.as_mut() else {
            return;
        };
        if pending & bits::MAIN_THREAD_CALLBACK != 0 {
            plugin.on_main_thread();
        }
        if pending & bits::PARAM_FLUSH_REQUESTED != 0 && self.active.is_none() {
            // GUI edits while inactive. While active, the plugin flushes in the next block.
            let mut out = Vec::new();
            plugin.flush_params(&[], &mut out);
            let edits: Vec<_> = out
                .iter()
                .filter_map(|event| match event.decode() {
                    Some(WireEventKind::ParamValue { id, value }) => Some((id, value)),
                    _ => None,
                })
                .collect();
            if !edits.is_empty() {
                self.send(BridgeMsg::PluginParamEdits(edits));
            }
            self.queue_flushed(&out);
        }
        if pending & bits::PARAM_EDITED != 0 {
            for (id, value) in self.notify.take_edits() {
                self.queue_value(id, value);
            }
        }
        if pending & bits::PARAMS_CHANGED != 0 && self.report_params() {
            self.queue_all_values();
        }
        if pending & bits::LATENCY_CHANGED != 0 {
            self.update_latency();
        }
        if pending & bits::PRESET_REPORTED != 0 {
            self.report_presets();
        }
        if pending & bits::STATE_DIRTY != 0 {
            self.send(BridgeMsg::StateDirty);
        }
        if pending & bits::NOTE_NAMES_CHANGED != 0 {
            let names = self
                .plugin
                .as_ref()
                .map(|p| p.note_names())
                .unwrap_or_default();
            self.send(BridgeMsg::NoteNames(names));
        }
        if pending & bits::GUI_RESIZE_REQUESTED != 0
            && let Some(size) = gui_size
        {
            #[cfg(bridge_frame)]
            if frame::owns_gui(self) {
                frame::plugin_resized(self, size);
            }
            // The app owns the frame (Linux).
            #[cfg(not(bridge_frame))]
            self.send(BridgeMsg::GuiSizeRequest(size));
        }
        if pending & bits::GUI_CLOSED != 0 {
            self.close_gui();
        }
        if pending & bits::RESTART_REQUESTED != 0 {
            self.restart();
        }
    }

    /// Publishes the plugin's latency and tells the app if it changed.
    fn update_latency(&mut self) {
        let Some(plugin) = self.plugin.as_ref() else {
            return;
        };
        let latency = plugin.latency();
        if latency != self.region.latency() {
            self.region.set_latency(latency);
            self.send(BridgeMsg::LatencyChanged(latency));
        }
    }

    /// The plugin asked for a restart: tell the app, deactivate and activate again with the same
    /// configuration, then report `Activated` (nothing to do while inactive: ports and latency are
    /// read at the next activation).
    fn restart(&mut self) {
        let Some(config) = self.active.as_ref().map(|a| a.config) else {
            return;
        };
        self.send(BridgeMsg::RestartRequested);
        self.deactivate();
        self.activate_and_report(config);
    }

    // --------------------------------------------------------------------------------------------
    // Control messages
    // --------------------------------------------------------------------------------------------

    /// Handles up to [`MESSAGES_PER_TICK`] control messages: read from the socket (Unix), or taken
    /// from the reader thread's queue (Windows; what is left keeps its event set, so the next
    /// wait returns at once).
    fn read_control(&mut self) {
        for _ in 0..MESSAGES_PER_TICK {
            if self.shutdown {
                return;
            }
            #[cfg(unix)]
            let received = self.channel.try_recv();
            #[cfg(windows)]
            let received = self.control.try_next().transpose();
            match received {
                Ok(Some(msg)) => self.handle(msg),
                Ok(None) => return,
                Err(ChannelError::Codec(message)) => {
                    // The channel stays in sync after a bad message; report it and go on.
                    self.error(
                        ErrorContext::Other,
                        format!("bad control message: {message}"),
                    );
                }
                Err(e) => {
                    if e != ChannelError::Closed {
                        eprintln!("komponist-bridge: receive: {e}");
                    }
                    self.shutdown = true;
                    return;
                }
            }
        }
    }

    fn handle(&mut self, msg: ControlMsg) {
        match msg {
            ControlMsg::GetDeviceAssets(request) => self.report_device_assets(request),
            ControlMsg::SetDeviceAssets(request, bytes) => self.set_device_assets(request, &bytes),
            ControlMsg::PrepareDeviceAssetEdit {
                request,
                edits,
                state,
            } => {
                use komponist_ipc::device_assets::edit_state::{Rejected, State};
                let prepared = self.plugin.as_ref().ok_or(Rejected).and_then(|plugin| {
                    plugin.prepare_asset_edit_state(&edits, state.as_ref().map(State::as_bytes))
                });
                self.send(BridgeMsg::DeviceAssetEditPrepared(request, prepared));
            }
            ControlMsg::PrepareDeviceOffline(request, work) => {
                self.prepare_device_offline(request, work);
            }
            ControlMsg::Load {
                format,
                bundle,
                plugin_id,
            } => self.load(format, &bundle, &plugin_id),
            ControlMsg::Activate {
                sample_rate,
                max_block,
            } => {
                if self.active.is_some() {
                    return self.error(ErrorContext::Activate, "already active");
                }
                if max_block == 0 || max_block as usize > MAX_BLOCK {
                    return self.error(
                        ErrorContext::Activate,
                        format!("max_block {max_block} is not in 1 ..= {MAX_BLOCK}"),
                    );
                }
                self.activate_and_report(ActivateConfig {
                    sample_rate,
                    min_frames: 1,
                    max_frames: max_block,
                });
            }
            ControlMsg::Deactivate => self.deactivate(),
            ControlMsg::GetState { request } => {
                if self.verify_restored_bases {
                    // StateApplied only acknowledges the overlay was queued. A checked capture
                    // also waits for a block that already pulled the overlay off the queue.
                    self.get_state_checked(request, &[], 0);
                    return;
                }
                self.sync_controller();
                let Some(plugin) = self.plugin.as_mut() else {
                    return self.error(ErrorContext::State, "no plugin loaded");
                };
                match plugin.save_state() {
                    Ok(state) => self.send(BridgeMsg::State {
                        request,
                        state,
                        verified: Vec::new(),
                    }),
                    Err(e) => self.error(ErrorContext::State, e.to_string()),
                }
            }
            ControlMsg::GetStateChecked {
                request,
                expected,
                budget_ms,
            } => {
                self.get_state_checked(request, &expected, budget_ms);
            }
            ControlMsg::SetState { request, state } => {
                self.set_state(request, &state, BTreeMap::new())
            }
            ControlMsg::SetClapState {
                request,
                state,
                parameter_bases,
            } => {
                self.set_state(request, &state, parameter_bases);
            }
            ControlMsg::LoadClapPreset(request, preset) => self.load_clap_preset(request, &preset),
            ControlMsg::CaptureClapPreset(request) => self.capture_clap_preset(request),
            ControlMsg::ListParams => {
                // Answered in every state (an empty list before `Load`).
                self.report_params();
            }
            ControlMsg::GetWaveformMarkers(request, slot) => {
                use komponist_ipc::waveform_markers::MAX_SNAPSHOT_BYTES;

                let snapshot = self
                    .plugin
                    .as_ref()
                    .and_then(|plugin| plugin.waveform_markers(slot));
                let bytes = snapshot.and_then(|snapshot| {
                    let mut bytes = vec![0; MAX_SNAPSHOT_BYTES];
                    let len = snapshot.encode(&mut bytes)?;
                    bytes.truncate(len);
                    Some(bytes)
                });
                self.send(BridgeMsg::WaveformMarkers(request, bytes));
            }
            ControlMsg::EditWaveformMarkers {
                request,
                slot,
                edit,
                expected,
            } => self.edit_waveform_markers(request, slot, &edit, &expected),
            ControlMsg::GetDeviceDisplay(request) => {
                let bytes = self
                    .plugin
                    .as_ref()
                    .and_then(|plugin| plugin.device_display())
                    .and_then(|display| display.encode());
                self.send(BridgeMsg::DeviceDisplay(request, bytes));
            }
            ControlMsg::PreviewWaveform { slot, time } => {
                if let Some(plugin) = &self.plugin {
                    plugin.preview_waveform(slot, time);
                }
            }
            ControlMsg::ParseParamText { request, id, text } => {
                let value = self.plugin.as_ref().and_then(|plugin| {
                    if text.len() <= 256 {
                        plugin.parse_param_text(id, &text)
                    } else {
                        None
                    }
                });
                self.send(BridgeMsg::ParamTextParsed { request, id, value });
            }
            ControlMsg::SetParam { id, value } => self.set_param(id, value),
            ControlMsg::OpenGui(target) => self.open_gui(target),
            ControlMsg::SetGuiSize(size) => {
                #[cfg(bridge_frame)]
                if frame::owns_gui(self) {
                    return frame::set_body_size(self, size);
                }
                let Some(plugin) = self.plugin.as_mut() else {
                    return self.error(ErrorContext::Gui, "no plugin loaded");
                };
                match plugin.gui_set_size(size) {
                    // The plugin kept another size (fixed size or constraints): the frame follows.
                    Ok(adopted) if adopted != size => self.send(BridgeMsg::GuiSizeRequest(adopted)),
                    Ok(_) => {}
                    Err(e) => self.error(ErrorContext::Gui, e.to_string()),
                }
            }
            ControlMsg::CloseGui => self.close_gui(),
            // The own frame's messages; there is none on Linux.
            #[cfg(bridge_frame)]
            ControlMsg::HeaderState { header, lang } => frame::set_header(self, header, &lang),
            #[cfg(bridge_frame)]
            ControlMsg::ActivateFrame => frame::activate(self),
            #[cfg(bridge_frame)]
            ControlMsg::RaiseFrame => frame::raise(self),
            #[cfg(not(bridge_frame))]
            ControlMsg::HeaderState { .. } | ControlMsg::ActivateFrame | ControlMsg::RaiseFrame => {
            }
            ControlMsg::SetRenderMode(mode) => self.set_render_mode(mode),
            ControlMsg::Shutdown => self.shutdown = true,
            ControlMsg::Ping { seq } => self.send(BridgeMsg::Pong { seq }),
        }
    }

    /// `OpenGui`: into the app's X11 child (Linux) or the bridge's own frame (macOS, Windows).
    /// Answer: `GuiOpened` (for the own frame once the plugin is attached) or `Error`.
    fn open_gui(&mut self, target: GuiTarget) {
        let Some(plugin) = self.plugin.as_mut() else {
            return self.error(ErrorContext::Gui, "no plugin loaded");
        };
        if !plugin.has_gui() {
            return self.error(ErrorContext::Gui, "the plugin has no embeddable GUI");
        }
        match target {
            #[cfg(x11_embed)]
            GuiTarget::X11Child { xid, scale } => {
                match plugin.gui_open(GuiParent::X11 { xid, scale }) {
                    Ok(info) => self.send(BridgeMsg::GuiOpened {
                        size: info.size,
                        resizable: info.resizable,
                    }),
                    Err(e) => self.error(ErrorContext::Gui, e.to_string()),
                }
            }
            #[cfg(bridge_frame)]
            GuiTarget::OwnFrame { header, lang, rect } => {
                if let Err(message) = frame::request(self, header, &lang, rect) {
                    self.error(ErrorContext::Gui, message);
                }
            }
            _ => self.error(ErrorContext::Gui, "this GUI target is for another platform"),
        }
    }

    /// `CloseGui`, or the plugin closed its GUI: closes it (and the own frame) and answers
    /// `GuiClosed`.
    fn close_gui(&mut self) {
        #[cfg(bridge_frame)]
        if frame::owns_gui(self) {
            return frame::close(self, false);
        }
        // CLAP: the host destroys a GUI the plugin closed.
        if let Some(plugin) = self.plugin.as_mut() {
            plugin.gui_close();
        }
        self.send(BridgeMsg::GuiClosed { rect: None });
    }

    fn load(
        &mut self,
        format: komponist_ipc::PluginFormat,
        bundle: &std::path::Path,
        plugin_id: &str,
    ) {
        if self.plugin.is_some() {
            return self.error(ErrorContext::Load, "a plugin is already loaded");
        }
        let context = HostContext {
            host_loop: self.event_loop.clone(),
            notify: self.notify.clone(),
            host_name: "komponist",
            host_version: env!("CARGO_PKG_VERSION"),
        };
        let plugin = match komponist_hosting::load(format, bundle, plugin_id, context) {
            Ok(plugin) => plugin,
            Err(e) => return self.error(ErrorContext::Load, e.to_string()),
        };
        let params = plugin.params();
        let values = params
            .iter()
            .map(|p| {
                let value = plugin.param_value(p.id).unwrap_or(p.default);
                ParamValue {
                    id: p.id,
                    value,
                    text: plugin.param_text(p.id, value).unwrap_or_default(),
                }
            })
            .collect();
        let native_layout = self.layout_for_params(plugin.as_ref(), &params);
        let taps = plugin.signal_taps().to_vec();
        let info = LoadedInfo {
            descriptor: plugin.descriptor().clone(),
            params,
            values,
            latency: plugin.latency(),
            note_names: plugin.note_names(),
            has_gui: plugin.has_gui(),
            native_layout,
        };
        self.region.set_latency(info.latency);
        self.plugin = Some(plugin);
        self.region.set_bridge_state(BridgeState::Loaded);
        self.send(BridgeMsg::Loaded(Box::new(info)));
        if !taps.is_empty() {
            self.send(BridgeMsg::SignalTaps(taps));
        }
    }

    fn layout_for_params(
        &self,
        plugin: &dyn HostedPlugin,
        params: &[ParamInfo],
    ) -> Option<Vec<u8>> {
        let (bytes, layout) = plugin.native_layout()?;
        match layout.validate_params(params) {
            Ok(()) => Some(bytes.to_vec()),
            Err(error) => {
                self.notify.log(LogLevel::Warning, &error.to_string());
                None
            }
        }
    }

    fn report_params(&mut self) -> bool {
        let params = self.plugin.as_ref().map(|p| p.params()).unwrap_or_default();
        if let Some(active) = self.active.as_mut()
            && !active
                .rt
                .refresh_parameter_ids(params.iter().map(|param| param.id).collect())
        {
            // Retry after audio has had time to retire the preceding table.
            self.params_due = Some(Instant::now() + VALUES_INTERVAL);
            return false;
        }
        self.params_due = None;
        let native_layout = self
            .plugin
            .as_ref()
            .and_then(|plugin| self.layout_for_params(plugin.as_ref(), &params));
        self.send(BridgeMsg::Params {
            params,
            native_layout,
        });
        true
    }

    fn get_state_checked(&mut self, request: u32, expected: &[(u32, f64, bool)], budget_ms: u32) {
        self.sync_controller();
        let Some(plugin) = self.plugin.as_mut() else {
            return self.error(ErrorContext::State, "no plugin loaded");
        };
        // A control message can update a VST3 controller before its processor sees the edit.
        let queued = self
            .active
            .as_ref()
            .is_some_and(|active| active.rt.has_queued_params());
        let strict_mismatch = expected.iter().any(|&(id, value, allow_quantized)| {
            !allow_quantized
                && !plugin
                    .param_value(id)
                    .is_some_and(|actual| same_param_value(actual, value))
        });
        if queued || strict_mismatch {
            self.send(BridgeMsg::StateDeferred { request });
            return;
        }
        let controller = self
            .active
            .as_ref()
            .and_then(|active| active.rt.controller());
        let mut updates = Vec::new();
        // Audio keeps running while the plugin serializes (CLAP allows `state.save` on the
        // main thread during processing), so the shared bridge state stays `Active`. Only the
        // two captures at the block boundaries below pause audio, and only for the in-flight
        // block plus the capture: they read evidence that the RT loop writes without atomics.
        // That evidence counts every change of a verified value, so the second capture rejects
        // a value that moved during the save even when it came back (macro or automation
        // output, an editor edit). The exception is the paused fallback below.
        let budget = Duration::from_millis(u64::from(budget_ms));
        // After an unstable attempt, a caller whose remaining time cannot fit another running
        // attempt plus a paused one goes straight to the paused one.
        let tight = self.unstable_snapshots >= 1 && budget_ms != 0 && budget < 2 * self.last_save;
        let paused = self.unstable_snapshots >= PAUSED_SNAPSHOT_AFTER || tight;
        let mut unstable = false;
        let mut save_took = None;
        let mut timed_save = |plugin: &mut dyn HostedPlugin| {
            let started = Instant::now();
            let state = plugin.save_state();
            save_took = Some(started.elapsed());
            state.map_err(|error| SnapshotError::Failed(error.to_string()))
        };
        let result = if let Some(active) = self.active.as_mut()
            && paused
        {
            // A value that the app or the plugin moves every block would defer forever. Hold
            // audio for this one snapshot: nothing can move, so it completes (the old behavior).
            // The fallback is used up when it starts, whatever its outcome.
            self.unstable_snapshots = 0;
            self.region.set_bridge_state(BridgeState::Loaded);
            let result = active.rt.snapshot(|delivered| {
                let Some((verified, changes)) = capture_verified(
                    plugin.as_mut(),
                    expected,
                    controller.as_deref(),
                    &mut updates,
                    Some(&delivered),
                ) else {
                    return Ok(None);
                };
                let state = timed_save(plugin.as_mut())?;
                let stable = is_stable(
                    plugin.as_mut(),
                    &verified,
                    &changes,
                    controller.as_deref(),
                    &mut updates,
                    Some(&delivered),
                );
                unstable = !stable;
                Ok(stable.then_some((state, verified)))
            });
            self.region.set_bridge_state(BridgeState::Active);
            result.unwrap_or_else(|timeout| Err(timeout.into()))
        } else if let Some(active) = self.active.as_mut() {
            let before = active.rt.snapshot(|delivered| {
                capture_verified(
                    plugin.as_mut(),
                    expected,
                    controller.as_deref(),
                    &mut updates,
                    Some(&delivered),
                )
            });
            match before {
                Ok(Some((verified, changes))) => timed_save(plugin.as_mut()).and_then(|state| {
                    let stable = active.rt.snapshot(|delivered| {
                        is_stable(
                            plugin.as_mut(),
                            &verified,
                            &changes,
                            controller.as_deref(),
                            &mut updates,
                            Some(&delivered),
                        )
                    })?;
                    unstable = !stable;
                    Ok(stable.then_some((state, verified)))
                }),
                Ok(None) => Ok(None),
                Err(timeout) => Err(timeout.into()),
            }
        } else {
            match capture_verified(
                plugin.as_mut(),
                expected,
                controller.as_deref(),
                &mut updates,
                None,
            ) {
                Some((verified, changes)) => timed_save(plugin.as_mut()).map(|state| {
                    is_stable(
                        plugin.as_mut(),
                        &verified,
                        &changes,
                        controller.as_deref(),
                        &mut updates,
                        None,
                    )
                    .then_some((state, verified))
                }),
                None => Ok(None),
            }
        };
        if let Some(took) = save_took {
            self.last_save = took;
        }
        if matches!(result, Ok(Some(_))) {
            self.verify_restored_bases = false;
            self.unstable_snapshots = 0;
        } else if unstable {
            self.unstable_snapshots = self.unstable_snapshots.saturating_add(1);
        }
        for (id, value) in updates {
            self.queue_value(id, value);
        }
        match result {
            Ok(Some((state, verified))) => {
                // Shared-memory edits have no control-path echo. Use the already
                // debounced, verified delivery to refresh custom display text.
                for &(id, value) in &verified {
                    if self
                        .plugin
                        .as_ref()
                        .and_then(|plugin| plugin.native_layout())
                        .and_then(|(_, layout)| layout.parameter_display(id))
                        .is_some_and(|display| display.plugin_text)
                    {
                        self.queue_value(id, value);
                    }
                }
                self.send(BridgeMsg::State {
                    request,
                    state,
                    verified,
                });
            }
            // A capture that timed out waiting for the current block is a retry, not a failure.
            Ok(None) | Err(SnapshotError::Timeout) => {
                self.send(BridgeMsg::StateDeferred { request });
            }
            Err(SnapshotError::Failed(error)) => self.error(ErrorContext::State, error),
        }
    }

    fn set_state(&mut self, request: u32, state: &[u8], bases: BTreeMap<u32, f64>) {
        if self.initial_activation {
            self.startup_state_restored = true;
        }
        let untouched = |message| StateRestoreError {
            message,
            changed_values: Vec::new(),
        };
        let result = (|| {
            let plugin = self
                .plugin
                .as_mut()
                .ok_or_else(|| untouched("no plugin loaded".to_owned()))?;
            if !bases.is_empty()
                && (plugin.descriptor().format != komponist_ipc::PluginFormat::Clap
                    || bases.len() > komponist_ipc::control::MAX_STATE_PARAMETER_BASES
                    || bases.values().any(|value| !value.is_finite()))
            {
                return Err(untouched("invalid host parameter state".to_owned()));
            }
            let active_restore = self.active.is_some();
            let mut apply_on_audio = BTreeMap::new();
            let mut load = || {
                if active_restore {
                    // The reports preceding this restore describe the outgoing state.
                    plugin.take_preset_reports();
                }
                let before: BTreeMap<_, _> = plugin
                    .params()
                    .into_iter()
                    .filter_map(|info| Some((info.id, plugin.param_value(info.id)?)))
                    .collect();
                let load_result = plugin.load_state(state);
                let infos = plugin.params();
                if load_result.is_ok() && !bases.is_empty() {
                    let values = infos.iter().filter_map(|info| {
                        let &value = bases.get(&info.id)?;
                        let excluded = komponist_ipc::control::param_flags::HIDDEN
                            | komponist_ipc::control::param_flags::READ_ONLY
                            | komponist_ipc::control::param_flags::BYPASS;
                        (info.flags & excluded == 0
                            && info.min.is_finite()
                            && info.max.is_finite()
                            && info.min <= info.max)
                            .then(|| (info.id, value.clamp(info.min, info.max)))
                    });
                    if active_restore {
                        apply_on_audio.extend(values);
                    } else {
                        let input: Vec<_> = values
                            .map(|(id, value)| {
                                WireEvent::new(0, WireEventKind::ParamValue { id, value })
                            })
                            .collect();
                        plugin.flush_params(&input, &mut Vec::new());
                    }
                }
                let values = infos
                    .into_iter()
                    .filter_map(|info| {
                        let value = apply_on_audio
                            .get(&info.id)
                            .copied()
                            .or_else(|| plugin.param_value(info.id))?;
                        if load_result.is_err() && before.get(&info.id) == Some(&value) {
                            return None;
                        }
                        Some(ParamValue {
                            id: info.id,
                            value,
                            text: plugin.param_text(info.id, value).unwrap_or_default(),
                        })
                    })
                    .collect::<Vec<_>>();
                match load_result {
                    Ok(()) => (Ok(values), true),
                    Err(error) => {
                        let changed = !values.is_empty();
                        (
                            Err(StateRestoreError {
                                message: error.to_string(),
                                changed_values: values,
                            }),
                            changed,
                        )
                    }
                }
            };
            let (result, superseded) = if let Some(active) = self.active.as_mut() {
                // Stop new requests at the client so it can retain important events while we
                // wait for the block already in flight.
                self.region.set_bridge_state(BridgeState::Loaded);
                let result = active.rt.restore(load);
                if matches!(&result, Ok((_, true))) {
                    self.verify_restored_bases = !apply_on_audio.is_empty();
                }
                for (id, value) in apply_on_audio {
                    // A successful restore clears the old queue. The state format bounds the
                    // overlay to the queue's capacity, before the client starts sending again.
                    assert!(active.rt.set_param(id, value));
                }
                self.region.set_bridge_state(BridgeState::Active);
                result.map_err(untouched)?
            } else {
                load()
            };
            if superseded {
                // Old coalesced values must not follow the authoritative state report.
                self.pending_values.clear();
                self.values_due = None;
            }
            result
        })();
        self.send(BridgeMsg::StateApplied { request, result });
    }

    fn load_clap_preset(&mut self, request: u32, preset: &komponist_ipc::clap_presets::PresetLoad) {
        let Some(plugin) = self.plugin.as_mut() else {
            self.send(BridgeMsg::ClapPresetApplied(
                request,
                Err("no plugin loaded".into()),
            ));
            return;
        };
        let mut rollback_failed = false;
        let load = || {
            let result = (|| {
                let (previous_state, state) =
                    plugin
                        .apply_clap_preset(preset)
                        .map_err(|(error, recovered)| {
                            rollback_failed = !recovered;
                            error
                        })?;
                let values = plugin
                    .params()
                    .into_iter()
                    .filter_map(|info| {
                        let value = plugin.param_value(info.id)?;
                        Some(ParamValue {
                            id: info.id,
                            value,
                            text: plugin.param_text(info.id, value).unwrap_or_default(),
                        })
                    })
                    .collect();
                Ok(komponist_ipc::control::AppliedClapPreset {
                    previous_state,
                    state,
                    values,
                })
            })();
            let superseded = result.is_ok() || rollback_failed;
            (result, superseded)
        };
        let result = if let Some(active) = self.active.as_mut() {
            self.region.set_bridge_state(BridgeState::Loaded);
            let result = active.rt.restore(load).and_then(|(result, _)| result);
            self.region.set_bridge_state(BridgeState::Active);
            result
        } else {
            let mut load = load;
            load().0
        };
        if result.is_ok() {
            // Values coalesced before the load must not overwrite the preset's reported values.
            self.pending_values.clear();
            self.values_due = None;
        }
        self.send(BridgeMsg::ClapPresetApplied(request, result));
        if rollback_failed {
            self.shutdown = true;
        }
    }

    fn capture_clap_preset(&mut self, request: u32) {
        if let Some(active) = self.active.as_mut()
            && (active.rt.has_queued_params() || active.rt.snapshot(|_| ()).is_err())
        {
            self.send(BridgeMsg::StateDeferred { request });
            return;
        }
        let result = if let Some(plugin) = self.plugin.as_mut() {
            plugin
                .save_preset_state()
                .map_err(|error| error.to_string())
        } else {
            Err("no plugin loaded".into())
        };
        self.send(BridgeMsg::ClapPresetCaptured(request, result));
    }

    fn report_device_assets(&mut self, request: u32) {
        use komponist_ipc::device_assets::MAX_SNAPSHOT_BYTES;

        let assets = self.plugin.as_ref().and_then(|plugin| plugin.asset_slots());
        let encoded = assets.and_then(|assets| {
            let mut bytes = vec![0; MAX_SNAPSHOT_BYTES];
            let len = assets.encode(&mut bytes)?;
            bytes.truncate(len);
            Some(bytes)
        });
        let file_slots = self
            .plugin
            .as_ref()
            .is_some_and(|plugin| plugin.supports_asset_slots());
        let offline = self
            .plugin
            .as_ref()
            .is_some_and(|plugin| plugin.supports_offline_preparation());
        let edit_state = self
            .plugin
            .as_ref()
            .is_some_and(|plugin| plugin.supports_asset_edit_state());
        self.send(BridgeMsg::DeviceAssets {
            request,
            snapshot: encoded,
            file_slots,
            offline,
            edit_state,
        });
    }

    fn set_device_assets(&mut self, request: u32, bytes: &[u8]) {
        let bindings = komponist_ipc::device_assets::Bindings::decode(bytes);
        let accepted = self
            .plugin
            .as_mut()
            .zip(bindings)
            .is_some_and(|(plugin, bindings)| plugin.set_asset_bindings(&bindings));
        self.send(BridgeMsg::DeviceAssetsApplied(request, accepted));
    }

    fn prepare_device_offline(
        &mut self,
        request: u32,
        work: Option<komponist_ipc::device_assets::OfflineRequest>,
    ) {
        use komponist_ipc::device_assets::OfflineStatus;

        let status = self.plugin.as_mut().and_then(|plugin| {
            if let Some(active) = self.active.as_mut() {
                active
                    .rt
                    .snapshot(|_| plugin.prepare_offline(work))
                    .unwrap_or(Some(OfflineStatus::Preparing))
            } else {
                plugin.prepare_offline(work)
            }
        });
        self.send(BridgeMsg::DeviceOfflineStatus(request, status));
    }

    fn edit_waveform_markers(
        &mut self,
        request: u32,
        slot: u32,
        encoded: &[u8],
        expected: &[(u32, f64)],
    ) {
        use komponist_ipc::waveform_markers::{EditResult, EditStatus, Request};

        let result = 'edit: {
            let Some(edit) = Request::decode(encoded) else {
                break 'edit EditResult::Rejected(EditStatus::Invalid);
            };
            let Some(plugin) = self.plugin.as_deref_mut() else {
                break 'edit EditResult::Rejected(EditStatus::Unavailable);
            };
            if let Some(active) = self.active.as_mut() {
                if active.rt.has_queued_params() {
                    break 'edit EditResult::Rejected(EditStatus::Unavailable);
                }
                self.region.set_bridge_state(BridgeState::Loaded);
                let result = active.rt.restore(|| {
                    let result =
                        komponist_hosting::edit_waveform_marker_state(plugin, slot, edit, expected);
                    let broken = matches!(
                        result,
                        EditResult::Failed {
                            rollback_failed: true,
                            ..
                        }
                    );
                    (result, broken)
                });
                match result {
                    Ok((result, broken)) => {
                        if broken {
                            self.shutdown = true;
                        } else {
                            self.region.set_bridge_state(BridgeState::Active);
                        }
                        result
                    }
                    Err(message) => {
                        self.region.set_bridge_state(BridgeState::Active);
                        EditResult::Failed {
                            message,
                            rollback_failed: false,
                        }
                    }
                }
            } else {
                let result =
                    komponist_hosting::edit_waveform_marker_state(plugin, slot, edit, expected);
                if matches!(
                    result,
                    EditResult::Failed {
                        rollback_failed: true,
                        ..
                    }
                ) {
                    self.shutdown = true;
                }
                result
            }
        };
        self.send(BridgeMsg::WaveformMarkersEdited(request, result));
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(plugin) = self.plugin.as_mut() else {
            return self.error(ErrorContext::Params, "no plugin loaded");
        };
        let mut out = Vec::new();
        match self.active.as_mut() {
            Some(active) => {
                if !active.rt.set_param(id, value) {
                    return self.error(
                        ErrorContext::Params,
                        format!("parameter queue full, value {value} of {id} dropped"),
                    );
                }
                if let Some(controller) = active.rt.controller() {
                    controller.invalidate(Some(id));
                }
                plugin.set_param(id, value);
            }
            None => {
                plugin.set_param(id, value);
                plugin.flush_params(
                    &[WireEvent::new(0, WireEventKind::ParamValue { id, value })],
                    &mut out,
                );
            }
        }
        self.queue_flushed(&out);
        // Echo with the plugin's text, so the generic view can show "−12.0 dB".
        self.queue_value(id, value);
    }

    fn set_render_mode(&mut self, mode: RenderMode) {
        if self.plugin.is_none() {
            return self.error(ErrorContext::Activate, "no plugin loaded");
        }
        let config = self.active.as_ref().map(|a| a.config);
        self.deactivate();
        let result = self.plugin.as_mut().map(|p| p.set_render_mode(mode));
        if let Some(Err(e)) = result {
            self.error(ErrorContext::Activate, e.to_string());
        }
        if let Some(config) = config {
            self.activate_and_report(config);
        }
    }

    // --------------------------------------------------------------------------------------------
    // Activation
    // --------------------------------------------------------------------------------------------

    /// Activates and answers `Activated` or `Error`.
    fn activate_and_report(&mut self, config: ActivateConfig) {
        match self.activate(config) {
            Ok(latency) => {
                if self.initial_activation {
                    self.report_presets();
                    self.initial_activation = false;
                }
                self.send(BridgeMsg::Activated { latency });
            }
            Err(e) => self.error(ErrorContext::Activate, e.to_string()),
        }
    }

    /// Activates the plugin and starts the RT thread; returns the latency.
    fn activate(&mut self, config: ActivateConfig) -> Result<u32, HostingError> {
        let Some(plugin) = self.plugin.as_mut() else {
            return Err(HostingError::WrongState("no plugin loaded"));
        };
        let mut processor = plugin.activate(config)?;
        // A plugin may ask for a restart from inside `activate`: nice-plug does whenever it reports
        // a latency there (it counts as activated by then). Nothing processes yet, so the restart
        // happens right here and the app sees one `Activated`. At most twice; a plugin that keeps
        // asking gets the regular restart later.
        for _ in 0..2 {
            if self.notify.take_bits(bits::MAIN_THREAD_CALLBACK) != 0 {
                plugin.on_main_thread();
            }
            if self.notify.take_bits(bits::RESTART_REQUESTED) == 0 {
                break;
            }
            plugin.deactivate(processor);
            processor = plugin.activate(config)?;
        }
        // Capture the startup state on the control thread before handing the processor
        // to the realtime worker, which may schedule main-thread preset loads.
        let baseline = if self.initial_activation
            && !self.startup_state_restored
            && plugin.descriptor().format == komponist_ipc::PluginFormat::Clap
        {
            plugin.save_state().ok()
        } else {
            None
        };
        let fallback = match plugin.descriptor().kind {
            PluginKind::Instrument => Fallback::Silence,
            PluginKind::Effect | PluginKind::MidiEffect | PluginKind::Other => {
                Fallback::Passthrough
            }
        };
        let rt_config = RtConfig {
            priority: self.rt_priority,
            // A rate of 0 never activates; a zero period then gets the policy's minimum.
            period: Duration::try_from_secs_f64(f64::from(config.max_frames) / config.sample_rate)
                .unwrap_or_default(),
            max_frames: config.max_frames,
            fallback,
        };
        let server = BridgeServer::new(self.region.clone());
        let param_ids: Vec<_> = plugin.params().into_iter().map(|param| param.id).collect();
        let controller =
            (plugin.descriptor().format == komponist_ipc::PluginFormat::Vst3).then(|| {
                Arc::new(ControllerValues::new(
                    param_ids.clone(),
                    self.notify.clone(),
                ))
            });
        let (rt, report) =
            match RtThread::start(server, processor, rt_config, controller, param_ids) {
                Ok(started) => started,
                Err(failure) => {
                    plugin.deactivate(failure.processor);
                    self.report_rt_priority(&failure.report);
                    return Err(failure.error);
                }
            };
        let latency = plugin.latency();
        self.region.set_latency(latency);
        self.region.set_bridge_state(BridgeState::Active);
        self.controller_due = None;
        self.active = Some(Active { rt, config });
        if let Some(state) = baseline {
            self.send(BridgeMsg::StartupState(state));
        }
        self.report_rt_priority(&report);
        // Right before `Activated` (sent by the caller), on every activation.
        let rt_status = BridgeMsg::RtPriority {
            granted: report.priority.is_ok(),
            priority: self.rt_priority as u32,
            detail: report
                .priority
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default(),
        };
        self.send(rt_status);
        Ok(latency)
    }

    /// Logs the realtime outcome when it is new or changed.
    fn report_rt_priority(&mut self, report: &StartReport) {
        let interval_ok = report.work_interval.is_none_or(|joined| joined.is_ok());
        let outcome = (report.priority.is_ok(), interval_ok);
        if self.rt_reported == Some(outcome) {
            return;
        }
        self.rt_reported = Some(outcome);
        let policy = sched::policy_name(self.rt_priority);
        match &report.priority {
            Ok(()) => self.log(
                LogLevel::Info,
                format!("{RT_LOG_PREFIX} the audio thread runs at {policy}"),
            ),
            Err(e) => self.log(
                LogLevel::Warning,
                format!(
                    "{RT_LOG_PREFIX} {policy} refused ({e}); the audio thread runs \
                     without realtime priority and may miss deadlines under load"
                ),
            ),
        }
        if let Some(Err(errno)) = report.work_interval {
            self.log(
                LogLevel::Warning,
                format!(
                    "{RT_LOG_PREFIX} no audio work interval (errno {errno}); the audio thread \
                     may run on slow cores and miss deadlines at light load"
                ),
            );
        }
    }

    /// Stops the RT thread and deactivates; parameter values that never reached a block go in
    /// through `flush_params`.
    fn deactivate(&mut self) {
        let Some(active) = self.active.take() else {
            return;
        };
        // From now on the app's `begin_block` returns `NotActive`.
        self.region.set_bridge_state(BridgeState::Loaded);
        let controller = active.rt.controller();
        let (processor, leftover) = active.rt.stop();
        self.controller_due = None;
        if let Some(controller) = &controller {
            // The thread can have published a final value after the last main-loop deadline.
            self.apply_controller_values(controller);
        }
        let Some(plugin) = self.plugin.as_mut() else {
            return;
        };
        plugin.deactivate(processor);
        let mut out = Vec::new();
        if !leftover.is_empty() {
            let events: Vec<WireEvent> = leftover
                .iter()
                .map(|&(id, value)| WireEvent::new(0, WireEventKind::ParamValue { id, value }))
                .collect();
            plugin.flush_params(&events, &mut out);
        }
        self.queue_flushed(&out);
    }

    /// Shutdown order: GUI, RT thread and deactivation, then destroy and unload.
    fn close(&mut self) {
        #[cfg(bridge_frame)]
        frame::close(self, true);
        if let Some(plugin) = self.plugin.as_mut() {
            plugin.gui_close();
        }
        self.deactivate();
        self.plugin = None;
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // `close` normally ran already; this covers early returns.
        self.close();
    }
}

/// After this many checked snapshots in a row that a moving value made unverifiable, the next
/// one pauses audio for its whole save. Such a value (an automated or macro-driven parameter,
/// or one the plugin moves itself) would otherwise defer every attempt while playback runs and
/// leave a save without a state. The pause is the exception: normally audio keeps running.
const PAUSED_SNAPSHOT_AFTER: u8 = 2;

/// Moves controller values produced by processing into the plugin's editor controller.
fn sync_controller(
    plugin: &mut dyn HostedPlugin,
    controller: Option<&ControllerValues>,
    updates: &mut Vec<(u32, f64)>,
) {
    if let Some(controller) = controller {
        let first = updates.len();
        updates.extend(controller.drain());
        for &(id, value) in &updates[first..] {
            plugin.sync_controller_param(id, value);
        }
    }
}

/// The verified `(id, value)` pairs of a first capture and the change count of each.
type Captured = (Vec<(u32, f64)>, Vec<u32>);

/// Why a checked snapshot has no state.
enum SnapshotError {
    /// A capture could not pause the current audio block in time: retry.
    Timeout,
    /// The plugin failed to serialize its state.
    Failed(String),
}

impl From<PauseTimeout> for SnapshotError {
    fn from(_: PauseTimeout) -> Self {
        Self::Timeout
    }
}

/// First half of a checked snapshot, run at a block boundary: the values that the app expects,
/// as the plugin holds them now, with the change count of each (0 without audio), or `None` when
/// they differ or an edit is still on its way.
fn capture_verified(
    plugin: &mut dyn HostedPlugin,
    expected: &[(u32, f64, bool)],
    controller: Option<&ControllerValues>,
    updates: &mut Vec<(u32, f64)>,
    delivered: Option<&DeliveredParams<'_>>,
) -> Option<Captured> {
    // Processing may have produced controller values after the caller's first check.
    sync_controller(plugin, controller, updates);
    // The VST3 editor can expose a new controller value before the component has processed it.
    if plugin.has_unapplied_edits() {
        return None;
    }
    let mut verified = Vec::with_capacity(expected.len());
    let mut changes = Vec::with_capacity(expected.len());
    for &(id, value, allow_quantized) in expected {
        let actual = plugin.param_value(id).filter(|actual| actual.is_finite())?;
        let processed = allow_quantized
            && delivered.is_some_and(|delivery| delivery.confirms_input(id, value, actual));
        if !same_param_value(actual, value) && !processed {
            return None;
        }
        verified.push((id, actual));
        changes.push(delivered.map_or(0, |delivery| delivery.changes(id)));
    }
    Some((verified, changes))
}

/// Second half, run at a block boundary after `save_state`: nothing the saved bytes depend on
/// moved while the plugin serialized them. A value that left and came back counts as moved.
fn is_stable(
    plugin: &mut dyn HostedPlugin,
    verified: &[(u32, f64)],
    changes: &[u32],
    controller: Option<&ControllerValues>,
    updates: &mut Vec<(u32, f64)>,
    delivered: Option<&DeliveredParams<'_>>,
) -> bool {
    sync_controller(plugin, controller, updates);
    verified.iter().zip(changes).all(|(&(id, value), &before)| {
        delivered.is_none_or(|delivery| delivery.changes(id) == before)
            && plugin
                .param_value(id)
                .is_some_and(|actual| same_param_value(actual, value))
    }) && !plugin.has_unapplied_edits()
}
