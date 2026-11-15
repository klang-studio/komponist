//! Leads.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Saw-Lead", 0.201, saw_lead),
    entry("Chip-Lead", 0.442, chip_lead),
    entry("Sync-Lead", 0.548, sync_lead),
    entry("PWM-Lead", 0.534, pwm_lead),
    entry("FM-Lead", 0.782, fm_lead),
    entry("Resonanz-Lead", 0.247, resonant_lead),
    entry("Pfeif-Lead", 0.500, whistle),
    entry("Schrei-Lead", 0.135, screamer),
    entry("Analog-Lead", 0.182, analog_lead),
    entry("Theremin", 0.501, theremin),
];

fn saw_lead() -> Patch {
    // Two saws ±6 cents and a square sub, a short filter envelope that opens
    // further the harder you play, a little drive, delayed vibrato.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.005, 0.3, 0.85, 0.15)).cents(-6.0),
            saw(1.0, 0.8, adsr(0.005, 0.3, 0.85, 0.15)).cents(6.0),
            square(0.5, 0.3, adsr(0.005, 0.3, 0.85, 0.15)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(0.005, 0.3, 0.4, 0.2));
    p.filter(lowpass(1800.0, 0.25).db24().drive(0.25).keytrack(0.5));
    p.touch_cutoff(env, 18.0);
    p.vibrato(5.5, 0.15, 0.8);
    p.pan(&[(1, -0.2), (2, 0.2)]);
    p.mono(LEGATO, 0.06);
    p.unison(3, 15.0, 0.6);
    p
}

fn chip_lead() -> Patch {
    // 8-bit: a square narrowed to a 12.5 % pulse by op 2 (PWM mode), and a
    // fast vibrato after a short delay.
    let mut p = patch(
        [
            square(1.0, 1.0, adsr(0.001, 0.1, 0.9, 0.05)),
            sine(1.0, 0.75, gate(0.001, 0.05)).mode(PWM),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    p.vibrato(7.0, 0.2, 0.3);
    p.mono(MONO, 0.0);
    // 4 bits, like an old sound chip.
    p.crush(4.0, 16_000.0, 0.7);
    p.output(0.522);
    p
}

fn sync_lead() -> Patch {
    // HTE sync swept by an envelope (op 2's level) over a detuned saw;
    // velocity opens the filter, vibrato fades in.
    let mut p = patch(
        [
            saw(1.0, 1.0, adsr(0.003, 0.5, 0.85, 0.2)),
            sine(1.0, 0.15, gate(0.003, 0.2)).mode(SYNC),
            saw(1.0, 0.4, adsr(0.003, 0.5, 0.85, 0.2)).cents(8.0),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let sweep = p.env(adsr(0.003, 0.8, 0.2, 0.3));
    p.route(ModTarget::OpLevel(1), slot(sweep, 0.5));
    p.filter(
        lowpass(2500.0, 0.15)
            .keytrack(0.4)
            .cutoff_mod(velocity(18.0)),
    );
    p.vibrato(5.5, 0.12, 0.6);
    p.mono(LEGATO, 0.05);
    // A wavefolder adds more bright overtones to the sync sweep.
    p.distort(FOLD, 10.0, 0.3, 0.4);
    p
}

fn pwm_lead() -> Patch {
    // A square whose pulse width (op 2, PWM mode) a key-synced LFO sweeps.
    let mut p = patch(
        [
            square(1.0, 0.9, adsr(0.005, 0.2, 0.9, 0.15)),
            sine(1.0, 0.35, gate(0.005, 0.15)).mode(PWM),
            square(0.5, 0.3, adsr(0.005, 0.2, 0.9, 0.15)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let pwm = p.lfo(lfo(LfoTriangle, 3.0));
    p.route(ModTarget::OpLevel(1), slot(pwm, 0.3));
    p.filter(lowpass(3000.0, 0.2).keytrack(0.4));
    p.mono(LEGATO, 0.04);
    p
}

fn fm_lead() -> Patch {
    // Linear FM (op 2 in FM mode) from a triangle: edgier than phase
    // modulation; velocity sets the index. A detuned second pair.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.005, 0.3, 0.85, 0.15)),
            tri(2.0, 0.0, adsr(0.005, 0.4, 0.7, 0.15))
                .mode(FM)
                .dynamic(0.06, 0.16),
            sine(1.0, 0.3, adsr(0.005, 0.3, 0.85, 0.15)).cents(7.0),
            sine(1.0, 0.0, gate(0.005, 0.15))
                .fb(0.3)
                .dynamic(0.03, 0.08),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.vibrato(5.8, 0.12, 0.5);
    p.mono(LEGATO, 0.03);
    p
}

fn resonant_lead() -> Patch {
    // Saws through a very resonant low-pass that follows the keys; the
    // envelope sweeps through the resonance.
    let mut p = patch(
        [
            saw(1.0, 1.0, adsr(0.003, 0.3, 0.85, 0.15)),
            saw(1.0, 0.5, adsr(0.003, 0.3, 0.85, 0.15)).cents(4.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let env = p.env(adsr(0.003, 0.3, 0.5, 0.2));
    p.filter(
        lowpass(1200.0, 0.85)
            .keytrack(0.7)
            .cutoff_mod(slot(env, 24.0)),
    );
    p.vibrato(5.0, 0.1, 0.8);
    p.mono(LEGATO, 0.08);
    p
}

fn whistle() -> Patch {
    // An octave up (semitone mode): a nearly pure sine with breath noise, a
    // slide into the note from below, and vibrato.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.05, 0.2, 0.9, 0.15)).semitones(12.0),
            sine(11.46, 0.01, gate(0.05, 0.15)).fb(1.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let slide = p.env(adsr(0.001, 0.08, 0.0, 0.05));
    p.route(ModTarget::OpPitch(0), slot(slide, -1.0));
    p.vibrato(5.5, 0.15, 0.5);
    p.mono(LEGATO, 0.12);
    p
}

fn screamer() -> Patch {
    // Saw and square into heavy drive and a resonant 24 dB low-pass; op 3
    // syncs the saw harder the harder you play.
    let mut p = patch(
        [
            saw(1.0, 0.8, gate(0.003, 0.15)),
            square(1.0, 0.6, gate(0.003, 0.15)).cents(-5.0),
            sine(1.0, 0.0, gate(0.003, 0.15))
                .mode(SYNC)
                .dynamic(0.05, 0.35),
            off(),
            off(),
            off(),
        ],
        &[(3, 1)],
        &[1, 2],
    );
    p.filter(lowpass(2800.0, 0.5).db24().drive(0.9).keytrack(0.4));
    p.vibrato(6.0, 0.15, 0.4);
    p.mono(LEGATO, 0.05);
    p.unison(2, 12.0, 0.5);
    p.distort(HARD, 12.0, 0.5, 0.5);
    p
}

fn analog_lead() -> Patch {
    // Saw, square and a saw an octave down through a driven 24 dB low-pass
    // with a snappy envelope; velocity drives harder.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.002, 0.3, 0.8, 0.12)),
            square(1.0, 0.6, adsr(0.002, 0.3, 0.8, 0.12)).cents(-4.0),
            saw(0.5, 0.5, adsr(0.002, 0.3, 0.8, 0.12)).cents(3.0),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(0.002, 0.25, 0.35, 0.15));
    p.filter(
        lowpass(900.0, 0.35)
            .db24()
            .drive(0.4)
            .keytrack(0.6)
            .cutoff_mod(slot(env, 30.0))
            .drive_mod(velocity(0.3)),
    );
    p.vibrato(5.2, 0.12, 0.7);
    p.mono(LEGATO, 0.07);
    p.unison(2, 10.0, 0.4);
    // A little saturation, like an overdriven mixer channel.
    p.distort(SOFT, 8.0, 0.2, 0.4);
    p
}

fn theremin() -> Patch {
    // A pure sine with a wide vibrato whose depth swells with a modulation
    // envelope (the LFO's amount follows the envelope).
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.15, 0.2, 0.95, 0.3)),
            sine(1.0, 0.02, gate(0.15, 0.3)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let swell = p.env(adsr(1.2, 0.5, 0.8, 0.3));
    let vibrato = p.lfo(lfo(LfoSine, 6.2).amount(0.3, slot(swell, 0.7)));
    p.route(ModTarget::Tune, slot(vibrato, 0.35));
    p.mono(LEGATO, 0.25);
    p
}
