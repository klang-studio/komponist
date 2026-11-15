//! Brass and woodwinds.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Brass", 0.394, brass),
    entry("Synth Brass", 0.145, synth_brass),
    entry("Trumpet", 0.187, trumpet),
    entry("French Horn", 0.350, french_horn),
    entry("Horn Section", 0.358, brass_section),
    entry("Flute", 0.201, flute),
    entry("Pan Flute", 0.383, pan_flute),
    entry("Clarinet", 0.334, clarinet),
    entry("Oboe", 0.343, oboe),
    entry("Saxophone", 0.129, saxophone),
    entry("Harmonica", 0.748, harmonica),
];

fn brass() -> Patch {
    // Three carriers brightened by one shared 1:1 modulator (op 6) whose
    // level swells with a brass-like envelope: fully on the main tone,
    // less on the octave (not harsh) and the sub (stays round). Velocity
    // adds bite on op 2, the pitch scoops in from below, and a vibrato fades
    // in on the same tuning.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.05, 0.3, 0.85, 0.2)),
            sine(1.0, 0.12, adsr(0.05, 0.3, 0.8, 0.2)),
            sine(1.0, 0.8, adsr(0.06, 0.3, 0.85, 0.2)).cents(4.0),
            sine(2.0, 0.3, adsr(0.06, 0.3, 0.8, 0.2)),
            sine(0.5, 0.25, adsr(0.08, 0.3, 0.85, 0.2)),
            sine(1.0, 0.0, adsr(0.06, 0.4, 0.75, 0.2)).fb(0.25),
        ],
        &[(2, 1), (6, 3), (6, 4), (6, 5)],
        &[1, 3, 4, 5],
    );
    p.depth(6, 4, 0.6);
    p.depth(6, 5, 0.4);
    let swell = p.env(adsr(0.08, 0.4, 0.6, 0.2));
    p.route(ModTarget::OpLevel(5), slot(swell, 0.2));
    p.route(ModTarget::OpLevel(1), velocity(0.08));
    let scoop = p.env(adsr(0.001, 0.08, 0.0, 0.05));
    p.route(ModTarget::Tune, slot(scoop, -0.3));
    p.vibrato(5.0, 0.1, 0.8);
    p.struck(0.0, -3.0);
    p
}

fn synth_brass() -> Patch {
    // Detuned saws through a 24 dB low-pass with a brassy filter envelope
    // that opens further the harder you play; velocity also adds drive.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.03, 0.5, 0.8, 0.3)).cents(-6.0),
            saw(1.0, 0.8, adsr(0.03, 0.5, 0.8, 0.3)).cents(6.0),
            saw(0.5, 0.5, adsr(0.03, 0.5, 0.8, 0.3)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(0.06, 0.6, 0.45, 0.3));
    p.filter(
        lowpass(600.0, 0.2)
            .db24()
            .keytrack(0.4)
            .drive(0.2)
            .drive_mod(velocity(0.3)),
    );
    p.touch_cutoff(env, 36.0);
    p
}

fn trumpet() -> Patch {
    // 1:1 with feedback and a 3:1 edge; an envelope brightens the attack,
    // velocity the edge, the pitch scoops in from below and a vibrato fades
    // in, both on the tuning.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.04, 0.2, 0.85, 0.12)),
            sine(1.0, 0.0, adsr(0.04, 0.2, 0.8, 0.12)).fb(0.3),
            sine(3.0, 0.0, adsr(0.05, 0.2, 0.7, 0.12)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1],
    );
    let bright = p.env(adsr(0.05, 0.25, 0.7, 0.15));
    p.route(ModTarget::OpLevel(1), slot(bright, 0.25));
    p.route(ModTarget::OpLevel(2), velocity(0.06));
    p.vibrato(5.5, 0.12, 0.6);
    let scoop = p.env(adsr(0.001, 0.06, 0.0, 0.05));
    p.route(ModTarget::Tune, slot(scoop, -0.25));
    p.filter(lowpass(5000.0, 0.15).keytrack(0.3));
    p
}

fn french_horn() -> Patch {
    // Mellow: low index, soft attack, dark low-pass, slow vibrato.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.08, 0.3, 0.9, 0.3)),
            sine(1.0, 0.0, adsr(0.1, 0.3, 0.8, 0.3)).dynamic(0.06, 0.15),
            sine(0.5, 0.35, adsr(0.1, 0.3, 0.9, 0.3)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.filter(lowpass(1400.0, 0.1).keytrack(0.4));
    p.vibrato(4.8, 0.06, 1.0);
    p
}

fn brass_section() -> Patch {
    // Three brass voices (1:1 pairs), detuned and an octave apart, with one
    // brightness envelope on all modulators; velocity opens the filter.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.05, 0.3, 0.85, 0.25)).cents(-5.0),
            sine(1.0, 0.0, adsr(0.05, 0.3, 0.8, 0.25)).fb(0.2),
            sine(1.0, 0.9, adsr(0.06, 0.3, 0.85, 0.25)).cents(5.0),
            sine(1.0, 0.0, adsr(0.06, 0.3, 0.8, 0.25)).fb(0.2),
            sine(2.0, 0.5, adsr(0.07, 0.3, 0.85, 0.25)),
            sine(2.0, 0.0, adsr(0.07, 0.3, 0.8, 0.25)),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    let swell = p.env(adsr(0.07, 0.5, 0.55, 0.25));
    for op in [1, 3, 5] {
        p.route(ModTarget::OpLevel(op), slot(swell, 0.22));
    }
    p.filter(
        lowpass(1800.0, 0.1)
            .keytrack(0.4)
            .cutoff_mod(velocity(24.0)),
    );
    // Players spread across the section.
    p.pan(&[(1, -0.4), (3, 0.4)]);
    p
}

fn flute() -> Patch {
    // A sine with a breathy edge: op 4 roughens it with noise, op 3 adds a
    // short chiff; vibrato fades in.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.08, 0.2, 0.9, 0.15)),
            sine(1.0, 0.0, adsr(0.06, 0.3, 0.6, 0.15)).dynamic(0.01, 0.04),
            sine(3.0, 0.0, adsr(0.002, 0.06, 0.0, 0.05)).dynamic(0.01, 0.05),
            noise(0.02, adsr(0.05, 0.2, 0.8, 0.15)),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1), (4, 1)],
        &[1],
    );
    p.vibrato(5.0, 0.1, 0.8);
    p.filter(lowpass(4000.0, 0.0).keytrack(0.5));
    p
}

fn pan_flute() -> Patch {
    // Breath noise (op 4) strongest at the attack, through a band-pass
    // operator (op 3) an octave above the note, so the breath follows the
    // pitch; over a soft sine that stays clear.
    let breath = adsr(0.01, 0.15, 0.08, 0.15);
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.06, 0.3, 0.85, 0.2)),
            sine(2.0, 0.0, adsr(0.06, 0.3, 0.5, 0.2)).dynamic(0.005, 0.02),
            filter_op(BP, 2.0, 0.4, breath),
            noise(0.4, breath),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.vibrato(4.5, 0.08, 1.2);
    p
}

fn clarinet() -> Patch {
    // Odd harmonics from a 2:1 modulator, a hollow triangle, vibrato.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.04, 0.2, 0.9, 0.12)),
            sine(2.0, 0.0, adsr(0.04, 0.2, 0.85, 0.12)).dynamic(0.06, 0.12),
            tri(1.0, 0.25, adsr(0.05, 0.2, 0.9, 0.12)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.vibrato(5.0, 0.06, 1.0);
    p.filter(lowpass(3000.0, 0.0).keytrack(0.5));
    p
}

fn oboe() -> Patch {
    // Nasal: bright FM through a band-pass that follows the keys.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.03, 0.2, 0.9, 0.1)),
            sine(1.0, 0.0, adsr(0.03, 0.2, 0.85, 0.1))
                .dynamic(0.1, 0.2)
                .fb(0.2),
            sine(3.0, 0.0, adsr(0.03, 0.2, 0.8, 0.1)).dynamic(0.02, 0.06),
            off(),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1],
    );
    p.filter(bandpass(1300.0, 0.25).keytrack(0.6));
    p.vibrato(5.5, 0.08, 0.7);
    p
}

fn saxophone() -> Patch {
    // Reedy 1:1 with feedback; an envelope on the 3:1 modulator adds the
    // "blat" of the attack; vibrato and a little drive.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.03, 0.3, 0.85, 0.12)),
            sine(1.0, 0.0, adsr(0.03, 0.3, 0.8, 0.12))
                .dynamic(0.08, 0.2)
                .fb(0.35),
            sine(3.0, 0.0, gate(0.03, 0.12)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1],
    );
    let blat = p.env(adsr(0.02, 0.15, 0.0, 0.1));
    p.route(ModTarget::OpLevel(2), slot(blat, 0.1));
    p.vibrato(5.2, 0.1, 0.5);
    p.filter(lowpass(3200.0, 0.2).drive(0.25).keytrack(0.4));
    p
}

fn harmonica() -> Patch {
    // A square reshaped by op 2 in PWM mode (harder playing, narrower reed),
    // through a band-pass; a hand tremolo fades in.
    let mut p = patch(
        [
            square(1.0, 0.7, adsr(0.03, 0.2, 0.85, 0.1)),
            sine(1.0, 0.0, adsr(0.03, 0.2, 0.8, 0.1))
                .mode(PWM)
                .dynamic(0.15, 0.4),
            sine(2.0, 0.3, adsr(0.03, 0.2, 0.85, 0.1)),
            sine(2.0, 0.0, adsr(0.03, 0.2, 0.8, 0.1)).dynamic(0.04, 0.1),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.filter(bandpass(1600.0, 0.3).keytrack(0.5));
    let hand = p.lfo(lfo(LfoSine, 5.5).fade(0.4));
    p.route(ModTarget::Volume, slot(hand, 0.17));
    p
}
