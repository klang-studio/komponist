//! Runtime cache of the markers requested by a device's waveform view.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use komponist_ipc::control::MAX_STATE_PARAMETER_BASES;
use komponist_model::project::SavedPluginState;
use komponist_plugin_api::device_assets::MAX_SLOTS;
use komponist_plugin_api::native_layout::NativeLayout;
use komponist_plugin_api::params::ParamInfo;
use komponist_plugin_api::params::param_flags;
use komponist_plugin_api::waveform_markers::{
    Edit, EditResult, EditStatus, MAX_EDIT_BYTES, Request, Snapshot,
};

const REFRESH: Duration = Duration::from_millis(80);
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

/// The result of one accepted waveform marker edit request.
#[derive(Clone, Debug, PartialEq)]
pub enum WaveformMarkerEditResult {
    /// The plugin applied and saved the edit. Both states use the plugin's project state format
    /// and carry the host-owned bases of parameters that automation or macros drive.
    Applied {
        previous_state: SavedPluginState,
        state: SavedPluginState,
    },
    /// The plugin declined the edit, or the target changed before it could be accepted.
    Rejected(EditStatus),
    /// The host could not complete the edit and capture its state.
    Failed(String),
}

impl WaveformMarkerEditResult {
    pub(crate) fn from_wire(
        result: EditResult,
        bases: &BTreeMap<u32, f64>,
        last_state: &mut Option<SavedPluginState>,
    ) -> Self {
        match result {
            EditResult::Applied {
                previous_state,
                state,
            } => {
                let saved = |bytes: Vec<u8>| SavedPluginState {
                    bytes: Arc::from(bytes),
                    parameter_bases: bases.clone(),
                };
                let state = saved(state);
                *last_state = Some(state.clone());
                Self::Applied {
                    previous_state: saved(previous_state),
                    state,
                }
            }
            EditResult::Rejected(status) => Self::Rejected(status),
            EditResult::Failed { message, .. } => Self::Failed(message),
        }
    }
}

/// Whether a layout edits waveform markers. Persistent parameter edits of such a device share
/// the ordered control stream with marker edits and state restores, so a value still on the
/// render path cannot land after a restore.
pub(crate) fn has_marker_graph(layout: &NativeLayout) -> bool {
    layout
        .pages
        .iter()
        .flat_map(|page| &page.cells)
        .any(|cell| {
            cell.graph
                .as_ref()
                .is_some_and(|graph| graph.waveform_markers)
        })
}

/// Parameter values to verify and persist around one marker edit. Values that automation or
/// macros drive stay host-owned bases next to the captured state, as for any snapshot.
#[derive(Debug, PartialEq)]
pub(crate) struct MarkerParameters {
    pub(crate) expected: Vec<(u32, f64)>,
    bases: BTreeMap<u32, f64>,
}

impl MarkerParameters {
    pub(crate) fn new(
        slot: u32,
        base: &BTreeMap<u32, f64>,
        automated: &BTreeMap<u32, Option<f64>>,
        macros: &BTreeMap<u32, f64>,
        inventory: &[ParamInfo],
        layout: Option<&NativeLayout>,
        retain: impl IntoIterator<Item = u32>,
    ) -> Option<Self> {
        let retained: BTreeSet<_> = retain.into_iter().collect();
        if automated.is_empty() && macros.is_empty() && retained.is_empty() {
            return Some(Self {
                expected: base.iter().map(|(&id, &value)| (id, value)).collect(),
                bases: BTreeMap::new(),
            });
        }

        // Driven values need the layout's marker graph to know which controls affect the edit.
        let marker_graphs: Vec<_> = layout?
            .pages
            .iter()
            .flat_map(|page| &page.cells)
            .filter_map(|cell| cell.graph.as_ref())
            .filter(|graph| graph.waveform_markers && graph.asset_slot == Some(slot))
            .collect();
        if marker_graphs.is_empty() {
            return None;
        }
        // Trim and discrete mode controls can affect which operations the plugin accepts.
        // Leave those controls in the live comparison with their persistent values.
        let affects_edit = |id| {
            inventory
                .iter()
                .find(|param| param.id == id)
                .is_none_or(|param| param.flags & param_flags::STEPPED != 0)
                || marker_graphs
                    .iter()
                    .any(|graph| graph.slots.values().any(|&wire| wire == id))
        };
        let mut expected = Vec::with_capacity(base.len());
        let mut bases = BTreeMap::new();
        for (&id, &value) in base {
            let saved = automated
                .get(&id)
                .map(|automated| automated.unwrap_or(value))
                .or_else(|| macros.get(&id).copied())
                .or_else(|| retained.contains(&id).then_some(value));
            if let Some(saved) = saved {
                bases.insert(id, saved);
            }
            if saved.is_none() || affects_edit(id) {
                expected.push((id, saved.unwrap_or(value)));
            }
        }
        (bases.len() <= MAX_STATE_PARAMETER_BASES).then_some(Self { expected, bases })
    }

    pub(crate) fn persisted_ids(&self) -> Vec<u32> {
        self.bases.keys().copied().collect()
    }

    /// The persistent values to store next to the captured states.
    pub(crate) fn bases(&self) -> &BTreeMap<u32, f64> {
        &self.bases
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
    use komponist_plugin_api::waveform_markers::SliceOptions;

    use super::*;

    #[test]
    fn marker_edits_keep_driven_values_as_bases_next_to_the_state() {
        let layout = NativeLayout::parse(
            br#"{
                "default_locale":"en", "locales":["en"],
                "author_ids":{"b1_freq":42,"b1_type":7,"b2_gain":9},
                "pages":[{"key":"main","labels":{"en":"Main"},"columns":1,
                          "cells":[{"col":0,"row":0,"cols":1,"rows":1,"widget":"graph",
                                    "graph":{"kind":"waveform","asset_slot":0,
                                             "waveform_markers":true,"slots":{"start":9},
                                             "name":{"en":"Waveform"},"description":{"en":"Range"}}}]}]
            }"#,
        )
        .unwrap();
        let parameter = |id, flags| ParamInfo {
            id,
            name: String::new(),
            module: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            step_count: 0,
            flags,
        };
        let inventory = [
            parameter(42, 0),
            parameter(7, param_flags::STEPPED),
            parameter(9, 0),
        ];
        let base = [(42, 0.625), (7, 2.0), (9, 0.75)].into();
        let automated = [(42, None), (7, None)].into();
        let macros = [(9, 0.5)].into();
        let plan =
            MarkerParameters::new(0, &base, &automated, &macros, &inventory, Some(&layout), [])
                .unwrap();
        // Stepped and graph-bound controls must match live; the free control is persisted only.
        assert_eq!(plan.expected, [(7, 2.0), (9, 0.5)]);
        assert_eq!(plan.bases(), &[(7, 2.0), (9, 0.5), (42, 0.625)].into());
        // Driven values need the marker graph of the edited slot.
        assert!(
            MarkerParameters::new(0, &base, &automated, &macros, &inventory, None, []).is_none()
        );
        assert!(
            MarkerParameters::new(1, &base, &automated, &macros, &inventory, Some(&layout), [])
                .is_none()
        );
        // A driver can be removed after submission while the instance still holds its value.
        let latest = MarkerParameters::new(
            0,
            &[(42, 0.75), (7, 2.0), (9, 0.75)].into(),
            &BTreeMap::new(),
            &BTreeMap::new(),
            &inventory,
            Some(&layout),
            plan.persisted_ids(),
        )
        .unwrap();
        assert_eq!(latest.bases(), &[(7, 2.0), (9, 0.75), (42, 0.75)].into());

        let mut last = None;
        let result = WaveformMarkerEditResult::from_wire(
            EditResult::Applied {
                previous_state: vec![1],
                state: vec![2],
            },
            latest.bases(),
            &mut last,
        );
        let WaveformMarkerEditResult::Applied {
            previous_state,
            state,
        } = result
        else {
            panic!("an applied edit keeps both states");
        };
        assert_eq!(&*previous_state.bytes, [1]);
        assert_eq!(&*state.bytes, [2]);
        assert_eq!(previous_state.parameter_bases, *latest.bases());
        assert_eq!(last, Some(state));
    }

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
