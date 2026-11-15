//! Chiptune: the pulse, triangle and noise channels of old game consoles
//! and home computers, with their arpeggios, sweeps and jingles. A square
//! with a pulse-width modulator (PWM mode) at level 0.75 is a 12.5 % pulse,
//! at 0.5 a 25 % pulse.

use super::build::*;
use super::{Entry, entry};
use crate::{EnvelopeParams, ModSource, Patch};

pub(super) const LIST: &[Entry] = &[
    entry("NES Pulse 12", 0.171, pulse_12),
    entry("Duty Cycle", 0.171, duty_cycle),
    entry("NES Triangle", 0.505, nes_triangle),
    entry("Chip Arp Maj7", 0.174, chip_arp),
    entry("Chip Snare", 0.321, chip_snare),
    entry("Power-Up", 0.171, power_up),
    entry("Coin", 0.178, coin),
    entry("Gameboy Lead", 0.468, gameboy_lead),
    entry("Chip Echo", 0.180, chip_echo),
    entry("SID Sweep", 0.171, sid_sweep),
];

/// A square narrowed to a pulse of `duty` (0..=0.5) by op 2: ops 1 and 2.
fn pulse(duty: f32, level: f32, envelope: EnvelopeParams) -> [OpSpec; 2] {
    [
        square(1.0, level, envelope),
        sine(1.0, 1.0 - 2.0 * duty, gate(0.001, envelope.release)).mode(PWM),
    ]
}

fn pulse_12() -> Patch {
    // The thin 12.5 % pulse with a stepped volume decay (4 bits); the mod
    // wheel widens it to a square.
    let [a, b] = pulse(0.125, 1.0, adsr(0.001, 0.6, 0.5, 0.05));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    p.route(ModTarget::OpLevel(1), slot(ModSource::ModWheel, -0.75));
    p.crush(4.0, 22_050.0, 0.6);
    p.output(0.576);
    p
}

fn duty_cycle() -> Patch {
    // The pulse width flips between 12.5 % and 50 % sixteen times a
    // second (a square LFO on the width), the classic chip "chorus".
    let [a, b] = pulse(0.3125, 1.0, adsr(0.001, 0.4, 0.7, 0.05));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    let flip = p.lfo(lfo(LfoSquare, 8.0));
    p.route(ModTarget::OpLevel(1), slot(flip, 0.375));
    p.crush(4.0, 22_050.0, 0.6);
    p.output(0.566);
    p
}

fn nes_triangle() -> Patch {
    // The triangle channel, the console's bass: a triangle quantized to
    // 4 bits, no volume control, so the touch does not matter.
    let mut p = patch(
        [
            tri(1.0, 1.0, gate(0.001, 0.03)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    p.crush(4.0, 48_000.0, 1.0);
    p.mono(MONO, 0.0);
    p.output(0.505);
    p
}

fn chip_arp() -> Patch {
    // A held key plays a major-seventh arpeggio (root, third, fifth,
    // seventh) at 15 notes a second, the way sound chips faked chords: two
    // square LFOs on the tuning, one twice as fast as the other, add up to
    // the four steps (±3.5 and ±2 semitones around +5.5). Both start low
    // (phase 0.5), so the arpeggio starts on the root.
    let [a, b] = pulse(0.25, 1.0, adsr(0.001, 0.3, 0.8, 0.05));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    p.tune = 5.5;
    let slow = p.lfo(lfo(LfoSquare, 3.75).phase(0.5));
    let fast = p.lfo(lfo(LfoSquare, 7.5).phase(0.5));
    p.route(ModTarget::Tune, slot(slow, 3.5));
    p.route(ModTarget::Tune, slot(fast, 2.0));
    p.crush(4.0, 22_050.0, 0.6);
    p.output(0.554);
    p
}

fn chip_snare() -> Patch {
    // The noise channel at a low sample rate plus a pulse dropping fast,
    // the same on every key.
    let mut p = patch(
        [
            square(1.0, 0.6, perc(0.08)).fixed(220.0),
            off(),
            off(),
            off(),
            noise(0.9, perc(0.18)),
            off(),
        ],
        &[],
        &[1, 5],
    );
    let drop = p.env(perc(0.05));
    p.route(ModTarget::OpPitch(0), slot(drop, 12.0));
    p.crush(4.0, 9_000.0, 1.0);
    p
}

fn power_up() -> Patch {
    // Rising sweeps, four a second (a rising saw LFO on the tuning, an
    // octave), climbing another octave within a second while the key is
    // held.
    let [a, b] = pulse(0.25, 1.0, adsr(0.001, 0.2, 0.9, 0.1));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    p.tune = 6.0;
    let sweep = p.lfo(lfo(LfoSawUp, 4.0));
    p.route(ModTarget::Tune, slot(sweep, 6.0));
    let climb = p.env(adsr(1.0, 0.1, 1.0, 0.1));
    p.route(ModTarget::Tune, slot(climb, 12.0));
    p.crush(4.0, 22_050.0, 0.6);
    p.output(0.550);
    p
}

fn coin() -> Patch {
    // The coin jingle: the note, then a fourth above after 70 ms (a delayed
    // envelope on the tuning), ringing out. Best around B5.
    let [a, b] = pulse(0.25, 1.0, adsr(0.001, 0.6, 0.0, 0.3));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    let step = p.env(EnvelopeParams {
        delay: 0.07,
        ..adsr(0.001, 0.1, 1.0, 0.3)
    });
    p.route(ModTarget::Tune, slot(step, 5.0));
    p.crush(4.0, 22_050.0, 0.6);
    p.output(0.645);
    p
}

fn gameboy_lead() -> Patch {
    // A 25 % pulse lead with a vibrato that waits a third of a second
    // (LFO delay); short glides between notes.
    let [a, b] = pulse(0.25, 1.0, adsr(0.001, 0.2, 0.8, 0.05));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    let vibrato = p.lfo(lfo(LfoTriangle, 6.0).delay(0.35).fade(0.1));
    p.route(ModTarget::Tune, slot(vibrato, 0.3));
    p.mono(LEGATO, 0.03);
    p.crush(4.0, 16_000.0, 0.6);
    p.output(0.526);
    p
}

fn chip_echo() -> Patch {
    // Short 12.5 % blips echoed in dotted sixteenths, like the fake delay
    // a second pulse channel played in old game music.
    let [a, b] = pulse(0.125, 1.0, perc(0.15));
    let mut p = patch([a, b, off(), off(), off(), off()], &[(2, 1)], &[1]);
    p.crush(4.0, 22_050.0, 0.6);
    p.echo(0.1875, 0.5, 0.4, true);
    p
}

fn sid_sweep() -> Patch {
    // Home-computer style: a pulse whose width a slow LFO sweeps, through
    // a resonant low-pass that a second LFO sweeps; notes glide.
    let [a, b] = pulse(0.25, 1.0, adsr(0.005, 0.3, 0.9, 0.1));
    let mut p = patch(
        [
            a,
            b,
            saw(1.0, 0.3, adsr(0.005, 0.3, 0.9, 0.1)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let width = p.lfo(lfo(LfoTriangle, 0.7));
    p.route(ModTarget::OpLevel(1), slot(width, 0.4));
    let sweep = p.lfo(lfo(LfoTriangle, 0.25).phase(0.75));
    p.filter(
        lowpass(1000.0, 0.65)
            .keytrack(0.5)
            .cutoff_mod(slot(sweep, 24.0)),
    );
    p.mono(LEGATO, 0.08);
    p.crush(8.0, 24_000.0, 0.3);
    p
}
