//! Stereo Delay exported through the native CLAP adapter.

use komponist_bundled_clap_adapter::{Binding, Effect, EffectDefinition};
use komponist_device_core::Spec;
use komponist_device_core::layout::{knob, param, single_page, toggle};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{Cell, DeviceLayout, Half, WidgetKind};
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
    #[id = "2t"]
    #[name = "1/2T"]
    HalfTriplet,
    #[id = "2"]
    #[name = "1/2"]
    Half,
    #[id = "1"]
    #[name = "1/1"]
    Whole,
}

impl EffectDefinition for Delay {
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

nice_export_clap!(DelayPlugin);

#[cfg(test)]
#[path = "../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[test]
fn localized_fixed_layout_matches_the_exported_clap_parameters() {
    layout_contract::assert_localized_bindings::<DelayPlugin>(9);
}
