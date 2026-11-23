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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsHandle;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

use komponist_builtin::BuiltinKind;
use komponist_ipc::control::{PROTOCOL_VERSION, StateRestoreError, socket_pair};
use komponist_ipc::{
    AppChannel, BridgeClient, BridgeMsg, ChannelError, ControlMsg, ErrorContext, GuiTarget,
    LogLevel, NoteName, ParamInfo, ParamValue, PluginFormat, ProcessExit, ProcessWatcher,
    RenderMode, ShmRegion,
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

#[derive(Debug)]
struct StateRequest {
    request: u32,
    purpose: StatePurpose,
    checked: bool,
    superseded: bool,
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

#[derive(Debug)]
struct StateRestore {
    request: u32,
    /// Newer user edits already applied optimistically to the manager's cache.
    later_edits: BTreeSet<u32>,
}

impl StateRestore {
    fn new(request: u32) -> Self {
        Self {
            request,
            later_edits: BTreeSet::new(),
        }
    }
}

/// How long one [`Bridge::read_messages`] may wait for the rest of a message that has begun to
/// arrive (#216). A large state streams through a socket buffer of about 256 KiB: read only what
/// is queued at each poll (every 100 ms when the app is idle), a state of tens of MiB would take
/// seconds and hold the bridge's main thread in `send` past its `Pong` timeout. Within this
/// budget it moves at socket speed; a bridge stalled mid-message costs one budget per poll until
/// the watchdog kills it.
const READ_BUDGET: Duration = Duration::from_millis(10);

/// One bridge process and everything the manager knows about its plugin.
#[derive(Debug)]
pub(crate) struct Bridge {
    device: DeviceId,
    channel: Arc<AppChannel>,
    writer: Option<mpsc::Sender<ControlMsg>>,
    watcher: ProcessWatcher,
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
    /// The last [`Bridge::read_messages`] stopped before it had read everything the bridge sent
    /// (a message half in, or its budget or message cap ran out): a `Pong` may be queued behind.
    behind: bool,
    /// When bytes last arrived while the reader was behind (or caught up with that backlog):
    /// the bridge's liveness while a `Pong` waits behind a message spanning several polls.
    stream_progress: Option<Instant>,
    last_ping: Instant,
    /// A `Ping` sent after a re-activation: its `Pong` means the bridge runs with the new setup.
    sync_ping: Option<u32>,
    /// Active, but `Loading` until then (warm-up after `Activated` or a re-activation).
    ready_at: Option<Instant>,
    stop_deadline: Option<Instant>,
    /// `GetState` requests without answer, in send order.
    state_requests: VecDeque<StateRequest>,
    state_restores: VecDeque<StateRestore>,
    // The most recent matched reply, including partial restore failures reported as UI updates.
    restore_reply: Option<(u32, Result<(), String>)>,
    snapshot_due: Option<Instant>,
    /// The latest state: the last snapshot, or the state the bridge was started with.
    pub last_state: Option<Arc<[u8]>>,
    pub params: Vec<ParamInfo>,
    pub sampler_ui: Option<komponist_ipc::sampler_ui::Snapshot>,
    sampler_ui_request: Option<u32>,
    sampler_ui_at: Instant,
    sampler_export_tempo: Option<f64>,
    pub native_layout: Option<Arc<komponist_ipc::native_layout::NativeLayout>>,
    pub values: BTreeMap<u32, ParamValue>,
    pub base_values: BTreeMap<u32, f64>,
    verified_bundled_kind: Option<BuiltinKind>,
    automated_params: BTreeMap<u32, Option<f64>>,
    temporary_params: BTreeSet<u32>,
    pending_params: BTreeSet<u32>,
    /// Macro outputs expected at the processor. The native editor may show a preview meanwhile.
    macro_params: BTreeMap<u32, f64>,
    /// Last macro values that became the ordinary parameter base on unmapping.
    released_macro_params: BTreeSet<u32>,
    macro_changed: bool,
    /// An explicit plugin state change is independent of the verified macro outputs.
    plugin_state_dirty: bool,
    macro_snapshots: Vec<Weak<[u8]>>,
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
    pub verified_bundled_kind: Option<BuiltinKind>,
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
        let mut state_restores = VecDeque::new();
        if let Some(state) = &s.state {
            let request = 0;
            state_restores.push_back(StateRestore::new(request));
            let _ = writer.send(ControlMsg::SetState {
                request,
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
            shared,
            phase: Phase::Hello,
            status: DeviceStatus::Loading,
            exited: false,
            timing: s.timing,
            long_call_since: Some(now),
            gui_requests: 0,
            next_seq: 0,
            ping_waiting: None,
            behind: false,
            stream_progress: None,
            last_ping: now,
            sync_ping: None,
            ready_at: None,
            stop_deadline: None,
            state_requests: VecDeque::new(),
            state_restores,
            restore_reply: None,
            snapshot_due: None,
            last_state: s.state,
            params: Vec::new(),
            sampler_ui: None,
            sampler_ui_request: None,
            sampler_ui_at: Instant::now(),
            sampler_export_tempo: None,
            native_layout: None,
            values: BTreeMap::new(),
            base_values: BTreeMap::new(),
            verified_bundled_kind: s.verified_bundled_kind,
            automated_params: BTreeMap::new(),
            temporary_params: BTreeSet::new(),
            pending_params: BTreeSet::new(),
            macro_params: BTreeMap::new(),
            released_macro_params: BTreeSet::new(),
            macro_changed: false,
            plugin_state_dirty: false,
            macro_snapshots: Vec::new(),
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

    pub(crate) fn set_sampler_export_tempo(&mut self, tempo: Option<f64>) {
        self.sampler_export_tempo = tempo;
        if tempo.is_some() {
            self.sampler_ui = None;
        }
        self.sampler_ui_request = None;
        self.sampler_ui_at = Instant::now();
    }

    pub(crate) fn poll_sampler_ui(&mut self) {
        let now = Instant::now();
        if self.running()
            && self.state_restores.is_empty()
            && self.sampler_ui_request.is_none()
            && now >= self.sampler_ui_at
        {
            let request = self.seq();
            self.sampler_ui_request = Some(request);
            self.sampler_ui_at = now + crate::bundled_sampler::UI_REFRESH;
            self.send(ControlMsg::GetSamplerUi(request, self.sampler_export_tempo));
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
        let checked = self.snapshot_needs_verification();
        let request = self.seq();
        let message = if checked {
            let expected = self.snapshot_expected()?;
            ControlMsg::GetStateChecked { request, expected }
        } else {
            ControlMsg::GetState { request }
        };
        self.state_requests.push_back(StateRequest {
            request,
            purpose,
            checked,
            superseded: false,
        });
        self.send(message);
        Some(request)
    }

    fn snapshot_needs_verification(&self) -> bool {
        !self.automated_params.is_empty()
            || !self.temporary_params.is_empty()
            || !self.pending_params.is_empty()
            || !self.macro_params.is_empty()
            || !self.released_macro_params.is_empty()
    }

    fn snapshot_expected(&self) -> Option<Vec<(u32, f64, bool)>> {
        if self.verified_bundled_kind.is_some() {
            // Automation and macro outputs are written into the saved state from the host's
            // persistent values. Independent user edits still need delivery verification.
            return self
                .pending_params
                .union(&self.released_macro_params)
                .filter(|id| {
                    self.pending_params.contains(id)
                        || (!self.macro_params.contains_key(id)
                            && !self.automated_params.contains_key(id)
                            && !self.temporary_params.contains(id))
                })
                .map(|&id| Some((id, *self.base_values.get(&id)?, true)))
                .collect();
        }
        let ids: BTreeSet<_> = self
            .temporary_params
            .iter()
            .chain(&self.pending_params)
            .chain(self.macro_params.keys())
            .chain(&self.released_macro_params)
            .copied()
            .collect();
        ids.into_iter()
            .map(|id| {
                let value = self
                    .macro_params
                    .get(&id)
                    .or_else(|| self.base_values.get(&id))?;
                Some((id, *value, !self.temporary_params.contains(&id)))
            })
            .collect()
    }

    /// Reinsert persistent values for controls whose live values may differ during automation.
    fn snapshot_with_param_bases(&self, state: Vec<u8>) -> Option<Vec<u8>> {
        let Some(kind) = self.verified_bundled_kind else {
            return Some(state);
        };
        let mut corrections = self
            .base_values
            .iter()
            .filter_map(|(&id, &base)| {
                let persistent = self
                    .automated_params
                    .get(&id)
                    .map(|value| value.unwrap_or(base));
                persistent
                    .or_else(|| self.macro_params.get(&id).copied())
                    .or_else(|| self.temporary_params.contains(&id).then_some(base))
                    .map(|value| (id, value))
            })
            .peekable();
        if corrections.peek().is_none() {
            return Some(state);
        }
        crate::bundled_state::restore_param_bases(
            kind,
            self.native_layout.as_deref()?,
            &state,
            corrections,
        )
    }

    /// A running device normally owes a fresh snapshot. An opaque plug-in state's temporary
    /// override may delay it. Failed restores preserve the original stored bytes.
    pub(crate) fn expects_snapshot_for_save(&self) -> bool {
        self.is_running()
            && !self.keep_stored_state
            && (self.verified_bundled_kind.is_some() || self.temporary_params.is_empty())
    }

    pub(crate) fn needs_snapshot_for_save(&self) -> bool {
        self.is_running()
            && (!self.pending_params.is_empty() || !self.released_macro_params.is_empty())
    }

    /// Whether this request still awaits a state reply or a state error.
    pub(crate) fn state_request_pending(&self, request: u32) -> bool {
        self.state_requests
            .iter()
            .any(|pending| pending.request == request)
    }

    fn take_state_request(&mut self, request: u32) -> Option<StateRequest> {
        let index = self
            .state_requests
            .iter()
            .position(|pending| pending.request == request)?;
        self.state_requests.drain(..=index).next_back()
    }

    fn invalidate_state_requests(&mut self) {
        for pending in &mut self.state_requests {
            pending.superseded = true;
        }
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
        self.send(ControlMsg::SetParam { id, value });
        self.edited_param(id, value);
    }

    /// Updates the live cache without retaining a temporary value as a document edit.
    pub(crate) fn preview_param(&mut self, id: u32, value: f64) {
        if !self.params.iter().any(|param| param.id == id) {
            return;
        }
        let temporary = if let Some(&expected) = self.macro_params.get(&id) {
            let temporary = (expected - value).abs() > 1e-6 * expected.abs().max(1.0);
            self.macro_changed |= temporary;
            temporary
        } else {
            self.base_values.get(&id) != Some(&value)
        };
        if temporary {
            self.temporary_params.insert(id);
            if self.verified_bundled_kind.is_none() {
                self.invalidate_state_requests();
            }
        }
        self.update_live_param(id, value);
    }

    pub(crate) fn set_automated_params(&mut self, params: BTreeMap<u32, Option<f64>>) {
        if self.verified_bundled_kind.is_some() && self.automated_params != params {
            // A program swap can remove a lane before audio restores the control. Keep
            // correcting any captured transient value until an explicit edit replaces it.
            self.temporary_params.extend(
                self.automated_params
                    .keys()
                    .filter(|id| !params.contains_key(id))
                    .copied(),
            );
            self.automated_params = params;
            self.invalidate_state_requests();
        }
    }

    pub(crate) fn set_macro_params(&mut self, values: BTreeMap<u32, f64>) {
        if self.macro_params == values {
            return;
        }
        let previous = std::mem::replace(&mut self.macro_params, values);
        if self.verified_bundled_kind.is_some() {
            if previous.keys().eq(self.macro_params.keys())
                && previous.iter().all(|(id, value)| {
                    self.macro_params.get(id) == Some(value)
                        || self.automated_params.get(id).is_some_and(Option::is_some)
                })
            {
                // Live macro motion cannot supersede a snapshot which uses the model's output.
                return;
            }
            for id in self
                .macro_params
                .keys()
                .filter(|id| !previous.contains_key(id))
            {
                // An explicit new mapping supersedes a manual write to the same target.
                self.pending_params.remove(id);
                self.released_macro_params.remove(id);
            }
        }
        for (id, value) in previous {
            if self.phase == Phase::Running
                && !self.macro_params.contains_key(&id)
                && self.base_values.insert(id, value) != Some(value)
            {
                for restore in &mut self.state_restores {
                    restore.later_edits.insert(id);
                }
                self.released_macro_params.insert(id);
                self.user_changed();
                self.snapshot_soon();
            }
        }
        self.macro_changed = true;
        self.invalidate_state_requests();
    }

    pub(crate) fn is_macro_snapshot(&self, state: &Arc<[u8]>) -> bool {
        let state = Arc::downgrade(state);
        self.macro_snapshots
            .iter()
            .any(|snapshot| snapshot.ptr_eq(&state))
    }

    fn update_live_param(&mut self, id: u32, value: f64) {
        let entry = self.values.entry(id).or_insert_with(|| ParamValue {
            id,
            value,
            text: String::new(),
        });
        if entry.value != value {
            entry.text.clear();
            entry.value = value;
        }
    }

    /// Sends `SetState` to the running plugin (an undo or redo changed the device's state); the
    /// plugin's parameter values follow with the bridge's echo. `false` while the bridge is not
    /// running (it then starts with the state it was given).
    pub(crate) fn set_state(&mut self, state: &Arc<[u8]>) -> bool {
        self.request_restore(state).is_some()
    }

    /// Queue a restore and return the sequence used by the bridge's reply.
    pub(crate) fn request_restore(&mut self, state: &Arc<[u8]>) -> Option<u32> {
        if self.phase != Phase::Running {
            return None;
        }
        self.user_changed();
        self.sampler_ui = None;
        self.sampler_ui_request = None;
        self.last_state = Some(Arc::clone(state));
        self.pending_params.clear();
        self.released_macro_params.clear();
        self.invalidate_state_requests();
        let request = self.seq();
        self.state_restores.push_back(StateRestore::new(request));
        self.send(ControlMsg::SetState {
            request,
            state: state.to_vec(),
        });
        Some(request)
    }

    pub(crate) fn restore_result(&self, request: u32) -> Option<Result<(), String>> {
        self.restore_reply
            .as_ref()
            .filter(|(sequence, _)| *sequence == request)
            .map(|(_, result)| result.clone())
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
        self.exited
            || self
                .wait_exit_status(timeout)
                .is_ok_and(|exit| exit.is_some())
    }

    pub(crate) fn wait_exit_status(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<ProcessExit>> {
        let exit = self.watcher.wait_exit(Some(timeout))?;
        self.exited |= exit.is_some();
        Ok(exit)
    }

    /// The device failed: status `Crashed`, processor fallback, process killed.
    fn fail(&mut self, reason: FailReason, events: &mut Vec<PluginManagerEvent>) {
        if matches!(self.phase, Phase::Failed | Phase::Stopping) {
            return;
        }
        self.kill();
        self.gui_open = false;
        self.native_layout = None;
        self.gui_requests = 0;
        self.state_requests.clear();
        self.state_restores.clear();
        self.snapshot_due = None;
        self.set_status(DeviceStatus::Crashed(reason), events);
    }

    fn update_native_layout(
        &mut self,
        bytes: Option<Vec<u8>>,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        self.native_layout = bytes.and_then(|bytes| {
            let layout =
                komponist_ipc::native_layout::NativeLayout::parse(&bytes).and_then(|layout| {
                    layout.validate_params(&self.params)?;
                    Ok(layout)
                });
            match layout {
                Ok(layout) => Some(Arc::new(layout)),
                Err(error) => {
                    events.push(PluginManagerEvent::Log {
                        device: Some(self.device),
                        level: LogLevel::Warning,
                        message: error.to_string(),
                    });
                    None
                }
            }
        });
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

    /// Non-blocking: reads control messages, checks the process and the
    /// watchdog. `collected` receives answers to `Collect` state requests.
    pub(crate) fn poll(
        &mut self,
        events: &mut Vec<PluginManagerEvent>,
        collected: &mut Vec<Collected>,
    ) {
        self.read_messages(events, collected);
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

    /// Reads every queued control message (bounded per call). Never waits for a message that
    /// has not begun to arrive; one that has is read on while its bytes keep coming, for at most
    /// [`READ_BUDGET`] per call, so a bridge that stalls in the middle of a message cannot hold
    /// up the UI thread or the watchdog after this (#216). While a message spans several calls,
    /// its arriving bytes count as the bridge's liveness (see `still_sending`).
    pub(crate) fn read_messages(
        &mut self,
        events: &mut Vec<PluginManagerEvent>,
        collected: &mut Vec<Collected>,
    ) {
        let was_behind = self.behind;
        self.behind = self.read_for_budget(events, collected);
        if self.behind || was_behind {
            self.stream_progress = self.channel.last_received();
        }
    }

    /// Reads messages for at most [`READ_BUDGET`]; whether it stopped before the stream was
    /// drained (see `behind`).
    fn read_for_budget(
        &mut self,
        events: &mut Vec<PluginManagerEvent>,
        collected: &mut Vec<Collected>,
    ) -> bool {
        let deadline = Instant::now() + READ_BUDGET;
        for _ in 0..256 {
            match self.channel.try_recv_until(deadline) {
                Ok(Some(msg)) => self.handle(msg, events, collected),
                // Without a half-read message and with budget left, the socket was empty.
                Ok(None) => return self.channel.is_receiving() || Instant::now() >= deadline,
                // The process is about to be reported as exited (or was killed).
                Err(ChannelError::Closed) => return false,
                Err(e) => {
                    events.push(PluginManagerEvent::Log {
                        device: Some(self.device),
                        level: LogLevel::Error,
                        message: format!("control channel: {e}"),
                    });
                    if !matches!(e, ChannelError::Codec(_)) {
                        return false;
                    }
                }
            }
        }
        // The message cap ended the loop.
        true
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
                self.update_native_layout(info.native_layout, events);
                self.base_values = info.values.iter().map(|v| (v.id, v.value)).collect();
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
            BridgeMsg::State {
                request,
                state,
                verified,
            } => {
                let pending = self.take_state_request(request);
                if pending.as_ref().is_some_and(|pending| pending.superseded)
                    || (self.snapshot_needs_verification()
                        && !pending.as_ref().is_some_and(|pending| pending.checked))
                {
                    self.snapshot_soon();
                    return;
                }
                if self.verified_bundled_kind.is_some()
                    && self
                        .pending_params
                        .iter()
                        .any(|id| !verified.iter().any(|(verified_id, _)| id == verified_id))
                {
                    self.snapshot_soon();
                    return;
                }
                let Some(state) = self.snapshot_with_param_bases(state) else {
                    self.snapshot_soon();
                    return;
                };
                let macro_snapshot = self.macro_changed
                    && !self.plugin_state_dirty
                    && self.pending_params.is_empty();
                if pending.as_ref().is_some_and(|pending| pending.checked) {
                    let mut changed = false;
                    for (id, value) in verified {
                        if (self.pending_params.contains(&id)
                            || self.released_macro_params.contains(&id))
                            && !self.macro_params.contains_key(&id)
                            && let Some(base) = self.base_values.get_mut(&id)
                            && (*base - value).abs() > 1e-6 * base.abs().max(1.0)
                        {
                            *base = value;
                            self.update_live_param(id, value);
                            changed = true;
                        }
                    }
                    if changed {
                        events.push(PluginManagerEvent::ParamsChanged { device });
                        events.push(PluginManagerEvent::ParamValuesChanged { device });
                    }
                    if self.verified_bundled_kind.is_none() {
                        self.temporary_params.clear();
                    }
                    self.pending_params.clear();
                    self.released_macro_params.clear();
                }
                let state: Arc<[u8]> = state.into();
                self.macro_changed = false;
                self.plugin_state_dirty = false;
                if macro_snapshot {
                    self.macro_snapshots
                        .retain(|snapshot| snapshot.strong_count() != 0);
                    self.macro_snapshots.push(Arc::downgrade(&state));
                }
                self.last_state = Some(state.clone());
                match pending.map(|request| request.purpose) {
                    Some(StatePurpose::Collect) => {
                        collected.push((request, state));
                    }
                    Some(StatePurpose::Event) | None => {
                        events.push(PluginManagerEvent::StateSnapshot { device, state });
                    }
                }
            }
            BridgeMsg::StateDeferred { request } => {
                if self.take_state_request(request).is_some() {
                    self.snapshot_soon();
                }
            }
            BridgeMsg::Params {
                params,
                native_layout,
            } => {
                let ids: BTreeSet<_> = params.iter().map(|param| param.id).collect();
                self.temporary_params.retain(|id| ids.contains(id));
                self.pending_params.retain(|id| ids.contains(id));
                self.macro_params.retain(|id, _| ids.contains(id));
                self.released_macro_params.retain(|id| ids.contains(id));
                self.base_values.retain(|id, _| ids.contains(id));
                self.values.retain(|id, _| ids.contains(id));
                self.params = params;
                self.update_native_layout(native_layout, events);
                events.push(PluginManagerEvent::ParamsChanged { device });
            }
            BridgeMsg::ParamValues(values) => {
                for value in values {
                    if !self.params.iter().any(|param| param.id == value.id) {
                        continue;
                    }
                    self.base_values.entry(value.id).or_insert(value.value);
                    self.values.insert(value.id, value);
                }
                events.push(PluginManagerEvent::ParamValuesChanged { device });
            }
            BridgeMsg::PluginParamEdits(edits) => {
                if self.verified_bundled_kind.is_some() && self.phase == Phase::Running {
                    for (id, value) in edits {
                        if value.is_finite() && !self.automated_params.contains_key(&id) {
                            self.edited_param(id, value);
                        }
                    }
                }
            }
            BridgeMsg::StateApplied { request, result } => {
                self.finish_restore(request, result, events);
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
                    self.plugin_state_dirty = true;
                    self.invalidate_state_requests();
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
            BridgeMsg::SamplerUi(request, bytes) => {
                if self.sampler_ui_request == Some(request) {
                    self.sampler_ui_request = None;
                    self.sampler_ui = bytes
                        .as_deref()
                        .and_then(komponist_ipc::sampler_ui::Snapshot::decode);
                }
            }
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

    fn finish_restore(
        &mut self,
        request: u32,
        result: Result<Vec<ParamValue>, StateRestoreError>,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        let Some(index) = self
            .state_restores
            .iter()
            .position(|restore| restore.request == request)
        else {
            return;
        };
        let restore = self.state_restores.remove(index).expect("matched restore");
        let device = self.device;
        let (values, error) = match result {
            Ok(values) => (values, None),
            Err(error) => (error.changed_values, Some(error.message)),
        };
        self.restore_reply = Some((request, error.clone().map_or(Ok(()), Err)));
        if error.is_none() || !values.is_empty() {
            for value in values {
                if !restore.later_edits.contains(&value.id) {
                    self.base_values.insert(value.id, value.value);
                    self.values.insert(value.id, value);
                }
            }
            // The compiler obtains the plugin's restored base by recompiling the program.
            events.push(PluginManagerEvent::ParamsChanged { device });
            events.push(PluginManagerEvent::ParamValuesChanged { device });
            events.push(PluginManagerEvent::StateApplied { device });
        }
        if let Some(message) = error {
            if self.phase != Phase::Running {
                self.keep_stored_state = true;
                events.push(PluginManagerEvent::Log {
                    device: Some(device),
                    level: LogLevel::Warning,
                    message: format!("{STATE_LOG_PREFIX} {message}"),
                });
                self.state_warning = Some(message);
            } else {
                events.push(PluginManagerEvent::Log {
                    device: Some(device),
                    level: LogLevel::Error,
                    message,
                });
            }
        }
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

    /// Updates the cached value and schedules a snapshot for a user's edit.
    pub(crate) fn edited_param(&mut self, id: u32, value: f64) {
        if self.phase != Phase::Running || !self.params.iter().any(|param| param.id == id) {
            return;
        }
        if self.macro_params.contains_key(&id) {
            self.preview_param(id, value);
            return;
        }
        for restore in &mut self.state_restores {
            restore.later_edits.insert(id);
        }
        // The engine reports a persistent edit only after ownership returns to the user.
        // Keep verification pending while that edit replaces the previous override.
        let replaced_override = self.temporary_params.remove(&id);
        if replaced_override || self.base_values.get(&id) != Some(&value) {
            self.pending_params.insert(id);
            self.invalidate_state_requests();
        }
        self.base_values.insert(id, value);
        self.update_live_param(id, value);
        self.user_changed();
        self.snapshot_soon();
    }

    pub(crate) fn running(&self) -> bool {
        self.phase == Phase::Running
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

    /// Whether a late `Pong` may only be stuck behind data the bridge is still sending (#216): a
    /// message that spans several polls (each reads for [`READ_BUDGET`]) holds the `Pong` back
    /// until it is in, so `pong_timeout` counts from its last bytes rather than from the `Ping`.
    /// A bridge frozen in the middle of a message sends nothing more, so it is found
    /// unresponsive one `pong_timeout` after its last byte.
    fn still_sending(&self, now: Instant) -> bool {
        self.stream_progress
            .is_some_and(|at| now.saturating_duration_since(at) <= self.timing.pong_timeout)
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
            Some((_, since))
                if now.duration_since(since) > self.timing.pong_timeout
                    && !self.still_sending(now) =>
            {
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
