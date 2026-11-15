//! Keys: electric and acoustic pianos, clavinet, harpsichord, organs.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("E-Piano", 0.385, e_piano),
    entry("E-Piano weich", 0.463, e_piano_soft),
    entry("Wurli", 0.136, wurli),
    entry("E-Piano Glocke", 0.507, e_piano_bell),
    entry("FM-Klavier", 0.343, fm_piano),
    entry("Clavinet", 0.217, clavinet),
    entry("Cembalo", 0.428, harpsichord),
    entry("Celesta", 0.348, celesta),
    entry("Spielzeugklavier", 0.388, toy_piano),
    entry("Orgel Jazz", 0.654, jazz_organ),
    entry("Orgel Rock", 0.193, rock_organ),
    entry("Kirchenorgel", 0.475, pipe_organ),
    entry("Akkordeon", 0.509, accordion),
    entry("Harmonium", 0.317, harmonium),
];

fn e_piano() -> Patch {
    // Three pairs: body (1:1), tine (14:1), warmth (1:1 with feedback).
    // Velocity brightens body and tine; a free LFO pans it like a stereo
    // "Suitcase" tremolo.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.002, 2.8, 0.1, 0.5)),
            sine(1.0, 0.0, adsr(0.002, 1.5, 0.05, 0.5)).dynamic(0.04, 0.14),
            sine(1.0, 0.5, adsr(0.002, 0.7, 0.0, 0.3)),
            sine(14.0, 0.0, adsr(0.001, 0.2, 0.0, 0.1)).dynamic(0.02, 0.1),
            sine(1.0, 0.35, adsr(0.002, 3.5, 0.1, 0.5)).cents(3.0),
            sine(1.0, 0.06, adsr(0.002, 2.0, 0.0, 0.5)).fb(0.2),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    let tremolo = p.lfo(lfo(LfoSine, 4.5).free());
    p.route(ModTarget::Balance, slot(tremolo, 0.6));
    p.struck(0.35, -3.0);
    p
}

fn e_piano_soft() -> Patch {
    // Like "E-Piano" with less tine, a low-pass and a slow chorus on the
    // third carrier.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.003, 3.5, 0.15, 0.6)),
            sine(1.0, 0.0, adsr(0.003, 2.0, 0.05, 0.6)).dynamic(0.03, 0.09),
            sine(1.0, 0.3, adsr(0.002, 0.9, 0.0, 0.4)),
            sine(14.0, 0.0, adsr(0.001, 0.15, 0.0, 0.1)).dynamic(0.0, 0.04),
            sine(1.0, 0.4, adsr(0.003, 4.0, 0.15, 0.6)).cents(-5.0),
            sine(1.0, 0.03, adsr(0.003, 2.5, 0.0, 0.6)),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    let chorus = p.lfo(lfo(LfoTriangle, 0.8).free());
    p.route(ModTarget::OpPitch(4), slot(chorus, 0.08));
    p.filter(lowpass(3000.0, 0.0).keytrack(0.5));
    // Body left of centre, the chorused carrier right: width from the chorus.
    p.pan(&[(1, -0.3), (5, 0.35)]);
    p.struck(0.35, -3.0);
    p
}

fn wurli() -> Patch {
    // Reedy: a 1:1 modulator with feedback, driven a little harder by the
    // touch, through a slightly overdriven low-pass; fast tremolo.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.002, 2.0, 0.3, 0.3)),
            sine(1.0, 0.0, adsr(0.002, 0.9, 0.2, 0.3))
                .dynamic(0.08, 0.22)
                .fb(0.35),
            sine(2.0, 0.25, adsr(0.002, 0.5, 0.0, 0.2)),
            sine(3.0, 0.0, adsr(0.001, 0.15, 0.0, 0.1)).dynamic(0.0, 0.08),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let tremolo = p.lfo(lfo(LfoSine, 5.5).free());
    p.route(ModTarget::Volume, slot(tremolo, 0.17));
    p.filter(
        lowpass(2800.0, 0.1)
            .drive(0.35)
            .keytrack(0.5)
            .drive_mod(velocity(0.3)),
    );
    p.struck(0.3, -3.0);
    p
}

fn e_piano_bell() -> Patch {
    // E-piano body with a bell layer (3.5:1) and a slow chorus.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.002, 3.0, 0.1, 0.6)),
            sine(1.0, 0.0, adsr(0.002, 1.2, 0.05, 0.6)).dynamic(0.04, 0.12),
            sine(1.0, 0.45, adsr(0.002, 2.5, 0.0, 0.8)),
            sine(3.5, 0.0, adsr(0.001, 1.0, 0.0, 0.6)).dynamic(0.05, 0.14),
            sine(2.0, 0.2, adsr(0.002, 1.5, 0.0, 0.5)),
            sine(7.0, 0.05, adsr(0.001, 0.3, 0.0, 0.2)),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    let chorus = p.lfo(lfo(LfoTriangle, 0.6).free());
    p.route(ModTarget::OpPitch(2), slot(chorus, 0.06));
    // The bell layer right of the body.
    p.pan(&[(1, -0.2), (3, 0.35)]);
    p.struck(0.35, -3.0);
    p
}

fn fm_piano() -> Patch {
    // Two slightly detuned strings (ops 1 and 3) with decaying 1:1
    // modulators, an octave partial, and a very short 13:1 hammer on both
    // strings, softer on the second so they differ in brightness. The
    // low-pass follows the keys and opens with velocity.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 4.0, 0.0, 0.4)),
            sine(1.0, 0.0, adsr(0.001, 1.2, 0.0, 0.4)).dynamic(0.05, 0.16),
            sine(1.0, 0.8, adsr(0.001, 3.5, 0.0, 0.4)).cents(4.0),
            sine(1.0, 0.0, adsr(0.001, 0.9, 0.0, 0.4)).dynamic(0.03, 0.12),
            sine(2.0, 0.3, adsr(0.001, 2.0, 0.0, 0.3)),
            sine(13.0, 0.0, adsr(0.0005, 0.03, 0.0, 0.03)).dynamic(0.02, 0.15),
        ],
        &[(2, 1), (4, 3), (6, 1), (6, 3)],
        &[1, 3, 5],
    );
    p.depth(6, 3, 0.7);
    p.filter(
        lowpass(1500.0, 0.0)
            .keytrack(0.8)
            .cutoff_mod(velocity(30.0)),
    );
    // The two strings slightly apart, like a close-miked piano.
    p.pan(&[(1, -0.25), (3, 0.25)]);
    p.struck(0.45, -4.0);
    p
}

fn clavinet() -> Patch {
    // Square-ish body (1:1 with feedback) plus a 3:1 bite and a square an
    // octave up, thinned by a high-pass.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.0, 0.25, 0.06)),
            sine(1.0, 0.0, adsr(0.001, 0.3, 0.12, 0.06))
                .dynamic(0.08, 0.25)
                .fb(0.4),
            sine(3.0, 0.0, adsr(0.001, 0.12, 0.0, 0.05)).dynamic(0.03, 0.15),
            square(2.0, 0.3, adsr(0.001, 0.6, 0.1, 0.06)),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1, 4],
    );
    p.filter(highpass(250.0, 0.2));
    p.struck(0.25, -2.0);
    p
}

fn harpsichord() -> Patch {
    // Two registers (8' and 4'), each a decaying 1:1 pair, and a 3:1 pluck
    // transient with a 5:1 edge.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.2, 0.0, 0.25)),
            sine(1.0, 0.0, adsr(0.001, 0.9, 0.0, 0.25)).dynamic(0.15, 0.3),
            sine(2.0, 0.6, adsr(0.001, 1.6, 0.0, 0.2)),
            sine(1.0, 0.18, adsr(0.001, 0.6, 0.0, 0.2)).fb(0.3),
            sine(3.0, 0.4, adsr(0.0005, 0.08, 0.0, 0.05)),
            sine(5.0, 0.12, adsr(0.0005, 0.05, 0.0, 0.05)),
        ],
        &[(2, 1), (4, 3), (6, 5)],
        &[1, 3, 5],
    );
    p.filter(highpass(150.0, 0.0));
    p.struck(0.3, -2.0);
    p
}

fn celesta() -> Patch {
    // Sounds an octave up: the carrier in semitone mode (+12), its partials
    // as ratios of the note (8 = 4 × the carrier).
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.0, 0.0, 0.8)).semitones(12.0),
            sine(8.0, 0.0, adsr(0.001, 0.5, 0.0, 0.5)).dynamic(0.02, 0.07),
            sine(6.0, 0.25, adsr(0.001, 0.8, 0.0, 0.5)),
            sine(2.0, 0.04, adsr(0.001, 0.4, 0.0, 0.3)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.4, -3.0);
    p
}

fn toy_piano() -> Patch {
    // Metal rods: slightly inharmonic 4.1:1 modulation in FM mode and a
    // 5.3 partial, short and bright.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.9, 0.0, 0.3)),
            sine(4.1, 0.0, adsr(0.001, 0.25, 0.0, 0.2))
                .mode(FM)
                .dynamic(0.06, 0.16),
            sine(5.3, 0.25, adsr(0.001, 0.3, 0.0, 0.2)),
            sine(1.0, 0.04, adsr(0.001, 0.1, 0.0, 0.1)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.3, -3.0);
    p
}

fn jazz_organ() -> Patch {
    // Drawbars 16', 8', 5⅓', 4', 2⅔', 2' as six unmodulated sines; the 2'
    // decays a little for the key click. A slow Leslie: one free LFO on
    // pitch and level.
    let mut p = patch(
        [
            sine(0.5, 0.6, gate(0.003, 0.05)),
            sine(1.0, 0.8, gate(0.003, 0.05)),
            sine(1.5, 0.5, gate(0.003, 0.05)),
            sine(2.0, 0.45, gate(0.003, 0.05)),
            sine(3.0, 0.25, gate(0.003, 0.05)),
            sine(4.0, 0.35, adsr(0.001, 0.04, 0.4, 0.05)),
        ],
        &[],
        &[1, 2, 3, 4, 5, 6],
    );
    let leslie = p.lfo(lfo(LfoSine, 0.8).free());
    p.route(ModTarget::Tune, slot(leslie, 0.05));
    p.route(ModTarget::Volume, slot(leslie, 0.1));
    // The rotor sweeps between the speakers.
    p.route(ModTarget::Balance, slot(leslie, 0.35));
    p
}

fn rock_organ() -> Patch {
    // Full drawbars, fast Leslie, through an overdriven amp (soft master
    // distortion: the chord distorts as a whole, like a real one).
    let mut p = patch(
        [
            sine(0.5, 0.7, gate(0.002, 0.05)),
            sine(1.0, 0.9, gate(0.002, 0.05)),
            sine(1.5, 0.6, gate(0.002, 0.05)),
            sine(2.0, 0.6, gate(0.002, 0.05)),
            sine(3.0, 0.3, gate(0.002, 0.05)),
            sine(4.0, 0.4, adsr(0.001, 0.04, 0.5, 0.05)).fb(0.15),
        ],
        &[],
        &[1, 2, 3, 4, 5, 6],
    );
    let leslie = p.lfo(lfo(LfoSine, 6.5).free());
    p.route(ModTarget::Tune, slot(leslie, 0.06));
    p.route(ModTarget::Volume, slot(leslie, 0.13));
    p.route(ModTarget::Balance, slot(leslie, 0.5));
    p.filter(lowpass(5000.0, 0.1).drive(0.2));
    p.distort(SOFT, 16.0, 0.35, 0.8);
    p.output(0.683);
    p
}

fn pipe_organ() -> Patch {
    // Principals 16', 8', 4', 2' and a 2⅔' mixture with slowly speaking
    // pipes; a faint reed (op 6 on the 8').
    let mut p = patch(
        [
            sine(0.5, 0.5, gate(0.06, 0.4)),
            sine(1.0, 0.9, gate(0.05, 0.4)),
            sine(2.0, 0.6, gate(0.04, 0.4)),
            sine(4.0, 0.35, gate(0.04, 0.4)),
            sine(3.0, 0.3, gate(0.05, 0.4)),
            sine(1.0, 0.08, gate(0.08, 0.4)),
        ],
        &[(6, 2)],
        &[1, 2, 3, 4, 5],
    );
    // Ranks spread across the organ case; the bass pipes in the middle.
    p.pan(&[(3, -0.3), (4, 0.3), (5, 0.15)]);
    p
}

fn accordion() -> Patch {
    // Musette: three reeds (saws) detuned by ±9 cents plus a square below,
    // a low-pass, and the bellows as a slow swell.
    let mut p = patch(
        [
            saw(1.0, 0.7, gate(0.03, 0.1)),
            saw(1.0, 0.7, gate(0.03, 0.1)).cents(-9.0),
            saw(1.0, 0.7, gate(0.03, 0.1)).cents(9.0),
            square(0.5, 0.3, gate(0.03, 0.1)),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3, 4],
    );
    p.filter(lowpass(2200.0, 0.2).keytrack(0.5));
    let bellows = p.lfo(lfo(LfoSine, 0.4).free());
    p.route(ModTarget::Volume, slot(bellows, 0.07));
    // The detuned reeds apart: the musette beating moves across.
    p.pan(&[(2, -0.45), (3, 0.45)]);
    p
}

fn harmonium() -> Patch {
    // Reed organ: a square reshaped by op 2 in PWM mode (narrow pulse) and a
    // soft triangle an octave lower, slowly speaking.
    let mut p = patch(
        [
            square(1.0, 0.8, gate(0.08, 0.2)),
            sine(1.0, 0.35, gate(0.08, 0.2)).mode(PWM),
            tri(0.5, 0.4, gate(0.1, 0.2)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.filter(lowpass(2500.0, 0.0).keytrack(0.4));
    p
}
