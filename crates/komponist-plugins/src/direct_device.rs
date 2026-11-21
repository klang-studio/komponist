//! Control-plane state for an opted-in direct CLAP device.

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use komponist_hosting::host_loop::notification_bits;
use komponist_hosting::{HostedPlugin, HostingError};
use komponist_ipc::native_layout::NativeLayout;
use komponist_ipc::{LogLevel, NoteName, ParamInfo, ParamValue, RenderMode};
use komponist_model::{DeviceId, DeviceRole};

use crate::bundled_clap::VerifiedClap;
use crate::direct::DirectClap;
use crate::direct_session::{DirectEngine, DirectSession};
use crate::manager::{
    DeviceStatus, FailReason, PluginManagerEvent, ProcessorSetup, STATE_LOG_PREFIX,
};

pub(crate) struct DirectDevice {
    session: DirectSession,
    pub(crate) params: Vec<ParamInfo>,
    pub(crate) sampler_ui: Option<komponist_ipc::sampler_ui::Snapshot>,
    sampler_ui_at: Instant,
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
    pending_restore: Option<Arc<[u8]>>,
    macro_params: BTreeMap<u32, f64>,
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
    pub(crate) fn start(
        proof: VerifiedClap,
        role: DeviceRole,
        setup: ProcessorSetup,
        state: Option<Arc<[u8]>>,
        debounce: Duration,
    ) -> Result<(Self, DirectEngine), HostingError> {
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
            params: Vec::new(),
            sampler_ui: None,
            sampler_ui_at: Instant::now(),
            layout: None,
            values: BTreeMap::new(),
            base: BTreeMap::new(),
            last_state: state,
            state_warning: warning,
            note_names: Vec::new(),
            sidechain,
            status: DeviceStatus::Ok,
            pending_writes: BTreeMap::new(),
            pending_restore: None,
            macro_params: BTreeMap::new(),
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

    pub(crate) fn poll_sampler_ui(&mut self) {
        let now = Instant::now();
        if self.pending_restore.is_none() && now >= self.sampler_ui_at {
            self.sampler_ui_at = now + crate::bundled_sampler::UI_REFRESH;
            self.sampler_ui = self.session.sampler_ui();
        }
    }

    pub(crate) fn preview_sampler_slice(&self, time: Option<f64>) {
        self.session.preview_sampler_slice(time);
    }

    pub(crate) fn poll(&mut self, id: DeviceId, events: &mut Vec<PluginManagerEvent>) {
        self.service(id, events);
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
        if observed.has(notification_bits::PARAMS_CHANGED)
            && let Some(inventory) = self.session.try_control(Inventory::read)
        {
            self.accept_inventory(inventory, false);
            Self::report_metadata(id, events);
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

    pub(crate) fn set_macro_params(&mut self, values: BTreeMap<u32, f64>) {
        if self.macro_params == values {
            return;
        }
        let previous = std::mem::replace(&mut self.macro_params, values);
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
        self.sampler_ui = None;
        self.pending_restore = Some(state);
        self.pending_writes.clear();
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

    pub(crate) fn needs_snapshot(&self) -> bool {
        self.user_edit || self.pending_restore.is_some()
    }

    pub(crate) fn awaits_snapshot(&self) -> bool {
        self.can_snapshot()
            || !matches!(self.status, DeviceStatus::Crashed(_)) && self.needs_snapshot()
    }

    pub(crate) fn can_snapshot(&self) -> bool {
        self.state_warning.is_none() && self.status == DeviceStatus::Ok
    }

    pub(crate) fn capture(&mut self) -> Option<Arc<[u8]>> {
        if !self.can_snapshot() || self.pending_restore.is_some() || !self.pending_writes.is_empty()
        {
            return None;
        }
        let base = &self.base;
        let macros = &self.macro_params;
        let captured = self
            .session
            .try_control(|plugin| {
                // A save must not capture a transient automated value or overwrite an edit that
                // is still in flight to the audio processor.
                for (&id, &base) in base {
                    let expected = macros.get(&id).copied().unwrap_or(base);
                    let actual = plugin.param_value(id)?;
                    if !actual.is_finite()
                        || (actual - expected).abs() > 1e-6 * expected.abs().max(1.0)
                    {
                        return None;
                    }
                }
                plugin.save_state().ok().map(Arc::<[u8]>::from)
            })
            .flatten()?;
        self.macro_snapshot =
            (!self.user_edit && !self.macro_params.is_empty()).then(|| Arc::downgrade(&captured));
        self.user_edit = false;
        self.snapshot_due = None;
        self.last_state = Some(captured.clone());
        Some(captured)
    }
}
