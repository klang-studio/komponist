//! The factory wavetables of the wavetable instrument „surf“ (#107), as recipes that its DSP and
//! its graph share, so the 3D view cannot drift away from the sound.
//!
//! A table is [`FRAMES`] single cycles; the table position 0 … 1 runs from the first to the last.
//! Every table is original and generated from a formula, there are no sample files. A recipe is
//! either a **spectrum** (the signed amplitude and phase of harmonic `k` at position `p`, for
//! `sin(2π·k·x + phase)`) or a **shape** (the waveform itself at phase `x`). The DSP
//! builds band-limited levels from the recipes with an FFT; the graph draws
//! [`preview_at`], a coarse copy computed once per table.
//!
//! [`TABLES`] is append-only: a project stores the index of the chosen table.

use std::f64::consts::{PI, TAU};
use std::sync::OnceLock;

/// The single cycles of a table.
pub const FRAMES: usize = 32;
/// The highest harmonic a spectrum recipe is asked for (the DSP's widest level).
pub const MAX_HARMONIC: usize = 512;

/// The tables, in the order of the `osc_*_table` choice (append only). The names are the
/// German ones; the shown names come from the catalogs (`choice-surf-table-*`).
pub const TABLES: [&str; 16] = [
    "Grundformen",
    "Pulsbreite",
    "Sync",
    "Obertöne",
    "Ungerade–Gerade",
    "Orgel",
    "Vokale",
    "Chor",
    "FM",
    "Falten",
    "Bits",
    "Kamm",
    "Glas",
    "Hohl",
    "Resonanz",
    "Zufall",
];

/// How a table is made.
#[derive(Clone, Copy, Debug)]
pub enum Recipe {
    /// Harmonic `k` (1 …) at position `p`: signed amplitude and phase of `sin(2π·k·x + phase)`.
    Spectrum(fn(p: f64, k: usize) -> (f64, f64)),
    /// The waveform at position `p` and phase `x` (0 … 1); the DSP removes its DC and
    /// band-limits it.
    Shape(fn(p: f64, x: f64) -> f64),
}

/// The recipe of table `table` (an index of [`TABLES`]; out of range: the first).
pub fn recipe(table: usize) -> Recipe {
    match table {
        1 => Recipe::Shape(pulse),
        2 => Recipe::Shape(sync),
        3 => Recipe::Spectrum(harmonics),
        4 => Recipe::Spectrum(odd_even),
        5 => Recipe::Spectrum(organ),
        6 => Recipe::Spectrum(vowels),
        7 => Recipe::Spectrum(choir),
        8 => Recipe::Shape(fm),
        9 => Recipe::Shape(fold),
        10 => Recipe::Shape(bits),
        11 => Recipe::Spectrum(comb),
        12 => Recipe::Spectrum(glass),
        13 => Recipe::Spectrum(hollow),
        14 => Recipe::Shape(resonance),
        15 => Recipe::Spectrum(random),
        _ => Recipe::Spectrum(basic),
    }
}

/// The position of frame `frame` (0 … [`FRAMES`] − 1).
pub fn frame_position(frame: usize) -> f64 {
    frame as f64 / (FRAMES - 1) as f64
}

/// `a` to `b` by `t`.
fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The segment `p` falls into when 0 … 1 is cut into `n` equal parts, and the fraction inside.
fn segment(p: f64, n: usize) -> (usize, f64) {
    let s = p.clamp(0.0, 1.0) * n as f64;
    let i = (s.floor() as usize).min(n - 1);
    (i, s - i as f64)
}

// ------------------------------------------------------------------------------------------------
// Spectra

/// Sine → triangle → saw → square.
fn basic(p: f64, k: usize) -> (f64, f64) {
    let kf = k as f64;
    let odd = k % 2 == 1;
    let sine = if k == 1 { 1.0 } else { 0.0 };
    let tri = if odd {
        let sign = if (k / 2).is_multiple_of(2) { 1.0 } else { -1.0 };
        sign * 8.0 / (PI * PI * kf * kf)
    } else {
        0.0
    };
    let saw = 2.0 / (PI * kf);
    let square = if odd { 4.0 / (PI * kf) } else { 0.0 };
    let shapes = [sine, tri, saw, square];
    let (i, t) = segment(p, 3);
    (mix(shapes[i], shapes[i + 1], t), 0.0)
}

/// From the fundamental alone to 32 harmonics, one after another.
fn harmonics(p: f64, k: usize) -> (f64, f64) {
    let n = 1.0 + p * 31.0;
    let kf = k as f64;
    let gate = (n - kf + 1.0).clamp(0.0, 1.0);
    (gate / kf.sqrt(), 0.0)
}

/// From odd harmonics only (square-like) to even ones over a weak fundamental.
fn odd_even(p: f64, k: usize) -> (f64, f64) {
    let kf = k as f64;
    let w = if k == 1 {
        1.0 - 0.7 * p
    } else if k % 2 == 1 {
        1.0 - p
    } else {
        p
    };
    (w / kf, 0.0)
}

/// Organ registrations: the drawbar harmonics 1, 2, 3, 4, 5, 6, 8, five settings in turn.
fn organ(p: f64, k: usize) -> (f64, f64) {
    const BARS: [usize; 7] = [1, 2, 3, 4, 5, 6, 8];
    const SETS: [[f64; 7]; 5] = [
        [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [1.0, 0.8, 0.0, 0.6, 0.0, 0.0, 0.0],
        [1.0, 0.8, 0.6, 0.6, 0.0, 0.0, 0.5],
        [0.8, 1.0, 0.8, 0.6, 0.4, 0.4, 0.6],
        [1.0, 1.0, 1.0, 1.0, 0.8, 0.8, 1.0],
    ];
    let Some(bar) = BARS.iter().position(|&b| b == k) else {
        return (0.0, 0.0);
    };
    let (i, t) = segment(p, SETS.len() - 1);
    (mix(SETS[i][bar], SETS[i + 1][bar], t), 0.0)
}

/// Formants over a saw-like source at `f0`: Gaussian peaks at the formant frequencies.
fn formants(
    p: f64,
    k: usize,
    f0: f64,
    sets: &[[f64; 3]],
    gains: [f64; 3],
    widths: [f64; 3],
) -> f64 {
    let (i, t) = segment(p, sets.len() - 1);
    let hz = k as f64 * f0;
    let mut a = 0.0;
    for n in 0..3 {
        let f = mix(sets[i][n], sets[i + 1][n], t);
        a += gains[n] * (-((hz - f) / widths[n]).powi(2)).exp();
    }
    a / (k as f64).powf(0.3)
}

/// A, E, I, O, U at 110 Hz.
fn vowels(p: f64, k: usize) -> (f64, f64) {
    const SETS: [[f64; 3]; 5] = [
        [800.0, 1150.0, 2900.0],
        [400.0, 1700.0, 2600.0],
        [300.0, 2200.0, 3000.0],
        [450.0, 800.0, 2830.0],
        [325.0, 700.0, 2530.0],
    ];
    let a = formants(p, k, 110.0, &SETS, [1.0, 0.6, 0.25], [110.0, 150.0, 220.0]);
    (a, 0.0)
}

/// Sung „uu“, „oo“, „aa“, „ee“ at 220 Hz with wide formants and a softer top.
fn choir(p: f64, k: usize) -> (f64, f64) {
    const SETS: [[f64; 3]; 4] = [
        [350.0, 600.0, 2400.0],
        [450.0, 800.0, 2830.0],
        [750.0, 1150.0, 2800.0],
        [400.0, 1900.0, 2600.0],
    ];
    let a = formants(p, k, 220.0, &SETS, [1.0, 0.5, 0.12], [220.0, 300.0, 400.0]);
    // Alternating phases make a rounder, less buzzy cycle.
    (a, if k.is_multiple_of(2) { PI / 2.0 } else { 0.0 })
}

/// A saw through a comb whose notches move apart.
fn comb(p: f64, k: usize) -> (f64, f64) {
    let delay = 0.04 + 0.2 * p;
    let kf = k as f64;
    ((PI * kf * delay).cos().abs() / kf, 0.0)
}

/// Narrow clusters of high harmonics that move up over a quiet fundamental.
fn glass(p: f64, k: usize) -> (f64, f64) {
    let kf = k as f64;
    let centres = [
        (1.0, 0.5, 0.6),
        (4.0 + 6.0 * p, 1.0, 0.8),
        (9.0 + 14.0 * p, 0.8, 1.2),
        (17.0 + 28.0 * p, 0.6, 1.8),
    ];
    let a: f64 = centres
        .iter()
        .map(|(c, g, w)| g * (-((kf - c) / w).powi(2)).exp())
        .sum();
    (a, (kf * 1.7).sin() * PI)
}

/// A saw whose low harmonics fade out one after another (the fundamental stays faint).
fn hollow(p: f64, k: usize) -> (f64, f64) {
    let kf = k as f64;
    let cut = 1.0 + 12.0 * p;
    let gate = if k == 1 {
        1.0 - 0.75 * p
    } else {
        (kf - cut + 1.0).clamp(0.0, 1.0)
    };
    (gate / kf, 0.0)
}

/// A reproducible hash in 0 … 1.
fn hash01(a: u64, b: u64) -> f64 {
    let mut h = a.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ b.wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// Random spectra (fixed seed) that blend into each other across six key frames.
fn random(p: f64, k: usize) -> (f64, f64) {
    let (i, t) = segment(p, 5);
    let kf = k as f64;
    let level = |key: usize| {
        let r = hash01(key as u64 + 1, k as u64);
        if r < 0.35 { 0.0 } else { r }
    };
    let a = mix(level(i), level(i + 1), t) / kf.powf(0.8);
    (a, hash01(99, k as u64) * TAU)
}

// ------------------------------------------------------------------------------------------------
// Shapes

/// A pulse from 50 % down to 3 %.
fn pulse(p: f64, x: f64) -> f64 {
    let width = 0.5 - 0.47 * p;
    if x < width { 1.0 } else { -1.0 }
}

/// A hard-synced saw: the slave runs at 1 … 8 times the master.
fn sync(p: f64, x: f64) -> f64 {
    let ratio = 1.0 + 7.0 * p;
    let y = x * ratio;
    2.0 * (y - y.floor()) - 1.0
}

/// Two-operator FM, ratio 1, index 0 … 6.
fn fm(p: f64, x: f64) -> f64 {
    (TAU * x + 6.0 * p * (TAU * x).sin()).sin()
}

/// A sine through a triangle folder, gain 1 … 6.
fn fold(p: f64, x: f64) -> f64 {
    let y = (1.0 + 5.0 * p) * (TAU * x).sin();
    (y * PI / 2.0).sin().asin() * 2.0 / PI
}

/// A sine with fewer and fewer steps per cycle and levels.
fn bits(p: f64, x: f64) -> f64 {
    let steps = (2.0f64).powf(8.0 - 5.0 * p);
    let levels = (2.0f64).powf(6.0 - 4.5 * p);
    let xq = (x * steps).floor() / steps;
    ((TAU * xq).sin() * levels).round() / levels
}

/// A resonant saw (phase-distortion style): a ringing sine whose rate climbs, fading over the
/// cycle.
fn resonance(p: f64, x: f64) -> f64 {
    let ratio = 1.0 + 15.0 * p;
    (1.0 - x) * (TAU * ratio * x).sin()
}

// ------------------------------------------------------------------------------------------------
// Preview

/// Points per cycle of the preview.
pub const PREVIEW_POINTS: usize = 128;
/// Harmonics a spectrum recipe's preview sums (enough for the eye at graph size).
const PREVIEW_HARMONICS: usize = 48;

type Preview = [[f32; PREVIEW_POINTS]; FRAMES];

/// The preview of `table`: every frame at [`PREVIEW_POINTS`] points, DC removed, scaled to a peak
/// of 1. Computed on first use and kept.
pub fn preview(table: usize) -> &'static Preview {
    static CACHE: [OnceLock<Box<Preview>>; TABLES.len()] = [const { OnceLock::new() }; 16];
    let table = table.min(TABLES.len() - 1);
    CACHE[table].get_or_init(|| {
        let recipe = recipe(table);
        let mut out = Box::new([[0.0f32; PREVIEW_POINTS]; FRAMES]);
        for (f, frame) in out.iter_mut().enumerate() {
            let p = frame_position(f);
            let mut cycle = [0.0f64; PREVIEW_POINTS];
            for (i, v) in cycle.iter_mut().enumerate() {
                let x = i as f64 / PREVIEW_POINTS as f64;
                *v = match recipe {
                    Recipe::Spectrum(spec) => (1..=PREVIEW_HARMONICS)
                        .map(|k| {
                            let (a, phase) = spec(p, k);
                            a * (TAU * k as f64 * x + phase).sin()
                        })
                        .sum(),
                    Recipe::Shape(shape) => shape(p, x),
                };
            }
            let mean = cycle.iter().sum::<f64>() / PREVIEW_POINTS as f64;
            let peak = cycle
                .iter()
                .fold(0.0f64, |m, v| m.max((v - mean).abs()))
                .max(1e-9);
            for (o, v) in frame.iter_mut().zip(cycle) {
                *o = ((v - mean) / peak) as f32;
            }
        }
        out
    })
}

/// The preview of `table` at position `p` (0 … 1, between frames linear) and phase `x` (0 … 1,
/// between points linear): −1 … +1.
pub fn preview_at(table: usize, p: f64, x: f64) -> f64 {
    let frames = preview(table);
    let fp = p.clamp(0.0, 1.0) * (FRAMES - 1) as f64;
    let f0 = (fp.floor() as usize).min(FRAMES - 2);
    let ft = fp - f0 as f64;
    let xp = x.rem_euclid(1.0) * PREVIEW_POINTS as f64;
    let x0 = (xp.floor() as usize) % PREVIEW_POINTS;
    let x1 = (x0 + 1) % PREVIEW_POINTS;
    let xt = xp - xp.floor();
    let at = |f: usize| mix(f64::from(frames[f][x0]), f64::from(frames[f][x1]), xt);
    mix(at(f0), at(f0 + 1), ft)
}

// ------------------------------------------------------------------------------------------------
// Modulation range

/// The range the matrix moves the position of oscillator `osc` (0 = A, 1 = B) over, as offsets
/// from the knob: `(lo, hi)` in position units. `slots` are `mod1_src`, `mod1_dst`, `mod1_amt` …
/// `mod4_amt`, `lfo1_uni`, `lfo2_uni` (the order of `va_curve::cutoff_mod_octaves` without
/// `at_cut`). One unit of the amount moves the position by `shape_mod`.
pub fn position_mod_range(osc: usize, slots: &[f64], shape_mod: f64) -> (f64, f64) {
    use super::va_curve::{dst, src};
    let at = |i: usize| slots.get(i).copied().unwrap_or(0.0);
    let uni = [at(12) >= 0.5, at(13) >= 0.5];
    let target = if osc == 0 { dst::PW_A } else { dst::PW_B };
    let (mut lo, mut hi) = (0.0, 0.0);
    for slot in 0..4 {
        let (source, dest, amount) = (
            at(3 * slot).round() as usize,
            at(3 * slot + 1).round() as usize,
            at(3 * slot + 2),
        );
        if source == src::OFF || dest != target {
            continue;
        }
        let span = amount * shape_mod;
        if src::is_bipolar(source, uni) {
            lo -= span.abs();
            hi += span.abs();
        } else {
            lo += span.min(0.0);
            hi += span.max(0.0);
        }
    }
    (lo, hi)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_table_previews_finite_and_full_scale() {
        for t in 0..TABLES.len() {
            for (f, frame) in preview(t).iter().enumerate() {
                let peak = frame.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!(frame.iter().all(|v| v.is_finite()), "{t}/{f}");
                assert!((peak - 1.0).abs() < 1e-3, "{t}/{f}: {peak}");
            }
        }
    }

    #[test]
    fn the_first_frame_of_the_basic_table_is_a_sine_and_the_last_a_square() {
        let sine = |x: f64| (TAU * x).sin();
        for i in 0..16 {
            let x = i as f64 / 16.0;
            assert!((preview_at(0, 0.0, x) - sine(x)).abs() < 1e-3);
        }
        assert!(preview_at(0, 1.0, 0.25) > 0.8 && preview_at(0, 1.0, 0.75) < -0.8);
    }

    #[test]
    fn frames_change_along_the_table() {
        for (t, name) in TABLES.iter().enumerate() {
            let diff: f64 = (0..PREVIEW_POINTS)
                .map(|i| {
                    let x = i as f64 / PREVIEW_POINTS as f64;
                    (preview_at(t, 0.0, x) - preview_at(t, 1.0, x)).abs()
                })
                .sum();
            assert!(diff > 1.0, "{name}: the table does not move ({diff})");
        }
    }

    #[test]
    fn the_modulation_range_follows_the_slots_to_the_position() {
        // LFO 1 (bipolar) → Position A 50 %, Env 3 → Position B 40 %, Velocity → Cutoff.
        let mut slots = vec![0.0; 14];
        slots[..9].copy_from_slice(&[1.0, 3.0, 0.5, 3.0, 4.0, 0.4, 4.0, 6.0, 1.0]);
        let (lo, hi) = position_mod_range(0, &slots, 1.0);
        assert!((lo + 0.5).abs() < 1e-12 && (hi - 0.5).abs() < 1e-12);
        let (lo, hi) = position_mod_range(1, &slots, 1.0);
        assert!(lo.abs() < 1e-12 && (hi - 0.4).abs() < 1e-12);
        slots[12] = 1.0; // LFO 1 unipolar
        assert_eq!(position_mod_range(0, &slots, 1.0), (0.0, 0.5));
    }
}
