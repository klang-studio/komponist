//! Device state and preset switching through the processor.

use komponist_engine::Processor;
use komponist_hexa::{
    HexaProcessor, Macro, decode_state, default_patch, encode_state, preset_patch, presets,
};

use crate::common::*;

/// Plays a short phrase and returns the output.
fn phrase(p: &mut HexaProcessor) -> Out {
    let events = [
        (0, note_on(57)),
        (0, note_on(64)),
        (frames(0.15), note_off(57)),
        (frames(0.15), note_off(64)),
    ];
    play(p, frames(0.3), 256, &events)
}

#[test]
fn preset_switch_by_state() {
    let piano = presets().find(|p| p.name == "E-Piano").unwrap();
    let bass = presets().find(|p| p.name == "FM Bass").unwrap();
    assert_eq!((piano.category, bass.category), ("Tasten", "Bass"));

    // The device starts with E-Piano …
    let state = encode_state(&preset_patch(piano.index).unwrap());
    let mut device = HexaProcessor::from_state(Some(&state)).unwrap();
    device.activate(RATE, MAX_BLOCK).unwrap();
    let before = phrase(&mut device);
    assert_eq!(*device.patch(), preset_patch(piano.index).unwrap());

    // … and switches to FM Bass: a new processor from the new state replaces it.
    let state = encode_state(&preset_patch(bass.index).unwrap());
    let mut device = HexaProcessor::from_state(Some(&state)).unwrap();
    device.activate(RATE, MAX_BLOCK).unwrap();
    assert_eq!(*device.patch(), preset_patch(bass.index).unwrap());
    let after = phrase(&mut device);

    // It sounds like FM Bass, not like E-Piano.
    let reference = phrase(&mut activated(preset_patch(bass.index).unwrap()));
    assert_eq!(after, reference);
    assert!(after.peak(0..after.len()) > 0.05);
    assert_ne!(after, before);
}

#[test]
fn every_preset_plays_from_its_state() {
    for info in presets() {
        let state = encode_state(&preset_patch(info.index).unwrap());
        let mut p = HexaProcessor::from_state(Some(&state)).unwrap();
        p.activate(RATE, MAX_BLOCK).unwrap();
        let out = play(&mut p, frames(0.2), 512, &[(0, note_on(60))]);
        assert!(out.peak(0..out.len()) > 1e-3, "{} is silent", info.name);
    }
}

#[test]
fn state_round_trips_through_the_processor() {
    // No state: Init.
    let p = HexaProcessor::from_state(None).unwrap();
    assert_eq!(*p.patch(), default_patch());
    assert!(HexaProcessor::from_state(Some(b"not json")).is_err());

    // Macro changes received as events end up in the state the processor's patch encodes to.
    let mut p = activated(preset_patch(20).unwrap());
    let events = [
        (0, note_on(60)),
        (10, param(Macro::Ratio2, 2.5)),
        (10, param(Macro::Decay, 0.6)),
        (700, param(Macro::Algorithm, 19.0)),
    ];
    let _ = play(&mut p, 1024, 512, &events);
    let state = encode_state(p.patch());
    let loaded = decode_state(Some(&state)).unwrap();
    assert_eq!(loaded, *p.patch());
    assert_eq!(Macro::Ratio2.get(&loaded), 2.5);
    assert_eq!(Macro::Algorithm.get(&loaded), 19.0);

    // A processor from that state plays the same.
    let mut a = HexaProcessor::from_state(Some(&state)).unwrap();
    a.activate(RATE, MAX_BLOCK).unwrap();
    let mut b = activated(loaded);
    assert_eq!(phrase(&mut a), phrase(&mut b));
}
