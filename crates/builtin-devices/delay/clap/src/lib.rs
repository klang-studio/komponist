//! Stereo Delay exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::{
    Binding, DeviceDefinition, Effect, device_labels, panel_from_parts, parameter_display,
};
use komponist_device_core::Spec;
use komponist_device_core::layout::{knob, param, single_page, toggle};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{Cell, DeviceLayout, Half, WidgetKind};
use komponist_native_clap_sdk::{GraphLabels, PanelV2};
use nice_plug::prelude::*;

struct Delay;

#[derive(Clone, Copy, Enum, PartialEq)]
enum SyncNote {
    #[id = "32"]
    #[name = "1/32"]
    ThirtySecond,
    #[id = "16t"]
    #[name = "1/16T"]
    SixteenthTriplet,
    #[id = "16"]
    #[name = "1/16"]
    Sixteenth,
    #[id = "16d"]
    #[name = "1/16."]
    SixteenthDotted,
    #[id = "8t"]
    #[name = "1/8T"]
    EighthTriplet,
    #[id = "8"]
    #[name = "1/8"]
    Eighth,
    #[id = "8d"]
    #[name = "1/8."]
    EighthDotted,
    #[id = "4t"]
    #[name = "1/4T"]
    QuarterTriplet,
    #[id = "4"]
    #[name = "1/4"]
    Quarter,
    #[id = "4d"]
    #[name = "1/4."]
    QuarterDotted,
    // The IDs differ from the retired "2t" and "2" so `migrate_state` can tell old states from new.
    #[id = "half"]
    #[name = "1/2"]
    Half,
    #[id = "half-dotted"]
    #[name = "1/2."]
    HalfDotted,
    #[id = "1"]
    #[name = "1/1"]
    Whole,
}

impl DeviceDefinition for Delay {
    const PUBLISH_FACTORY_PRESETS: bool = true;
    const SPEC: &'static Spec = &komponist_delay::SPEC;
    const NAME: &'static str = "Delay";
    const ID: &'static str = "org.klang-studio.komponist.delay";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Delay,
    ];

    /// Before the note order matched the DSP table, the CLAP enum listed "2t" (named 1/2T) at index
    /// 10 and "2" (named 1/2) at index 11, while the DSP played a half note at 10 and a dotted half
    /// at 11. States saved with those IDs keep the sound they had: "2t" was a half note, "2" a
    /// dotted half.
    fn migrate_state(state: &mut PluginState) {
        use nice_plug::plugin::ParamValue;
        if let Some(ParamValue::String(id)) = state.params.get_mut("note") {
            match id.as_str() {
                "2t" => *id = "half".to_owned(),
                "2" => *id = "half-dotted".to_owned(),
                _ => {}
            }
        }
    }

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        (info.id.as_str() == "note").then(|| Binding::choice::<SyncNote>(info))
    }

    fn layout(_: Lang) -> DeviceLayout {
        // Native v1 can expose the two XY axes as controls.
        single_page(
            4,
            vec![
                knob(0, 0, "time"),
                knob(1, 0, "feedback"),
                knob(2, 0, "low_cut"),
                knob(3, 0, "high_cut"),
                knob(0, 1, "width"),
                knob(1, 1, "mix"),
                toggle(2, 1, 1, Half::Top, param("ping_pong")),
                toggle(3, 1, 1, Half::Top, param("sync")),
                Cell::half(2, 1, 2, Half::Bottom, WidgetKind::Dropdown, param("note")),
            ],
        )
    }
}

type DelayPlugin = Effect<Delay>;

impl komponist_bundled_clap_adapter::AudioDefinition for Delay {
    fn panel_v2() -> Option<PanelV2> {
        let english_params = Self::SPEC.params_in(Lang::En);
        let mut panel = panel_from_parts(
            |lang| Self::SPEC.layout_in(lang).clone(),
            english_params,
            Self::SPEC.params_in(Lang::De),
        );
        panel.device_labels = Some(device_labels(&Self::SPEC.catalog));
        let graph = panel.grid.pages[0]
            .cells
            .iter()
            .position(|cell| cell.widget == WidgetKind::Graph)?;
        let graph_labels = GraphLabels {
            name: Lang::ALL
                .into_iter()
                .map(|lang| (lang.code().into(), lang.ui_graph_xy().into()))
                .collect(),
            description: Lang::ALL
                .into_iter()
                .map(|lang| {
                    (
                        lang.code().into(),
                        format!("{} · {}", lang.param_time(), lang.param_feedback()),
                    )
                })
                .collect(),
            hide_title: true,
            signal_taps: Default::default(),
            asset_slot: None,
            waveform_markers: false,
            waveform_toolbar: false,
            waveform_slice_controls: None,
            audition_note: None,
            trim_unit: None,
        };
        Some(PanelV2 {
            panel,
            graph_labels: [((0, graph), graph_labels)].into(),
            canvases: Default::default(),
            reduction_taps: Default::default(),
            headings: Default::default(),
            pad_bank: None,
            parameter_display: parameter_display(english_params),
        })
    }

    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_delay::dsp()
    }
}

komponist_bundled_clap_adapter::export_spec_with_presets!(Delay, DelayPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_fixed_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<DelayPlugin>(9);
}

#[cfg(test)]
#[test]
fn the_v2_panel_keeps_the_xy_grid_and_clap_parameter_ids() {
    use serde_json::json;

    let v1 = layout_contract::read_layout::<DelayPlugin>();
    let v2 = layout_contract::read_layout_extension::<DelayPlugin>(
        komponist_native_clap_sdk::EXTENSION_ID_V2,
    );
    assert_eq!(v1["schema_version"], 1);
    assert!(v1.get("parameter_display").is_none());
    assert_eq!(v2["schema_version"], 2);
    assert_eq!(v2["author_ids"], v1["author_ids"]);
    let time = v2["author_ids"]["time"].as_u64().unwrap().to_string();
    let feedback = v2["author_ids"]["feedback"].as_u64().unwrap().to_string();
    let page = &v2["pages"][0];
    assert_eq!(page["columns"], 4);
    assert_eq!(page["cells"].as_array().unwrap().len(), 10);
    assert_eq!(
        page["cells"][0],
        json!({
            "widget": "graph", "col": 0, "row": 0, "cols": 2, "rows": 2,
            "graph": {
                "kind": "xy", "hide_title": true,
                "name": {"de": Lang::De.ui_graph_xy(), "en": Lang::En.ui_graph_xy()},
                "description": {
                    "de": format!("{} · {}", Lang::De.param_time(), Lang::De.param_feedback()),
                    "en": format!("{} · {}", Lang::En.param_time(), Lang::En.param_feedback()),
                },
                "slots": {"x": v2["author_ids"]["time"], "y": v2["author_ids"]["feedback"]},
            }
        })
    );
    assert_eq!(
        v2["parameter_display"][time],
        json!({"min": 1.0, "max": 2000.0, "default": 375.0,
               "scale": "logarithmic", "unit": "milliseconds"})
    );
    assert_eq!(
        v2["parameter_display"][feedback],
        json!({"min": 0.0, "max": 0.95, "default": 0.42,
               "scale": "linear", "unit": "percent"})
    );
    assert_eq!(
        page["cells"][3]["choices"],
        v1["pages"][0]["cells"][8]["choices"]
    );
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Delay>();
}

#[cfg(test)]
mod migration_tests {
    use std::collections::BTreeMap;

    use nice_plug::plugin::ParamValue;

    use super::*;

    fn migrated(id: &str) -> String {
        let mut state = PluginState {
            version: String::new(),
            params: BTreeMap::new(),
            fields: BTreeMap::new(),
        };
        state
            .params
            .insert("note".to_owned(), ParamValue::String(id.to_owned()));
        <Delay as komponist_bundled_clap_adapter::DeviceDefinition>::migrate_state(&mut state);
        let Some(ParamValue::String(id)) = state.params.get("note") else {
            panic!("note vanished");
        };
        id.clone()
    }

    /// The DSP table index an ID plays, as the saved state of the previous release did.
    fn index_of(id: &str) -> usize {
        <SyncNote as Enum>::ids()
            .unwrap()
            .iter()
            .position(|c| *c == id)
            .unwrap()
    }

    #[test]
    fn retired_ids_keep_the_note_they_played() {
        // Old "2t" selected index 10 and old "2" index 11 of the DSP table.
        assert_eq!(komponist_delay::NOTES[index_of(&migrated("2t"))].0, "1/2");
        assert_eq!(komponist_delay::NOTES[index_of(&migrated("2"))].0, "1/2.");
    }

    #[test]
    fn current_and_unrelated_ids_stay() {
        for id in ["32", "8d", "4", "1", "half", "half-dotted"] {
            assert_eq!(migrated(id), id);
        }
    }
}
