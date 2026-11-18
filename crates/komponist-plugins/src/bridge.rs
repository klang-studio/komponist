//! One bridge process as the plugin manager sees it (ARCHITECTURE §8.1).
//!
//! [`Bridge::spawn`] starts `komponist-bridge --instance --control-fd N` with the bridge end of a
//! fresh control socket, watches it through a pidfd, sends the memfd (Windows: `--instance` with the
//! control pipe as the child's stdin, a process handle, the section and its events duplicated into
//! the bridge) and queues the start-up
//! messages: `Load`, `SetState` (if there is a state), `Activate`. The state goes in before the
//! activation, so the plugin starts processing with it (the bridge's own tests use this order too).
//!
//! All messages to the bridge go through a writer thread: a bridge busy in a long plugin call does
//! not read its socket, and a large state would otherwise block the UI thread in `send` until it
//! does. Messages from the bridge are read on the UI thread in [`Bridge::poll`].
//!
//! Status (`DeviceStatus`): `Loading` until the first `Activated`, then `Ok`. A failed `Load` or
//! `Activate`, a fatal error, an exited process or a missed watchdog `Pong` make it `Crashed`; the
//! bridge is then killed and the processor falls back for good.
//!
//! A stored state the plugin refuses (`SetState` fails while loading) does not: the device runs
//! with what the plugin has (its defaults), gets a state warning, and the stored blob is kept. No
//! snapshot is taken (none reported, none for saving) until the user changes something on the
//! device: a parameter set through the manager, a GUI edit during processing, or "state dirty"
//! from the running plugin. A reload tries the stored blob again.
//!
//! Watchdog: a `Ping` every [`Timing::ping_interval`], answered within [`Timing::pong_timeout`].
//! Paused while the bridge loads (until `Activated`) and while an `OpenGui` is outstanding, since
//! the bridge answers only between two plugin calls; such a phase may last at most
//! [`Timing::long_call_timeout`].

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use komponist_ipc::control::{PROTOCOL_VERSION, socket_pair};
use komponist_ipc::{
    AppChannel, BridgeClient, BridgeMonitor, BridgeMsg, ChannelError, ControlMsg, ErrorContext,
    GuiTarget, LogLevel, NoteName, ParamInfo, ParamValue, PluginFormat, ProcessWatcher, RenderMode,
    ShmRegion, WireEventKind,
};
use komponist_model::{DeviceId, DeviceRole};

use crate::external::{ExternalProcessor, RUN_ACTIVE, RUN_DEAD, Shared};
use crate::manager::{
    DeviceStatus, FailReason, PluginError, PluginManagerEvent, ProcessorSetup, STATE_LOG_PREFIX,
};

/// Watchdog and shutdown timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Timing {
    /// Time between two `Ping`s.
    pub ping_interval: Duration,
    /// How long a `Pong` may take while the bridge is not inside a long plugin call.
    pub pong_timeout: Duration,
    /// How long loading (until `Activated`) or opening the GUI may take.
    pub long_call_timeout: Duration,
    /// Debounce of state snapshots after "state dirty" or a parameter change.
    pub snapshot_debounce: Duration,
    /// How long a device stays `Loading` after its bridge reported it active (also after a
    /// re-activation), see [`crate::WARM_UP`].
    pub warm_up: Duration,
}

impl Default for Timing {
    fn default() -> Timing {
        Timing {
            ping_interval: Duration::from_secs(1),
            // Generous: the machine may be loaded (spike WP-15 saw ms-long stalls, WP-31 seconds
            // for a cold GUI open), and a missed Pong kills the plugin.
            pong_timeout: Duration::from_secs(5),
            // A cold Surge XT loads in up to 7 s (WP-31); sample libraries take longer.
            long_call_timeout: Duration::from_secs(60),
            snapshot_debounce: Duration::from_secs(1),
            warm_up: crate::WARM_UP,
        }
    }
}

/// How far a bridge got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for `Hello`.
    Hello,
    /// `Load`, `SetState`, `Activate` sent; waiting for `Activated`.
    Loading,
    /// Active.
    Running,
    /// Crashed or killed; the process may still need reaping.
    Failed,
    /// `Shutdown` sent (device released); killed if it does not exit in time.
    Stopping,
}

/// Why a state was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatePurpose {
    /// Reported as `PluginManagerEvent::StateSnapshot`.
    Event,
    /// Collected by `snapshot_states` (no event).
    Collect,
}

/// A state blob that arrived for a `Collect` request.
pub(crate) type Collected = (u32, Arc<[u8]>);

/// Starts `exe args… --instance` with the bridge end of a fresh control channel and watches it:
/// on Unix the end is an inherited fd named by `--control-fd`; on Windows it is the child's stdin
/// (komponist-ipc `socket_pair`), so no handle goes on the command line and std's spawn lock keeps
/// it out of every other child.
fn spawn_process(
    exe: &Path,
    args: &[OsString],
) -> Result<(AppChannel, ProcessWatcher), PluginError> {
    #[cfg(unix)]
    let (channel, bridge_end) = socket_pair().map_err(|e| PluginError::Ipc(e.to_string()))?;
    #[cfg(unix)]
    let mut command = {
        let raw = bridge_end.as_raw_fd();
        let mut command = Command::new(exe);
        command
            .args(args)
            .arg("--instance")
            .arg("--control-fd")
            .arg(raw.to_string())
            .stdin(Stdio::null());
        // SAFETY: runs between fork and exec and makes one async-signal-safe syscall on an fd
        // that stays open in the parent until `spawn` returned.
        unsafe {
            command.pre_exec(move || {
                // Let the bridge end of the socket survive exec.
                let fd = BorrowedFd::borrow_raw(raw);
                rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::empty())?;
                Ok(())
            });
        }
        command
    };
    #[cfg(windows)]
    let (channel, mut command) = {
        let (channel, bridge_end) = socket_pair().map_err(|e| PluginError::Ipc(e.to_string()))?;
        let mut command = Command::new(exe);
        command
            .args(args)
            .arg("--instance")
            .stdin(Stdio::from(bridge_end));
        (channel, command)
    };
    let mut child = command
        .spawn()
        .map_err(|e| PluginError::Spawn(format!("{}: {e}", exe.display())))?;
    let watcher = match ProcessWatcher::new(child.id()) {
        Ok(watcher) => watcher,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(PluginError::Spawn(format!("process watcher: {e}")));
        }
    };
    // The watcher reaps the child. Our copy of the bridge end must go (on Windows the `Command`
    // holds it), or a dead bridge would never close the channel.
    drop(child);
    drop(command);
    #[cfg(unix)]
    drop(bridge_end);
    Ok((channel, watcher))
}

/// One bridge process and everything the manager knows about its plugin.
#[derive(Debug)]
pub(crate) struct Bridge {
    device: DeviceId,
    channel: Arc<AppChannel>,
    writer: Option<mpsc::Sender<ControlMsg>>,
    watcher: ProcessWatcher,
    monitor: BridgeMonitor,
    shared: Arc<Shared>,
    phase: Phase,
    status: DeviceStatus,
    exited: bool,
    timing: Timing,
    /// When the current long phase (loading, `OpenGui`) started.
    long_call_since: Option<Instant>,
    /// `OpenGui` requests without answer.
    gui_requests: usize,
    next_seq: u32,
    /// The watchdog `Ping` waiting for its `Pong`, and since when it counts.
    ping_waiting: Option<(u32, Instant)>,
    last_ping: Instant,
    /// A `Ping` sent after a re-activation: its `Pong` means the bridge runs with the new setup.
    sync_ping: Option<u32>,
    /// Active, but `Loading` until then (warm-up after `Activated` or a re-activation).
    ready_at: Option<Instant>,
    stop_deadline: Option<Instant>,
    /// `GetState` requests without answer, in send order.
    state_requests: VecDeque<(u32, StatePurpose)>,
    snapshot_due: Option<Instant>,
    /// The latest state: the last snapshot, or the state the bridge was started with.
    pub last_state: Option<Arc<[u8]>>,
    pub params: Vec<ParamInfo>,
    pub values: BTreeMap<u32, ParamValue>,
    pub note_names: Vec<NoteName>,
    pub has_gui: bool,
    /// The plugin has a sidechain input (#6): from the catalog until `Loaded` says so itself.
    pub sidechain: bool,
    pub latency: u32,
    gui_open: bool,
    gui_closing: bool,
    /// Why the stored state could not be loaded (see the module docs).
    pub state_warning: Option<String>,
    /// The stored state failed to load and the user changed nothing yet: no snapshots, so the
    /// document keeps the blob.
    keep_stored_state: bool,
}

/// A started bridge and the processor for the engine.
pub(crate) struct Started {
    pub bridge: Bridge,
    pub processor: ExternalProcessor,
}

/// What `spawn` needs.
pub(crate) struct SpawnSetup<'a> {
    /// The launcher: executable and its leading arguments (`PluginManagerConfig`).
    pub exe: &'a Path,
    pub args: &'a [OsString],
    pub device: DeviceId,
    pub role: DeviceRole,
    pub format: PluginFormat,
    pub bundle: &'a Path,
    pub plugin_id: &'a str,
    pub state: Option<Arc<[u8]>>,
    pub setup: ProcessorSetup,
    pub timing: Timing,
    /// The catalog says the plugin has a sidechain input.
    pub sidechain: bool,
}

impl Bridge {
    /// Starts a bridge and queues its start-up messages; returns at once.
    pub(crate) fn spawn(s: SpawnSetup<'_>) -> Result<Started, PluginError> {
        if s.bundle.to_str().is_none() {
            return Err(PluginError::Spawn(format!(
                "bundle path {} is not valid UTF-8",
                s.bundle.to_string_lossy()
            )));
        }
        let (channel, watcher) = spawn_process(s.exe, s.args)?;

        let kill = |watcher: &ProcessWatcher| {
            let _ = watcher.kill();
        };
        let region = match ShmRegion::create() {
            Ok(region) => Arc::new(region),
            Err(e) => {
                kill(&watcher);
                return Err(PluginError::Ipc(e.to_string()));
            }
        };
        // The socket is empty, so this frame never blocks; the bridge takes it after `Hello`.
        #[cfg(unix)]
        let sent = channel.send_fd(region.fd());
        // Windows: the section and its events, duplicated into the bridge through its process
        // handle (the watcher's).
        #[cfg(windows)]
        let sent = channel.send_region(&region, watcher.as_handle());
        if let Err(e) = sent {
            kill(&watcher);
            return Err(PluginError::Ipc(e.to_string()));
        }
        let channel = Arc::new(channel);
        let writer = start_writer(channel.clone());
        let _ = writer.send(ControlMsg::Load {
            format: s.format,
            bundle: s.bundle.to_path_buf(),
            plugin_id: s.plugin_id.to_owned(),
        });
        if let Some(state) = &s.state {
            let _ = writer.send(ControlMsg::SetState {
                state: state.to_vec(),
            });
        }
        let _ = writer.send(activate(s.setup));

        let shared = Arc::new(Shared::new());
        let processor = ExternalProcessor::new(
            BridgeClient::new(region.clone()),
            shared.clone(),
            s.role,
            s.setup,
        );
        let now = Instant::now();
        let bridge = Bridge {
            device: s.device,
            channel,
            writer: Some(writer),
            watcher,
            monitor: BridgeMonitor::new(region),
            shared,
            phase: Phase::Hello,
            status: DeviceStatus::Loading,
            exited: false,
            timing: s.timing,
            long_call_since: Some(now),
            gui_requests: 0,
            next_seq: 0,
            ping_waiting: None,
            last_ping: now,
            sync_ping: None,
            ready_at: None,
            stop_deadline: None,
            state_requests: VecDeque::new(),
            snapshot_due: None,
            last_state: s.state,
            params: Vec::new(),
            values: BTreeMap::new(),
            note_names: Vec::new(),
            has_gui: false,
            sidechain: s.sidechain,
            latency: 0,
            gui_open: false,
            gui_closing: false,
            state_warning: None,
            keep_stored_state: false,
        };
        Ok(Started { bridge, processor })
    }

    /// Blocks of this bridge that missed their deadline or found it busy.
    pub(crate) fn late_blocks(&self) -> u64 {
        self.shared.late_blocks.load(Ordering::Relaxed)
    }

    /// Process id of the bridge.
    pub(crate) fn pid(&self) -> u32 {
        self.watcher.pid()
    }

    /// The device status. A re-activation the processor asked for (`Processor::activate` with a
    /// new rate, on another thread) counts as `Loading` at once, before the next poll takes it.
    pub(crate) fn status(&self) -> DeviceStatus {
        if self.phase == Phase::Running && self.shared.reactivating.load(Ordering::Acquire) {
            DeviceStatus::Loading
        } else {
            self.status.clone()
        }
    }

    /// Whether the bridge runs (loaded and active).
    pub(crate) fn is_running(&self) -> bool {
        self.phase == Phase::Running
    }

    /// Whether the process is gone and nothing is left to do.
    pub(crate) fn is_finished(&self) -> bool {
        self.exited && matches!(self.phase, Phase::Failed | Phase::Stopping)
    }

    /// The control socket, for `poll()`.
    #[cfg(unix)]
    pub(crate) fn socket(&self) -> BorrowedFd<'_> {
        self.channel.as_fd()
    }

    /// Queues a message for the bridge (dropped if the bridge is gone).
    pub(crate) fn send(&self, msg: ControlMsg) {
        if !matches!(self.phase, Phase::Failed | Phase::Stopping)
            && let Some(writer) = &self.writer
        {
            let _ = writer.send(msg);
        }
    }

    /// Switches realtime/offline processing. The bridge deactivates and re-activates the plugin
    /// for it, so a running device is `Loading` (fallback audio) until the bridge confirms, like a
    /// re-activation for another rate.
    pub(crate) fn set_render_mode(
        &mut self,
        mode: RenderMode,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        self.send(ControlMsg::SetRenderMode(mode));
        if self.phase == Phase::Running {
            self.shared.reactivating.store(true, Ordering::Release);
            let seq = self.seq();
            self.sync_ping = Some(seq);
            self.send(ControlMsg::Ping { seq });
            self.ready_at = None;
            self.set_status(DeviceStatus::Loading, events);
        }
    }

    fn seq(&mut self) -> u32 {
        self.next_seq = self.next_seq.wrapping_add(1);
        self.next_seq
    }

    /// Sends `GetState`; the answer is matched to `purpose`. Returns the request number.
    pub(crate) fn request_state(&mut self, purpose: StatePurpose) -> Option<u32> {
        if self.phase != Phase::Running || self.keep_stored_state {
            return None;
        }
        let request = self.seq();
        self.state_requests.push_back((request, purpose));
        self.send(ControlMsg::GetState { request });
        Some(request)
    }

    /// Whether this request still awaits a state reply or a state error.
    pub(crate) fn state_request_pending(&self, request: u32) -> bool {
        self.state_requests.iter().any(|&(seq, _)| seq == request)
    }

    /// Schedules a debounced `StateSnapshot`.
    pub(crate) fn snapshot_soon(&mut self) {
        if self.snapshot_due.is_none() {
            self.snapshot_due = Some(Instant::now() + self.timing.snapshot_debounce);
        }
    }

    /// The user changed something on the device: from now on its snapshots replace a stored
    /// state that failed to load.
    fn user_changed(&mut self) {
        self.keep_stored_state = false;
        self.state_warning = None;
    }

    /// Sends `SetParam` and keeps the value (its text follows with the bridge's echo).
    pub(crate) fn set_param(&mut self, id: u32, value: f64) {
        if self.phase != Phase::Running {
            return;
        }
        self.user_changed();
        self.send(ControlMsg::SetParam { id, value });
        let entry = self.values.entry(id).or_insert_with(|| ParamValue {
            id,
            value,
            text: String::new(),
        });
        entry.value = value;
        self.snapshot_soon();
    }

    /// Sends `SetState` to the running plugin (an undo or redo changed the device's state); the
    /// plugin's parameter values follow with the bridge's echo. `false` while the bridge is not
    /// running (it then starts with the state it was given).
    pub(crate) fn set_state(&mut self, state: &Arc<[u8]>) -> bool {
        if self.phase != Phase::Running {
            return false;
        }
        self.user_changed();
        self.last_state = Some(Arc::clone(state));
        self.send(ControlMsg::SetState {
            state: state.to_vec(),
        });
        true
    }

    /// Sends `OpenGui`; the watchdog waits while it is outstanding.
    pub(crate) fn open_gui(&mut self, target: GuiTarget) -> Result<(), PluginError> {
        if !self.has_gui {
            return Err(PluginError::NoGui);
        }
        if self.phase != Phase::Running {
            return Err(PluginError::Ipc("the plugin is not running".into()));
        }
        self.gui_requests += 1;
        self.long_call_since.get_or_insert_with(Instant::now);
        self.send(ControlMsg::OpenGui(target));
        Ok(())
    }

    /// Sends `CloseGui`; the `GuiClosed` answer triggers a state snapshot.
    pub(crate) fn close_gui(&mut self) {
        self.gui_closing = true;
        self.send(ControlMsg::CloseGui);
    }

    /// Releases the device: `Shutdown`, killed after `grace` if still alive.
    pub(crate) fn stop(&mut self, grace: Duration) {
        self.shared.run.store(RUN_DEAD, Ordering::Release);
        if matches!(self.phase, Phase::Failed | Phase::Stopping) {
            return;
        }
        if let Some(writer) = &self.writer {
            let _ = writer.send(ControlMsg::Shutdown);
        }
        self.phase = Phase::Stopping;
        self.stop_deadline = Some(Instant::now() + grace);
        // The writer thread ends after `Shutdown` (or when the socket closes).
        self.writer = None;
    }

    /// SIGKILL (no-op once the process is gone). Final: later messages of the bridge are ignored.
    pub(crate) fn kill(&mut self) {
        self.shared.run.store(RUN_DEAD, Ordering::Release);
        let _ = self.watcher.kill();
        self.writer = None;
        if self.phase != Phase::Stopping {
            self.phase = Phase::Failed;
        }
    }

    /// Blocks up to `timeout` for the process to exit (shutdown).
    pub(crate) fn wait_exit(&mut self, timeout: Duration) -> bool {
        if !self.exited {
            self.exited = matches!(self.watcher.wait_exit(Some(timeout)), Ok(Some(_)));
        }
        self.exited
    }

    /// The device failed: status `Crashed`, processor fallback, process killed.
    fn fail(&mut self, reason: FailReason, events: &mut Vec<PluginManagerEvent>) {
        if matches!(self.phase, Phase::Failed | Phase::Stopping) {
            return;
        }
        self.kill();
        self.gui_open = false;
        self.gui_requests = 0;
        self.state_requests.clear();
        self.snapshot_due = None;
        self.set_status(DeviceStatus::Crashed(reason), events);
    }

    fn set_status(&mut self, status: DeviceStatus, events: &mut Vec<PluginManagerEvent>) {
        if self.status != status {
            self.status = status.clone();
            events.push(PluginManagerEvent::StatusChanged {
                device: self.device,
                status,
            });
        }
    }

    /// Takes a re-activation the processor asked for (`Processor::activate` with a new rate). The
    /// device is `Loading` until the bridge confirms it and the warm-up passed.
    fn take_reactivation(&mut self, events: &mut Vec<PluginManagerEvent>) {
        let request = match self.shared.reactivate.try_lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        let Some(setup) = request else {
            return;
        };
        if matches!(self.phase, Phase::Failed | Phase::Stopping) {
            return;
        }
        // In order after whatever is queued: the bridge ends up active with the new setup, and
        // the `Pong` after it says so.
        self.send(ControlMsg::Deactivate);
        self.send(activate(setup));
        let seq = self.seq();
        self.sync_ping = Some(seq);
        self.send(ControlMsg::Ping { seq });
        self.ready_at = None;
        self.set_status(DeviceStatus::Loading, events);
    }

    /// Ends the warm-up: `Ok` once it passed (and no re-activation runs).
    fn check_ready(&mut self, now: Instant, events: &mut Vec<PluginManagerEvent>) {
        if let Some(at) = self.ready_at
            && now >= at
            && self.phase == Phase::Running
            && !self.shared.reactivating.load(Ordering::Acquire)
        {
            self.ready_at = None;
            self.set_status(DeviceStatus::Ok, events);
        }
    }

    /// Non-blocking: reads messages and plugin output events, checks the process and the
    /// watchdog. `collected` receives answers to `Collect` state requests.
    pub(crate) fn poll(
        &mut self,
        events: &mut Vec<PluginManagerEvent>,
        collected: &mut Vec<Collected>,
    ) {
        self.read_messages(events, collected);
        self.read_output_events(events);
        self.check_process(events);
        if self.exited {
            return;
        }
        self.take_reactivation(events);
        let now = Instant::now();
        self.check_ready(now, events);
        if let Some(deadline) = self.stop_deadline
            && now >= deadline
        {
            let _ = self.watcher.kill();
            self.stop_deadline = None;
        }
        if self.phase == Phase::Running
            && let Some(due) = self.snapshot_due
            && now >= due
        {
            self.snapshot_due = None;
            self.request_state(StatePurpose::Event);
        }
        self.watchdog(now, events);
    }

    /// Reads every queued control message (bounded per call).
    pub(crate) fn read_messages(
        &mut self,
        events: &mut Vec<PluginManagerEvent>,
        collected: &mut Vec<Collected>,
    ) {
        for _ in 0..256 {
            match self.channel.try_recv() {
                Ok(Some(msg)) => self.handle(msg, events, collected),
                Ok(None) => return,
                // The process is about to be reported as exited (or was killed).
                Err(ChannelError::Closed) => return,
                Err(e) => {
                    events.push(PluginManagerEvent::Log {
                        device: Some(self.device),
                        level: LogLevel::Error,
                        message: format!("control channel: {e}"),
                    });
                    if !matches!(e, ChannelError::Codec(_)) {
                        return;
                    }
                }
            }
        }
    }

    fn handle(
        &mut self,
        msg: BridgeMsg,
        events: &mut Vec<PluginManagerEvent>,
        collected: &mut Vec<Collected>,
    ) {
        if matches!(self.phase, Phase::Failed | Phase::Stopping) {
            // Killed or shut down: nothing it says matters any more.
            return;
        }
        let device = self.device;
        match msg {
            BridgeMsg::Hello { protocol, .. } => {
                if protocol != PROTOCOL_VERSION {
                    let message =
                        format!("bridge protocol {protocol}, expected {PROTOCOL_VERSION}");
                    return self.fail(FailReason::LoadFailed(message), events);
                }
                if self.phase == Phase::Hello {
                    self.phase = Phase::Loading;
                }
            }
            BridgeMsg::Loaded(info) => {
                let info = *info;
                self.params = info.params;
                self.values = info.values.into_iter().map(|v| (v.id, v)).collect();
                self.note_names = info.note_names;
                self.has_gui = info.has_gui;
                self.sidechain = info.descriptor.sidechain;
                events.push(PluginManagerEvent::ParamsChanged { device });
                events.push(PluginManagerEvent::ParamValuesChanged { device });
                if !self.note_names.is_empty() {
                    events.push(PluginManagerEvent::NoteNamesChanged { device });
                }
                self.update_latency(info.latency, events);
            }
            BridgeMsg::Activated { latency } => {
                self.update_latency(latency, events);
                if matches!(self.phase, Phase::Hello | Phase::Loading) {
                    self.phase = Phase::Running;
                    self.long_call_since = (self.gui_requests > 0).then(Instant::now);
                    self.last_ping = Instant::now();
                    self.shared.run.store(RUN_ACTIVE, Ordering::Release);
                    // Audio flows from now on; the status follows after the warm-up.
                    self.ready_at = Some(Instant::now() + self.timing.warm_up);
                    self.check_ready(Instant::now(), events);
                }
            }
            BridgeMsg::State { request, state } => {
                let state: Arc<[u8]> = state.into();
                let purpose = match self.state_requests.iter().position(|(r, _)| *r == request) {
                    // Answers come in order; earlier requests without answer had errors.
                    Some(i) => self.state_requests.drain(..=i).next_back().map(|(_, p)| p),
                    None => None,
                };
                self.last_state = Some(state.clone());
                match purpose {
                    Some(StatePurpose::Collect) => collected.push((request, state)),
                    Some(StatePurpose::Event) | None => {
                        events.push(PluginManagerEvent::StateSnapshot { device, state });
                    }
                }
            }
            BridgeMsg::Params(params) => {
                self.params = params;
                events.push(PluginManagerEvent::ParamsChanged { device });
            }
            BridgeMsg::ParamValues(values) => {
                for value in values {
                    self.values.insert(value.id, value);
                }
                events.push(PluginManagerEvent::ParamValuesChanged { device });
            }
            BridgeMsg::GuiOpened { size, resizable } => {
                self.gui_answered();
                self.gui_open = true;
                events.push(PluginManagerEvent::GuiOpened {
                    device,
                    size,
                    resizable,
                });
            }
            BridgeMsg::GuiSizeRequest(size) => {
                events.push(PluginManagerEvent::GuiSizeRequest { device, size });
            }
            BridgeMsg::GuiClosed { rect } => {
                let snapshot = self.gui_open || self.gui_closing;
                self.gui_open = false;
                self.gui_closing = false;
                events.push(PluginManagerEvent::GuiClosed { device, rect });
                if snapshot {
                    self.request_state(StatePurpose::Event);
                }
            }
            BridgeMsg::HeaderAction(event) => {
                events.push(PluginManagerEvent::FrameHeaderAction { device, event });
            }
            BridgeMsg::Shortcut { chord, .. } => {
                events.push(PluginManagerEvent::FrameShortcut { device, chord });
            }
            BridgeMsg::HeaderKey(key) => events.push(PluginManagerEvent::FrameKey { device, key }),
            BridgeMsg::FrameMoved(rect) => {
                events.push(PluginManagerEvent::FrameMoved { device, rect });
            }
            BridgeMsg::CloseRequested => {
                events.push(PluginManagerEvent::FrameCloseRequested { device });
            }
            BridgeMsg::FrameWindow(number) => {
                events.push(PluginManagerEvent::FrameWindow { device, number });
            }
            BridgeMsg::LatencyChanged(latency) => self.update_latency(latency, events),
            BridgeMsg::RestartRequested => {}
            BridgeMsg::StateDirty => {
                // While loading, "dirty" comes from the plugin itself, not from the user.
                if self.phase == Phase::Running {
                    self.user_changed();
                }
                self.snapshot_soon();
            }
            BridgeMsg::NoteNames(names) => {
                self.note_names = names;
                events.push(PluginManagerEvent::NoteNamesChanged { device });
            }
            BridgeMsg::Log { level, message } => {
                events.push(PluginManagerEvent::Log {
                    device: Some(device),
                    level,
                    message,
                });
            }
            BridgeMsg::Error {
                context,
                message,
                fatal,
            } => self.handle_error(context, message, fatal, events),
            BridgeMsg::Pong { seq } => {
                if self.ping_waiting.is_some_and(|(s, _)| s == seq) {
                    self.ping_waiting = None;
                }
                if self.sync_ping == Some(seq) {
                    self.sync_ping = None;
                    if self.phase == Phase::Running {
                        self.shared.reactivating.store(false, Ordering::Release);
                        self.ready_at = Some(Instant::now() + self.timing.warm_up);
                        self.check_ready(Instant::now(), events);
                    }
                }
            }
            BridgeMsg::RtPriority { .. } => {
                // The bridge also logs the outcome as a `Log` line ("RT priority: …"), which the
                // app shows as a status-bar warning when it is a warning.
            }
        }
    }

    fn handle_error(
        &mut self,
        context: ErrorContext,
        message: String,
        fatal: bool,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        let loading = matches!(self.phase, Phase::Hello | Phase::Loading);
        let fails = fatal
            || context == ErrorContext::Activate
            || (loading && context == ErrorContext::Load);
        if fails {
            return self.fail(FailReason::LoadFailed(message), events);
        }
        if loading && context == ErrorContext::State {
            // The stored state (`SetState`, the only state request while loading) was refused.
            self.keep_stored_state = true;
            events.push(PluginManagerEvent::Log {
                device: Some(self.device),
                level: LogLevel::Warning,
                message: format!("{STATE_LOG_PREFIX} {message}"),
            });
            self.state_warning = Some(message);
            return;
        }
        match context {
            ErrorContext::State => {
                // A `GetState` failed (answers come in order).
                self.state_requests.pop_front();
            }
            ErrorContext::Gui if self.gui_requests > 0 => {
                self.gui_answered();
                events.push(PluginManagerEvent::GuiClosed {
                    device: self.device,
                    rect: None,
                });
            }
            _ => {}
        }
        events.push(PluginManagerEvent::Log {
            device: Some(self.device),
            level: LogLevel::Error,
            message,
        });
    }

    fn gui_answered(&mut self) {
        self.gui_requests = self.gui_requests.saturating_sub(1);
        if self.gui_requests == 0 && self.phase == Phase::Running {
            self.long_call_since = None;
            // The watchdog starts over after a long call.
            if let Some((_, since)) = &mut self.ping_waiting {
                *since = Instant::now();
            }
        }
    }

    fn update_latency(&mut self, latency: u32, events: &mut Vec<PluginManagerEvent>) {
        if latency != self.latency {
            self.latency = latency;
            events.push(PluginManagerEvent::LatencyChanged {
                device: self.device,
                latency,
            });
        }
    }

    /// Parameter changes the plugin made during processing (its GUI), from the shared ring.
    fn read_output_events(&mut self, events: &mut Vec<PluginManagerEvent>) {
        let mut changed = false;
        while let Some(event) = self.monitor.pop_output_event() {
            if let Some(WireEventKind::ParamValue { id, value }) = event.decode() {
                let entry = self.values.entry(id).or_insert_with(|| ParamValue {
                    id,
                    value,
                    text: String::new(),
                });
                if entry.value != value {
                    entry.value = value;
                    // No text for values from the audio thread; the generic view formats them.
                    entry.text.clear();
                }
                changed = true;
            }
        }
        if changed {
            events.push(PluginManagerEvent::ParamValuesChanged {
                device: self.device,
            });
            // Values from the audio thread are edits in the plugin's GUI.
            self.user_changed();
            self.snapshot_soon();
        }
    }

    fn check_process(&mut self, events: &mut Vec<PluginManagerEvent>) {
        if self.exited {
            return;
        }
        let exit = match self.watcher.try_exit() {
            Ok(Some(exit)) => exit,
            Ok(None) => return,
            Err(e) => {
                // Not expected for our own child; treat it as gone.
                self.exited = true;
                return self.fail(FailReason::LoadFailed(format!("pidfd: {e}")), events);
            }
        };
        self.exited = true;
        self.shared.run.store(RUN_DEAD, Ordering::Release);
        // Messages the bridge sent before it died (a fatal error, its last logs).
        let mut late = Vec::new();
        self.read_messages(events, &mut late);
        self.fail(
            FailReason::Exited {
                code: exit.code,
                signal: exit.signal,
            },
            events,
        );
    }

    fn watchdog(&mut self, now: Instant, events: &mut Vec<PluginManagerEvent>) {
        if matches!(self.phase, Phase::Failed | Phase::Stopping) {
            return;
        }
        if let Some(since) = self.long_call_since {
            if now.duration_since(since) > self.timing.long_call_timeout {
                self.fail(FailReason::Unresponsive, events);
            }
            return;
        }
        match self.ping_waiting {
            Some((_, since)) if now.duration_since(since) > self.timing.pong_timeout => {
                self.fail(FailReason::Unresponsive, events);
            }
            Some(_) => {}
            None if now.duration_since(self.last_ping) >= self.timing.ping_interval => {
                let seq = self.seq();
                self.ping_waiting = Some((seq, now));
                self.last_ping = now;
                self.send(ControlMsg::Ping { seq });
            }
            None => {}
        }
    }
}

impl Drop for Bridge {
    /// Never leaves a bridge behind (no `PR_SET_PDEATHSIG`): SIGKILL and a short wait to reap it.
    fn drop(&mut self) {
        self.shared.run.store(RUN_DEAD, Ordering::Release);
        self.writer = None;
        if !self.exited {
            let _ = self.watcher.kill();
            let _ = self.watcher.wait_exit(Some(Duration::from_millis(500)));
        }
    }
}

/// `Activate` for `setup`.
fn activate(setup: ProcessorSetup) -> ControlMsg {
    ControlMsg::Activate {
        sample_rate: f64::from(setup.sample_rate),
        max_block: setup.max_block as u32,
    }
}

/// The thread that sends everything to one bridge, in order. Ends when the sender is dropped or
/// the socket breaks (the bridge died).
fn start_writer(channel: Arc<AppChannel>) -> mpsc::Sender<ControlMsg> {
    let (tx, rx) = mpsc::channel::<ControlMsg>();
    std::thread::Builder::new()
        .name("kp-bridge-tx".into())
        .spawn(move || {
            while let Ok(msg) = rx.recv() {
                let shutdown = msg == ControlMsg::Shutdown;
                if channel.send(&msg).is_err() || shutdown {
                    return;
                }
            }
        })
        .expect("spawn a bridge writer thread");
    tx
}
