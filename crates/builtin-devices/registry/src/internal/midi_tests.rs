//! Registration of the MIDI effects and their event stages in the built-in catalog.

use komponist_engine::Processor;

use super::Internal;

#[test]
fn midi_effects_are_registered_with_their_roles_and_german_parameter_names() {
    assert_eq!(
        Internal::Arpeggiator.role(),
        komponist_model::DeviceRole::MidiEffect
    );
    assert_eq!(
        Internal::Tonleiter.role(),
        komponist_model::DeviceRole::MidiEffect
    );
    assert!(!Internal::Eq.is_midi_effect());
    let names =
        |kind: Internal| -> Vec<String> { kind.params().iter().map(|p| p.name.clone()).collect() };
    assert_eq!(
        names(Internal::Arpeggiator),
        ["Muster", "Rate", "Gate", "Oktaven", "Halten"]
    );
    assert_eq!(
        names(Internal::Tonleiter),
        ["Grundton", "Tonleiter", "Modus"]
    );
    // The processors are MIDI effects: they have a stage, the audio effects have none.
    for kind in [Internal::Arpeggiator, Internal::Tonleiter] {
        assert!(kind.processor(&[]).event_processor().is_some());
    }
    assert!(Internal::Eq.processor(&[]).event_processor().is_none());
}
