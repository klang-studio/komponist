//! A file-slot and asynchronous preparation fixture for the example plug-in.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use komponist_native_clap_sdk::assets::AssetProvider;
use komponist_native_clap_sdk::device_assets::{
    AssetKind, AssetStatus, Bindings, OfflineRequest, OfflineStatus, Slot, Snapshot,
};

use super::GainParams;

const SLOT: u32 = 42;
const PREPARATION: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct FixtureAssets {
    path: Mutex<Option<PathBuf>>,
    offline: Mutex<Option<(OfflineRequest, Instant)>>,
}

impl AssetProvider for GainParams {
    const FILE_SLOTS: bool = true;
    const OFFLINE_PREPARATION: bool = true;

    fn asset_slots(&self) -> Option<Snapshot> {
        let path = self
            .asset_state
            .path
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let status = match path.as_ref() {
            None => AssetStatus::Empty,
            Some(path) if path.is_file() => AssetStatus::Ready,
            Some(_) => AssetStatus::Missing,
        };
        Some(Snapshot {
            slots: vec![Slot {
                id: SLOT,
                role: "reference".into(),
                label: "Reference file".into(),
                kind: AssetKind::File,
                status,
            }],
        })
    }

    fn set_asset_bindings(&self, bindings: Bindings) -> bool {
        let [binding] = bindings.slots.as_slice() else {
            return false;
        };
        if binding.id != SLOT {
            return false;
        }
        let mut path = self
            .asset_state
            .path
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *path = binding.path.as_deref().map(PathBuf::from);
        true
    }

    fn prepare_offline(&self, request: Option<OfflineRequest>) -> OfflineStatus {
        let mut work = self
            .asset_state
            .offline
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(request) = request else {
            *work = None;
            return OfflineStatus::Ready;
        };
        let ready_at = match *work {
            Some((active, deadline)) if active == request => deadline,
            Some((active, _)) if active.id == request.id => return OfflineStatus::Failed,
            _ => {
                let deadline = Instant::now() + PREPARATION;
                *work = Some((request, deadline));
                deadline
            }
        };
        if Instant::now() >= ready_at {
            OfflineStatus::Ready
        } else {
            OfflineStatus::Preparing
        }
    }
}
