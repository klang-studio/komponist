//! Round-trip the complete Hexa patch, including fields without a CLAP parameter.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crossbeam_queue::ArrayQueue;
use komponist_bundled_clap_adapter::Parameters;
use komponist_hexa::algorithms::{ALGORITHMS, routing};
use komponist_hexa::device::{Field, OpField, WIRE_COUNT};
use komponist_hexa::macros::Macro;
use komponist_hexa::{Patch, default_patch};
use komponist_native_clap_sdk::display::{Display, DisplaySource, FmFrame};
use nice_plug::prelude::{ParamPtr, Params};
use serde::{Deserialize, Serialize};

const PATCH_FIELD: &str = "hexa-patch-v1";
pub(super) const FM_ROUTING_KEY: &str = "fm-routing";

/// The patch includes derived controls and fields that may never be exposed. The
/// companion wrapper values distinguish new automation from stale overlapping macros
/// when a saved snapshot is restored.
#[derive(Clone, Copy)]
pub(super) struct Snapshot {
    pub(super) patch: Patch,
    pub(super) applied: [f64; WIRE_COUNT],
}

#[derive(Serialize, Deserialize)]
struct SavedPatch {
    patch: Patch,
    applied: Vec<f64>,
}

impl Snapshot {
    fn encode(self) -> String {
        serde_json::to_string(&SavedPatch {
            patch: self.patch,
            applied: self.applied.into(),
        })
        .expect("serialize a valid Hexa patch and wrapper values")
    }

    fn decode(payload: &str) -> Option<Self> {
        let saved = serde_json::from_str::<SavedPatch>(payload).ok()?;
        let applied: [f64; WIRE_COUNT] = saved.applied.try_into().ok()?;
        applied.iter().all(|value| value.is_finite()).then(|| Self {
            patch: saved.patch.sanitized(),
            applied,
        })
    }
}

pub(super) struct HexaParams {
    pub(super) bindings: Parameters,
    pub(super) to_audio: ArrayQueue<Snapshot>,
    pub(super) from_audio: ArrayQueue<Snapshot>,
    /// Accessed only by the host's control thread. Audio exchanges bounded
    /// fixed-size snapshots through the queues without allocating or locking.
    last: Mutex<Snapshot>,
}

impl HexaParams {
    pub(super) fn new(bindings: Parameters) -> Self {
        let snapshot = Snapshot {
            patch: default_patch(),
            applied: std::array::from_fn(|index| {
                let mut applied = None;
                bindings.changed_value(index, &mut applied);
                applied.expect("initialized Hexa binding")
            }),
        };
        Self {
            bindings,
            last: Mutex::new(snapshot),
            to_audio: ArrayQueue::new(1),
            from_audio: ArrayQueue::new(1),
        }
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        let mut last = self.last.lock().expect("Hexa state snapshot lock");
        if let Some(snapshot) = self.from_audio.pop() {
            *last = snapshot;
        }
        *last
    }

    fn fm_routing(&self) -> FmFrame {
        let mut snapshot = self.snapshot();
        // Preview only routing changes while the instance is idle. Use the same
        // wire order as the audio callback so algorithm edits and individual
        // output edits remain consistent with the next processed patch.
        for (wire, applied) in snapshot.applied.iter_mut().enumerate() {
            let Some(
                field @ (Field::Macro(Macro::Algorithm | Macro::Feedback)
                | Field::Op(_, OpField::Output)),
            ) = Field::from_wire(wire as u32)
            else {
                continue;
            };
            let mut current = Some(*applied);
            if let Some(value) = self.bindings.changed_value(wire, &mut current) {
                field.set(&mut snapshot.patch, value);
                *applied = current.expect("initialized routing parameter");
            }
        }

        let routing = routing(&snapshot.patch);
        let number = snapshot.applied[Macro::Algorithm.id() as usize] as u8;
        let fixed = number
            .checked_sub(1)
            .and_then(|index| ALGORITHMS.get(usize::from(index)))
            .filter(|algorithm| {
                algorithm.modulates == routing.modulates
                    && (0..routing.modulates.len())
                        .all(|op| algorithm.is_carrier(op) == (routing.outputs & (1 << op) != 0))
            });
        FmFrame {
            key: FM_ROUTING_KEY.into(),
            algorithm: fixed.map(|_| number),
            modulates: routing.modulates.to_vec(),
            outputs: routing.outputs,
            feedback: routing
                .feedback
                .or_else(|| fixed.map(|algorithm| algorithm.feedback))
                .map(|op| op as u8),
        }
    }
}

impl DisplaySource for HexaParams {
    fn paint(&self, display: &mut Display) {
        display.fm_topology(self.fm_routing());
    }
}

// SAFETY: The adapter owns the bound parameters throughout the lifetime of this Params object.
unsafe impl Params for HexaParams {
    fn param_map(&self) -> Vec<(String, ParamPtr, String)> {
        self.bindings.param_map()
    }

    fn serialize_fields(&self) -> BTreeMap<String, String> {
        BTreeMap::from([(PATCH_FIELD.into(), self.snapshot().encode())])
    }

    fn deserialize_fields(&self, fields: &BTreeMap<String, String>) {
        let Some(snapshot) = fields
            .get(PATCH_FIELD)
            .and_then(|value| Snapshot::decode(value))
        else {
            return;
        };
        let mut last = self.last.lock().expect("Hexa state snapshot lock");
        // The wrapper excludes process while loading fields. Discard a publication
        // from the previous patch and make an immediate subsequent save consistent.
        self.from_audio.pop();
        *last = snapshot;
        self.to_audio.force_push(snapshot);
    }
}

/// Produce a CLAP state stream from an existing Hexa patch using the pinned wrapper's
/// uncompressed format. The pure codec is shared with project compatibility.
pub fn legacy_patch_to_clap_state(
    legacy_state: &[u8],
) -> Result<Vec<u8>, komponist_hexa::StateError> {
    komponist_hexa::state::legacy_patch_to_clap_state(legacy_state, env!("CARGO_PKG_VERSION"))
}
