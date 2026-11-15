//! Techno and house: acid lines, dub chords, blips and stabs. Delays are
//! timed for 120 BPM, like the rhythmic presets.

use super::build::*;
use super::{Entry, entry};
use crate::{EnvelopeParams, ModSource, Patch};

pub(super) const LIST: &[Entry] = &[
    entry("303 Squelch", 0.118, squelch),
    entry("Acid Square", 0.104, acid_square),
    entry("Rumble", 0.271, rumble),
    entry("Dub Chord", 0.813, dub_chord),
    entry("Minimal Blip", 0.363, minimal_blip),
    entry("House Organ", 0.697, house_organ),
    entry("Deep Chord", 0.665, deep_chord),
    entry("Hypno Sequence", 0.401, hypno_sequence),
    entry("Steel Hit", 0.438, steel_hit),
    entry("Detroit Strings", 0.625, detroit_strings),
];

fn squelch() -> Patch {
    // The acid box: one saw through a resonant 24 dB low-pass whose
    // envelope snaps shut. Hard notes are accents (more envelope and
    // resonance), overlapping notes slide; the mod wheel lengthens both the
    // filter decay and the slide, aftertouch makes it scream.
    let mut p = patch(
        [
            saw(1.0, 1.0, adsr(0.002, 1.2, 0.6, 0.04)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    let env = p.env(EnvelopeParams {
        decay_curve: 0.6,
        ..adsr(0.001, 0.22, 0.0, 0.05)
    });
    p.filter(
        lowpass(220.0, 0.72)
            .db24()
            .keytrack(0.4)
            .resonance_mod(velocity(0.15)),
    );
    p.route(ModTarget::Cutoff(0), slot(env, 36.0));
    p.route_via(ModTarget::Cutoff(0), slot(env, 18.0), ModSource::Velocity);
    p.route(
        ModTarget::ModEnvelope(0, 3),
        slot(ModSource::ModWheel, 0.25),
    );
    p.route(ModTarget::Glide, slot(ModSource::ModWheel, 0.3));
    p.route(ModTarget::Resonance(0), slot(ModSource::Aftertouch, 0.2));
    p.mono(LEGATO, 0.06);
    p.distort(SOFT, 14.0, 0.35, 0.45);
    p.echo(0.375, 0.35, 0.15, true);
    p
}

fn acid_square() -> Patch {
    // The acid line on the square wave, hollower and rounder, with a hard
    // clipper; the mod wheel opens the filter, aftertouch adds envelope.
    let mut p = patch(
        [
            square(1.0, 0.9, adsr(0.002, 1.0, 0.6, 0.04)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    let env = p.env(adsr(0.001, 0.3, 0.0, 0.05));
    p.filter(
        lowpass(260.0, 0.68)
            .db24()
            .keytrack(0.4)
            .cutoff_mod(slot(ModSource::ModWheel, 30.0)),
    );
    p.touch_cutoff(env, 40.0);
    p.route_via(ModTarget::Cutoff(0), slot(env, 18.0), ModSource::Aftertouch);
    p.mono(MONO, 0.0);
    p.distort(HARD, 8.0, 0.4, 0.3);
    p.echo(0.375, 0.4, 0.2, true);
    p
}

fn rumble() -> Patch {
    // The techno rumble: a kick-like sine dropping into a long, distorted
    // low tail with a dull roar of filtered noise, smeared by a dark reverb.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.0005, 1.4, 0.0, 0.6)),
            sine(1.0, 0.0, perc(0.02)).fixed(220.0).dynamic(0.05, 0.2),
            noise(0.35, adsr(0.005, 1.0, 0.0, 0.5)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let drop = p.env(adsr(0.0005, 0.07, 0.0, 0.05));
    p.route(ModTarget::OpPitch(0), slot(drop, 30.0));
    p.filter(lowpass(380.0, 0.3).keytrack(0.5));
    p.distort(SOFT, 20.0, 0.6, 0.6);
    p.hall(4.0, 0.85, 0.4);
    p.effects.eq.low_gain = 4.0;
    p.output(0.317);
    p
}

fn dub_chord() -> Patch {
    // Dub techno: a minor-seventh chord on every key (ops 2–4 a third,
    // fifth and seventh above) through a short low-pass, drowned in a dark
    // ping-pong delay. The mod wheel opens it.
    let env = adsr(0.002, 0.3, 0.0, 0.25);
    let mut p = patch(
        [
            saw(1.0, 0.6, env),
            saw(1.0, 0.5, env).semitones(3.0),
            saw(1.0, 0.5, env).semitones(7.0),
            saw(1.0, 0.45, env).semitones(10.0),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3, 4],
    );
    let sweep = p.env(adsr(0.001, 0.2, 0.0, 0.1));
    p.filter(
        lowpass(600.0, 0.35)
            .keytrack(0.3)
            .cutoff_mod(slot(ModSource::ModWheel, 24.0)),
    );
    p.touch_cutoff(sweep, 24.0);
    p.pan(&[(2, -0.3), (4, 0.3)]);
    p.echo(0.375, 0.65, 0.45, true);
    p.effects.delay.damping = 0.6;
    p.hall(3.0, 0.6, 0.3);
    p
}

fn minimal_blip() -> Patch {
    // A tiny FM blip that is different on every note: a random value per
    // note shifts the modulator's ratio (inharmonic or not) and places it
    // in the stereo field; the echoes bounce between the speakers.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.09)),
            sine(2.0, 0.0, perc(0.05)).dynamic(0.04, 0.12),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    p.route(ModTarget::OpRatio(1), slot(ModSource::Random, 0.45));
    p.route(ModTarget::Balance, slot(ModSource::Random, 0.7));
    p.echo(0.25, 0.4, 0.3, true);
    p
}

fn house_organ() -> Patch {
    // The organ of early-90s house: sine and half-sine drawbars with a
    // percussive fifth and an FM click, short and punchy.
    let body = adsr(0.002, 0.5, 0.6, 0.08);
    let mut p = patch(
        [
            sine(1.0, 1.0, body),
            half(2.0, 0.5, body),
            sine(3.0, 0.35, perc(0.25)),
            sine(1.0, 0.0, perc(0.06)).dynamic(0.03, 0.12),
            sine(4.0, 0.2, body),
            off(),
        ],
        &[(4, 1)],
        &[1, 2, 3, 5],
    );
    p.pan(&[(2, -0.2), (5, 0.2)]);
    p.wet(CHORUS, 0.25);
    p.hall(1.5, 0.4, 0.15);
    p
}

fn deep_chord() -> Patch {
    // Deep house: a minor-ninth chord per key in soft saws, a round
    // 24 dB low-pass that blooms with the touch, chorus. The mod wheel
    // opens the filter, aftertouch brings in the ninth.
    let env = adsr(0.005, 1.2, 0.4, 0.5);
    let mut p = patch(
        [
            saw(1.0, 1.0, env),
            saw(1.0, 0.85, env).semitones(3.0),
            saw(1.0, 0.85, env).semitones(7.0),
            saw(1.0, 0.75, env).semitones(10.0),
            tri(1.0, 0.3, env).semitones(14.0),
            off(),
        ],
        &[],
        &[1, 2, 3, 4, 5],
    );
    let bloom = p.env(adsr(0.01, 0.6, 0.3, 0.5));
    p.filter(
        lowpass(800.0, 0.25)
            .db24()
            .keytrack(0.4)
            .cutoff_mod(slot(ModSource::ModWheel, 24.0)),
    );
    p.touch_cutoff(bloom, 22.0);
    p.route(ModTarget::OpLevel(4), slot(ModSource::Aftertouch, 0.4));
    p.pan(&[(2, -0.35), (4, 0.35)]);
    p.wet(CHORUS, 0.4);
    p.hall(2.5, 0.5, 0.25);
    p
}

fn hypno_sequence() -> Patch {
    // A held note becomes a sequence: a looping envelope plucks the filter
    // on every sixteenth while a slow free LFO drifts the cutoff. The mod
    // wheel lengthens the plucks, aftertouch adds resonance.
    let mut p = patch(
        [
            saw(1.0, 0.8, gate(0.003, 0.1)),
            square(0.5, 0.4, gate(0.003, 0.1)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let pluck = p.env(looped(adsr(0.002, 0.123, 0.0, 0.05)));
    let drift = p.lfo(lfo(LfoTriangle, 0.07).free());
    p.filter(lowpass(250.0, 0.55).db24().keytrack(0.4));
    p.route(ModTarget::Cutoff(0), slot(pluck, 36.0));
    p.route(ModTarget::Cutoff(0), slot(drift, 12.0));
    p.route(ModTarget::ModEnvelope(0, 3), slot(ModSource::ModWheel, 0.2));
    p.route(ModTarget::Resonance(0), slot(ModSource::Aftertouch, 0.25));
    p.mono(MONO, 0.0);
    p.echo(0.375, 0.4, 0.25, true);
    p
}

fn steel_hit() -> Patch {
    // An industrial hit: a sine ring-modulated by an inharmonic partner
    // (√2) with an FM clang, a noise burst and a short downward bend,
    // clipped hard and thrown into a metallic room.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.7)),
            sine(1.414, 1.0, perc(0.7)).mode(RING),
            sine(3.17, 0.0, perc(0.3)).dynamic(0.05, 0.2),
            off(),
            noise(0.4, perc(0.08)),
            off(),
        ],
        &[(2, 1), (3, 1)],
        &[1, 5],
    );
    let bend = p.env(perc(0.12));
    p.route(ModTarget::Tune, slot(bend, 5.0));
    p.distort(HARD, 14.0, 0.3, 0.5);
    p.hall(2.5, 0.2, 0.3);
    p
}

fn detroit_strings() -> Patch {
    // Synth strings as in Detroit techno: detuned saws and an octave,
    // slow attack, deep chorus. A vibrato waits half a second, then fades
    // in; aftertouch deepens it, the mod wheel darkens the strings.
    let env = adsr(0.25, 1.0, 0.85, 0.8);
    let mut p = patch(
        [
            saw(1.0, 0.7, env).cents(-8.0),
            saw(1.0, 0.7, env).cents(8.0),
            saw(2.0, 0.3, env),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    p.filter(
        lowpass(3200.0, 0.15)
            .keytrack(0.3)
            .cutoff_mod(slot(ModSource::ModWheel, -30.0)),
    );
    let vibrato = p.lfo(
        lfo(LfoSine, 5.0)
            .delay(0.5)
            .fade(0.6)
            .amount(0.4, slot(ModSource::Aftertouch, 0.6)),
    );
    p.route(ModTarget::Tune, slot(vibrato, 0.2));
    p.pan(&[(1, -0.5), (2, 0.5)]);
    p.wet(CHORUS, 0.5);
    p.hall(2.5, 0.4, 0.3);
    p.output(0.546);
    p
}
