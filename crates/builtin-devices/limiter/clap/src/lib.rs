//! Limiter exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::signal_taps;
use komponist_bundled_clap_adapter::{DeviceDefinition, Effect, panel_from_parts};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_native_clap_sdk::{Panel, WidgetKind};
use nice_plug::prelude::*;

struct Limiter;

const REDUCTION_TAP: &str = "reduction";

impl DeviceDefinition for Limiter {
    const PUBLISH_FACTORY_PRESETS: bool = true;
    const SPEC: &'static Spec = &komponist_limiter::SPEC;
    const NAME: &'static str = "Limiter";
    const ID: &'static str = "org.klang-studio.komponist.limiter";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const LATENCY_PARAMETER: Option<&'static str> = Some("lookahead");
    const REPORTS_REDUCTION: bool = true;
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Limiter,
    ];
}

type LimiterPlugin = Effect<Limiter>;

impl komponist_bundled_clap_adapter::AudioDefinition for Limiter {
    const SIGNAL_TAPS: &'static [signal_taps::Descriptor] =
        &[
            signal_taps::Descriptor::new(REDUCTION_TAP, signal_taps::REDUCTION, 32, 16_384)
                .at_reference_rate(48_000),
        ];

    fn authored_panel() -> Option<Panel> {
        let panel = panel_from_parts(
            |lang| Self::SPEC.layout_in(lang).clone(),
            Self::SPEC.params_in(Lang::En),
            Self::SPEC.params_in(Lang::De),
        );
        let reduction_taps = panel
            .grid
            .pages
            .iter()
            .enumerate()
            .flat_map(|(page, grid)| {
                grid.cells
                    .iter()
                    .enumerate()
                    .filter(|(_, cell)| cell.widget == WidgetKind::MeterGr)
                    .map(move |(cell, _)| ((page, cell), REDUCTION_TAP.to_owned()))
            })
            .collect();
        Some(Panel {
            parameter_display: Default::default(),
            full_band_choice_labels: Default::default(),
            graph_labels: Default::default(),
            canvases: Default::default(),
            headings: Default::default(),
            reduction_taps,
            pad_bank: None,
            ..panel
        })
    }

    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_limiter::dsp()
    }
}

komponist_bundled_clap_adapter::export_spec_with_presets!(Limiter, LimiterPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<LimiterPlugin>(Limiter::SPEC.params().len());
}

#[cfg(test)]
#[test]
fn layout_preserves_limiter_controls_and_adds_the_full_height_meter() {
    use serde_json::json;

    let layout = layout_contract::read_layout::<LimiterPlugin>();
    let page = &layout["pages"][0];
    assert_eq!(page["columns"], 4);
    let cells = page["cells"].as_array().unwrap();
    let (meter, controls) = cells.split_last().unwrap();
    assert_eq!(controls.len(), 4);
    assert_eq!(
        meter,
        &json!({
            "widget": "meter_gr", "col": 3, "row": 0, "cols": 1, "rows": 3,
            "reduction_tap": REDUCTION_TAP
        })
    );
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Limiter>();
}
