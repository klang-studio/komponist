//! Hall packaged as a conventional CLAP bundle with its bilingual native layout.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, Effect};
use komponist_device_core::Spec;
use komponist_model::params::ParamInfo;
use nice_plug::prelude::*;

struct Hall;

#[derive(Clone, Copy, Enum, PartialEq)]
enum Room {
    #[id = "chamber"]
    #[name = "Room"]
    Chamber,
    #[id = "hall"]
    #[name = "Hall"]
    Hall,
    #[id = "church"]
    #[name = "Church"]
    Church,
}

impl DeviceDefinition for Hall {
    const SPEC: &'static Spec = &komponist_reverb::SPEC;
    const NAME: &'static str = "Hall";
    const ID: &'static str = "org.klang-studio.komponist.hall";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Stereo,
        ClapFeature::Reverb,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        (info.id.as_str() == "room").then(|| Binding::choice::<Room>(info))
    }
}

type HallPlugin = Effect<Hall>;

impl komponist_bundled_clap_adapter::AudioDefinition for Hall {
    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_reverb::dsp()
    }
}

nice_export_clap!(HallPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;
#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Hall>();
}
