//! Basses.

use super::build::*;
use super::{Entry, entry};
use crate::Patch;

pub(super) const LIST: &[Entry] = &[
    entry("FM Bass", 0.337, fm_bass),
    entry("Slap Bass", 0.351, slap_bass),
    entry("Electric Bass", 0.386, finger_bass),
    entry("Fretless", 0.464, fretless),
    entry("Acid Bass", 0.087, acid_bass),
    entry("Square Bass", 0.558, square_bass),
    entry("Analog Bass", 0.493, analog_bass),
    entry("Reese Bass", 0.280, reese_bass),
    entry("Sub Bass", 0.894, sub_bass),
    entry("Wobble Bass", 0.114, wobble_bass),
    entry("Pluck Bass", 0.287, pluck_bass),
    entry("Sync Bass", 0.579, sync_bass),
    entry("Distorted Bass", 0.270, distorted_bass),
];

fn fm_bass() -> Patch {
    // Classic DX bass: a 1:1 modulator with feedback for the growl, harder
    // with the touch, and a sine sub an octave down.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.2, 0.4, 0.15)),
            sine(1.0, 0.0, adsr(0.001, 0.35, 0.15, 0.15))
                .dynamic(0.12, 0.32)
                .fb(0.4),
            sine(0.5, 0.7, adsr(0.001, 1.5, 0.6, 0.15)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.struck(0.25, -3.0);
    p
}

fn slap_bass() -> Patch {
    // Thumb slap: op 3 (7:1, very short) is the click, op 2 the string's
    // bite; velocity makes both harder. An octave layer on op 4.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.0, 0.3, 0.12)),
            sine(1.0, 0.0, adsr(0.001, 0.2, 0.08, 0.1)).dynamic(0.1, 0.3),
            sine(7.0, 0.0, adsr(0.0005, 0.02, 0.0, 0.02)).dynamic(0.05, 0.3),
            sine(2.0, 0.3, adsr(0.001, 0.4, 0.1, 0.1)),
            sine(1.0, 0.08, adsr(0.001, 0.15, 0.0, 0.1)),
            off(),
        ],
        &[(2, 1), (3, 1), (5, 4)],
        &[1, 4],
    );
    p.struck(0.25, -3.0);
    p
}

fn finger_bass() -> Patch {
    // Saw and sine through a 24 dB low-pass that follows the keys and opens
    // with velocity.
    let mut p = patch(
        [
            saw(1.0, 0.7, adsr(0.002, 1.5, 0.5, 0.12)),
            sine(1.0, 0.8, adsr(0.002, 1.5, 0.6, 0.12)),
            sine(1.0, 0.0, adsr(0.002, 0.3, 0.1, 0.1)).dynamic(0.03, 0.1),
            off(),
            off(),
            off(),
        ],
        &[(3, 2)],
        &[1, 2],
    );
    p.filter(
        lowpass(700.0, 0.1)
            .db24()
            .keytrack(0.5)
            .cutoff_mod(velocity(18.0)),
    );
    p
}

fn fretless() -> Patch {
    // Soft attack, a slide into the note from below (envelope on the tuning
    // with negative depth), a gentle resonance and, on the same tuning, a
    // slow vibrato that fades in.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.02, 1.5, 0.7, 0.2)),
            sine(1.0, 0.0, adsr(0.02, 0.6, 0.3, 0.2)).dynamic(0.05, 0.12),
            tri(1.0, 0.5, adsr(0.02, 1.5, 0.7, 0.2)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let slide = p.env(adsr(0.001, 0.12, 0.0, 0.05));
    p.route(ModTarget::Tune, slot(slide, -0.7));
    p.vibrato(4.8, 0.1, 0.5);
    p.filter(lowpass(900.0, 0.35).keytrack(0.5));
    p.mono(LEGATO, 0.06);
    p
}

fn acid_bass() -> Patch {
    // One saw through a resonant, driven 24 dB low-pass; the filter envelope
    // snaps shut, velocity accents with more resonance.
    let mut p = patch(
        [
            saw(1.0, 1.0, adsr(0.001, 0.5, 0.6, 0.08)),
            off(),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1],
    );
    let env = p.env(adsr(0.001, 0.25, 0.0, 0.1));
    p.filter(
        lowpass(220.0, 0.65)
            .db24()
            .drive(0.4)
            .cutoff_mod(slot(env, 42.0))
            .resonance_mod(velocity(0.2)),
    );
    p.mono(LEGATO, 0.07);
    // Through a distortion pedal, as the 303 usually was.
    p.distort(SOFT, 12.0, 0.2, 0.5);
    p.output(0.51);
    p
}

fn square_bass() -> Patch {
    // Square with a triangle an octave below, softly filtered.
    let mut p = patch(
        [
            square(1.0, 0.8, adsr(0.002, 0.8, 0.6, 0.1)),
            tri(0.5, 0.7, adsr(0.002, 0.8, 0.7, 0.1)),
            off(),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2],
    );
    p.filter(
        lowpass(1200.0, 0.15)
            .keytrack(0.4)
            .cutoff_mod(velocity(12.0)),
    );
    p.mono(MONO, 0.0);
    p
}

fn analog_bass() -> Patch {
    // Two saws ±7 cents and a square sub through a 24 dB low-pass with a
    // classic filter envelope that opens further the harder you play.
    let mut p = patch(
        [
            saw(1.0, 0.8, adsr(0.002, 1.0, 0.7, 0.12)).cents(-7.0),
            saw(1.0, 0.8, adsr(0.002, 1.0, 0.7, 0.12)).cents(7.0),
            square(0.5, 0.6, adsr(0.002, 1.0, 0.7, 0.12)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let env = p.env(adsr(0.002, 0.4, 0.3, 0.15));
    p.filter(lowpass(350.0, 0.3).db24().keytrack(0.5));
    p.touch_cutoff(env, 30.0);
    p.mono(MONO, 0.0);
    p
}

fn reese_bass() -> Patch {
    // Two saws ±12 cents beating against each other, a sine sub, and a
    // slowly wandering, slightly driven low-pass.
    let mut p = patch(
        [
            saw(1.0, 0.9, gate(0.005, 0.15)).cents(-12.0),
            saw(1.0, 0.9, gate(0.005, 0.15)).cents(12.0),
            sine(0.5, 0.5, gate(0.005, 0.15)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let wander = p.lfo(lfo(LfoTriangle, 0.25).free());
    p.filter(
        lowpass(700.0, 0.3)
            .db24()
            .drive(0.3)
            .cutoff_mod(slot(wander, 8.0)),
    );
    // Slightly wide, the sub stays centred.
    p.pan(&[(1, -0.25), (2, 0.25)]);
    p.mono(MONO, 0.0);
    p.unison(2, 12.0, 0.6);
    p
}

fn sub_bass() -> Patch {
    // Almost pure sine; velocity adds a hint of harmonics for definition.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.003, 1.0, 0.85, 0.15)),
            sine(1.0, 0.0, adsr(0.003, 0.2, 0.0, 0.1)).dynamic(0.0, 0.08),
            sine(2.0, 0.12, adsr(0.003, 1.0, 0.8, 0.15)),
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

fn wobble_bass() -> Patch {
    // Saw, square and sub through a driven, resonant 24 dB low-pass; a
    // key-synced LFO wobbles the cutoff from every note's start.
    let mut p = patch(
        [
            saw(1.0, 0.8, gate(0.003, 0.1)),
            square(1.0, 0.6, gate(0.003, 0.1)).cents(5.0),
            sine(0.5, 0.6, gate(0.003, 0.1)),
            off(),
            off(),
            off(),
        ],
        &[],
        &[1, 2, 3],
    );
    let wobble = p.lfo(lfo(LfoSine, 4.0));
    p.filter(
        lowpass(700.0, 0.55)
            .db24()
            .drive(0.5)
            .cutoff_mod(slot(wobble, 24.0)),
    );
    p.mono(MONO, 0.0);
    p
}

fn pluck_bass() -> Patch {
    // Short FM pluck: a 2:1 modulator snapping shut, and a plucked sub.
    patch(
        [
            sine(1.0, 1.0, adsr(0.001, 0.6, 0.0, 0.1)),
            sine(2.0, 0.0, adsr(0.001, 0.08, 0.0, 0.05)).dynamic(0.15, 0.4),
            sine(0.5, 0.8, adsr(0.001, 0.8, 0.0, 0.1)),
            sine(1.0, 0.0, adsr(0.001, 0.1, 0.0, 0.05)).dynamic(0.05, 0.15),
            off(),
            off(),
        ],
        &[(2, 1), (4, 3)],
        &[1, 3],
    )
}

fn sync_bass() -> Patch {
    // HTE sync: op 2 restarts the saw on op 1 every cycle; an envelope
    // sweeps the sync amount (op 2's level) for the tearing sweep.
    let mut p = patch(
        [
            saw(1.0, 1.0, adsr(0.002, 1.0, 0.7, 0.12)),
            sine(1.0, 0.0, gate(0.002, 0.12)).mode(SYNC),
            sine(0.5, 0.5, adsr(0.002, 1.0, 0.8, 0.12)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    let sweep = p.env(adsr(0.001, 0.5, 0.15, 0.2));
    p.route(ModTarget::OpLevel(1), slot(sweep, 0.6));
    p.filter(lowpass(2500.0, 0.2).keytrack(0.5));
    p.mono(MONO, 0.0);
    p
}

fn distorted_bass() -> Patch {
    // FM bass into some filter drive (velocity drives harder) and a 24 dB
    // low-pass, then hard master distortion.
    let mut p = patch(
        [
            sine(1.0, 1.0, adsr(0.001, 1.0, 0.6, 0.12)),
            sine(1.0, 0.0, adsr(0.001, 0.3, 0.2, 0.12))
                .dynamic(0.1, 0.25)
                .fb(0.3),
            sine(0.5, 0.6, adsr(0.001, 1.0, 0.7, 0.12)),
            off(),
            off(),
            off(),
        ],
        &[(2, 1)],
        &[1, 3],
    );
    p.filter(
        lowpass(1600.0, 0.25)
            .db24()
            .drive(0.4)
            .drive_mod(velocity(0.15)),
    );
    p.mono(MONO, 0.0);
    p.distort(HARD, 18.0, 0.5, 0.6);
    p.output(0.511);
    p
}
