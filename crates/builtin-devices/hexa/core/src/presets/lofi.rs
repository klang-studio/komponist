//! Distorted and lo-fi sounds: the master distortion and bitcrusher.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Fuzz Guitar", 0.489, fuzz_guitar),
    entry("Lo-Fi Piano", 0.346, lofi_piano),
    entry("8-Bit Bass", 0.605, bit_bass),
    entry("Fold Lead", 0.504, fold_lead),
    entry("Industrial", 0.479, industrial),
    entry("Broken Radio", 1.000, broken_radio),
    entry("Tape Pad", 0.676, tape_pad),
];

fn fuzz_guitar() -> Patch {
    // Two detuned saws and a square an octave down into hard fuzz; the tone
    // control and a low-pass take the fizz off. Power chords work: the
    // distortion acts on the sum, like a real amp.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.005, 1.5, 0.6, 0.3)).cents(-6.0),
            saw(1.0, 0.8, adsr(0.005, 1.5, 0.6, 0.3)).cents(6.0),
            square(0.5, 0.4, adsr(0.005, 1.5, 0.6, 0.3)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    p.filter(lowpass(3000.0, 0.2).keytrack(0.4));
    p.distort(HARD, 30.0, 0.45, 0.85);
    p.pan(&[(1, -0.3), (2, 0.3)]);
    p.wet(REVERB, 0.15);
    p.output(0.551);
    p
}

fn lofi_piano() -> Patch {
    // An e-piano (1:1 body, 14:1 tine) through 6 bits at 9 kHz and a
    // chorus: a worn cassette.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.002, 2.8, 0.1, 0.5)),
            sine(1.0, 0.0, adsr(0.002, 1.5, 0.05, 0.5)).dynamic(0.04, 0.14),
            sine(1.0, 0.4, adsr(0.002, 0.7, 0.0, 0.3)),
            sine(14.0, 0.0, adsr(0.001, 0.2, 0.0, 0.1)).dynamic(0.02, 0.08),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.35, -3.0);
    p.crush(6.0, 9000.0, 0.55);
    p.wet(CHORUS, 0.4);
    p.output(0.54);
    p
}

fn bit_bass() -> Patch {
    // A square bass with a square an octave down, mono, at 4 bits.
    let mut p = patch(
        [
            square(1.0, 1.0, adsr(0.002, 0.4, 0.7, 0.08)),
            square(0.5, 0.5, adsr(0.002, 0.4, 0.7, 0.08)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    p.mono(MONO, 0.0);
    p.crush(4.0, 22_050.0, 1.0);
    p
}

fn fold_lead() -> Patch {
    // A sine into the wavefolder: the harder you play, the louder the sine
    // and the more it folds (velocity on the level), from flute-like to
    // bright and buzzy. Legato, with vibrato.
    let mut p = patch(
        [
            sine(1.0, 0.3, adsr(0.01, 0.3, 0.9, 0.2)).dynamic(0.3, 1.0),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    p.distort(FOLD, 14.0, 0.25, 1.0);
    p.mono(LEGATO, 0.04);
    p.vibrato(5.0, 0.1, 0.6);
    p
}

fn industrial() -> Patch {
    // A square ring-modulated by an inharmonic 2.3 and a burst of noise,
    // through hard distortion and the bitcrusher: a metallic stab.
    let mut p = patch(
        [
            square(1.0, 1.0, perc(0.4)),
            sine(2.3, 1.0, perc(0.4)).mode(RING),
            noise(0.3, perc(0.1)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.distort(HARD, 24.0, 0.3, 0.7);
    p.crush(6.0, 8000.0, 0.5);
    p.output(0.411);
    p
}

fn broken_radio() -> Patch {
    // A saw with hiss through a narrow band-pass, 3 bits at 3 kHz, and a
    // sample-and-hold LFO that lets the reception drop out.
    let mut p = patch(
        [
            saw(1.0, 0.8, gate(0.01, 0.2)),
            off(),
            noise(0.25, gate(0.01, 0.2)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 3],
    );
    p.filter(bandpass(1500.0, 0.4));
    p.crush(3.0, 3000.0, 0.6);
    let fading = p.lfo(lfo(SampleHold, 6.0).free());
    p.route(ModTarget::Volume, slot(fading, -0.3));
    p.output(0.717);
    p
}

fn tape_pad() -> Patch {
    // Two detuned saws and a sine sub, slowly swelling, gently saturated
    // like tape, with chorus and a slow pitch wobble ("wow").
    let mut p = patch(
        [
            saw(1.0, 0.6, adsr(0.8, 1.0, 0.85, 1.5)).cents(-5.0),
            saw(1.0, 0.6, adsr(0.8, 1.0, 0.85, 1.5)).cents(5.0),
            sine(0.5, 0.5, adsr(0.8, 1.0, 0.85, 1.5)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    p.filter(lowpass(1800.0, 0.1).keytrack(0.4));
    p.distort(SOFT, 9.0, 0.5, 0.6);
    p.wet(CHORUS, 0.5);
    let wow = p.lfo(lfo(LfoSine, 0.6).free());
    p.route(ModTarget::Tune, slot(wow, 0.08));
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p.output(0.356);
    p
}
