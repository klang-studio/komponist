//! Control-plane state for an opted-in direct CLAP device.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use komponist_builtin::BuiltinKind;
use komponist_hosting::host_loop::notification_bits;
use komponist_hosting::{HostedPlugin, HostingError};
use komponist_ipc::native_layout::NativeLayout;
use komponist_ipc::waveform_markers::{EditResult, EditStatus, Request};
use komponist_ipc::{LogLevel, NoteName, ParamInfo, ParamValue, RenderMode};
use komponist_model::{DeviceId, DeviceRole};

use crate::bundled_clap::VerifiedClap;
use crate::bundled_state::{self, MarkerParameters, ParameterState};
use crate::direct::DirectClap;
use crate::direct_session::{DirectEngine, DirectSession};
use crate::manager::{
    DeviceStatus, FailReason, PluginManagerEvent, ProcessorSetup, STATE_LOG_PREFIX,
};

use crate::waveform_markers::{MarkerCache, WaveformMarkerEditResult};

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
    bundled_kind: Option<BuiltinKind>,
    pub(crate) params: Vec<ParamInfo>,
    pub(crate) markers: MarkerCache,
    pub(crate) layout: Option<Arc<NativeLayout>>,
    pub(crate) values: BTreeMap<u32, ParamValue>,
    pub(crate) base: BTreeMap<u32, f64>,
    last_state: Option<Arc<[u8]>>,
    pub(crate) state_warning: Option<String>,
    pub(crate) note_names: Vec<NoteName>,
    pub(crate) sidechain: bool,
    pub(crate) latency: u32,
    pub(crate) status: DeviceStatus,
    pending_writes: BTreeMap<u32, f64>,
    /// Persistent edits that still need to reach the plugin before a snapshot can replace state.
    unconfirmed_writes: BTreeSet<u32>,
    pending_restore: Option<Arc<[u8]>>,
    pending_marker_edit: Option<PendingMarkerEdit>,
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

impl DirectDevice {
    pub(crate) fn signal_tap(&self, key: &str) -> Option<&crate::signal_taps::SignalTap> {
        self.session.signal_tap(key)
    }

    pub(crate) fn start(
        proof: VerifiedClap,
        role: DeviceRole,
        setup: ProcessorSetup,
        state: Option<Arc<[u8]>>,
        debounce: Duration,
    ) -> Result<(Self, DirectEngine), HostingError> {
        let bundled_kind = bundled_state::kind_for_id(proof.plugin_id());
        let mut host = DirectClap::load(proof)?;
        let (warning, inventory, sidechain) = host
            .try_control(|plugin| {
                let warning = state.as_ref().and_then(|state| {
                    plugin
                        .load_state(state)
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
            bundled_kind,
            params: Vec::new(),
            assets: crate::device_assets::DeviceAssets::new(),
            markers: MarkerCache::default(),
            layout: None,
            values: BTreeMap::new(),
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

    pub(crate) fn invalidate_waveform_markers(&mut self) {
        self.pending_marker_edit = None;
        self.markers.invalidate();
    }

    fn can_edit_waveform_markers(&self) -> bool {
        self.can_snapshot()
            && self.pending_restore.is_none()
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
            self.parameter_state(),
        )
    }

    fn parameter_state(&self) -> Option<ParameterState<'_>> {
        Some(ParameterState {
            kind: self.bundled_kind?,
            layout: self.layout.as_deref()?,
        })
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
        let result = edit.params.normalize(result);
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
        let result = WaveformMarkerEditResult::from_wire(result, &mut self.last_state);
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

    pub(crate) fn poll_assets(&mut self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        if self.pending_restore.is_some() || matches!(self.status, DeviceStatus::Crashed(_)) {
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
            let (slots, file_slots, offline) = self.session.asset_slots();
            self.assets.report_slots(slots, file_slots, offline);
        }
        if let Some(request) = self.assets.offline_due() {
            let status = self.session.prepare_offline(request);
            self.assets.report_offline(request, status);
        }
    }

    pub(crate) fn poll(&mut self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        self.service(id, events);
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
                self.status = DeviceStatus::Crashed(FailReason::LoadFailed(error.to_string()));
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
        if let Some(state) = self.pending_restore.clone()
            && let Some((result, inventory)) = self.session.restore(&state, Inventory::read)
        {
            self.pending_restore = None;
            self.assets.finish_restore();
            self.last_state = Some(state);
            self.state_warning = result.err().map(|error| error.to_string());
            self.report_warning(id, events);
            self.accept_inventory(inventory, true);
            Self::report_metadata(id, events);
            if self.state_warning.is_none() {
                events.push(PluginManagerEvent::StateApplied { device: id });
            }
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
        if observed.has(notification_bits::STATE_DIRTY) {
            self.user_edit = true;
            self.snapshot_due = Some(Instant::now() + self.debounce);
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

    pub(crate) fn edited_param(&mut self, id: u32, value: f64) -> bool {
        if !value.is_finite() || !self.base.contains_key(&id) {
            return false;
        }
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
        true
    }

    pub(crate) fn preview_param(&mut self, id: u32, value: f64) {
        if let Some(previous) = self.values.get_mut(&id)
            && previous.value != value
        {
            previous.value = value;
            previous.text.clear();
        }
    }

    pub(crate) fn set_param(&mut self, id: u32, value: f64) {
        if self.edited_param(id, value) {
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

    pub(crate) fn set_state(&mut self, state: Arc<[u8]>) {
        self.invalidate_waveform_markers();
        self.assets.begin_restore();
        self.pending_restore = Some(state);
        self.pending_writes.clear();
        self.unconfirmed_writes.clear();
        self.released_automation.clear();
        self.snapshot_due = None;
        self.user_edit = false;
    }

    pub(crate) fn state_for_reload(&self) -> Option<Arc<[u8]>> {
        self.pending_restore
            .clone()
            .or_else(|| self.last_state.clone())
    }

    pub(crate) fn request_state(&mut self) {
        if self.state_warning.is_none() {
            self.snapshot_due = Some(Instant::now());
        }
    }

    pub(crate) fn is_current_param_snapshot(&self, state: &Arc<[u8]>) -> bool {
        !self.needs_snapshot()
            && !self.is_macro_snapshot(state)
            && self
                .last_state
                .as_ref()
                .is_some_and(|last| Arc::ptr_eq(last, state))
    }

    pub(crate) fn needs_snapshot(&self) -> bool {
        self.user_edit || self.pending_restore.is_some() || self.pending_marker_edit.is_some()
    }

    pub(crate) fn awaits_snapshot(&self) -> bool {
        self.can_snapshot()
            || !matches!(self.status, DeviceStatus::Crashed(_)) && self.needs_snapshot()
    }

    pub(crate) fn can_snapshot(&self) -> bool {
        self.state_warning.is_none() && self.status == DeviceStatus::Ok
    }

    pub(crate) fn capture(&mut self) -> Option<Arc<[u8]>> {
        if !self.can_snapshot()
            || self.pending_restore.is_some()
            || self.pending_marker_edit.is_some()
            || !self.pending_writes.is_empty()
        {
            return None;
        }
        let base = &self.base;
        let macros = &self.macro_params;
        let automated = &self.automated_params;
        let unconfirmed = &self.unconfirmed_writes;
        let released = &self.released_automation;
        let can_restore_bases = self.parameter_state().is_some();
        let (state, corrections) = self
            .session
            .try_control(|plugin| {
                let mut corrections = Vec::new();
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
                    // and can be repaired on the detached copy of the known bundle's state.
                    if unconfirmed.contains(&id)
                        || !can_restore_bases
                        || !(automated.contains_key(&id)
                            || macros.contains_key(&id)
                            || released.contains(&id))
                    {
                        return None;
                    }
                    corrections.push((id, expected));
                }
                Some((plugin.save_state().ok()?, corrections))
            })
            .flatten()?;
        let captured: Arc<[u8]> = if corrections.is_empty() {
            state.into()
        } else {
            self.parameter_state()?
                .restore_param_bases(&state, corrections)?
                .into()
        };
        self.unconfirmed_writes.clear();
        self.macro_snapshot =
            (!self.user_edit && !self.macro_params.is_empty()).then(|| Arc::downgrade(&captured));
        self.user_edit = false;
        self.snapshot_due = None;
        self.last_state = Some(captured.clone());
        Some(captured)
    }
}
