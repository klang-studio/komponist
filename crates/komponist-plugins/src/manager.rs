//! The app side of plugin hosting: bridge lifecycle, device status, parameters, state snapshots and
//! GUI message routing (ARCHITECTURE §8.1, §8.4, §8.6).
//!
//! One bridge process per external device. `create_processor` starts it and returns an
//! [`ExternalProcessor`](crate::ExternalProcessor) at once; loading continues in the background
//! (status `Loading`, the processor passes audio through or stays silent meanwhile). The manager
//! watches every bridge's pidfd: a dead bridge turns the device `Crashed` (red edge, the track keeps
//! running); "Neu laden" ([`PluginManager::reload`]) starts a new bridge with the last known state.
//!
//! State snapshots are taken on save ([`PluginManager::snapshot_for_save`]), on GUI close and,
//! debounced, after "state dirty"; they arrive as [`PluginManagerEvent::StateSnapshot`] and the app
//! stores them with `edits::set_device_state` as tracked, coalesced undo steps (`komponist-app`'s
//! `param_undo`).
//!
//! Built-in in-process devices (hexa) are not managed here; the app creates their processors.
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

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_engine::Processor;
use komponist_ipc::frame::{FrameChord, FrameKeyEvent, FrameRect, HeaderEvent, HeaderModel};
use komponist_ipc::shm::MAX_BLOCK;
use komponist_ipc::{
    ControlMsg, GuiSize, GuiTarget, LogLevel, NoteName, ParamInfo, ParamValue, RenderMode,
};
use komponist_model::{Device, DeviceId, DeviceRole, PluginRef, Project, TrackId};
#[cfg(unix)]
use rustix::event::{PollFd, PollFlags, Timespec};

use crate::bridge::{Bridge, Collected, SpawnSetup, StatePurpose, Timing};
use crate::bundled_clap::{BundleVerificationError, BundledClap};
use crate::catalog::{
    Catalog, Platform, PluginDirs, PluginLocation, SCAN_CACHE_FILE, SCAN_TIMEOUT, ScanCache,
    ScanReport, env_path,
};
use crate::direct_device::DirectDevice;
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
    /// (instrument). Also while a running bridge re-activates for another sample rate (export,
    /// device change; fallback audio meanwhile), and for [`crate::WARM_UP`] after the bridge
    /// reported the plugin active (audio already flows; some plugins, e.g. klopfer, still load
    /// in the background). An export waits for `Ok`.
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

/// State blobs captured from running plugin bridges.
pub type DeviceSnapshots = Vec<(DeviceId, Arc<[u8]>)>;

/// The outcome of collecting current external plug-in states for a project save.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SaveSnapshots {
    /// Fresh states that can be used even when another bridge did not answer.
    pub captured: DeviceSnapshots,
    /// Expected snapshots that were unavailable. The file can retain the document's state.
    pub unavailable: Vec<DeviceId>,
    /// Known persistent edits with no valid snapshot. These must block the save.
    pub blocked: Vec<DeviceId>,
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
    /// A plugin has applied a requested state restore to its processor.
    StateApplied {
        /// The restored device.
        device: DeviceId,
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
        /// Where the bridge's own frame was (macOS); `None` on Linux.
        rect: Option<FrameRect>,
    },
    /// The user clicked in the header of the bridge's own frame (macOS, #52).
    FrameHeaderAction {
        /// The device.
        device: DeviceId,
        /// What the user did.
        event: HeaderEvent,
    },
    /// The bridge's own frame took a key from the focused plugin for komponist (macOS).
    FrameShortcut {
        /// The device.
        device: DeviceId,
        /// The chord.
        chord: FrameChord,
    },
    /// A key event while the header of the bridge's own frame has the focus (macOS).
    FrameKey {
        /// The device.
        device: DeviceId,
        /// The key event.
        key: FrameKeyEvent,
    },
    /// The bridge's own frame moved or changed its size (macOS).
    FrameMoved {
        /// The device.
        device: DeviceId,
        /// Where it is now.
        rect: FrameRect,
    },
    /// The close button of the bridge's own frame (macOS): close it like ×.
    FrameCloseRequested {
        /// The device.
        device: DeviceId,
    },
    /// The window number of the bridge's own frame (macOS), for ordering frames.
    FrameWindow {
        /// The device.
        device: DeviceId,
        /// Window server id.
        number: i64,
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
    /// The plugin is not in the catalog (device status becomes `Missing`). Boxed to keep the error
    /// small (clippy `result_large_err`).
    NotInstalled(Box<PluginRef>),
    /// The plugin is built in and runs in process; the app creates its processor.
    InProcess(Box<PluginRef>),
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

/// First argument that makes the `komponist` executable run as a plugin bridge (WP-94).
pub const BRIDGE_FLAG: &str = "--bridge";

/// Fixed setup of the plugin manager.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PluginManagerConfig {
    /// The bridge launcher: this executable runs the bridge (the app's own executable by default,
    /// or the `komponist-bridge` binary) …
    pub bridge_exe: PathBuf,
    /// … with these leading arguments (`--bridge` for the app's executable). The bridge's own
    /// arguments (`--instance …`, `--scan <bundle>`) follow.
    pub bridge_args: Vec<OsString>,
    /// Path of the scan cache (`~/.cache/komponist/plugins.json`, on macOS
    /// `~/Library/Caches/komponist/plugins.json`).
    pub cache_file: PathBuf,
    /// Scan processes running at once.
    pub parallel_scans: usize,
}

impl PluginManagerConfig {
    /// The default setup of the app: the bridge is the running executable itself with `--bridge`
    /// (WP-94: nothing next to it can be missing or stale), the cache in
    /// `$XDG_CACHE_HOME/komponist/` (else `~/.cache/komponist/`; on macOS
    /// `~/Library/Caches/komponist/`, where the XDG variable is not honoured; on Windows
    /// `%LOCALAPPDATA%\komponist\`), and half the CPU cores (1 ..= 8) for scans. When the running executable cannot be determined, `bridge_exe` is empty and
    /// every start of a bridge fails with a clear [`PluginError::Spawn`].
    pub fn defaults() -> PluginManagerConfig {
        let exe = std::env::current_exe().unwrap_or_default();
        let cache_dir = cache_dir_from(Platform::CURRENT, &|name: &str| env_path(name))
            .unwrap_or_else(|| std::env::temp_dir().join("komponist"));
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        PluginManagerConfig {
            bridge_exe: exe,
            bridge_args: vec![OsString::from(BRIDGE_FLAG)],
            cache_file: cache_dir.join(SCAN_CACHE_FILE),
            parallel_scans: (cores / 2).clamp(1, 8),
        }
    }
}

/// The cache folder for a platform's convention and the environment `var` (a variable by name).
/// `None` without a usable home folder (Linux: without `$XDG_CACHE_HOME` too; Windows: without
/// an absolute `%LOCALAPPDATA%`).
fn cache_dir_from(platform: Platform, var: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let base = match platform {
        Platform::Macos => var("HOME")?.join("Library/Caches"),
        Platform::Linux => var("XDG_CACHE_HOME")
            .filter(|d| d.is_absolute())
            .or_else(|| var("HOME").map(|h| h.join(".cache")))?,
        Platform::Windows => var("LOCALAPPDATA").filter(|d| d.is_absolute())?,
    };
    Some(base.join("komponist"))
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
    private_clap_dir: Option<PathBuf>,
    bundled_clap: Option<BundledClap>,
    force_bundled_bridge: bool,
    direct: BTreeMap<DeviceId, DirectDevice>,
    retiring_direct: Vec<DirectDevice>,
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
        let direct = std::env::var_os("KOMPONIST_EXPERIMENTAL_DIRECT_CLAP")
            .is_some_and(|value| value == "1");
        let force_bridge = std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE")
            .is_some_and(|value| value == "1");
        Self::new_with_bundled_clap(config, direct, force_bridge)
    }

    pub(crate) fn new_with_bundled_clap(
        config: PluginManagerConfig,
        enable_bundles: bool,
        force_bundled_bridge: bool,
    ) -> PluginManager {
        let cache = ScanCache::load(&config.cache_file);
        let mut pending = Vec::new();
        let mut report = |error: BundleVerificationError| {
            pending.push(PluginManagerEvent::Log {
                device: None,
                level: LogLevel::Warning,
                message: error.to_string(),
            })
        };
        let private_clap_dir = BundledClap::installed_directory().unwrap_or_else(|error| {
            report(error);
            None
        });
        let bundled_clap = if enable_bundles {
            config.cache_file.parent().and_then(|path| {
                BundledClap::discover(path).unwrap_or_else(|error| {
                    report(error);
                    None
                })
            })
        } else {
            None
        };
        let mut manager = PluginManager {
            config,
            catalog: Catalog::builtin(),
            cache,
            scan: None,
            last_scan: None,
            bridges: BTreeMap::new(),
            private_clap_dir,
            bundled_clap,
            force_bundled_bridge,
            direct: BTreeMap::new(),
            retiring_direct: Vec::new(),
            statuses: BTreeMap::new(),
            retiring: Vec::new(),
            pending,
            render_mode: RenderMode::Realtime,
            timing: Timing::default(),
        };
        manager.refresh_catalog();
        manager
    }

    fn refresh_catalog(&mut self) {
        let mut descriptors = self.cache.descriptors();
        let shipped = |descriptor: &komponist_ipc::PluginDescriptor| {
            descriptor.format == komponist_ipc::PluginFormat::Clap
                && self.bundled_clap.as_ref().is_some_and(|bundled| {
                    bundled
                        .bundle_entries()
                        .any(|(path, id)| path == descriptor.bundle && id == descriptor.id)
                })
        };
        descriptors.retain(|descriptor| {
            !self
                .private_clap_dir
                .as_ref()
                .is_some_and(|path| descriptor.bundle.starts_with(path))
                || shipped(descriptor)
        });
        for descriptor in &mut descriptors {
            if shipped(descriptor)
                && descriptor.kind == komponist_ipc::PluginKind::Other
                && descriptor
                    .features
                    .iter()
                    .any(|feature| feature == "note-effect")
            {
                descriptor.kind = komponist_ipc::PluginKind::MidiEffect;
            }
        }
        // A duplicate scanner result may not replace the verified release artifact in the
        // catalog. Verification still runs at load time; scanner metadata grants no trust.
        descriptors.sort_by_key(|descriptor| !shipped(descriptor));
        self.catalog.set_scanned(&descriptors);
    }

    /// A processor removed from the engine must be handed back to its main-thread CLAP session.
    pub fn has_retiring_direct(&self) -> bool {
        !self.retiring_direct.is_empty()
    }

    /// Prepare a migrated copy of a loaded project when the verified bundled catalog is enabled.
    ///
    /// The source stays intact if any bundled device or parameter cannot be translated.
    pub fn migrate_project_for_open(
        &self,
        source: &Project,
        project_dir: &Path,
    ) -> Result<Option<Project>, String> {
        use crate::legacy_bundled::{
            bundled_clap_id, migrate_legacy_bundled_project, plan_legacy_bundled_device,
        };

        let Some(inventory) = &self.bundled_clap else {
            return Ok(None);
        };
        let plans = source
            .tracks
            .iter()
            .flat_map(|track| &track.channel.devices)
            .chain(&source.master.devices)
            .filter(|device| bundled_clap_id(&device.plugin).is_some())
            .map(|device| {
                plan_legacy_bundled_device(inventory, device, Some(project_dir)).map_err(|error| {
                    format!("device {} ({}): {error}", device.id.0, device.plugin.id)
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if plans.is_empty() {
            return Ok(None);
        }
        migrate_legacy_bundled_project(source, plans).map(|migration| Some(migration.project))
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
        let mut dirs = dirs.clone();
        if let Some(bundled) = &self.bundled_clap {
            dirs.clap.retain(|path| path != bundled.directory());
            dirs.clap.insert(0, bundled.directory().to_owned());
        } else if let Some(private) = &self.private_clap_dir {
            dirs.clap.retain(|path| path != private);
        }
        self.scan = Some(ScanJob::start(ScanSetup {
            bridge_exe: self.config.bridge_exe.clone(),
            bridge_args: self.config.bridge_args.clone(),
            dirs,
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
    /// `InProcess` for hexa, `Spawn` / `Ipc`.
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
            .or_else(|| {
                self.direct
                    .get(&device.id)
                    .and_then(|direct| direct.last_state.clone())
            })
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
            return Err(PluginError::NotInstalled(Box::new(device.plugin.clone())));
        };
        let (format, bundle, plugin_id) = match &entry.location {
            PluginLocation::InProcess(_) => {
                return Err(PluginError::InProcess(Box::new(device.plugin.clone())));
            }
            PluginLocation::Bundle {
                format,
                path,
                plugin_id,
            } => (*format, path.clone(), plugin_id.clone()),
        };
        let sidechain = entry.sidechain;
        if !(setup.sample_rate.is_finite() && setup.sample_rate > 0.0)
            || !(1..=MAX_BLOCK).contains(&setup.max_block)
        {
            return Err(PluginError::Ipc(format!(
                "invalid processor setup {setup:?}"
            )));
        }
        // A project device has one instance. A local instance keeps its control half alive until
        // the engine returns the audio half; external bridge retirement remains asynchronous.
        self.retire(device.id, false);
        if self
            .private_clap_dir
            .as_ref()
            .is_some_and(|root| bundle.starts_with(root))
        {
            let verified = self
                .bundled_clap
                .as_ref()
                .ok_or_else(|| PluginError::Ipc("bundled CLAP verification is unavailable".into()))
                .and_then(|bundled| {
                    bundled
                        .verify(&bundle, &plugin_id)
                        .map_err(|error| PluginError::Ipc(error.to_string()))?
                        .ok_or_else(|| {
                            PluginError::Ipc("bundle is not in the app release inventory".into())
                        })
                });
            let proof = match verified {
                Ok(proof) => proof,
                Err(error) => {
                    self.set_status(
                        device.id,
                        DeviceStatus::Crashed(FailReason::LoadFailed(error.to_string())),
                    );
                    return Err(error);
                }
            };
            if !self.force_bundled_bridge {
                return match DirectDevice::start(
                    proof,
                    device.role,
                    setup,
                    state,
                    self.timing.snapshot_debounce,
                ) {
                    Ok((mut instance, processor)) => {
                        self.statuses.remove(&device.id);
                        instance.report_warning(device.id, &mut self.pending);
                        DirectDevice::report_metadata(device.id, &mut self.pending);
                        self.pending.push(PluginManagerEvent::StatusChanged {
                            device: device.id,
                            status: DeviceStatus::Ok,
                        });
                        instance.set_render_mode(device.id, self.render_mode, &mut self.pending);
                        self.direct.insert(device.id, instance);
                        Ok(Box::new(processor))
                    }
                    Err(error) => {
                        let error = PluginError::Ipc(error.to_string());
                        self.set_status(
                            device.id,
                            DeviceStatus::Crashed(FailReason::LoadFailed(error.to_string())),
                        );
                        Err(error)
                    }
                };
            }
        }
        let started = Bridge::spawn(SpawnSetup {
            exe: &self.config.bridge_exe,
            args: &self.config.bridge_args,
            device: device.id,
            role: device.role,
            format,
            bundle: &bundle,
            plugin_id: &plugin_id,
            state,
            setup,
            timing: self.timing,
            sidechain,
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
        if let Some(direct) = self.direct.remove(&device) {
            self.retiring_direct.push(direct);
        }
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
        if let Some(direct) = self.direct.get(&device) {
            return direct.status.clone();
        }
        match self.bridges.get(&device) {
            Some(bridge) => bridge.status(),
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
        for (&id, direct) in &mut self.direct {
            direct.poll(id, &mut events);
        }
        self.retiring_direct.retain_mut(|direct| !direct.retired());
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
            self.refresh_catalog();
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
        self.direct
            .get(&device)
            .map(|direct| direct.params.as_slice())
            .or_else(|| self.bridges.get(&device).map(|b| b.params.as_slice()))
    }

    /// The validated native layout for a running bridge.
    pub fn native_layout(
        &self,
        device: DeviceId,
    ) -> Option<&Arc<komponist_ipc::native_layout::NativeLayout>> {
        if let Some(direct) = self.direct.get(&device) {
            return direct.layout.as_ref();
        }
        self.bridges.get(&device)?.native_layout.as_ref()
    }

    /// The last known value and text of parameter `id`.
    pub fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        if let Some(direct) = self.direct.get(&device) {
            return direct.values.get(&id);
        }
        self.bridges.get(&device)?.values.get(&id)
    }

    /// Value before any automation. The live cache may contain playback values.
    pub fn param_base(&self, device: DeviceId, id: u32) -> Option<f64> {
        if let Some(direct) = self.direct.get(&device) {
            return direct.base.get(&id).copied();
        }
        self.bridges.get(&device)?.base_values.get(&id).copied()
    }

    /// Sets a parameter from the generic view or a device-card knob (sent over the control socket;
    /// the bridge delivers it to the plugin's audio side too).
    pub fn set_param(&mut self, device: DeviceId, id: u32, value: f64) {
        if let Some(direct) = self.direct.get_mut(&device) {
            direct.set_param(id, value);
            self.pending
                .push(PluginManagerEvent::ParamValuesChanged { device });
        } else if let Some(bridge) = self.bridges.get_mut(&device) {
            bridge.set_param(id, value);
        }
    }

    /// Records an edit already sent to the processor, either from the app or a plugin GUI.
    pub fn edited_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        if let Some(direct) = self.direct.get_mut(&device) {
            let edited = direct.edited_param(id, value);
            if edited {
                self.pending
                    .push(PluginManagerEvent::ParamValuesChanged { device });
            }
            return edited;
        }
        let Some(bridge) = self.bridges.get_mut(&device).filter(|b| b.running()) else {
            return false;
        };
        bridge.edited_param(id, value);
        self.pending
            .push(PluginManagerEvent::ParamValuesChanged { device });
        true
    }

    /// Reflects a plugin GUI's temporary automation value without scheduling a snapshot.
    pub fn preview_param(&mut self, device: DeviceId, id: u32, value: f64) {
        if let Some(direct) = self.direct.get_mut(&device) {
            direct.preview_param(id, value);
            self.pending
                .push(PluginManagerEvent::ParamValuesChanged { device });
            return;
        }
        if let Some(bridge) = self.bridges.get_mut(&device).filter(|b| b.running()) {
            bridge.preview_param(id, value);
            self.pending
                .push(PluginManagerEvent::ParamValuesChanged { device });
        }
    }

    /// The plain output value of every mapped native plugin parameter in the active program.
    pub fn set_macro_params(&mut self, values: &[(DeviceId, u32, f64)]) {
        let mut by_device = BTreeMap::<_, BTreeMap<_, _>>::new();
        for &(device, id, value) in values {
            by_device.entry(device).or_default().insert(id, value);
        }
        for (&device, direct) in &mut self.direct {
            direct.set_macro_params(by_device.remove(&device).unwrap_or_default());
        }
        for (&device, bridge) in &mut self.bridges {
            bridge.set_macro_params(by_device.remove(&device).unwrap_or_default());
        }
    }

    /// A verified snapshot with a macro change and no pending independent parameter edit.
    pub fn is_macro_snapshot(&self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        if let Some(direct) = self.direct.get(&device) {
            return direct.is_macro_snapshot(state);
        }
        self.bridges
            .get(&device)
            .is_some_and(|bridge| bridge.is_macro_snapshot(state))
    }

    /// Restores `state` in the running plugin of `device` (undo or redo changed the device's
    /// state in the document); `false` if the device has no running bridge.
    pub fn set_state(&mut self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        if let Some(direct) = self.direct.get_mut(&device) {
            direct.set_state(state.clone());
            return true;
        }
        self.bridges
            .get_mut(&device)
            .is_some_and(|bridge| bridge.set_state(state))
    }

    /// Asks for a state snapshot; it arrives as [`PluginManagerEvent::StateSnapshot`] (not while
    /// the device has a [`Self::state_warning`]).
    pub fn request_state(&mut self, device: DeviceId) {
        if let Some(direct) = self.direct.get_mut(&device) {
            direct.request_state();
            return;
        }
        if let Some(bridge) = self.bridges.get_mut(&device) {
            bridge.request_state(StatePurpose::Event);
        }
    }

    /// Best-effort snapshots of running external devices, waiting at most `timeout` in
    /// total. Devices that do not answer in time are left out (their last snapshot stays in the
    /// document), and so are devices with a [`Self::state_warning`]. Sorted by device id; a late
    /// answer arrives later as a `StateSnapshot` event.
    pub fn snapshot_states(&mut self, timeout: Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        self.snapshot_states_if(timeout, |_| true)
    }

    /// Whether a running plugin has edits that are newer than its last safe state.
    pub fn has_pending_state_edits(&self) -> bool {
        self.bridges.values().any(Bridge::needs_snapshot_for_save)
            || self.direct.values().any(DirectDevice::needs_snapshot)
    }

    /// Collects current plug-in states under one shared deadline. An unanswered request from an
    /// otherwise running bridge is reported separately from a known unsaved persistent edit.
    pub fn snapshot_for_save(&mut self, timeout: Duration) -> SaveSnapshots {
        let deadline = Instant::now() + timeout;
        let mut expected = BTreeSet::new();
        let mut required = BTreeSet::new();
        for (&device, bridge) in &self.bridges {
            if bridge.expects_snapshot_for_save() {
                expected.insert(device);
            }
            if bridge.needs_snapshot_for_save() {
                required.insert(device);
            }
        }
        for (&device, direct) in &self.direct {
            if direct.can_snapshot() {
                expected.insert(device);
            }
            if direct.needs_snapshot() {
                required.insert(device);
            }
        }
        let mut captured =
            self.snapshot_states_if(deadline.saturating_duration_since(Instant::now()), |_| true);
        loop {
            for (device, _) in &captured {
                expected.remove(device);
                required.remove(device);
            }
            let now = Instant::now();
            if now >= deadline
                || !required.iter().any(|device| {
                    self.bridges.get(device).is_some_and(Bridge::is_running)
                        || self
                            .direct
                            .get(device)
                            .is_some_and(DirectDevice::needs_snapshot)
                })
            {
                break;
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(30)));
            captured.extend(self.snapshot_states_if(
                deadline.saturating_duration_since(Instant::now()),
                |device| required.contains(&device),
            ));
        }
        captured.sort_by_key(|(device, _)| *device);
        SaveSnapshots {
            captured,
            unavailable: expected.difference(&required).copied().collect(),
            blocked: required.into_iter().collect(),
        }
    }

    /// Captures every running bridge on the track to be replaced. Devices without a running
    /// bridge keep their document state. Returns the first bridge that could not be snapshotted.
    pub fn snapshot_selected_states(
        &mut self,
        devices: &[DeviceId],
        timeout: Duration,
    ) -> Result<DeviceSnapshots, DeviceId> {
        let live: Vec<_> = devices
            .iter()
            .copied()
            .filter(|device| {
                self.bridges.get(device).is_some_and(Bridge::is_running)
                    || self
                        .direct
                        .get(device)
                        .is_some_and(DirectDevice::can_snapshot)
            })
            .collect();
        let states = self.snapshot_states_if(timeout, |device| live.contains(&device));
        if let Some(device) = live
            .into_iter()
            .find(|device| !states.iter().any(|(captured, _)| captured == device))
        {
            return Err(device);
        }
        Ok(states)
    }

    fn snapshot_states_if(
        &mut self,
        timeout: Duration,
        include: impl Fn(DeviceId) -> bool,
    ) -> Vec<(DeviceId, Arc<[u8]>)> {
        let deadline = Instant::now() + timeout;
        let mut waiting: BTreeMap<DeviceId, u32> = self
            .bridges
            .iter_mut()
            .filter(|&(&device, _)| include(device))
            .filter_map(|(&device, b)| Some((device, b.request_state(StatePurpose::Collect)?)))
            .collect();
        let mut direct_waiting: BTreeSet<_> = self
            .direct
            .iter()
            .filter(|&(&device, direct)| include(device) && direct.can_snapshot())
            .map(|(&device, _)| device)
            .collect();
        let mut states = Vec::new();
        let mut collected: Vec<Collected> = Vec::new();
        while !waiting.is_empty() || !direct_waiting.is_empty() {
            direct_waiting.retain(|device| {
                let Some(direct) = self.direct.get_mut(device) else {
                    return false;
                };
                if let Some(state) = direct.capture() {
                    states.push((*device, state));
                    false
                } else {
                    direct.can_snapshot()
                }
            });
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
                if !bridge.is_running() || !bridge.state_request_pending(request) {
                    waiting.remove(&device);
                }
            }
            let now = Instant::now();
            if (waiting.is_empty() && direct_waiting.is_empty()) || now >= deadline {
                break;
            }
            #[cfg(unix)]
            {
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
            // Windows: a pipe cannot be waited on for "readable"; look again shortly.
            #[cfg(windows)]
            std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
        }
        states.sort_by_key(|(device, _)| *device);
        states
    }

    /// Note names of a device's plugin (empty if none or unknown).
    pub fn note_names(&self, device: DeviceId) -> &[NoteName] {
        if let Some(direct) = self.direct.get(&device) {
            return &direct.note_names;
        }
        self.bridges
            .get(&device)
            .map_or(&[][..], |b| b.note_names.as_slice())
    }

    /// Latency of a device in frames (0 if unknown); feeds `compile::CompileEnv::device_latency`.
    pub fn latency(&self, device: DeviceId) -> u32 {
        if let Some(direct) = self.direct.get(&device) {
            return direct.latency;
        }
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
        if let Some(direct) = self.direct.get(&device) {
            return direct.state_warning.as_deref();
        }
        self.bridges.get(&device)?.state_warning.as_deref()
    }

    /// Whether a device's plugin has its own GUI.
    pub fn has_gui(&self, device: DeviceId) -> bool {
        self.bridges.get(&device).is_some_and(|b| b.has_gui)
    }

    /// Whether a device's plugin has a sidechain input (#6), so its head shows the „SC“ chip:
    /// what the catalog says until the bridge loaded the plugin, then what the plugin says.
    /// `false` for devices without a bridge (missing plugins).
    pub fn has_sidechain(&self, device: DeviceId) -> bool {
        if let Some(direct) = self.direct.get(&device) {
            return direct.sidechain;
        }
        self.bridges.get(&device).is_some_and(|b| b.sidechain)
    }

    /// Opens the plugin GUI in `target`: the frame's X11 child window (Linux) or the bridge's own
    /// frame window (macOS); answered by [`PluginManagerEvent::GuiOpened`].
    pub fn open_gui(&mut self, device: DeviceId, target: GuiTarget) -> Result<(), PluginError> {
        self.bridges
            .get_mut(&device)
            .ok_or(PluginError::UnknownDevice(device))?
            .open_gui(target)
    }

    /// What the header of the bridge's own frame shows, in language `lang` (macOS).
    pub fn set_frame_header(&mut self, device: DeviceId, header: HeaderModel, lang: &str) {
        if let Some(bridge) = self.bridges.get(&device) {
            bridge.send(ControlMsg::HeaderState {
                header,
                lang: lang.to_owned(),
            });
        }
    }

    /// Orders the bridge's own frame to the front without activating it (macOS; the app became
    /// active).
    pub fn raise_frame(&mut self, device: DeviceId) {
        if let Some(bridge) = self.bridges.get(&device) {
            bridge.send(ControlMsg::RaiseFrame);
        }
    }

    /// Brings the bridge's own frame to the front (macOS; the caller yielded its activation to
    /// [`PluginManager::bridge_pid`] first).
    pub fn activate_frame(&mut self, device: DeviceId) {
        if let Some(bridge) = self.bridges.get(&device) {
            bridge.send(ControlMsg::ActivateFrame);
        }
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
        for (&id, direct) in &mut self.direct {
            direct.set_render_mode(id, mode, &mut self.pending);
        }
        for bridge in self.bridges.values_mut() {
            bridge.set_render_mode(mode, &mut self.pending);
        }
    }

    /// Sets the render mode of a subset of running bridges, leaving the default unchanged.
    pub fn set_selected_render_mode(&mut self, devices: &[DeviceId], mode: RenderMode) {
        for device in devices {
            if let Some(direct) = self.direct.get_mut(device) {
                direct.set_render_mode(*device, mode, &mut self.pending);
            }
            if let Some(bridge) = self.bridges.get_mut(device) {
                bridge.set_render_mode(mode, &mut self.pending);
            }
        }
    }

    /// Shuts all bridges down (app exit): `Shutdown` to each, up to [`SHUTDOWN_GRACE`] in total
    /// for them to exit, then SIGKILL for the rest. Cancels a running scan. Blocking.
    pub fn shutdown(&mut self) {
        self.scan = None;
        // App shutdown stops the audio stream before shutting down the plugin host.
        self.retiring_direct
            .extend(std::mem::take(&mut self.direct).into_values());
        self.retiring_direct.retain_mut(|direct| !direct.retired());
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

    /// Sets the warm-up of bridges started (or re-activated) from now on (default
    /// [`crate::WARM_UP`]); tests and measurements use zero to see the plugin's own readiness.
    pub fn set_warm_up(&mut self, warm_up: Duration) {
        self.timing.warm_up = warm_up;
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
    use std::path::Path;

    #[test]
    fn cache_dir_follows_the_platform() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| PathBuf::from(v))
            }
        };
        let home = Path::new("/Users/u");
        // An absolute path on the OS that runs the test (`is_absolute` follows the host).
        const XDG: &str = if cfg!(windows) {
            "C:/x/cache"
        } else {
            "/x/cache"
        };
        let all = env(&[("HOME", "/Users/u"), ("XDG_CACHE_HOME", XDG)]);
        assert_eq!(
            cache_dir_from(Platform::Macos, &all),
            Some(home.join("Library/Caches").join("komponist"))
        );
        assert_eq!(cache_dir_from(Platform::Macos, &env(&[])), None);
        assert_eq!(
            cache_dir_from(Platform::Linux, &all),
            Some(Path::new(XDG).join("komponist"))
        );
        let relative = env(&[("HOME", "/Users/u"), ("XDG_CACHE_HOME", "rel")]);
        assert_eq!(
            cache_dir_from(Platform::Linux, &relative),
            Some(home.join(".cache").join("komponist"))
        );
        assert_eq!(cache_dir_from(Platform::Linux, &env(&[])), None);
        // Windows: %LOCALAPPDATA%, never $HOME (an absolute path only where Windows paths are).
        assert_eq!(cache_dir_from(Platform::Windows, &all), None);
        #[cfg(windows)]
        assert_eq!(
            cache_dir_from(
                Platform::Windows,
                &env(&[("LOCALAPPDATA", r"C:\Users\u\AppData\Local")])
            ),
            Some(r"C:\Users\u\AppData\Local\komponist".into())
        );
    }

    #[test]
    fn default_config_runs_the_executable_itself_as_the_bridge() {
        let config = PluginManagerConfig::defaults();
        assert_eq!(config.bridge_exe, std::env::current_exe().unwrap());
        assert_eq!(config.bridge_args, [OsString::from("--bridge")]);
        assert!(config.cache_file.ends_with("komponist/plugins.json"));
        assert!(config.cache_file.is_absolute());
        assert!((1..=8).contains(&config.parallel_scans));
    }

    #[test]
    fn a_new_manager_lists_the_builtins_and_manages_nothing() {
        let dir = std::env::temp_dir().join(format!("kp-manager-new-{}", std::process::id()));
        let manager = PluginManager::new(PluginManagerConfig {
            bridge_exe: "/nonexistent/komponist-bridge".into(),
            bridge_args: Vec::new(),
            cache_file: dir.join("plugins.json"),
            parallel_scans: 1,
        });
        assert_eq!(
            manager.catalog().entries().len(),
            1 + komponist_builtin::Internal::ALL.len(),
            "hexa and the internal devices"
        );
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
            bridge_args: Vec::new(),
            cache_file: dir.join("plugins.json"),
            parallel_scans: 1,
        });
        // A scanned plugin whose bridge cannot start.
        manager
            .catalog
            .set_scanned(&[komponist_ipc::PluginDescriptor {
                format: komponist_ipc::PluginFormat::Clap,
                id: "x.synth".into(),
                name: "Synth".into(),
                vendor: "Vendor".into(),
                version: "1.0".into(),
                kind: komponist_ipc::PluginKind::Instrument,
                features: Vec::new(),
                bundle: "/nonexistent/Synth.clap".into(),
                sidechain: false,
            }]);
        let klopfer = Device {
            id: DeviceId(5),
            role: DeviceRole::Instrument,
            plugin: manager
                .catalog()
                .entries()
                .iter()
                .find(|e| !e.is_builtin())
                .expect("the scanned plugin")
                .plugin
                .clone(),
            bypass: false,
            state: None,
            sidechain: None,
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
