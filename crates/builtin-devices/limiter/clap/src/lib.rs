//! Limiter exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::signal_taps;
use komponist_bundled_clap_adapter::{DeviceDefinition, Effect, panel_from_parts};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_native_clap_sdk::{DeviceLayout, PanelV2, WidgetKind};
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

    fn layout(lang: Lang) -> DeviceLayout {
        let mut grid = Self::SPEC.layout_in(lang).clone();
        for page in &mut grid.pages {
            page.cols = 3;
            page.cells.retain(|cell| cell.widget != WidgetKind::MeterGr);
        }
        grid
    }
}

type LimiterPlugin = Effect<Limiter>;

impl komponist_bundled_clap_adapter::AudioDefinition for Limiter {
    const SIGNAL_TAPS: &'static [signal_taps::Descriptor] =
        &[
            signal_taps::Descriptor::new(REDUCTION_TAP, signal_taps::REDUCTION, 32, 16_384)
                .at_reference_rate(48_000),
        ];

    fn panel_v2() -> Option<PanelV2> {
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
        Some(PanelV2 {
            parameter_display: Default::default(),
            full_band_choice_labels: Default::default(),
            panel,
            graph_labels: Default::default(),
            canvases: Default::default(),
            headings: Default::default(),
            reduction_taps,
            pad_bank: None,
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
fn localized_fixed_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<LimiterPlugin>(4);
}

#[cfg(test)]
#[test]
fn version_two_preserves_limiter_controls_and_adds_the_full_height_meter() {
    use serde_json::json;

    let v1 = layout_contract::read_layout::<LimiterPlugin>();
    let v2 = layout_contract::read_layout_extension::<LimiterPlugin>(
        komponist_native_clap_sdk::EXTENSION_ID_V2,
    );
    assert_eq!(v2["schema_version"], 2);
    assert_eq!(v2["author_ids"], v1["author_ids"]);
    let page = &v2["pages"][0];
    assert_eq!(page["columns"], 4);
    assert_eq!(page["labels"], v1["pages"][0]["labels"]);
    let cells = page["cells"].as_array().unwrap();
    let (meter, controls) = cells.split_last().unwrap();
    assert_eq!(controls, v1["pages"][0]["cells"].as_array().unwrap());
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
