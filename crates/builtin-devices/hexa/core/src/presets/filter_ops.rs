//! Operators as filters (opsix style): each operator can filter its own
//! sources, so one voice holds several filters, in parallel or in series,
//! with their cutoffs following the note or fixed, and filter FM.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("Formant Choir", 1.000, formant_choir),
    entry("Filter FM Bass", 0.349, filter_fm_bass),
    entry("Filter Snare", 0.428, filter_snare),
    entry("Stereo Filter Pad", 0.158, stereo_filter_pad),
    entry("Dual Filter Pluck", 0.272, double_filter_pluck),
];

fn formant_choir() -> Patch {
    // Two detuned saws through three parallel band-passes at the formants
    // of "a" (700, 1150, 2600 Hz, fixed like a vocal tract). A slow LFO
    // lowers the first two towards "o" and back.
    let voice = adsr(0.4, 1.0, 0.9, 1.2);
    let mut p = patch(
        [
            filter_op(BP, 1.0, 0.7, voice).fixed(700.0),
            filter_op(BP, 1.0, 0.7, voice).fixed(1150.0),
            filter_op(BP, 1.0, 0.6, voice).fixed(2600.0),
            off(),
            saw(1.0, 1.0, gate(0.01, 1.2)).cents(-7.0),
            saw(1.0, 1.0, gate(0.01, 1.2)).cents(7.0),
        ],
        &[(5, 1), (5, 2), (5, 3), (6, 1), (6, 2), (6, 3)],
        &[1, 2, 3],
    );
    p.operators[2].level = 0.5;
    let vowel = p.lfo(lfo(LfoSine, 0.15).free());
    p.route(ModTarget::OpPitch(0), slot(vowel, -3.0));
    p.route(ModTarget::OpPitch(1), slot(vowel, -2.5));
    p.vibrato(5.0, 0.1, 0.8);
    p.pan(&[(1, -0.2), (2, 0.2)]);
    p.wet(CHORUS, 0.35);
    p.wet(REVERB, 0.35);
    p.output(0.592);
    p
}

fn filter_fm_bass() -> Patch {
    // The opsix growl: a saw through a resonant low-pass operator an octave
    // above the note, whose cutoff a sine at the note moves at audio rate
    // (frequency mode); the touch and a short envelope set how much. A clean
    // sub underneath.
    let mut p = patch(
        [
            filter_op(LP, 2.0, 0.6, adsr(0.002, 0.8, 0.7, 0.1)),
            saw(1.0, 1.0, gate(0.002, 0.1)),
            sine(1.0, 0.0, adsr(0.001, 0.4, 0.3, 0.1))
                .mode(FM)
                .dynamic(0.03, 0.09),
            sine(0.5, 0.7, adsr(0.002, 0.8, 0.8, 0.1)),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1, 4],
    );
    p.mono(MONO, 0.0);
    p
}

fn filter_snare() -> Patch {
    // The same on every key: a sine body dropping a fifth, and one noise
    // source through two filter operators of its own, a band-pass for the
    // snares and a high-pass for the sizzle, each with its own decay. The
    // body stays free of noise filtering, which one voice filter cannot do.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.12)).fixed(190.0),
            filter_op(BP, 1.0, 0.3, perc(0.2)).fixed(2500.0),
            noise(1.0, perc(0.2)).dynamic(0.5, 1.0),
            filter_op(HP, 1.0, 0.2, perc(0.07)).fixed(7000.0),
            off(),
            off(),
        ],
        &[(3, 2), (3, 4)],
        &[1, 2, 4],
    );
    let drop = p.env(perc(0.03));
    p.route(ModTarget::OpPitch(0), slot(drop, 7.0));
    p.operators[3].level = 0.6;
    p.pan(&[(4, 0.2)]);
    p.wet(REVERB, 0.15);
    p
}

fn stereo_filter_pad() -> Patch {
    // The same detuned saws through a low-pass on the left and a high-pass
    // on the right; one slow LFO moves both cutoffs in opposite directions,
    // so the sound wanders between the sides. The saws open at once, the
    // filters' envelopes shape the swell.
    let pad = adsr(1.0, 2.0, 0.9, 2.5);
    let mut p = patch(
        [
            filter_op(LP, 3.0, 0.5, pad),
            filter_op(HP, 3.0, 0.5, pad),
            saw(1.0, 1.0, gate(0.01, 2.5)).cents(-6.0),
            saw(1.0, 1.0, gate(0.01, 2.5)).cents(6.0),
            off(),
            off(),
        ],
        &[(3, 1), (3, 2), (4, 1), (4, 2)],
        &[1, 2],
    );
    let drift = p.lfo(lfo(LfoTriangle, 0.08).free());
    p.route(ModTarget::OpPitch(0), slot(drift, 12.0));
    p.route(ModTarget::OpPitch(1), slot(drift, -12.0));
    p.pan(&[(1, -0.8), (2, 0.8)]);
    p.wet(REVERB, 0.4);
    p
}

fn double_filter_pluck() -> Patch {
    // Two synths in one: a saw through a low-pass whose envelope sweeps the
    // cutoff down three octaves, and a saw an octave up through a more
    // resonant low-pass with a faster sweep.
    let mut p = patch(
        [
            filter_op(LP, 1.0, 0.4, adsr(0.001, 1.2, 0.0, 0.4)),
            filter_op(LP, 2.0, 0.7, adsr(0.001, 0.6, 0.0, 0.3)),
            saw(1.0, 1.0, gate(0.001, 0.4)).cents(-4.0),
            saw(2.0, 1.0, gate(0.001, 0.3)).cents(4.0),
            off(),
            off(),
        ],
        &[(3, 1), (4, 2)],
        &[1, 2],
    );
    let slow = p.env(perc(0.35));
    let fast = p.env(perc(0.12));
    p.route(ModTarget::OpPitch(0), slot(slow, 36.0));
    p.route(ModTarget::OpPitch(1), slot(fast, 30.0));
    p.operators[1].level = 0.6;
    p.pan(&[(1, -0.3), (2, 0.3)]);
    p.wet(DELAY, 0.2);
    p
}
