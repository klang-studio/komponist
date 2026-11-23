//! Arpeggiator as an event-only CLAP bundle with its bilingual native layout.

use komponist_bundled_clap_adapter::{Binding, DeviceDefinition, MidiEffect};
use komponist_device_core::Spec;
use komponist_model::params::ParamInfo;
use nice_plug::prelude::*;

struct Arpeggiator;

impl DeviceDefinition for Arpeggiator {
    const SPEC: &'static Spec = &komponist_arpeggiator::SPEC;
    const NAME: &'static str = "Arpeggiator";
    const ID: &'static str = "org.klang-studio.komponist.arpeggiator";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const FEATURES: &'static [ClapFeature] = &[ClapFeature::NoteEffect];

    fn choice(info: &'static ParamInfo) -> Option<Binding> {
        match info.id.as_str() {
            "mode" => Some(Binding::choice::<Mode>(info)),
            "rate" => Some(Binding::choice::<Rate>(info)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Mode {
    #[id = "up"]
    #[name = "Up"]
    Up,
    #[id = "down"]
    #[name = "Down"]
    Down,
    #[id = "up-down"]
    #[name = "Up/Down"]
    UpDown,
    #[id = "random"]
    #[name = "Random"]
    Random,
    #[id = "as-played"]
    #[name = "As Played"]
    AsPlayed,
}

#[derive(Clone, Copy, Enum, PartialEq)]
enum Rate {
    #[id = "quarter"]
    #[name = "1/4"]
    Quarter,
    #[id = "quarter-triplet"]
    #[name = "1/4T"]
    QuarterTriplet,
    #[id = "eighth"]
    #[name = "1/8"]
    Eighth,
    #[id = "eighth-triplet"]
    #[name = "1/8T"]
    EighthTriplet,
    #[id = "sixteenth"]
    #[name = "1/16"]
    Sixteenth,
    #[id = "sixteenth-triplet"]
    #[name = "1/16T"]
    SixteenthTriplet,
    #[id = "thirty-second"]
    #[name = "1/32"]
    ThirtySecond,
    #[id = "thirty-second-triplet"]
    #[name = "1/32T"]
    ThirtySecondTriplet,
}

type ArpeggiatorPlugin = MidiEffect<Arpeggiator>;

impl komponist_bundled_clap_adapter::MidiDefinition for Arpeggiator {
    fn event_stage() -> Box<dyn komponist_engine::EventProcessor + Send> {
        komponist_arpeggiator::stage()
    }
}

nice_export_clap!(ArpeggiatorPlugin);

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
        layout_contract::assert_localized_bindings::<ArpeggiatorPlugin>(
            Arpeggiator::SPEC.params().len(),
        );
    }

    #[test]
    fn clap_note_ports_match_the_builtin_stage() {
        midi_contract::assert_stage_parity::<ArpeggiatorPlugin>(
            Arpeggiator::SPEC,
            komponist_arpeggiator::stage,
        );
    }
    #[test]
    fn restores_the_same_parameter_id_and_value_in_another_instance() {
        midi_contract::assert_state_round_trip::<ArpeggiatorPlugin>("mode");
    }
}

#[cfg(test)]
#[test]
fn choice_bindings_match_the_model() {
    komponist_bundled_clap_adapter::assert_choice_bindings::<Arpeggiator>();
}
