//! Electronic basses: 808, dubstep and drum'n'bass, FM and pulse basses.

use super::build::*;
use super::{Entry, entry};
use crate::{ModSource, Patch};

pub(super) const LIST: &[Entry] = &[
    entry("808 Bass", 0.501, bass_808),
    entry("Neuro Bass", 0.154, neuro_bass),
    entry("Growl Bass", 0.739, growl_bass),
    entry("Wheel Wobble", 0.135, wheel_wobble),
    entry("Donk Bass", 0.837, donk_bass),
    entry("Foghorn Bass", 0.613, foghorn_bass),
    entry("TX Bass", 0.760, tx_bass),
    entry("PWM Bass", 0.426, pwm_bass),
    entry("Electro Bass", 0.274, electro_bass),
    entry("Rubber Bass", 0.533, rubber_bass),
];

fn bass_808() -> Patch {
    // The long 808 boom: a sine with a quick pitch drop, a click and soft
    // saturation; overlapping notes slide, as in trap.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 2.5, 0.0, 0.3)),
            sine(1.0, 0.0, perc(0.015))
                .fixed(1200.0)
                .dynamic(0.02, 0.08),
            off(),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1],
    );
    let drop = p.env(adsr(0.0005, 0.05, 0.0, 0.05));
    p.route(ModTarget::OpPitch(0), slot(drop, 12.0));
    p.mono(LEGATO, 0.08);
    p.distort(SOFT, 12.0, 0.2, 0.4);
    p
}

fn neuro_bass() -> Patch {
    // Drum'n'bass: a folded FM sine and a sub saw, the fold and the filter
    // chewed by a falling saw LFO. The mod wheel speeds the LFO up by two
    // octaves, aftertouch adds feedback grit.
    let mut p = patch(
        [
            sine(1.0, 1.0, gate(0.003, 0.1)).fold(0.2),
            sine(1.0, 0.15, gate(0.003, 0.1)).fb(0.5),
            saw(0.5, 0.45, gate(0.003, 0.1)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let chew = p.lfo(lfo(LfoSawDown, 4.0).rate_mod(slot(ModSource::ModWheel, 2.0)));
    p.route(ModTarget::OpFold(0), slot(chew, 0.35));
    p.filter(
        lowpass(1200.0, 0.5)
            .db24()
            .drive(0.4)
            .cutoff_mod(slot(chew, 18.0)),
    );
    p.route(ModTarget::OpFeedback(1), slot(ModSource::Aftertouch, 0.4));
    p.mono(MONO, 0.0);
    p.distort(HARD, 10.0, 0.4, 0.4);
    p
}

fn growl_bass() -> Patch {
    // A talking bass: saw and square through two band-pass operators at
    // fixed vowel formants, which an LFO moves together ("yoy"); a clean
    // sub underneath. The mod wheel makes it talk faster.
    let voice = gate(0.003, 0.1);
    let mut p = patch(
        [
            filter_op(BP, 1.0, 0.75, voice).fixed(450.0),
            filter_op(BP, 1.0, 0.7, voice).fixed(1000.0),
            saw(1.0, 1.0, voice),
            square(1.0, 0.6, voice).cents(8.0),
            sine(0.5, 0.6, voice),
            off(),
        ],
        &[(3, 1), (3, 2), (4, 1), (4, 2)],
        &[1, 2, 5],
    );
    let talk = p.lfo(
        lfo(LfoSine, 2.0)
            .phase(0.75)
            .rate_mod(slot(ModSource::ModWheel, 2.0)),
    );
    p.route(ModTarget::OpPitch(0), slot(talk, 9.0));
    p.route(ModTarget::OpPitch(1), slot(talk, 12.0));
    p.mono(MONO, 0.0);
    p.distort(SOFT, 16.0, 0.3, 0.5);
    p.output(0.500);
    p
}

fn wheel_wobble() -> Patch {
    // Dubstep wobble played with the mod wheel: an LFO on a resonant
    // low-pass, slow at rest, three octaves faster with the wheel up.
    // Aftertouch adds resonance.
    let mut p = patch(
        [
            saw(1.0, 0.8, gate(0.003, 0.1)).cents(-6.0),
            saw(1.0, 0.8, gate(0.003, 0.1)).cents(6.0),
            sine(0.5, 0.7, gate(0.003, 0.1)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let wobble = p.lfo(
        lfo(LfoSine, 1.0)
            .phase(0.75)
            .rate_mod(slot(ModSource::ModWheel, 3.0)),
    );
    p.filter(
        lowpass(250.0, 0.55)
            .db24()
            .drive(0.3)
            .cutoff_mod(slot(wobble, 36.0)),
    );
    p.route(ModTarget::Resonance(0), slot(ModSource::Aftertouch, 0.25));
    p.mono(LEGATO, 0.04);
    p.distort(SOFT, 12.0, 0.3, 0.4);
    p
}

fn donk_bass() -> Patch {
    // Hard house donk: a short, hollow FM knock with a pitch blip.
    let mut p = patch(
        [
            sine(1.0, 1.0, perc(0.25)),
            sine(1.5, 0.0, perc(0.06)).dynamic(0.12, 0.25),
            tri(2.0, 0.3, perc(0.1)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let blip = p.env(perc(0.025));
    p.route(ModTarget::Tune, slot(blip, 12.0));
    p.mono(MONO, 0.0);
    p
}

fn foghorn_bass() -> Patch {
    // A wide, distorted dubstep horn: unison saws that dip in from below,
    // a low-pass opening slowly. Notes glide.
    let env = adsr(0.05, 0.5, 0.9, 0.3);
    let mut p = patch(
        [
            saw(1.0, 1.0, env).cents(-12.0),
            saw(1.0, 1.0, env).cents(12.0),
            saw(0.5, 0.8, env),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let dip = p.env(perc(0.2));
    p.route(ModTarget::Tune, slot(dip, -2.0));
    let open = p.env(adsr(0.4, 0.5, 0.6, 0.3));
    p.filter(
        lowpass(450.0, 0.2)
            .db24()
            .keytrack(0.3)
            .cutoff_mod(slot(open, 24.0)),
    );
    p.pan(&[(1, -0.4), (2, 0.4)]);
    p.mono(LEGATO, 0.15);
    p.unison(3, 25.0, 0.8);
    p.distort(SOFT, 18.0, 0.5, 0.6);
    p
}

fn tx_bass() -> Patch {
    // The late-80s FM bass on half sines: a half-sine carrier and its
    // octave below, an FM modulator with feedback whose level follows the
    // touch.
    let body = adsr(0.001, 1.5, 0.3, 0.08);
    let mut p = patch(
        [
            half(1.0, 1.0, body),
            sine(1.0, 0.0, adsr(0.001, 0.5, 0.2, 0.08))
                .dynamic(0.05, 0.18)
                .fb(0.3),
            half(0.5, 0.5, body),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.mono(MONO, 0.0);
    p
}

fn pwm_bass() -> Patch {
    // A pulse bass whose width an envelope squeezes from thin to square;
    // the mod wheel keeps it thin. A 24 dB low-pass follows the touch.
    let mut p = patch(
        [
            square(1.0, 1.0, adsr(0.002, 0.8, 0.7, 0.08)),
            sine(1.0, 0.1, gate(0.002, 0.08)).mode(PWM),
            square(0.5, 0.4, adsr(0.002, 0.8, 0.7, 0.08)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let squeeze = p.env(perc(0.3));
    p.route(ModTarget::OpLevel(1), slot(squeeze, 0.65));
    p.route(ModTarget::OpLevel(1), slot(ModSource::ModWheel, 0.6));
    let env = p.env(adsr(0.001, 0.3, 0.2, 0.1));
    p.filter(lowpass(400.0, 0.3).db24().keytrack(0.5));
    p.touch_cutoff(env, 30.0);
    p.mono(MONO, 0.0);
    p
}

fn electro_bass() -> Patch {
    // 80s electro: square and saw, a resonant low-pass that zaps shut,
    // short glides.
    let mut p = patch(
        [
            square(1.0, 0.8, adsr(0.002, 0.4, 0.5, 0.06)),
            saw(1.0, 0.6, adsr(0.002, 0.4, 0.5, 0.06)).cents(5.0),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let zap = p.env(adsr(0.001, 0.18, 0.0, 0.05));
    p.filter(lowpass(300.0, 0.6).db24().keytrack(0.5));
    p.touch_cutoff(zap, 40.0);
    p.mono(LEGATO, 0.03);
    p
}

fn rubber_bass() -> Patch {
    // A bouncy bass: a folded triangle that springs up from a fifth below
    // on every note; harder notes fold further.
    let mut p = patch(
        [
            tri(1.0, 1.0, adsr(0.002, 0.6, 0.6, 0.1))
                .fold(0.1)
                .fold_mod(velocity(0.3)),
            sine(0.5, 0.5, adsr(0.002, 0.6, 0.7, 0.1)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    let boing = p.env(adsr(0.0005, 0.12, 0.0, 0.05));
    p.route(ModTarget::Tune, slot(boing, -7.0));
    p.filter(lowpass(1200.0, 0.2).keytrack(0.5));
    p.mono(MONO, 0.0);
    p
}
