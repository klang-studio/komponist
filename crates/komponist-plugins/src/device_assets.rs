//! App-side state shared by both plugin routes for file slots and render preparation.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_ipc::LogLevel;
use komponist_ipc::device_assets::{
    Binding, Bindings, MAX_SLOTS, OfflineRequest, OfflineStatus, Snapshot, edit_state,
};
use komponist_model::DeviceId;
use komponist_model::plugin_assets::AssetLink;
use komponist_model::project::SavedPluginState;

use crate::manager::PluginManagerEvent;

const REFRESH: Duration = Duration::from_millis(80);

/// A file edit initiated by the user, as distinct from restoring project links.
pub struct AssetEdit {
    pub slot: u32,
    pub attaching: bool,
    pub relinking: bool,
}

impl AssetEdit {
    pub(crate) fn as_wire_edit(&self) -> edit_state::Edit {
        let intent = if self.relinking {
            edit_state::Intent::Relink
        } else if self.attaching {
            edit_state::Intent::Assign
        } else {
            edit_state::Intent::Clear
        };
        edit_state::Edit {
            slot: self.slot,
            intent,
        }
    }
}

/// Direct and compatibility edits complete in the caller. The bridge replies on its next polls.
pub enum AssetEditPreparation {
    Ready(Option<SavedPluginState>),
    Pending,
}

#[derive(Debug)]
pub(crate) struct AssetEditState {
    pub source: Option<Arc<[u8]>>,
    pub parameter_bases: BTreeMap<u32, f64>,
    pub trim_changed: bool,
}

impl AssetEditState {
    pub(crate) fn finish(
        self,
        prepared: edit_state::Prepared,
    ) -> Result<Option<SavedPluginState>, edit_state::Rejected> {
        let replacement = prepared?;
        let bytes = match replacement {
            Some(bytes) if edit_state::valid_state(&bytes) => Arc::from(bytes),
            Some(_) => return Err(edit_state::Rejected),
            None if self.trim_changed => self.source.ok_or(edit_state::Rejected)?,
            None => return Ok(None),
        };
        Ok(Some(SavedPluginState {
            bytes,
            parameter_bases: self.parameter_bases,
        }))
    }
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
    pub supports_edit_state: bool,
    file_slots: Option<bool>,
    slot_poll_at: Instant,
    desired_bindings: BTreeMap<u32, Binding>,
    pending_bindings: BTreeMap<u32, Binding>,
    applying_bindings: Vec<u32>,
    binding_snapshot_pending: bool,
    rejected_bindings: BTreeSet<u32>,
    offline_request: Option<OfflineRequest>,
    offline_work_id: u32,
    offline_status: Option<OfflineStatus>,
    clear_offline: bool,
    restoring: bool,
    offline_poll_at: Instant,
}

impl DeviceAssets {
    pub(crate) fn new() -> Self {
        let now = Instant::now();
        Self {
            snapshot: None,
            supports_offline: false,
            supports_edit_state: false,
            file_slots: None,
            slot_poll_at: now,
            desired_bindings: BTreeMap::new(),
            pending_bindings: BTreeMap::new(),
            applying_bindings: Vec::new(),
            binding_snapshot_pending: false,
            rejected_bindings: BTreeSet::new(),
            offline_request: None,
            offline_work_id: 0,
            offline_status: None,
            clear_offline: false,
            restoring: false,
            offline_poll_at: now,
        }
    }

    pub(crate) fn inventory_pending(&self) -> bool {
        self.file_slots != Some(false) && self.snapshot.is_none()
    }

    pub(crate) fn slots_due(&mut self) -> bool {
        let now = Instant::now();
        if self.file_slots != Some(false) && now >= self.slot_poll_at {
            self.slot_poll_at = now + REFRESH;
            true
        } else {
            false
        }
    }

    pub(crate) fn report_slots(
        &mut self,
        snapshot: Option<Snapshot>,
        file_slots: bool,
        offline: bool,
        edit_state: bool,
    ) {
        self.file_slots = Some(file_slots);
        if !file_slots {
            self.snapshot = None;
            self.binding_snapshot_pending = false;
        } else if let Some(snapshot) = snapshot.filter(|snapshot| !snapshot.slots.is_empty()) {
            self.snapshot = Some(snapshot);
            self.binding_snapshot_pending = false;
        }
        self.supports_offline = offline;
        self.supports_edit_state = file_slots && edit_state;
    }

    pub(crate) fn queue_bindings(&mut self, bindings: &Bindings) -> bool {
        if !bindings.valid() || self.file_slots == Some(false) {
            return false;
        }
        let new = bindings
            .slots
            .iter()
            .filter(|binding| !self.desired_bindings.contains_key(&binding.id))
            .count();
        if self.desired_bindings.len() + new > MAX_SLOTS {
            return false;
        }
        for binding in &bindings.slots {
            self.pending_bindings.insert(binding.id, binding.clone());
            self.desired_bindings.insert(
                binding.id,
                Binding {
                    reload: false,
                    ..binding.clone()
                },
            );
        }
        self.offline_status = self.offline_request.map(|_| OfflineStatus::Preparing);
        true
    }

    pub(crate) fn take_bindings(&mut self) -> Option<Bindings> {
        if self.pending_bindings.is_empty() || !self.applying_bindings.is_empty() {
            return None;
        }
        self.slot_poll_at = Instant::now();
        self.applying_bindings = self.pending_bindings.keys().copied().collect();
        Some(Bindings {
            slots: std::mem::take(&mut self.pending_bindings)
                .into_values()
                .collect(),
        })
    }

    pub(crate) fn bindings_pending(&self) -> bool {
        !self.pending_bindings.is_empty() || !self.applying_bindings.is_empty()
    }

    pub(crate) fn render_bindings_pending(&self) -> bool {
        self.bindings_pending()
            || self.binding_snapshot_pending
            || self.restoring && !self.desired_bindings.is_empty()
    }

    pub(crate) fn report_bindings(&mut self, accepted: bool) {
        // The acknowledgement can arrive one poll before the post-binding inventory.
        self.binding_snapshot_pending |= accepted && !self.applying_bindings.is_empty();
        for slot in self.applying_bindings.drain(..) {
            if accepted {
                self.rejected_bindings.remove(&slot);
            } else {
                self.rejected_bindings.insert(slot);
            }
        }
    }

    pub(crate) fn begin_restore(&mut self) {
        self.restoring = true;
        if self.offline_request.is_some() {
            self.advance_offline_work();
            self.clear_offline = true;
            self.offline_status = Some(OfflineStatus::Preparing);
        }
    }

    pub(crate) fn finish_restore(&mut self) {
        if !self.restoring {
            return;
        }
        self.restoring = false;
        for (&slot, binding) in &self.desired_bindings {
            // Keep a newer pending update, including an explicit reload, when present.
            self.pending_bindings
                .entry(slot)
                .or_insert_with(|| binding.clone());
        }
        self.slot_poll_at = Instant::now();
        self.offline_poll_at = self.slot_poll_at;
    }

    pub(crate) fn offline_status(&self) -> Option<OfflineStatus> {
        if !self.supports_offline || self.offline_request.is_none() {
            return None;
        }
        if self.restoring || self.clear_offline || self.bindings_pending() {
            Some(OfflineStatus::Preparing)
        } else if !self.rejected_bindings.is_empty() {
            Some(OfflineStatus::Failed)
        } else {
            self.offline_status
        }
    }

    pub(crate) fn set_offline(&mut self, request: Option<OfflineRequest>) {
        if request == self.offline_request {
            return;
        }
        self.offline_request = request;
        if request.is_some() {
            self.advance_offline_work();
        }
        self.offline_status = request.map(|_| OfflineStatus::Preparing);
        self.clear_offline |= request.is_none();
        self.offline_poll_at = Instant::now();
    }

    fn advance_offline_work(&mut self) {
        self.offline_work_id = self.offline_work_id.wrapping_add(1).max(1);
    }

    fn offline_work(&self) -> Option<OfflineRequest> {
        self.offline_request.map(|request| OfflineRequest {
            id: self.offline_work_id,
            ..request
        })
    }

    pub(crate) fn offline_due(&mut self) -> Option<Option<OfflineRequest>> {
        let now = Instant::now();
        if self.supports_offline
            && !self.restoring
            && !self.bindings_pending()
            && (self.clear_offline || self.offline_status() == Some(OfflineStatus::Preparing))
            && now >= self.offline_poll_at
        {
            self.offline_poll_at = now + REFRESH;
            Some(if self.clear_offline {
                None
            } else {
                self.offline_work()
            })
        } else {
            None
        }
    }

    pub(crate) fn report_offline(
        &mut self,
        request: Option<OfflineRequest>,
        status: Option<OfflineStatus>,
    ) {
        if self.restoring {
            return;
        }
        if request.is_none() && self.clear_offline {
            self.clear_offline = status == Some(OfflineStatus::Preparing);
            if status == Some(OfflineStatus::Ready) {
                self.offline_poll_at = Instant::now();
            } else if !self.clear_offline {
                self.offline_status = self.offline_request.map(|_| OfflineStatus::Failed);
            }
        } else if request.is_some()
            && request == self.offline_work()
            && !self.clear_offline
            && !self.bindings_pending()
            && self.rejected_bindings.is_empty()
        {
            self.offline_status = Some(status.unwrap_or(OfflineStatus::Failed));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_ipc::device_assets::{AssetKind, AssetStatus, Slot};

    fn bindings(id: u32) -> Bindings {
        Bindings {
            slots: vec![Binding {
                id,
                path: None,
                reload: false,
            }],
        }
    }

    #[test]
    fn retries_a_missing_file_slot_snapshot_only_for_supported_plugins() {
        let mut assets = DeviceAssets::new();
        assert!(assets.slots_due());
        assets.report_slots(None, true, true, false);
        assert!(assets.inventory_pending());
        assert!(assets.queue_bindings(&bindings(0)));
        assets.slot_poll_at = Instant::now();
        assert!(assets.slots_due());

        let snapshot = Snapshot {
            slots: vec![Slot {
                id: 0,
                role: "file".into(),
                label: "File".into(),
                kind: AssetKind::File,
                status: AssetStatus::Empty,
            }],
        };
        assets.report_slots(Some(snapshot.clone()), true, true, false);
        assets.report_slots(None, true, true, false);
        assert!(!assets.inventory_pending());
        assert_eq!(assets.snapshot, Some(snapshot));

        let mut unsupported = DeviceAssets::new();
        unsupported.report_slots(None, false, true, false);
        assert!(!unsupported.inventory_pending());
        assert!(!unsupported.slots_due());
        assert!(!unsupported.queue_bindings(&bindings(0)));
    }

    #[test]
    fn render_waits_for_a_fresh_inventory_after_the_binding_acknowledgement() {
        let mut assets = DeviceAssets::new();
        let snapshot = |status| Snapshot {
            slots: vec![Slot {
                id: 0,
                role: "sample".into(),
                label: "Sample".into(),
                kind: AssetKind::AudioSample,
                status,
            }],
        };
        assets.report_slots(Some(snapshot(AssetStatus::Ready)), true, true, false);
        assert!(!assets.render_bindings_pending());
        assert!(assets.queue_bindings(&bindings(0)));
        assert!(assets.render_bindings_pending());
        assert!(assets.take_bindings().is_some());
        assets.report_slots(Some(snapshot(AssetStatus::Ready)), true, true, false);
        assert!(assets.render_bindings_pending());

        assets.report_bindings(true);
        assert!(!assets.bindings_pending());
        assert!(!assets.inventory_pending());
        assert!(assets.render_bindings_pending());
        assets.report_slots(None, true, true, false);
        assert!(assets.render_bindings_pending());

        assets.report_slots(Some(snapshot(AssetStatus::Loading)), true, true, false);
        assert!(!assets.render_bindings_pending());

        assets.begin_restore();
        assert!(assets.render_bindings_pending());
        assets.finish_restore();
        assert!(assets.render_bindings_pending());
    }

    #[test]
    fn restore_replays_latest_bindings_then_retires_and_restarts_offline_work() {
        let mut assets = DeviceAssets::new();
        assets.report_slots(None, true, true, false);
        let load = |id, path: &str, reload| Binding {
            id,
            path: Some(path.into()),
            reload,
        };
        let clear = Binding {
            id: 2,
            path: None,
            reload: false,
        };
        let original = Bindings {
            slots: vec![
                load(0, "/old.wav", true),
                load(1, "/kept.wav", true),
                clear.clone(),
            ],
        };
        assert!(assets.queue_bindings(&original));
        assert_eq!(assets.take_bindings(), Some(original));
        assets.report_bindings(true);

        let request = Some(OfflineRequest {
            id: 1,
            sample_rate: 48_000.0,
            tempo: 120.0,
        });
        assets.set_offline(request);
        let first = assets.offline_due().unwrap();
        assets.report_offline(first, Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Ready));

        assets.begin_restore();
        assets.report_offline(first, Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Preparing));
        assert_eq!(assets.offline_due(), None);
        let replacement = load(0, "/new.wav", true);
        assert!(assets.queue_bindings(&Bindings {
            slots: vec![replacement.clone()]
        }));

        assets.finish_restore();
        assert_eq!(
            assets.take_bindings().unwrap().slots,
            [replacement, load(1, "/kept.wav", false), clear]
        );
        assert_eq!(assets.offline_due(), None);
        assets.report_bindings(true);
        assert_eq!(assets.offline_due(), Some(None));

        assets.report_offline(first, Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Preparing));
        assets.report_offline(None, Some(OfflineStatus::Ready));
        let restarted = assets.offline_due().unwrap().unwrap();
        assert_ne!(Some(restarted), first);
        assert_eq!(
            Some(restarted),
            request.map(|request| OfflineRequest {
                id: restarted.id,
                ..request
            })
        );
        assets.report_offline(first, Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Preparing));
        assets.report_offline(Some(restarted), Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Ready));

        assets.set_offline(request.map(|request| OfflineRequest { id: 2, ..request }));
        let next = assets.offline_due().unwrap().unwrap();
        assert_ne!(next.id, restarted.id);
    }

    #[test]
    fn rejected_bindings_hold_offline_preparation_until_those_slots_are_accepted() {
        let mut assets = DeviceAssets::new();
        assets.report_slots(None, true, true, false);
        let request = Some(OfflineRequest {
            id: 1,
            sample_rate: 48_000.0,
            tempo: 120.0,
        });
        assets.set_offline(request);
        assert!(assets.queue_bindings(&bindings(0)));
        assert_eq!(assets.take_bindings(), Some(bindings(0)));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Preparing));
        assert_eq!(assets.offline_due(), None);

        assets.report_bindings(false);
        assets.report_offline(request, Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Failed));
        assert_eq!(assets.offline_due(), None);

        for slot in [1, 0] {
            assert!(assets.queue_bindings(&bindings(slot)));
            assert_eq!(assets.take_bindings(), Some(bindings(slot)));
            assets.report_bindings(true);
        }
        assert_eq!(assets.offline_due(), Some(request));
        assets.report_offline(request, Some(OfflineStatus::Ready));
        assert_eq!(assets.offline_status(), Some(OfflineStatus::Ready));
    }
}
