//! The operator wavefolder: plain sines and triangles whose overtones open
//! and close with envelopes, velocity and LFOs (West Coast style).

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("West Coast Pluck", 0.387, west_coast_pluck),
    entry("Fold Bass", 0.545, fold_bass),
    entry("Buchla Bongo", 0.395, bongo),
    entry("Sweep Fold Lead", 0.500, fold_sweep_lead),
    entry("Fold Pad", 0.520, fold_pad),
];

fn west_coast_pluck() -> Patch {
    // A sine whose fold snaps open with each note and closes faster than
    // the sound decays: bright, woody attack, round tail. Velocity adds a
    // little base fold, so hard notes stay brighter.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.6, 0.0, 0.6)).fold(0.04),
            sine(2.0, 0.25, adsr(0.001, 1.2, 0.0, 0.5))
                .fold(0.1)
                .cents(4.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let snap = p.env(perc(0.25));
    p.route(ModTarget::OpFold(0), slot(snap, 0.55));
    p.route(ModTarget::OpFold(1), velocity(0.3));
    p.struck(0.3, 0.0);
    p.pan(&[(1, -0.15), (2, 0.25)]);
    p.wet(DELAY, 0.2);
    p.wet(REVERB, 0.2);
    p
}

fn fold_bass() -> Patch {
    // Mono bass: a folded sine for the growl, opening with a short
    // envelope, over a clean sine sub an octave down. A 24 dB low-pass
    // keeps the folds from fizzing on high notes.
    let mut p = patch(
        [
            sine(1.0, 0.9, adsr(0.002, 0.8, 0.7, 0.1)).fold(0.08),
            sine(0.5, 0.8, adsr(0.002, 0.8, 0.8, 0.1)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let growl = p.env(adsr(0.002, 0.3, 0.15, 0.1));
    p.route(ModTarget::OpFold(0), slot(growl, 0.4));
    p.filter(lowpass(2500.0, 0.15).db24().keytrack(0.5));
    p.mono(MONO, 0.0);
    p
}

fn bongo() -> Patch {
    // The classic Buchla bongo: a sine with a fast pitch drop, folded only
    // by the touch. Soft notes thud, hard ones knock.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.18))
                .fold(0.02)
                .fold_mod(velocity(0.45)),
            sine(1.5, 0.3, perc(0.08)).fold(0.2),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let drop = p.env(perc(0.03));
    p.route(ModTarget::OpPitch(0), slot(drop, 7.0));
    p.route(ModTarget::OpPitch(1), slot(drop, 7.0));
    p.struck(0.2, 0.0);
    p.pan(&[(2, 0.3)]);
    p.wet(REVERB, 0.15);
    p
}

fn fold_sweep_lead() -> Patch {
    // Legato lead: a folded triangle swept slowly by an LFO, phase-modulated
    // by a folded sine an octave up, so the modulating signal is itself rich
    // in overtones. Vibrato fades in.
    let mut p = patch(
        [
            tri(1.0, 1.0, adsr(0.01, 0.4, 0.9, 0.2)).fold(0.3),
            sine(2.0, 0.04, gate(0.01, 0.2)).fold(0.4),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let sweep = p.lfo(lfo(LfoTriangle, 0.35));
    p.route(ModTarget::OpFold(0), slot(sweep, 0.25));
    p.mono(LEGATO, 0.05);
    p.vibrato(5.2, 0.12, 0.5);
    p.wet(DELAY, 0.25);
    p
}

fn fold_pad() -> Patch {
    // Two detuned sines that fold more and more over a few seconds while
    // held, in unison and through chorus and reverb: from soft to glassy.
    let mut p = patch(
        [
            sine(1.0, 0.8, adsr(1.2, 2.0, 0.9, 2.5)).cents(-5.0),
            sine(1.0, 0.8, adsr(1.2, 2.0, 0.9, 2.5)).cents(5.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let bloom = p.env(adsr(3.5, 1.0, 1.0, 2.5));
    p.route(ModTarget::OpFold(0), slot(bloom, 0.3));
    p.route(ModTarget::OpFold(1), slot(bloom, 0.35));
    p.pan(&[(1, -0.5), (2, 0.5)]);
    p.unison(2, 8.0, 0.6);
    p.wet(CHORUS, 0.4);
    p.wet(REVERB, 0.4);
    p.output(0.537);
    p
}
