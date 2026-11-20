//! Effects and percussion.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Bass Drum", 0.181, kick),
    entry("Snare", 0.476, snare),
    entry("Hi-Hat", 0.288, hi_hat),
    entry("Tom", 0.232, tom),
    entry("Cymbal", 0.289, cymbal),
    entry("Laser", 0.233, laser),
    entry("Wind", 1.000, wind),
    entry("Computer", 0.145, computer),
    entry("Drone", 0.555, drone),
    entry("UFO", 0.195, ufo),
    entry("Ring Bell", 0.496, ring_bell),
    entry("Robot", 0.445, robot),
    entry("Ring Lead", 0.333, ring_lead),
    entry("Surf", 0.446, surf),
    entry("Clap", 0.747, clap),
    entry("Noise Sweep", 1.000, noise_sweep),
];

fn kick() -> Patch {
    // A sine whose pitch falls three octaves within ~60 ms (envelope on the
    // operator's pitch) plus a short click from a fixed 250 Hz modulator,
    // the same on every key. Best around C1–C2.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.0005, 0.45, 0.0, 0.2)),
            sine(1.0, 0.0, perc(0.015)).fixed(250.0).dynamic(0.05, 0.2),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let drop = p.env(adsr(0.0005, 0.06, 0.0, 0.05));
    p.route(ModTarget::OpPitch(0), slot(drop, 36.0));
    p
}

fn snare() -> Patch {
    // A tone dropping an octave quickly plus the snares: noise (op 5), the
    // same on every key.
    let mut p = patch(
        [
            sine(1.0, 0.7, adsr(0.0005, 0.15, 0.0, 0.1)),
            sine(1.6, 0.0, perc(0.05)).dynamic(0.05, 0.15),
            off(),
            off(),
            noise(0.7, adsr(0.0005, 0.22, 0.0, 0.15)),
            off(),
        ],
        &[(2, 1)],
        &[1, 5],
    );
    let drop = p.env(adsr(0.0005, 0.04, 0.0, 0.03));
    p.route(ModTarget::OpPitch(0), slot(drop, 12.0));
    p.filter(highpass(180.0, 0.1));
    p
}

fn hi_hat() -> Patch {
    // Metallic tones from fixed, inharmonic frequencies plus noise for the
    // sizzle; a steep high-pass keeps only the top.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.08)).fixed(3500.0),
            sine(1.0, 0.8, perc(0.08)).fixed(5180.0).fb(0.8),
            sine(1.0, 0.7, perc(0.07)).fixed(7720.0),
            sine(1.0, 0.6, perc(0.07)).fixed(9830.0),
            noise(0.5, perc(0.06)),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3, 5],
    );
    p.filter(highpass(6000.0, 0.2).db24());
    // Slightly right, as on a drum kit seen from the audience.
    p.balance = 0.3;
    p
}

fn tom() -> Patch {
    // A sine dropping a fifth, with a soft 1.5:1 strike.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.0005, 0.5, 0.0, 0.25)),
            sine(1.5, 0.0, perc(0.06)).dynamic(0.03, 0.1),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let drop = p.env(adsr(0.0005, 0.15, 0.0, 0.1));
    p.route(ModTarget::OpPitch(0), slot(drop, 7.0));
    p
}

fn cymbal() -> Patch {
    // Six fixed, inharmonic frequencies in three noisy pairs, the last one
    // ring-modulated for more clang; long decay, high-passed.
    let mut p = patch(
        [
            sine(1.0, 0.9, adsr(0.001, 2.5, 0.0, 1.5)).fixed(4300.0),
            sine(1.0, 0.9, adsr(0.001, 2.0, 0.0, 1.5))
                .fixed(6010.0)
                .fb(0.9),
            sine(1.0, 0.8, adsr(0.001, 2.2, 0.0, 1.5)).fixed(8270.0),
            sine(1.0, 0.7, adsr(0.001, 1.8, 0.0, 1.5)).fixed(9470.0),
            sine(1.0, 0.5, adsr(0.001, 1.5, 0.0, 1.2)).fixed(2900.0),
            sine(1.0, 0.6, adsr(0.001, 1.5, 0.0, 1.2))
                .fixed(3870.0)
                .mode(RING),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    p.filter(highpass(3000.0, 0.1).db24());
    p.pan(&[(1, -0.3), (3, 0.3)]);
    p
}

fn laser() -> Patch {
    // Pitch falls three octaves (envelope on the tuning) with FM bite.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.4, 0.0, 0.2)),
            sine(2.0, 0.0, adsr(0.001, 0.4, 0.0, 0.2)).dynamic(0.05, 0.2),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let zap = p.env(adsr(0.001, 0.3, 0.0, 0.1));
    p.route(ModTarget::Tune, slot(zap, 36.0));
    // Flies in from the left as it falls.
    p.route(ModTarget::Balance, slot(zap, -0.8));
    p
}

fn wind() -> Patch {
    // Noise through a resonant band-pass that follows the keys; gusts from
    // a slow free LFO.
    let mut p = patch(
        [
            off(),
            off(),
            off(),
            off(),
            noise(1.0, adsr(1.0, 1.0, 0.9, 2.0)),
            off(),
        ],
        &[],
        &[5],
    );
    let gust = p.lfo(lfo(LfoSine, 0.15).free());
    p.filter(
        bandpass(900.0, 0.4)
            .keytrack(0.5)
            .cutoff_mod(slot(gust, 18.0)),
    );
    p.route(ModTarget::Balance, slot(gust, 0.5));
    p
}

fn computer() -> Patch {
    // Sci-fi computer: a square jumping in pitch on a sample-and-hold LFO,
    // faster the harder you play.
    let mut p = patch(
        [
            square(1.0, 0.8, gate(0.002, 0.1)),
            sine(2.0, 0.0, gate(0.002, 0.1)).dynamic(0.02, 0.08),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let random = p.lfo(lfo(SampleHold, 10.0).rate_mod(velocity(1.5)));
    p.route(ModTarget::OpPitch(0), slot(random, 12.0));
    p.route(ModTarget::Balance, slot(random, 0.6));
    p.filter(lowpass(4000.0, 0.3));
    p.crush(5.0, 6000.0, 0.8);
    p.output(0.581);
    p
}

fn drone() -> Patch {
    // Beating saws over a sub that a fifth (1.5:1) modulates; one very slow
    // LFO moves its feedback and the resonant filter.
    let mut p = patch(
        [
            saw(1.0, 0.6, adsr(2.0, 2.0, 0.9, 4.0)).cents(-4.0),
            saw(1.0, 0.6, adsr(2.0, 2.0, 0.9, 4.0)).cents(4.0),
            sine(0.5, 0.6, adsr(2.0, 2.0, 0.9, 4.0)),
            sine(1.5, 0.06, gate(2.0, 4.0)).fb(0.4),
            off(),
            off(),
        ],
        &[(4, 3)],
        &[1, 2, 3],
    );
    let slow = p.lfo(lfo(LfoTriangle, 0.05).free());
    p.route(ModTarget::OpFeedback(3), slot(slow, 0.4));
    p.filter(lowpass(700.0, 0.5).db24().cutoff_mod(slot(slow, 15.0)));
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p
}

fn ufo() -> Patch {
    // A sine warbling on a triangle LFO whose rate climbs with a slow
    // envelope (envelope on the LFO rate): the saucer speeds up.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.3, 1.0, 0.9, 1.0)),
            sine(1.0, 0.0, gate(0.3, 1.0)).dynamic(0.02, 0.06),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let spin_up = p.env(adsr(3.0, 1.0, 1.0, 1.0));
    let warble = p.lfo(lfo(LfoTriangle, 2.0).rate_mod(slot(spin_up, 3.0)));
    p.route(ModTarget::Tune, slot(warble, 3.0));
    p.route(ModTarget::Balance, slot(warble, 0.5));
    p
}

fn ring_bell() -> Patch {
    // Two ring-modulated pairs with inharmonic ratios (1 × 2.76 and
    // 3 × 1.41): only sum and difference frequencies remain, a clangorous
    // bell; higher notes ring shorter and softer at the top.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 4.0, 0.0, 3.0)),
            sine(2.76, 1.0, adsr(0.001, 3.5, 0.0, 3.0)).mode(RING),
            sine(3.0, 0.5, adsr(0.001, 2.5, 0.0, 2.0)),
            sine(1.41, 0.8, adsr(0.001, 2.0, 0.0, 2.0)).mode(RING),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.3, -2.0);
    p.pan(&[(1, -0.25), (3, 0.3)]);
    p
}

fn robot() -> Patch {
    // A saw ring-modulated by a fixed 50 Hz sine (the classic robot voice:
    // every key gets the same metallic buzz), through a formant-like
    // band-pass.
    let mut p = patch(
        [
            saw(1.0, 1.0, gate(0.01, 0.15)),
            sine(1.0, 1.0, gate(0.01, 0.15)).fixed(50.0).mode(RING),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    p.filter(bandpass(1200.0, 0.35).keytrack(0.3));
    p
}

fn ring_lead() -> Patch {
    // A saw lead ring-modulated by a fifth (1.5:1); a slow LFO moves the
    // ring amount between growl and clang. Legato with a short glide.
    let mut p = patch(
        [
            saw(1.0, 0.9, adsr(0.01, 0.3, 0.8, 0.2)),
            sine(1.5, 0.6, adsr(0.01, 0.3, 0.9, 0.2)).mode(RING),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let sweep = p.lfo(lfo(LfoTriangle, 0.3).free());
    p.route(ModTarget::OpLevel(1), slot(sweep, 0.35));
    p.filter(lowpass(3500.0, 0.25).keytrack(0.5));
    p.mono(LEGATO, 0.05);
    p
}

fn surf() -> Patch {
    // Waves: noise through a low-pass that a very slow LFO opens and closes
    // with the volume; a second LFO lets the surf wander across.
    let mut p = patch(
        [
            noise(1.0, gate(1.5, 3.0)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    let wave = p.lfo(lfo(LfoSine, 0.08).free());
    let wander = p.lfo(lfo(LfoSine, 0.05).free());
    p.filter(lowpass(700.0, 0.15).cutoff_mod(slot(wave, 24.0)));
    p.route(ModTarget::Volume, slot(wave, 0.3));
    p.route(ModTarget::Balance, slot(wander, 0.6));
    p
}

fn clap() -> Patch {
    // Noise through a band-pass, decaying fast; the first 30 ms flutter
    // (a 50 Hz square LFO on the volume, faded out by an envelope), like
    // several hands not quite together.
    let mut p = patch(
        [
            noise(1.0, adsr(0.0005, 0.15, 0.0, 0.1)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    let burst = p.env(adsr(0.0005, 0.03, 0.0, 0.02));
    let flutter = p.lfo(lfo(LfoSquare, 50.0).amount(0.0, slot(burst, 1.0)));
    p.route(ModTarget::Volume, slot(flutter, -0.5));
    p.filter(bandpass(1100.0, 0.1).drive(0.4));
    p
}

fn noise_sweep() -> Patch {
    // Riser: noise through a resonant band-pass whose cutoff climbs four
    // octaves while the key is held (a slow envelope on the cutoff).
    let mut p = patch(
        [
            noise(1.0, adsr(0.2, 0.1, 1.0, 1.5)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    let rise = p.env(adsr(4.0, 0.1, 1.0, 1.5));
    p.filter(
        bandpass(250.0, 0.45)
            .drive(0.5)
            .cutoff_mod(slot(rise, 48.0)),
    );
    p
}
