//! Mallets and bells.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Marimba", 0.383, marimba),
    entry("Vibraphone", 0.334, vibraphone),
    entry("Xylophone", 0.315, xylophone),
    entry("Glockenspiel", 0.459, glockenspiel),
    entry("Tubular Bell", 0.451, tubular_bell),
    entry("Church Bell", 0.631, church_bell),
    entry("Gong", 0.466, gong),
    entry("Kalimba", 0.364, kalimba),
    entry("Steel Drum", 0.510, steel_drum),
    entry("Music Box", 0.383, music_box),
    entry("Crystal", 0.389, crystal),
];

fn marimba() -> Patch {
    // Wooden bar: fundamental plus the bar's 3.9 partial, struck by a short
    // 3.5:1 modulator that gets harder with the touch.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.7, 0.0, 0.4)),
            sine(3.5, 0.0, adsr(0.0005, 0.06, 0.0, 0.05)).dynamic(0.05, 0.2),
            sine(3.9, 0.25, adsr(0.001, 0.2, 0.0, 0.1)),
            sine(1.0, 0.05, adsr(0.001, 0.1, 0.0, 0.1)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.5, -3.0);
    p
}

fn vibraphone() -> Patch {
    // Long fundamental, the 4:1 partial, a soft strike; the motor is a free
    // tremolo.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 4.0, 0.0, 1.5)),
            sine(1.0, 0.0, adsr(0.001, 0.08, 0.0, 0.1)).dynamic(0.03, 0.12),
            sine(4.0, 0.35, adsr(0.001, 1.2, 0.0, 0.8)),
            sine(1.0, 0.03, adsr(0.001, 0.2, 0.0, 0.1)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let motor = p.lfo(lfo(LfoSine, 5.0).free());
    p.route(ModTarget::Volume, slot(motor, 0.2));
    p.route(ModTarget::Balance, slot(motor, 0.3));
    p.struck(0.3, -3.0);
    p
}

fn xylophone() -> Patch {
    // An octave up (semitone mode), short, with a hard 3:1 strike.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.0005, 0.35, 0.0, 0.2)).semitones(12.0),
            sine(6.0, 0.0, adsr(0.0005, 0.05, 0.0, 0.04)).dynamic(0.08, 0.25),
            sine(6.0, 0.4, adsr(0.0005, 0.15, 0.0, 0.1)),
            sine(12.0, 0.15, adsr(0.0005, 0.06, 0.0, 0.05)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.5, -3.0);
    p
}

fn glockenspiel() -> Patch {
    // Steel bars two octaves up (semitone mode, +24) with their inharmonic
    // partials 2.76 and 5.4 (as ratios of the note: × 4).
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.0005, 1.8, 0.0, 1.0)).semitones(24.0),
            sine(8.0, 0.0, adsr(0.0005, 0.04, 0.0, 0.04)).dynamic(0.02, 0.1),
            sine(11.04, 0.3, adsr(0.0005, 0.8, 0.0, 0.5)),
            sine(21.6, 0.12, adsr(0.0005, 0.4, 0.0, 0.3)),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3, 4],
    );
    p.struck(0.3, -2.0);
    p
}

fn tubular_bell() -> Patch {
    // Strike tone from a 3.5:1 modulator, the hum an octave down, and an
    // inharmonic 2.4 partial; long decays.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 5.0, 0.0, 3.0)),
            sine(3.5, 0.0, adsr(0.001, 2.0, 0.0, 2.0)).dynamic(0.08, 0.2),
            sine(0.5, 0.4, adsr(0.002, 6.0, 0.0, 3.0)),
            sine(2.0, 0.1, adsr(0.001, 3.0, 0.0, 2.0)),
            sine(2.4, 0.3, adsr(0.001, 3.5, 0.0, 2.5)),
            sine(5.6, 0.06, adsr(0.001, 1.0, 0.0, 1.0)),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    // Partials from slightly different places, like a large bell.
    p.pan(&[(3, -0.3), (5, 0.35)]);
    p.struck(0.3, -2.0);
    p
}

fn church_bell() -> Patch {
    // Minor-third bell: hum 0.5, prime 1, minor third 1.2, fifth 1.5,
    // nominal 2; op 6 (2.83, √8) adds shimmer, mostly to the nominal (the
    // prime gets half, so the strike note stays clear).
    let mut p = patch(
        [
            sine(0.5, 0.8, adsr(0.001, 7.0, 0.0, 4.0)),
            sine(1.0, 0.7, adsr(0.001, 5.0, 0.0, 3.5)),
            sine(1.2, 0.5, adsr(0.001, 4.0, 0.0, 3.0)),
            sine(1.5, 0.35, adsr(0.001, 3.0, 0.0, 2.5)),
            sine(2.0, 0.6, adsr(0.001, 3.0, 0.0, 2.5)),
            sine(2.83, 0.0, adsr(0.001, 1.5, 0.0, 1.5)).dynamic(0.04, 0.12),
        ],
        &[(6, 5), (6, 2)],
        &[1, 2, 3, 4, 5],
    );
    p.depth(6, 2, 0.5);
    // Partials spread: the bell fills the space.
    p.pan(&[(2, -0.2), (3, 0.35), (4, -0.35), (5, 0.2)]);
    p.struck(0.25, -2.0);
    p
}

fn gong() -> Patch {
    // Inharmonic chain 1 ← 1.41 ← 2.83 (with feedback) over a low 0.71
    // layer, and an inharmonic ring modulator (1.73) adding clang to the
    // top; a modulation envelope lets the crash bloom after the strike.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.002, 6.0, 0.0, 4.0)),
            sine(1.41, 0.0, adsr(0.002, 5.0, 0.0, 4.0)),
            sine(2.83, 0.15, adsr(0.002, 4.0, 0.0, 3.0)).fb(0.4),
            sine(0.71, 0.4, adsr(0.002, 7.0, 0.0, 4.0)),
            sine(3.17, 0.08, adsr(0.002, 3.0, 0.0, 3.0)),
            sine(1.73, 0.35, adsr(0.002, 4.0, 0.0, 3.0)).mode(RING),
        ],
        &[(2, 1), (3, 2), (5, 4), (6, 1)],
        &[1, 4],
    );
    let bloom = p.env(adsr(0.6, 3.0, 0.3, 3.0));
    p.route(ModTarget::OpLevel(1), slot(bloom, 0.3));
    p.pan(&[(1, -0.2), (4, 0.3)]);
    p
}

fn kalimba() -> Patch {
    // Tine with a 7:1 click and a 5.4 overtone.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.2, 0.0, 0.6)),
            sine(7.0, 0.0, adsr(0.0005, 0.03, 0.0, 0.03)).dynamic(0.05, 0.2),
            sine(5.4, 0.18, adsr(0.0005, 0.25, 0.0, 0.15)),
            sine(1.0, 0.03, adsr(0.0005, 0.05, 0.0, 0.05)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.4, -3.0);
    p
}

fn steel_drum() -> Patch {
    // Pan partials 1, 2, 3 with a 1.5:1 modulator; the note bends down into
    // pitch from slightly above, like a struck pan.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.6, 0.0, 0.6)),
            sine(1.5, 0.0, adsr(0.001, 0.4, 0.0, 0.3)).dynamic(0.04, 0.12),
            sine(2.0, 0.5, adsr(0.001, 1.0, 0.0, 0.5)),
            sine(1.0, 0.06, adsr(0.001, 0.3, 0.0, 0.2)),
            sine(3.0, 0.2, adsr(0.001, 0.4, 0.0, 0.3)),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3, 5],
    );
    let strike = p.env(perc(0.06));
    p.route(ModTarget::Tune, slot(strike, 0.4));
    p.struck(0.4, -3.0);
    p
}

fn music_box() -> Patch {
    // A comb tooth an octave up (semitone mode): pure, with an inharmonic
    // 4.9 partial and a tiny pluck.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.0005, 2.5, 0.0, 1.2)).semitones(12.0),
            sine(6.0, 0.0, adsr(0.0005, 0.03, 0.0, 0.03)).dynamic(0.02, 0.08),
            sine(9.8, 0.2, adsr(0.0005, 0.6, 0.0, 0.4)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.struck(0.4, -2.0);
    p
}

fn crystal() -> Patch {
    // Glassy 1:3 and 2:7 pairs; the first modulator is a triangle in FM
    // mode, the second shimmers on a slow free LFO.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.002, 3.5, 0.2, 2.5)),
            tri(3.0, 0.0, adsr(0.002, 1.5, 0.1, 2.0))
                .mode(FM)
                .dynamic(0.03, 0.08),
            sine(2.0, 0.5, adsr(0.002, 2.5, 0.1, 2.5)).cents(5.0),
            sine(7.0, 0.04, adsr(0.002, 1.0, 0.05, 1.5)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let shimmer = p.lfo(lfo(LfoTriangle, 0.3).free());
    p.route(ModTarget::OpLevel(3), slot(shimmer, 0.03));
    p.pan(&[(1, -0.35), (3, 0.35)]);
    p
}
