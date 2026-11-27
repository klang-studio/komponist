//! The LFO behaviour of volt pinned before #352 moves it into a shared component, in two layers:
//!
//! - **Differential oracle:** the production [`Lfo`] and the verbatim copy in
//!   [`crate::lfo_reference`] run the same scripts in lockstep; `increment` and every `step` must
//!   give the same bits. Runs on every platform.
//! - **Render digests:** an FNV-1a hash over the output bits of representative renders, committed
//!   for the reference platform (Linux x86_64); elsewhere the digests are printed and not
//!   compared. volt steps its LFOs once per control interval, so every block split must give the
//!   same bits; that is checked on every platform.

use komponist_device_dsp::va_curve::{dst, src};
use komponist_processor::EventKind;

use super::baseline_support::{Dice, check_digests, split_independent_digest};
use super::{bend, cc, off, on, pressure, processor, render, slot, wire};
use crate::lfo::Lfo;
use crate::lfo_reference;

const RATES: [f32; 4] = [44_100.0, 48_000.0, 96_000.0, 22_050.0];

// ------------------------------------------------------------------------------------------------
// Differential oracle

#[test]
fn the_increment_matches_the_reference() {
    let mut dice = Dice(1);
    let mut rates: Vec<f32> = vec![
        0.0, 0.001, 0.01, 0.15, 1.0, 2.0, 5.5, 13.3, 50.0, 199.9, 200.0,
    ];
    // Tempo-synced rates: 4 bars … 1/32 at a few tempi.
    for bpm in [60.0f64, 90.0, 120.0, 127.0, 174.0] {
        for beats in [16.0f64, 8.0, 4.0, 2.0, 1.0, 0.5, 0.25, 0.125] {
            rates.push((bpm / 60.0 / beats) as f32);
        }
    }
    rates.extend((0..2000).map(|_| 200.0 * dice.unit() * dice.unit()));
    for rate in rates {
        for sr in RATES {
            assert_eq!(
                Lfo::increment(rate, sr),
                lfo_reference::Lfo::increment(rate, sr),
                "{rate} Hz at {sr} Hz"
            );
        }
    }
}

/// One LFO script: how the LFO starts and what drives it step by step.
#[derive(Clone, Copy, Debug)]
struct Case {
    seed: u32,
    /// Restarted at the start (a key LFO of a voice) or free-running (fade 1, never restarted).
    keyed: bool,
    shape: usize,
    uni: bool,
    rate: f32,
    /// Seconds of fade-in; 0 = off (step 1).
    fade: f32,
    /// Rate modulation depth in octaves (the matrix's LFO-rate destination).
    rate_mod: f32,
    /// Restart, shape and rate changes now and then (a key LFO only restarts).
    events: bool,
    /// Raw phase increments instead of rates (wrap edge cases).
    raw: bool,
}

/// Runs `case` for `steps` control intervals through both LFOs and compares every value.
fn lockstep(case: Case, steps: usize, script: u64) {
    let mut prod = Lfo::new(case.seed);
    let mut refr = lfo_reference::Lfo::new(case.seed);
    if case.keyed {
        prod.restart();
        refr.restart();
    }
    let sr = 48_000.0;
    let mut dice = Dice(script);
    let mut shape = case.shape;
    let mut rate = case.rate;
    let fade_step = if case.fade > 0.0 {
        32.0 / (case.fade * sr)
    } else {
        1.0
    };
    let mut wander = 0.0f32;
    for step in 0..steps {
        if case.events {
            match dice.next() % 600 {
                0 if case.keyed => {
                    prod.restart();
                    refr.restart();
                }
                1 => shape = (dice.next() % 8) as usize,
                2 => rate = 0.01 + 40.0 * dice.unit(),
                _ => {}
            }
        }
        let inc = if case.raw {
            match dice.next() % 5 {
                0 => u32::MAX,
                1 => 1 << 31,
                2 => (1 << 31) + 1,
                _ => dice.next() as u32,
            }
        } else {
            // A slowly wandering modulation in -1..1, as from an envelope or another LFO.
            wander = (wander + 0.05 * (2.0 * dice.unit() - 1.0)).clamp(-1.0, 1.0);
            let hz = rate * (wander * case.rate_mod).exp2();
            Lfo::increment(hz.clamp(0.001, 200.0), sr)
        };
        let fade = if case.keyed { fade_step } else { 1.0 };
        let a = prod.step(shape, inc, fade, case.uni);
        let b = refr.step(shape, inc, fade, case.uni);
        assert_eq!(
            a.to_bits(),
            b.to_bits(),
            "{case:?}, step {step}, shape {shape}, increment {inc}: {a} vs {b}"
        );
    }
}

#[test]
fn volt_lfos_match_the_reference_bit_for_bit() {
    let base = Case {
        seed: 1,
        keyed: true,
        shape: 0,
        uni: false,
        rate: 5.0,
        fade: 0.0,
        rate_mod: 0.0,
        events: false,
        raw: false,
    };
    let mut cases = Vec::new();
    // Every shape (6 and above are the smooth random of the catch-all arm) on both outputs, keyed
    // with a fade-in and free-running, at a slow, a medium and the fastest rate.
    for shape in [0, 1, 2, 3, 4, 5, 6, usize::MAX] {
        for uni in [false, true] {
            for (keyed, fade) in [(true, 0.0), (true, 0.4), (false, 0.0)] {
                for rate in [0.15, 7.7, 200.0] {
                    cases.push(Case {
                        seed: 0x6b6e_7572 ^ shape as u32,
                        keyed,
                        shape,
                        uni,
                        rate,
                        fade,
                        ..base
                    });
                }
            }
        }
    }
    // Rate modulation (two octaves either way, as the matrix allows), restarts, shape and rate
    // changes; the engine's seeds (free LFOs and a voice's odd seeds).
    for seed in [0x6b6e_7572, 0x6b6e_7572 ^ 0x5555, 0x1234_5679, u32::MAX] {
        for keyed in [true, false] {
            for uni in [false, true] {
                cases.push(Case {
                    seed,
                    keyed,
                    uni,
                    shape: 5,
                    rate: 3.0,
                    fade: 0.1,
                    rate_mod: 2.0,
                    events: true,
                    ..base
                });
            }
        }
    }
    // Phase wraps at the edges of the `u32` phase.
    for shape in [4, 5] {
        cases.push(Case {
            shape,
            raw: true,
            events: true,
            ..base
        });
    }
    for (i, case) in cases.into_iter().enumerate() {
        // 20 s of control intervals at 48 kHz.
        lockstep(case, 30_000, i as u64);
    }
}

// ------------------------------------------------------------------------------------------------
// Render digests

/// One second: a note, a second one off the 32-sample grid, a third, a retrigger of the first
/// (the same key again), the wheels, releases and a late note.
fn performance() -> Vec<(u32, EventKind)> {
    vec![
        (0, on(60, 100)),
        (1000, on(64, 80)),
        (5003, on(67, 120)),
        (9001, on(60, 90)),
        (12_000, cc(1, 100)),
        (13_500, bend(3000)),
        (14_017, pressure(90)),
        (20_000, off(64)),
        (26_000, off(60)),
        (26_000, off(67)),
        (30_011, on(72, 70)),
        (40_000, off(72)),
    ]
}

const FRAMES: usize = 48_000;

/// Renders `changes` with every split; asserts they give the same bits and returns the digest.
fn render_digest(changes: &[(&str, f64)]) -> u64 {
    split_independent_digest(FRAMES, &performance(), |calls| {
        let mut p = processor(changes);
        let mut out = [Vec::new(), Vec::new()];
        for (n, here) in calls {
            let part = render(&mut p, n, &here, 120.0);
            out[0].extend_from_slice(&part[0]);
            out[1].extend_from_slice(&part[1]);
        }
        out
    })
}

/// The values of preset `name`, as changes by model id.
fn preset(name: &str) -> Vec<(&'static str, f64)> {
    let index = crate::presets::presets()
        .iter()
        .position(|p| p.name == name)
        .unwrap_or_else(|| panic!("no preset {name}"));
    let values = crate::SPEC.preset_values(index).unwrap();
    super::KEYS
        .iter()
        .map(|key| (*key, values[wire(key) as usize]))
        .collect()
}

/// Sample-and-hold into the cutoff, smooth random into the pitch, the LFOs' rates modulated by
/// the third envelope and by LFO 1, fades, unipolar output, unison.
fn lfo_matrix() -> Vec<(&'static str, f64)> {
    let mut all = vec![
        ("lfo1_shape", 4.0),
        ("lfo1_rate", 6.0),
        ("lfo1_fade", 0.3),
        ("lfo1_uni", 1.0),
        ("lfo2_shape", 5.0),
        ("lfo2_rate", 3.0),
        ("lfo2_fade", 0.05),
        ("mod_a", 0.3),
        ("mod_d", 0.4),
        ("mod_s", 0.2),
        ("unison", 3.0),
        ("analog", 0.5),
    ];
    all.extend(slot(1, src::LFO1, dst::CUTOFF, 0.5));
    all.extend(slot(2, src::LFO2, dst::PITCH, 0.1));
    all.extend(slot(3, src::ENV3, dst::LFO1_RATE, 0.8));
    all.extend(slot(4, src::LFO1, dst::LFO2_RATE, -0.6));
    all
}

/// Both LFOs free-running (shared by all voices): a fast unipolar sine on the pulse width and
/// a tempo-synced smooth random on the cutoff; the mod wheel's vibrato; mono with retrigger.
fn free_lfos() -> Vec<(&'static str, f64)> {
    let mut all = vec![
        ("lfo1_key", 0.0),
        ("lfo1_shape", 0.0),
        ("lfo1_rate", 13.0),
        ("lfo1_uni", 1.0),
        ("lfo2_key", 0.0),
        ("lfo2_shape", 5.0),
        ("lfo2_sync", 1.0),
        ("lfo2_div", 6.0),
        ("wheel_vib", 50.0),
        ("mode", 1.0),
    ];
    all.extend(slot(1, src::LFO1, dst::PW_A, 0.4));
    all.extend(slot(2, src::LFO2, dst::CUTOFF, 0.6));
    all
}

/// The remaining shapes: a keyed unipolar square with a fade on the volume, a free unipolar
/// triangle and a keyed saw on the pulse width of B and the mix.
fn uni_shapes() -> Vec<(&'static str, f64)> {
    let mut all = vec![
        ("lfo1_shape", 3.0),
        ("lfo1_rate", 4.0),
        ("lfo1_uni", 1.0),
        ("lfo1_fade", 0.05),
        ("lfo2_shape", 1.0),
        ("lfo2_key", 0.0),
        ("lfo2_uni", 1.0),
        ("lfo2_rate", 0.7),
    ];
    all.extend(slot(1, src::LFO1, dst::VOLUME, -0.5));
    all.extend(slot(2, src::LFO2, dst::PW_B, 0.5));
    all.extend(slot(3, src::LFO2, dst::MIX, 0.3));
    all
}

/// Factory presets that use their LFOs: vibrato with a fade (Mono Lead), a slow triangle (Wide
/// Pad), a synced free triangle (Wobble), a smooth random (Noise Wind).
const PRESETS: [&str; 4] = ["Mono Lead", "Wide Pad", "Wobble", "Noise Wind"];

#[test]
fn volt_renders_match_the_committed_digests() {
    let mut actual = vec![
        ("lfo-matrix".to_string(), render_digest(&lfo_matrix())),
        ("free-lfos".to_string(), render_digest(&free_lfos())),
        ("uni-shapes".to_string(), render_digest(&uni_shapes())),
    ];
    for name in PRESETS {
        actual.push((name.to_string(), render_digest(&preset(name))));
    }
    check_digests("volt", &actual, VOLT_DIGESTS);
}

/// The committed digests (Linux x86_64), from `e97f261c`, before #352. Every block split gives
/// the same bits, so there is one per render.
const VOLT_DIGESTS: &[(&str, u64)] = &[
    ("lfo-matrix", 0xa10986aa51579f52),
    ("free-lfos", 0x7251f41e999a64d9),
    ("uni-shapes", 0x925038e7975e0705),
    ("Mono Lead", 0x236b9e4d2906ec81),
    ("Wide Pad", 0xafa582a2c9d2812e),
    ("Wobble", 0x63f7608cdb6117d1),
    ("Noise Wind", 0x4df84a48418bfd75),
];
