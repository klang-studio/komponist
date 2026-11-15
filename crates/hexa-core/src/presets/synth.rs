//! Synth sounds: plucks, stabs, polysynths.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Synth-Pluck", 0.514, synth_pluck),
    entry("Glocken-Pluck", 0.398, bell_pluck),
    entry("Stab", 0.181, stab),
    entry("Poly-Synth", 0.565, poly_synth),
    entry("Clav-Wah", 0.418, clav_wah),
    entry("Arp-Bleep", 0.187, arp_bleep),
    entry("Zufallsfilter", 0.205, random_filter),
    entry("Hoover", 0.912, hoover),
];

fn synth_pluck() -> Patch {
    // Detuned saws through a 24 dB low-pass whose envelope snaps shut,
    // opening further the harder you play; velocity also adds resonance.
    let mut p = patch(
        [
            saw(1.0, 0.8, perc(0.6)).cents(-5.0),
            saw(1.0, 0.8, perc(0.6)).cents(5.0),
            square(0.5, 0.3, perc(0.5)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(perc(0.18));
    p.filter(
        lowpass(300.0, 0.3)
            .db24()
            .keytrack(0.5)
            .resonance_mod(velocity(0.2)),
    );
    p.touch_cutoff(env, 48.0);
    p.pan(&[(1, -0.3), (2, 0.3)]);
    p
}

fn bell_pluck() -> Patch {
    // Short FM bell: a 3.5:1 triangle in FM mode and an octave pair.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(1.0)),
            tri(3.5, 0.0, perc(0.3)).mode(FM).dynamic(0.08, 0.2),
            sine(2.0, 0.5, perc(0.6)),
            sine(1.0, 0.0, perc(0.2)).dynamic(0.05, 0.15),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.3, -3.0);
    p
}

fn stab() -> Patch {
    // Wide detuned saws and an octave, short and punchy through a driven
    // low-pass with a fast envelope that opens further the harder you play.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.002, 0.35, 0.0, 0.2)).cents(-10.0),
            saw(1.0, 0.8, adsr(0.002, 0.35, 0.0, 0.2)).cents(10.0),
            saw(2.0, 0.4, adsr(0.002, 0.3, 0.0, 0.2)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(0.001, 0.2, 0.0, 0.1));
    p.filter(lowpass(800.0, 0.25).db24().drive(0.35));
    p.touch_cutoff(env, 30.0);
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p.unison(2, 10.0, 0.8);
    p
}

fn poly_synth() -> Patch {
    // Saw and square detuned, a low-pass with a gentle envelope, and a
    // slow ensemble on the tuning.
    let mut p = patch(
        [
            saw(1.0, 0.7, adsr(0.01, 0.8, 0.7, 0.4)).cents(-7.0),
            square(1.0, 0.5, adsr(0.01, 0.8, 0.7, 0.4)).cents(7.0),
            saw(0.5, 0.4, adsr(0.01, 0.8, 0.7, 0.4)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(0.01, 0.8, 0.5, 0.4));
    p.filter(
        lowpass(1400.0, 0.2)
            .keytrack(0.4)
            .cutoff_mod(slot(env, 14.0)),
    );
    let ensemble = p.lfo(lfo(LfoTriangle, 0.6).free());
    p.route(ModTarget::Tune, slot(ensemble, 0.05));
    p.pan(&[(1, -0.3), (2, 0.3)]);
    p.unison(2, 10.0, 0.8);
    p
}

fn clav_wah() -> Patch {
    // Clavinet through an envelope-swept band-pass (auto-wah); velocity
    // drives it harder.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.9, 0.3, 0.06)),
            sine(1.0, 0.2, adsr(0.001, 0.3, 0.15, 0.06)).fb(0.45),
            square(2.0, 0.35, adsr(0.001, 0.6, 0.1, 0.06)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let wah = p.env(adsr(0.002, 0.3, 0.2, 0.1));
    p.filter(
        bandpass(500.0, 0.55)
            .cutoff_mod(slot(wah, 30.0))
            .drive(0.3)
            .drive_mod(velocity(0.3)),
    );
    p
}

fn arp_bleep() -> Patch {
    // Short square bleep, brightened by a synced saw (op 2 in sync mode)
    // and thinned by a resonant high-pass.
    let mut p = patch(
        [
            square(1.0, 0.9, perc(0.18)),
            sine(1.0, 0.0, perc(0.1)).mode(SYNC).dynamic(0.1, 0.4),
            square(2.0, 0.3, perc(0.12)).cents(5.0),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.filter(highpass(300.0, 0.4).keytrack(0.5));
    p.crush(6.0, 11_025.0, 0.6);
    p
}

fn random_filter() -> Patch {
    // Saws through a resonant low-pass jumping on a sample-and-hold LFO; the
    // harder you play, the faster it jumps (velocity on the LFO rate).
    let mut p = patch(
        [
            saw(1.0, 0.8, gate(0.005, 0.3)).cents(-4.0),
            saw(1.0, 0.8, gate(0.005, 0.3)).cents(4.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let random = p.lfo(lfo(SampleHold, 6.0).rate_mod(velocity(1.0)));
    p.filter(lowpass(1200.0, 0.6).db24().cutoff_mod(slot(random, 24.0)));
    // Each step jumps to a new place too.
    p.route(ModTarget::Balance, slot(random, 0.5));
    p
}

fn hoover() -> Patch {
    // Rave hoover: wide detuned saws reshaped by one PWM modulator (op 2),
    // half as much on the sub saw so the low end stays solid; the pitch
    // dives in from a fifth above, a free LFO moves the width.
    let mut p = patch(
        [
            saw(1.0, 0.7, adsr(0.01, 0.5, 0.85, 0.4)).cents(-15.0),
            sine(1.0, 0.25, gate(0.01, 0.4)).mode(PWM),
            saw(1.0, 0.7, adsr(0.01, 0.5, 0.85, 0.4)).cents(15.0),
            saw(0.5, 0.5, adsr(0.01, 0.5, 0.85, 0.4)),
            off(),
            off(),
        ],
        &[(2, 1), (2, 3), (2, 4)],
        &[1, 3, 4],
    );
    p.depth(2, 4, 0.5);
    let dive = p.env(adsr(0.001, 0.35, 0.0, 0.1));
    p.route(ModTarget::Tune, slot(dive, 7.0));
    let width = p.lfo(lfo(LfoSine, 0.8).free());
    p.route(ModTarget::OpLevel(1), slot(width, 0.2));
    p.filter(lowpass(3500.0, 0.2));
    p.pan(&[(1, -0.5), (3, 0.5)]);
    p.mono(LEGATO, 0.12);
    p.unison(3, 25.0, 1.0);
    p
}
