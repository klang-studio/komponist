//! The app side of plugin hosting: bridge lifecycle, device status, parameters, state snapshots and
//! GUI message routing (ARCHITECTURE §8.1, §8.4, §8.6).
//!
//! One bridge process per external device. `create_processor` starts it and returns an
//! [`ExternalProcessor`](crate::ExternalProcessor) at once; loading continues in the background
//! (status `Loading`, the processor passes audio through or stays silent meanwhile). The manager
//! watches every bridge's pidfd: a dead bridge turns the device `Crashed` (red edge, the track keeps
//! running); "Neu laden" ([`PluginManager::reload`]) starts a new bridge with the last known state.
//!
//! State snapshots are taken on save ([`PluginManager::snapshot_states`]), on GUI close and,
//! debounced, after "state dirty"; they arrive as [`PluginManagerEvent::StateSnapshot`] and the app
//! stores them with `edits::set_device_state` through `Document::apply_untracked`.
//!
//! Built-in in-process devices (pisynth) are not managed here; the app creates their processors.
//! Their status is always `Ok`.
//!
//! Start order per bridge: memfd, `Load`, `SetState` (if there is a state), `Activate`, so the
//! plugin starts processing with its state. Messages to a bridge go through a writer thread of its
//! own; a bridge busy in a long plugin call (a cold Surge XT loads for seconds) never blocks the UI
//! thread. Watchdog: a `Ping` every second, the `Pong` due within 5 s; paused while the bridge
//! loads or opens its GUI, which may take up to a minute. A bridge that misses it is
//! `Crashed(Unresponsive)` and killed.
//!
//! Thread: UI thread only, non-blocking except where noted.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_engine::Processor;
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{ControlMsg, GuiSize, LogLevel, NoteName, ParamInfo, ParamValue, RenderMode};
use komponist_model::{Device, DeviceId, DeviceRole, PluginRef, Project, TrackId};
use rustix::event::{PollFd, PollFlags, Timespec};

use crate::bridge::{Bridge, Collected, SpawnSetup, StatePurpose, Timing};
use crate::catalog::{
    Catalog, PluginDirs, PluginLocation, SCAN_CACHE_FILE, SCAN_TIMEOUT, ScanCache, ScanReport,
};
use crate::scan::{ScanJob, ScanSetup};

/// How long a released bridge may take for `Shutdown` before it is killed.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Start of the `Log` line (level `Warning`) reporting that a device's stored state could not be
/// loaded; the reason follows. See [`PluginManager::state_warning`].
pub const STATE_LOG_PREFIX: &str = "state not loaded:";

/// Runtime status of a device (not part of the document).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DeviceStatus {
    /// Bridge starting or plugin loading ("Lädt"): audio passes through (effect) or is silent
    /// (instrument).
    Loading,
    /// Running.
    Ok,
    /// The bridge died or the plugin failed ("Abgestürzt", "Neu laden").
    Crashed(FailReason),
    /// The plugin is not installed ("Fehlt", S06): placeholder, state kept.
    Missing,
}

/// Why a device is `Crashed`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FailReason {
    /// The bridge process exited (`signal` set when killed, e.g. SIGSEGV = 11).
    Exited {
        /// Exit code, if it exited normally.
        code: Option<i32>,
        /// Terminating signal, if killed.
        signal: Option<i32>,
    },
    /// Load or activation failed, or the bridge could not start; the message. (A stored state
    /// that fails to load does not fail the device: see [`PluginManager::state_warning`].)
    LoadFailed(String),
    /// The bridge stopped answering control messages within the timeout.
    Unresponsive,
}

/// Something the app has to react to, returned by [`PluginManager::poll`].
#[derive(Debug, Clone, PartialEq)]
pub enum PluginManagerEvent {
    /// A device's status changed (redraw cards, headers, insert slots).
    StatusChanged {
        /// The device.
        device: DeviceId,
        /// New status.
        status: DeviceStatus,
    },
    /// A fresh state blob; store it in the document (untracked).
    StateSnapshot {
        /// The device.
        device: DeviceId,
        /// The state.
        state: Arc<[u8]>,
    },
    /// The parameter list changed (rebuild the generic view).
    ParamsChanged {
        /// The device.
        device: DeviceId,
    },
    /// Parameter values changed (plugin GUI edits); new values are in
    /// [`PluginManager::param_value`].
    ParamValuesChanged {
        /// The device.
        device: DeviceId,
    },
    /// The latency changed: recompile the program (delay compensation).
    LatencyChanged {
        /// The device.
        device: DeviceId,
        /// Frames.
        latency: u32,
    },
    /// The note names changed (step rows may follow them).
    NoteNamesChanged {
        /// The device.
        device: DeviceId,
    },
    /// The plugin GUI asks for a size: resize the frame window, then call
    /// [`PluginManager::set_gui_size`].
    GuiSizeRequest {
        /// The device.
        device: DeviceId,
        /// Requested size in physical pixels.
        size: GuiSize,
    },
    /// The plugin GUI opened inside the frame.
    GuiOpened {
        /// The device.
        device: DeviceId,
        /// Initial size in physical pixels.
        size: GuiSize,
        /// Whether the frame may resize it.
        resizable: bool,
    },
    /// The plugin GUI closed (by the plugin or after `close_gui`): close the frame window.
    GuiClosed {
        /// The device.
        device: DeviceId,
    },
    /// A background scan finished; the catalog is updated.
    ScanFinished(ScanReport),
    /// A log line from a bridge or plugin.
    Log {
        /// The device, if the line came from a bridge.
        device: Option<DeviceId>,
        /// Severity.
        level: LogLevel,
        /// Text.
        message: String,
    },
}

/// A device whose plugin is not installed (S06 dialog: "Spur Arp · Instrument", plugin id).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MissingPlugin {
    /// The track, `None` for the master chain.
    pub track: Option<TrackId>,
    /// The device.
    pub device: DeviceId,
    /// Instrument or effect.
    pub role: DeviceRole,
    /// Position in the chain (0 = instrument; "Effekt 4" = index 4).
    pub index: usize,
    /// What is missing (name, format, id).
    pub plugin: PluginRef,
}

/// Why a plugin manager call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginError {
    /// The plugin is not in the catalog (device status becomes `Missing`).
    NotInstalled(PluginRef),
    /// The plugin is built in and runs in process; the app creates its processor.
    InProcess(PluginRef),
    /// No bridge for this device.
    UnknownDevice(DeviceId),
    /// The bridge process could not be started.
    Spawn(String),
    /// Shared memory or control socket failure.
    Ipc(String),
    /// The plugin has no GUI.
    NoGui,
}

impl core::fmt::Display for PluginError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PluginError::NotInstalled(p) => write!(f, "plugin {} ({}) not installed", p.name, p.id),
            PluginError::InProcess(p) => write!(f, "plugin {} runs in process", p.id),
            PluginError::UnknownDevice(d) => write!(f, "no bridge for device {}", d.0),
            PluginError::Spawn(m) => write!(f, "bridge start failed: {m}"),
            PluginError::Ipc(m) => write!(f, "bridge communication failed: {m}"),
            PluginError::NoGui => write!(f, "plugin has no GUI"),
        }
    }
}

impl std::error::Error for PluginError {}

/// Fixed setup of the plugin manager.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginManagerConfig {
    /// Path of the `komponist-bridge` binary (next to the app binary by default).
    pub bridge_exe: PathBuf,
    /// Path of the scan cache (`~/.cache/komponist/plugins.json`).
    pub cache_file: PathBuf,
    /// The bundled `Klopfer.clap`, if present.
    pub klopfer_bundle: Option<PathBuf>,
    /// Scan processes running at once.
    pub parallel_scans: usize,
}

impl PluginManagerConfig {
    /// The default setup of the app: `komponist-bridge` next to the running executable, the cache
    /// in `$XDG_CACHE_HOME/komponist/` (else `~/.cache/komponist/`), klopfer from the first of
    /// `<exe>/../lib/komponist/plugins/Klopfer.clap` (installed), the sibling checkout's
    /// `../klopfer/target/bundled/Klopfer.clap` (development, `scripts/build-klopfer.sh`) and
    /// `~/.clap/Klopfer.clap` that exists, and half the CPU cores (1 ..= 8) for scans.
    pub fn defaults() -> PluginManagerConfig {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let cache_dir = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|d| d.is_absolute())
            .or_else(|| home.as_ref().map(|h| h.join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        let dev_klopfer = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../klopfer/target/bundled/Klopfer.clap");
        let klopfer_bundle = [
            Some(exe_dir.join("../lib/komponist/plugins/Klopfer.clap")),
            Some(dev_klopfer),
            home.map(|h| h.join(".clap/Klopfer.clap")),
        ]
        .into_iter()
        .flatten()
        .find_map(|p| std::fs::canonicalize(p).ok());
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        PluginManagerConfig {
            bridge_exe: exe_dir.join("komponist-bridge"),
            cache_file: cache_dir.join("komponist").join(SCAN_CACHE_FILE),
            klopfer_bundle,
            parallel_scans: (cores / 2).clamp(1, 8),
        }
    }
}

/// How a processor is prepared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessorSetup {
    /// The engine's sample rate in Hz.
    pub sample_rate: f32,
    /// The engine's `max_block`.
    pub max_block: usize,
}

/// Owner of all bridges and the catalog. UI thread.
///
/// Dropping the manager kills every bridge (SIGKILL; there is no `PR_SET_PDEATHSIG`) and cancels
/// a running scan; [`PluginManager::shutdown`] is the graceful way.
#[derive(Debug)]
pub struct PluginManager {
    config: PluginManagerConfig,
    catalog: Catalog,
    cache: ScanCache,
    scan: Option<ScanJob>,
    last_scan: Option<ScanReport>,
    /// One bridge per external device.
    bridges: BTreeMap<DeviceId, Bridge>,
    /// Status of devices without a bridge: `Missing`, or `Crashed` when the bridge could not start.
    statuses: BTreeMap<DeviceId, DeviceStatus>,
    /// Released or replaced bridges, until their process is gone.
    retiring: Vec<Bridge>,
    /// Events that happened outside `poll` (returned by the next `poll`).
    pending: Vec<PluginManagerEvent>,
    render_mode: RenderMode,
    timing: Timing,
}

impl PluginManager {
    /// Loads the scan cache into the catalog (built-ins plus cached plugins). Starts no process.
    pub fn new(config: PluginManagerConfig) -> PluginManager {
        let cache = ScanCache::load(&config.cache_file);
        let mut catalog = Catalog::builtin(config.klopfer_bundle.clone());
        catalog.set_scanned(&cache.descriptors());
        PluginManager {
            config,
            catalog,
            cache,
            scan: None,
            last_scan: None,
            bridges: BTreeMap::new(),
            statuses: BTreeMap::new(),
            retiring: Vec::new(),
            pending: Vec::new(),
            render_mode: RenderMode::Realtime,
            timing: Timing::default(),
        }
    }

    /// The catalog.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Starts a background scan of `dirs` (bundles unchanged since the cache are not rescanned);
    /// ends with [`PluginManagerEvent::ScanFinished`]. A running scan is restarted.
    pub fn start_scan(&mut self, dirs: &PluginDirs) {
        // Dropping a running job cancels it and kills its scan processes.
        self.scan = None;
        self.scan = Some(ScanJob::start(ScanSetup {
            bridge_exe: self.config.bridge_exe.clone(),
            dirs: dirs.clone(),
            cache: self.cache.clone(),
            cache_file: self.config.cache_file.clone(),
            parallel: self.config.parallel_scans,
            timeout: SCAN_TIMEOUT,
        }));
    }

    /// Whether a scan runs.
    pub fn is_scanning(&self) -> bool {
        self.scan.is_some()
    }

    /// The last finished scan.
    pub fn last_scan(&self) -> Option<&ScanReport> {
        self.last_scan.as_ref()
    }

    /// Devices of `project` whose plugin is not in the catalog (S06 dialog on open).
    pub fn missing_plugins(&self, project: &Project) -> Vec<MissingPlugin> {
        let chains = project
            .tracks
            .iter()
            .map(|t| (Some(t.id), &t.channel.devices))
            .chain(std::iter::once((None, &project.master.devices)));
        let mut missing = Vec::new();
        for (track, devices) in chains {
            for (index, device) in devices.iter().enumerate() {
                if self.catalog.resolve(&device.plugin).is_none() {
                    missing.push(MissingPlugin {
                        track,
                        device: device.id,
                        role: device.role,
                        index,
                        plugin: device.plugin.clone(),
                    });
                }
            }
        }
        missing
    }

    /// Starts a bridge for `device` and returns its processor (already activated for `setup`) for
    /// `Command::InsertProcessor`. Loading continues in the background with the device's stored
    /// state; status `Loading` until the bridge is active. Errors: `NotInstalled` (status `Missing`),
    /// `InProcess` for pisynth, `Spawn` / `Ipc`.
    pub fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Result<Box<dyn Processor>, PluginError> {
        self.start(device, setup, device.state.clone())
    }

    /// "Neu laden": kills the old bridge if still alive and starts a new one with the last known
    /// state (the latest snapshot, else `device.state`). The returned processor replaces the old one
    /// in the same slot (`InsertProcessor`).
    pub fn reload(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Result<Box<dyn Processor>, PluginError> {
        let state = self
            .bridges
            .get(&device.id)
            .and_then(|b| b.last_state.clone())
            .or_else(|| device.state.clone());
        if let Some(mut old) = self.bridges.remove(&device.id) {
            old.kill();
            self.retiring.push(old);
        }
        self.start(device, setup, state)
    }

    /// Resolves `device`, starts its bridge with `state` and records the status.
    fn start(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
        state: Option<Arc<[u8]>>,
    ) -> Result<Box<dyn Processor>, PluginError> {
        let Some(entry) = self.catalog.resolve(&device.plugin) else {
            self.retire(device.id, false);
            self.set_status(device.id, DeviceStatus::Missing);
            return Err(PluginError::NotInstalled(device.plugin.clone()));
        };
        let (format, bundle, plugin_id) = match &entry.location {
            PluginLocation::InProcess(_) => {
                return Err(PluginError::InProcess(device.plugin.clone()));
            }
            PluginLocation::Bundle {
                format,
                path,
                plugin_id,
            } => (*format, path.clone(), plugin_id.clone()),
        };
        if !(setup.sample_rate.is_finite() && setup.sample_rate > 0.0)
            || !(1..=MAX_BLOCK).contains(&setup.max_block)
        {
            return Err(PluginError::Ipc(format!(
                "invalid processor setup {setup:?}"
            )));
        }
        // A device has one bridge: an earlier one goes.
        self.retire(device.id, false);
        let started = Bridge::spawn(SpawnSetup {
            exe: &self.config.bridge_exe,
            device: device.id,
            role: device.role,
            format,
            bundle: &bundle,
            plugin_id: &plugin_id,
            state,
            setup,
            timing: self.timing,
        });
        match started {
            Ok(started) => {
                if self.render_mode != RenderMode::Realtime {
                    started
                        .bridge
                        .send(ControlMsg::SetRenderMode(self.render_mode));
                }
                self.statuses.remove(&device.id);
                self.bridges.insert(device.id, started.bridge);
                self.pending.push(PluginManagerEvent::StatusChanged {
                    device: device.id,
                    status: DeviceStatus::Loading,
                });
                Ok(Box::new(started.processor))
            }
            Err(error) => {
                self.set_status(
                    device.id,
                    DeviceStatus::Crashed(FailReason::LoadFailed(error.to_string())),
                );
                Err(error)
            }
        }
    }

    /// Records the status of a device without bridge and reports a change.
    fn set_status(&mut self, device: DeviceId, status: DeviceStatus) {
        if self.statuses.get(&device) != Some(&status) {
            self.statuses.insert(device, status.clone());
            self.pending
                .push(PluginManagerEvent::StatusChanged { device, status });
        }
    }

    /// Takes the bridge of `device` out: `Shutdown` with a grace period if `graceful`, else
    /// SIGKILL. Its process is reaped by later polls.
    fn retire(&mut self, device: DeviceId, graceful: bool) {
        if let Some(mut bridge) = self.bridges.remove(&device) {
            if graceful {
                bridge.stop(SHUTDOWN_GRACE);
            } else {
                bridge.kill();
            }
            self.retiring.push(bridge);
        }
    }

    /// The device left the project: shuts its bridge down (after the engine removed the processor;
    /// the processor tolerates a dead bridge either way).
    pub fn release(&mut self, device: DeviceId) {
        self.retire(device, true);
        self.statuses.remove(&device);
    }

    /// Status of a device; `Ok` for devices this manager does not manage (in-process built-ins).
    pub fn status(&self, device: DeviceId) -> DeviceStatus {
        match self.bridges.get(&device) {
            Some(bridge) => bridge.status().clone(),
            None => self
                .statuses
                .get(&device)
                .cloned()
                .unwrap_or(DeviceStatus::Ok),
        }
    }

    /// Call every UI frame: reads control messages and plugin output events, checks pidfds and scan
    /// workers, and returns what happened. Never blocks.
    pub fn poll(&mut self) -> Vec<PluginManagerEvent> {
        let mut events = std::mem::take(&mut self.pending);
        let mut collected = Vec::new();
        for (&device, bridge) in &mut self.bridges {
            bridge.poll(&mut events, &mut collected);
            // Answers to a `snapshot_states` that gave up waiting still belong in the document.
            for (_, state) in collected.drain(..) {
                events.push(PluginManagerEvent::StateSnapshot { device, state });
            }
        }
        let mut ignored = Vec::new();
        for bridge in &mut self.retiring {
            bridge.poll(&mut ignored, &mut collected);
            ignored.clear();
            collected.clear();
        }
        self.retiring.retain(|b| !b.is_finished());
        if let Some(outcome) = self.scan.as_mut().and_then(ScanJob::try_finish) {
            self.scan = None;
            self.cache = outcome.cache;
            self.catalog.set_scanned(&self.cache.descriptors());
            if let Some(error) = outcome.cache_error {
                events.push(PluginManagerEvent::Log {
                    device: None,
                    level: LogLevel::Warning,
                    message: format!("plugin scan cache not written: {error}"),
                });
            }
            self.last_scan = Some(outcome.report.clone());
            events.push(PluginManagerEvent::ScanFinished(outcome.report));
        }
        events
    }

    /// The parameter list of a device's plugin (generic parameter grid).
    pub fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        self.bridges.get(&device).map(|b| b.params.as_slice())
    }

    /// The last known value and text of parameter `id`.
    pub fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        self.bridges.get(&device)?.values.get(&id)
    }

    /// Sets a parameter from the generic view or a device-card knob (sent over the control socket;
    /// the bridge delivers it to the plugin's audio side too).
    pub fn set_param(&mut self, device: DeviceId, id: u32, value: f64) {
        if let Some(bridge) = self.bridges.get_mut(&device) {
            bridge.set_param(id, value);
        }
    }

    /// Asks for a state snapshot; it arrives as [`PluginManagerEvent::StateSnapshot`] (not while
    /// the device has a [`Self::state_warning`]).
    pub fn request_state(&mut self, device: DeviceId) {
        if let Some(bridge) = self.bridges.get_mut(&device) {
            bridge.request_state(StatePurpose::Event);
        }
    }

    /// Blocking, for saving: snapshots every running external device, waiting at most `timeout` in
    /// total. Devices that do not answer in time are left out (their last snapshot stays in the
    /// document), and so are devices with a [`Self::state_warning`]. Sorted by device id; a late
    /// answer arrives later as a `StateSnapshot` event.
    pub fn snapshot_states(&mut self, timeout: Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        let deadline = Instant::now() + timeout;
        let mut waiting: BTreeMap<DeviceId, u32> = self
            .bridges
            .iter_mut()
            .filter_map(|(&device, b)| Some((device, b.request_state(StatePurpose::Collect)?)))
            .collect();
        let mut states = Vec::new();
        let mut collected: Vec<Collected> = Vec::new();
        while !waiting.is_empty() {
            for (&device, bridge) in &mut self.bridges {
                let Some(&request) = waiting.get(&device) else {
                    continue;
                };
                bridge.poll(&mut self.pending, &mut collected);
                for (answer, state) in collected.drain(..) {
                    if answer == request {
                        states.push((device, state));
                        waiting.remove(&device);
                    } else {
                        // An earlier `snapshot_states` gave up on this one.
                        self.pending
                            .push(PluginManagerEvent::StateSnapshot { device, state });
                    }
                }
                if !bridge.is_running() {
                    waiting.remove(&device);
                }
            }
            let now = Instant::now();
            if waiting.is_empty() || now >= deadline {
                break;
            }
            let mut fds: Vec<PollFd<'_>> = waiting
                .keys()
                .filter_map(|d| self.bridges.get(d))
                .map(|b| PollFd::from_borrowed_fd(b.socket(), PollFlags::IN))
                .collect();
            let wait = (deadline - now).min(Duration::from_millis(50));
            let ts = Timespec {
                tv_sec: wait.as_secs() as i64,
                tv_nsec: i64::from(wait.subsec_nanos()),
            };
            let _ = rustix::event::poll(&mut fds, Some(&ts));
        }
        states.sort_by_key(|(device, _)| *device);
        states
    }

    /// Note names of a device's plugin (empty if none or unknown).
    pub fn note_names(&self, device: DeviceId) -> &[NoteName] {
        self.bridges
            .get(&device)
            .map_or(&[][..], |b| b.note_names.as_slice())
    }

    /// Latency of a device in frames (0 if unknown); feeds `compile::CompileEnv::device_latency`.
    pub fn latency(&self, device: DeviceId) -> u32 {
        self.bridges.get(&device).map_or(0, |b| b.latency)
    }

    /// Blocks of the device's current bridge that missed the callback deadline or found the bridge
    /// still busy with an earlier block (each one got the fallback: passthrough or silence). For a
    /// "late" indicator in the UI; counts from the bridge's start (a reload starts at 0), 0 for
    /// devices without a bridge.
    pub fn late_blocks(&self, device: DeviceId) -> u64 {
        self.bridges.get(&device).map_or(0, Bridge::late_blocks)
    }

    /// Why the device's stored state could not be loaded, while that matters (UI warning
    /// „Zustand konnte nicht geladen werden“ on the device).
    ///
    /// When the plugin refuses the stored state, the device still starts (status `Ok`) with what
    /// the plugin has, usually its defaults. The manager then reports one `Log` event at level
    /// `Warning` whose message starts with [`STATE_LOG_PREFIX`], and keeps the stored blob: no
    /// `StateSnapshot`, nothing from [`Self::snapshot_states`] or [`Self::request_state`] for this
    /// device, so the document keeps the blob for a later fix or another machine. The first change
    /// by the user ([`Self::set_param`], an edit in the plugin's GUI, "state dirty" from the
    /// running plugin) ends this: the warning clears and snapshots resume. [`Self::reload`] tries
    /// the stored blob again. `None` for devices without such a problem or without a bridge.
    pub fn state_warning(&self, device: DeviceId) -> Option<&str> {
        self.bridges.get(&device)?.state_warning.as_deref()
    }

    /// Whether a device's plugin has its own GUI.
    pub fn has_gui(&self, device: DeviceId) -> bool {
        self.bridges.get(&device).is_some_and(|b| b.has_gui)
    }

    /// Opens the plugin GUI inside the X11 window `parent_xid` (the frame's child window) at UI
    /// `scale`; answered by [`PluginManagerEvent::GuiOpened`].
    pub fn open_gui(
        &mut self,
        device: DeviceId,
        parent_xid: u64,
        scale: f64,
    ) -> Result<(), PluginError> {
        self.bridges
            .get_mut(&device)
            .ok_or(PluginError::UnknownDevice(device))?
            .open_gui(parent_xid, scale)
    }

    /// Tells the plugin GUI its new size (after the frame resized).
    pub fn set_gui_size(&mut self, device: DeviceId, size: GuiSize) {
        if let Some(bridge) = self.bridges.get(&device) {
            bridge.send(ControlMsg::SetGuiSize(size));
        }
    }

    /// Closes the plugin GUI (frame closed) and requests a state snapshot.
    pub fn close_gui(&mut self, device: DeviceId) {
        if let Some(bridge) = self.bridges.get_mut(&device) {
            bridge.close_gui();
        }
    }

    /// Switches every bridge to realtime or offline processing (around an export).
    pub fn set_render_mode(&mut self, mode: RenderMode) {
        self.render_mode = mode;
        for bridge in self.bridges.values() {
            bridge.send(ControlMsg::SetRenderMode(mode));
        }
    }

    /// Shuts all bridges down (app exit): `Shutdown` to each, up to [`SHUTDOWN_GRACE`] in total
    /// for them to exit, then SIGKILL for the rest. Cancels a running scan. Blocking.
    pub fn shutdown(&mut self) {
        self.scan = None;
        let mut all: Vec<Bridge> = std::mem::take(&mut self.bridges).into_values().collect();
        all.append(&mut self.retiring);
        for bridge in &mut all {
            bridge.stop(SHUTDOWN_GRACE);
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        for bridge in &mut all {
            let left = deadline.saturating_duration_since(Instant::now());
            if !bridge.wait_exit(left) {
                bridge.kill();
            }
        }
        // Dropping reaps the killed ones.
        drop(all);
        self.statuses.clear();
    }

    /// Test hook: shorter watchdog and debounce times for bridges started from now on.
    #[doc(hidden)]
    pub fn set_watchdog_timing(&mut self, ping_interval: Duration, pong_timeout: Duration) {
        self.timing.ping_interval = ping_interval;
        self.timing.pong_timeout = pong_timeout;
    }

    /// Process id of a device's bridge (diagnostics and tests).
    #[doc(hidden)]
    pub fn bridge_pid(&self, device: DeviceId) -> Option<u32> {
        self.bridges.get(&device).map(Bridge::pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_points_next_to_the_executable() {
        let config = PluginManagerConfig::defaults();
        let exe_dir = std::env::current_exe().unwrap();
        assert_eq!(
            config.bridge_exe,
            exe_dir.parent().unwrap().join("komponist-bridge")
        );
        assert!(config.cache_file.ends_with("komponist/plugins.json"));
        assert!(config.cache_file.is_absolute());
        assert!((1..=8).contains(&config.parallel_scans));
        if let Some(klopfer) = &config.klopfer_bundle {
            assert!(klopfer.is_absolute() && klopfer.ends_with("Klopfer.clap"));
        }
    }

    #[test]
    fn a_new_manager_lists_the_builtins_and_manages_nothing() {
        let dir = std::env::temp_dir().join(format!("kp-manager-new-{}", std::process::id()));
        let manager = PluginManager::new(PluginManagerConfig {
            bridge_exe: "/nonexistent/komponist-bridge".into(),
            cache_file: dir.join("plugins.json"),
            klopfer_bundle: None,
            parallel_scans: 1,
        });
        assert_eq!(manager.catalog().entries().len(), 1);
        assert!(!manager.is_scanning());
        assert_eq!(manager.last_scan(), None);
        assert_eq!(manager.status(DeviceId(1)), DeviceStatus::Ok);
        assert_eq!(manager.latency(DeviceId(1)), 0);
        assert!(manager.note_names(DeviceId(1)).is_empty());
        assert!(manager.params(DeviceId(1)).is_none());
        assert!(!manager.has_gui(DeviceId(1)));
    }

    #[test]
    fn a_missing_bridge_binary_is_a_spawn_error_with_status() {
        let dir = std::env::temp_dir().join(format!("kp-manager-spawn-{}", std::process::id()));
        let mut manager = PluginManager::new(PluginManagerConfig {
            bridge_exe: "/nonexistent/komponist-bridge".into(),
            cache_file: dir.join("plugins.json"),
            klopfer_bundle: Some("/nonexistent/Klopfer.clap".into()),
            parallel_scans: 1,
        });
        let klopfer = Device {
            id: DeviceId(5),
            role: DeviceRole::Instrument,
            plugin: manager.catalog().entries()[1].plugin.clone(),
            bypass: false,
            state: None,
        };
        let setup = ProcessorSetup {
            sample_rate: 48_000.0,
            max_block: 512,
        };
        assert!(matches!(
            manager.create_processor(&klopfer, setup),
            Err(PluginError::Spawn(_))
        ));
        assert!(matches!(
            manager.status(DeviceId(5)),
            DeviceStatus::Crashed(FailReason::LoadFailed(_))
        ));
        let bad = ProcessorSetup {
            sample_rate: 48_000.0,
            max_block: MAX_BLOCK + 1,
        };
        assert!(matches!(
            manager.create_processor(&klopfer, bad),
            Err(PluginError::Ipc(_))
        ));
        let events = manager.poll();
        assert!(events.iter().any(|e| matches!(
            e,
            PluginManagerEvent::StatusChanged {
                device: DeviceId(5),
                status: DeviceStatus::Crashed(_)
            }
        )));
    }
}
