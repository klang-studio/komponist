//! Instance mode: `komponist-bridge --instance --control-fd N` (ARCHITECTURE §8.1–8.2).
//!
//! Start-up (komponist-ipc `control` docs): send `Hello`, receive the memfd and map it, then serve
//! control messages until `Shutdown` or until the app closes the socket. Everything except block
//! processing happens on the main thread, which is also the plugin's main thread:
//!
//! - **Main loop:** one `poll()` ([`EventLoop`]) over the control socket, the notification eventfd
//!   ([`BridgeNotify`]), the plugin's fds and timers. Each tick dispatches plugin fds and timers,
//!   handles notifications, reads control messages and forwards queued log lines and parameter
//!   values. Only this thread sends on the control channel.
//! - **Watchdog:** every control message is answered from this loop between two plugin calls, so a
//!   stalled loop shows as a missing answer. Until the protocol has a ping (contract change request
//!   in the WP-31 report), `ListParams` serves as the heartbeat: it is answered in every state
//!   (with an empty list before `Load`) and has no side effects.
//! - **RT thread:** started by `Activate` ([`RtThread`]), stopped by `Deactivate`, restarts, render
//!   mode changes and shutdown. Its `SCHED_FIFO` outcome is reported as a `Log` line starting with
//!   [`RT_LOG_PREFIX`] (`Info` on success, `Warning` on failure; processing goes on either way).
//! - **Parameters:** `SetParam` reaches the plugin as an event at offset 0 of the next block while
//!   active, through `flush_params` while inactive. Values the app set, values the plugin changed
//!   outside processing (flush output, VST3 edits) and all values after a state load go back as
//!   `ParamValues` with the plugin's text, coalesced to at most one message per
//!   [`VALUES_INTERVAL`]. GUI edits during processing travel through the shared output ring.
//! - **Shutdown** (spike WP-15 §4): close the GUI → stop the RT thread and deactivate → destroy
//!   and unload the plugin → exit 0.

use std::collections::BTreeMap;
use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::notification_bits as bits;
use komponist_hosting::{
    ActivateConfig, HostContext, HostedPlugin, HostingError, PluginKind, RenderMode,
};
use komponist_ipc::control::PROTOCOL_VERSION;
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    BridgeChannel, BridgeMsg, BridgeServer, BridgeState, ChannelError, ControlMsg, ErrorContext,
    LoadedInfo, LogLevel, ParamValue, ShmRegion, WireEvent, WireEventKind,
};
use rustix::event::{PollFd, PollFlags};

use crate::event_loop::EventLoop;
use crate::notify::{BridgeNotify, LOG_PENDING};
use crate::rt::{Fallback, RtConfig, RtThread};

/// Every `Log` line about the RT thread's scheduling starts with this, so the app can tell it
/// apart from plugin output (it shows a status-bar warning when the level is `Warning`).
pub const RT_LOG_PREFIX: &str = "RT priority:";

/// At most one `ParamValues` message per interval (about 30 Hz).
pub const VALUES_INTERVAL: Duration = Duration::from_millis(33);

/// Longest `poll()` when nothing is scheduled; the control socket and the eventfd wake it earlier.
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// Control messages handled per tick before plugin fds and timers get their turn again.
const MESSAGES_PER_TICK: usize = 64;

/// Options of instance mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// Inherited fd of the bridge end of the control socket.
    pub control_fd: RawFd,
    /// `SCHED_FIFO` priority of the RT thread.
    pub rt_priority: i32,
}

/// Runs instance mode until shutdown; the process exit code.
pub fn run(options: Options) -> ExitCode {
    // SAFETY: only asks the kernel whether the number is an open fd; nothing is closed or kept.
    let open = rustix::io::fcntl_getfd(unsafe { BorrowedFd::borrow_raw(options.control_fd) });
    if let Err(e) = open {
        eprintln!("komponist-bridge: control fd {}: {e}", options.control_fd);
        return ExitCode::FAILURE;
    }
    // SAFETY: the app made this inherited, open fd ours (`--control-fd`); nothing else in this
    // process owns it. A wrong kind of fd fails at the first send below.
    let fd = unsafe { OwnedFd::from_raw_fd(options.control_fd) };
    // Plugins may spawn helper processes; they must not inherit the socket, or a dead bridge would
    // never close it for the app.
    let _ = rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC);
    let channel = BridgeChannel::from_fd(fd);
    let hello = BridgeMsg::Hello {
        protocol: PROTOCOL_VERSION,
        pid: std::process::id(),
    };
    if let Err(e) = channel.send(&hello) {
        eprintln!("komponist-bridge: control fd {}: {e}", options.control_fd);
        return ExitCode::FAILURE;
    }
    let region = match channel
        .recv_fd()
        .map_err(|e| e.to_string())
        .and_then(|fd| ShmRegion::open(fd).map_err(|e| e.to_string()))
    {
        Ok(region) => Arc::new(region),
        Err(message) => {
            let _ = channel.send(&BridgeMsg::Error {
                context: ErrorContext::Other,
                message: format!("shared memory: {message}"),
                fatal: true,
            });
            eprintln!("komponist-bridge: shared memory: {message}");
            return ExitCode::FAILURE;
        }
    };
    match Bridge::new(channel, region, options.rt_priority) {
        Ok(bridge) => bridge.run(),
        Err(e) => {
            eprintln!("komponist-bridge: {e}");
            ExitCode::FAILURE
        }
    }
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
    channel: BridgeChannel,
    region: Arc<ShmRegion>,
    event_loop: Rc<EventLoop>,
    notify: Arc<BridgeNotify>,
    rt_priority: i32,
    /// Outcome of the last `SCHED_FIFO` attempt that was reported (reported again on change).
    rt_reported: Option<bool>,
    /// Values for the next `ParamValues`, by id (latest wins).
    pending_values: BTreeMap<u32, f64>,
    values_due: Option<Instant>,
    values_sent: Option<Instant>,
    shutdown: bool,
}

impl Bridge {
    fn new(
        channel: BridgeChannel,
        region: Arc<ShmRegion>,
        rt_priority: i32,
    ) -> std::io::Result<Bridge> {
        Ok(Bridge {
            active: None,
            plugin: None,
            channel,
            region,
            event_loop: EventLoop::new(),
            notify: Arc::new(BridgeNotify::new()?),
            rt_priority,
            rt_reported: None,
            pending_values: BTreeMap::new(),
            values_due: None,
            values_sent: None,
            shutdown: false,
        })
    }

    /// The main loop; returns after `Shutdown` or when the app is gone.
    fn run(mut self) -> ExitCode {
        while !self.shutdown {
            let now = Instant::now();
            let wait = self
                .values_due
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
            if let Some(plugin) = self.plugin.as_mut() {
                for (fd, events) in ready.fds {
                    if self.event_loop.fd_registered(fd) {
                        plugin.on_fd(fd, events);
                    }
                }
                for timer in ready.timers {
                    if self.event_loop.timer_registered(timer) {
                        plugin.on_timer(timer);
                    }
                }
            }
            self.handle_notifications();
            if !control.is_empty() {
                self.read_control();
            }
            self.forward_logs();
            self.send_values_if_due();
        }
        self.close();
        ExitCode::SUCCESS
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
            self.queue_flushed(&out);
        }
        if pending & bits::PARAM_EDITED != 0 {
            for (id, value) in self.notify.take_edits() {
                self.queue_value(id, value);
            }
        }
        if pending & bits::PARAMS_CHANGED != 0 {
            let params = self.plugin.as_ref().map(|p| p.params()).unwrap_or_default();
            self.send(BridgeMsg::Params(params));
            self.queue_all_values();
        }
        if pending & bits::LATENCY_CHANGED != 0 {
            self.update_latency();
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
            self.send(BridgeMsg::GuiSizeRequest(size));
        }
        if pending & bits::GUI_CLOSED != 0 {
            // CLAP: the host destroys a GUI the plugin closed.
            if let Some(plugin) = self.plugin.as_mut() {
                plugin.gui_close();
            }
            self.send(BridgeMsg::GuiClosed);
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

    fn read_control(&mut self) {
        for _ in 0..MESSAGES_PER_TICK {
            if self.shutdown {
                return;
            }
            match self.channel.try_recv() {
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
                let Some(plugin) = self.plugin.as_mut() else {
                    return self.error(ErrorContext::State, "no plugin loaded");
                };
                match plugin.save_state() {
                    Ok(state) => self.send(BridgeMsg::State { request, state }),
                    Err(e) => self.error(ErrorContext::State, e.to_string()),
                }
            }
            ControlMsg::SetState { state } => {
                let Some(plugin) = self.plugin.as_mut() else {
                    return self.error(ErrorContext::State, "no plugin loaded");
                };
                match plugin.load_state(&state) {
                    Ok(()) => self.queue_all_values(),
                    Err(e) => self.error(ErrorContext::State, e.to_string()),
                }
            }
            ControlMsg::ListParams => {
                // Also the heartbeat: answered in every state.
                let params = self.plugin.as_ref().map(|p| p.params()).unwrap_or_default();
                self.send(BridgeMsg::Params(params));
            }
            ControlMsg::SetParam { id, value } => self.set_param(id, value),
            ControlMsg::OpenGui { parent_xid, scale } => {
                let Some(plugin) = self.plugin.as_mut() else {
                    return self.error(ErrorContext::Gui, "no plugin loaded");
                };
                match plugin.gui_open(parent_xid, scale) {
                    Ok(info) => self.send(BridgeMsg::GuiOpened {
                        size: info.size,
                        resizable: info.resizable,
                    }),
                    Err(e) => self.error(ErrorContext::Gui, e.to_string()),
                }
            }
            ControlMsg::SetGuiSize(size) => {
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
            ControlMsg::CloseGui => {
                if let Some(plugin) = self.plugin.as_mut() {
                    plugin.gui_close();
                }
                self.send(BridgeMsg::GuiClosed);
            }
            ControlMsg::SetRenderMode(mode) => self.set_render_mode(mode),
            ControlMsg::Shutdown => self.shutdown = true,
        }
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
        let info = LoadedInfo {
            descriptor: plugin.descriptor().clone(),
            params,
            values,
            latency: plugin.latency(),
            note_names: plugin.note_names(),
            has_gui: plugin.has_gui(),
        };
        self.region.set_latency(info.latency);
        self.plugin = Some(plugin);
        self.region.set_bridge_state(BridgeState::Loaded);
        self.send(BridgeMsg::Loaded(Box::new(info)));
    }

    fn set_param(&mut self, id: u32, value: f64) {
        let Some(plugin) = self.plugin.as_mut() else {
            return self.error(ErrorContext::Params, "no plugin loaded");
        };
        plugin.set_param(id, value);
        let mut out = Vec::new();
        match self.active.as_mut() {
            Some(active) => {
                if !active.rt.set_param(id, value) {
                    return self.error(
                        ErrorContext::Params,
                        format!("parameter queue full, value {value} of {id} dropped"),
                    );
                }
            }
            None => plugin.flush_params(
                &[WireEvent::new(0, WireEventKind::ParamValue { id, value })],
                &mut out,
            ),
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
            Ok(latency) => self.send(BridgeMsg::Activated { latency }),
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
        let fallback = match plugin.descriptor().kind {
            PluginKind::Instrument => Fallback::Silence,
            PluginKind::Effect | PluginKind::Other => Fallback::Passthrough,
        };
        let rt_config = RtConfig {
            priority: self.rt_priority,
            max_frames: config.max_frames,
            fallback,
        };
        let server = BridgeServer::new(self.region.clone());
        let (rt, report) = match RtThread::start(server, processor, rt_config) {
            Ok(started) => started,
            Err(failure) => {
                plugin.deactivate(failure.processor);
                self.report_rt_priority(failure.report.priority);
                return Err(failure.error);
            }
        };
        let latency = plugin.latency();
        self.region.set_latency(latency);
        self.region.set_bridge_state(BridgeState::Active);
        self.active = Some(Active { rt, config });
        self.report_rt_priority(report.priority);
        Ok(latency)
    }

    /// Logs the `SCHED_FIFO` outcome when it is new or changed.
    fn report_rt_priority(&mut self, outcome: std::io::Result<()>) {
        if self.rt_reported == Some(outcome.is_ok()) {
            return;
        }
        self.rt_reported = Some(outcome.is_ok());
        let priority = self.rt_priority;
        match outcome {
            Ok(()) => self.log(
                LogLevel::Info,
                format!("{RT_LOG_PREFIX} the audio thread runs at SCHED_FIFO {priority}"),
            ),
            Err(e) => self.log(
                LogLevel::Warning,
                format!(
                    "{RT_LOG_PREFIX} SCHED_FIFO {priority} refused ({e}); the audio thread runs \
                     without realtime priority and may miss deadlines under load"
                ),
            ),
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
        let (processor, leftover) = active.rt.stop();
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
