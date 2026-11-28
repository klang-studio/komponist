use super::*;
use crate::filter::{FilterKind, FilterParams};
use crate::midi::CC_SUSTAIN;
use crate::modulation::{MAX_PITCH_DEPTH, ModSlot, ModTarget, Usage};
use crate::operator::{MAX_SYNC_OCTAVES, ModulationMode, OperatorParams, PitchMode};
use crate::voicing::{MAX_PORTAMENTO, MAX_UNISON, VoiceMode};
use crate::waveform::Waveform;
use crate::{EnvelopeParams, KeyScaling, MAX_DETUNE, Synth, pitch};

const SR: f32 = 48_000.0;

// Roles in the default patch.
const CARRIER: usize = 0;
const MODULATOR: usize = 1;

/// A synth with all voices on one engine.
fn synth(patch: Patch) -> Box<Synth<1>> {
    Synth::boxed(SR, patch)
}

fn render_seconds(engine: &mut Synth<1>, seconds: f32) -> Vec<f32> {
    let n = (seconds * SR) as usize;
    let mut l = vec![0.0; n];
    let mut r = vec![0.0; n];
    engine.render(&mut l, &mut r);
    assert_eq!(l, r);
    l
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |a, &x| a.max(x.abs()))
}

/// Op 2 modulates op 1 (heard) and op 3 (heard) in `mode`, at `level`
/// with `depths` towards op 1 and op 3. Returns the output of C4.
fn two_targets(mode: ModulationMode, level: f32, depths: (f32, f32), connected: bool) -> Vec<f32> {
    let mut patch = Patch::default();
    patch.operators[0] = OperatorParams {
        output: true,
        ..Default::default()
    };
    patch.operators[2] = OperatorParams {
        output: true,
        ratio: 2.0,
        ..Default::default()
    };
    patch.operators[1] = OperatorParams {
        mode,
        ratio: 3.0,
        level,
        modulates: core::array::from_fn(|t| connected && (t == 0 || t == 2)),
        depths: core::array::from_fn(|t| match t {
            0 => depths.0,
            2 => depths.1,
            _ => 1.0,
        }),
        envelope: EnvelopeParams {
            sustain: 1.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut engine = synth(patch);
    engine.note_on(60, 127);
    render_seconds(&mut engine, 0.1)
}

#[test]
fn depth_scales_the_modulation_per_target() {
    for mode in [
        ModulationMode::Phase,
        ModulationMode::Frequency,
        ModulationMode::PulseWidth,
    ] {
        // Half the depth on both targets = half the level.
        let half_level = two_targets(mode, 0.25, (1.0, 1.0), true);
        let half_depth = two_targets(mode, 0.5, (0.5, 0.5), true);
        let diff = half_level
            .iter()
            .zip(&half_depth)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0, f32::max);
        assert!(diff < 1e-5, "{mode:?}: {diff}");
        // Depth 0 on both = not connected.
        assert_eq!(
            two_targets(mode, 0.5, (0.0, 0.0), true),
            two_targets(mode, 0.5, (1.0, 1.0), false),
            "{mode:?}"
        );
        // The depths act per target.
        assert_ne!(
            two_targets(mode, 0.5, (1.0, 0.0), true),
            two_targets(mode, 0.5, (0.0, 1.0), true),
            "{mode:?}"
        );
    }
}

/// Peak of op 1 alone (sine, heard) playing `note` for `seconds`.
fn scaled_peak(
    level: f32,
    scaling: KeyScaling,
    note: u8,
    envelope: EnvelopeParams,
    seconds: f32,
) -> Vec<f32> {
    let mut patch = Patch::default();
    patch.operators[1] = OperatorParams {
        level: 0.0,
        ..Default::default()
    };
    patch.operators[0] = OperatorParams {
        output: true,
        level,
        key_scaling: scaling,
        envelope,
        ..Default::default()
    };
    let mut engine = synth(patch);
    engine.note_on(note, 127);
    render_seconds(&mut engine, seconds)
}

#[test]
fn key_scaling_formulas() {
    let k = KeyScaling {
        break_point: 60.0,
        left: 6.0,
        right: -6.0,
        rate: 1.0,
    };
    assert!((k.gain(72.0) - 0.501).abs() < 1e-3);
    assert!((k.gain(48.0) - 1.995).abs() < 1e-3);
    assert_eq!(k.gain(60.0), 1.0);
    assert!((k.speed(72.0) - 2.0).abs() < 1e-5);
    assert!((k.speed(48.0) - 0.5).abs() < 1e-5);
    assert_eq!(KeyScaling::NONE.gain(100.0), 1.0);
    assert_eq!(KeyScaling::NONE.speed(100.0), 1.0);
    let wild = KeyScaling {
        break_point: f32::NAN,
        left: 99.0,
        right: -99.0,
        rate: 2.0,
    }
    .sanitized();
    assert_eq!(
        wild,
        KeyScaling {
            break_point: 60.0,
            left: 24.0,
            right: -24.0,
            rate: 1.0
        }
    );
}

#[test]
fn level_scaling_follows_the_note_and_never_exceeds_level_one() {
    let held = EnvelopeParams {
        sustain: 1.0,
        ..Default::default()
    };
    let scaling = KeyScaling {
        right: -6.0,
        left: 12.0,
        ..KeyScaling::NONE
    };
    let at = |level, note| peak(&scaled_peak(level, scaling, note, held, 0.1)[2400..]);
    // One octave up: -6 dB.
    assert!((at(0.5, 72) / at(0.5, 60) - 0.501).abs() < 0.01);
    // One octave down: +12 dB, but a level of 0.5 only reaches 1.
    assert!((at(0.5, 48) / at(0.5, 60) - 2.0).abs() < 0.02);
    assert!((at(0.2, 48) / at(0.2, 60) - 3.98).abs() < 0.05);
}

#[test]
fn rate_scaling_shortens_high_notes() {
    let decaying = EnvelopeParams {
        sustain: 0.0,
        decay: 1.0,
        decay_curve: 0.0,
        ..Default::default()
    };
    let scaling = KeyScaling {
        rate: 1.0,
        ..KeyScaling::NONE
    };
    // Time until the (linear) decay has fallen below half.
    let half_time = |note| {
        let out = scaled_peak(1.0, scaling, note, decaying, 2.0);
        let windows: Vec<f32> = out.chunks(480).map(peak).collect();
        let top = windows.iter().cloned().fold(0.0, f32::max);
        windows.iter().position(|&p| p < 0.5 * top).unwrap() as f32 * 0.01
    };
    let (c4, c5, c3) = (half_time(60), half_time(72), half_time(48));
    assert!((c4 / c5 - 2.0).abs() < 0.15, "C4 {c4} s, C5 {c5} s");
    assert!((c3 / c4 - 2.0).abs() < 0.15, "C3 {c3} s, C4 {c4} s");
}

#[test]
fn envelope_positions_follow_the_newest_voice() {
    let mut patch = Patch::default();
    patch.operators[0].envelope = EnvelopeParams {
        attack: 0.1,
        decay: 0.1,
        sustain: 0.5,
        release: 0.1,
        ..Default::default()
    };
    let used = patch
        .envelopes
        .push(EnvelopeParams {
            attack: 0.5,
            ..Default::default()
        })
        .unwrap();
    patch.envelopes.push(EnvelopeParams::default());
    patch.modulate(
        ModTarget::OpPitch(0),
        ModSlot {
            source: crate::ModSource::Envelope(used),
            depth: 1.0,
        },
    );
    let mut engine = synth(patch);
    assert_eq!(engine.envelope_positions(), None);

    engine.note_on(60, 100);
    render_seconds(&mut engine, 0.05);
    let p = engine.envelope_positions().unwrap();
    assert!(
        (1.4..1.6).contains(&p[0]),
        "op 1 halfway through the attack: {}",
        p[0]
    );
    assert!(
        (1.05..1.15).contains(&p[OPERATORS]),
        "envelope 1 a tenth into its attack: {}",
        p[OPERATORS]
    );
    assert_eq!(p[OPERATORS + 1], -1.0, "unused envelope");
    assert_eq!(p[OPERATORS + 2], -1.0, "missing envelope");

    render_seconds(&mut engine, 0.3);
    assert_eq!(engine.envelope_positions().unwrap()[0], 4.0, "sustain");

    // A second note: its envelopes are shown.
    engine.note_on(64, 100);
    render_seconds(&mut engine, 0.02);
    assert!((1.1..1.3).contains(&engine.envelope_positions().unwrap()[0]));

    engine.note_off(60);
    engine.note_off(64);
    render_seconds(&mut engine, 0.05);
    let release = engine.envelope_positions().unwrap()[0];
    assert!(
        (5.3..5.7).contains(&release),
        "halfway through the release: {release}"
    );
    render_seconds(&mut engine, 0.2);
    assert_eq!(engine.envelope_positions(), None);
}

#[test]
fn lfo_positions_follow_the_newest_voice() {
    use crate::{LfoParams, LfoWaveform};
    let mut patch = Patch::default();
    patch.operators[0].envelope.release = 0.05;
    let lfo = LfoParams {
        waveform: LfoWaveform::SawUp,
        rate: 2.0,
        fade_in: 1.0,
        amount: 0.5,
        ..Default::default()
    };
    let used = patch.lfos.push(lfo).unwrap();
    patch.lfos.push(LfoParams::default());
    patch.modulate(
        ModTarget::OpPitch(0),
        ModSlot {
            source: crate::ModSource::Lfo(used),
            depth: 1.0,
        },
    );
    let mut engine = synth(patch);
    assert_eq!(engine.lfo_positions(), None);

    engine.note_on(60, 100);
    render_seconds(&mut engine, 0.1);
    let p = engine.lfo_positions().unwrap();
    assert!(
        (0.18..0.22).contains(&p[0]),
        "a fifth of a period: {}",
        p[0]
    );
    assert!(
        (0.045..0.055).contains(&p[1]),
        "a tenth into the fade-in at half amount: {}",
        p[1]
    );
    assert!(
        (p[2] - (2.0 * p[0] - 1.0) * p[1]).abs() < 1e-6,
        "output = shape × level: {p:?}"
    );
    assert_eq!(p[3], -1.0, "unused LFO");
    assert_eq!(p[6], -1.0, "missing LFO");

    engine.note_off(60);
    render_seconds(&mut engine, 0.5);
    assert_eq!(engine.lfo_positions(), None);
}

#[test]
fn portamento_glides_in_the_set_time() {
    let mut patch = Patch {
        voice_mode: VoiceMode::Legato,
        portamento: 0.1,
        ..Patch::default()
    };
    patch.operators[0].envelope.sustain = 1.0;
    let mut engine = synth(patch);
    engine.note_on(60, 100);
    render_seconds(&mut engine, 0.02);
    engine.note_on(72, 100);
    render_seconds(&mut engine, 0.05);
    assert!(
        (engine.engine(0).pitch(0) - 66.0).abs() < 0.2,
        "halfway: {}",
        engine.engine(0).pitch(0)
    );
    render_seconds(&mut engine, 0.06);
    assert_eq!(engine.engine(0).pitch(0), 72.0);
    // Legato: still one voice, never restarted.
    assert_eq!(engine.active_voices(), 1);
    // Back to the held key, gliding down.
    engine.note_off(72);
    render_seconds(&mut engine, 0.05);
    assert!(
        (engine.engine(0).pitch(0) - 66.0).abs() < 0.2,
        "back halfway: {}",
        engine.engine(0).pitch(0)
    );
}

#[test]
fn mono_plays_one_voice_and_poly_without_portamento_jumps() {
    let mut engine = synth(Patch {
        voice_mode: VoiceMode::Mono,
        ..Patch::default()
    });
    for note in [60, 64, 67] {
        engine.note_on(note, 100);
    }
    render_seconds(&mut engine, 0.01);
    assert_eq!(engine.active_voices(), 1);
    assert_eq!(engine.engine(0).pitch(0), 67.0);

    let mut engine = synth(Patch::default());
    engine.note_on(60, 100);
    engine.note_on(67, 100);
    render_seconds(&mut engine, 0.01);
    assert_eq!(engine.active_voices(), 2);
    assert_eq!(engine.engine(0).pitch(1), 67.0);
}

#[test]
fn unison_stacks_voices_per_key() {
    let mut engine = synth(Patch {
        unison: 3,
        ..Patch::default()
    });
    engine.note_on(60, 100);
    render_stereo(&mut engine, 0.01);
    assert_eq!(engine.active_voices(), 3);
    for note in [62, 64, 65, 67, 69] {
        engine.note_on(note, 100);
    }
    render_stereo(&mut engine, 0.01);
    // 16 voices: five keys of three, the oldest one stolen in part.
    assert_eq!(engine.active_voices(), 16);
    engine.note_off(69);
    render_stereo(&mut engine, 0.01);
    engine.note_off(67);
    render_stereo(&mut engine, 0.5);
    assert!(engine.active_voices() <= 10);
}

#[test]
fn unison_keeps_the_level_and_spreads_only_with_width() {
    let held = |unison, detune, spread| {
        let mut patch = Patch {
            unison,
            detune,
            unison_spread: spread,
            ..Patch::default()
        };
        patch.operators[0].envelope.sustain = 1.0;
        patch.operators[1].envelope.sustain = 1.0;
        let mut engine = synth(patch);
        engine.note_on(60, 100);
        let (l, r) = render_stereo(&mut engine, 1.0);
        (l[4800..].to_vec(), r[4800..].to_vec())
    };
    let (single, _) = held(1, 20.0, 1.0);
    let (wide_l, wide_r) = held(4, 20.0, 1.0);
    let (narrow_l, narrow_r) = held(4, 20.0, 0.0);
    let ratio = rms(&narrow_l) / rms(&single);
    assert!((0.6..1.5).contains(&ratio), "unison level {ratio}");
    assert_eq!(narrow_l, narrow_r, "no width: mono");
    assert!(
        wide_l
            .iter()
            .zip(&wide_r)
            .any(|(a, b)| (a - b).abs() > 1e-3),
        "width: stereo"
    );
    // Detune makes the voices drift apart: the sum beats (its level moves).
    let windows: Vec<f32> = narrow_l.chunks(2400).map(rms).collect();
    let (lo, hi) = windows
        .iter()
        .fold((f32::MAX, 0.0f32), |(lo, hi), &w| (lo.min(w), hi.max(w)));
    assert!(hi / lo > 1.1, "no beating: {lo}..{hi}");
}

#[test]
fn noise_is_broadband_pitch_independent_and_differs_per_voice() {
    let mut patch = Patch::default();
    patch.operators[0] = OperatorParams {
        waveform: Waveform::Noise,
        output: true,
        envelope: EnvelopeParams {
            sustain: 1.0,
            ..Default::default()
        },
        ..Default::default()
    };
    patch.operators[1].level = 0.0;
    let play = |note| {
        let mut engine = synth(patch);
        engine.note_on(note, 127);
        render_seconds(&mut engine, 0.5)[2400..].to_vec()
    };
    let (c4, c6) = (play(60), play(84));
    // White: the sample-to-sample change is as large as the signal
    // (a 262 Hz sine would change by 3 % of it).
    let diff: Vec<f32> = c4.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        rms(&diff) / rms(&c4) > 1.0,
        "not broadband: {}",
        rms(&diff) / rms(&c4)
    );
    // The pitch does not change the level.
    assert!((rms(&c6) / rms(&c4) - 1.0).abs() < 0.1);
    // Two voices at once rustle independently: little correlation.
    let mut engine = synth(patch);
    engine.note_on(60, 127);
    engine.note_on(64, 127);
    let both = render_seconds(&mut engine, 0.5)[2400..].to_vec();
    let ratio = rms(&both) / rms(&c4);
    assert!(
        (1.25..1.6).contains(&ratio),
        "two voices {ratio}, expected about √2"
    );
}

/// Amplitude of the `hz` component of `x` (Goertzel), 1.0 for a full-scale sine.
fn tone(x: &[f32], hz: f32) -> f32 {
    let w = core::f32::consts::TAU * hz / SR;
    let (mut s1, mut s2) = (0.0f64, 0.0f64);
    let c = 2.0 * (w as f64).cos();
    for &v in x {
        let s0 = v as f64 + c * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let power = s1 * s1 + s2 * s2 - c * s1 * s2;
    (2.0 * power.sqrt() / x.len() as f64) as f32
}

/// Op 1 (heard, 1:1) ring-modulated by op 2 (3:1) at `level`.
fn ring(level: f32, mode: ModulationMode) -> Vec<f32> {
    let held = EnvelopeParams {
        sustain: 1.0,
        ..Default::default()
    };
    let mut patch = Patch::default();
    patch.operators[0] = OperatorParams {
        output: true,
        envelope: held,
        ..Default::default()
    };
    patch.operators[1] = OperatorParams {
        mode,
        ratio: 3.0,
        level,
        modulates: core::array::from_fn(|t| t == 0),
        envelope: held,
        ..Default::default()
    };
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    // Whole periods of 440 Hz.
    render_seconds(&mut engine, 0.6)[4800..4800 + 24_000].to_vec()
}

#[test]
fn ring_modulation_leaves_sum_and_difference() {
    let f = 440.0;
    let pure = ring(1.0, ModulationMode::Ring);
    let full = tone(&pure, 2.0 * f).max(tone(&pure, 4.0 * f));
    assert!(full > 0.01, "no sidebands: {full}");
    for gone in [f, 3.0 * f] {
        assert!(
            tone(&pure, gone) < full * 0.01,
            "{gone} Hz left: {} vs {full}",
            tone(&pure, gone)
        );
    }
    // Halfway: amplitude modulation, the carrier at half (as loud as a
    // sideband of the pure product), the sidebands at a quarter.
    let half = ring(0.5, ModulationMode::Ring);
    assert!(
        (tone(&half, f) / full - 1.0).abs() < 0.1,
        "carrier {}",
        tone(&half, f) / full
    );
    assert!(
        (tone(&half, 2.0 * f) / full - 0.5).abs() < 0.1,
        "sideband {}",
        tone(&half, 2.0 * f) / full
    );
    // Level 0: exactly the plain carrier.
    assert_eq!(
        ring(0.0, ModulationMode::Ring),
        ring(0.0, ModulationMode::Phase)
    );
}

#[test]
fn portamento_is_sanitized() {
    let patch = Patch {
        portamento: 9.0,
        ..Patch::default()
    }
    .sanitized();
    assert_eq!(patch.portamento, MAX_PORTAMENTO);
    assert_eq!(
        Patch {
            portamento: f32::NAN,
            ..Patch::default()
        }
        .sanitized()
        .portamento,
        0.0
    );
    let unison = Patch {
        unison: 0,
        detune: 500.0,
        unison_spread: -1.0,
        ..Patch::default()
    }
    .sanitized();
    assert_eq!(
        (unison.unison, unison.detune, unison.unison_spread),
        (1, MAX_DETUNE, 0.0)
    );
    assert_eq!(
        Patch {
            unison: 99,
            ..Patch::default()
        }
        .sanitized()
        .unison,
        MAX_UNISON as u8
    );
}

#[test]
fn depths_are_sanitized() {
    let op = OperatorParams {
        depths: [f32::NAN, -1.0, 2.0, 0.5, 1.0, 0.0],
        ..Default::default()
    };
    assert_eq!(op.sanitized().depths, [1.0, 0.0, 1.0, 0.5, 1.0, 0.0]);
}

#[test]
fn silent_without_notes() {
    let mut e = synth(Patch::default());
    assert_eq!(peak(&render_seconds(&mut e, 0.1)), 0.0);
}

#[test]
fn note_sounds_and_decays_to_silence() {
    let mut e = synth(Patch::default());
    e.note_on(69, 127);
    let held = render_seconds(&mut e, 0.5);
    assert!(peak(&held) > 0.1);
    assert_eq!(e.active_voices(), 1);

    e.note_off(69);
    render_seconds(&mut e, 2.0);
    assert_eq!(e.active_voices(), 0);
    assert_eq!(peak(&render_seconds(&mut e, 0.1)), 0.0);
}

#[test]
fn unmodulated_carrier_has_expected_pitch() {
    let mut patch = Patch::default();
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].envelope = EnvelopeParams {
        attack: 0.001,
        decay: 1.0,
        sustain: 1.0,
        release: 0.1,
        ..Default::default()
    };
    let mut e = synth(patch);
    e.note_on(69, 127);
    let buf = render_seconds(&mut e, 1.0);
    // Count rising zero crossings after the attack.
    let crossings = buf[480..]
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let expected = 440.0 * (buf.len() - 480) as f32 / SR;
    assert!(
        (crossings as f32 - expected).abs() <= 1.0,
        "{crossings} vs {expected}"
    );
}

#[test]
fn full_polyphony_stays_finite_and_bounded() {
    let mut patch = Patch {
        volume: 1.0,
        ..Default::default()
    };
    patch.operators[MODULATOR].ratio = 3.5;
    patch.operators[MODULATOR].level = 1.0;
    let mut e = synth(patch);
    for n in 0..(MAX_VOICES as u8 + 4) {
        e.note_on(40 + n * 3, 127);
    }
    assert_eq!(e.active_voices(), MAX_VOICES);
    let buf = render_seconds(&mut e, 0.5);
    assert!(buf.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
}

#[test]
fn invalid_patch_values_are_sanitized() {
    let mut e = synth(Patch::default());
    let mut patch = Patch {
        volume: -0.5,
        ..Default::default()
    };
    patch.operators[MODULATOR] = OperatorParams {
        ratio: f32::NAN,
        level: 1e9,
        envelope: EnvelopeParams {
            attack: -1.0,
            hold: f32::NAN,
            decay: f32::INFINITY,
            sustain: 2.0,
            ..Default::default()
        },
        ..Default::default()
    };
    e.set_patch(patch);
    let p = e.patch().operators[MODULATOR];
    assert_eq!(p.ratio, OperatorParams::default().ratio);
    assert_eq!(p.level, 1.0);
    assert_eq!(p.envelope.attack, 0.0);
    assert_eq!(p.envelope.hold, EnvelopeParams::default().hold);
    assert_eq!(p.envelope.decay, EnvelopeParams::default().decay);
    assert_eq!(p.envelope.sustain, 1.0);
    assert_eq!(e.patch().volume, 0.0);
    e.note_on(60, 100);
    assert!(render_seconds(&mut e, 0.1).iter().all(|x| x.is_finite()));
}

#[test]
fn unrouted_operators_do_not_change_the_sound() {
    let mut busy = Patch::default();
    for op in &mut busy.operators[2..] {
        op.level = 1.0;
        op.feedback = 1.0;
    }
    let mut a = synth(Patch::default());
    let mut b = synth(busy);
    a.note_on(60, 100);
    b.note_on(60, 100);
    assert_eq!(render_seconds(&mut a, 0.2), render_seconds(&mut b, 0.2));
}

#[test]
fn loudness_is_normalized_by_the_number_of_heard_operators() {
    let mut single = Patch::default();
    single.operators[MODULATOR].level = 0.0;
    // Same as `single`, plus an identical second carrier.
    let mut double = single;
    double.operators[2] = double.operators[CARRIER];
    let mut a = synth(single);
    let mut b = synth(double);
    a.note_on(60, 100);
    b.note_on(60, 100);
    assert_eq!(render_seconds(&mut a, 0.2), render_seconds(&mut b, 0.2));
}

#[test]
fn modulation_works_in_both_directions() {
    // Default: operator 2 modulates operator 1. Mirrored: operator 1
    // modulates operator 2, which is heard; this direction has one sample
    // of modulation delay, so the result is close but not identical.
    let default = Patch::default();
    let mut mirrored = default;
    mirrored.operators.swap(0, 1);
    mirrored.operators[0].modulates = core::array::from_fn(|i| i == 1);
    mirrored.operators[1].modulates = [false; OPERATORS];
    let mut a = synth(default);
    let mut b = synth(mirrored);
    a.note_on(48, 100);
    b.note_on(48, 100);
    let (x, y) = (render_seconds(&mut a, 0.3), render_seconds(&mut b, 0.3));
    let max_diff = x
        .iter()
        .zip(&y)
        .fold(0.0f32, |m, (a, b)| m.max((a - b).abs()));
    assert!(
        max_diff > 0.0 && max_diff < 0.05 * peak(&x),
        "max diff {max_diff}"
    );
}

#[test]
fn feedback_changes_the_waveform_and_stays_finite() {
    let mut plain = Patch::default();
    plain.operators[MODULATOR].level = 0.0;
    let mut fed = plain;
    fed.operators[CARRIER].feedback = 1.0;
    let mut a = synth(plain);
    let mut b = synth(fed);
    a.note_on(60, 127);
    b.note_on(60, 127);
    let (x, y) = (render_seconds(&mut a, 0.2), render_seconds(&mut b, 0.2));
    assert!(y.iter().all(|s| s.is_finite()));
    assert_ne!(x, y);
}

#[test]
fn patch_without_heard_operators_is_silent() {
    let mut patch = Patch::default();
    patch.operators[CARRIER].output = false;
    let mut e = synth(patch);
    e.note_on(60, 100);
    assert_eq!(peak(&render_seconds(&mut e, 0.1)), 0.0);
    assert_eq!(e.active_voices(), 0);
}

#[test]
fn deterministic() {
    let run = || {
        let mut e = synth(Patch::default());
        e.note_on(60, 100);
        e.note_on(64, 90);
        let mut out = render_seconds(&mut e, 0.2);
        e.note_off(60);
        out.extend(render_seconds(&mut e, 0.2));
        out
    };
    assert_eq!(run(), run());
}

/// A plain sine carrier at full sustain with one LFO (index 0), not yet used.
fn lfo_patch(waveform: crate::LfoWaveform, rate: f32) -> Patch {
    let mut patch = Patch::default();
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].envelope.sustain = 1.0;
    patch.lfos.push(crate::LfoParams {
        waveform,
        rate,
        ..Default::default()
    });
    patch
}

fn lfo(depth: f32) -> ModSlot {
    ModSlot {
        source: crate::ModSource::Lfo(0),
        depth,
    }
}

/// Ignores steps below 1e-6, such as the decimation filter's pre-ringing
/// (around 1e-9) before a note's onset.
fn rising_zero_crossings(buf: &[f32]) -> usize {
    buf.windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0 && w[1] - w[0] > 1e-6)
        .count()
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
}

/// Ratio of zero crossings in the first and second half second of a held
/// A4, for a square LFO at 1 Hz (high, then low).
fn high_low_pitch_ratio(patch: Patch) -> f32 {
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    let out = render_seconds(&mut engine, 1.0);
    let half = out.len() / 2;
    rising_zero_crossings(&out[..half]) as f32 / rising_zero_crossings(&out[half..]) as f32
}

#[test]
fn unused_lfos_do_not_change_the_sound() {
    let mut changed = lfo_patch(crate::LfoWaveform::SampleHold, 13.0);
    changed.operators[MODULATOR].level = Patch::default().operators[MODULATOR].level;
    changed.operators[CARRIER].envelope = Patch::default().operators[CARRIER].envelope;
    // A slot with zero depth counts as unused.
    changed.modulate(ModTarget::OpPitch(CARRIER), lfo(0.0));
    assert!(changed.usage().is_empty());
    let mut a = synth(Patch::default());
    let mut b = synth(changed);
    a.note_on(60, 100);
    b.note_on(60, 100);
    assert_eq!(render_seconds(&mut a, 0.5), render_seconds(&mut b, 0.5));
}

#[test]
fn operator_pitch_slot_bends_in_semitones() {
    let mut patch = lfo_patch(crate::LfoWaveform::Square, 1.0);
    patch.modulate(ModTarget::OpPitch(CARRIER), lfo(1.0));
    let ratio = high_low_pitch_ratio(patch);
    let expected = libm::exp2f(2.0 / 12.0);
    assert!(
        (ratio - expected).abs() < 0.01,
        "ratio {ratio}, expected {expected}"
    );
}

#[test]
fn tune_slot_gives_vibrato() {
    let mut patch = lfo_patch(crate::LfoWaveform::Square, 1.0);
    patch.modulate(ModTarget::Tune, lfo(1.0));
    let ratio = high_low_pitch_ratio(patch);
    assert!(
        (ratio - libm::exp2f(2.0 / 12.0)).abs() < 0.01,
        "ratio {ratio}"
    );
}

#[test]
fn volume_slot_moves_around_the_base_volume() {
    let mut patch = lfo_patch(crate::LfoWaveform::Square, 1.0);
    patch.volume = 0.5;
    patch.modulate(ModTarget::Volume, lfo(0.25));
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    let out = render_seconds(&mut engine, 1.0);
    let half = out.len() / 2;
    // 0.75 in the upper half, 0.25 in the lower one (skipping the attack).
    let ratio = rms(&out[half + 1000..]) / rms(&out[1000..half]);
    assert!((ratio - 1.0 / 3.0).abs() < 0.01, "ratio {ratio}");
}

#[test]
fn smooth_random_moves_the_volume_without_steps_or_allocations() {
    use assert_no_alloc::assert_no_alloc;
    let mut patch = lfo_patch(crate::LfoWaveform::SmoothRandom, 2.0);
    patch.volume = 0.5;
    patch.modulate(ModTarget::Volume, lfo(0.25));
    let mut engine = synth(patch);
    assert_no_alloc(|| engine.note_on(69, 127));
    // Two seconds in blocks of 20 ms (almost nine periods of A4 each).
    let (mut l, mut r) = ([0.0; 960], [0.0; 960]);
    let mut levels = Vec::with_capacity(100);
    for _ in 0..100 {
        assert_no_alloc(|| engine.render(&mut l, &mut r));
        levels.push(rms(&l));
    }
    // Past the attack: the volume wanders between 0.25 and 0.75 …
    let levels = &levels[5..];
    let (low, high) = levels
        .iter()
        .fold((f32::MAX, 0.0f32), |(lo, hi), &x| (lo.min(x), hi.max(x)));
    assert!(high / low > 1.3, "barely moves: {low}..{high}");
    // … without steps: at most 1.5 · 2 · 2 Hz · 20 ms · 0.25 = 0.03 per block,
    // 12 % of the lowest volume (a square would jump by 200 %).
    for pair in levels.windows(2) {
        assert!(
            (pair[1] / pair[0] - 1.0).abs() < 0.15,
            "step {} → {}",
            pair[0],
            pair[1]
        );
    }
}

#[test]
fn level_slot_modulates_modulation_depth() {
    let mut patch = lfo_patch(crate::LfoWaveform::Square, 1.0);
    patch.operators[MODULATOR].envelope.sustain = 1.0;
    patch.modulate(ModTarget::OpLevel(MODULATOR), lfo(0.5));
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    let out = render_seconds(&mut engine, 1.0);
    let half = out.len() / 2;
    // Clamped at 0 in the lower half: a pure sine again.
    assert!(rising_zero_crossings(&out[..half]) > rising_zero_crossings(&out[half..]));
}

fn crossings_per_second(patch: Patch, note: u8) -> usize {
    let mut engine = synth(patch);
    engine.note_on(note, 127);
    render_seconds(&mut engine, 0.1);
    rising_zero_crossings(&render_seconds(&mut engine, 1.0))
}

#[test]
fn pitch_modes() {
    let mut patch = Patch::default();
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].envelope.sustain = 1.0;
    patch.operators[CARRIER].pitch_mode = PitchMode::Semitone;
    patch.operators[CARRIER].semitones = 12.0;
    assert!(crossings_per_second(patch, 57).abs_diff(440) <= 1);

    // Fixed frequency ignores the note and the master tuning.
    patch.operators[CARRIER].pitch_mode = PitchMode::Frequency;
    patch.operators[CARRIER].frequency = 1000.0;
    patch.tune = 7.0;
    assert!(crossings_per_second(patch, 40).abs_diff(1000) <= 1);
    assert!(crossings_per_second(patch, 90).abs_diff(1000) <= 1);

    // Ratio mode follows the tuning: A4 + 12 semitones.
    patch.operators[CARRIER].pitch_mode = PitchMode::Ratio;
    patch.tune = 12.0;
    assert!(crossings_per_second(patch, 69).abs_diff(880) <= 1);
}

#[test]
fn pitch_envelope_drops_from_an_octave_up() {
    let mut patch = Patch::default();
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].envelope.sustain = 1.0;
    patch.envelopes.push(EnvelopeParams {
        attack: 0.001,
        decay: 0.5,
        sustain: 0.0,
        ..Default::default()
    });
    patch.modulate(
        ModTarget::OpPitch(CARRIER),
        ModSlot {
            source: crate::ModSource::Envelope(0),
            depth: 12.0,
        },
    );
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    let start = render_seconds(&mut engine, 0.02);
    render_seconds(&mut engine, 3.0);
    let settled = render_seconds(&mut engine, 0.1);
    // Near 880 Hz right after the attack, 440 Hz once decayed.
    let start_hz = rising_zero_crossings(&start) as f32 / 0.02;
    let settled_hz = rising_zero_crossings(&settled) as f32 / 0.1;
    assert!(start_hz > 700.0, "{start_hz}");
    assert!((settled_hz - 440.0).abs() <= 10.0, "{settled_hz}");
}

#[test]
fn envelope_on_an_unused_lfo_is_not_computed() {
    let mut patch = lfo_patch(crate::LfoWaveform::Sine, 1.0);
    patch.envelopes.push(EnvelopeParams::default());
    patch.modulate(
        ModTarget::LfoAmount(0),
        ModSlot {
            source: crate::ModSource::Envelope(0),
            depth: 1.0,
        },
    );
    assert!(patch.usage().is_empty());
    patch.modulate(ModTarget::Tune, lfo(1.0));
    assert_eq!(
        patch.usage(),
        Usage {
            lfos: 1,
            envelopes: 1
        }
    );
}

/// A single carrier with `waveform` at full sustain, and the filter.
fn filter_patch(waveform: Waveform, filter: FilterParams) -> Patch {
    let mut patch = Patch::default();
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].waveform = waveform;
    patch.operators[CARRIER].envelope.sustain = 1.0;
    patch.filters[0] = filter;
    patch
}

/// RMS after the attack of a held note.
fn held_rms(patch: Patch, note: u8, velocity: u8) -> f32 {
    let mut engine = synth(patch);
    engine.note_on(note, velocity);
    render_seconds(&mut engine, 0.2);
    rms(&render_seconds(&mut engine, 0.5))
}

/// High-frequency share: RMS of the sample-to-sample difference over the RMS.
fn brightness(patch: Patch, note: u8, velocity: u8) -> f32 {
    let mut engine = synth(patch);
    engine.note_on(note, velocity);
    render_seconds(&mut engine, 0.2);
    let out = render_seconds(&mut engine, 0.5);
    let diff: Vec<f32> = out.windows(2).map(|w| w[1] - w[0]).collect();
    rms(&diff) / rms(&out)
}

const LOW_PASS: FilterParams = FilterParams {
    kind: FilterKind::LowPass,
    ..FilterParams::DEFAULT
};

#[test]
fn filter_off_changes_nothing() {
    let off = FilterParams {
        kind: FilterKind::Off,
        cutoff: 100.0,
        resonance: 1.0,
        drive: 1.0,
        ..FilterParams::DEFAULT
    };
    let mut a = synth(Patch::default());
    let mut b = synth(Patch {
        filters: [off],
        ..Patch::default()
    });
    a.note_on(60, 100);
    b.note_on(60, 100);
    assert_eq!(render_seconds(&mut a, 0.3), render_seconds(&mut b, 0.3));
}

#[test]
fn low_pass_darkens_a_saw() {
    let open = brightness(
        filter_patch(Waveform::Sawtooth, FilterParams::DEFAULT),
        57,
        127,
    );
    let closed = brightness(
        filter_patch(
            Waveform::Sawtooth,
            FilterParams {
                cutoff: 500.0,
                ..LOW_PASS
            },
        ),
        57,
        127,
    );
    assert!(closed < 0.3 * open, "{closed} vs {open}");
}

#[test]
fn keytracking_follows_the_note() {
    // Cutoff at C4's frequency, tracking 100 %: a sine at the note's own
    // frequency is 3 dB down on every key.
    let filter = FilterParams {
        cutoff: pitch::note_to_freq(60.0),
        keytrack: 1.0,
        ..LOW_PASS
    };
    for note in [36u8, 60, 96] {
        let ratio = held_rms(filter_patch(Waveform::Sine, filter), note, 127)
            / held_rms(
                filter_patch(Waveform::Sine, FilterParams::DEFAULT),
                note,
                127,
            );
        let db = 20.0 * libm::log10f(ratio);
        assert!((db + 3.0).abs() < 0.3, "note {note}: {db:.2} dB");
    }
}

#[test]
fn velocity_opens_the_filter() {
    let mut patch = filter_patch(
        Waveform::Sawtooth,
        FilterParams {
            cutoff: 200.0,
            ..LOW_PASS
        },
    );
    patch.modulate(
        ModTarget::Cutoff(0),
        ModSlot {
            source: crate::ModSource::Velocity,
            depth: 48.0,
        },
    );
    let soft = brightness(patch, 45, 20);
    let hard = brightness(patch, 45, 127);
    assert!(hard > 2.0 * soft, "{hard} vs {soft}");
}

#[test]
fn a_resonant_filter_rings_out_after_the_envelope() {
    let mut patch = filter_patch(
        Waveform::Sawtooth,
        FilterParams {
            kind: FilterKind::LowPass,
            cutoff: 200.0,
            resonance: 1.0,
            ..FilterParams::DEFAULT
        },
    );
    patch.operators[CARRIER].envelope = EnvelopeParams {
        release: 0.001,
        ..patch.operators[CARRIER].envelope
    };
    let mut engine = synth(patch);
    engine.note_on(48, 127);
    render_seconds(&mut engine, 0.2);
    engine.note_off(48);
    render_seconds(&mut engine, 0.005);
    assert_eq!(engine.active_voices(), 0);
    // The envelope has ended, the filter still rings and decays.
    let tail = render_seconds(&mut engine, 0.02);
    assert!(peak(&tail[..480]) > 1e-3, "cut off: {}", peak(&tail[..480]));
    let rest = render_seconds(&mut engine, 2.0);
    assert!(peak(&rest[rest.len() - 4800..]) < 1e-4);
    assert_eq!(engine.engine(0).ringing, 0);
}

#[test]
fn drive_fades_in_from_zero() {
    let level = |drive| {
        let filter = FilterParams {
            kind: FilterKind::LowPass,
            cutoff: 20_000.0,
            drive,
            ..FilterParams::DEFAULT
        };
        let mut patch = filter_patch(Waveform::Sine, filter);
        patch.volume = 0.05;
        held_rms(patch, 69, 127)
    };
    let (off, tiny) = (level(0.0), level(0.001));
    assert!((tiny / off - 1.0).abs() < 0.01, "jump at 0: {off} → {tiny}");
}

#[test]
fn drive_adds_harmonics() {
    // Wide open low-pass, so only the drive changes the sine.
    let open = FilterParams {
        cutoff: crate::filter::MAX_CUTOFF,
        ..LOW_PASS
    };
    let clean = brightness(filter_patch(Waveform::Sine, open), 45, 127);
    let driven = brightness(
        filter_patch(Waveform::Sine, FilterParams { drive: 1.0, ..open }),
        45,
        127,
    );
    assert!(driven > 1.5 * clean, "{driven} vs {clean}");
}

fn render_stereo(engine: &mut Synth<1>, seconds: f32) -> (Vec<f32>, Vec<f32>) {
    let n = (seconds * SR) as usize;
    let mut l = vec![0.0; n];
    let mut r = vec![0.0; n];
    engine.render(&mut l, &mut r);
    (l, r)
}

/// Two heard sines at full sustain, op 1 at 1:1 and op 3 at 2:1.
fn two_carriers() -> Patch {
    let mut patch = Patch::default();
    patch.operators[MODULATOR].level = 0.0;
    for i in [CARRIER, 2] {
        patch.operators[i].output = true;
        patch.operators[i].level = 1.0;
        patch.operators[i].envelope.sustain = 1.0;
    }
    patch.operators[2].ratio = 2.0;
    patch
}

#[test]
fn centred_patches_stay_mono() {
    let mut engine = synth(two_carriers());
    engine.note_on(60, 100);
    let (l, r) = render_stereo(&mut engine, 0.2);
    assert!(!engine.engine(0).stereo);
    assert_eq!(l, r);
}

#[test]
fn operator_balance_places_carriers() {
    let mut patch = two_carriers();
    patch.operators[CARRIER].pan = -1.0;
    patch.operators[2].pan = 1.0;
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    let (l, r) = render_stereo(&mut engine, 0.5);
    // Left: only the 440 Hz carrier, right: only the 880 Hz one.
    let skip = 4800;
    assert!((rising_zero_crossings(&l[skip..]) as f32 / 0.4 - 440.0).abs() < 5.0);
    assert!((rising_zero_crossings(&r[skip..]) as f32 / 0.4 - 880.0).abs() < 5.0);
}

#[test]
fn balance_only_turns_the_far_side_down() {
    let mut patch = two_carriers();
    patch.balance = -0.5;
    let mut engine = synth(patch);
    engine.note_on(60, 100);
    let (l, r) = render_stereo(&mut engine, 0.3);
    let mut mono = synth(two_carriers());
    mono.note_on(60, 100);
    let (m, _) = render_stereo(&mut mono, 0.3);
    // Left unchanged, right at half level.
    assert!(l.iter().zip(&m).all(|(a, b)| (a - b).abs() < 1e-5));
    assert!(r.iter().zip(&m).all(|(a, b)| (a - 0.5 * b).abs() < 1e-5));
}

#[test]
fn lfo_on_the_balance_pans_back_and_forth() {
    let mut patch = lfo_patch(crate::LfoWaveform::Square, 1.0);
    patch.modulate(ModTarget::Balance, lfo(1.0));
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    let (l, r) = render_stereo(&mut engine, 1.0);
    let half = l.len() / 2;
    // Square high: right only; low: left only (skipping the edges).
    assert!(rms(&l[2000..half - 2000]) < 1e-4 && rms(&r[2000..half - 2000]) > 0.1);
    assert!(rms(&r[half + 2000..]) < 1e-4 && rms(&l[half + 2000..]) > 0.1);
}

#[test]
fn changing_the_unison_spread_glides() {
    let mut patch = Patch {
        unison: 3,
        unison_spread: 1.0,
        ..Patch::default()
    };
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].envelope.sustain = 1.0;
    let mut engine = synth(patch);
    engine.note_on(69, 127);
    render_stereo(&mut engine, 0.2);
    engine.set_patch(Patch { unison: 1, ..patch });
    let (l, r) = render_stereo(&mut engine, 0.2);
    // Without smoothing the far channels of the outer voices jump to full
    // level within a sample; the largest step stays near a sine's.
    let step = |x: &[f32]| {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    };
    let sine_step = 2.0 * core::f32::consts::PI * 440.0 / SR;
    assert!(
        step(&l) < 1.5 * sine_step && step(&r) < 1.5 * sine_step,
        "{} {}",
        step(&l),
        step(&r)
    );
    assert_eq!(engine.engine(0).spread, 0.0);
}

#[test]
fn switching_to_stereo_does_not_click() {
    let mut patch = two_carriers();
    patch.operators[2].level = 0.0;
    let mut engine = synth(patch);
    engine.note_on(60, 100);
    let (mut l, _) = render_stereo(&mut engine, 0.1);
    patch.operators[CARRIER].pan = 0.8;
    engine.set_patch(patch);
    let (l2, r2) = render_stereo(&mut engine, 0.1);
    assert!(engine.engine(0).stereo);
    l.extend(&l2);
    // The left side fades over the smoothing time, no jump at the switch.
    let max_step = l
        .windows(2)
        .map(|w| (w[1] - w[0]).abs())
        .fold(0.0f32, f32::max);
    assert!(max_step < 0.05, "{max_step}");
    assert!(rms(&l2[2400..]) < 0.3 * rms(&r2[2400..]));
}

#[test]
fn sanitized_drops_routes_of_missing_lfos() {
    let mut patch = lfo_patch(crate::LfoWaveform::Sine, 1.0);
    let before = patch.modulation.len();
    patch.modulate(
        ModTarget::OpLevel(CARRIER),
        ModSlot {
            source: crate::ModSource::Lfo(1),
            depth: 0.5,
        },
    );
    patch.modulate(ModTarget::Tune, lfo(100.0));
    let patch = patch.sanitized();
    let routes = &patch.modulation.as_slice()[before..];
    assert_eq!(routes[0].source, crate::ModSource::None);
    assert_eq!(routes[1].depth, MAX_PITCH_DEPTH);
}

/// Carrier (op 1) and modulator (op 2) at full sustain, the modulator in `mode`.
fn mode_patch(carrier: Waveform, mode: ModulationMode, level: f32) -> Patch {
    let mut patch = Patch::default();
    for op in &mut patch.operators[..2] {
        op.envelope = EnvelopeParams {
            attack: 0.001,
            sustain: 1.0,
            ..Default::default()
        };
    }
    patch.operators[CARRIER].waveform = carrier;
    patch.operators[MODULATOR].mode = mode;
    patch.operators[MODULATOR].level = level;
    patch
}

/// Output of a held A4 after the attack, at 44 kHz (exactly 100 samples per period).
fn held_a4(patch: Patch) -> Vec<f32> {
    let mut engine = Synth::<1>::boxed(44_000.0, patch);
    engine.note_on(69, 127);
    let mut l = vec![0.0; 44_000];
    let mut r = l.clone();
    engine.render(&mut l, &mut r);
    l.split_off(4_400)
}

#[test]
fn pulse_width_mode_narrows_a_square() {
    let positive = |out: &[f32]| out.iter().filter(|&&x| x > 0.0).count() as f32 / out.len() as f32;
    // Level 0 leaves the square alone; 0.5 squeezes the upper half into a quarter period.
    let plain = positive(&held_a4(mode_patch(
        Waveform::Square,
        ModulationMode::PulseWidth,
        0.0,
    )));
    let narrow = positive(&held_a4(mode_patch(
        Waveform::Square,
        ModulationMode::PulseWidth,
        0.5,
    )));
    assert!((plain - 0.5).abs() < 0.01, "{plain}");
    assert!((narrow - 0.25).abs() < 0.01, "{narrow}");
}

#[test]
fn sync_mode_keeps_the_modulator_period_and_sweeps_harmonics() {
    let crossings = |out: &[f32]| rising_zero_crossings(out);
    let plain = held_a4(mode_patch(Waveform::Sawtooth, ModulationMode::Sync, 0.0));
    // One octave faster in between resets: two carrier cycles per period.
    let synced = held_a4(mode_patch(
        Waveform::Sawtooth,
        ModulationMode::Sync,
        1.0 / MAX_SYNC_OCTAVES,
    ));
    // ±1 for a crossing at the window's edge.
    assert!(crossings(&synced).abs_diff(2 * crossings(&plain)) <= 1);
    // Still periodic with the modulator's period (100 samples).
    let mismatch = synced
        .iter()
        .zip(&synced[100..])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(mismatch < 1e-3, "{mismatch}");
}

#[test]
fn sync_with_an_odd_speed_still_restarts_every_period() {
    let synced = held_a4(mode_patch(Waveform::Sine, ModulationMode::Sync, 0.37));
    let mismatch = synced
        .iter()
        .zip(&synced[100..])
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(mismatch < 1e-2, "{mismatch}");
}

#[test]
fn frequency_mode_with_silent_modulator_changes_nothing() {
    let pm = held_a4(mode_patch(Waveform::Sine, ModulationMode::Phase, 0.0));
    let fm = held_a4(mode_patch(Waveform::Sine, ModulationMode::Frequency, 0.0));
    assert_eq!(pm, fm);
}

#[test]
fn sustain_pedal_holds_released_notes_until_lifted() {
    let mut engine = synth(Patch::default());
    engine.midi(&[0xb0, CC_SUSTAIN, 127]);
    engine.note_on(60, 100);
    render_seconds(&mut engine, 0.1);
    engine.note_off(60);
    // Far longer than the release: still sounding thanks to the pedal.
    render_seconds(&mut engine, 2.0);
    assert_eq!(engine.active_voices(), 1);
    engine.midi(&[0xb0, CC_SUSTAIN, 0]);
    render_seconds(&mut engine, 2.0);
    assert_eq!(engine.active_voices(), 0);

    // Pedal up while the key is still held: the note keeps sounding.
    engine.midi(&[0xb0, CC_SUSTAIN, 127]);
    engine.note_on(62, 100);
    engine.midi(&[0xb0, CC_SUSTAIN, 0]);
    render_seconds(&mut engine, 2.0);
    assert_eq!(engine.active_voices(), 1);
}

#[test]
fn all_sound_off_silences_and_all_notes_off_releases() {
    let mut engine = synth(Patch::default());
    engine.set_sustain(true);
    engine.note_on(60, 100);
    engine.note_on(64, 100);
    render_seconds(&mut engine, 0.05);
    engine.midi(&[0xb0, 123, 0]);
    // Released despite the pedal: the default patch decays to silence.
    let tail = render_seconds(&mut engine, 1.0);
    assert!(peak(&tail[tail.len() - 4800..]) < 1e-3);
    engine.note_on(60, 100);
    render_seconds(&mut engine, 0.05);
    engine.midi(&[0xb5, 120, 0]);
    assert_eq!(engine.active_voices(), 0);
    // After the decimator's delay, nothing.
    let after = render_seconds(&mut engine, 0.05);
    assert_eq!(peak(&after[2 * BLOCK_SIZE..]), 0.0);
}

#[test]
fn midi_velocity_zero_is_note_off() {
    let mut e = synth(Patch::default());
    e.midi(&[0x91, 60, 100]);
    render_seconds(&mut e, 0.05);
    assert_eq!(e.active_voices(), 1);
    e.midi(&[0x91, 60, 0]);
    render_seconds(&mut e, 2.0);
    assert_eq!(e.active_voices(), 0);
    // Unknown and truncated messages are ignored.
    e.midi(&[0xb0, 7, 100]);
    e.midi(&[0x90]);
    assert_eq!(e.active_voices(), 0);
}

#[test]
fn odd_buffer_sizes_match_block_aligned_rendering() {
    let mut a = synth(Patch::default());
    let mut b = synth(Patch::default());
    a.note_on(60, 100);
    b.note_on(60, 100);
    let mut la = vec![0.0; 960];
    let mut ra = la.clone();
    a.render(&mut la, &mut ra);
    let mut lb = Vec::new();
    for _ in 0..30 {
        let mut l = vec![0.0; 32];
        let mut r = l.clone();
        b.render(&mut l, &mut r);
        lb.extend(l);
    }
    assert_eq!(la, lb);
}

/// A held A4 (at `SR`) of a plain sine carrier with `fold` and `fold_mod`,
/// after the attack.
fn folded_a4(fold: f32, fold_mod: ModSlot, velocity: u8) -> Vec<f32> {
    let mut patch = mode_patch(Waveform::Sine, ModulationMode::Phase, 0.0);
    patch.operators[CARRIER].fold = fold;
    patch.modulate(ModTarget::OpFold(CARRIER), fold_mod);
    let mut engine = synth(patch);
    engine.note_on(69, velocity);
    let mut out = render_seconds(&mut engine, 0.5);
    out.split_off(4_800)
}

#[test]
fn fold_clamped_to_zero_changes_nothing() {
    let plain = folded_a4(0.0, ModSlot::NONE, 100);
    // The modulation pushes below 0: the folder runs, but leaves the wave alone.
    let down = ModSlot {
        source: crate::ModSource::Velocity,
        depth: -1.0,
    };
    assert_eq!(folded_a4(0.0, down, 100), plain);
}

#[test]
fn fold_fades_in_from_zero() {
    let (off, tiny) = (
        rms(&folded_a4(0.0, ModSlot::NONE, 127)),
        rms(&folded_a4(0.001, ModSlot::NONE, 127)),
    );
    assert!((tiny / off - 1.0).abs() < 0.01, "jump at 0: {off} → {tiny}");
}

#[test]
fn fold_adds_odd_harmonics_and_keeps_the_peak() {
    let plain = folded_a4(0.0, ModSlot::NONE, 127);
    let folded = folded_a4(0.5, ModSlot::NONE, 127);
    // A sine stays odd: strong third harmonic, hardly any second.
    assert!(tone(&plain, 1320.0) < 1e-3 * tone(&plain, 440.0));
    let (fundamental, second, third) = (
        tone(&folded, 440.0),
        tone(&folded, 880.0),
        tone(&folded, 1320.0),
    );
    assert!(
        third > 0.1 * fundamental,
        "third {third}, fundamental {fundamental}"
    );
    assert!(second < 1e-3 * fundamental, "second {second}");
    assert!(
        peak(&folded) <= peak(&plain) * 1.001,
        "{} vs {}",
        peak(&folded),
        peak(&plain)
    );
}

#[test]
fn velocity_on_the_fold_brightens_hard_notes() {
    let slot = ModSlot {
        source: crate::ModSource::Velocity,
        depth: 0.5,
    };
    // Share of the power above the fundamental.
    let bright = |velocity| {
        let out = folded_a4(0.0, slot, velocity);
        let fundamental = tone(&out, 440.0) / core::f32::consts::SQRT_2;
        1.0 - (fundamental / rms(&out)).powi(2)
    };
    assert!(
        bright(127) > 3.0 * bright(20),
        "{} vs {}",
        bright(127),
        bright(20)
    );
}

/// A held A4 (at `SR`) of op 1 as a filter of `kind` at `ratio` with
/// `resonance`, fed by a saw on op 2; `fm` > 0 adds a sine on op 3 moving
/// the cutoff (frequency mode) at that level.
fn filtered_a4(kind: crate::OperatorKind, ratio: f32, resonance: f32, fm: f32) -> Vec<f32> {
    let mut patch = mode_patch(Waveform::Sine, ModulationMode::Phase, 1.0);
    patch.operators[CARRIER] = OperatorParams {
        kind,
        ratio,
        resonance,
        ..patch.operators[CARRIER]
    };
    patch.operators[MODULATOR].waveform = Waveform::Sawtooth;
    if fm > 0.0 {
        patch.operators[2] = OperatorParams {
            mode: ModulationMode::Frequency,
            level: fm,
            envelope: patch.operators[CARRIER].envelope,
            ..Default::default()
        };
        patch.operators[2].modulates[CARRIER] = true;
    }
    let mut engine = synth(patch);
    engine.note_on(69, 100);
    let mut out = render_seconds(&mut engine, 0.5);
    out.split_off(4_800)
}

/// Amplitudes of the saw's harmonics 1..=8 of A4 in `out`.
fn harmonics(out: &[f32]) -> [f32; 8] {
    core::array::from_fn(|h| tone(out, 440.0 * (h + 1) as f32))
}

#[test]
fn filter_operator_low_pass_follows_its_ratio() {
    use crate::OperatorKind::LowPass;
    let dark = harmonics(&filtered_a4(LowPass, 1.0, 0.0, 0.0));
    let open = harmonics(&filtered_a4(LowPass, 8.0, 0.0, 0.0));
    // Relative to the fundamental, the 6th harmonic is far weaker below
    // the cutoff at 440 Hz than with the cutoff at 3520 Hz.
    let (dark6, open6) = (dark[5] / dark[0], open[5] / open[0]);
    assert!(open6 > 10.0 * dark6, "{dark6} vs {open6}");
    // A saw's 6th harmonic is 1/6 of the fundamental: the open filter keeps it.
    assert!((open6 - 1.0 / 6.0).abs() < 0.05, "{open6}");
}

#[test]
fn filter_operator_high_pass_and_band_pass() {
    use crate::OperatorKind::{BandPass, HighPass};
    // Two octaves below the cutoff (12 dB per octave), the fundamental drops
    // to about 1/16 against the 6th harmonic, whose share in a saw is 1/6.
    let high = harmonics(&filtered_a4(HighPass, 4.0, 0.0, 0.0));
    assert!(high[0] / high[5] < 6.0 / 10.0, "{high:?}");
    // The band-pass at the 3rd harmonic passes it at unity, not its neighbours.
    let band = harmonics(&filtered_a4(BandPass, 3.0, 0.8, 0.0));
    assert!(
        band[2] > 3.0 * band[0] && band[2] > 3.0 * band[5],
        "{band:?}"
    );
}

#[test]
fn filter_operator_resonance_lifts_the_cutoff() {
    use crate::OperatorKind::LowPass;
    let flat = harmonics(&filtered_a4(LowPass, 3.0, 0.0, 0.0));
    let peaked = harmonics(&filtered_a4(LowPass, 3.0, 0.9, 0.0));
    assert!(
        peaked[2] / peaked[0] > 3.0 * flat[2] / flat[0],
        "{flat:?} {peaked:?}"
    );
}

#[test]
fn filter_operator_without_input_is_silent() {
    let mut patch = Patch::default();
    patch.operators[CARRIER].kind = crate::OperatorKind::LowPass;
    patch.operators[MODULATOR].modulates = [false; crate::OPERATORS];
    let mut engine = synth(patch);
    engine.note_on(60, 100);
    assert_eq!(peak(&render_seconds(&mut engine, 0.2)), 0.0);
}

#[test]
fn filter_fm_changes_the_sound_and_stays_bounded() {
    use crate::OperatorKind::LowPass;
    let plain = filtered_a4(LowPass, 2.0, 0.7, 0.0);
    let swept = filtered_a4(LowPass, 2.0, 0.7, 0.1);
    assert!(swept.iter().all(|x| x.is_finite()));
    assert!(
        peak(&swept) < 2.0 * peak(&plain),
        "{} vs {}",
        peak(&swept),
        peak(&plain)
    );
    let difference = rms(&plain
        .iter()
        .zip(&swept)
        .map(|(a, b)| a - b)
        .collect::<Vec<_>>());
    assert!(difference > 0.1 * rms(&plain), "{difference}");
}

#[test]
fn filter_operator_resonance_is_sanitized() {
    let op = OperatorParams {
        resonance: f32::NAN,
        ..Default::default()
    }
    .sanitized();
    assert_eq!(op.resonance, 0.0);
    let op = OperatorParams {
        resonance: 3.0,
        ..Default::default()
    }
    .sanitized();
    assert_eq!(op.resonance, 1.0);
}

#[test]
fn routes_on_one_target_add_up_and_clamp_once() {
    let held = |level: f32, routes: &[crate::ModRoute]| {
        let mut patch = mode_patch(Waveform::Sine, ModulationMode::Phase, 0.0);
        patch.operators[CARRIER].level = level;
        for &route in routes {
            patch.modulation.push(route);
        }
        let mut engine = synth(patch);
        engine.note_on(69, 127);
        render_seconds(&mut engine, 0.2)
    };
    let velocity = |depth, via| crate::ModRoute {
        source: crate::ModSource::Velocity,
        target: ModTarget::OpLevel(CARRIER),
        depth,
        via,
    };
    let none = crate::ModSource::None;
    // 0.5 + 0.25 + 0.25 at full velocity: the same as level 1.
    assert_eq!(
        held(0.5, &[velocity(0.25, none), velocity(0.25, none)]),
        held(1.0, &[])
    );
    // Clamped once, after the sum: 0.9 + 0.3 - 0.2 = 1.0, not 1.0 - 0.2.
    assert_eq!(
        held(0.9, &[velocity(0.3, none), velocity(-0.2, none)]),
        held(1.0, &[])
    );
    // Via velocity: depth × velocity × velocity, at 127 the full depth.
    assert_eq!(
        held(0.5, &[velocity(0.5, crate::ModSource::Velocity)]),
        held(1.0, &[])
    );
}

/// A held sine on op 1 alone at level 0.5 (full volume, sustain at full
/// level), with `slot` on `target`.
fn controlled_sine(target: ModTarget, slot: ModSlot) -> Box<Synth<1>> {
    let mut patch = Patch {
        volume: 1.0,
        ..Patch::default()
    };
    patch.operators[MODULATOR].level = 0.0;
    patch.operators[CARRIER].level = 0.5;
    patch.operators[CARRIER].envelope = EnvelopeParams {
        attack: 0.001,
        sustain: 1.0,
        ..Default::default()
    };
    patch.modulate(target, slot);
    synth(patch)
}

#[test]
fn the_pitch_wheel_bends_through_its_route() {
    let bend = ModSlot {
        source: crate::ModSource::PitchBend,
        depth: 2.0,
    };
    let mut engine = controlled_sine(ModTarget::Tune, bend);
    engine.note_on(69, 127);
    let rest = render_seconds(&mut engine, 0.2);
    assert!(tone(&rest, 440.0) > 10.0 * tone(&rest, pitch::note_to_freq(71.0)));
    engine.handle_midi(MidiMessage::PitchBend {
        channel: 0,
        value: 8191,
    });
    render_seconds(&mut engine, 0.1);
    let up = render_seconds(&mut engine, 0.2);
    assert!(tone(&up, pitch::note_to_freq(71.0)) > 10.0 * tone(&up, 440.0));
    engine.handle_midi(MidiMessage::PitchBend {
        channel: 0,
        value: -8192,
    });
    render_seconds(&mut engine, 0.1);
    let down = render_seconds(&mut engine, 0.2);
    assert!(tone(&down, pitch::note_to_freq(67.0)) > 10.0 * tone(&down, 440.0));
}

#[test]
fn the_mod_wheel_moves_smoothly() {
    let wheel = ModSlot {
        source: crate::ModSource::ModWheel,
        depth: -1.0,
    };
    let mut engine = controlled_sine(ModTarget::OpLevel(CARRIER), wheel);
    engine.note_on(69, 127);
    let open = render_seconds(&mut engine, 0.1);
    engine.handle_midi(MidiMessage::ControlChange {
        channel: 0,
        controller: crate::controls::CC_MOD_WHEEL,
        value: 127,
    });
    let fading = render_seconds(&mut engine, 0.005);
    render_seconds(&mut engine, 0.1);
    let closed = render_seconds(&mut engine, 0.1);
    assert!(peak(&open[2400..]) > 0.45);
    assert!(peak(&fading) > 0.1, "the level glides instead of jumping");
    assert!(peak(&closed) < 1e-3, "{}", peak(&closed));
}

#[test]
fn aftertouch_follows_the_channel_and_single_keys() {
    let pressure = ModSlot {
        source: crate::ModSource::Aftertouch,
        depth: -1.0,
    };
    let level = |events: &[MidiMessage]| {
        let mut engine = controlled_sine(ModTarget::OpLevel(CARRIER), pressure);
        engine.note_on(69, 127);
        for &msg in events {
            engine.handle_midi(msg);
        }
        render_seconds(&mut engine, 0.2);
        peak(&render_seconds(&mut engine, 0.1))
    };
    let free = level(&[]);
    assert!(free > 0.45);
    assert!(
        level(&[MidiMessage::ChannelPressure {
            channel: 0,
            pressure: 127
        }]) < 1e-3
    );
    assert!(
        level(&[MidiMessage::PolyPressure {
            channel: 0,
            note: 69,
            pressure: 127
        }]) < 1e-3
    );
    assert_eq!(
        level(&[MidiMessage::PolyPressure {
            channel: 0,
            note: 70,
            pressure: 127
        }]),
        free,
        "another key"
    );
}

#[test]
fn key_tracking_follows_the_note() {
    // 64 semitones per unit: the pitch moves 1:1 with the keys, doubled.
    let track = ModSlot {
        source: crate::ModSource::KeyTrack,
        depth: MAX_PITCH_DEPTH,
    };
    let mut engine = controlled_sine(ModTarget::OpPitch(CARRIER), track);
    engine.note_on(72, 127);
    let out = render_seconds(&mut engine, 0.2);
    let expected = pitch::note_to_freq(72.0 + 12.0 * MAX_PITCH_DEPTH / 64.0);
    assert!(tone(&out, expected) > 10.0 * tone(&out, pitch::note_to_freq(72.0)));
}

#[test]
fn every_note_gets_its_own_random_value() {
    let random = ModSlot {
        source: crate::ModSource::Random,
        depth: 0.5,
    };
    let mut engine = controlled_sine(ModTarget::OpLevel(CARRIER), random);
    let mut peaks = Vec::new();
    for _ in 0..4 {
        engine.note_on(69, 127);
        peaks.push(peak(&render_seconds(&mut engine, 0.1)[2400..]));
        engine.note_off(69);
        render_seconds(&mut engine, 1.0);
    }
    assert!(peaks.iter().all(|&p| p < 0.999), "{peaks:?}");
    let spread =
        peaks.iter().fold(0.0f32, |m, &p| m.max(p)) - peaks.iter().fold(1.0f32, |m, &p| m.min(p));
    assert!(spread > 0.02, "{peaks:?}");
}

#[test]
fn fine_modulation_multiplies_the_frequency() {
    // 1 + 0.5: a fifth above (×1.5), whatever the note.
    let fine = ModSlot {
        source: crate::ModSource::Velocity,
        depth: 0.5,
    };
    let mut engine = controlled_sine(ModTarget::OpRatio(CARRIER), fine);
    engine.note_on(57, 127);
    let out = render_seconds(&mut engine, 0.2);
    assert!(tone(&out, 330.0) > 10.0 * tone(&out, 220.0));
}

#[test]
fn velocity_shortens_the_attack() {
    // Attack 1 s; full velocity moves it to the bottom of the time scale.
    let attack = |velocity: u8| {
        let mut patch = Patch {
            volume: 1.0,
            ..Patch::default()
        };
        patch.operators[MODULATOR].level = 0.0;
        patch.operators[CARRIER].envelope = EnvelopeParams {
            attack: 1.0,
            sustain: 1.0,
            ..Default::default()
        };
        let faster = ModSlot {
            source: crate::ModSource::Velocity,
            depth: -1.0,
        };
        patch.modulate(ModTarget::OpEnvelope(CARRIER, 1), faster);
        let mut engine = synth(patch);
        engine.note_on(69, velocity);
        peak(&render_seconds(&mut engine, 0.05))
    };
    assert!(attack(127) > 0.5, "fast attack: {}", attack(127));
    assert!(attack(1) < 0.05, "slow attack: {}", attack(1));
}

#[test]
fn mod_envelope_sustain_follows_the_key() {
    // Envelope 1 sets op 1's level; the key moves its sustain.
    let level = |note: u8| {
        let mut patch = Patch {
            volume: 1.0,
            ..Patch::default()
        };
        patch.operators[MODULATOR].level = 0.0;
        patch.operators[CARRIER].level = 0.0;
        patch.operators[CARRIER].envelope.sustain = 1.0;
        patch.envelopes.push(EnvelopeParams {
            attack: 0.001,
            decay: 0.01,
            sustain: 0.5,
            ..Default::default()
        });
        patch.modulate(
            ModTarget::OpLevel(CARRIER),
            ModSlot {
                source: crate::ModSource::Envelope(0),
                depth: 1.0,
            },
        );
        patch.modulate(
            ModTarget::ModEnvelope(0, 4),
            ModSlot {
                source: crate::ModSource::KeyTrack,
                depth: 0.5,
            },
        );
        let mut engine = synth(patch);
        engine.note_on(note, 127);
        render_seconds(&mut engine, 0.2);
        peak(&render_seconds(&mut engine, 0.1))
    };
    // Note 60: key 0 → sustain 0.5; note 124: key 1 → sustain 1.
    assert!((level(60) - 0.5).abs() < 0.02, "{}", level(60));
    assert!((level(124) - 1.0).abs() < 0.02, "{}", level(124));
}

#[test]
fn modulated_fade_in_and_glide_follow_their_sources() {
    // LFO 1 (20 Hz) on the pitch with a 2 s fade-in; full velocity removes
    // the fade. The vibrato shows as sidebands 20 Hz from the tone.
    let wobble = |velocity: u8| {
        let mut patch = Patch {
            volume: 1.0,
            ..Patch::default()
        };
        patch.operators[MODULATOR].level = 0.0;
        patch.operators[CARRIER].envelope.sustain = 1.0;
        patch.lfos.push(crate::LfoParams {
            rate: 20.0,
            fade_in: 2.0,
            ..Default::default()
        });
        patch.modulate(
            ModTarget::OpPitch(CARRIER),
            ModSlot {
                source: crate::ModSource::Lfo(0),
                depth: 1.0,
            },
        );
        patch.modulate(
            ModTarget::LfoFade(0),
            ModSlot {
                source: crate::ModSource::Velocity,
                depth: -1.0,
            },
        );
        let mut engine = synth(patch);
        engine.note_on(69, velocity);
        let out = render_seconds(&mut engine, 0.2);
        tone(&out, 460.0)
    };
    assert!(
        wobble(127) > 5.0 * wobble(1),
        "{} vs {}",
        wobble(127),
        wobble(1)
    );

    // Glide 1 s; the mod wheel shortens it to nothing.
    let glided = |wheel: u8| {
        let mut patch = Patch {
            volume: 1.0,
            portamento: 1.0,
            voice_mode: VoiceMode::Mono,
            ..Patch::default()
        };
        patch.operators[MODULATOR].level = 0.0;
        patch.operators[CARRIER].envelope.sustain = 1.0;
        patch.modulate(
            ModTarget::Glide,
            ModSlot {
                source: crate::ModSource::ModWheel,
                depth: -1.0,
            },
        );
        let mut engine = synth(patch);
        engine.handle_midi(MidiMessage::ControlChange {
            channel: 0,
            controller: crate::controls::CC_MOD_WHEEL,
            value: wheel,
        });
        engine.note_on(57, 127);
        render_seconds(&mut engine, 0.3);
        engine.note_on(69, 127);
        render_seconds(&mut engine, 0.05);
        let out = render_seconds(&mut engine, 0.1);
        tone(&out, 440.0)
    };
    assert!(
        glided(127) > 3.0 * glided(0),
        "{} vs {}",
        glided(127),
        glided(0)
    );
}

#[test]
fn modulated_depth_scales_a_route() {
    // Op 2 modulates op 1 at depth 0; velocity opens the route.
    let bright = |velocity: u8| {
        let mut patch = mode_patch(Waveform::Sine, ModulationMode::Phase, 0.5);
        patch.volume = 1.0;
        patch.operators[MODULATOR].depths[CARRIER] = 0.0;
        patch.modulate(
            ModTarget::OpDepth(MODULATOR, CARRIER),
            ModSlot {
                source: crate::ModSource::Velocity,
                depth: 1.0,
            },
        );
        let mut engine = synth(patch);
        engine.note_on(69, velocity);
        let out = render_seconds(&mut engine, 0.2);
        tone(&out, 880.0) / tone(&out, 440.0)
    };
    assert!(bright(127) > 0.1, "{}", bright(127));
    assert!(
        bright(127) > 5.0 * bright(1),
        "{} vs {}",
        bright(127),
        bright(1)
    );
}

#[test]
fn modulation_of_the_newest_voice_is_readable() {
    let wheel = ModSlot {
        source: crate::ModSource::ModWheel,
        depth: -0.5,
    };
    let mut engine = controlled_sine(ModTarget::OpLevel(CARRIER), wheel);
    assert!(engine.modulation().is_none(), "silent");
    engine.handle_midi(MidiMessage::ControlChange {
        channel: 0,
        controller: crate::controls::CC_MOD_WHEEL,
        value: 127,
    });
    engine.note_on(69, 127);
    render_seconds(&mut engine, 0.2);
    let live = engine.modulation().unwrap();
    assert!(
        (live.get(ModTarget::OpLevel(CARRIER)) + 0.5).abs() < 1e-3,
        "{}",
        live.get(ModTarget::OpLevel(CARRIER))
    );
    assert_eq!(live.get(ModTarget::OpPitch(CARRIER)), 0.0);
}
