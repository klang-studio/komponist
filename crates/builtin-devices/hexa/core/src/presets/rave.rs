//! Trance and rave: supersaws, plucks, screeching leads, risers. Delays
//! are timed for 120 BPM, like the rhythmic presets.

use super::build::*;
use super::{Entry, entry};
use crate::{EnvelopeParams, ModSource, Patch};

pub(super) const LIST: &[Entry] = &[
    entry("Supersaw", 0.964, supersaw),
    entry("Trance Pluck", 0.628, trance_pluck),
    entry("Hardstyle Lead", 0.906, hardstyle_lead),
    entry("Euro Stab", 0.932, euro_stab),
    entry("Offbeat Bass", 0.521, offbeat_bass),
    entry("Hands-Up Lead", 0.785, hands_up_lead),
    entry("Riser", 0.274, riser),
    entry("Psy Rolling Bass", 0.537, psy_roll_bass),
    entry("Euphoria Pad", 0.684, euphoria_pad),
    entry("Rave Siren", 0.253, rave_siren),
];

fn supersaw() -> Patch {
    // Three saws per voice and four voices per key, spread wide: the
    // trance wall. The mod wheel opens the low-pass.
    let env = adsr(0.01, 0.6, 0.9, 0.6);
    let mut p = patch(
        [
            saw(1.0, 0.6, env).cents(-12.0),
            saw(1.0, 0.6, env),
            saw(1.0, 0.6, env).cents(12.0),
            saw(2.0, 0.2, env),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3, 4],
    );
    p.filter(
        lowpass(2500.0, 0.1)
            .keytrack(0.4)
            .cutoff_mod(slot(ModSource::ModWheel, 30.0)),
    );
    p.pan(&[(1, -0.5), (3, 0.5)]);
    p.unison(4, 30.0, 1.0);
    p.wet(CHORUS, 0.3);
    p.hall(3.0, 0.4, 0.3);
    p.output(0.640);
    p
}

fn trance_pluck() -> Patch {
    // Short detuned saws snapped shut by a resonant low-pass, echoed in
    // dotted eighths across the stereo field.
    let mut p = patch(
        [
            saw(1.0, 0.8, perc(0.5)).cents(-10.0),
            saw(1.0, 0.8, perc(0.5)).cents(10.0),
            square(2.0, 0.3, perc(0.3)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(perc(0.2));
    p.filter(lowpass(350.0, 0.35).db24().keytrack(0.5));
    p.touch_cutoff(env, 50.0);
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p.unison(2, 12.0, 0.8);
    p.echo(0.375, 0.45, 0.35, true);
    p.hall(2.5, 0.4, 0.3);
    p
}

fn hardstyle_lead() -> Patch {
    // Each note dives in from an octave above, through hard clipping; the
    // notes glide. The mod wheel brings in a wide vibrato.
    let env = adsr(0.003, 0.4, 0.85, 0.25);
    let mut p = patch(
        [
            saw(1.0, 0.8, env).cents(-10.0),
            saw(1.0, 0.8, env).cents(10.0),
            square(0.5, 0.4, env),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let dive = p.env(EnvelopeParams {
        decay_curve: 0.5,
        ..adsr(0.001, 0.15, 0.0, 0.1)
    });
    p.route(ModTarget::Tune, slot(dive, 12.0));
    let vibrato = p.lfo(lfo(LfoSine, 6.0).amount(0.0, slot(ModSource::ModWheel, 1.0)));
    p.route(ModTarget::Tune, slot(vibrato, 0.6));
    p.filter(lowpass(5000.0, 0.15));
    p.mono(LEGATO, 0.08);
    p.unison(3, 20.0, 0.9);
    p.distort(HARD, 22.0, 0.45, 0.7);
    p.hall(2.0, 0.4, 0.25);
    p.output(0.349);
    p
}

fn euro_stab() -> Patch {
    // The 90s eurodance stab: detuned saws, a square octave and a fifth
    // ping, short, through a snappy low-pass.
    let env = adsr(0.002, 0.45, 0.0, 0.2);
    let mut p = patch(
        [
            saw(1.0, 0.7, env).cents(-6.0),
            saw(1.0, 0.7, env).cents(6.0),
            square(2.0, 0.4, env),
            sine(3.0, 0.3, perc(0.2)),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3, 4],
    );
    let snap = p.env(adsr(0.001, 0.25, 0.0, 0.1));
    p.filter(lowpass(1500.0, 0.2).keytrack(0.4));
    p.touch_cutoff(snap, 30.0);
    p.pan(&[(1, -0.3), (2, 0.3)]);
    p.wet(CHORUS, 0.3);
    p.hall(1.8, 0.4, 0.25);
    p
}

fn offbeat_bass() -> Patch {
    // The trance bass between the kicks: short saw and sub, a resonant
    // low-pass that bites harder with the touch.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.002, 0.25, 0.0, 0.08)),
            sine(0.5, 0.6, adsr(0.002, 0.25, 0.0, 0.08)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let env = p.env(perc(0.15));
    p.filter(lowpass(250.0, 0.4).db24().keytrack(0.5));
    p.touch_cutoff(env, 36.0);
    p.mono(MONO, 0.0);
    p
}

fn hands_up_lead() -> Patch {
    // A hard-sync lead (op 2 restarts op 1) swept by an envelope; the mod
    // wheel pushes the sync harder. A vibrato starts after a moment.
    let env = adsr(0.003, 0.4, 0.85, 0.3);
    let mut p = patch(
        [
            saw(1.0, 1.0, env),
            sine(1.0, 0.15, gate(0.003, 0.3)).mode(SYNC),
            square(1.0, 0.8, env).cents(7.0),
            saw(2.0, 0.5, env).cents(-5.0),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3, 4],
    );
    let sweep = p.env(adsr(0.003, 0.6, 0.25, 0.3));
    p.route(ModTarget::OpLevel(1), slot(sweep, 0.4));
    p.route(ModTarget::OpLevel(1), slot(ModSource::ModWheel, 0.4));
    let vibrato = p.lfo(lfo(LfoSine, 5.5).delay(0.3).fade(0.4));
    p.route(ModTarget::Tune, slot(vibrato, 0.15));
    p.filter(lowpass(4500.0, 0.15).keytrack(0.3));
    p.pan(&[(3, -0.3), (4, 0.3)]);
    p.mono(LEGATO, 0.05);
    p.unison(2, 15.0, 0.8);
    p.echo(0.375, 0.35, 0.25, true);
    p.hall(2.0, 0.4, 0.25);
    p
}

fn riser() -> Patch {
    // The build-up: noise and a saw through a low-pass that climbs for
    // two seconds while the saw rises an octave and the sound circles
    // faster and faster between the speakers.
    let mut p = patch(
        [
            noise(1.0, adsr(0.3, 0.1, 1.0, 1.0)),
            saw(1.0, 0.7, adsr(0.3, 0.1, 1.0, 1.0)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let rise = p.env(EnvelopeParams {
        attack_curve: -0.5,
        ..adsr(2.0, 0.1, 1.0, 1.0)
    });
    p.filter(lowpass(200.0, 0.4).cutoff_mod(slot(rise, 60.0)));
    p.route(ModTarget::Tune, slot(rise, 12.0));
    let circle = p.lfo(lfo(LfoSine, 0.8).rate_mod(slot(rise, 3.0)));
    p.route(ModTarget::Balance, slot(circle, 0.7));
    p.hall(3.0, 0.3, 0.35);
    p
}

fn psy_roll_bass() -> Patch {
    // Psytrance: a held note rolls in sixteenths, each one a short saw
    // pluck (looping envelopes on the amplitude and the filter). The mod
    // wheel opens the filter.
    let roll = looped(adsr(0.001, 0.12, 0.0, 0.05));
    let mut p = patch(
        [
            saw(1.0, 1.0, roll),
            sine(0.5, 0.5, roll),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let pluck = p.env(looped(adsr(0.001, 0.08, 0.0, 0.05)));
    p.filter(
        lowpass(200.0, 0.35)
            .db24()
            .keytrack(0.5)
            .cutoff_mod(slot(ModSource::ModWheel, 24.0)),
    );
    p.route(ModTarget::Cutoff(0), slot(pluck, 30.0));
    p.route_via(ModTarget::Cutoff(0), slot(pluck, 12.0), ModSource::Velocity);
    p.mono(MONO, 0.0);
    p
}

fn euphoria_pad() -> Patch {
    // Big, slowly opening saws for the breakdown: a free LFO breathes in
    // the cutoff, aftertouch lifts it, the mod wheel adds resonance.
    let env = adsr(1.0, 1.0, 0.9, 2.0);
    let mut p = patch(
        [
            saw(1.0, 0.7, env).cents(-9.0),
            saw(1.0, 0.7, env).cents(9.0),
            saw(0.5, 0.4, env),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let breath = p.lfo(lfo(LfoSine, 0.12).free());
    let open = p.env(adsr(2.5, 1.0, 0.7, 2.0));
    p.filter(
        lowpass(500.0, 0.25)
            .db24()
            .keytrack(0.3)
            .cutoff_mod(slot(open, 24.0)),
    );
    p.route(ModTarget::Cutoff(0), slot(breath, 8.0));
    p.route(ModTarget::Cutoff(0), slot(ModSource::Aftertouch, 18.0));
    p.route(ModTarget::Resonance(0), slot(ModSource::ModWheel, 0.5));
    p.pan(&[(1, -0.5), (2, 0.5)]);
    p.unison(3, 20.0, 1.0);
    p.wet(CHORUS, 0.4);
    p.hall(5.0, 0.4, 0.45);
    p.output(0.469);
    p
}

fn rave_siren() -> Patch {
    // The siren: saw and square wailing up and down a fifth through a
    // resonant low-pass that follows; the mod wheel spins it faster.
    let env = adsr(0.01, 0.2, 1.0, 0.3);
    let mut p = patch(
        [
            saw(1.0, 1.0, env),
            square(1.0, 0.7, env).cents(10.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let wail = p.lfo(
        lfo(LfoTriangle, 1.2)
            .phase(0.75)
            .rate_mod(slot(ModSource::ModWheel, 2.0)),
    );
    p.route(ModTarget::Tune, slot(wail, 3.5));
    p.filter(
        lowpass(1500.0, 0.5)
            .keytrack(0.6)
            .cutoff_mod(slot(wail, 6.0)),
    );
    p.mono(MONO, 0.0);
    p.echo(0.25, 0.4, 0.3, true);
    p
}
