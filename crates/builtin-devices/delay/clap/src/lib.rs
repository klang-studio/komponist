//! Stereo Delay exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::{
    Binding, DeviceDefinition, Effect, device_labels, panel_from_parts, parameter_display,
};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::WidgetKind;
use komponist_native_clap_sdk::{GraphLabels, Panel};
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

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        (info.id.as_str() == "note").then(|| Binding::choice::<SyncNote>(info))
    }
}

type DelayPlugin = Effect<Delay>;

impl komponist_bundled_clap_adapter::AudioDefinition for Delay {
    fn authored_panel() -> Option<Panel> {
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
        Some(Panel {
            graph_labels: [((0, graph), graph_labels)].into(),
            canvases: Default::default(),
            reduction_taps: Default::default(),
            headings: Default::default(),
            pad_bank: None,
            parameter_display: parameter_display(english_params),
            full_band_choice_labels: Default::default(),
            ..panel
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
fn localized_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<DelayPlugin>(Delay::SPEC.params().len());
}

#[cfg(test)]
#[test]
fn the_panel_keeps_the_xy_grid_and_clap_parameter_ids() {
    use serde_json::json;

    let layout = layout_contract::read_layout::<DelayPlugin>();
    let time = layout["author_ids"]["time"].as_u64().unwrap().to_string();
    let feedback = layout["author_ids"]["feedback"]
        .as_u64()
        .unwrap()
        .to_string();
    let page = &layout["pages"][0];
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
                "slots": {"x": layout["author_ids"]["time"], "y": layout["author_ids"]["feedback"]},
            }
        })
    );
    assert_eq!(
        layout["parameter_display"][time],
        json!({"min": 1.0, "max": 2000.0, "default": 375.0,
               "scale": "logarithmic", "unit": "milliseconds"})
    );
    assert_eq!(
        layout["parameter_display"][feedback],
        json!({"min": 0.0, "max": 0.95, "default": 0.42,
               "scale": "linear", "unit": "percent"})
    );
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Delay>();
}
