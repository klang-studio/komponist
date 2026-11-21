//! Volt using its existing voice engine and public native CLAP parameter layout.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, Instrument};
use komponist_device_core::{
    Spec,
    layout::{knob, param, toggle},
};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, Page, WidgetKind};
use komponist_native_clap_sdk::Panel;
use nice_plug::prelude::*;

struct Volt;

macro_rules! stable_choice {
    ($name:ident { $($variant:ident => $id:literal, $text:literal);+ $(;)? }) => {
        #[derive(Clone, Copy, Enum, PartialEq)]
        enum $name {
            $(#[id = $id] #[name = $text] $variant),+
        }
    };
}

stable_choice!(Wave {
    Sine => "sine", "Sine";
    Triangle => "triangle", "Triangle";
    Saw => "saw", "Saw";
    Square => "square", "Square";
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
stable_choice!(ModTarget {
    Off => "off", "Off";
    Pitch => "pitch", "Pitch";
    PitchB => "pitch-b", "Osc B Pitch";
    WidthA => "width-a", "Width A";
    WidthB => "width-b", "Width B";
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

impl DeviceDefinition for Volt {
    const SPEC: &'static Spec = &komponist_volt::SPEC;
    const NAME: &'static str = komponist_volt::NAME;
    const ID: &'static str = "org.klang-studio.komponist.volt";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[
        ClapFeature::Instrument,
        ClapFeature::Synthesizer,
        ClapFeature::Stereo,
    ];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        Some(match info.id.as_str() {
            "osc_a_wave" | "osc_b_wave" => Binding::choice::<Wave>(info),
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
        volt_layout(lang)
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

fn volt_layout(lang: Lang) -> DeviceLayout {
    let [mut sound, mut envelopes, mut matrix]: [Page; 3] = Volt::SPEC
        .layout_in(lang)
        .pages
        .clone()
        .try_into()
        .expect("Volt has three builtin pages");
    // Public v1 supports fixed parameters. Keep every control, including the dedicated
    // tempo divisions. The builtin signal/filter/envelope/LFO graphs require layout
    // capabilities or an editor before Volt can replace the builtin device.
    sound
        .cells
        .retain(|cell| matches!(cell.param, CellParam::Id(_)));
    sound.dividers.clear();
    matrix.dividers.clear();
    envelopes.cells.clear();
    for (row, (env, _)) in komponist_volt::ENVELOPES.iter().enumerate() {
        for (col, suffix) in ["a", "d", "s", "r"].into_iter().enumerate() {
            envelopes
                .cells
                .push(knob(col as u8, row as u8, &format!("{env}_{suffix}")));
        }
    }
    envelopes.cols = 4;
    envelopes.dividers.clear();

    let mut lfo = Page {
        name: "LFOs".into(),
        cols: 9,
        cells: Vec::new(),
        dividers: Vec::new(),
    };
    for (row, (number, _)) in komponist_volt::LFOS.iter().enumerate() {
        let row = row as u8;
        let id = |suffix| format!("lfo{number}_{suffix}");
        let dropdown = |col, suffix| {
            Cell::half(
                col,
                row,
                2,
                Half::Top,
                WidgetKind::Dropdown,
                param(&id(suffix)),
            )
        };
        lfo.cells.extend([
            dropdown(0, "shape"),
            toggle(2, row, 1, Half::Top, param(&id("sync"))),
            dropdown(3, "div"),
            toggle(5, row, 1, Half::Top, param(&id("key"))),
            toggle(6, row, 1, Half::Top, param(&id("uni"))),
            knob(7, row, &id("rate")),
            knob(8, row, &id("fade")),
        ]);
    }
    DeviceLayout {
        pages: vec![sound, envelopes, lfo, matrix],
    }
}

type VoltPlugin = Instrument<Volt>;

nice_export_clap!(VoltPlugin);

#[cfg(test)]
mod tests;
