//! The LFO behaviour of sampler-core pinned before #362 moves it onto `klang_synth::CyclicLfo`,
//! in two layers:
//!
//! - **Differential oracle:** the production [`Lfo`] and the verbatim copy in
//!   [`crate::lfo_reference`] run the same scripts in lockstep, each with its own copy of the
//!   voice RNG; every value, every output of `next` and the RNG state after every step (with and
//!   without the noise source's draw in between, as in `Voice::render`) must give the same bits.
//!   Runs on every platform.
//! - **Render digests:** an FNV-1a hash over the output bits of representative [`Engine`] renders,
//!   committed for the reference platform (Linux x86_64); elsewhere the digests are printed and not
//!   compared. The voices render per sample, but the free-running per-pad phases advance once per
//!   call, so every block split has its own digest.

use std::sync::Arc;

use crate::baseline_support::{Dice, Digest, SPLITS, Split, check_digests};
use crate::lfo_reference;
use crate::modulation::{LFO_RATE_RANGE_OCT, Lfo, LfoShape, ModSlot, ModSource, ModTarget, Rng};
use crate::{
    Engine, EnvParams, FilterMode, NUM_OUTPUTS, NUM_PADS, NUM_SAMPLE_SLOTS, PadLayers, PadParams,
    PadSettings, PlaybackDirection, PlaybackRegion, Sample, SampleTrigger, slot_index,
};

const SHAPES: [LfoShape; 6] = [
    LfoShape::Sine,
    LfoShape::Triangle,
    LfoShape::SawUp,
    LfoShape::SawDown,
    LfoShape::Square,
    LfoShape::SampleHold,
];

const SAMPLE_RATES: [f32; 6] = [8_000.0, 22_050.0, 44_100.0, 48_000.0, 96_000.0, 192_000.0];

/// Start phases as the engine hands them to `reset`: 0 for retriggering LFOs, otherwise the
/// pad's free-running `f32` phase in `0..1` (and a few values outside, which `from_unit` wraps).
const PHASES: [f32; 10] = [
    0.0,
    0.25,
    0.5,
    0.75,
    0.123_456_7,
    0.999_999_94,
    1.0,
    -0.25,
    3.75,
    f32::MIN_POSITIVE,
];

/// Rates beyond the musical range: zero, negative, non-finite, Nyquist and above (the increment
/// aliases), and huge.
fn edge_rate(dice: &mut Dice, sample_rate: f32) -> f32 {
    match dice.next() % 12 {
        0 => 0.0,
        1 => -3.0,
        2 => f32::NAN,
        3 => f32::INFINITY,
        4 => f32::NEG_INFINITY,
        5 => sample_rate / 2.0,
        6 => sample_rate,
        7 => sample_rate * 1.5,
        8 => 1e9,
        9 => f32::MIN_POSITIVE,
        _ => sample_rate * dice.unit(),
    }
}

// ------------------------------------------------------------------------------------------------
// Differential oracle

/// One LFO script: how the LFO starts and what drives it sample by sample.
#[derive(Clone, Copy, Debug)]
struct Case {
    seed: u32,
    shape: LfoShape,
    phase: f32,
    sample_rate: f32,
    rate: f32,
    /// LFO → LFO rate amount (`Voice::render` feeds the previous sample's matrix value back).
    self_mod: f32,
    /// The noise source draws from the same RNG after every LFO step.
    noise: bool,
    /// Resets, shape and rate changes now and then.
    events: bool,
    /// Edge-case rates instead of `rate` (see [`edge_rate`]).
    edge: bool,
}

/// Runs `case` for `steps` samples through both LFOs and compares every value and RNG state.
fn lockstep(case: Case, steps: usize, script: u64) {
    let mut rng_p = Rng::new(case.seed);
    let mut rng_r = Rng::new(case.seed);
    // `Voice::start` draws the per-note random value before it resets the LFO.
    let _ = rng_p.unipolar();
    let _ = rng_r.unipolar();
    let mut prod = Lfo::default();
    let mut refr = lfo_reference::Lfo::default();
    prod.reset(case.phase, &mut rng_p);
    refr.reset(case.phase, &mut rng_r);
    assert_eq!(rng_p, rng_r, "{case:?}: reset");

    let mut dice = Dice(script);
    let (mut shape, mut rate, mut rate_mod) = (case.shape, case.rate, 0.0f32);
    for step in 0..steps {
        if case.events {
            match dice.next() % 700 {
                0 => {
                    let phase = 4.0 * dice.unit() - 1.0;
                    prod.reset(phase, &mut rng_p);
                    refr.reset(phase, &mut rng_r);
                }
                1 => shape = SHAPES[(dice.next() % 6) as usize],
                2 => rate = 0.01 + 60.0 * dice.unit() * dice.unit(),
                _ => {}
            }
        }
        for s in SHAPES {
            assert_eq!(
                prod.value(s).to_bits(),
                refr.value(s).to_bits(),
                "{case:?}, step {step}: value of {s:?}"
            );
        }
        let hz = if case.edge {
            edge_rate(&mut dice, case.sample_rate)
        } else {
            rate * (rate_mod * LFO_RATE_RANGE_OCT).exp2()
        };
        let a = prod.next(shape, hz, case.sample_rate, &mut rng_p);
        let b = refr.next(shape, hz, case.sample_rate, &mut rng_r);
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "{case:?}, step {step}: next({shape:?}, {hz} Hz)"
        );
        assert_eq!(rng_p, rng_r, "{case:?}, step {step}: RNG after next");
        if case.noise {
            assert_eq!(
                rng_p.bipolar().to_bits(),
                rng_r.bipolar().to_bits(),
                "{case:?}, step {step}: noise"
            );
        }
        rate_mod = a * case.self_mod;
    }
    assert_eq!(rng_p, rng_r, "{case:?}: RNG at the end");
}

#[test]
fn every_shape_rate_and_start_phase_matches_the_reference() {
    let mut script = 0;
    for shape in SHAPES {
        for sample_rate in SAMPLE_RATES {
            for (i, &phase) in PHASES.iter().enumerate() {
                // (rate, self modulation): still, slow, musical, fast, modulated, audio rate.
                let (rate, self_mod) = [
                    (0.0, 0.0),
                    (0.05, 0.0),
                    (2.0, 0.0),
                    (7.7, 0.5),
                    (50.0, -1.0),
                    (1_000.0, 0.25),
                    (sample_rate / 2.0, 0.0),
                    (sample_rate * 0.37, 0.0),
                ][i % 8];
                script += 1;
                lockstep(
                    Case {
                        seed: script as u32,
                        shape,
                        phase,
                        sample_rate,
                        rate,
                        self_mod,
                        noise: i % 2 == 0,
                        events: false,
                        edge: false,
                    },
                    4_000,
                    script,
                );
            }
        }
    }
}

#[test]
fn scripted_resets_shape_and_rate_changes_match_the_reference() {
    for (n, sample_rate) in SAMPLE_RATES.into_iter().enumerate() {
        for noise in [false, true] {
            let seed = 362 + n as u64 * 2 + u64::from(noise);
            lockstep(
                Case {
                    seed: seed as u32,
                    shape: SHAPES[n % 6],
                    phase: 0.3,
                    sample_rate,
                    rate: 4.0,
                    self_mod: 0.7,
                    noise,
                    events: true,
                    edge: false,
                },
                100_000,
                seed,
            );
        }
    }
}

#[test]
fn edge_case_rates_match_the_reference() {
    for (n, sample_rate) in SAMPLE_RATES.into_iter().enumerate() {
        for shape in SHAPES {
            let seed = 1_000 + n as u64 * 8 + shape as u64;
            lockstep(
                Case {
                    seed: seed as u32,
                    shape,
                    phase: 0.9,
                    sample_rate,
                    rate: 0.0,
                    self_mod: 0.0,
                    noise: true,
                    events: true,
                    edge: true,
                },
                20_000,
                seed,
            );
        }
    }
}

#[test]
fn reset_matches_the_reference_for_any_phase() {
    let mut dice = Dice(7);
    let mut rng_p = Rng::new(99);
    let mut rng_r = Rng::new(99);
    for i in 0..100_000 {
        let phase = match i % 4 {
            0 => dice.unit(),
            1 => f32::from_bits(dice.next() as u32),
            2 => 16.0 * dice.unit() - 8.0,
            _ => (dice.unit() + 30.0 * dice.unit() * 512.0 / 48_000.0).fract(),
        };
        let mut prod = Lfo::default();
        let mut refr = lfo_reference::Lfo::default();
        prod.reset(phase, &mut rng_p);
        refr.reset(phase, &mut rng_r);
        assert_eq!(rng_p, rng_r);
        for s in SHAPES {
            assert_eq!(
                prod.value(s).to_bits(),
                refr.value(s).to_bits(),
                "reset({phase}), {s:?}"
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------
// Render digests

/// A deterministic test sample of `frames` frames at `rate`: a falling ramp (period 97 frames)
/// with noise and a decay, so pitch, filter and lo-fi modulation all change the bits.
fn test_sample(seed: u64, frames: usize, rate: f32, stereo: bool) -> Sample {
    let mut dice = Dice(seed);
    let mut channel = |offset: usize| -> Vec<f32> {
        (0..frames)
            .map(|i| {
                let ramp = 1.0 - 2.0 * ((i + offset) % 97) as f32 / 97.0;
                let noise = 2.0 * dice.unit() - 1.0;
                let decay = 1.0 - i as f32 / frames as f32;
                (0.6 * ramp + 0.2 * noise) * (0.3 + 0.7 * decay)
            })
            .collect()
    };
    if stereo {
        let left = channel(0);
        let right = channel(31);
        Sample::stereo(rate, left, right)
    } else {
        Sample::mono(rate, channel(0))
    }
}

fn slot(source: ModSource, target: ModTarget, amount: f32) -> ModSlot {
    ModSlot {
        source,
        target,
        amount,
    }
}

/// Pad parameters: every shape on its own pad with different targets, rates, retrigger modes and
/// outputs; Sample-and-Hold together with the noise source (both draw from the voice RNG); the
/// LFO modulating its own rate; velocity and an envelope on the rate; LFO at note on (sample
/// start, attack); a Nyquist-rate square; a pad without slots (as the drum Sampler plays it); a
/// pad with noise only; a gate pad with a modulated end.
///
/// No pad modulates drive: its `tanh` comes from the system libm, whose results differ between
/// glibc versions (2.44 here, older on CI), and the digests must not depend on that.
fn pads() -> Vec<PadParams> {
    use LfoShape::*;
    use ModSource as S;
    use ModTarget as T;
    let pad = |shape, rate, retrigger, output: u8, slots: &[ModSlot]| {
        let mut p = PadParams {
            lfo_shape: shape,
            lfo_rate_hz: rate,
            lfo_retrigger: retrigger,
            output,
            amp: EnvParams {
                attack_ms: 2.0,
                hold_ms: 10.0,
                decay_ms: 300.0,
                sustain: 0.6,
                release_ms: 80.0,
            },
            ..PadParams::default()
        };
        p.slots[..slots.len()].copy_from_slice(slots);
        p
    };
    let mut pads = vec![
        pad(Sine, 5.5, true, 0, &[slot(S::Lfo, T::Pitch, 0.05)]),
        PadParams {
            filter_mode: FilterMode::LowPass,
            cutoff_hz: 1_200.0,
            resonance: 0.4,
            ..pad(Triangle, 0.37, false, 1, &[slot(S::Lfo, T::Cutoff, 0.5)])
        },
        pad(SawUp, 13.3, false, 0, &[slot(S::Lfo, T::Pan, 0.8)]),
        pad(SawDown, 50.0, true, 2, &[slot(S::Lfo, T::Volume, -0.6)]),
        PadParams {
            filter_mode: FilterMode::HighPass,
            cutoff_hz: 400.0,
            ..pad(
                Square,
                2.2,
                false,
                1,
                &[
                    slot(S::Lfo, T::Cutoff, 0.2),
                    slot(S::Lfo, T::Resonance, 0.3),
                ],
            )
        },
        PadParams {
            filter_mode: FilterMode::BandPass,
            cutoff_hz: 900.0,
            ..pad(
                SampleHold,
                7.7,
                true,
                0,
                &[slot(S::Lfo, T::Pitch, 0.1), slot(S::Noise, T::Cutoff, 0.3)],
            )
        },
        pad(
            SampleHold,
            0.01,
            false,
            2,
            &[
                slot(S::Lfo, T::Bits, -0.3),
                slot(S::Lfo, T::Downsample, 0.4),
                slot(S::Noise, T::Pan, 0.5),
            ],
        ),
        pad(
            Sine,
            3.0,
            true,
            3,
            &[slot(S::Lfo, T::LfoRate, 1.0), slot(S::Lfo, T::Pitch, 0.02)],
        ),
        pad(
            SampleHold,
            9.0,
            false,
            0,
            &[
                slot(S::Velocity, T::LfoRate, 0.8),
                slot(S::ModEnv1, T::LfoRate, -0.5),
                slot(S::Lfo, T::Volume, 0.5),
                slot(S::Noise, T::Volume, 0.1),
            ],
        ),
        pad(
            Square,
            4.4,
            false,
            1,
            &[
                slot(S::Lfo, T::SampleStart, 0.3),
                slot(S::Lfo, T::AmpAttack, 0.5),
                slot(S::Lfo, T::AmpDecay, -0.5),
                slot(S::Lfo, T::Pitch, 0.03),
            ],
        ),
        pad(Square, 24_000.0, true, 2, &[slot(S::Lfo, T::Pitch, 0.01)]),
        pad(SawUp, 6.0, false, 3, &[]),
        pad(Sine, 6.0, true, 3, &[slot(S::Noise, T::Pitch, 0.02)]),
        PadParams {
            gate: true,
            ..pad(
                SawUp,
                1.3,
                false,
                0,
                &[
                    slot(S::Lfo, T::SampleEnd, -0.4),
                    slot(S::Random, T::LfoRate, 0.6),
                ],
            )
        },
        pad(Triangle, 11.0, false, 2, &[slot(S::Lfo, T::Pitch, 0.04)]),
    ];
    pads.resize(NUM_PADS, PadParams::default());
    pads
}

/// A performance event for [`Engine`], at a sample position.
#[derive(Clone, Copy, Debug)]
enum Play {
    Hit(usize, f32),
    Off(usize),
    /// `Engine::set_lfo_phase`, as a host transport sync would.
    Phase(usize, f32),
    /// `Engine::trigger` (General-Sampler's path): pad, tune, independent pitch.
    Trigger(usize, f32, bool),
}

/// One second at 48 kHz: two hits per pad off any block grid, note offs for the gate pad, a phase
/// jump and region triggers on free-running pads.
fn performance() -> Vec<(usize, Play)> {
    let mut plays = Vec::new();
    for p in 0..15 {
        plays.push((997 * p + 13 * (p % 3), Play::Hit(p, 0.4 + 0.04 * p as f32)));
        plays.push((23_011 + 1_499 * p, Play::Hit(p, 1.0 - 0.03 * p as f32)));
    }
    plays.extend([
        (20_000, Play::Off(13)),
        (45_000, Play::Off(13)),
        (17_777, Play::Phase(1, 0.6)),
        (17_777, Play::Phase(6, 0.95)),
        (5_003, Play::Trigger(14, -5.0, false)),
        (33_333, Play::Trigger(14, 7.0, true)),
        (41_001, Play::Trigger(4, 0.0, false)),
    ]);
    // `render_digest` plays the events in list order: keep them sorted by position.
    plays.sort_by_key(|(at, _)| *at);
    plays
}

const SECONDS: f32 = 1.0;

/// Renders [`performance`] through an [`Engine`] at `sample_rate` with `split`, the calls also
/// cut at the events (as a host processor does), advancing the free-running LFO phases after
/// every call, and hashes the four output buses.
fn render_digest(sample_rate: f32, split: Split) -> u64 {
    let mut samples: Vec<Option<Arc<Sample>>> = vec![None; NUM_SAMPLE_SLOTS];
    let mut layers = vec![PadLayers::default(); NUM_PADS];
    for pad in 0..NUM_PADS {
        let sample = test_sample(pad as u64, 33_075, 44_100.0, pad % 2 == 1);
        samples[slot_index(pad, 0)] = Some(Arc::new(sample));
        layers[pad].layers[0].loaded = true;
    }
    let settings: Vec<PadSettings> = pads()
        .iter()
        .map(|p| PadSettings::new(p, sample_rate))
        .collect();
    let plays = performance();
    let frames = (SECONDS * sample_rate) as usize;
    // Event positions are in 48 kHz frames.
    let at = |i: usize| plays[i].0 * frames / 48_000;

    let mut engine = Engine::new(sample_rate);
    let mut digest = Digest::new();
    let mut sizes = split.sizes();
    let (mut pos, mut next, mut peak) = (0, 0, 0.0f32);
    while pos < frames {
        while next < plays.len() && at(next) <= pos {
            match plays[next].1 {
                Play::Hit(pad, velocity) => {
                    engine.note_on(pad, velocity, &samples, &settings, &layers)
                }
                Play::Off(pad) => engine.note_off(pad, &settings),
                Play::Phase(pad, phase) => engine.set_lfo_phase(pad, phase),
                Play::Trigger(pad, tune, independent_pitch) => {
                    let slot = slot_index(pad, 0);
                    let sample = samples[slot].as_deref().unwrap();
                    engine.trigger(
                        SampleTrigger {
                            pad,
                            slot,
                            key: 60,
                            note_id: None,
                            channel: None,
                            velocity: 0.9,
                            tune,
                            independent_pitch,
                            region: PlaybackRegion {
                                start: 1_000.0,
                                end: 30_000.0,
                                fade_in: 64.0,
                                fade_out: 256.0,
                                looping: None,
                                direction: PlaybackDirection::Forward,
                            },
                        },
                        sample,
                        &settings[pad],
                    );
                }
            }
            next += 1;
        }
        let until = if next < plays.len() {
            at(next).min(frames)
        } else {
            frames
        };
        let n = sizes().min(until - pos);
        for _ in 0..n {
            let mut bus = [[0.0; 2]; NUM_OUTPUTS];
            engine.render_frame(&samples, &settings, &mut bus);
            for out in &bus[..4] {
                digest.samples(out);
                peak = peak.max(out[0].abs()).max(out[1].abs());
            }
        }
        engine.advance_lfo_phases(&settings, n);
        pos += n;
    }
    assert!(peak > 0.01, "silent render");
    digest.0
}

#[test]
fn engine_renders_match_the_committed_digests() {
    let mut actual = Vec::new();
    for sample_rate in [48_000.0, 44_100.0] {
        for split in SPLITS {
            actual.push((
                format!("engine-{sample_rate}/{}", split.name()),
                render_digest(sample_rate, split),
            ));
        }
    }
    check_digests("sampler-core", &actual, ENGINE_DIGESTS);
}

/// The committed digests (Linux x86_64), from `01db1cd8`, before #362.
const ENGINE_DIGESTS: &[(&str, u64)] = &[
    ("engine-48000/split-1", 0x22c693ee62baeaf2),
    ("engine-48000/split-31", 0x621b363de5732e9f),
    ("engine-48000/split-32", 0xadac622af0c25c0f),
    ("engine-48000/split-33", 0xfe317e29073433a0),
    ("engine-48000/split-64", 0xfb981d8fa5300cef),
    ("engine-48000/split-256", 0xba6813691024d497),
    ("engine-48000/irregular-362-256", 0x40b69de02b052328),
    ("engine-44100/split-1", 0x072f425614604adb),
    ("engine-44100/split-31", 0x64a1ed776f4e30d6),
    ("engine-44100/split-32", 0x5c219115b594310a),
    ("engine-44100/split-33", 0xa15e45fadb137eb7),
    ("engine-44100/split-64", 0x535aac56f2fb6238),
    ("engine-44100/split-256", 0x94755aa43f640a39),
    ("engine-44100/irregular-362-256", 0x3efc02788e36dd89),
];
