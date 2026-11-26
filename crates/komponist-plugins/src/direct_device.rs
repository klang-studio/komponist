//! Control-plane state for an opted-in direct CLAP device.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::notification_bits;
use komponist_hosting::{HostedPlugin, HostingError};
use komponist_ipc::clap_presets::{PresetLoad, PresetReport};
use komponist_ipc::control::{MAX_STATE_PARAMETER_BASES, param_flags};
use komponist_ipc::native_layout::NativeLayout;
use komponist_ipc::waveform_markers::{EditResult, EditStatus, Request};
use komponist_ipc::{
    LogLevel, NoteName, ParamInfo, ParamValue, RenderMode, WireEvent, WireEventKind,
};
use komponist_model::project::SavedPluginState;
use komponist_model::{DeviceId, DeviceRole};

use crate::bundled_clap::VerifiedClap;
use crate::direct::DirectClap;
use crate::direct_session::{DirectEngine, DirectSession};
use crate::manager::{
    DeviceStatus, FailReason, PluginManagerEvent, ProcessorSetup, STATE_LOG_PREFIX,
};

use crate::waveform_markers::{MarkerCache, MarkerParameters, WaveformMarkerEditResult};

const MARKER_EDIT_TIMEOUT: Duration = Duration::from_secs(1);

struct PendingMarkerEdit {
    slot: u32,
    request: Request,
    params: MarkerParameters,
    deadline: Instant,
}

pub(crate) struct DirectDevice {
    pub(crate) assets: crate::device_assets::DeviceAssets,
    session: DirectSession,
    pub(crate) params: Vec<ParamInfo>,
    pub(crate) markers: MarkerCache,
    pub(crate) layout: Option<Arc<NativeLayout>>,
    pub(crate) values: BTreeMap<u32, ParamValue>,
    pending_param_text: BTreeMap<u32, (u64, String)>,
    pending_value_text: BTreeMap<u32, f64>,
    pub(crate) base: BTreeMap<u32, f64>,
    last_state: Option<SavedPluginState>,
    pub(crate) state_warning: Option<String>,
    pub(crate) note_names: Vec<NoteName>,
    pub(crate) sidechain: bool,
    pub(crate) latency: u32,
    pub(crate) status: DeviceStatus,
    pending_writes: BTreeMap<u32, f64>,
    /// Persistent edits that still need to reach the plugin before a snapshot can replace state.
    unconfirmed_writes: BTreeSet<u32>,
    pending_restore: Option<SavedPluginState>,
    pending_marker_edit: Option<PendingMarkerEdit>,
    pending_preset: Option<(PresetLoad, Instant)>,
    /// Edits made after the request must reach the replacement control epoch.
    after_preset_writes: BTreeMap<u32, f64>,
    capture_preset: bool,
    pub(crate) display: Option<Arc<komponist_ipc::device_canvas::DisplayList>>,
    display_at: Instant,
    macro_params: BTreeMap<u32, f64>,
    /// A direct lane retains the host base. An automated macro retains its saved output.
    automated_params: BTreeMap<u32, Option<f64>>,
    released_automation: BTreeSet<u32>,
    snapshot_due: Option<Instant>,
    macro_snapshot: Option<Weak<[u8]>>,
    user_edit: bool,
    debounce: Duration,
}

impl std::fmt::Debug for DirectDevice {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("DirectDevice")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

struct Inventory {
    params: Vec<ParamInfo>,
    values: BTreeMap<u32, ParamValue>,
    layout: Option<Arc<NativeLayout>>,
    note_names: Vec<NoteName>,
}

enum MetadataUpdate {
    Inventory(Inventory),
    NoteNames(Vec<NoteName>),
}

impl Inventory {
    fn read(plugin: &mut dyn HostedPlugin) -> Self {
        let params = plugin.params();
        let values = params
            .iter()
            .filter_map(|param| {
                let id = param.id;
                let value = plugin.param_value(id)?;
                Some((
                    id,
                    ParamValue {
                        id,
                        value,
                        text: plugin.param_text(id, value).unwrap_or_default(),
                    },
                ))
            })
            .collect();
        Self {
            params,
            values,
            layout: plugin
                .native_layout()
                .map(|(_, layout)| Arc::new(layout.clone())),
            note_names: plugin.note_names(),
        }
    }
}

/// Load the opaque bytes, then the host-owned parameter bases through CLAP parameters. An
/// inactive instance takes the bases at once; an active one returns them for the audio thread.
fn load_saved_state(
    plugin: &mut dyn HostedPlugin,
    state: &SavedPluginState,
    active: bool,
) -> Result<BTreeMap<u32, f64>, HostingError> {
    if state.parameter_bases.len() > MAX_STATE_PARAMETER_BASES
        || state
            .parameter_bases
            .values()
            .any(|value| !value.is_finite())
    {
        return Err(HostingError::State("invalid host parameter state".into()));
    }
    plugin.load_state(&state.bytes)?;
    if state.parameter_bases.is_empty() {
        return Ok(BTreeMap::new());
    }
    let excluded = param_flags::HIDDEN | param_flags::READ_ONLY | param_flags::BYPASS;
    let bases: BTreeMap<_, _> = plugin
        .params()
        .iter()
        .filter_map(|info| {
            let value = *state.parameter_bases.get(&info.id)?;
            (info.flags & excluded == 0
                && info.min.is_finite()
                && info.max.is_finite()
                && info.min <= info.max)
                .then(|| (info.id, value.clamp(info.min, info.max)))
        })
        .collect();
    if active {
        return Ok(bases);
    }
    let input: Vec<_> = bases
        .iter()
        .map(|(&id, &value)| WireEvent::new(0, WireEventKind::ParamValue { id, value }))
        .collect();
    plugin.flush_params(&input, &mut Vec::new());
    Ok(BTreeMap::new())
}

impl DirectDevice {
    pub(crate) fn is_running_from(&self, path: &std::path::Path) -> bool {
        !matches!(self.status, DeviceStatus::Crashed(_)) && self.session.is_from_bundle(path)
    }

    pub(crate) fn has_clap_presets_from(&self, path: &std::path::Path) -> bool {
        self.status == DeviceStatus::Ok && self.session.is_from_bundle(path)
    }

    pub(crate) fn signal_tap(&self, key: &str) -> Option<&crate::signal_taps::SignalTap> {
        self.session.signal_tap(key)
    }

    pub(crate) fn start(
        proof: VerifiedClap,
        role: DeviceRole,
        setup: ProcessorSetup,
        state: Option<SavedPluginState>,
        debounce: Duration,
    ) -> Result<(Self, DirectEngine), HostingError> {
        let mut host = DirectClap::load(proof)?;
        let (warning, inventory, sidechain) = host
            .try_control(|plugin| {
                let warning = state.as_ref().and_then(|state| {
                    load_saved_state(plugin, state, false)
                        .err()
                        .map(|error| error.to_string())
                });
                (
                    warning,
                    Inventory::read(plugin),
                    plugin.descriptor().sidechain,
                )
            })
            .ok_or(HostingError::WrongState("inactive CLAP instance is busy"))?;
        let (session, engine) = DirectSession::start(host, setup, role)?;
        let mut device = Self {
            latency: session.latency(),
            session,
            params: Vec::new(),
            assets: crate::device_assets::DeviceAssets::new(),
            markers: MarkerCache::default(),
            layout: None,
            values: BTreeMap::new(),
            pending_param_text: BTreeMap::new(),
            pending_value_text: BTreeMap::new(),
            base: BTreeMap::new(),
            last_state: state,
            state_warning: warning,
            note_names: Vec::new(),
            sidechain,
            status: DeviceStatus::Ok,
            pending_writes: BTreeMap::new(),
            unconfirmed_writes: BTreeSet::new(),
            pending_restore: None,
            pending_marker_edit: None,
            pending_preset: None,
            after_preset_writes: BTreeMap::new(),
            capture_preset: false,
            display: None,
            display_at: Instant::now(),
            macro_params: BTreeMap::new(),
            automated_params: BTreeMap::new(),
            released_automation: BTreeSet::new(),
            snapshot_due: None,
            macro_snapshot: None,
            user_edit: false,
            debounce,
        };
        device.accept_inventory(inventory, true);
        Ok((device, engine))
    }

    fn accept_inventory(&mut self, inventory: Inventory, restored: bool) {
        let mut next_base = BTreeMap::new();
        for (&id, value) in &inventory.values {
            let plain = if restored {
                self.pending_writes.get(&id).copied().unwrap_or(value.value)
            } else {
                self.base.get(&id).copied().unwrap_or(value.value)
            };
            next_base.insert(id, plain);
        }
        self.base = next_base;
        self.values = inventory.values;
        self.params = inventory.params;
        self.layout = inventory.layout;
        self.note_names = inventory.note_names;
    }

    pub(crate) fn report_metadata(id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        events.push(PluginManagerEvent::ParamsChanged { device: id });
        events.push(PluginManagerEvent::NoteNamesChanged { device: id });
    }

    pub(crate) fn report_warning(&self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        if let Some(reason) = &self.state_warning {
            events.push(PluginManagerEvent::Log {
                device: Some(id),
                level: LogLevel::Warning,
                message: format!("{STATE_LOG_PREFIX} {reason}"),
            });
        }
    }

    pub(crate) fn poll_waveform_markers(&mut self) {
        if self.status == DeviceStatus::Ok
            && self.pending_restore.is_none()
            && !self.capturing_state_with_parameter_bases()
            && self.pending_preset.is_none()
            && self.pending_marker_edit.is_none()
            && let Some(slot) = self.markers.poll()
        {
            self.markers
                .report(slot, self.session.waveform_markers(slot));
        }
    }

    fn fail_after_rollback(
        &mut self,
        device: DeviceId,
        message: &str,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        self.session.stop_after_failed_restore();
        self.fail_preset_capture(device, message, events);
        self.status = DeviceStatus::Crashed(FailReason::LoadFailed(message.into()));
        events.push(PluginManagerEvent::StatusChanged {
            device,
            status: self.status.clone(),
        });
    }

    pub(crate) fn edit_waveform_markers(
        &mut self,
        device: DeviceId,
        slot: u32,
        request: Request,
        events: &mut Vec<PluginManagerEvent>,
    ) -> Result<(), EditStatus> {
        if self.is_editing_waveform_markers() || !self.can_edit_waveform_markers() {
            return Err(EditStatus::Unavailable);
        }
        self.markers.checked_edit(slot, request)?;
        let params = self
            .marker_parameters(slot)
            .ok_or(EditStatus::Unavailable)?;
        self.pending_marker_edit = Some(PendingMarkerEdit {
            slot,
            request,
            params,
            deadline: Instant::now() + MARKER_EDIT_TIMEOUT,
        });
        self.poll_marker_edit(device, events);
        Ok(())
    }

    pub(crate) fn is_editing_waveform_markers(&self) -> bool {
        self.pending_marker_edit.is_some()
    }

    pub(crate) fn invalidate_waveform_markers(&mut self) -> Option<u32> {
        let canceled = self.pending_marker_edit.take().map(|edit| edit.slot);
        self.markers.invalidate();
        canceled
    }

    fn can_edit_waveform_markers(&self) -> bool {
        self.can_snapshot()
            && self.pending_preset.is_none()
            && self.pending_restore.is_none()
            && !self.capturing_state_with_parameter_bases()
            && !self.assets.bindings_pending()
            && self.pending_writes.is_empty()
            && self.unconfirmed_writes.is_empty()
    }

    fn marker_parameters(&self, slot: u32) -> Option<MarkerParameters> {
        MarkerParameters::new(
            slot,
            &self.base,
            &self.automated_params,
            &self.macro_params,
            &self.params,
            self.layout.as_deref(),
            [],
        )
    }

    fn try_marker_edit(&mut self, edit: &PendingMarkerEdit) -> Option<EditResult> {
        if Instant::now() >= edit.deadline
            || !self.can_edit_waveform_markers()
            || self.marker_parameters(edit.slot).as_ref() != Some(&edit.params)
        {
            return Some(EditResult::Rejected(EditStatus::Unavailable));
        }
        let request = match self.markers.checked_edit(edit.slot, edit.request) {
            Ok(request) => request,
            Err(status) => return Some(EditResult::Rejected(status)),
        };
        self.session.try_control(|plugin| {
            komponist_hosting::edit_waveform_marker_state(
                plugin,
                edit.slot,
                request,
                &edit.params.expected,
            )
        })
    }

    fn poll_marker_edit(&mut self, device: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        let Some(edit) = self.pending_marker_edit.take() else {
            return;
        };
        let Some(result) = self.try_marker_edit(&edit) else {
            // The audio processor won the control guard. Keep the original revision and
            // expected values for one attempt on the next host poll.
            self.pending_marker_edit = Some(edit);
            return;
        };
        let slot = edit.slot;
        if let EditResult::Failed {
            message,
            rollback_failed: true,
        } = &result
        {
            self.fail_after_rollback(device, message, events);
        }
        if !matches!(
            result,
            EditResult::Rejected(EditStatus::Invalid | EditStatus::Unchanged)
        ) {
            self.markers.refresh(slot);
        }
        if matches!(result, EditResult::Applied { .. }) {
            self.user_edit = false;
            self.snapshot_due = None;
        }
        let mut last_state = None;
        let result =
            WaveformMarkerEditResult::from_wire(result, edit.params.bases(), &mut last_state);
        if let Some(state) = last_state {
            self.last_state = Some(state);
        }
        events.push(PluginManagerEvent::WaveformMarkersEdited {
            device,
            slot,
            result,
        });
    }

    pub(crate) fn preview_waveform(&self, slot: u32, time: Option<f64>) {
        self.session.preview_waveform(slot, time);
    }

    pub(crate) fn poll_display(&mut self) {
        let now = Instant::now();
        if now >= self.display_at
            && self.pending_restore.is_none()
            && self.status == DeviceStatus::Ok
            && self
                .layout
                .as_ref()
                .is_some_and(|layout| layout.has_canvas())
        {
            self.display_at = now + komponist_ipc::device_canvas::REFRESH;
            self.display = self.session.device_display().map(Arc::new);
        }
    }

    pub(crate) fn prepare_asset_edit_state(
        &self,
        edits: &komponist_ipc::device_assets::edit_state::Edits,
        state: Option<&[u8]>,
    ) -> komponist_ipc::device_assets::edit_state::PreparedChange {
        self.session.prepare_asset_edit_state(edits, state)
    }

    pub(crate) fn poll_assets(&mut self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        if self.pending_restore.is_some()
            || self.pending_preset.is_some()
            || matches!(self.status, DeviceStatus::Crashed(_))
        {
            return;
        }
        if let Some(bindings) = self.assets.take_bindings() {
            let accepted = self.session.set_asset_bindings(&bindings);
            self.assets.report_bindings(accepted);
            if !accepted {
                events.push(crate::device_assets::binding_rejected(id));
            }
        }
        if self.assets.slots_due() {
            let (slots, file_slots, offline, edit_state) = self.session.asset_slots();
            self.assets
                .report_slots(slots, file_slots, offline, edit_state);
        }
        if let Some(request) = self.assets.offline_due() {
            let status = self.session.prepare_offline(request);
            self.assets.report_offline(request, status);
        }
    }

    pub(crate) fn queue_param_text(&mut self, request: u64, id: u32, text: String) {
        self.pending_param_text.insert(id, (request, text));
    }

    pub(crate) fn poll(&mut self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        self.service(id, events);
        if self.status == DeviceStatus::Ok
            && self.pending_restore.is_none()
            && !self.pending_param_text.is_empty()
            && let Some(parsed) = self.session.try_control(|plugin| {
                self.pending_param_text
                    .iter()
                    .map(|(&param_id, (request, text))| {
                        (*request, param_id, plugin.parse_param_text(param_id, text))
                    })
                    .collect::<Vec<_>>()
            })
        {
            self.pending_param_text.clear();
            for (request, param_id, value) in parsed {
                let value = self
                    .params
                    .iter()
                    .find(|param| param.id == param_id)
                    .and_then(|param| {
                        value.filter(|value| (param.min..=param.max).contains(value))
                    });
                events.push(PluginManagerEvent::ParamTextParsed {
                    device: id,
                    request,
                    id: param_id,
                    value,
                });
            }
        }
        if self.status == DeviceStatus::Ok
            && self.pending_restore.is_none()
            && !self.pending_value_text.is_empty()
            && let Some(texts) = self.session.try_control(|plugin| {
                self.pending_value_text
                    .iter()
                    .map(|(&id, &value)| (id, value, plugin.param_text(id, value)))
                    .collect::<Vec<_>>()
            })
        {
            self.pending_value_text.clear();
            let mut changed = false;
            for (param_id, value, text) in texts {
                if let Some(text) = text
                    && let Some(cached) = self.values.get_mut(&param_id)
                    && cached.value == value
                    && cached.text != text
                {
                    cached.text = text;
                    changed = true;
                }
            }
            if changed {
                events.push(PluginManagerEvent::ParamValuesChanged { device: id });
            }
        }
        self.poll_marker_edit(id, events);
        if self.snapshot_due.is_some_and(|due| Instant::now() >= due)
            && let Some(state) = self.capture()
        {
            events.push(PluginManagerEvent::StateSnapshot { device: id, state });
        }
    }

    pub(crate) fn service(&mut self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        if matches!(self.status, DeviceStatus::Crashed(_)) {
            return;
        }
        let observed = match self.session.poll() {
            Ok(observed) => observed,
            Err(error) => {
                let message = error.to_string();
                if self.pending_preset.take().is_some() {
                    self.after_preset_writes.clear();
                    events.push(PluginManagerEvent::ClapPresetFailed {
                        device: id,
                        message: message.clone(),
                    });
                }
                self.fail_preset_capture(id, &message, events);
                self.status = DeviceStatus::Crashed(FailReason::LoadFailed(message));
                events.push(PluginManagerEvent::StatusChanged {
                    device: id,
                    status: self.status.clone(),
                });
                return;
            }
        };
        let status = if self.session.is_ready() {
            DeviceStatus::Ok
        } else {
            DeviceStatus::Loading
        };
        if self.status != status {
            self.status = status;
            events.push(PluginManagerEvent::StatusChanged {
                device: id,
                status: self.status.clone(),
            });
        }
        let latency = self.session.latency();
        if latency != self.latency {
            self.latency = latency;
            events.push(PluginManagerEvent::LatencyChanged {
                device: id,
                latency,
            });
        }
        if let Some(state) = self.pending_restore.as_ref()
            && let Some((result, inventory)) = self.session.restore(
                |plugin| load_saved_state(plugin, state, true),
                Inventory::read,
            )
        {
            self.last_state = self.pending_restore.take();
            self.assets.finish_restore();
            let bases = match result {
                Ok(bases) => {
                    self.state_warning = None;
                    bases
                }
                Err(error) => {
                    self.state_warning = Some(error.to_string());
                    BTreeMap::new()
                }
            };
            self.report_warning(id, events);
            // The restored bases reach the processor with the next block, like a user edit.
            // Automation and macros drive their targets again after `StateApplied`; their
            // bases stay host-side so a late base cannot overwrite the reasserted value.
            let (driven, queued): (BTreeMap<_, _>, BTreeMap<_, _>) =
                bases.into_iter().partition(|(param, _)| {
                    self.automated_params.contains_key(param)
                        || self.macro_params.contains_key(param)
                });
            self.unconfirmed_writes.extend(queued.keys().copied());
            self.pending_writes.extend(queued);
            self.accept_inventory(inventory, true);
            for (param, value) in driven {
                if let Some(base) = self.base.get_mut(&param) {
                    *base = value;
                }
            }
            for (param, value) in self.pending_writes.clone() {
                self.preview_param(param, value);
            }
            Self::report_metadata(id, events);
            if self.state_warning.is_none() {
                events.push(PluginManagerEvent::StateApplied { device: id });
            }
        }
        self.service_preset(id, events);
        if self.capture_preset
            && self.pending_preset.is_none()
            && self.pending_restore.is_none()
            && self.pending_marker_edit.is_none()
            && self.pending_writes.is_empty()
            && self.unconfirmed_writes.is_empty()
            && let Some(result) = self
                .session
                .try_control(|plugin| plugin.save_preset_state())
        {
            self.capture_preset = false;
            events.push(PluginManagerEvent::ClapPresetCaptured {
                device: id,
                result: result.map_err(|error| error.to_string()),
            });
        }
        if self.pending_restore.is_none() {
            self.pending_writes
                .retain(|&param, value| !self.session.set_param(param, *value));
        }
        let metadata = self.session.refresh_metadata(|plugin, pending| {
            if pending & notification_bits::PARAMS_CHANGED != 0 {
                MetadataUpdate::Inventory(Inventory::read(plugin))
            } else {
                MetadataUpdate::NoteNames(plugin.note_names())
            }
        });
        match metadata {
            Some(MetadataUpdate::Inventory(inventory)) => {
                self.accept_inventory(inventory, false);
                Self::report_metadata(id, events);
            }
            Some(MetadataUpdate::NoteNames(note_names)) => {
                self.note_names = note_names;
                events.push(PluginManagerEvent::NoteNamesChanged { device: id });
            }
            None => {}
        }
        self.report_presets(id, events);
        if observed.has(notification_bits::STATE_DIRTY) {
            self.user_edit = true;
            self.snapshot_due = Some(Instant::now() + self.debounce);
        }
    }

    /// Preset loads the plugin reported on its own (CLAP preset-load): the selection for the
    /// app and a snapshot of the state the plugin loaded.
    fn report_presets(&mut self, device: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        for report in self.session.take_preset_reports() {
            match report {
                PresetReport::Loaded { source, startup } => {
                    if !startup {
                        // The plugin changed its own state: the snapshot follows.
                        self.user_edit = true;
                        self.snapshot_due = Some(Instant::now() + self.debounce);
                    }
                    events.push(PluginManagerEvent::ClapPresetReported {
                        device,
                        source,
                        startup,
                    });
                }
                PresetReport::Failed { message, .. } => {
                    events.push(PluginManagerEvent::ClapPresetFailed { device, message });
                }
            }
        }
    }

    fn fail_preset_capture(
        &mut self,
        device: DeviceId,
        message: &str,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        if std::mem::take(&mut self.capture_preset) {
            events.push(PluginManagerEvent::ClapPresetCaptured {
                device,
                result: Err(message.into()),
            });
        }
    }

    pub(crate) fn is_capturing_clap_preset(&self) -> bool {
        self.capture_preset
    }

    pub(crate) fn capture_clap_preset(&mut self) -> bool {
        if self.status != DeviceStatus::Ok
            || self.capture_preset
            || self.pending_preset.is_some()
            || self.pending_restore.is_some()
        {
            return false;
        }
        self.capture_preset = true;
        true
    }

    pub(crate) fn load_clap_preset(&mut self, preset: PresetLoad) -> bool {
        if self.status != DeviceStatus::Ok
            || self.pending_preset.is_some()
            || self.pending_restore.is_some()
            || self.capture_preset
            || self.is_editing_waveform_markers()
        {
            return false;
        }
        self.markers.invalidate();
        self.assets.begin_restore();
        self.pending_preset = Some((preset, Instant::now() + Duration::from_secs(2)));
        true
    }

    pub(crate) fn is_loading_clap_preset(&self) -> bool {
        self.pending_preset.is_some()
    }

    /// Persistent edits that must take the epoch-ordered control queue instead of the render
    /// path: during a preset load or a restore, and for devices that edit waveform markers
    /// (restored bases reach the processor on that queue too, as in the bridge).
    pub(crate) fn requires_control_param_edit(&self) -> bool {
        self.pending_preset.is_some()
            || self.pending_restore.is_some()
            || self
                .layout
                .as_deref()
                .is_some_and(crate::waveform_markers::has_marker_graph)
    }

    fn service_preset(&mut self, device: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        let Some((preset, deadline)) = self.pending_preset.as_ref() else {
            return;
        };
        let result = if Instant::now() >= *deadline {
            Err((
                "timed out waiting for preceding parameter edits".into(),
                true,
            ))
        } else {
            if self.status != DeviceStatus::Ok || !self.pending_writes.is_empty() {
                return;
            }
            let base = &self.base;
            let automated = &self.automated_params;
            let macros = &self.macro_params;
            let unconfirmed = &self.unconfirmed_writes;
            let Some(Some(result)) = self.session.replace_state(|plugin| {
                // Check delivery and capture under the same processing guard.
                if unconfirmed.iter().any(|id| {
                    let expected = automated
                        .get(id)
                        .copied()
                        .flatten()
                        .or_else(|| macros.get(id).copied())
                        .or_else(|| base.get(id).copied());
                    expected.is_some_and(|expected| {
                        !plugin.param_value(*id).is_some_and(|actual| {
                            (actual - expected).abs() <= 1e-6 * expected.abs().max(1.0)
                        })
                    })
                }) {
                    return (None, false);
                }
                let result = plugin.apply_clap_preset(preset);
                let replaced = match &result {
                    Ok(_) => true,
                    Err((_, recovered)) => !recovered,
                };
                (
                    Some(result.map(|state| (state, Inventory::read(plugin)))),
                    replaced,
                )
            }) else {
                return;
            };
            result
        };
        let source = match self.pending_preset.take() {
            Some((PresetLoad::Discovered(preset), _)) => Some(preset.source),
            _ => None,
        };
        self.assets.finish_restore();
        match result {
            Ok(((before, after), inventory)) => {
                let state: Arc<[u8]> = after.into();
                self.last_state = Some(state.clone().into());
                self.state_warning = None;
                self.unconfirmed_writes.clear();
                self.released_automation.clear();
                self.snapshot_due = None;
                self.user_edit = false;
                self.accept_inventory(inventory, true);
                Self::report_metadata(device, events);
                events.push(PluginManagerEvent::ClapPresetLoaded {
                    device,
                    source,
                    previous_state: before.into(),
                    state,
                });
            }
            Err((message, recovered)) => {
                if !recovered {
                    self.fail_after_rollback(device, &message, events);
                }
                events.push(PluginManagerEvent::ClapPresetFailed { device, message });
            }
        }
        let later_writes = std::mem::take(&mut self.after_preset_writes);
        if !matches!(self.status, DeviceStatus::Crashed(_)) {
            for (id, value) in later_writes {
                self.set_param(id, value);
            }
        }
    }

    pub(crate) fn set_render_mode(
        &mut self,
        id: DeviceId,
        mode: RenderMode,
        events: &mut Vec<PluginManagerEvent>,
    ) {
        self.session.set_render_mode(mode);
        if !self.session.is_ready() && self.status == DeviceStatus::Ok {
            self.status = DeviceStatus::Loading;
            events.push(PluginManagerEvent::StatusChanged {
                device: id,
                status: DeviceStatus::Loading,
            });
        }
    }

    pub(crate) fn retired(&mut self) -> bool {
        self.session.retired()
    }

    fn param_edit_value(&self, id: u32, value: f64) -> Option<f64> {
        (value.is_finite() && self.base.contains_key(&id)).then(|| {
            self.params
                .iter()
                .find(|param| param.id == id && param.step_count != 0)
                .map_or(value, |param| crate::params::param_info(param).snap(value))
        })
    }

    pub(crate) fn edited_param(&mut self, id: u32, value: f64) -> bool {
        let Some(value) = self.param_edit_value(id, value) else {
            return false;
        };
        self.record_param_edit(id, value);
        true
    }

    fn record_param_edit(&mut self, id: u32, value: f64) {
        self.preview_param(id, value);
        if !self.macro_params.contains_key(&id) {
            if self.pending_restore.is_some() {
                self.pending_writes.insert(id, value);
            }
            self.base.insert(id, value);
            self.released_automation.remove(&id);
            self.unconfirmed_writes.insert(id);
            self.state_warning = None;
            self.user_edit = true;
            self.snapshot_due = Some(Instant::now() + self.debounce);
        }
    }

    pub(crate) fn preview_param(&mut self, id: u32, value: f64) {
        if let Some(previous) = self.values.get_mut(&id)
            && previous.value != value
        {
            previous.value = value;
            previous.text.clear();
            if self
                .layout
                .as_ref()
                .and_then(|layout| layout.parameter_display(id))
                .is_some_and(|display| display.plugin_text)
            {
                self.pending_value_text.insert(id, value);
            }
        }
    }

    pub(crate) fn set_param(&mut self, id: u32, requested: f64) {
        let Some(value) = self.param_edit_value(id, requested) else {
            return;
        };
        if self.pending_preset.is_some() {
            self.preview_param(id, value);
            self.after_preset_writes.insert(id, requested);
        } else {
            self.record_param_edit(id, value);
            self.pending_writes.insert(id, value);
        }
    }

    pub(crate) fn set_automated_params(&mut self, params: BTreeMap<u32, Option<f64>>) {
        // SetProgram and the base-value restore arrive on the audio thread asynchronously.
        self.released_automation.extend(
            self.automated_params
                .keys()
                .filter(|id| !params.contains_key(id))
                .copied(),
        );
        self.released_automation
            .retain(|id| !params.contains_key(id));
        self.automated_params = params;
    }

    pub(crate) fn set_macro_params(&mut self, values: BTreeMap<u32, f64>) {
        if self.macro_params == values {
            return;
        }
        let previous = std::mem::replace(&mut self.macro_params, values);
        // A newly mapped parameter takes its persistent value from the macro.
        self.unconfirmed_writes
            .retain(|id| !self.macro_params.contains_key(id));
        for (id, value) in previous {
            if !self.macro_params.contains_key(&id) && self.base.contains_key(&id) {
                self.base.insert(id, value);
                self.user_edit = true;
            }
        }
        self.snapshot_due = Some(Instant::now() + self.debounce);
    }

    pub(crate) fn is_macro_snapshot(&self, state: &Arc<[u8]>) -> bool {
        self.macro_snapshot
            .as_ref()
            .is_some_and(|value| value.ptr_eq(&Arc::downgrade(state)))
    }

    pub(crate) fn set_state(&mut self, state: SavedPluginState) -> Option<u32> {
        let canceled = self.invalidate_waveform_markers();
        self.assets.begin_restore();
        self.pending_preset = None;
        self.after_preset_writes.clear();
        self.pending_restore = Some(state);
        self.pending_writes.clear();
        self.unconfirmed_writes.clear();
        self.released_automation.clear();
        self.snapshot_due = None;
        self.user_edit = false;
        canceled
    }

    pub(crate) fn state_for_reload(&self) -> Option<SavedPluginState> {
        self.pending_restore
            .clone()
            .or_else(|| self.last_state.clone())
    }

    pub(crate) fn request_state(&mut self) {
        if self.state_warning.is_none() {
            self.snapshot_due = Some(Instant::now());
        }
    }

    pub(crate) fn snapshot_capture_pending(&self) -> bool {
        self.needs_snapshot() || self.state_warning.is_none() && self.snapshot_due.is_some()
    }

    /// The restored bases must reach the document before a marker edit can supersede capture.
    fn capturing_state_with_parameter_bases(&self) -> bool {
        self.last_state
            .as_ref()
            .is_some_and(|state| !state.parameter_bases.is_empty())
            && self.snapshot_capture_pending()
    }

    pub(crate) fn is_current_param_snapshot(&self, state: &Arc<[u8]>) -> bool {
        !self.needs_snapshot()
            && !self.is_macro_snapshot(state)
            && self
                .last_state
                .as_ref()
                .is_some_and(|last| Arc::ptr_eq(&last.bytes, state))
    }

    pub(crate) fn needs_snapshot(&self) -> bool {
        self.user_edit
            || self.pending_restore.is_some()
            || self.pending_marker_edit.is_some()
            || self.pending_preset.is_some()
    }

    pub(crate) fn awaits_snapshot(&self) -> bool {
        self.can_snapshot()
            || !matches!(self.status, DeviceStatus::Crashed(_)) && self.needs_snapshot()
    }

    pub(crate) fn can_snapshot(&self) -> bool {
        self.state_warning.is_none() && self.status == DeviceStatus::Ok
    }

    pub(crate) fn capture(&mut self) -> Option<SavedPluginState> {
        if !self.can_snapshot()
            || self.pending_restore.is_some()
            || self.pending_marker_edit.is_some()
            || self.pending_preset.is_some()
            || !self.pending_writes.is_empty()
        {
            return None;
        }
        let base = &self.base;
        let macros = &self.macro_params;
        let automated = &self.automated_params;
        let unconfirmed = &self.unconfirmed_writes;
        let released = &self.released_automation;
        let (state, corrections) = self
            .session
            .try_control(|plugin| {
                let mut corrections = BTreeMap::new();
                for (&id, &base) in base {
                    let expected = automated
                        .get(&id)
                        .copied()
                        .flatten()
                        .or_else(|| macros.get(&id).copied())
                        .unwrap_or(base);
                    let actual = plugin.param_value(id)?;
                    if !actual.is_finite() || !expected.is_finite() {
                        return None;
                    }
                    if (actual - expected).abs() <= 1e-6 * expected.abs().max(1.0) {
                        continue;
                    }
                    // Do not acknowledge a persistent hand edit until the processor received
                    // it. Differences on playback-owned parameters have an authoritative base
                    // that the project stores next to the opaque state.
                    if unconfirmed.contains(&id)
                        || !(automated.contains_key(&id)
                            || macros.contains_key(&id)
                            || released.contains(&id))
                    {
                        return None;
                    }
                    corrections.insert(id, expected);
                }
                if corrections.len() > MAX_STATE_PARAMETER_BASES {
                    return None;
                }
                Some((plugin.save_state().ok()?, corrections))
            })
            .flatten()?;
        let captured = SavedPluginState {
            bytes: state.into(),
            parameter_bases: corrections,
        };
        self.unconfirmed_writes.clear();
        self.macro_snapshot = (!self.user_edit && !self.macro_params.is_empty())
            .then(|| Arc::downgrade(&captured.bytes));
        self.user_edit = false;
        self.snapshot_due = None;
        self.last_state = Some(captured.clone());
        Some(captured)
    }
}
