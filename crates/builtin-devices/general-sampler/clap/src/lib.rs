//! The chromatic sampler packaged as a native CLAP instrument.

mod panel;

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition};
use komponist_device_core::{Dsp, Spec};
use komponist_model::params::ParamInfo;
use komponist_sampler_clap::{AssetEditIntent, Sampled, SampledDefinition, SliceSettingsCodec};
use komponist_sampler_shared::{ManagedLoader, SamplerHandle};
use nice_plug::prelude::*;

struct GeneralSampler;

#[derive(Clone, Copy, Enum, PartialEq)]
enum Mode {
    #[id = "one-shot"]
    #[name = "1×"]
    OneShot,
    #[id = "gate"]
    Gate,
    #[id = "slices"]
    Slices,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Direction {
    #[id = "forward"]
    Forward,
    #[id = "reverse"]
    Reverse,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Filter {
    #[id = "off"]
    Off,
    #[id = "low-pass"]
    #[name = "Low-pass"]
    LowPass,
    #[id = "high-pass"]
    #[name = "High-pass"]
    HighPass,
    #[id = "band-pass"]
    #[name = "Band-pass"]
    BandPass,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Wave {
    #[id = "sine"]
    Sine,
    #[id = "triangle"]
    Triangle,
    #[id = "saw-up"]
    #[name = "Rising saw"]
    SawUp,
    #[id = "saw-down"]
    #[name = "Falling saw"]
    SawDown,
    #[id = "square"]
    Square,
    #[id = "sample-hold"]
    #[name = "Sample & hold"]
    SampleHold,
    #[id = "smooth-random"]
    #[name = "Smooth random"]
    SmoothRandom,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Target {
    #[id = "off"]
    Off,
    #[id = "pitch"]
    Pitch,
    #[id = "cutoff"]
    Cutoff,
    #[id = "start"]
    Start,
}

impl DeviceDefinition for GeneralSampler {
    const SPEC: &'static Spec = &komponist_general_sampler::SPEC;
    const NAME: &'static str = "Sampler";
    const ID: &'static str = "org.klang-studio.komponist.general-sampler";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Sampler,
        ClapFeature::Stereo,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        Some(match info.id.as_str() {
            "mode" => Binding::choice::<Mode>(info),
            "direction" => Binding::choice::<Direction>(info),
            "filter" => Binding::choice::<Filter>(info),
            "lfo_shape" => Binding::choice::<Wave>(info),
            "lfo_target" | "velocity_target" => Binding::choice::<Target>(info),
            _ => return None,
        })
    }
}

impl SampledDefinition for GeneralSampler {
    const PADS: usize = 1;
    const DISPLAY: Option<
        fn(
            &komponist_bundled_clap_adapter::Parameters,
            &mut komponist_native_clap_sdk::display::Display,
        ),
    > = Some(panel::paint);
    const SLICE_SETTINGS: Option<SliceSettingsCodec> = Some(SliceSettingsCodec {
        read: |assets| komponist_general_sampler::read_settings(Some(assets)),
        write: |assets, settings| komponist_general_sampler::write_settings(Some(assets), settings),
    });

    fn trim_for_asset_edit(_slot: usize, intent: AssetEditIntent) -> Option<[usize; 2]> {
        (intent == AssetEditIntent::Assign).then(|| {
            ["start", "end"].map(|id| {
                Self::SPEC
                    .params()
                    .iter()
                    .position(|parameter| parameter.id.as_str() == id)
                    .expect("the sampler declares both trim controls")
            })
        })
    }

    fn panel() -> komponist_native_clap_sdk::Panel {
        panel::panel()
    }

    fn sampler() -> (Box<dyn Dsp>, SamplerHandle, ManagedLoader) {
        komponist_general_sampler::dsp_with_managed_loader()
    }
}

type GeneralSamplerPlugin = Sampled<GeneralSampler>;

nice_export_clap!(GeneralSamplerPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[path = "../../../sampler-shared/clap_contract.rs"]
mod sampler_contract;

#[cfg(test)]
mod marker_tests;

#[cfg(test)]
#[test]
fn all_controls_have_stable_bilingual_bindings_and_restore_sample_assets() {
    layout_contract::assert_localized_bindings::<GeneralSamplerPlugin>(
        GeneralSampler::SPEC.params().len(),
    );
    sampler_contract::sample_asset_roundtrip::<GeneralSamplerPlugin>(60, "mode", true);
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<GeneralSampler>();
}
