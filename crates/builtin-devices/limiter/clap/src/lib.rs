//! Limiter exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::{DeviceDefinition, Effect};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_native_clap_sdk::{DeviceLayout, WidgetKind};
use nice_plug::prelude::*;

struct Limiter;

impl DeviceDefinition for Limiter {
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
    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_limiter::dsp()
    }
}

nice_export_clap!(LimiterPlugin);

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
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Limiter>();
}
