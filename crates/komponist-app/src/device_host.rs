//! Where the processors of devices come from: the seam between `EngineSync` and plugin hosting.
//!
//! [`EngineSync`](crate::engine_sync::EngineSync) asks a [`DeviceHost`] for a processor whenever a
//! device gets a slot, tells it when a device leaves, reads latencies for the compiler and status
//! for the UI, and forwards its events every frame. The app handles those events
//! (`App::show` step 1): state snapshots go into the document as coalesced undo steps (`crate::param_undo`), a latency
//! change recompiles the program, a finished scan retries the devices that were missing, crashes
//! and warnings go to the status bar.
//!
//! Two implementations:
//! - [`PluginHost`], the app's: hexa and the internal devices in process through
//!   [`BuiltinHost`], every other device (scanned CLAP and VST3 plugins) in a bridge process of the
//!   [`PluginManager`] (ARCHITECTURE §8).
//! - [`BuiltinHost`], for tests and as the fallback: hexa in process
//!   (Hexa) and the internal devices ([`Internal`], the Sampler among them); every other device gets no processor, so the engine passes an
//!   effect's input through and an instrument stays silent, and its status is
//!   [`DeviceStatus::Missing`].
//!
//! # API for the views and plugin windows
//!
//! Read through `app.engine().host()`, change through `app.engine_mut().host_mut()` or, where an
//! action exists, through [`App::dispatch`](crate::App::dispatch):
//!
//! - Device cards, insert slots, track headers (WP-44): [`DeviceHost::status`],
//!   [`DeviceHost::state_warning`] („Zustand konnte nicht geladen werden“),
//!   [`DeviceHost::late_blocks`], "Neu laden" = `PluginAction::Reload`.
//! - Generic parameter grid (WP-44): [`DeviceHost::params`], [`DeviceHost::param_value`]; knob
//!   changes go through `PluginAction::SetParam` (bridged devices: [`DeviceHost::set_param`],
//!   hexa: the engine's `SetParam`).
//! - Browser (WP-44) and S06/S07 (WP-52): [`DeviceHost::catalog`], [`DeviceHost::role`],
//!   [`DeviceHost::missing_plugins`], [`DeviceHost::is_scanning`], [`DeviceHost::last_scan`];
//!   a rescan is `SettingsAction::RescanPlugins` (or `App::scan_plugins`).
//! - Step rows (WP-54): [`DeviceHost::note_names`].
//! - Plugin windows (WP-50): [`DeviceHost::has_gui`], [`DeviceHost::open_gui`] with the frame's
//!   X11 child, [`DeviceHost::set_gui_size`] after a resize, `PluginAction::CloseWindow` (which
//!   calls [`DeviceHost::close_gui`]). The answers (`GuiOpened`, `GuiSizeRequest`, `GuiClosed`)
//!   are in `App::device_events` for the frame in which they arrived.
//!
//! Thread: UI thread.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use komponist_builtin::{BuiltinKind, BuiltinProcessor, Internal};
use komponist_engine::{DeviceMeter, Processor};
use komponist_ipc::frame::HeaderModel;
use komponist_ipc::{GuiSize, GuiTarget, NoteName, ParamInfo, ParamValue, PluginKind, RenderMode};
use komponist_model::{Device, DeviceId, DeviceRole, PluginRef, Project};
use komponist_plugins::{
    Catalog, DeviceSnapshots, DeviceStatus, FailReason, MissingPlugin, PluginDirs, PluginError,
    PluginManager, PluginManagerConfig, PluginManagerEvent, ProcessorSetup, SaveSnapshots,
    ScanReport,
};
use komponist_sampler as sampler;
use komponist_sampler::SamplerHandle;

/// Creates processors for devices and reports their runtime status.
///
/// The methods after [`DeviceHost::role`] have defaults for a host without plugin manager (no
/// parameters, no GUI, no scan).
pub trait DeviceHost {
    /// Creates the processor for `device`, already activated for `setup`, for
    /// `Command::InsertProcessor`. `None`: the device plays without a processor (passthrough for
    /// an effect, silence for an instrument); its [`DeviceHost::status`] says why.
    ///
    /// Called again for a device that still has a slot ("Neu laden", a missing plugin that a scan
    /// found): the new processor replaces the old one in the same slot.
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>>;

    /// `device` left the program; its processor (if any) is being removed from the engine.
    fn release(&mut self, device: DeviceId);

    /// Runtime status of `device` (device cards, track headers, insert slots); `Ok` for devices
    /// the host has not seen.
    fn status(&self, device: DeviceId) -> DeviceStatus;

    /// Latency of `device`'s processor in frames (`compile::CompileEnv::device_latency`).
    fn latency(&self, device: DeviceId) -> u32;

    /// Called every UI frame; returns what happened since the last call (status changes, state
    /// snapshots, latency changes, …). Never blocks.
    fn poll(&mut self) -> Vec<PluginManagerEvent>;

    /// Instrument or effect, for a plugin from the browser; `None` if unknown (not in the
    /// catalog).
    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole>;

    /// Device-specific presentation for an active authenticated bundled CLAP instance.
    fn verified_bundled_kind(&self, _device: DeviceId) -> Option<BuiltinKind> {
        None
    }

    /// The plugins the browser offers (built-ins first, then scanned plugins by name).
    fn catalog(&self) -> &Catalog;

    /// Devices of `project` whose plugin is not in the catalog (S06 dialog on open).
    fn missing_plugins(&self, project: &Project) -> Vec<MissingPlugin> {
        missing_in(self.catalog(), project)
    }

    /// Prepare a migration on opening a project when the host has enabled bundled CLAP.
    fn migrate_project_for_open(
        &self,
        _source: &Project,
        _project_dir: &Path,
    ) -> Result<Option<Project>, String> {
        Ok(None)
    }

    /// Starts a background scan of `dirs`; ends with [`PluginManagerEvent::ScanFinished`]. A
    /// running scan is restarted.
    fn start_scan(&mut self, dirs: &PluginDirs) {
        let _ = dirs;
    }

    /// Whether a scan runs (S07 "Neu scannen" shows progress).
    fn is_scanning(&self) -> bool {
        false
    }

    /// The last finished scan (S07: found, per folder, skipped).
    fn last_scan(&self) -> Option<&ScanReport> {
        None
    }

    /// Whether `device` has a sidechain input the "SC" chip in its head can set (WP-104). Internal
    /// devices answer through [`Internal::has_sidechain`](komponist_builtin::Internal::has_sidechain)
    /// without the host; a host reports it for plugins with a sidechain port (#6: the
    /// [`PluginHost`] for bridged CLAP and VST3 plugins whose catalog entry or loaded instance
    /// has one).
    fn has_sidechain(&self, device: DeviceId) -> bool {
        let _ = device;
        false
    }

    /// Calls `f` with the level meter of every in-process device that has one (internal devices,
    /// WP-89); the UI takes their values once per frame ([`DeviceMeter::take`]).
    fn for_each_meter(&self, f: &mut dyn FnMut(DeviceId, &DeviceMeter)) {
        let _ = f;
    }

    /// Whether [`DeviceHost::poll`] has work even while the UI is idle (bridges running, a scan in
    /// progress): the app then repaints at a low rate so status changes arrive.
    fn needs_polling(&self) -> bool {
        false
    }

    /// The parameters of a bridged device's plugin (generic grid); `None` for in-process and
    /// unknown devices.
    fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        let _ = device;
        None
    }

    /// Native device presentation validated against the device's current parameter inventory.
    fn native_layout(
        &self,
        _device: DeviceId,
    ) -> Option<&Arc<komponist_ipc::native_layout::NativeLayout>> {
        None
    }

    /// The last known value and text of parameter `id`. Values from the plugin's own GUI arrive
    /// without text; format them in the view.
    fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        let _ = (device, id);
        None
    }

    /// Base value of a bridged parameter, independent of live automation.
    fn param_base(&self, device: DeviceId, id: u32) -> Option<f64> {
        self.param_value(device, id).map(|v| v.value)
    }

    /// Tracks an edit that reached an active bridge through the engine slot.
    fn edited_param(&mut self, _device: DeviceId, _id: u32, _value: f64) -> bool {
        false
    }

    /// Updates the visible value of a plugin parameter during an automation override.
    fn preview_param(&mut self, _device: DeviceId, _id: u32, _value: f64) {}

    /// Update the bridge's verified snapshot expectations for current track-macro outputs.
    fn set_macro_params(&mut self, _values: &[(DeviceId, u32, f64)]) {}

    /// Whether a snapshot was caused by a track macro and contained no independent edit.
    fn is_macro_snapshot(&self, _device: DeviceId, _state: &Arc<[u8]>) -> bool {
        false
    }

    /// Sets a parameter of a bridged device; `false` if this host does not handle `device` (the
    /// caller then sends the engine's `SetParam` to the device's slot, as for hexa).
    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        let _ = (device, id, value);
        false
    }

    /// Restores `state` in the running plugin of a bridged device (undo or redo changed the state
    /// in the document); `false` if this host does not handle `device` or its bridge is not
    /// running.
    fn set_state(&mut self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        let _ = (device, state);
        false
    }

    /// Why the device's stored state could not be loaded, while that matters (device card
    /// warning „Zustand konnte nicht geladen werden“).
    fn state_warning(&self, device: DeviceId) -> Option<&str> {
        let _ = device;
        None
    }

    /// Blocks that missed their deadline since the device's bridge started (a "late" indicator).
    fn late_blocks(&self, device: DeviceId) -> u64 {
        let _ = device;
        0
    }

    /// Note names of an instrument (step sequencer rows); empty if none.
    fn note_names(&self, device: DeviceId) -> &[NoteName] {
        let _ = device;
        &[]
    }

    /// Tells the host the folder of the project file (`None`: not saved yet): relative sample
    /// paths resolve against it. Called before the devices of a project get their processors.
    fn set_project_dir(&mut self, dir: Option<PathBuf>) {
        let _ = dir;
    }

    /// The folder last given to [`DeviceHost::set_project_dir`]: audio clips resolve their relative
    /// links against it (#11).
    fn project_dir(&self) -> Option<PathBuf> {
        None
    }

    /// The UI side of a sampler device (loading samples, what the pads show); `None` for every
    /// other device.
    fn sampler(&self, device: DeviceId) -> Option<&SamplerHandle> {
        let _ = device;
        None
    }

    /// Bounded UI data from an authenticated bundled sampler.
    fn sampler_ui(&self, _device: DeviceId) -> Option<&komponist_ipc::sampler_ui::Snapshot> {
        None
    }

    /// Whether a sampler still needs to hand its decoded samples or stretch to audio.
    fn sampler_is_loading(&self, device: DeviceId) -> bool {
        use komponist_builtin::{BuiltinKind, Internal};
        if let Some(handle) = self.sampler(device) {
            return !handle.is_idle();
        }
        matches!(
            self.verified_bundled_kind(device),
            Some(BuiltinKind::Internal(
                Internal::Sampler | Internal::GeneralSampler
            ))
        ) && self
            .sampler_ui(device)
            .is_none_or(|snapshot| !snapshot.idle)
    }

    fn sampler_sync_unavailable(&self, device: DeviceId) -> bool {
        self.sampler(device).is_some_and(|handle| {
            handle.sync_status() == Some(komponist_sampler::SyncStatus::Unavailable)
        }) || self.sampler_ui(device).is_some_and(|snapshot| {
            snapshot.sync == Some(komponist_ipc::sampler_ui::Sync::Unavailable)
        })
    }

    /// Start or release a slice audition in the running bundled sampler.
    fn preview_sampler_slice(&self, _device: DeviceId, _time: Option<f64>) {}

    /// Whether the device's plugin has its own GUI.
    fn has_gui(&self, device: DeviceId) -> bool {
        let _ = device;
        false
    }

    /// Opens the plugin GUI in `target` (the frame's X11 child on Linux, the bridge's own frame
    /// window on macOS); answered by [`PluginManagerEvent::GuiOpened`].
    fn open_gui(&mut self, device: DeviceId, target: GuiTarget) -> Result<(), PluginError> {
        let _ = target;
        Err(PluginError::UnknownDevice(device))
    }

    /// What the header of the bridge's own frame shows, in language `lang` (macOS).
    fn set_frame_header(&mut self, device: DeviceId, header: HeaderModel, lang: &str) {
        let _ = (device, header, lang);
    }

    /// Brings the bridge's own frame to the front (macOS; the app yielded its activation to
    /// [`DeviceHost::bridge_pid`] first).
    fn activate_frame(&mut self, device: DeviceId) {
        let _ = device;
    }

    /// Orders the bridge's own frame to the front without activating it (macOS; the app became
    /// active).
    fn raise_frame(&mut self, device: DeviceId) {
        let _ = device;
    }

    /// The process id of the device's bridge, if it has one.
    fn bridge_pid(&self, device: DeviceId) -> Option<u32> {
        let _ = device;
        None
    }

    /// Tells the plugin GUI its new size (after the frame window resized).
    fn set_gui_size(&mut self, device: DeviceId, size: GuiSize) {
        let _ = (device, size);
    }

    /// Closes the plugin GUI; a state snapshot follows.
    fn close_gui(&mut self, device: DeviceId) {
        let _ = device;
    }

    /// Asks for a state snapshot; it arrives as [`PluginManagerEvent::StateSnapshot`].
    fn request_state(&mut self, device: DeviceId) {
        let _ = device;
    }

    /// Blocking, for saving: the current state of every bridged device, waiting at most
    /// `timeout` in total. Devices that do not answer in time keep their last snapshot.
    fn snapshot_states(&mut self, timeout: Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        let _ = timeout;
        Vec::new()
    }

    /// Whether a plugin holds a persistent edit that still needs a safe snapshot.
    fn has_pending_state_edits(&self) -> bool {
        false
    }

    /// Snapshots external devices and rejects a save that would omit a persistent plugin edit.
    fn snapshot_for_save(&mut self, timeout: Duration) -> SaveSnapshots {
        SaveSnapshots {
            captured: self.snapshot_states(timeout),
            ..Default::default()
        }
    }

    /// Snapshots bridged devices on a MIDI track before replacing its chain. Hosts with running
    /// bridges must return an error if any selected bridge lacks a usable snapshot.
    fn snapshot_selected_states(
        &mut self,
        _devices: &[DeviceId],
        _timeout: Duration,
    ) -> Result<DeviceSnapshots, DeviceId> {
        Ok(Vec::new())
    }

    /// Switches every bridge to realtime or offline processing (around an export).
    fn set_render_mode(&mut self, mode: RenderMode) {
        let _ = mode;
    }

    /// Switches only the devices needed for an isolated offline render. Hosts with bridges
    /// implement the scoped switch independently of the full-project switch.
    fn set_selected_render_mode(&mut self, _devices: &[DeviceId], _mode: RenderMode) {}

    /// Shuts every bridge down (app exit). Blocking, at most a couple of seconds.
    fn shutdown(&mut self) {}
}

/// Whether `plugin` is hexa, the one built-in instrument that is not an internal device.
pub fn is_builtin_instrument(plugin: &PluginRef) -> bool {
    BuiltinKind::from_plugin(plugin) == Some(BuiltinKind::Hexa)
}

/// Whether `device` is hexa, the in-process built-in.
pub fn is_hexa(device: &Device) -> bool {
    is_builtin_instrument(&device.plugin)
}

/// The internal device `device` is (Sampler, effects, MIDI effects), if it is one.
pub fn internal_kind(device: &Device) -> Option<Internal> {
    Internal::from_plugin(&device.plugin)
}

/// Whether `device` runs in process: hexa or an internal device.
pub fn is_in_process(device: &Device) -> bool {
    BuiltinKind::from_plugin(&device.plugin).is_some()
}

/// The pads of a sampler that hold a sample as note names for the step rows: the sample's name.
/// An empty pad reports none, so its row keeps the name the clip stored.
pub fn pad_note_names(views: &[sampler::PadView]) -> Vec<NoteName> {
    views
        .iter()
        .enumerate()
        .filter(|(_, view)| !view.name.trim().is_empty())
        .map(|(pad, view)| NoteName {
            key: sampler::note_of_pad(pad),
            channel: None,
            name: view.name.clone(),
        })
        .collect()
}

/// Devices of `project` whose plugin `catalog` does not resolve, in document order (master last).
fn missing_in(catalog: &Catalog, project: &Project) -> Vec<MissingPlugin> {
    let chains = project
        .tracks
        .iter()
        .map(|t| (Some(t.id), &t.channel.devices))
        .chain(std::iter::once((None, &project.master.devices)));
    let mut missing = Vec::new();
    for (track, devices) in chains {
        for (index, device) in devices.iter().enumerate() {
            if catalog.resolve(&device.plugin).is_none() {
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

// ------------------------------------------------------------------------------------------------
// BuiltinHost
// ------------------------------------------------------------------------------------------------

/// hexa and the internal effects in process, everything else without processor (`Missing`).
#[derive(Debug)]
pub struct BuiltinHost {
    devices: HashMap<DeviceId, Known>,
    /// The level meters of the internal devices' processors.
    meters: HashMap<DeviceId, Arc<DeviceMeter>>,
    events: Vec<PluginManagerEvent>,
    /// hexa only.
    catalog: Catalog,
    /// The folder of the project file, for relative sample paths.
    project_dir: Option<PathBuf>,
    /// The sampler devices' UI sides.
    samplers: HashMap<DeviceId, SamplerHandle>,
    /// The pad names of the samplers as note names (step rows).
    pad_names: HashMap<DeviceId, Vec<NoteName>>,
    /// Samplers still loading the samples their state links: `Loading` until the loader is idle,
    /// so an export waits for the first hits' samples.
    warming: HashSet<DeviceId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Known {
    status: DeviceStatus,
    latency: u32,
}

impl Default for BuiltinHost {
    fn default() -> BuiltinHost {
        BuiltinHost {
            devices: HashMap::new(),
            meters: HashMap::new(),
            events: Vec::new(),
            catalog: Catalog::builtin(),
            project_dir: None,
            samplers: HashMap::new(),
            pad_names: HashMap::new(),
            warming: HashSet::new(),
        }
    }
}

impl BuiltinHost {
    /// A host that has seen no device.
    pub fn new() -> BuiltinHost {
        BuiltinHost::default()
    }

    fn set(&mut self, device: DeviceId, status: DeviceStatus, latency: u32) {
        let changed = self.devices.get(&device).map(|k| &k.status) != Some(&status);
        if changed {
            self.events.push(PluginManagerEvent::StatusChanged {
                device,
                status: status.clone(),
            });
        }
        self.devices.insert(device, Known { status, latency });
    }
}

impl DeviceHost for BuiltinHost {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        let Some(kind) = BuiltinKind::from_plugin(&device.plugin) else {
            self.set(device.id, DeviceStatus::Missing, 0);
            return None;
        };
        let BuiltinProcessor {
            mut processor,
            meter,
            sampler,
        } = kind.processor_from_state(device.state.as_deref());
        if let Some(meter) = meter {
            self.meters.insert(device.id, meter);
        }
        if let Some(handle) = sampler {
            // The samples the state links load in the background; the pads play as they arrive.
            for (pad, link) in sampler::links::read(device.state.as_deref()) {
                if kind == BuiltinKind::Internal(Internal::GeneralSampler) && pad != 0 {
                    continue;
                }
                handle.load_link(pad, link, self.project_dir.clone());
            }
            if kind == BuiltinKind::Internal(Internal::Sampler) {
                self.pad_names
                    .insert(device.id, pad_note_names(&handle.views()));
            } else {
                self.pad_names.remove(&device.id);
            }
            self.samplers.insert(device.id, handle);
        }
        match processor.activate(setup.sample_rate, setup.max_block) {
            Ok(()) => {
                let loading = self
                    .samplers
                    .get(&device.id)
                    .is_some_and(|handle| !handle.is_idle());
                if loading {
                    self.warming.insert(device.id);
                }
                let status = if loading {
                    DeviceStatus::Loading
                } else {
                    DeviceStatus::Ok
                };
                self.set(device.id, status, processor.latency());
                Some(processor)
            }
            Err(e) => {
                self.set(
                    device.id,
                    DeviceStatus::Crashed(FailReason::LoadFailed(e.to_string())),
                    0,
                );
                None
            }
        }
    }

    fn release(&mut self, device: DeviceId) {
        self.devices.remove(&device);
        self.meters.remove(&device);
        self.samplers.remove(&device);
        self.pad_names.remove(&device);
        self.warming.remove(&device);
    }

    fn set_project_dir(&mut self, dir: Option<PathBuf>) {
        self.project_dir = dir;
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.project_dir.clone()
    }

    fn sampler(&self, device: DeviceId) -> Option<&SamplerHandle> {
        self.samplers.get(&device)
    }

    fn note_names(&self, device: DeviceId) -> &[NoteName] {
        self.pad_names.get(&device).map_or(&[], Vec::as_slice)
    }

    fn for_each_meter(&self, f: &mut dyn FnMut(DeviceId, &DeviceMeter)) {
        for (id, meter) in &self.meters {
            f(*id, meter);
        }
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        let status = self
            .devices
            .get(&device)
            .map_or(DeviceStatus::Ok, |k| k.status.clone());
        if status == DeviceStatus::Ok
            && self
                .samplers
                .get(&device)
                .is_some_and(|handle| !handle.is_idle())
        {
            DeviceStatus::Loading
        } else {
            status
        }
    }

    fn latency(&self, device: DeviceId) -> u32 {
        self.devices.get(&device).map_or(0, |k| k.latency)
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        // A parameter can change an internal device's latency (limiter lookahead): its processor
        // publishes the new value in its meter (RT-safe), and the same event a bridged plugin's
        // latency change raises makes the app recompile the delay compensation.
        for (device, meter) in &self.meters {
            let latency = meter.latency();
            if let Some(known) = self.devices.get_mut(device)
                && known.latency != latency
            {
                known.latency = latency;
                self.events.push(PluginManagerEvent::LatencyChanged {
                    device: *device,
                    latency,
                });
            }
        }
        let ready: Vec<DeviceId> = self
            .warming
            .iter()
            .copied()
            .filter(|d| self.samplers.get(d).is_none_or(SamplerHandle::is_idle))
            .collect();
        for device in ready {
            self.warming.remove(&device);
            let latency = self.devices.get(&device).map_or(0, |k| k.latency);
            self.set(device, DeviceStatus::Ok, latency);
        }
        for (device, handle) in &self.samplers {
            if let Some(previous) = self.pad_names.get_mut(device) {
                let names = pad_note_names(&handle.views());
                if *previous != names {
                    *previous = names;
                }
            }
        }
        std::mem::take(&mut self.events)
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        // hexa and the internal devices; everything else needs the plugin manager's catalog.
        if is_builtin_instrument(plugin) {
            Some(DeviceRole::Instrument)
        } else {
            Internal::from_plugin(plugin).map(Internal::role)
        }
    }

    fn catalog(&self) -> &Catalog {
        &self.catalog
    }
}

// ------------------------------------------------------------------------------------------------
// PluginHost
// ------------------------------------------------------------------------------------------------

/// The app's device host: hexa in process through [`BuiltinHost`], every other device
/// (klopfer, CLAP, VST3) in a bridge process of the [`PluginManager`].
///
/// A device the manager cannot resolve plays without processor and stays `Missing` with its
/// state; after a scan the app asks again ([`DeviceHost::create_processor`] for a device that
/// still has a slot). Dropping the host kills every bridge (the manager's drop);
/// [`DeviceHost::shutdown`] is the graceful way at app exit.
#[derive(Debug)]
pub struct PluginHost {
    manager: PluginManager,
    builtin: BuiltinHost,
    /// Devices handed to the manager and not released since.
    external: HashSet<DeviceId>,
}

impl PluginHost {
    /// A host on a new [`PluginManager`] for `config` (catalog from the scan cache; starts no
    /// process).
    pub fn new(config: PluginManagerConfig) -> PluginHost {
        PluginHost {
            manager: PluginManager::new(config),
            builtin: BuiltinHost::new(),
            external: HashSet::new(),
        }
    }

    /// The plugin manager (diagnostics and tests; processors come only through
    /// [`DeviceHost::create_processor`]).
    pub fn manager(&self) -> &PluginManager {
        &self.manager
    }

    /// Whether `device` is handled by the manager (a bridge, or `Missing` / `Crashed` without
    /// one).
    fn is_external(&self, device: DeviceId) -> bool {
        self.external.contains(&device)
    }
}

impl DeviceHost for PluginHost {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        if is_in_process(device) {
            return self.builtin.create_processor(device, setup);
        }
        // A device seen before ("Neu laden", found by a scan) starts a fresh bridge with its last
        // known state; the manager kills an old bridge that is still alive.
        let created = if self.external.insert(device.id) {
            self.manager.create_processor(device, setup)
        } else {
            self.manager.reload(device, setup)
        };
        // The manager records why it failed: `Missing`, or `Crashed(LoadFailed)`.
        created.ok()
    }

    fn release(&mut self, device: DeviceId) {
        if self.external.remove(&device) {
            self.manager.release(device);
        } else {
            self.builtin.release(device);
        }
    }

    fn for_each_meter(&self, f: &mut dyn FnMut(DeviceId, &DeviceMeter)) {
        self.builtin.for_each_meter(f);
        self.manager.for_each_effect_meter(f);
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        if self.is_external(device) {
            self.manager.status(device)
        } else {
            self.builtin.status(device)
        }
    }

    fn latency(&self, device: DeviceId) -> u32 {
        if self.is_external(device) {
            self.manager.latency(device)
        } else {
            self.builtin.latency(device)
        }
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        let mut events = self.builtin.poll();
        events.extend(self.manager.poll());
        events
    }

    fn migrate_project_for_open(
        &self,
        source: &Project,
        project_dir: &Path,
    ) -> Result<Option<Project>, String> {
        self.manager.migrate_project_for_open(source, project_dir)
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        match self.manager.catalog().resolve(plugin)?.kind {
            PluginKind::Instrument => Some(DeviceRole::Instrument),
            PluginKind::Effect => Some(DeviceRole::Effect),
            PluginKind::MidiEffect => Some(DeviceRole::MidiEffect),
            PluginKind::Other => None,
        }
    }

    fn verified_bundled_kind(&self, device: DeviceId) -> Option<BuiltinKind> {
        self.manager.verified_bundled_kind(device)
    }

    fn catalog(&self) -> &Catalog {
        self.manager.catalog()
    }

    fn missing_plugins(&self, project: &Project) -> Vec<MissingPlugin> {
        self.manager.missing_plugins(project)
    }

    fn start_scan(&mut self, dirs: &PluginDirs) {
        self.manager.start_scan(dirs);
    }

    fn is_scanning(&self) -> bool {
        self.manager.is_scanning()
    }

    fn last_scan(&self) -> Option<&ScanReport> {
        self.manager.last_scan()
    }

    fn needs_polling(&self) -> bool {
        !self.external.is_empty()
            || self.manager.is_scanning()
            || self.manager.has_retiring_direct()
    }

    fn params(&self, device: DeviceId) -> Option<&[ParamInfo]> {
        self.manager.params(device)
    }

    fn native_layout(
        &self,
        device: DeviceId,
    ) -> Option<&Arc<komponist_ipc::native_layout::NativeLayout>> {
        self.manager.native_layout(device)
    }

    fn param_value(&self, device: DeviceId, id: u32) -> Option<&ParamValue> {
        self.manager.param_value(device, id)
    }

    fn param_base(&self, device: DeviceId, id: u32) -> Option<f64> {
        self.manager.param_base(device, id)
    }

    fn edited_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        self.manager.edited_param(device, id, value)
    }

    fn preview_param(&mut self, device: DeviceId, id: u32, value: f64) {
        self.manager.preview_param(device, id, value);
    }

    fn set_macro_params(&mut self, values: &[(DeviceId, u32, f64)]) {
        self.manager.set_macro_params(values);
    }

    fn is_macro_snapshot(&self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        self.manager.is_macro_snapshot(device, state)
    }

    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        if !self.is_external(device) {
            return false;
        }
        self.manager.set_param(device, id, value);
        true
    }

    fn set_state(&mut self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        self.is_external(device) && self.manager.set_state(device, state)
    }

    fn state_warning(&self, device: DeviceId) -> Option<&str> {
        self.manager.state_warning(device)
    }

    fn late_blocks(&self, device: DeviceId) -> u64 {
        self.manager.late_blocks(device)
    }

    fn note_names(&self, device: DeviceId) -> &[NoteName] {
        if self.is_external(device) {
            self.manager.note_names(device)
        } else {
            self.builtin.note_names(device)
        }
    }

    fn set_project_dir(&mut self, dir: Option<PathBuf>) {
        self.manager.set_project_dir(dir.clone());
        self.builtin.set_project_dir(dir);
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.builtin.project_dir()
    }

    fn sampler_ui(&self, device: DeviceId) -> Option<&komponist_ipc::sampler_ui::Snapshot> {
        self.manager.sampler_ui(device)
    }

    fn preview_sampler_slice(&self, device: DeviceId, time: Option<f64>) {
        self.manager.preview_sampler_slice(device, time);
    }

    fn sampler(&self, device: DeviceId) -> Option<&SamplerHandle> {
        self.builtin.sampler(device)
    }

    fn has_gui(&self, device: DeviceId) -> bool {
        self.manager.has_gui(device)
    }

    fn has_sidechain(&self, device: DeviceId) -> bool {
        // Internal devices answer from their spec before the host is asked (`Card`); the
        // manager knows the bridged ones (#6).
        self.is_external(device) && self.manager.has_sidechain(device)
    }

    fn open_gui(&mut self, device: DeviceId, target: GuiTarget) -> Result<(), PluginError> {
        self.manager.open_gui(device, target)
    }

    fn set_frame_header(&mut self, device: DeviceId, header: HeaderModel, lang: &str) {
        self.manager.set_frame_header(device, header, lang);
    }

    fn activate_frame(&mut self, device: DeviceId) {
        self.manager.activate_frame(device);
    }

    fn raise_frame(&mut self, device: DeviceId) {
        self.manager.raise_frame(device);
    }

    fn bridge_pid(&self, device: DeviceId) -> Option<u32> {
        self.manager.bridge_pid(device)
    }

    fn set_gui_size(&mut self, device: DeviceId, size: GuiSize) {
        self.manager.set_gui_size(device, size);
    }

    fn close_gui(&mut self, device: DeviceId) {
        self.manager.close_gui(device);
    }

    fn request_state(&mut self, device: DeviceId) {
        self.manager.request_state(device);
    }

    fn snapshot_states(&mut self, timeout: Duration) -> Vec<(DeviceId, Arc<[u8]>)> {
        self.manager.snapshot_states(timeout)
    }

    fn has_pending_state_edits(&self) -> bool {
        self.manager.has_pending_state_edits()
    }

    fn snapshot_for_save(&mut self, timeout: Duration) -> SaveSnapshots {
        self.manager.snapshot_for_save(timeout)
    }

    fn snapshot_selected_states(
        &mut self,
        devices: &[DeviceId],
        timeout: Duration,
    ) -> Result<DeviceSnapshots, DeviceId> {
        self.manager.snapshot_selected_states(devices, timeout)
    }

    fn set_render_mode(&mut self, mode: RenderMode) {
        self.manager.set_render_mode(mode);
    }

    fn set_selected_render_mode(&mut self, devices: &[DeviceId], mode: RenderMode) {
        self.manager.set_selected_render_mode(devices, mode);
    }

    fn shutdown(&mut self) {
        self.manager.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    const SETUP: ProcessorSetup = ProcessorSetup {
        sample_rate: 48_000.0,
        max_block: komponist_engine::MAX_BLOCK,
    };

    #[test]
    fn hexa_plays_and_everything_else_is_missing() {
        let project = demo_project();
        let mut host = BuiltinHost::new();
        let mut made = Vec::new();
        for track in &project.tracks {
            for device in &track.channel.devices {
                let p = host.create_processor(device, SETUP);
                made.push((
                    device.plugin.name.clone(),
                    p.is_some(),
                    host.status(device.id),
                ));
            }
        }
        for (name, has_processor, status) in &made {
            if name == "Hexa" || name == komponist_sampler::NAME {
                assert!(has_processor);
                assert_eq!(*status, DeviceStatus::Ok);
            } else {
                assert!(!has_processor, "{name}");
                assert_eq!(*status, DeviceStatus::Missing, "{name}");
            }
        }
        let events = host.poll();
        assert_eq!(events.len(), made.len(), "one status event per device");
        assert!(host.poll().is_empty());
        // Unknown devices are Ok, released devices forgotten.
        let lead = &project.tracks[2].channel.devices[0];
        host.release(lead.id);
        assert_eq!(host.status(lead.id), DeviceStatus::Ok);
        assert_eq!(host.latency(lead.id), 0);
    }

    #[test]
    fn the_builtin_host_offers_hexa_and_reports_the_rest_missing() {
        let project = demo_project();
        let mut host = BuiltinHost::new();
        let names: Vec<&str> = host
            .catalog()
            .entries()
            .iter()
            .map(|e| e.plugin.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "Hexa",
                "Klopfer",
                "EQ",
                "Delay",
                "Hall",
                "Limiter",
                "Presse",
                "Arpeggiator",
                "Tonleiter",
                "Volt",
                "Sampler",
            ]
        );
        let missing = host.missing_plugins(&project);
        let in_process = project
            .tracks
            .iter()
            .flat_map(|t| &t.channel.devices)
            .filter(|d| is_in_process(d))
            .count();
        let all = project
            .tracks
            .iter()
            .map(|t| t.channel.devices.len())
            .sum::<usize>();
        assert_eq!(missing.len(), all - in_process);
        assert!(missing.iter().all(|m| m.plugin.name != "Hexa"));
        // Drums: the Sampler (in process) is at index 0, so the compressor behind it is the
        // first missing device.
        assert_eq!(missing[0].track, Some(project.tracks[0].id));
        assert_eq!(missing[0].index, 1);
        assert_eq!(missing[0].role, DeviceRole::Effect);
        // No plugin manager: no parameters, no GUI, no scan.
        let device = project.tracks[0].channel.devices[0].id;
        assert!(host.params(device).is_none());
        assert!(!host.set_param(device, 0, 0.5));
        assert!(!host.has_gui(device));
        assert!(
            host.open_gui(device, GuiTarget::X11Child { xid: 1, scale: 1.0 })
                .is_err()
        );
        host.start_scan(&PluginDirs::defaults());
        assert!(!host.is_scanning() && !host.needs_polling());
        assert!(host.snapshot_states(Duration::ZERO).is_empty());
    }
}

#[cfg(test)]
mod sampler_tests {
    use super::*;
    use komponist_model::DeviceRole;
    use komponist_sampler::{SampleLink, links, testkit};

    const SETUP: ProcessorSetup = ProcessorSetup {
        sample_rate: 48_000.0,
        max_block: komponist_engine::MAX_BLOCK,
    };

    fn sampler_device(state: Option<Vec<u8>>) -> Device {
        Device {
            id: DeviceId(7),
            role: DeviceRole::Instrument,
            plugin: Internal::Sampler.plugin_ref(),
            bypass: false,
            state: state.map(Arc::from),
            sidechain: None,
        }
    }

    #[test]
    fn the_demo_names_the_sampler_like_the_builtin() {
        let drums = &komponist_model::fixtures::demo_project().tracks[0]
            .channel
            .devices[0];
        let own = Internal::Sampler.plugin_ref();
        assert_eq!(
            (&drums.plugin.format, &drums.plugin.id),
            (&own.format, &own.id)
        );
        assert_eq!(drums.plugin.name, sampler::NAME);
    }

    #[test]
    fn a_sampler_is_loading_until_its_samples_are_in_and_reports_its_pad_names() {
        let dir = std::env::temp_dir().join(format!("kp-host-sampler-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let kit = testkit::write_kit(&dir).unwrap();
        let mut state = None;
        for (pad, path) in kit.iter().take(2).enumerate() {
            state = Some(links::write(
                state.as_deref(),
                pad,
                Some(&SampleLink::new(path, None)),
            ));
        }
        let device = sampler_device(state);
        let mut host = BuiltinHost::new();
        host.set_project_dir(Some(dir.clone()));
        assert!(host.create_processor(&device, SETUP).is_some());
        let handle = host.sampler(device.id).expect("a sampler").clone();
        assert!(handle.wait_idle(std::time::Duration::from_secs(5)));
        // Once the loader is idle the next poll says Ok.
        host.poll();
        assert_eq!(host.status(device.id), DeviceStatus::Ok);
        // The pads that hold a sample name their notes; the empty ones report none.
        let names: Vec<(u8, &str)> = host
            .note_names(device.id)
            .iter()
            .map(|n| (n.key, n.name.as_str()))
            .collect();
        assert_eq!(names, [(36, "Kick"), (37, "Snare")]);
        host.release(device.id);
        assert!(host.sampler(device.id).is_none());
        assert!(host.note_names(device.id).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
