use std::path::Path;

use komponist_ipc::device_assets::{
    AssetKind, AssetStatus, Bindings, MAX_ERROR_BYTES, OfflineRequest, OfflineStatus, Slot,
    Snapshot,
};
use komponist_native_clap_sdk::assets::AssetProvider;
use komponist_sampler_shared::{PadStatus, SyncStatus, links::SampleLink};

use super::{SampleParameters, SampledDefinition};

impl<T: SampledDefinition> AssetProvider for SampleParameters<T> {
    const FILE_SLOTS: bool = true;
    const OFFLINE_PREPARATION: bool = true;

    fn asset_slots(&self) -> Option<Snapshot> {
        Some(Snapshot {
            slots: self
                .samples
                .views()
                .into_iter()
                .take(T::PADS)
                .enumerate()
                .map(|(id, view)| Slot {
                    id: id as u32,
                    role: "sample".into(),
                    label: if T::PADS == 1 {
                        "Sample".into()
                    } else {
                        format!("Pad {}", id + 1)
                    },
                    kind: AssetKind::AudioSample,
                    status: match view.status {
                        PadStatus::Empty => AssetStatus::Empty,
                        PadStatus::Loading => AssetStatus::Loading,
                        PadStatus::Ready => AssetStatus::Ready,
                        PadStatus::Missing => AssetStatus::Missing,
                        PadStatus::Failed(reason) => {
                            let reason: String = reason
                                .chars()
                                .map(|character| {
                                    if character.is_control() {
                                        ' '
                                    } else {
                                        character
                                    }
                                })
                                .collect();
                            let reason = reason.trim();
                            let reason = if reason.is_empty() {
                                "Decode failed"
                            } else {
                                reason
                            };
                            AssetStatus::Failed(
                                reason[..reason.floor_char_boundary(MAX_ERROR_BYTES)].into(),
                            )
                        }
                    },
                })
                .collect(),
        })
    }

    fn set_asset_bindings(&self, bindings: Bindings) -> bool {
        if bindings
            .slots
            .iter()
            .any(|binding| binding.id as usize >= T::PADS)
        {
            return false;
        }
        let mut current = self
            .host_bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for binding in bindings.slots {
            let pad = binding.id as usize;
            let link = binding
                .path
                .as_deref()
                .map(|path| SampleLink::new(Path::new(path), None));
            if !binding.reload && current.get(&pad) == Some(&link) {
                continue;
            }
            if let Some(link) = &link {
                self.samples.load_link(pad, link.clone(), None);
            } else {
                self.samples.clear(pad);
            }
            current.insert(pad, link);
        }
        true
    }

    fn prepare_offline(&self, request: Option<OfflineRequest>) -> OfflineStatus {
        let mut current = self
            .offline
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let Some(request) = request else {
            *current = None;
            return OfflineStatus::Ready;
        };
        if let Some(previous) = *current
            && previous.id == request.id
            && previous != request
        {
            return OfflineStatus::Failed;
        }
        if *current != Some(request) {
            let plain = |id| self.params.plain_value_by_id(id);
            if let Some((source_bpm, sync)) = plain("source_bpm").zip(plain("sync")) {
                self.samples
                    .prepare_general_sync(source_bpm, request.tempo, sync >= 0.5);
            }
            *current = Some(request);
        }
        if !self.samples.is_idle() {
            return OfflineStatus::Preparing;
        }
        let bindings = self
            .host_bindings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.samples.sync_status() == Some(SyncStatus::Unavailable)
            || bindings.iter().any(|(&pad, link)| {
                link.is_some()
                    && matches!(
                        self.samples.view(pad).status,
                        PadStatus::Missing | PadStatus::Failed(_)
                    )
            })
        {
            OfflineStatus::Failed
        } else {
            OfflineStatus::Ready
        }
    }
}
