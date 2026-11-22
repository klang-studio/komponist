//! The eight-pad drum sampler packaged as a native CLAP instrument.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition};
use komponist_device_core::layout::{knob, param, toggle};
use komponist_device_core::{Dsp, Spec};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{Cell, DeviceLayout, Half, Page, WidgetKind};
use komponist_sampler::PADS;
use komponist_sampler_clap::{Sampled, SampledDefinition};
use komponist_sampler_shared::{ManagedLoader, SamplerHandle};
use nice_plug::prelude::*;

struct DrumSampler;

#[derive(Clone, Copy, Enum, PartialEq)]
enum Mode {
    #[id = "one-shot"]
    #[name = "One shot"]
    OneShot,
    #[id = "gate"]
    Gate,
}

impl DeviceDefinition for DrumSampler {
    const SPEC: &'static Spec = &komponist_sampler::SPEC;
    const NAME: &'static str = komponist_sampler::NAME;
    const ID: &'static str = "org.klang-studio.komponist.sampler";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Drum,
        ClapFeature::Sampler,
        ClapFeature::Stereo,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        info.id
            .as_str()
            .ends_with("_mode")
            .then(|| Binding::choice::<Mode>(info))
    }

    fn layout(_: Lang) -> DeviceLayout {
        DeviceLayout {
            pages: (1..=PADS)
                .step_by(2)
                .map(|first| {
                    let mut cells = Vec::new();
                    for (pad, col) in [(first, 0), (first + 1, 5)] {
                        let key = |suffix: &str| format!("pad{pad}_{suffix}");
                        cells.extend([
                            knob(col, 0, &key("start")),
                            knob(col + 1, 0, &key("end")),
                            Cell::half(
                                col + 2,
                                0,
                                2,
                                Half::Top,
                                WidgetKind::Dropdown,
                                param(&key("mode")),
                            ),
                            toggle(col + 2, 0, 1, Half::Bottom, param(&key("mute"))),
                            toggle(col + 3, 0, 1, Half::Bottom, param(&key("solo"))),
                            Cell::half(
                                col + 4,
                                0,
                                1,
                                Half::Top,
                                WidgetKind::Number,
                                param(&key("choke")),
                            ),
                            knob(col, 1, &key("tune")),
                            knob(col + 1, 1, &key("decay")),
                            knob(col + 2, 1, &key("drive")),
                            knob(col + 3, 1, &key("pan")),
                            knob(col + 4, 1, &key("level")),
                        ]);
                    }
                    if first == 1 {
                        cells.extend([
                            knob(0, 2, "kit_tune"),
                            toggle(1, 2, 2, Half::Top, param("follow")),
                        ]);
                    }
                    Page {
                        name: format!("Pads {first}–{}", first + 1),
                        cols: 10,
                        cells,
                        dividers: Vec::new(),
                    }
                })
                .collect(),
        }
    }
}

impl SampledDefinition for DrumSampler {
    const PADS: usize = PADS;

    fn sampler() -> (Box<dyn Dsp>, SamplerHandle, ManagedLoader) {
        komponist_sampler::dsp_with_managed_loader()
    }
}

type DrumSamplerPlugin = Sampled<DrumSampler>;

nice_export_clap!(DrumSamplerPlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[path = "../../../sampler-shared/clap_contract.rs"]
mod sampler_contract;

#[cfg(test)]
#[test]
fn all_pads_have_stable_bilingual_bindings_and_restore_sample_links() {
    layout_contract::assert_localized_bindings::<DrumSamplerPlugin>(
        DrumSampler::SPEC.params().len(),
    );
    sampler_contract::sample_asset_roundtrip::<DrumSamplerPlugin>(
        komponist_sampler::FIRST_NOTE,
        "pad1_mode",
    );
}
