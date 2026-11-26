//! Parametric EQ exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::signal_taps;
use komponist_bundled_clap_adapter::{
    Binding, DeviceDefinition, Effect, panel_from_parts, parameter_display,
};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::layout::CellParam;
use komponist_model::params::{ParamInfo, ParamKind};
use komponist_native_clap_sdk::{GraphLabels, Panel};
use nice_plug::prelude::*;

struct Eq;

const SPECTRUM: &str = "spectrum";

#[derive(Clone, Copy, Enum, PartialEq)]
enum Shape {
    #[id = "low-cut"]
    #[name = "Low cut"]
    LowCut,
    #[id = "low-shelf"]
    #[name = "Low shelf"]
    LowShelf,
    #[id = "bell"]
    Bell,
    #[id = "high-shelf"]
    #[name = "High shelf"]
    HighShelf,
    #[id = "high-cut"]
    #[name = "High cut"]
    HighCut,
}

impl DeviceDefinition for Eq {
    const PUBLISH_FACTORY_PRESETS: bool = true;
    const SPEC: &'static Spec = &komponist_eq::SPEC;
    const NAME: &'static str = "EQ";
    const ID: &'static str = "org.klang-studio.komponist.eq";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Equalizer,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        info.id
            .as_str()
            .ends_with("_type")
            .then(|| Binding::choice::<Shape>(info))
    }
}

type EqPlugin = Effect<Eq>;

impl komponist_bundled_clap_adapter::AudioDefinition for Eq {
    const SIGNAL_TAPS: &'static [signal_taps::Descriptor] = &[signal_taps::Descriptor::new(
        SPECTRUM,
        signal_taps::SPECTRUM_SOURCE,
        1,
        16_384,
    )];

    fn authored_panel() -> Option<Panel> {
        let en = Self::SPEC.params_in(Lang::En);
        let de = Self::SPEC.params_in(Lang::De);
        let mut panel = panel_from_parts(|lang| Self::SPEC.layout_in(lang).clone(), en, de);
        let index =
            panel.grid.pages[0]
                .cells
                .iter_mut()
                .enumerate()
                .find_map(|(index, cell)| {
                    if let CellParam::Graph(graph) = &mut cell.param {
                        graph
                            .bind
                            .push(("spectrum_enabled".into(), "analyse".into()));
                        Some(index)
                    } else {
                        None
                    }
                })?;
        let labels = |text: fn(Lang) -> &'static str| {
            Lang::ALL
                .into_iter()
                .map(|lang| (lang.code().into(), text(lang).into()))
                .collect()
        };
        let graphs = GraphLabels {
            name: labels(Lang::ui_graph_eq),
            description: labels(Lang::param_eq_analyse),
            hide_title: true,
            signal_taps: [("spectrum".into(), SPECTRUM.into())].into(),
            asset_slot: None,
            waveform_markers: false,
            waveform_toolbar: false,
            waveform_slice_controls: None,
            audition_note: None,
            trim_unit: None,
        };
        let full_band_choice_labels = en
            .iter()
            .zip(de)
            .filter_map(|(en, de)| {
                let (ParamKind::Enum(en_choices), ParamKind::Enum(de_choices)) =
                    (&en.kind, &de.kind)
                else {
                    return None;
                };
                let choices = en_choices
                    .iter()
                    .zip(de_choices)
                    .map(|(en, de)| [("en".into(), en.clone()), ("de".into(), de.clone())].into())
                    .collect();
                Some((en.id.clone(), choices))
            })
            .collect();
        Some(Panel {
            graph_labels: [((0, index), graphs)].into(),
            canvases: Default::default(),
            reduction_taps: Default::default(),
            pad_bank: None,
            headings: Default::default(),
            parameter_display: parameter_display(en),
            full_band_choice_labels,
            ..panel
        })
    }

    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_eq::dsp()
    }
}

komponist_bundled_clap_adapter::export_spec_with_presets!(Eq, EqPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<EqPlugin>(Eq::SPEC.params().len());
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Eq>();
}

#[cfg(test)]
#[test]
fn the_panel_preserves_the_fixed_parameter_ids_and_binds_all_bands() {
    let layout = layout_contract::read_layout::<EqPlugin>();
    let ids = &layout["author_ids"];
    let page = &layout["pages"][0];
    assert_eq!(page["columns"], 6);
    let cells = page["cells"].as_array().unwrap();
    assert_eq!(cells.len(), 8);
    let graph = &cells[0]["graph"];
    assert_eq!(graph["bands"], komponist_eq::BANDS);
    assert_eq!(graph["select_band"], true);
    assert_eq!(graph["signal_taps"]["spectrum"], SPECTRUM);
    assert_eq!(graph["slots"]["spectrum_enabled"], ids["analyse"]);
    for band in 1..=komponist_eq::BANDS {
        for (cell, role) in [(1, "type"), (2, "on"), (4, "freq"), (5, "gain"), (6, "q")] {
            let key = format!("b{band}_{role}");
            assert_eq!(
                cells[cell]["band_controls"][band - 1]["param_id"],
                ids[&key]
            );
            assert_eq!(graph["slots"][&key], ids[&key]);
        }
    }
    let low_cut = &cells[1]["band_controls"][0]["choices"][0];
    for lang in Lang::ALL {
        assert_eq!(
            low_cut["labels"][lang.code()],
            lang.choice_eq_type_low_cut_short()
        );
        assert_eq!(
            low_cut["full_labels"][lang.code()],
            lang.choice_eq_type_low_cut()
        );
    }
    let freq = &layout["parameter_display"][ids["b1_freq"].as_u64().unwrap().to_string()];
    assert_eq!(freq["unit"], "hertz");
    assert_eq!(freq["scale"], "logarithmic");
}
