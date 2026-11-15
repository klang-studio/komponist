//! What the modulation matrix adds: one source on several targets, several
//! sources on one target, and depths scaled by a second source (via).

use super::build::*;
use super::{Entry, entry};
use crate::{ModSource, Patch};

pub(super) const LIST: &[Entry] = &[
    entry("Sing-Lead", 0.527, singing_lead),
    entry("Drift-Pad", 0.442, drift_pad),
    entry("Anschlag-Glocke", 0.311, touch_bell),
];

fn singing_lead() -> Patch {
    // One LFO, three targets: the vibrato also moves the filter and the
    // balance a little, like a singer leaning into the note. All three are
    // scaled by a slow swell (via), so they bloom only on held notes; the
    // cutoff adds that to its own short envelope.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.01, 0.3, 0.85, 0.2)).cents(-5.0),
            saw(1.0, 0.8, adsr(0.01, 0.3, 0.85, 0.2)).cents(5.0),
            sine(0.5, 0.4, adsr(0.01, 0.3, 0.9, 0.2)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let vibrato = p.lfo(lfo(LfoSine, 5.3));
    let swell = p.env(adsr(0.8, 0.1, 1.0, 0.3));
    let pluck = p.env(adsr(0.005, 0.25, 0.3, 0.2));
    p.filter(lowpass(1400.0, 0.3).db24().keytrack(0.5));
    p.route(ModTarget::Cutoff(0), slot(pluck, 18.0));
    p.route_via(ModTarget::Cutoff(0), slot(vibrato, 4.0), swell);
    p.route_via(ModTarget::Tune, slot(vibrato, 0.18), swell);
    p.route_via(ModTarget::Balance, slot(vibrato, 0.3), swell);
    p.mono(LEGATO, 0.05);
    p.wet(DELAY, 0.2);
    p
}

fn drift_pad() -> Patch {
    // Two free LFOs at unrelated slow rates on the same cutoff: their sum
    // never repeats, so the pad keeps drifting. The faster one also pans the
    // two saws against each other.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(1.2, 1.0, 0.9, 2.5)).cents(-8.0),
            saw(1.0, 0.8, adsr(1.2, 1.0, 0.9, 2.5)).cents(8.0),
            tri(0.5, 0.6, adsr(1.2, 1.0, 0.9, 2.5)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let slow = p.lfo(lfo(LfoSine, 0.11).free());
    let slower = p.lfo(lfo(LfoTriangle, 0.07).free());
    p.filter(lowpass(900.0, 0.4).db24().keytrack(0.3));
    p.route(ModTarget::Cutoff(0), slot(slow, 14.0));
    p.route(ModTarget::Cutoff(0), slot(slower, 10.0));
    p.route(ModTarget::OpPan(0), slot(slow, -0.6));
    p.route(ModTarget::OpPan(1), slot(slow, 0.6));
    p.wet(CHORUS, 0.3);
    p.wet(REVERB, 0.4);
    p.output(0.65);
    p
}

fn touch_bell() -> Patch {
    // An FM bell whose strike follows the touch twice over: the modulators'
    // attack envelope is scaled by velocity (via), and hard notes also bend
    // in from a quarter tone above. Soft notes ring pure.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 4.0, 0.0, 2.0)),
            sine(3.5, 0.02, adsr(0.001, 3.0, 0.0, 2.0)),
            sine(2.0, 0.5, adsr(0.001, 3.0, 0.0, 2.0)),
            sine(7.0, 0.0, adsr(0.001, 1.5, 0.0, 1.5)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let strike = p.env(perc(0.4));
    let bend = p.env(perc(0.08));
    p.route_via(
        ModTarget::OpLevel(1),
        slot(strike, 0.12),
        ModSource::Velocity,
    );
    p.route_via(
        ModTarget::OpLevel(3),
        slot(strike, 0.05),
        ModSource::Velocity,
    );
    p.route_via(ModTarget::Tune, slot(bend, 0.5), ModSource::Velocity);
    p.struck(0.3, -2.0);
    p.pan(&[(1, -0.25), (3, 0.25)]);
    p.wet(REVERB, 0.35);
    p
}
