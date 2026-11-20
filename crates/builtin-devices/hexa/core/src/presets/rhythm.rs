//! Rhythmic sounds from looping envelopes: gates, pumping, repeats. The
//! cycles are timed for 120 BPM (there is no tempo sync yet); block rounding
//! makes each a fraction of a millisecond longer.

use super::build::*;
use super::{Entry, entry};
use crate::{EnvelopeParams, Patch};

/// Note lengths at 120 BPM, in seconds.
const QUARTER: f32 = 0.5;
const EIGHTH: f32 = 0.25;
const SIXTEENTH: f32 = 0.125;

pub(super) const LIST: &[Entry] = &[
    entry("Trance Gate Pad", 0.439, trance_gate),
    entry("Pump Pad", 0.571, pump_pad),
    entry("Bubble Bass", 0.550, bubble_bass),
    entry("Bell Repeat", 0.325, bell_repeat),
    entry("Auto-Wah Clav", 1.000, auto_wah_clav),
];

fn trance_gate() -> Patch {
    // Detuned saws whose own envelopes loop in sixteenths: open for half a
    // step, then a fast fall to silence, so a held chord chops itself up.
    let gate = looped(EnvelopeParams {
        hold: 0.5 * SIXTEENTH - 0.002,
        ..adsr(0.002, 0.5 * SIXTEENTH, 0.0, 0.3)
    });
    let mut p = patch(
        [
            saw(1.0, 0.8, gate).cents(-8.0),
            saw(1.0, 0.8, gate).cents(8.0),
            saw(0.5, 0.5, gate),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    p.filter(lowpass(3500.0, 0.2).db24().keytrack(0.3));
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p.wet(DELAY, 0.25);
    p.wet(REVERB, 0.3);
    p
}

fn pump_pad() -> Patch {
    // Sidechain pumping without a kick: a looping envelope on the volume
    // ducks the pad on every quarter and lets it swell back.
    let mut p = patch(
        [
            saw(1.0, 0.7, adsr(0.3, 1.0, 0.9, 1.2)).cents(-6.0),
            saw(1.0, 0.7, adsr(0.3, 1.0, 0.9, 1.2)).cents(6.0),
            tri(0.5, 0.6, adsr(0.3, 1.0, 0.9, 1.2)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let duck = p.env(looped(adsr(0.005, QUARTER - 0.005, 0.0, 0.1)));
    p.route(ModTarget::Volume, slot(duck, -0.8));
    p.filter(lowpass(2200.0, 0.15).keytrack(0.3));
    p.pan(&[(1, -0.5), (2, 0.5)]);
    p.wet(CHORUS, 0.3);
    p.wet(REVERB, 0.3);
    p.output(0.699);
    p
}

fn bubble_bass() -> Patch {
    // Mono: a folded sine whose fold snaps open on every sixteenth and
    // closes again, over a clean sub; the bass talks while held.
    let mut p = patch(
        [
            sine(1.0, 0.9, adsr(0.002, 0.5, 0.8, 0.1)).fold(0.04),
            sine(0.5, 0.8, adsr(0.002, 0.5, 0.9, 0.1)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let blub = p.env(looped(adsr(0.002, SIXTEENTH - 0.002, 0.0, 0.1)));
    p.route(ModTarget::OpFold(0), slot(blub, 0.45));
    p.filter(lowpass(2500.0, 0.15).db24().keytrack(0.5));
    p.mono(MONO, 0.0);
    p
}

fn bell_repeat() -> Patch {
    // An FM bell whose modulators strike again on every eighth: the tone
    // decays slowly while the brightness keeps flaring, like an echo in the
    // sound itself.
    let strike = looped(adsr(0.001, EIGHTH - 0.001, 0.0, 0.5));
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 4.0, 0.0, 1.5)),
            sine(3.5, 0.12, strike),
            sine(2.0, 0.5, adsr(0.001, 3.0, 0.0, 1.5)),
            sine(5.0, 0.06, strike),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    p.struck(0.3, -2.0);
    p.pan(&[(1, -0.3), (3, 0.3)]);
    p.wet(REVERB, 0.35);
    p
}

fn auto_wah_clav() -> Patch {
    // Clavinet through a resonant band-pass whose looping envelope sweeps
    // it on every eighth: a rhythmic wah while the key is held.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.0, 0.4, 0.06)),
            sine(1.0, 0.0, adsr(0.001, 0.3, 0.12, 0.06))
                .dynamic(0.08, 0.25)
                .fb(0.4),
            sine(3.0, 0.0, adsr(0.001, 0.12, 0.0, 0.05)).dynamic(0.03, 0.15),
            square(2.0, 0.3, adsr(0.001, 0.6, 0.2, 0.06)),
            off(),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1, 4],
    );
    let wah = p.env(looped(EnvelopeParams {
        decay_curve: 0.5,
        ..adsr(0.01, EIGHTH - 0.01, 0.0, 0.1)
    }));
    p.filter(
        bandpass(500.0, 0.6)
            .keytrack(0.4)
            .cutoff_mod(slot(wah, 30.0)),
    );
    p.struck(0.25, -2.0);
    p
}
