//! Parametric EQ exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::{Binding, Effect, EffectDefinition};
use komponist_device_core::Spec;
use komponist_device_core::layout::{knob, param, toggle};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{Cell, DeviceLayout, Half, Page, WidgetKind};
use nice_plug::prelude::*;

struct Eq;

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

impl EffectDefinition for Eq {
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

    fn layout(lang: Lang) -> DeviceLayout {
        // The v1 layout uses fixed bindings; each pair of bands has its own page.
        let prefix = match lang {
            Lang::De => "Bänder",
            _ => "Bands",
        };
        DeviceLayout {
            pages: (1..=komponist_eq::BANDS)
                .step_by(2)
                .map(|first| {
                    let mut cells = Vec::new();
                    for (row, band) in (first..=first + 1).enumerate() {
                        let row = row as u8;
                        let key = |suffix: &str| format!("b{band}_{suffix}");
                        cells.extend([
                            toggle(0, row, 2, Half::Top, param(&key("on"))),
                            Cell::half(
                                0,
                                row,
                                2,
                                Half::Bottom,
                                WidgetKind::Dropdown,
                                param(&key("type")),
                            ),
                            knob(2, row, &key("freq")),
                            knob(3, row, &key("gain")),
                            knob(4, row, &key("q")),
                        ]);
                    }
                    if first == 1 {
                        cells.push(knob(2, 2, "out"));
                    }
                    Page {
                        name: format!("{prefix} {first}–{}", first + 1),
                        cols: 5,
                        cells,
                        dividers: Vec::new(),
                    }
                })
                .collect(),
        }
    }
}

type EqPlugin = Effect<Eq>;

nice_export_clap!(EqPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_fixed_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<EqPlugin>(31);
}
