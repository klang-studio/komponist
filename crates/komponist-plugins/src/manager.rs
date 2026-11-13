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
//! Thread: UI thread only, non-blocking except where noted.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use komponist_engine::Processor;
use komponist_ipc::{GuiSize, LogLevel, NoteName, ParamInfo, ParamValue, RenderMode};
use komponist_model::{Device, DeviceId, DeviceRole, PluginRef, Project, TrackId};

use crate::catalog::{Catalog, PluginDirs, ScanReport};

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
    /// Load, activation or state restore failed; the bridge's message.
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

/// How a processor is prepared.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessorSetup {
    /// The engine's sample rate in Hz.
    pub sample_rate: f32,
    /// The engine's `max_block`.
    pub max_block: usize,
}

/// Owner of all bridges and the catalog. UI thread.
#[derive(Debug)]
pub struct PluginManager {
    _private: (),
}

impl PluginManager {
    /// Loads the scan cache into the catalog (built-ins plus cached plugins). Starts no process.
    pub fn new(config: PluginManagerConfig) -> PluginManager {
        todo!("WP-45")
    }

    /// The catalog.
    pub fn catalog(&self) -> &Catalog {
        todo!("WP-45")
    }

    /// Starts a background scan of `dirs` (bundles unchanged since the cache are not rescanned);
    /// ends with [`PluginManagerEvent::ScanFinished`]. A running scan is restarted.
    pub fn start_scan(&mut self, dirs: &PluginDirs) {
        todo!("WP-45")
    }

    /// Whether a scan runs.
    pub fn is_scanning(&self) -> bool {
        todo!("WP-45")
    }

    /// The last finished scan.
    pub fn last_scan(&self) -> Option<&ScanReport> {
        todo!("WP-45")
    }

    /// Devices of `project` whose plugin is not in the catalog (S06 dialog on open).
    pub fn missing_plugins(&self, project: &Project) -> Vec<MissingPlugin> {
        todo!("WP-45")
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
        todo!("WP-45")
    }

    /// "Neu laden": kills the old bridge if still alive and starts a new one with the last known
    /// state (the latest snapshot, else `device.state`). The returned processor replaces the old one
    /// in the same slot (`InsertProcessor`).
    pub fn reload(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Result<Box<dyn Processor>, PluginError> {
        todo!("WP-45")
    }

    /// The device left the project: shuts its bridge down (after the engine removed the processor;
    /// the processor tolerates a dead bridge either way).
    pub fn release(&mut self, device: DeviceId) {
        todo!("WP-45")
    }

    /// Status of a device; `Ok` for devices this manager does not manage (in-process built-ins).
    pub fn status(&self, device: DeviceId) -> DeviceStatus {
        todo!("WP-45")
    }

    /// Call every UI frame: reads control messages and plugin output events, checks pidfds and scan
    /// workers, and returns what happened. Never blocks.
    pub fn poll(&mut self) -> Vec<PluginManagerEvent> {
        todo!("WP-45")
    }

    /// The parameter list of a device's plugin (generic parameter grid).
    pub fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        todo!("WP-45")
    }

    /// The last known value and text of parameter `id`.
    pub fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        todo!("WP-45")
    }

    /// Sets a parameter from the generic view or a device-card knob (sent over the control socket;
    /// the bridge delivers it to the plugin's audio side too).
    pub fn set_param(&mut self, device: DeviceId, id: u32, value: f64) {
        todo!("WP-45")
    }

    /// Asks for a state snapshot; it arrives as [`PluginManagerEvent::StateSnapshot`].
    pub fn request_state(&mut self, device: DeviceId) {
        todo!("WP-45")
    }

    /// Blocking, for saving: snapshots every running external device, waiting at most `timeout` in
    /// total. Devices that do not answer in time are left out (their last snapshot stays in the
    /// document).
    pub fn snapshot_states(&mut self, timeout: Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        todo!("WP-45")
    }

    /// Note names of a device's plugin (empty if none or unknown).
    pub fn note_names(&self, device: DeviceId) -> &[NoteName] {
        todo!("WP-45")
    }

    /// Latency of a device in frames (0 if unknown); feeds `compile::CompileEnv::device_latency`.
    pub fn latency(&self, device: DeviceId) -> u32 {
        todo!("WP-45")
    }

    /// Whether a device's plugin has its own GUI.
    pub fn has_gui(&self, device: DeviceId) -> bool {
        todo!("WP-45")
    }

    /// Opens the plugin GUI inside the X11 window `parent_xid` (the frame's child window) at UI
    /// `scale`; answered by [`PluginManagerEvent::GuiOpened`].
    pub fn open_gui(
        &mut self,
        device: DeviceId,
        parent_xid: u64,
        scale: f64,
    ) -> Result<(), PluginError> {
        todo!("WP-45")
    }

    /// Tells the plugin GUI its new size (after the frame resized).
    pub fn set_gui_size(&mut self, device: DeviceId, size: GuiSize) {
        todo!("WP-45")
    }

    /// Closes the plugin GUI (frame closed) and requests a state snapshot.
    pub fn close_gui(&mut self, device: DeviceId) {
        todo!("WP-45")
    }

    /// Switches every bridge to realtime or offline processing (around an export).
    pub fn set_render_mode(&mut self, mode: RenderMode) {
        todo!("WP-45")
    }

    /// Shuts all bridges down (app exit).
    pub fn shutdown(&mut self) {
        todo!("WP-45")
    }
}
