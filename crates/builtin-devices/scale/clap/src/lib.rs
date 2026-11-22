//! Scale as an event-only CLAP bundle with its bilingual native layout.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, MidiEffect};
use komponist_device_core::Spec;
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_model::params::layout::{DeviceLayout, WidgetKind};
use nice_plug::prelude::*;

struct Scale;

impl DeviceDefinition for Scale {
    const SPEC: &'static Spec = &komponist_scale::SPEC;
    const NAME: &'static str = "Scale";
    const ID: &'static str = "org.klang-studio.komponist.scale";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[ClapFeature::NoteEffect];

    fn layout(lang: Lang) -> DeviceLayout {
        // Public layout v1 permits only two-column dropdowns. The built-in editor uses
        // wider dropdowns for Scale and Mode; all three controls remain available.
        let mut layout = Self::SPEC.layout_in(lang).clone();
        for cell in layout.pages.iter_mut().flat_map(|page| &mut page.cells) {
            if cell.widget == WidgetKind::Dropdown {
                cell.cols = 2;
            }
        }
        layout
    }

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        match info.id.as_str() {
            "root" => Some(Binding::choice::<Root>(info)),
            "scale" => Some(Binding::choice::<ScaleType>(info)),
            "mode" => Some(Binding::choice::<Mode>(info)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Root {
    #[id = "c"]
    #[name = "C"]
    C,
    #[id = "c-sharp"]
    #[name = "C#"]
    CSharp,
    #[id = "d"]
    #[name = "D"]
    D,
    #[id = "d-sharp"]
    #[name = "D#"]
    DSharp,
    #[id = "e"]
    #[name = "E"]
    E,
    #[id = "f"]
    #[name = "F"]
    F,
    #[id = "f-sharp"]
    #[name = "F#"]
    FSharp,
    #[id = "g"]
    #[name = "G"]
    G,
    #[id = "g-sharp"]
    #[name = "G#"]
    GSharp,
    #[id = "a"]
    #[name = "A"]
    A,
    #[id = "a-sharp"]
    #[name = "A#"]
    ASharp,
    #[id = "b"]
    #[name = "B"]
    B,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum ScaleType {
    #[id = "major"]
    #[name = "Major"]
    Major,
    #[id = "minor"]
    #[name = "Minor"]
    Minor,
    #[id = "harmonic-minor"]
    #[name = "Harmonic Minor"]
    HarmonicMinor,
    #[id = "melodic-minor"]
    #[name = "Melodic Minor"]
    MelodicMinor,
    #[id = "dorian"]
    #[name = "Dorian"]
    Dorian,
    #[id = "phrygian"]
    #[name = "Phrygian"]
    Phrygian,
    #[id = "lydian"]
    #[name = "Lydian"]
    Lydian,
    #[id = "mixolydian"]
    #[name = "Mixolydian"]
    Mixolydian,
    #[id = "locrian"]
    #[name = "Locrian"]
    Locrian,
    #[id = "pentatonic-major"]
    #[name = "Major Pentatonic"]
    PentatonicMajor,
    #[id = "pentatonic-minor"]
    #[name = "Minor Pentatonic"]
    PentatonicMinor,
    #[id = "blues"]
    #[name = "Blues"]
    Blues,
    #[id = "whole-tone"]
    #[name = "Whole Tone"]
    WholeTone,
    #[id = "chromatic"]
    #[name = "Chromatic"]
    Chromatic,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Mode {
    #[id = "nearest"]
    #[name = "Nearest"]
    Nearest,
    #[id = "up"]
    #[name = "Up"]
    Up,
    #[id = "down"]
    #[name = "Down"]
    Down,
    #[id = "filter"]
    #[name = "Filter"]
    Filter,
}

type ScalePlugin = MidiEffect<Scale>;

impl komponist_bundled_clap_adapter::MidiDefinition for Scale {
    fn event_stage() -> Box<dyn komponist_engine::EventProcessor + Send> {
        komponist_scale::stage()
    }
}

nice_export_clap!(ScalePlugin);

#[cfg(test)]
#[path = "../../../layout_contract.rs"]
mod layout_contract;

#[cfg(test)]
#[path = "../../../midi_contract.rs"]
mod midi_contract;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_localized_stable_bindings() {
        layout_contract::assert_localized_bindings::<ScalePlugin>(Scale::SPEC.params().len());
    }

    #[test]
    fn clap_note_ports_match_the_builtin_stage() {
        midi_contract::assert_stage_parity::<ScalePlugin>(Scale::SPEC, komponist_scale::stage);
    }
    #[test]
    fn restores_the_same_parameter_id_and_value_in_another_instance() {
        midi_contract::assert_state_round_trip::<ScalePlugin>("root");
    }
}
