//! Surf using volt's voice engine with its wavetable oscillators and the public native CLAP
//! parameter layout.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, Instrument, panel_from_parts};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::layout::{CellParam, GraphConfig};
use komponist_model::params::{ParamId, ParamInfo};
use komponist_native_clap_sdk::{GraphLabels, Labels, Panel};
use nice_plug::prelude::*;

struct Surf;

macro_rules! stable_choice {
    ($name:ident { $($variant:ident => $id:literal, $text:literal);+ $(;)? }) => {
        #[derive(Clone, Copy, Enum, PartialEq)]
        enum $name {
            $(#[id = $id] #[name = $text] $variant),+
        }
    };
}

// The factory tables in the order of `komponist_model::params::wavetable::TABLES` (append only).
stable_choice!(Table {
    Basic => "basic", "Basic Shapes";
    Pulse => "pulse", "Pulse Width";
    Sync => "sync", "Sync";
    Harmonics => "harmonics", "Harmonics";
    OddEven => "odd-even", "Odd–Even";
    Organ => "organ", "Organ";
    Vowels => "vowels", "Vowels";
    Choir => "choir", "Choir";
    Fm => "fm", "FM";
    Fold => "fold", "Fold";
    Bits => "bits", "Bits";
    Comb => "comb", "Comb";
    Glass => "glass", "Glass";
    Hollow => "hollow", "Hollow";
    Resonance => "resonance", "Resonance";
    Random => "random", "Random";
});
stable_choice!(Filter {
    Low24 => "lp-24", "LP 24";
    Low12 => "lp-12", "LP 12";
    Band12 => "bp-12", "BP 12";
    High12 => "hp-12", "HP 12";
    Notch => "notch", "Notch";
});
stable_choice!(LfoShape {
    Sine => "sine", "Sine";
    Triangle => "triangle", "Triangle";
    Saw => "saw", "Saw";
    Square => "square", "Square";
    Random => "random", "Random";
    Smooth => "smooth", "Smooth";
});
stable_choice!(LfoDivision {
    FourBars => "4-bars", "4 Bars";
    TwoBars => "2-bars", "2 Bars";
    OneBar => "1-bar", "1 Bar";
    Half => "1-2", "1/2";
    Quarter => "1-4", "1/4";
    Eighth => "1-8", "1/8";
    Sixteenth => "1-16", "1/16";
    ThirtySecond => "1-32", "1/32";
});
stable_choice!(ModSource {
    Off => "off", "Off";
    Lfo1 => "lfo-1", "LFO 1";
    Lfo2 => "lfo-2", "LFO 2";
    Envelope => "env-3", "Env 3";
    Velocity => "velocity", "Velocity";
    Key => "key", "Key";
    Wheel => "wheel", "Mod Wheel";
    Pressure => "pressure", "Pressure";
    Random => "random", "Random";
});
// Volt's destinations; the pulse widths are surf's table positions.
stable_choice!(ModTarget {
    Off => "off", "Off";
    Pitch => "pitch", "Pitch";
    PitchB => "pitch-b", "Osc B Pitch";
    PositionA => "position-a", "Position A";
    PositionB => "position-b", "Position B";
    OscMix => "osc-mix", "Mix A/B";
    Cutoff => "cutoff", "Cutoff";
    Resonance => "resonance", "Resonance";
    Drive => "drive", "Drive";
    Volume => "volume", "Volume";
    Detune => "detune", "Detune";
    LfoRate1 => "lfo-1-rate", "LFO 1 Rate";
    LfoRate2 => "lfo-2-rate", "LFO 2 Rate";
});
stable_choice!(VoiceMode {
    Poly => "poly", "Poly";
    Mono => "mono", "Mono";
    Legato => "legato", "Legato";
});

impl DeviceDefinition for Surf {
    const PUBLISH_FACTORY_PRESETS: bool = true;
    const SPEC: &'static Spec = &komponist_surf::SPEC;
    const NAME: &'static str = komponist_surf::NAME;
    const ID: &'static str = "org.klang-studio.komponist.surf";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        Some(match info.id.as_str() {
            "osc_a_table" | "osc_b_table" => Binding::choice::<Table>(info),
            "filter_type" => Binding::choice::<Filter>(info),
            "lfo1_shape" | "lfo2_shape" => Binding::choice::<LfoShape>(info),
            "lfo1_div" | "lfo2_div" => Binding::choice::<LfoDivision>(info),
            "mod1_src" | "mod2_src" | "mod3_src" | "mod4_src" => Binding::choice::<ModSource>(info),
            "mod1_dst" | "mod2_dst" | "mod3_dst" | "mod4_dst" => Binding::choice::<ModTarget>(info),
            "mode" => Binding::choice::<VoiceMode>(info),
            _ => return None,
        })
    }
}

type SurfPlugin = Instrument<Surf>;

impl komponist_bundled_clap_adapter::AudioDefinition for Surf {
    fn authored_panel() -> Option<Panel> {
        let english = Self::SPEC.layout_in(Lang::En);
        let german = Self::SPEC.layout_in(Lang::De);
        let mut panel = panel_from_parts(
            |lang| Self::SPEC.layout_in(lang).clone(),
            Self::SPEC.params_in(Lang::En),
            Self::SPEC.params_in(Lang::De),
        );
        let labels = |en: &str, de: &str| -> Labels {
            [("en".into(), en.into()), ("de".into(), de.into())].into()
        };
        let mut graph_labels = std::collections::BTreeMap::new();
        for (page_index, (en, de)) in english.pages.iter().zip(&german.pages).enumerate() {
            for (cell_index, (en, de)) in en.cells.iter().zip(&de.cells).enumerate() {
                match (&en.param, &de.param) {
                    (
                        CellParam::Choice {
                            key,
                            label: en,
                            options: en_options,
                        },
                        CellParam::Choice {
                            label: de,
                            options: de_options,
                            ..
                        },
                    ) => {
                        let key = ParamId::new(*key);
                        panel.parameter_labels.insert(key.clone(), labels(en, de));
                        panel.choice_labels.insert(
                            key,
                            en_options
                                .iter()
                                .zip(de_options)
                                .map(|((_, en), (_, de))| labels(en, de))
                                .collect(),
                        );
                    }
                    (CellParam::Graph(graph), _) => {
                        let text = |lang: Lang| match graph.config {
                            GraphConfig::Envelope => (
                                lang.graph_title_volt_envelope("{selection}"),
                                lang.ui_graph_envelope(),
                            ),
                            GraphConfig::LfoShape => {
                                (lang.ui_graph_lfo_shape().into(), lang.ui_graph_lfo_shape())
                            }
                            GraphConfig::Wavetable => {
                                (lang.ui_graph_wavetable().into(), lang.ui_graph_wavetable())
                            }
                            GraphConfig::FilterVa => {
                                (lang.ui_graph_filter().into(), lang.ui_graph_filter())
                            }
                            _ => unreachable!("Surf graph"),
                        };
                        let (en, en_description) = text(Lang::En);
                        let (de, de_description) = text(Lang::De);
                        graph_labels.insert(
                            (page_index, cell_index),
                            GraphLabels {
                                name: labels(&en, &de),
                                description: labels(en_description, de_description),
                                hide_title: false,
                                signal_taps: Default::default(),
                                asset_slot: None,
                                waveform_markers: false,
                                waveform_toolbar: false,
                                waveform_slice_controls: None,
                                audition_note: None,
                                trim_unit: None,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
        Some(Panel {
            graph_labels,
            canvases: Default::default(),
            reduction_taps: Default::default(),
            headings: Default::default(),
            pad_bank: None,
            full_band_choice_labels: Default::default(),
            ..panel
        })
    }

    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_surf::dsp()
    }
}

komponist_bundled_clap_adapter::export_spec_with_presets!(Surf, SurfPlugin);

#[cfg(test)]
mod tests;

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Surf>();
}
