//! Surf using volt's voice engine with its wavetable oscillators and the public native CLAP
//! parameter layout.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, Instrument};
use komponist_device_core::Spec;
use komponist_device_core::layout::{knob, param};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, Page, WidgetKind};
use komponist_native_clap_sdk::Panel;
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
    FourBars => "4-bars", "4 bars";
    TwoBars => "2-bars", "2 bars";
    OneBar => "1-bar", "1 bar";
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
    Envelope => "env-3", "Env. 3";
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

    fn layout(lang: Lang) -> DeviceLayout {
        surf_layout(lang)
    }

    fn configure_panel(panel: &mut Panel) {
        for (id, labels) in &mut panel.parameter_labels {
            for (prefix, group) in [
                ("amp_", "Amp"),
                ("filt_", "Filter"),
                ("mod_", "Mod"),
                ("lfo1_", "LFO 1"),
                ("lfo2_", "LFO 2"),
            ] {
                if id.as_str().starts_with(prefix) {
                    for label in labels.values_mut() {
                        *label = format!("{group} {label}");
                    }
                    break;
                }
            }
        }
    }
}

/// The public v1 layout: fixed parameters only, so the first page shows both oscillators side by
/// side where surf's card has the wavetable graph and the A/B switch; envelopes and LFOs are volt's
/// public pages; the matrix page is surf's.
fn surf_layout(lang: Lang) -> DeviceLayout {
    let [mut sound, _, mut matrix]: [Page; 3] = Surf::SPEC
        .layout_in(lang)
        .pages
        .clone()
        .try_into()
        .expect("Surf has three builtin pages");
    sound
        .cells
        .retain(|cell| matches!(cell.param, CellParam::Id(_)));
    let half =
        |col, row, cols, half, widget, id| Cell::half(col, row, cols, half, widget, param(id));
    sound.cells.extend([
        half(0, 0, 2, Half::Top, WidgetKind::Dropdown, "osc_a_table"),
        half(0, 0, 2, Half::Bottom, WidgetKind::Dropdown, "osc_b_table"),
        knob(2, 0, "osc_a_pos"),
        knob(3, 0, "osc_b_pos"),
        half(0, 1, 1, Half::Top, WidgetKind::Number, "osc_a_oct"),
        half(0, 1, 1, Half::Bottom, WidgetKind::Number, "osc_b_oct"),
        knob(1, 1, "osc_a_fine"),
        knob(2, 1, "osc_b_fine"),
    ]);
    sound.dividers.clear();
    matrix.dividers.clear();
    DeviceLayout {
        pages: vec![
            sound,
            komponist_volt::public_envelope_page(lang),
            komponist_volt::public_lfo_page(),
            matrix,
        ],
    }
}

type SurfPlugin = Instrument<Surf>;

impl komponist_bundled_clap_adapter::AudioDefinition for Surf {
    fn dsp() -> Box<dyn komponist_device_core::Dsp> {
        komponist_surf::dsp()
    }
}

nice_export_clap!(SurfPlugin);

#[cfg(test)]
mod tests;
