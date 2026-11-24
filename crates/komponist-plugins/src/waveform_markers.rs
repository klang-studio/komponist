//! Runtime cache of the markers requested by a device's waveform view.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use komponist_ipc::device_assets::MAX_SLOTS;
use komponist_ipc::waveform_markers::{
    Edit, EditResult, EditStatus, MAX_EDIT_BYTES, Request, Snapshot,
};

const REFRESH: Duration = Duration::from_millis(80);
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// The result of one accepted waveform marker edit request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaveformMarkerEditResult {
    /// The plugin applied and saved the edit. Both states use the plugin's project state format.
    Applied {
        previous_state: Arc<[u8]>,
        state: Arc<[u8]>,
    },
    /// The plugin declined the edit, or the target changed before it could be accepted.
    Rejected(EditStatus),
    /// The host could not complete the edit and capture its state.
    Failed(String),
}

impl WaveformMarkerEditResult {
    pub(crate) fn from_wire(result: EditResult, last_state: &mut Option<Arc<[u8]>>) -> Self {
        match result {
            EditResult::Applied {
                previous_state,
                state,
            } => {
                let state: Arc<[u8]> = state.into();
                *last_state = Some(Arc::clone(&state));
                Self::Applied {
                    previous_state: previous_state.into(),
                    state,
                }
            }
            EditResult::Rejected(status) => Self::Rejected(status),
            EditResult::Failed { message, .. } => Self::Failed(message),
        }
    }
}

#[derive(Debug)]
struct Slot {
    requested: bool,
    refresh_at: Instant,
    current: Option<Current>,
}

#[derive(Debug)]
struct Current {
    provider_revision: u64,
    view: Snapshot,
}

#[derive(Debug, Default)]
pub(crate) struct MarkerCache {
    slots: BTreeMap<u32, Slot>,
}

impl MarkerCache {
    pub(crate) fn request(&mut self, slot: u32) {
        if let Some(entry) = self.slots.get_mut(&slot) {
            entry.requested = true;
        } else if self.slots.len() < MAX_SLOTS {
            self.slots.insert(
                slot,
                Slot {
                    requested: true,
                    refresh_at: Instant::now(),
                    current: None,
                },
            );
        }
    }

    pub(crate) fn poll(&mut self) -> Option<u32> {
        let now = Instant::now();
        let (&slot, entry) = self
            .slots
            .iter_mut()
            .filter(|(_, entry)| entry.requested && now >= entry.refresh_at)
            .min_by_key(|(_, entry)| entry.refresh_at)?;
        entry.requested = false;
        entry.refresh_at = now + REFRESH;
        Some(slot)
    }

    pub(crate) fn invalidate(&mut self) {
        for entry in self.slots.values_mut() {
            entry.current = None;
            entry.refresh_at = Instant::now();
        }
    }

    pub(crate) fn refresh(&mut self, slot: u32) {
        if let Some(entry) = self.slots.get_mut(&slot) {
            entry.current = None;
            entry.refresh_at = Instant::now();
            entry.requested = true;
        }
    }

    pub(crate) fn report(&mut self, slot: u32, snapshot: Option<Snapshot>) {
        let Some(entry) = self.slots.get_mut(&slot) else {
            return;
        };
        entry.current = snapshot.map(|mut view| {
            let provider_revision = view.revision;
            // Provider revisions can repeat across instances. Hold on to a host revision only
            // while this instance still presents the same marker data for this exact asset slot.
            view.revision = entry
                .current
                .as_ref()
                .filter(|previous| {
                    previous.provider_revision == provider_revision
                        && previous.view.positions == view.positions
                        && previous.view.slice_options == view.slice_options
                })
                .map_or_else(
                    || NEXT_REVISION.fetch_add(1, Ordering::Relaxed),
                    |previous| previous.view.revision,
                );
            Current {
                provider_revision,
                view,
            }
        });
    }

    pub(crate) fn snapshot(&self, slot: u32) -> Option<&Snapshot> {
        self.slots
            .get(&slot)?
            .current
            .as_ref()
            .map(|current| &current.view)
    }

    pub(crate) fn checked_edit(
        &self,
        slot: u32,
        mut request: Request,
    ) -> Result<Request, EditStatus> {
        request
            .encode(&mut [0; MAX_EDIT_BYTES])
            .ok_or(EditStatus::Invalid)?;
        let current = self
            .slots
            .get(&slot)
            .and_then(|entry| entry.current.as_ref())
            .ok_or(EditStatus::Unavailable)?;
        if current.view.revision != request.revision {
            return Err(EditStatus::Stale);
        }
        if !current.view.editable {
            return Err(EditStatus::Unavailable);
        }
        match request.operation {
            Edit::SetSliceOptions { slice, .. } => {
                let options = current
                    .view
                    .slice_options
                    .as_ref()
                    .ok_or(EditStatus::Unavailable)?;
                if usize::from(slice) >= options.len() {
                    return Err(EditStatus::Invalid);
                }
            }
            Edit::DetectSlices if current.view.slice_options.is_none() => {
                return Err(EditStatus::Unavailable);
            }
            _ => {}
        }
        request.revision = current.provider_revision;
        Ok(request)
    }
}

#[cfg(test)]
mod tests {
    use komponist_ipc::waveform_markers::SliceOptions;

    use super::*;

    fn snapshot(positions: Vec<f64>) -> Snapshot {
        Snapshot {
            revision: 1,
            editable: true,
            positions,
            slice_options: None,
            default_options: None,
            preparation: None,
        }
    }

    #[test]
    fn an_older_eligible_slot_is_serviced_before_a_repeated_request() {
        let mut cache = MarkerCache::default();
        cache.request(0);
        cache.request(1);
        let now = Instant::now();
        cache.slots.get_mut(&0).unwrap().refresh_at = now;
        cache.slots.get_mut(&1).unwrap().refresh_at = now - REFRESH;
        assert_eq!(cache.poll(), Some(1));
        assert_eq!(cache.poll(), Some(0));
    }

    #[test]
    fn host_revision_tracks_each_runtime_slot_and_instance() {
        let mut initial = MarkerCache::default();
        initial.request(0);
        assert_eq!(initial.poll(), Some(0));
        assert_eq!(initial.poll(), None);
        initial.report(0, Some(snapshot(vec![0.1])));
        let mut edit = Request {
            revision: initial.snapshot(0).unwrap().revision,
            operation: Edit::Remove { index: 0 },
        };
        initial.report(0, Some(snapshot(vec![0.1])));
        assert_eq!(initial.checked_edit(0, edit).unwrap().revision, 1);

        initial.request(1);
        initial.report(1, Some(snapshot(vec![0.1])));
        assert_eq!(initial.checked_edit(1, edit), Err(EditStatus::Stale));

        let mut replacement = MarkerCache::default();
        replacement.request(0);
        replacement.report(0, Some(snapshot(vec![0.1])));
        assert_eq!(replacement.checked_edit(0, edit), Err(EditStatus::Stale));
        edit.revision = replacement.snapshot(0).unwrap().revision;
        replacement.invalidate();
        assert_eq!(
            replacement.checked_edit(0, edit),
            Err(EditStatus::Unavailable)
        );
        replacement.report(0, Some(snapshot(vec![0.1])));
        assert_eq!(replacement.checked_edit(0, edit), Err(EditStatus::Stale));
    }

    #[test]
    fn slice_option_edits_require_current_bounds_and_capability() {
        let mut cache = MarkerCache::default();
        cache.request(0);
        let mut view = snapshot(vec![0.1]);
        cache.report(0, Some(view.clone()));
        let edit = |cache: &MarkerCache, operation| {
            cache.checked_edit(
                0,
                Request {
                    revision: cache.snapshot(0).unwrap().revision,
                    operation,
                },
            )
        };
        assert_eq!(
            edit(&cache, Edit::DetectSlices),
            Err(EditStatus::Unavailable)
        );
        let inherited = SliceOptions {
            reverse: Some(false),
            fade_in_ms: Some(0.0),
            fade_out_ms: Some(0.0),
            ..Default::default()
        };
        view.slice_options = Some(vec![SliceOptions::default(); 2]);
        view.default_options = Some(inherited);
        cache.report(0, Some(view.clone()));
        let set = |slice| Edit::SetSliceOptions {
            slice,
            options: SliceOptions::default(),
        };
        assert_eq!(edit(&cache, set(2)), Err(EditStatus::Invalid));
        assert_eq!(edit(&cache, set(1)).unwrap().revision, view.revision);
        let revision = cache.snapshot(0).unwrap().revision;
        view.slice_options.as_mut().unwrap()[0].gate = true;
        cache.report(0, Some(view));
        assert_eq!(
            cache.checked_edit(
                0,
                Request {
                    revision,
                    operation: set(0)
                }
            ),
            Err(EditStatus::Stale)
        );
    }

    #[test]
    fn unavailable_and_invalid_edits_do_not_reach_the_provider() {
        let mut cache = MarkerCache::default();
        cache.request(0);
        cache.report(
            0,
            Some(Snapshot {
                editable: false,
                ..snapshot(vec![0.1])
            }),
        );
        let revision = cache.snapshot(0).unwrap().revision;
        assert_eq!(
            cache.checked_edit(
                0,
                Request {
                    revision,
                    operation: Edit::Remove { index: 0 },
                }
            ),
            Err(EditStatus::Unavailable)
        );
        assert_eq!(
            cache.checked_edit(
                0,
                Request {
                    revision,
                    operation: Edit::Insert { seconds: f64::NAN },
                }
            ),
            Err(EditStatus::Invalid)
        );
    }
}
