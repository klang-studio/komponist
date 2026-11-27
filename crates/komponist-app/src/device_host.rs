//! Where the processors of devices come from: the seam between `EngineSync` and plugin hosting.
//!
//! [`EngineSync`](crate::engine_sync::EngineSync) asks a [`DeviceHost`] for a processor whenever a
//! device gets a slot, tells it when a device leaves, reads latencies for the compiler and status
//! for the UI, and forwards its events every frame. The app handles those events
//! (`App::show` step 1): state snapshots go into the document as coalesced undo steps (`crate::param_undo`), a latency
//! change recompiles the program, a finished scan retries the devices that were missing, crashes
//! and warnings go to the status bar.
//!
//! Verified packaged CLAPs run directly and external plugins run in the bridge.
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
//!   changes go through `PluginAction::SetParam`.
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

use std::collections::{BTreeMap, HashSet};
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use komponist_engine::Processor;
use komponist_ipc::clap_presets::{ClapPreset, ClapPresetSource};
use komponist_ipc::device_assets::{
    AssetKind, AssetStatus, Bindings, OfflineRequest, OfflineStatus, Snapshot,
};
use komponist_ipc::frame::HeaderModel;
use komponist_ipc::{GuiSize, GuiTarget, NoteName, ParamInfo, ParamValue, PluginKind, RenderMode};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::project::SavedPluginState;
use komponist_model::{Device, DeviceId, DeviceRole, PluginRef, Project};
use komponist_plugins::{
    AssetEdit, AssetEditPreparation, Catalog, DeviceSnapshots, DeviceStatus, MissingPlugin,
    PluginDirs, PluginError, PluginManager, PluginManagerConfig, PluginManagerEvent,
    ProcessorSetup, SaveSnapshots, ScanReport,
};

/// The index and modified flag of the preset recorded through CLAP preset-load.
pub fn selected_preset(device: &Device, presets: &[ClapPreset]) -> Option<(usize, bool)> {
    komponist_ipc::clap_presets::selected_preset(device.preset.as_ref(), presets)
}

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

    /// Test probe: the CLAP ID of a running instance from the manifest-verified package.
    #[doc(hidden)]
    fn verified_bundle(&self, _device: DeviceId) -> Option<&str> {
        None
    }

    /// The plugins the browser offers, sorted by name.
    fn catalog(&self) -> &Catalog;

    /// Refresh the latest marker snapshot for this slot.
    fn request_waveform_markers(&mut self, _device: DeviceId, _slot: u32) {}

    /// Latest marker snapshot from the live device, when available.
    fn waveform_markers(
        &self,
        _device: DeviceId,
        _slot: u32,
    ) -> Option<&komponist_ipc::waveform_markers::Snapshot> {
        None
    }

    /// Queue an edit against a specific live marker snapshot.
    fn edit_waveform_markers(
        &mut self,
        _device: DeviceId,
        _slot: u32,
        _request: komponist_ipc::waveform_markers::Request,
    ) -> Result<(), komponist_ipc::waveform_markers::EditStatus> {
        Err(komponist_ipc::waveform_markers::EditStatus::Unavailable)
    }

    /// Presets discovered for a live CLAP instance.
    fn clap_presets(&self, _device: &Device) -> &[ClapPreset] {
        &[]
    }

    /// Requests a discovered preset by its opaque source on the live instance.
    fn load_clap_preset(&mut self, _device: &Device, _source: &ClapPresetSource) -> bool {
        false
    }

    /// Whether this running device has CLAP user preset operations.
    fn supports_user_presets(&self, _device: DeviceId) -> bool {
        false
    }

    /// Requests the current preset state, completed by a device event.
    fn capture_clap_preset(&mut self, _device: DeviceId) -> bool {
        false
    }

    /// Applies the stored preset format to a running device.
    fn load_user_preset(&mut self, _device: DeviceId, _state: Vec<u8>) -> bool {
        false
    }

    /// Prepares the initial project state of a device to be inserted from the browser.
    fn prepare_clap_preset(
        &self,
        _plugin: &PluginRef,
        _source: &ClapPresetSource,
    ) -> Option<komponist_plugins::PresetPreparation> {
        None
    }

    /// Whether a scan failed to probe the bundle of a device that is still running.
    fn device_probe_failed(&self, _device: DeviceId) -> bool {
        false
    }

    /// Devices of `project` whose plugin is not in the catalog (S06 dialog on open).
    fn missing_plugins(&self, project: &Project) -> Vec<MissingPlugin> {
        missing_in(self.catalog(), project)
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

    /// Whether the plugin exposes a sidechain input.
    fn has_sidechain(&self, _device: DeviceId) -> bool {
        false
    }

    /// Whether [`DeviceHost::poll`] has work even while the UI is idle (bridges running, a scan in
    /// progress): the app then repaints at a low rate so status changes arrive.
    fn needs_polling(&self) -> bool {
        false
    }

    /// The parameters of the live plugin.
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

    fn device_display(
        &self,
        _device: DeviceId,
    ) -> Option<&Arc<komponist_ipc::device_canvas::DisplayList>> {
        None
    }

    /// Named telemetry published by the live plugin.
    fn signal_tap(
        &self,
        _device: DeviceId,
        _key: &str,
    ) -> Option<&komponist_plugins::signal_taps::SignalTap> {
        None
    }

    /// Queue a text conversion. `true` accepts the request for asynchronous completion through
    /// `ParamTextParsed`; `false` means the caller can apply its numeric fallback immediately.
    fn request_param_text(
        &mut self,
        _device: DeviceId,
        _request: u64,
        _id: u32,
        _text: String,
    ) -> bool {
        false
    }

    /// The last known value and cached text. The view formats live values without cached text.
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

    /// Records a durable render-path edit and returns its delivery identity.
    fn edit_param_through_render(
        &mut self,
        device: DeviceId,
        id: u32,
        value: f64,
    ) -> Option<NonZeroU32> {
        let _ = (device, id, value);
        None
    }

    /// Updates the visible value of a plugin parameter during an automation override.
    fn preview_param(&mut self, _device: DeviceId, _id: u32, _value: f64) {}

    /// Update the bridge's verified snapshot expectations for current track-macro outputs.
    fn set_macro_params(&mut self, _values: &[(DeviceId, u32, f64)]) {}

    /// Publish the runtime parameter IDs whose live values can come from automation.
    fn set_automated_params(&mut self, _values: &[(DeviceId, u32, Option<f64>)]) {}

    /// Whether a snapshot was caused by a track macro and contained no independent edit.
    fn is_macro_snapshot(&self, _device: DeviceId, _state: &Arc<[u8]>) -> bool {
        false
    }

    /// Whether the host captured this state and has no later edits awaiting capture.
    fn is_current_param_snapshot(&self, _device: DeviceId, _state: &Arc<[u8]>) -> bool {
        false
    }

    /// Sets a parameter of a bridged device; `false` if this host does not handle `device` (the
    /// caller then sends the engine's `SetParam` to the device's slot, as for hexa).
    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        let _ = (device, id, value);
        false
    }

    /// Restores a plugin snapshot and its host-owned parameter bases.
    fn restore_saved_state(&mut self, device: DeviceId, state: &SavedPluginState) -> bool {
        self.set_state(device, &state.bytes)
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

    /// Whether the file inventory or an asset is still being prepared for rendering.
    fn render_assets_pending(&self, device: &Device) -> bool {
        self.asset_inventory_pending(device.id)
            || self.render_asset_bindings_pending(device.id)
            || self.asset_slots(device.id).is_some_and(|inventory| {
                inventory.slots.iter().any(|slot| {
                    slot.status == AssetStatus::Loading
                        || slot.kind == AssetKind::AudioSample
                            && slot.status == AssetStatus::Empty
                            && device.asset_links.contains_key(&slot.id)
                })
            })
    }

    /// Audition a position on a live audio asset, or release its active preview.
    fn preview_waveform(&self, _device: DeviceId, _slot: u32, _time: Option<f64>) {}

    /// File slots offered by the active plugin instance.
    fn asset_slots(&self, _device: DeviceId) -> Option<&Snapshot> {
        None
    }

    fn supports_asset_edit_state(&self, _device: DeviceId) -> bool {
        false
    }

    fn prepare_asset_edit(
        &mut self,
        _device: &Device,
        _changes: &[AssetEdit],
        _request: u64,
    ) -> Result<AssetEditPreparation, DeviceId> {
        Ok(AssetEditPreparation::Ready(None))
    }

    fn cancel_asset_edit(&mut self, _device: DeviceId) {}

    /// Queue an update containing resolved runtime paths.
    fn set_asset_bindings(&mut self, _device: DeviceId, _bindings: &Bindings) -> bool {
        false
    }

    fn sync_asset_links(
        &mut self,
        _device: DeviceId,
        _before: &BTreeMap<u32, AssetLink>,
        _after: &BTreeMap<u32, AssetLink>,
    ) {
    }

    fn asset_inventory_pending(&self, _device: DeviceId) -> bool {
        false
    }

    fn render_asset_bindings_pending(&self, _device: DeviceId) -> bool {
        false
    }

    fn supports_offline_preparation(&self, _device: DeviceId) -> bool {
        false
    }

    fn set_offline_preparation(&mut self, _device: DeviceId, _request: Option<OfflineRequest>) {}

    fn offline_preparation_status(&self, _device: DeviceId) -> Option<OfflineStatus> {
        None
    }

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
    fn snapshot_states(&mut self, timeout: Duration) -> DeviceSnapshots {
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

    /// Collects states of selected running devices and reports those still pending at the deadline.
    fn snapshot_available_states(
        &mut self,
        _devices: &[DeviceId],
        _timeout: Duration,
    ) -> SaveSnapshots {
        SaveSnapshots::default()
    }

    /// Ensures the deferred retirement can capture state in a later poll.
    fn schedule_retiring_snapshot(&mut self, _device: DeviceId) {}

    /// Whether the live instance still owes a capture or state commit before retirement.
    fn retiring_snapshot_pending(&self, _device: DeviceId) -> bool {
        false
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

/// The device host for packaged and external plugins.
pub struct PluginHost {
    manager: PluginManager,
    project_dir: Option<PathBuf>,
    /// Devices handed to the manager and not released since.
    external: HashSet<DeviceId>,
}

impl PluginHost {
    /// A host on a new [`PluginManager`] for `config` (catalog from the scan cache; starts no
    /// process).
    pub fn new(config: PluginManagerConfig) -> PluginHost {
        PluginHost {
            manager: PluginManager::new(config),
            project_dir: None,
            external: HashSet::new(),
        }
    }

    pub(crate) fn without_bundled_clap(config: PluginManagerConfig) -> Self {
        Self {
            manager: PluginManager::without_bundled_clap(config),
            project_dir: None,
            external: HashSet::new(),
        }
    }

    pub(crate) fn for_test_inventory(
        config: PluginManagerConfig,
        inventory: Arc<komponist_plugins::bundled_clap::BundledClap>,
    ) -> Self {
        Self {
            manager: PluginManager::for_test_inventory(config, inventory),
            project_dir: None,
            external: HashSet::new(),
        }
    }

    /// The plugin manager (diagnostics and tests; processors come only through
    /// [`DeviceHost::create_processor`]).
    pub fn manager(&self) -> &PluginManager {
        &self.manager
    }

    pub fn start_bundled_scan(&mut self) {
        self.manager.start_bundled_scan();
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
        }
    }

    fn status(&self, device: DeviceId) -> DeviceStatus {
        self.manager.status(device)
    }

    fn latency(&self, device: DeviceId) -> u32 {
        self.manager.latency(device)
    }

    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.manager.poll()
    }

    fn role(&self, plugin: &PluginRef) -> Option<DeviceRole> {
        match self.manager.catalog().resolve(plugin)?.kind {
            PluginKind::Instrument => Some(DeviceRole::Instrument),
            PluginKind::Effect => Some(DeviceRole::Effect),
            PluginKind::MidiEffect => Some(DeviceRole::MidiEffect),
            PluginKind::Other => None,
        }
    }

    fn verified_bundle(&self, device: DeviceId) -> Option<&str> {
        self.manager.verified_bundle(device)
    }

    fn catalog(&self) -> &Catalog {
        self.manager.catalog()
    }

    fn device_probe_failed(&self, device: DeviceId) -> bool {
        self.manager.device_probe_failed(device)
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

    fn device_display(
        &self,
        device: DeviceId,
    ) -> Option<&Arc<komponist_ipc::device_canvas::DisplayList>> {
        self.manager.device_display(device)
    }

    fn signal_tap(
        &self,
        device: DeviceId,
        key: &str,
    ) -> Option<&komponist_plugins::signal_taps::SignalTap> {
        self.manager.signal_tap(device, key)
    }

    fn request_param_text(
        &mut self,
        device: DeviceId,
        request: u64,
        id: u32,
        text: String,
    ) -> bool {
        self.is_external(device) && self.manager.request_param_text(device, request, id, text)
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

    fn edit_param_through_render(
        &mut self,
        device: DeviceId,
        id: u32,
        value: f64,
    ) -> Option<NonZeroU32> {
        self.manager.edit_param_through_render(device, id, value)
    }

    fn preview_param(&mut self, device: DeviceId, id: u32, value: f64) {
        self.manager.preview_param(device, id, value);
    }

    fn set_macro_params(&mut self, values: &[(DeviceId, u32, f64)]) {
        self.manager.set_macro_params(values);
    }

    fn set_automated_params(&mut self, values: &[(DeviceId, u32, Option<f64>)]) {
        self.manager.set_automated_params(values);
    }

    fn is_macro_snapshot(&self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        self.manager.is_macro_snapshot(device, state)
    }

    fn is_current_param_snapshot(&self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        self.manager.is_current_param_snapshot(device, state)
    }

    fn set_param(&mut self, device: DeviceId, id: u32, value: f64) -> bool {
        if !self.is_external(device) {
            return false;
        }
        self.manager.set_param(device, id, value);
        true
    }

    fn restore_saved_state(&mut self, device: DeviceId, state: &SavedPluginState) -> bool {
        self.is_external(device) && self.manager.restore_saved_state(device, state)
    }

    fn set_state(&mut self, device: DeviceId, state: &Arc<[u8]>) -> bool {
        self.is_external(device) && self.manager.set_state(device, state)
    }

    fn request_waveform_markers(&mut self, device: DeviceId, slot: u32) {
        self.manager.request_waveform_markers(device, slot);
    }

    fn waveform_markers(
        &self,
        device: DeviceId,
        slot: u32,
    ) -> Option<&komponist_ipc::waveform_markers::Snapshot> {
        self.manager.waveform_markers(device, slot)
    }

    fn edit_waveform_markers(
        &mut self,
        device: DeviceId,
        slot: u32,
        request: komponist_ipc::waveform_markers::Request,
    ) -> Result<(), komponist_ipc::waveform_markers::EditStatus> {
        self.manager.edit_waveform_markers(device, slot, request)
    }

    fn supports_user_presets(&self, device: DeviceId) -> bool {
        self.manager.supports_user_presets(device)
    }

    fn capture_clap_preset(&mut self, device: DeviceId) -> bool {
        self.manager.capture_clap_preset(device)
    }

    fn load_user_preset(&mut self, device: DeviceId, state: Vec<u8>) -> bool {
        self.manager.load_user_preset(device, state)
    }

    fn prepare_clap_preset(
        &self,
        plugin: &PluginRef,
        source: &ClapPresetSource,
    ) -> Option<komponist_plugins::PresetPreparation> {
        self.manager.prepare_clap_preset(plugin, source)
    }

    fn clap_presets(&self, device: &Device) -> &[ClapPreset] {
        self.manager.clap_presets(device)
    }

    fn load_clap_preset(&mut self, device: &Device, source: &ClapPresetSource) -> bool {
        self.manager.load_clap_preset(device, source)
    }

    fn late_blocks(&self, device: DeviceId) -> u64 {
        self.manager.late_blocks(device)
    }

    fn state_warning(&self, device: DeviceId) -> Option<&str> {
        self.manager.state_warning(device)
    }

    fn note_names(&self, device: DeviceId) -> &[NoteName] {
        self.manager.note_names(device)
    }

    fn set_project_dir(&mut self, dir: Option<PathBuf>) {
        self.manager.set_project_dir(dir.clone());
        self.project_dir = dir;
    }

    fn project_dir(&self) -> Option<PathBuf> {
        self.project_dir.clone()
    }

    fn preview_waveform(&self, device: DeviceId, slot: u32, time: Option<f64>) {
        self.manager.preview_waveform(device, slot, time);
    }

    fn asset_slots(&self, device: DeviceId) -> Option<&Snapshot> {
        self.manager.asset_slots(device)
    }

    fn supports_asset_edit_state(&self, device: DeviceId) -> bool {
        self.manager.supports_asset_edit_state(device)
    }

    fn prepare_asset_edit(
        &mut self,
        device: &Device,
        changes: &[AssetEdit],
        request: u64,
    ) -> Result<AssetEditPreparation, DeviceId> {
        self.manager.prepare_asset_edit(device, changes, request)
    }

    fn cancel_asset_edit(&mut self, device: DeviceId) {
        self.manager.cancel_asset_edit(device);
    }

    fn set_asset_bindings(&mut self, device: DeviceId, bindings: &Bindings) -> bool {
        self.manager.set_asset_bindings(device, bindings)
    }

    fn sync_asset_links(
        &mut self,
        device: DeviceId,
        before: &BTreeMap<u32, AssetLink>,
        after: &BTreeMap<u32, AssetLink>,
    ) {
        self.manager.sync_asset_links(device, before, after);
    }

    fn asset_inventory_pending(&self, device: DeviceId) -> bool {
        self.manager.asset_inventory_pending(device)
    }

    fn render_asset_bindings_pending(&self, device: DeviceId) -> bool {
        self.manager.render_asset_bindings_pending(device)
    }

    fn supports_offline_preparation(&self, device: DeviceId) -> bool {
        self.manager.supports_offline_preparation(device)
    }

    fn set_offline_preparation(&mut self, device: DeviceId, request: Option<OfflineRequest>) {
        self.manager.set_offline_preparation(device, request);
    }

    fn offline_preparation_status(&self, device: DeviceId) -> Option<OfflineStatus> {
        self.manager.offline_preparation_status(device)
    }

    fn has_gui(&self, device: DeviceId) -> bool {
        self.manager.has_gui(device)
    }

    fn has_sidechain(&self, device: DeviceId) -> bool {
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

    fn snapshot_states(&mut self, timeout: Duration) -> DeviceSnapshots {
        self.manager.snapshot_states(timeout)
    }

    fn has_pending_state_edits(&self) -> bool {
        self.manager.has_pending_state_edits()
    }

    fn snapshot_for_save(&mut self, timeout: Duration) -> SaveSnapshots {
        self.manager.snapshot_for_save(timeout)
    }

    fn snapshot_available_states(
        &mut self,
        devices: &[DeviceId],
        timeout: Duration,
    ) -> SaveSnapshots {
        self.manager.snapshot_available_states(devices, timeout)
    }

    fn schedule_retiring_snapshot(&mut self, device: DeviceId) {
        self.manager.schedule_retiring_snapshot(device);
    }

    fn retiring_snapshot_pending(&self, device: DeviceId) -> bool {
        self.manager.retiring_snapshot_pending(device)
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
    use crate::app::App;
    use komponist_model::plugin_assets::AssetLink;

    const SETUP: ProcessorSetup = ProcessorSetup {
        sample_rate: 48_000.0,
        max_block: komponist_engine::MAX_BLOCK,
    };

    fn shipped(host: &PluginHost, name: &str) -> Option<PluginRef> {
        let id = format!("org.klang-studio.komponist.{name}");
        let plugin = host
            .catalog()
            .entries()
            .iter()
            .find(|entry| {
                entry.plugin.id == id && komponist_plugins::requires_bundled(&entry.plugin)
            })
            .map(|entry| entry.plugin.clone());
        if plugin.is_none() {
            assert!(
                !std::env::var_os("KOMPONIST_REQUIRE_FIXTURES").is_some_and(|value| value == "1"),
                "required bundled fixture {id} is missing from the catalog"
            );
            eprintln!("skipping: build {id} with scripts/build-test-plugins.sh");
        }
        plugin
    }

    #[test]
    fn the_fixture_catalog_resolves_shipped_devices_and_reports_unavailable_plugins() {
        let mut host = App::headless_plugin_host();
        let Some(hexa) = shipped(&host, "hexa") else {
            return;
        };
        assert_eq!(host.catalog().entries().len(), 12);
        assert_eq!(host.role(&hexa), Some(DeviceRole::Instrument));
        let demo = komponist_model::fixtures::demo_project();
        for track in &demo.tracks[..3] {
            let (_, instrument) = track.channel.instrument().unwrap();
            assert_eq!(
                host.role(&instrument.plugin),
                Some(DeviceRole::Instrument),
                "{} ({}) must resolve in the staged package",
                instrument.plugin.id,
                instrument.plugin.module
            );
        }
        let mut unavailable = hexa.clone();
        unavailable.id = "de.klang-studio.test.unavailable".into();
        unavailable.module = "not-installed.clap".into();
        let (project, _) = komponist_model::edits::add_track(
            &Project::new("Fixture inventory"),
            komponist_model::edits::NewTrack {
                name: None,
                hue: None,
                instrument: Some(unavailable.clone()),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        assert_eq!(host.role(&unavailable), None);
        let missing = host.missing_plugins(&project);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].plugin, unavailable);
        assert_eq!(missing[0].role, DeviceRole::Instrument);
        let device = &project.tracks[0].channel.devices[0];
        assert!(host.create_processor(device, SETUP).is_none());
        assert_eq!(host.status(device.id), DeviceStatus::Missing);
        assert!(host.params(device.id).is_none());
        assert!(!host.has_gui(device.id));
        host.release(device.id);
        assert_eq!(host.status(device.id), DeviceStatus::Ok);
    }

    #[test]
    fn the_shipped_sampler_loads_linked_audio_into_its_authored_pad_bank() {
        use komponist_ipc::device_assets::AssetStatus;
        use std::time::Instant;

        let host = App::headless_plugin_host();
        let Some(sampler) = shipped(&host, "sampler") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let kit = komponist_audiofile::fixtures::write_kit(dir.path()).unwrap();
        let (project, _) = komponist_model::edits::add_track(
            &Project::new("Sampler assets"),
            komponist_model::edits::NewTrack {
                name: None,
                hue: None,
                instrument: Some(sampler),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = project.tracks[0].channel.devices[0].id;
        let links = kit
            .iter()
            .take(2)
            .enumerate()
            .map(|(pad, path)| (pad as u32, AssetLink::new(path, None)))
            .collect();
        let project =
            komponist_model::edits::set_device_asset_links(&project, device, links).unwrap();
        let mut app = App::headless(project);
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            app.step();
            let host = app.engine().host();
            if host.status(device) == DeviceStatus::Ok
                && host.asset_slots(device).is_some_and(|assets| {
                    assets
                        .slots
                        .iter()
                        .filter(|slot| slot.status == AssetStatus::Ready)
                        .count()
                        == 2
                })
            {
                let layout = host.native_layout(device).unwrap();
                let bank = layout.pad_bank.as_ref().unwrap();
                assert_eq!(
                    bank.pads
                        .iter()
                        .take(2)
                        .map(|pad| pad.note)
                        .collect::<Vec<_>>(),
                    [36, 37]
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the sampler fixture did not load its kit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
