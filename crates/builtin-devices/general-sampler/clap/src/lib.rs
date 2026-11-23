//! The chromatic sampler packaged as a native CLAP instrument.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition};
use komponist_device_core::layout::{knob_large, param, toggle};
use komponist_device_core::{Dsp, Spec};
use komponist_i18n::Lang;
use komponist_model::params::layout::{Cell, DeviceLayout, Half, Page, WidgetKind};
use komponist_model::params::{ParamInfo, ParamKind};
use komponist_sampler_clap::{Sampled, SampledDefinition};
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

    fn layout(lang: Lang) -> DeviceLayout {
        // Six two-column controls per row, leaving room for the longer categorical labels.
        let params = Self::SPEC.params_in(lang);
        let titles = match lang {
            Lang::De => ["Wiedergabe", "Modulation"],
            _ => ["Playback", "Modulation"],
        };
        DeviceLayout {
            pages: params
                .chunks(17)
                .zip(titles)
                .map(|(group, name)| Page {
                    name: name.into(),
                    cols: 12,
                    cells: group
                        .iter()
                        .enumerate()
                        .map(|(index, info)| {
                            let col = ((index % 6) * 2) as u8;
                            let row = (index / 6) as u8;
                            let id = info.id.as_str();
                            match info.kind {
                                ParamKind::Enum(_) => Cell::half(
                                    col,
                                    row,
                                    2,
                                    Half::Top,
                                    WidgetKind::Dropdown,
                                    param(id),
                                ),
                                ParamKind::Bool => toggle(col, row, 2, Half::Top, param(id)),
                                ParamKind::Int { .. } => Cell::half(
                                    col,
                                    row,
                                    1,
                                    Half::Top,
                                    WidgetKind::Number,
                                    param(id),
                                ),
                                ParamKind::Float { .. } => knob_large(col, row, id),
                            }
                        })
                        .collect(),
                    dividers: Vec::new(),
                })
                .collect(),
        }
    }
}

impl SampledDefinition for GeneralSampler {
    const PADS: usize = 1;

    fn sampler() -> (Box<dyn Dsp>, SamplerHandle, ManagedLoader) {
        komponist_general_sampler::dsp_with_managed_loader()
    }

    fn restore_settings(handle: &SamplerHandle, assets: &[u8]) {
        handle.set_general_settings(&komponist_general_sampler::read_settings(Some(assets)));
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
#[test]
fn all_controls_have_stable_bilingual_bindings_and_restore_sample_assets() {
    layout_contract::assert_localized_bindings::<GeneralSamplerPlugin>(
        GeneralSampler::SPEC.params().len(),
    );
    sampler_contract::sample_asset_roundtrip::<GeneralSamplerPlugin>(60, "mode");
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<GeneralSampler>();
}
