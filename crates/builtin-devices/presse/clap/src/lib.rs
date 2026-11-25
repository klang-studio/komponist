//! Presse exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::signal_taps;
use komponist_bundled_clap_adapter::{
    Binding, DeviceDefinition, Effect, device_labels, panel_from_parts, parameter_display,
};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{CellParam, DeviceLayout, GraphConfig, WidgetKind};
use komponist_native_clap_sdk::{GraphLabels, Labels, PanelV2};
use nice_plug::prelude::*;

struct Presse;

const DETECTOR: &str = "detector";
const REDUCTION: &str = "reduction";

#[derive(Clone, Copy, Enum, PartialEq)]
enum Detector {
    #[id = "peak"]
    Peak,
    #[id = "rms"]
    #[name = "RMS"]
    Rms,
}

impl DeviceDefinition for Presse {
    const PUBLISH_FACTORY_PRESETS: bool = true;
    const SPEC: &'static Spec = &komponist_presse::SPEC;
    const NAME: &'static str = "Presse";
    const ID: &'static str = "org.klang-studio.komponist.presse";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const SIDECHAIN: bool = true;
    const SIDECHAIN_FROM_MAIN: bool = true;
    const LATENCY_PARAMETER: Option<&'static str> = Some("lookahead");
    const REPORTS_REDUCTION: bool = true;
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Compressor,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        (info.id.as_str() == "detector").then(|| Binding::choice::<Detector>(info))
    }

    fn layout(lang: Lang) -> DeviceLayout {
        let mut grid = Self::SPEC.layout_in(lang).clone();
        for page in &mut grid.pages {
            page.cols = 7;
            page.dividers.clear();
            page.cells
                .retain(|cell| !matches!(cell.widget, WidgetKind::Graph | WidgetKind::MeterGr));
            for cell in &mut page.cells {
                cell.disabled_by = None;
            }
        }
        grid
    }
}

type PressePlugin = Effect<Presse>;

impl komponist_bundled_clap_adapter::AudioDefinition for Presse {
    const SIGNAL_TAPS: &'static [signal_taps::Descriptor] = &[
        signal_taps::Descriptor::new(DETECTOR, signal_taps::LEVEL_HISTORY, 32, 16_384)
            .at_reference_rate(48_000),
        signal_taps::Descriptor::new(REDUCTION, signal_taps::REDUCTION, 32, 16_384)
            .at_reference_rate(48_000),
    ];

    fn panel_v2() -> Option<PanelV2> {
        let english = Self::SPEC.params_in(Lang::En);
        let mut panel = panel_from_parts(
            |lang| Self::SPEC.layout_in(lang).clone(),
            english,
            Self::SPEC.params_in(Lang::De),
        );
        panel.device_labels = Some(device_labels(&Self::SPEC.catalog));
        let labels = |text: fn(Lang) -> &'static str| -> Labels {
            Lang::ALL
                .into_iter()
                .map(|lang| (lang.code().into(), text(lang).into()))
                .collect()
        };
        let mut graph_labels = std::collections::BTreeMap::new();
        let mut reduction_taps = std::collections::BTreeMap::new();
        for (index, cell) in panel.grid.pages[0].cells.iter().enumerate() {
            if cell.widget == WidgetKind::MeterGr {
                reduction_taps.insert((0, index), REDUCTION.into());
            }
            let CellParam::Graph(graph) = &cell.param else {
                continue;
            };
            let (name, description, signal_taps) = match graph.config {
                GraphConfig::Compressor => (
                    labels(Lang::ui_graph_transfer),
                    labels(Lang::devices_detector),
                    [("level".into(), DETECTOR.into())].into(),
                ),
                GraphConfig::GrHistory => (
                    labels(Lang::ui_graph_gr_history),
                    labels(Lang::ui_meter_gr_name),
                    [
                        ("level".into(), DETECTOR.into()),
                        ("reduction".into(), REDUCTION.into()),
                    ]
                    .into(),
                ),
                _ => continue,
            };
            graph_labels.insert(
                (0, index),
                GraphLabels {
                    name,
                    description,
                    signal_taps,
                    hide_title: false,
                    asset_slot: None,
                    waveform_markers: false,
                    waveform_toolbar: false,
                    waveform_slice_controls: None,
                    audition_note: None,
                    trim_unit: None,
                },
            );
        }
        Some(PanelV2 {
            panel,
            graph_labels,
            reduction_taps,
            canvases: Default::default(),
            headings: Default::default(),
            pad_bank: None,
            parameter_display: parameter_display(english),
        })
    }

    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_presse::dsp()
    }
}

komponist_bundled_clap_adapter::export_spec_with_presets!(Presse, PressePlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_fixed_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<PressePlugin>(14);
}

#[cfg(test)]
#[test]
fn the_v2_panel_carries_the_compressor_controls_and_live_feeds() {
    use serde_json::json;

    let v1 = layout_contract::read_layout::<PressePlugin>();
    let v2 = layout_contract::read_layout_extension::<PressePlugin>(
        komponist_native_clap_sdk::EXTENSION_ID_V2,
    );
    let ids = &v2["author_ids"];
    assert_eq!(ids, &v1["author_ids"]);
    let page = &v2["pages"][0];
    let cells = page["cells"].as_array().unwrap();
    assert_eq!(page["columns"], 8);
    assert_eq!(page["dividers"], json!([2, 4, 6]));
    assert_eq!(cells.len(), 17);
    let display = |param: &str| &v2["parameter_display"][ids[param].as_u64().unwrap().to_string()];
    assert_eq!(display("threshold")["unit"], "decibels");
    assert_eq!(display("knee")["unit"], "decibel_width");
    assert_eq!(display("ratio")["unit"], "ratio");
    assert_eq!(display("ratio")["scale"], "logarithmic");
    let makeup = cells
        .iter()
        .find(|cell| cell["param_id"] == ids["makeup"])
        .unwrap();
    assert_eq!(makeup["disabled_by"], ids["auto_makeup"]);
    assert_eq!(cells[0]["graph"]["signal_taps"], json!({"level": DETECTOR}));
    assert_eq!(
        cells[10]["graph"]["signal_taps"],
        json!({"level": DETECTOR, "reduction": REDUCTION})
    );
    assert_eq!(cells[16]["reduction_tap"], REDUCTION);
    assert!(
        v1["pages"][0]["cells"]
            .as_array()
            .unwrap()
            .iter()
            .all(|cell| cell.get("disabled_by").is_none())
    );
}

#[cfg(test)]
#[test]
fn defaults_to_stereo_and_advertises_the_alternate_sidechain_configuration() {
    use clap_sys::ext::audio_ports::{CLAP_EXT_AUDIO_PORTS, clap_plugin_audio_ports};
    use clap_sys::ext::audio_ports_config::{
        CLAP_EXT_AUDIO_PORTS_CONFIG, clap_plugin_audio_ports_config,
    };

    let instance = layout_contract::Instance::new::<PressePlugin>();
    let plugin = instance.0;
    // SAFETY: The instance is live, initialized and inactive on this test's main thread.
    unsafe {
        let get = (*plugin).get_extension.unwrap();
        let ports = get(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())
            .cast::<clap_plugin_audio_ports>()
            .as_ref()
            .unwrap();
        let count = ports.count.unwrap();
        assert_eq!(count(plugin, true), 1);
        assert_eq!(count(plugin, false), 1);

        let fallback = get(
            plugin,
            c"org.klang-studio.komponist.sidechain-fallback/1".as_ptr(),
        )
        .cast::<[u32; 2]>()
        .as_ref()
        .unwrap();
        assert_eq!(fallback[0], 1); // Main detector fallback.
        let configs = get(plugin, CLAP_EXT_AUDIO_PORTS_CONFIG.as_ptr())
            .cast::<clap_plugin_audio_ports_config>()
            .as_ref()
            .unwrap();
        assert!(configs.select.unwrap()(plugin, fallback[1]));
        assert_eq!(count(plugin, true), 2);
        assert_eq!(count(plugin, false), 1);
    }
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Presse>();
}
