//! Ambient textures and glitches: sounds that move by themselves, from
//! random values per note, sample-and-hold and smooth random LFOs and
//! looping envelopes.

use super::build::*;
use super::{Entry, entry};
use crate::{EnvelopeParams, ModSource, Patch};

pub(super) const LIST: &[Entry] = &[
    entry("Crystal Rain", 0.437, crystal_rain),
    entry("Granular Cloud", 0.755, granular_cloud),
    entry("Deep Sea", 0.629, deep_sea),
    entry("Star Field", 0.838, star_field),
    entry("Breath Pad", 0.813, breath_pad),
    entry("Slow-Motion Pad", 0.307, slow_motion_pad),
    entry("Glitch Stutter", 0.234, glitch_stutter),
    entry("Data Stream", 0.359, data_stream),
    entry("Glitch Pluck", 0.213, glitch_pluck),
    entry("Interference", 0.219, interference),
    entry("Wander Pad", 0.289, wander_pad),
];

fn crystal_rain() -> Patch {
    // High FM drops that never sound the same twice: a random value per
    // note shifts the modulator's ratio, detunes the note a little and
    // places it in the stereo field. Echoes and a long hall.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.5, 0.0, 2.5)),
            sine(3.0, 0.0, adsr(0.001, 0.8, 0.0, 0.8)).dynamic(0.04, 0.12),
            sine(2.0, 0.3, adsr(0.001, 1.5, 0.0, 1.5)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.route(ModTarget::OpRatio(1), slot(ModSource::Random, 0.15));
    p.route(ModTarget::Tune, slot(ModSource::Random, 0.15));
    p.route(ModTarget::Balance, slot(ModSource::Random, 0.8));
    p.struck(0.3, -2.0);
    p.echo(0.3, 0.5, 0.3, true);
    p.hall(6.0, 0.4, 0.5);
    p
}

fn granular_cloud() -> Patch {
    // Three streams of tiny grains (looping operator envelopes of different
    // lengths), each jumping to a new pitch on a free sample-and-hold LFO,
    // spread across the stereo field and smeared by a long hall.
    let grain = |length: f32| looped(adsr(0.01, length, 0.0, 0.8));
    let mut p = patch(
        [
            sine(1.0, 0.8, grain(0.07)),
            tri(2.0, 0.6, grain(0.05)),
            sine(0.5, 0.7, grain(0.09)),
            sine(1.0, 0.3, adsr(1.0, 1.0, 0.8, 2.0)),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3, 4],
    );
    let jump_a = p.lfo(lfo(SampleHold, 13.0).free());
    let jump_b = p.lfo(lfo(SampleHold, 17.0).free());
    p.route(ModTarget::OpPitch(0), slot(jump_a, 7.0));
    p.route(ModTarget::OpPitch(1), slot(jump_b, 12.0));
    p.route(ModTarget::OpPitch(2), slot(jump_a, -5.0));
    p.route(ModTarget::OpPan(0), slot(jump_b, 0.8));
    p.route(ModTarget::OpPan(1), slot(jump_a, -0.8));
    p.filter(lowpass(4000.0, 0.1).keytrack(0.3));
    p.hall(8.0, 0.4, 0.6);
    p.output(0.7);
    p
}

fn deep_sea() -> Patch {
    // A dark drone: low saw, sub sine and rumbling noise through a resonant
    // low-pass that two slow free LFOs move in and out of sync. The mod
    // wheel lets light in from above.
    let env = adsr(2.0, 1.0, 1.0, 4.0);
    let mut p = patch(
        [
            saw(0.5, 0.8, env).cents(-5.0),
            sine(0.25, 0.7, env),
            noise(0.3, env),
            saw(0.5, 0.5, env).cents(5.0),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3, 4],
    );
    let swell = p.lfo(lfo(LfoSine, 0.05).free());
    let tide = p.lfo(lfo(LfoTriangle, 0.08).free().phase(0.3));
    p.filter(
        lowpass(220.0, 0.5)
            .db24()
            .keytrack(0.3)
            .cutoff_mod(slot(ModSource::ModWheel, 36.0)),
    );
    p.route(ModTarget::Cutoff(0), slot(swell, 18.0));
    p.route(ModTarget::Resonance(0), slot(tide, 0.2));
    p.pan(&[(1, -0.5), (4, 0.5)]);
    p.hall(10.0, 0.8, 0.5);
    p.output(0.561);
    p
}

fn star_field() -> Patch {
    // High partials twinkling on their own (free sample-and-hold LFOs on
    // their levels, each at its own rate) over a soft fundamental.
    let env = adsr(0.8, 1.0, 0.9, 3.0);
    let mut p = patch(
        [
            sine(1.0, 0.6, env),
            sine(4.0, 0.25, env),
            sine(6.0, 0.25, env),
            sine(8.0, 0.25, env),
            sine(11.0, 0.2, env),
            off(),
        ],
        &[],
        &[1, 2, 3, 4, 5],
    );
    for (op, rate) in [(1, 5.0), (2, 7.0), (3, 9.0), (4, 11.0)] {
        let twinkle = p.lfo(lfo(SampleHold, rate).free());
        p.route(ModTarget::OpLevel(op), slot(twinkle, 0.25));
    }
    p.pan(&[(2, -0.6), (3, 0.6), (4, -0.3), (5, 0.3)]);
    p.echo(0.5, 0.5, 0.3, true);
    p.hall(8.0, 0.3, 0.5);
    p.output(0.374);
    p
}

fn breath_pad() -> Patch {
    // Pitched breath: noise through two resonant band-pass operators that
    // follow the note (root and octave), over a soft triangle, swelling
    // slowly in and out.
    let env = adsr(1.5, 1.0, 1.0, 2.5);
    let mut p = patch(
        [
            filter_op(BP, 1.0, 0.7, env),
            filter_op(BP, 2.0, 0.65, env),
            noise(1.0, gate(0.01, 2.5)),
            tri(1.0, 0.7, env),
            off(),
            off(),
        ],
        &[(3, 1), (3, 2)],
        &[1, 2, 4],
    );
    let breath = p.lfo(lfo(LfoSine, 0.2).free());
    p.route(ModTarget::OpLevel(0), slot(breath, 0.3));
    p.route(ModTarget::OpLevel(1), slot(breath, -0.3));
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p.wet(CHORUS, 0.3);
    p.hall(6.0, 0.4, 0.5);
    p.output(0.494);
    p
}

fn slow_motion_pad() -> Patch {
    // An FM pad that unfolds slowly: soft notes take seconds to come in,
    // hard ones speak at once (velocity shortens the attacks), and the
    // modulator's brightness keeps growing on a slow attack of its own.
    let env = adsr(3.0, 2.0, 0.9, 3.0);
    let mut p = patch(
        [
            sine(1.0, 1.0, env),
            sine(2.0, 0.08, adsr(5.0, 2.0, 0.8, 3.0)),
            sine(1.0, 0.6, env).cents(6.0),
            sine(3.0, 0.05, adsr(6.0, 2.0, 0.8, 3.0)),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    for op in [0, 2] {
        p.route(
            ModTarget::OpEnvelope(op, 1),
            slot(ModSource::Velocity, -0.4),
        );
    }
    p.pan(&[(1, -0.4), (3, 0.4)]);
    p.wet(CHORUS, 0.3);
    p.hall(8.0, 0.4, 0.5);
    p.output(0.350);
    p
}

fn glitch_stutter() -> Patch {
    // A held note stutters in thirty-seconds (looping envelopes) and jumps
    // in pitch and brightness on a sample-and-hold LFO, crushed.
    let stutter = looped(EnvelopeParams {
        hold: 0.03,
        ..adsr(0.001, 0.03, 0.0, 0.05)
    });
    let mut p = patch(
        [
            saw(1.0, 0.8, stutter),
            square(1.0, 0.5, stutter).cents(10.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let jump = p.lfo(lfo(SampleHold, 8.0));
    p.route(ModTarget::Tune, slot(jump, 12.0));
    p.filter(
        lowpass(2000.0, 0.4)
            .keytrack(0.4)
            .cutoff_mod(slot(jump, 24.0)),
    );
    p.route(ModTarget::Balance, slot(jump, 0.6));
    p.crush(6.0, 8_000.0, 0.5);
    p.output(0.528);
    p
}

fn data_stream() -> Patch {
    // Computer chatter: an FM pair whose carrier ratio and modulation
    // jump on a fast sample-and-hold LFO; the harder you play, the faster
    // it chatters.
    let mut p = patch(
        [
            sine(2.0, 1.0, gate(0.002, 0.05)),
            sine(1.0, 0.1, gate(0.002, 0.05)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let chatter = p.lfo(lfo(SampleHold, 12.0).rate_mod(velocity(1.5)));
    p.route(ModTarget::OpRatio(0), slot(chatter, 0.6));
    p.route(ModTarget::OpLevel(1), slot(chatter, 0.1));
    p.filter(bandpass(2000.0, 0.3).keytrack(0.5));
    p.crush(8.0, 12_000.0, 0.5);
    p.output(0.508);
    p
}

fn glitch_pluck() -> Patch {
    // A crushed FM pluck with a random value per note on the modulator's
    // ratio and depth: every note a different tone colour.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.25)),
            sine(2.0, 0.0, perc(0.12)).dynamic(0.08, 0.2),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    p.route(ModTarget::OpRatio(1), slot(ModSource::Random, 0.6));
    p.route(ModTarget::OpDepth(1, 0), slot(ModSource::Random, 0.4));
    p.crush(5.0, 6_000.0, 0.6);
    p.echo(0.125, 0.3, 0.25, true);
    p
}

fn interference() -> Patch {
    // A saw ring-modulated by a partner whose pitch runs against the
    // keyboard (key tracking, inverted): every key gives a different
    // clangour. The mod wheel deepens the ring modulation, a slow
    // sample-and-hold LFO detunes the partner in steps.
    let mut p = patch(
        [
            saw(1.0, 0.9, gate(0.005, 0.2)),
            sine(1.5, 0.5, gate(0.005, 0.2)).mode(RING),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    p.route(ModTarget::OpPitch(1), slot(ModSource::KeyTrack, -48.0));
    p.route(ModTarget::OpLevel(1), slot(ModSource::ModWheel, 0.5));
    let steps = p.lfo(lfo(SampleHold, 3.0));
    p.route(ModTarget::OpPitch(1), slot(steps, 2.0));
    p.filter(lowpass(5000.0, 0.2));
    p.distort(HARD, 6.0, 0.3, 0.3);
    p.output(0.701);
    p
}

fn wander_pad() -> Patch {
    // A soft FM pad that never sits still: free smooth random LFOs glide the
    // modulators' brightness, the filter, a slight detune and the stereo
    // position from one random value to the next, without the steps of
    // sample-and-hold; every voice takes its own course.
    let env = adsr(1.2, 1.5, 0.9, 3.0);
    let mut p = patch(
        [
            sine(1.0, 1.0, env),
            sine(2.0, 0.12, env),
            sine(1.0, 0.7, env).cents(-7.0),
            sine(3.0, 0.06, env),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    );
    let glow = p.lfo(lfo(SmoothRandom, 0.4).free());
    let drift = p.lfo(lfo(SmoothRandom, 0.25).free());
    let sway = p.lfo(lfo(SmoothRandom, 0.15).free());
    p.route(ModTarget::OpLevel(1), slot(glow, 0.08));
    p.route(ModTarget::OpLevel(3), slot(glow, -0.04));
    p.route(ModTarget::Tune, slot(drift, 0.12));
    p.filter(
        lowpass(2500.0, 0.2)
            .keytrack(0.4)
            .cutoff_mod(slot(glow, 12.0)),
    );
    p.route(ModTarget::Balance, slot(sway, 0.5));
    p.wet(CHORUS, 0.25);
    p.hall(7.0, 0.4, 0.5);
    p.output(0.332);
    p
}
