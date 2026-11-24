//! App-side state shared by both plugin routes for file slots and render preparation.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use komponist_ipc::LogLevel;
use komponist_ipc::device_assets::{
    Binding, Bindings, MAX_SLOTS, OfflineRequest, OfflineStatus, Snapshot,
};
use komponist_model::DeviceId;
use komponist_model::plugin_assets::AssetLink;

use crate::manager::PluginManagerEvent;

const REFRESH: Duration = Duration::from_millis(80);

/// A file edit initiated by the user, as distinct from restoring project links.
pub struct AssetEdit {
    pub slot: u32,
    pub attaching: bool,
    pub relinking: bool,
}

pub(crate) fn binding_rejected(device: DeviceId) -> PluginManagerEvent {
    PluginManagerEvent::Log {
        device: Some(device),
        level: LogLevel::Warning,
        message: "Plugin rejected a file-slot binding update".into(),
    }
}

pub(crate) fn project_bindings(
    before: &BTreeMap<u32, AssetLink>,
    after: &BTreeMap<u32, AssetLink>,
    dir: Option<&Path>,
) -> Result<Option<Bindings>, ()> {
    let mut slots = Vec::new();
    for (&id, link) in after {
        let absolute = link.runtime_path(dir).ok_or(())?;
        let path = absolute.to_str().ok_or(())?.to_owned();
        slots.push(Binding {
            id,
            path: Some(path),
            reload: false,
        });
    }
    slots.extend(
        before
            .keys()
            .filter(|id| !after.contains_key(id))
            .map(|&id| Binding {
                id,
                path: None,
                reload: false,
            }),
    );
    if slots.is_empty() {
        return Ok(None);
    }
    let bindings = Bindings { slots };
    bindings.valid().then_some(Some(bindings)).ok_or(())
}

#[derive(Debug)]
pub(crate) struct DeviceAssets {
    pub snapshot: Option<Snapshot>,
    pub supports_offline: bool,
    discovered: bool,
    has_slots: bool,
    slot_poll_at: Instant,
    pending_bindings: BTreeMap<u32, Binding>,
    offline_request: Option<OfflineRequest>,
    pub offline_status: Option<OfflineStatus>,
    clear_offline: bool,
    offline_poll_at: Instant,
}

impl DeviceAssets {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            snapshot: None,
            supports_offline: false,
            discovered: false,
            has_slots: false,
            slot_poll_at: now,
            pending_bindings: BTreeMap::new(),
            offline_request: None,
            offline_status: None,
            clear_offline: false,
            offline_poll_at: now,
        }
    }

    pub(crate) fn slots_due(&mut self) -> bool {
        let now = Instant::now();
        if (!self.discovered || self.has_slots) && now >= self.slot_poll_at {
            self.slot_poll_at = now + REFRESH;
            true
        } else {
            false
        }
    }

    pub(crate) fn report_slots(&mut self, snapshot: Option<Snapshot>, offline: bool) {
        self.discovered = true;
        self.has_slots |= snapshot.is_some();
        self.snapshot = snapshot;
        self.supports_offline = offline;
    }

    pub(crate) fn queue_bindings(&mut self, bindings: &Bindings) -> bool {
        if !bindings.valid() || (self.discovered && !self.has_slots) {
            return false;
        }
        let new = bindings
            .slots
            .iter()
            .filter(|binding| !self.pending_bindings.contains_key(&binding.id))
            .count();
        if self.pending_bindings.len() + new > MAX_SLOTS {
            return false;
        }
        self.pending_bindings.extend(
            bindings
                .slots
                .iter()
                .map(|binding| (binding.id, binding.clone())),
        );
        true
    }

    pub(crate) fn take_bindings(&mut self) -> Option<Bindings> {
        if self.pending_bindings.is_empty() {
            return None;
        }
        self.slot_poll_at = Instant::now();
        Some(Bindings {
            slots: std::mem::take(&mut self.pending_bindings)
                .into_values()
                .collect(),
        })
    }

    pub(crate) fn bindings_pending(&self) -> bool {
        !self.pending_bindings.is_empty()
    }

    pub(crate) fn set_offline(&mut self, request: Option<OfflineRequest>) {
        if request == self.offline_request {
            return;
        }
        self.offline_request = request;
        self.offline_status = request.map(|_| OfflineStatus::Preparing);
        self.clear_offline = request.is_none();
        self.offline_poll_at = Instant::now();
    }

    pub(crate) fn offline_due(&mut self) -> Option<Option<OfflineRequest>> {
        let now = Instant::now();
        if self.supports_offline
            && (self.clear_offline || self.offline_status == Some(OfflineStatus::Preparing))
            && now >= self.offline_poll_at
        {
            self.offline_poll_at = now + REFRESH;
            Some(self.offline_request)
        } else {
            None
        }
    }

    pub(crate) fn report_offline(
        &mut self,
        request: Option<OfflineRequest>,
        status: Option<OfflineStatus>,
    ) {
        if request == self.offline_request {
            if request.is_none() {
                self.clear_offline = status == Some(OfflineStatus::Preparing);
            } else {
                self.offline_status = Some(status.unwrap_or(OfflineStatus::Failed));
            }
        }
    }
}
