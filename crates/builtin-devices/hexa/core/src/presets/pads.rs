//! Strings and pads.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Strings", 1.000, strings),
    entry("Slow Strings", 1.000, slow_strings),
    entry("Cello", 0.282, cello),
    entry("Warm Pad", 0.711, warm_pad),
    entry("Glass Pad", 0.493, glass_pad),
    entry("Sweep Pad", 0.462, sweep_pad),
    entry("Choir", 0.842, choir),
    entry("Dark Pad", 0.548, dark_pad),
    entry("PWM Pad", 0.444, pwm_pad),
    entry("Shimmer Pad", 0.522, shimmer_pad),
    entry("Morph Pad", 0.289, evolving_pad),
    entry("Analog Pad", 0.647, analog_pad),
    entry("Fog", 0.477, mist),
];

fn strings() -> Patch {
    // Ensemble: four detuned saws and one an octave down through a 24 dB
    // low-pass; a slow free LFO on the tuning for movement.
    let mut p = patch(
        [
            saw(1.0, 0.6, adsr(0.25, 0.5, 0.9, 0.8)).cents(-11.0),
            saw(1.0, 0.6, adsr(0.25, 0.5, 0.9, 0.8)).cents(-3.0),
            saw(1.0, 0.6, adsr(0.25, 0.5, 0.9, 0.8)).cents(4.0),
            saw(1.0, 0.6, adsr(0.25, 0.5, 0.9, 0.8)).cents(12.0),
            saw(0.5, 0.35, adsr(0.3, 0.5, 0.9, 0.8)),
            off(),
        ],
        &[],
        &[1, 2, 3, 4, 5],
    );
    let ensemble = p.lfo(lfo(LfoTriangle, 0.7).free());
    p.route(ModTarget::Tune, slot(ensemble, 0.06));
    p.filter(
        lowpass(3500.0, 0.1)
            .db24()
            .keytrack(0.3)
            .cutoff_mod(velocity(12.0)),
    );
    // Desks spread across the stage.
    p.pan(&[(1, -0.6), (2, -0.2), (3, 0.2), (4, 0.6)]);
    p.unison(2, 12.0, 1.0);
    p
}

fn slow_strings() -> Patch {
    // Slow bowing, darker, octave layer, a slower ensemble.
    let mut p = patch(
        [
            saw(1.0, 0.6, adsr(0.9, 0.8, 0.9, 1.6)).cents(-8.0),
            saw(1.0, 0.6, adsr(0.9, 0.8, 0.9, 1.6)).cents(8.0),
            saw(2.0, 0.3, adsr(1.1, 0.8, 0.9, 1.6)).cents(-4.0),
            saw(2.0, 0.3, adsr(1.1, 0.8, 0.9, 1.6)).cents(4.0),
            tri(0.5, 0.4, adsr(1.0, 0.8, 0.9, 1.6)),
            off(),
        ],
        &[],
        &[1, 2, 3, 4, 5],
    );
    let ensemble = p.lfo(lfo(LfoSine, 0.4).free());
    p.route(ModTarget::Tune, slot(ensemble, 0.05));
    p.filter(lowpass(2400.0, 0.1).db24().keytrack(0.3));
    p.pan(&[(1, -0.5), (2, 0.5), (3, -0.3), (4, 0.3)]);
    p.unison(2, 10.0, 1.0);
    p
}

fn cello() -> Patch {
    // Solo string: saws with a little 1:1 rasp, a resonant body filter that
    // opens with velocity, and vibrato.
    let mut p = patch(
        [
            saw(1.0, 0.9, adsr(0.12, 0.3, 0.9, 0.4)),
            saw(1.0, 0.5, adsr(0.12, 0.3, 0.9, 0.4)).cents(5.0),
            sine(1.0, 0.0, adsr(0.08, 0.2, 0.6, 0.3)).dynamic(0.02, 0.08),
            off(),
            off(),
            off(),
        ],
        &[(3, 1)],
        &[1, 2],
    );
    p.vibrato(5.0, 0.12, 0.7);
    p.filter(
        lowpass(1800.0, 0.25)
            .db24()
            .keytrack(0.5)
            .cutoff_mod(velocity(12.0)),
    );
    p
}

fn warm_pad() -> Patch {
    // Detuned triangles with a sub; the FM shimmer grows in with a slow
    // modulation envelope.
    let mut p = patch(
        [
            tri(1.0, 0.8, adsr(0.6, 1.0, 0.85, 1.5)).cents(-6.0),
            sine(1.0, 0.0, adsr(1.2, 1.0, 0.8, 1.5)),
            tri(1.0, 0.8, adsr(0.6, 1.0, 0.85, 1.5)).cents(6.0),
            sine(2.0, 0.0, adsr(1.5, 1.0, 0.7, 1.5)),
            sine(0.5, 0.5, adsr(0.8, 1.0, 0.85, 1.5)),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3, 5],
    );
    let swell = p.env(adsr(2.0, 2.0, 0.6, 1.5));
    p.route(ModTarget::OpLevel(1), slot(swell, 0.08));
    p.route(ModTarget::OpLevel(3), slot(swell, 0.05));
    p.filter(lowpass(2200.0, 0.0).keytrack(0.3));
    p.pan(&[(1, -0.4), (3, 0.4)]);
    p.unison(2, 8.0, 1.0);
    p
}

fn glass_pad() -> Patch {
    // Inharmonic modulators (3.5, 7, 1.41) with slow envelopes; two free
    // LFOs at unrelated rates keep the glassy partials drifting.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.8, 2.0, 0.7, 2.0)),
            sine(3.5, 0.04, adsr(1.0, 2.0, 0.6, 2.0)),
            sine(2.0, 0.6, adsr(1.0, 2.0, 0.7, 2.0)).cents(7.0),
            sine(7.0, 0.02, adsr(1.5, 2.0, 0.5, 2.0)),
            sine(4.0, 0.25, adsr(1.2, 2.0, 0.6, 2.0)).cents(-5.0),
            sine(1.41, 0.02, adsr(1.5, 2.0, 0.6, 2.0)),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    let drift = p.lfo(lfo(LfoTriangle, 0.15).free());
    p.route(ModTarget::OpLevel(1), slot(drift, 0.03));
    let drift2 = p.lfo(lfo(LfoSine, 0.23).free());
    p.route(ModTarget::OpLevel(3), slot(drift2, 0.02));
    p.pan(&[(1, -0.4), (3, 0.4)]);
    p
}

fn sweep_pad() -> Patch {
    // Saws through a resonant 24 dB low-pass, swept by a very slow free
    // triangle LFO.
    let mut p = patch(
        [
            saw(1.0, 0.7, adsr(0.5, 1.0, 0.9, 1.5)).cents(-8.0),
            saw(1.0, 0.7, adsr(0.5, 1.0, 0.9, 1.5)).cents(8.0),
            saw(0.5, 0.5, adsr(0.5, 1.0, 0.9, 1.5)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let sweep = p.lfo(lfo(LfoTriangle, 0.12).free());
    p.filter(lowpass(900.0, 0.45).db24().cutoff_mod(slot(sweep, 30.0)));
    p.pan(&[(1, -0.5), (2, 0.5)]);
    p
}

fn choir() -> Patch {
    // Vowel "ah": two soft, detuned 1:1 voices and a 3:1 layer through a
    // band-pass around the first formant; slow attack, gentle vibrato.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.35, 0.5, 0.9, 0.8)).cents(-6.0),
            sine(1.0, 0.0, adsr(0.35, 0.5, 0.9, 0.8)).dynamic(0.08, 0.14),
            sine(1.0, 1.0, adsr(0.4, 0.5, 0.9, 0.8)).cents(6.0),
            sine(1.0, 0.0, adsr(0.4, 0.5, 0.9, 0.8)).dynamic(0.08, 0.14),
            sine(3.0, 0.15, adsr(0.4, 0.5, 0.9, 0.8)),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3, 5],
    );
    p.vibrato(4.5, 0.08, 1.0);
    p.filter(bandpass(850.0, 0.3));
    // Two singers apart.
    p.pan(&[(1, -0.4), (3, 0.4)]);
    p
}

fn dark_pad() -> Patch {
    // Low and slow: saws and a sub through a dark, breathing low-pass.
    let mut p = patch(
        [
            saw(1.0, 0.6, adsr(1.5, 2.0, 0.9, 3.0)).cents(-5.0),
            saw(1.0, 0.6, adsr(1.5, 2.0, 0.9, 3.0)).cents(5.0),
            tri(0.5, 0.7, adsr(1.5, 2.0, 0.9, 3.0)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let breath = p.lfo(lfo(LfoSine, 0.08).free());
    p.filter(lowpass(450.0, 0.35).db24().cutoff_mod(slot(breath, 12.0)));
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p
}

fn pwm_pad() -> Patch {
    // Two PWM pairs (ops 2 and 4 in PWM mode on detuned squares); two free
    // LFOs at different rates sweep their pulse widths.
    let mut p = patch(
        [
            square(1.0, 0.7, adsr(0.4, 1.0, 0.9, 1.2)),
            sine(1.0, 0.3, gate(0.4, 1.2)).mode(PWM),
            square(1.0, 0.7, adsr(0.4, 1.0, 0.9, 1.2)).cents(7.0),
            sine(1.0, 0.3, gate(0.4, 1.2)).mode(PWM).cents(7.0),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let a = p.lfo(lfo(LfoTriangle, 0.35).free());
    let b = p.lfo(lfo(LfoTriangle, 0.47).free());
    p.route(ModTarget::OpLevel(1), slot(a, 0.25));
    p.route(ModTarget::OpLevel(3), slot(b, 0.25));
    p.filter(lowpass(2800.0, 0.15).keytrack(0.3));
    // Classic: the two PWM layers hard apart.
    p.pan(&[(1, -0.6), (3, 0.6)]);
    p.unison(2, 8.0, 1.0);
    p
}

fn shimmer_pad() -> Patch {
    // High, airy partials: a 3.01:1 pair, an octave pair whose triangle
    // modulator (FM mode) sparkles on a sample-and-hold LFO; high-passed.
    let mut p = patch(
        [
            sine(1.0, 0.9, adsr(1.0, 2.0, 0.8, 2.5)),
            sine(3.01, 0.03, adsr(1.0, 2.0, 0.8, 2.5)),
            sine(2.0, 0.6, adsr(1.2, 2.0, 0.8, 2.5)).cents(4.0),
            tri(7.0, 0.0, adsr(1.5, 2.0, 0.8, 2.5)).mode(FM),
            sine(4.0, 0.3, adsr(1.5, 2.0, 0.7, 2.5)).cents(-4.0),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3, 5],
    );
    let sparkle = p.lfo(lfo(SampleHold, 6.0).free());
    p.route(ModTarget::OpLevel(3), slot(sparkle, 0.04));
    p.filter(highpass(250.0, 0.0));
    p.pan(&[(1, -0.3), (3, 0.4), (5, -0.4)]);
    p
}

fn evolving_pad() -> Patch {
    // Slowly evolving: an envelope with a 3 s attack opens the FM, a slow
    // LFO moves the feedback, another the second modulator and the filter.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(1.0, 2.0, 0.9, 3.0)),
            sine(2.0, 0.0, gate(1.0, 3.0)).fb(0.3),
            saw(1.0, 0.4, adsr(1.5, 2.0, 0.9, 3.0)).cents(6.0),
            sine(1.5, 0.0, gate(1.5, 3.0)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let grow = p.env(adsr(3.0, 2.0, 0.7, 3.0));
    p.route(ModTarget::OpLevel(1), slot(grow, 0.2));
    let wobble = p.lfo(lfo(LfoTriangle, 0.2).free());
    p.route(ModTarget::OpFeedback(1), slot(wobble, 0.3));
    let drift = p.lfo(lfo(LfoSine, 0.07).free());
    p.route(ModTarget::OpLevel(3), slot(drift, 0.06));
    p.route(ModTarget::Balance, slot(drift, 0.4));
    p.filter(lowpass(1600.0, 0.3).cutoff_mod(slot(drift, 18.0)));
    p
}

fn analog_pad() -> Patch {
    // Two saws ±9 cents and a square sub; a slow filter envelope and a free
    // LFO on the tuning.
    let mut p = patch(
        [
            saw(1.0, 0.7, adsr(0.7, 1.0, 0.85, 1.2)).cents(-9.0),
            saw(1.0, 0.7, adsr(0.7, 1.0, 0.85, 1.2)).cents(9.0),
            square(0.5, 0.35, adsr(0.7, 1.0, 0.85, 1.2)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(1.0, 1.5, 0.5, 1.2));
    p.filter(
        lowpass(800.0, 0.2)
            .keytrack(0.3)
            .cutoff_mod(slot(env, 18.0)),
    );
    let ensemble = p.lfo(lfo(LfoSine, 0.5).free());
    p.route(ModTarget::Tune, slot(ensemble, 0.05));
    p.pan(&[(1, -0.5), (2, 0.5)]);
    p.unison(3, 15.0, 1.0);
    p
}

fn mist() -> Patch {
    // Airy noise (op 6 → 5 with full feedback) under a soft pad; a slow LFO
    // sweeps a resonant low-pass operator (op 4, fixed 1.5 kHz) through the
    // noise alone, so the pad stays still.
    let mut p = patch(
        [
            sine(1.0, 0.6, adsr(1.5, 2.0, 0.9, 3.0)),
            sine(2.0, 0.03, gate(1.5, 3.0)),
            tri(0.5, 0.5, adsr(1.5, 2.0, 0.9, 3.0)).cents(5.0),
            filter_op(LP, 1.0, 0.5, gate(0.01, 3.0)).fixed(1500.0),
            sine(1.0, 0.5, adsr(2.0, 2.0, 0.8, 3.0)),
            sine(6.73, 1.0, gate(2.0, 3.0)).fb(1.0),
        ],
        &[(2, 1), (6, 5), (5, 4)],
        &[1, 3, 4],
    );
    let sweep = p.lfo(lfo(LfoSine, 0.1).free());
    p.route(ModTarget::OpPitch(3), slot(sweep, 18.0));
    // The noise drifts across with the sweep.
    p.route(ModTarget::OpPan(3), slot(sweep, 0.7));
    p
}
