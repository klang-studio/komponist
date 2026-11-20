//! Plucked strings.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Harp", 0.326, harp),
    entry("Nylon Guitar", 0.330, nylon_guitar),
    entry("Steel Guitar", 0.284, steel_guitar),
    entry("Muted Guitar", 0.512, muted_guitar),
    entry("Koto", 0.405, koto),
    entry("Sitar", 0.429, sitar),
    entry("Banjo", 0.261, banjo),
    entry("Pizzicato", 0.380, pizzicato),
    entry("Hammered Dulcimer", 0.223, dulcimer),
];

fn harp() -> Patch {
    // Round 1:1 pluck, an octave partial, and a short 4:1 finger noise,
    // half as strong on the octave so it stays round.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.5, 0.0, 1.2)),
            sine(1.0, 0.0, adsr(0.001, 0.5, 0.0, 0.5)).dynamic(0.04, 0.12),
            sine(2.0, 0.3, adsr(0.001, 1.2, 0.0, 0.8)),
            sine(4.0, 0.0, adsr(0.0005, 0.04, 0.0, 0.03)).dynamic(0.02, 0.1),
            off(),
            off(),
        ],
        &[(2, 1), (4, 1), (4, 3)],
        &[1, 3],
    );
    p.depth(4, 3, 0.5);
    p.struck(0.4, -3.0);
    p
}

fn nylon_guitar() -> Patch {
    // Two chains (1 ← 2 ← 3 and 4 ← 5) for body and octave; a low-pass that
    // follows the keys and opens with velocity.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.0, 0.0, 0.5)),
            sine(1.0, 0.0, adsr(0.001, 0.6, 0.0, 0.4)).dynamic(0.08, 0.18),
            sine(3.0, 0.0, adsr(0.001, 0.15, 0.0, 0.1)).dynamic(0.02, 0.08),
            sine(2.0, 0.35, adsr(0.001, 1.2, 0.0, 0.4)),
            sine(1.0, 0.06, adsr(0.001, 0.3, 0.0, 0.2)),
            off(),
        ],
        &[(2, 1), (3, 2), (5, 4)],
        &[1, 4],
    );
    p.filter(
        lowpass(2400.0, 0.1)
            .keytrack(0.6)
            .cutoff_mod(velocity(12.0)),
    );
    p.struck(0.4, -3.0);
    p
}

fn steel_guitar() -> Patch {
    // Brighter: stronger 1:1 and 3:1 modulators, a 5:1 pick transient.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.5, 0.0, 0.6)),
            sine(1.0, 0.0, adsr(0.001, 0.8, 0.0, 0.5)).dynamic(0.1, 0.22),
            sine(2.0, 0.6, adsr(0.001, 1.8, 0.0, 0.5)).cents(3.0),
            sine(3.0, 0.0, adsr(0.001, 0.5, 0.0, 0.3)).dynamic(0.04, 0.12),
            sine(5.0, 0.0, adsr(0.0005, 0.05, 0.0, 0.05)).dynamic(0.03, 0.12),
            off(),
        ],
        &[(2, 1), (4, 3), (5, 1)],
        &[1, 3],
    );
    p.filter(lowpass(6000.0, 0.0).keytrack(0.5));
    p.pan(&[(1, -0.15), (3, 0.2)]);
    p.struck(0.4, -3.0);
    p
}

fn muted_guitar() -> Patch {
    // Palm-muted: very short, filtered, brighter with velocity.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.18, 0.0, 0.08)),
            sine(1.0, 0.0, adsr(0.001, 0.06, 0.0, 0.05)).dynamic(0.1, 0.25),
            sine(2.0, 0.3, adsr(0.001, 0.12, 0.0, 0.06)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.filter(
        lowpass(1400.0, 0.2)
            .keytrack(0.5)
            .cutoff_mod(velocity(18.0)),
    );
    p.struck(0.2, -2.0);
    p
}

fn koto() -> Patch {
    // Twangy 1:1 pluck with a flick of pitch (a short envelope bends the
    // note up and back) and a hollow 3:1 partial.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.8, 0.0, 0.5)),
            sine(1.0, 0.0, adsr(0.001, 0.25, 0.0, 0.2)).dynamic(0.12, 0.3),
            sine(3.0, 0.25, adsr(0.001, 0.6, 0.0, 0.3)),
            sine(2.0, 0.05, adsr(0.001, 0.2, 0.0, 0.1)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let flick = p.env(adsr(0.04, 0.12, 0.0, 0.1));
    p.route(ModTarget::Tune, slot(flick, 0.5));
    p.struck(0.4, -3.0);
    p
}

fn sitar() -> Patch {
    // Buzzing jawari: op 2 in PWM mode keeps reshaping a saw while its
    // envelope decays; sympathetic strings swell in at 2 and 3; a resonant
    // low-pass for the nasal colour.
    let mut p = patch(
        [
            saw(1.0, 1.0, adsr(0.001, 3.0, 0.0, 0.8)),
            sine(1.0, 0.0, adsr(0.001, 1.5, 0.2, 0.8))
                .mode(PWM)
                .dynamic(0.3, 0.6),
            sine(2.0, 0.15, adsr(0.05, 3.0, 0.0, 1.5)).cents(4.0),
            sine(3.0, 0.1, adsr(0.08, 3.0, 0.0, 1.5)).cents(-3.0),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3, 4],
    );
    let flick = p.env(adsr(0.02, 0.2, 0.0, 0.1));
    p.route(ModTarget::Tune, slot(flick, 0.3));
    p.filter(lowpass(3000.0, 0.35).keytrack(0.5));
    p.struck(0.3, -2.0);
    p
}

fn banjo() -> Patch {
    // Bright, short twang through a high-pass.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.8, 0.0, 0.2)),
            sine(1.0, 0.0, adsr(0.001, 0.15, 0.0, 0.1))
                .dynamic(0.15, 0.3)
                .fb(0.2),
            sine(3.0, 0.35, adsr(0.001, 0.3, 0.0, 0.1)),
            sine(5.0, 0.0, adsr(0.0005, 0.04, 0.0, 0.03)).dynamic(0.03, 0.12),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.filter(highpass(300.0, 0.3));
    p.struck(0.4, -3.0);
    p
}

fn pizzicato() -> Patch {
    // Two detuned saws plucked through a low-pass with a fast envelope; the
    // pluck is a little softer on the second saw.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.001, 0.5, 0.0, 0.25)),
            saw(1.0, 0.8, adsr(0.001, 0.45, 0.0, 0.25)).cents(7.0),
            sine(1.0, 0.0, adsr(0.001, 0.08, 0.0, 0.05)).dynamic(0.03, 0.1),
            off(),
            off(),
            off(),
        ],
        &[(3, 1), (3, 2)],
        &[1, 2],
    );
    p.depth(3, 2, 0.75);
    let env = p.env(perc(0.25));
    p.filter(
        lowpass(500.0, 0.1)
            .keytrack(0.7)
            .cutoff_mod(slot(env, 30.0)),
    );
    p.struck(0.3, -2.0);
    p
}

fn dulcimer() -> Patch {
    // Two courses per note, ±6 cents apart, each a bright 1:1 pair, and a
    // metallic 4:1 strike on both.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.5, 0.0, 1.2)).cents(-6.0),
            sine(1.0, 0.0, adsr(0.001, 0.7, 0.0, 0.6)).dynamic(0.06, 0.16),
            sine(1.0, 1.0, adsr(0.001, 2.3, 0.0, 1.2)).cents(6.0),
            sine(1.0, 0.0, adsr(0.001, 0.6, 0.0, 0.6)).dynamic(0.06, 0.16),
            sine(4.0, 0.0, adsr(0.0005, 0.03, 0.0, 0.03)).dynamic(0.03, 0.12),
            off(),
        ],
        &[(2, 1), (4, 3), (5, 1), (5, 3)],
        &[1, 3],
    );
    // The two courses apart: their beating moves across.
    p.pan(&[(1, -0.3), (3, 0.3)]);
    p.struck(0.4, -3.0);
    p
}
