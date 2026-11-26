//! Persistent parameter values for authenticated bundles using the pinned CLAP wrapper.

use std::collections::{BTreeMap, BTreeSet};

use komponist_builtin::BuiltinKind;
use komponist_device_core::pinned_clap_state;
use komponist_ipc::ParamInfo;
use komponist_ipc::control::param_flags;
use komponist_ipc::native_layout::NativeLayout;
use komponist_ipc::waveform_markers::EditResult;
use komponist_model::params::ParamId;
use serde_json::{Map, Value};

pub(crate) fn kind_for_id(id: &str) -> Option<BuiltinKind> {
    BuiltinKind::all().find(|kind| crate::catalog::clap_id(kind.catalog().id) == id)
}

#[derive(Clone, Copy)]
pub(crate) struct ParameterState<'a> {
    pub(crate) kind: BuiltinKind,
    pub(crate) layout: &'a NativeLayout,
}

impl ParameterState<'_> {
    /// Restore the persistent values of known automation targets in a captured snapshot.
    /// The live processor, unrelated parameter entries and all wrapper fields are left untouched.
    pub(crate) fn restore_param_bases(
        self,
        state: &[u8],
        corrections: impl IntoIterator<Item = (u32, f64)>,
    ) -> Option<Vec<u8>> {
        apply_parameter_values(state, &self.parameter_values(corrections)?)
    }

    fn parameter_values(
        self,
        corrections: impl IntoIterator<Item = (u32, f64)>,
    ) -> Option<Map<String, Value>> {
        let mut corrections: BTreeMap<_, _> = corrections.into_iter().collect();
        let mut params = Map::new();
        if corrections.is_empty() {
            return Some(params);
        }
        for (author, wire) in self.layout.author_ids()? {
            let Some(value) = corrections.remove(wire) else {
                continue;
            };
            let param = ParamId::new(author);
            let info = match self.kind {
                BuiltinKind::Hexa => komponist_hexa::Macro::ALL
                    .into_iter()
                    .find(|mac| mac.key() == author)
                    .map(komponist_hexa::Macro::param_info)
                    .or_else(|| {
                        komponist_hexa::device::param(&param).map(|card| card.info.clone())
                    })?,
                BuiltinKind::Internal(kind) => {
                    kind.params()[usize::try_from(kind.wire(&param)?).ok()?].clone()
                }
            };
            let (min, max) = pinned_clap_state::clap_range(&info);
            if !(min..=max).contains(&value) {
                return None;
            }
            let persistent = pinned_clap_state::from_clap(&info, value);
            // The pinned wrapper accepts ordinal values for enums with stable string IDs.
            params.insert(
                author.clone(),
                pinned_clap_state::parameter(&info, persistent).0,
            );
        }
        corrections.is_empty().then_some(params)
    }
}

fn apply_parameter_values(state: &[u8], corrections: &Map<String, Value>) -> Option<Vec<u8>> {
    if corrections.is_empty() {
        return Some(state.to_vec());
    }
    let mut document = pinned_clap_state::decode(state)?;
    let params = document.get_mut("params")?.as_object_mut()?;
    params.extend(corrections.clone());
    Some(pinned_clap_state::encode_document(&document))
}

/// Parameter values to verify and preserve around one marker edit.
#[derive(Debug, PartialEq)]
pub(crate) struct MarkerParameters {
    pub(crate) expected: Vec<(u32, f64)>,
    persistent: Map<String, Value>,
    persisted_ids: Vec<u32>,
}

impl MarkerParameters {
    pub(crate) fn new(
        slot: u32,
        base: &BTreeMap<u32, f64>,
        automated: &BTreeMap<u32, Option<f64>>,
        macros: &BTreeMap<u32, f64>,
        inventory: &[ParamInfo],
        bundle: Option<ParameterState<'_>>,
        retain: impl IntoIterator<Item = u32>,
    ) -> Option<Self> {
        let retained: BTreeSet<_> = retain.into_iter().collect();
        if automated.is_empty() && macros.is_empty() && retained.is_empty() {
            return Some(Self {
                expected: base.iter().map(|(&id, &value)| (id, value)).collect(),
                persistent: Map::new(),
                persisted_ids: Vec::new(),
            });
        }

        // Opaque states still require a settled parameter inventory. For verified bundles the
        // pinned wrapper lets us preserve driver-owned parameters without touching the processor.
        let bundle = bundle?;
        let marker_graphs: Vec<_> = bundle
            .layout
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
        let mut persistent = BTreeMap::new();
        for (&id, &value) in base {
            let saved = automated
                .get(&id)
                .map(|automated| automated.unwrap_or(value))
                .or_else(|| macros.get(&id).copied())
                .or_else(|| retained.contains(&id).then_some(value));
            if let Some(saved) = saved {
                persistent.insert(id, saved);
            }
            if saved.is_none() || affects_edit(id) {
                expected.push((id, saved.unwrap_or(value)));
            }
        }
        Some(Self {
            expected,
            persisted_ids: persistent.keys().copied().collect(),
            persistent: bundle.parameter_values(persistent)?,
        })
    }

    pub(crate) fn persisted_ids(&self) -> &[u32] {
        &self.persisted_ids
    }

    pub(crate) fn normalize(&self, result: EditResult) -> EditResult {
        if self.persistent.is_empty() {
            return result;
        }
        let EditResult::Applied {
            previous_state,
            state,
        } = result
        else {
            return result;
        };
        let normalize = |state: &[u8]| apply_parameter_values(state, &self.persistent);
        match (normalize(&previous_state), normalize(&state)) {
            (Some(previous_state), Some(state)) => EditResult::Applied {
                previous_state,
                state,
            },
            // The live edit already happened. If the pinned state cannot be made persistent,
            // retire this instance so it cannot replace the project's last known-good bytes.
            _ => EditResult::Failed {
                message: "Could not preserve parameter bases for the waveform marker edit".into(),
                rollback_failed: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_builtin::Internal;
    use serde_json::json;

    #[test]
    fn only_known_automation_parameters_are_replaced_in_the_saved_state() {
        let layout = NativeLayout::parse(
            br#"{
                "default_locale":"en", "locales":["en"],
                "author_ids":{"b1_freq":42,"b1_type":7},
                "pages":[{"key":"main","labels":{"en":"Main"},"columns":1,
                          "cells":[{"col":0,"row":0,"cols":1,"rows":1,"widget":"knob","param_id":42}]}]
            }"#,
        )
        .unwrap();
        let document = json!({
            "version":"0.1.0",
            "params":{
                "b1_type":{"string":"high-shelf"}, "b1_freq":{"f32":0.19}, "b2_gain":{"f32":0.75}
            },
            "fields":{"factory_preset":"\"Telephone\"","other":"{\"authored\":true}"}
        });
        let state = pinned_clap_state::encode_document(&document);
        let codec = ParameterState {
            kind: BuiltinKind::Internal(Internal::Eq),
            layout: &layout,
        };
        let corrected = codec
            .restore_param_bases(&state, [(42, 0.625), (7, 2.0)])
            .unwrap();
        let mut expected = document;
        expected["params"]["b1_type"] = json!({"i32":2});
        expected["params"]["b1_freq"] = json!({"f32":0.625});
        assert_eq!(pinned_clap_state::decode(&corrected), Some(expected));
        assert!(codec.restore_param_bases(&state, [(99, 0.4)]).is_none());
        assert!(
            codec
                .restore_param_bases(&state, [(42, f64::NAN)])
                .is_none()
        );
    }
    #[test]
    fn marker_history_preserves_persistent_controls_when_an_unrelated_driver_is_live() {
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
        ).unwrap();
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
        let codec = ParameterState {
            kind: kind_for_id("org.klang-studio.komponist.eq").unwrap(),
            layout: &layout,
        };
        let plan =
            MarkerParameters::new(0, &base, &automated, &macros, &inventory, Some(codec), [])
                .unwrap();
        assert_eq!(plan.expected, [(7, 2.0), (9, 0.5)]);
        assert!(
            MarkerParameters::new(0, &base, &automated, &macros, &inventory, None, []).is_none()
        );

        let document = |markers| {
            json!({
                "version":"0.1.0",
                "params":{"b1_freq":{"f32":0.25},"b1_type":{"i32":2},"b2_gain":{"f32":0.5}},
                "fields":{"markers":markers}
            })
        };
        // A driver can be removed after submission while the bridge still holds its live value.
        let latest_base = [(42, 0.75), (7, 2.0), (9, 0.75)].into();
        let latest = MarkerParameters::new(
            0,
            &latest_base,
            &BTreeMap::new(),
            &BTreeMap::new(),
            &inventory,
            Some(codec),
            plan.persisted_ids().iter().copied(),
        )
        .unwrap();
        assert_eq!(latest.persistent["b1_freq"], json!({"f32":0.75}));
        assert_eq!(latest.persistent["b2_gain"], json!({"f32":0.75}));

        let before = document("old");
        let after = document("new");
        let result = plan.normalize(EditResult::Applied {
            previous_state: pinned_clap_state::encode_document(&before),
            state: pinned_clap_state::encode_document(&after),
        });
        let EditResult::Applied {
            previous_state,
            state,
        } = result
        else {
            panic!("valid edits must retain both versions of the persistent state");
        };
        for (bytes, mut expected) in [(previous_state, before), (state, after)] {
            expected["params"]["b1_freq"] = json!({"f32":0.625});
            assert_eq!(pinned_clap_state::decode(&bytes), Some(expected));
        }
    }

    #[test]
    fn persistent_state_repairs_every_exposed_hexa_macro() {
        use komponist_hexa::Macro;

        let author_ids: BTreeMap<_, _> = Macro::ALL
            .into_iter()
            .map(|mac| (mac.key(), mac.id()))
            .collect();
        let layout = NativeLayout::parse(&serde_json::to_vec(&json!({
            "default_locale":"en", "locales":["en"],
            "author_ids":author_ids,
            "pages":[{"key":"main","labels":{"en":"Main"},"columns":1,
                      "cells":[{"col":0,"row":0,"cols":1,"rows":1,"widget":"knob","param_id":0}]}]
        })).unwrap()).unwrap();
        let applied = Macro::ALL.map(|mac| (mac.id(), 1.0));
        let state =
            komponist_hexa::state::encode_clap_patch(komponist_hexa::default_patch(), "0.1.0");
        let codec = ParameterState {
            kind: BuiltinKind::Hexa,
            layout: &layout,
        };
        let restored = codec.restore_param_bases(&state, applied).unwrap();
        let document = pinned_clap_state::decode(&restored).unwrap();
        for mac in Macro::ALL {
            let info = mac.param_info();
            let plain = pinned_clap_state::from_clap(&info, 1.0);
            assert_eq!(
                document["params"][mac.key()],
                pinned_clap_state::parameter(&info, plain).0,
                "{}",
                mac.key()
            );
        }
    }
}
