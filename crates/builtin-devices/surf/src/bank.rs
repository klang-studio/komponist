//! The factory wavetables, band-limited: built once per process from the recipes in
//! [`komponist_device_dsp::wavetable`] and shared by every surf.
//!
//! Every frame of every table exists in [`LEVELS`] versions. Level `L` holds the harmonics up to
//! `512 >> L` in `max(4 · (512 >> L), 256)` samples (2048 … 256: at least eight samples per period
//! of the highest harmonic, so the images of the linear read stay low), plus one guard sample (a copy of
//! the first) so a linear read never wraps. A voice reads the two levels whose highest harmonic
//! stays below half the Nyquist frequency and crossfades between them by pitch
//! ([`Mip::for_increment`]), so nothing aliases and a glide does not step.
//!
//! Each frame is scaled to the RMS of a sine of amplitude 0.85 (the position does not change the
//! loudness), unless a peak of one of its levels would pass 1; a spiky frame is held at peak 1
//! instead.
//!
//! Size: 16 tables × 32 frames × 5376 samples × 4 bytes ≈ 11 MB. Building takes the FFTs of
//! `realfft` (MIT, already a workspace dependency); it runs off the audio thread, the first time a
//! surf is created ([`bank`]).

use std::sync::OnceLock;

use komponist_device_dsp::wavetable::{
    FRAMES, MAX_HARMONIC, Recipe, TABLES, frame_position, recipe,
};
use realfft::RealFftPlanner;
use realfft::num_complex::Complex;

/// Band-limited versions of a frame.
pub const LEVELS: usize = 10;
/// Samples of a shape recipe before its FFT (oversampled, so its own aliasing stays out of the
/// harmonics kept).
const SHAPE_POINTS: usize = 8192;
/// RMS every frame is scaled to: a sine of amplitude 0.85.
const TARGET_RMS: f64 = 0.85 * std::f64::consts::FRAC_1_SQRT_2;

/// The highest harmonic of level `level`.
pub const fn harmonics(level: usize) -> usize {
    MAX_HARMONIC >> level
}

/// log2 of the samples of level `level` (without the guard sample).
pub const fn level_bits(level: usize) -> u32 {
    let n = harmonics(level) * 4;
    let n = if n < 256 { 256 } else { n };
    n.trailing_zeros()
}

/// One table: `frames[frame][level]`, each `2^level_bits + 1` samples.
pub struct Table {
    frames: Vec<[Box<[f32]>; LEVELS]>,
}

impl Table {
    /// Frame `frame` at level `level` (guard sample included).
    pub fn frame(&self, frame: usize, level: usize) -> &[f32] {
        &self.frames[frame.min(FRAMES - 1)][level.min(LEVELS - 1)]
    }
}

/// All factory tables.
pub struct Bank {
    tables: Vec<Table>,
}

impl Bank {
    /// Table `table` (out of range: the last).
    pub fn table(&self, table: usize) -> &Table {
        &self.tables[table.min(self.tables.len() - 1)]
    }

    /// Builds every table.
    pub fn build() -> Bank {
        let mut planner = RealFftPlanner::<f64>::new();
        Bank {
            tables: (0..TABLES.len())
                .map(|t| build_table(recipe(t), &mut planner))
                .collect(),
        }
    }
}

static BANK: OnceLock<Bank> = OnceLock::new();

/// The bank, built on the first call (not on the audio thread: the factory of the DSP calls it).
pub fn bank() -> &'static Bank {
    BANK.get_or_init(Bank::build)
}

/// The bank if it has been built: what the audio thread uses (an atomic load, no lock).
pub fn built() -> Option<&'static Bank> {
    BANK.get()
}

/// The spectrum of one frame: bins 0 … [`MAX_HARMONIC`], in the convention of an inverse real
/// FFT whose output at `n` is `Σ 2·Re(X_k · e^{2πikn/N})`.
fn spectrum(recipe: Recipe, p: f64, planner: &mut RealFftPlanner<f64>) -> Vec<Complex<f64>> {
    let mut bins = vec![Complex::new(0.0, 0.0); MAX_HARMONIC + 1];
    match recipe {
        Recipe::Spectrum(f) => {
            for (k, bin) in bins.iter_mut().enumerate().skip(1) {
                let (a, phase) = f(p, k);
                // a·sin(θ + φ) = 2·Re(X·e^{iθ}) with X = a/2 · (sin φ − i·cos φ).
                *bin = Complex::new(phase.sin(), -phase.cos()) * (a * 0.5);
            }
        }
        Recipe::Shape(f) => {
            let fft = planner.plan_fft_forward(SHAPE_POINTS);
            let mut input: Vec<f64> = (0..SHAPE_POINTS)
                .map(|i| f(p, i as f64 / SHAPE_POINTS as f64))
                .collect();
            let mut out = fft.make_output_vec();
            fft.process(&mut input, &mut out).expect("forward FFT");
            let scale = 1.0 / SHAPE_POINTS as f64;
            for (k, bin) in bins.iter_mut().enumerate().skip(1) {
                *bin = out[k] * scale;
            }
        }
    }
    bins
}

fn build_table(recipe: Recipe, planner: &mut RealFftPlanner<f64>) -> Table {
    let frames = (0..FRAMES)
        .map(|f| {
            let bins = spectrum(recipe, frame_position(f), planner);
            let levels: Vec<Vec<f64>> = (0..LEVELS)
                .map(|l| render_level(&bins, l, planner))
                .collect();
            // Loudness from the spectrum (Parseval: a sine of amplitude a has RMS a/√2 = √2·|X|),
            // peak from every level (fewer harmonics can ring higher).
            let rms = bins
                .iter()
                .skip(1)
                .map(|b| 2.0 * b.norm_sqr())
                .sum::<f64>()
                .sqrt();
            let peak = levels.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
            let gain = if rms > 1e-9 {
                (TARGET_RMS / rms).min(1.0 / peak.max(1e-9))
            } else {
                0.0
            };
            let level = |l: usize| -> Box<[f32]> {
                let samples = &levels[l];
                let mut out: Vec<f32> = samples.iter().map(|v| (v * gain) as f32).collect();
                out.push(out[0]);
                out.into_boxed_slice()
            };
            std::array::from_fn(level)
        })
        .collect();
    Table { frames }
}

/// One frame at level `level`: the bins up to its highest harmonic, inverse FFT.
fn render_level(
    bins: &[Complex<f64>],
    level: usize,
    planner: &mut RealFftPlanner<f64>,
) -> Vec<f64> {
    let n = 1usize << level_bits(level);
    let ifft = planner.plan_fft_inverse(n);
    let mut input = ifft.make_input_vec();
    let top = harmonics(level).min(n / 2 - 1);
    input[1..=top].copy_from_slice(&bins[1..=top]);
    let mut out = ifft.make_output_vec();
    ifft.process(&mut input, &mut out).expect("inverse FFT");
    // realfft sums both halves of the spectrum: the output is Σ 2·Re(X_k·e^{iθ}) already.
    out
}

/// The two levels a voice reads at increment `inc` (`klang_dsp::phase`: 2^32 per period) and the
/// weight of the second: level `a` keeps its highest harmonic under a quarter of the sample rate
/// at the top of its range, level `a + 1` is one octave darker.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mip {
    /// The brighter level.
    pub a: usize,
    /// The darker level.
    pub b: usize,
    /// Weight of `b`, 0 … 1.
    pub w: f32,
}

impl Mip {
    /// The levels for increment `inc`.
    pub fn for_increment(inc: u32) -> Mip {
        // Level L is free of aliasing while harmonics(L) · f ≤ 0.5, i.e. L ≥ log2(1024 · f).
        let f = inc.max(1) as f32 * (1.0 / 4_294_967_296.0);
        let x = (2.0 * MAX_HARMONIC as f32 * f).log2();
        let floor = x.floor();
        let (a, w) = if x < -1.0 {
            (0, 0.0)
        } else if x < 0.0 {
            (0, x + 1.0)
        } else {
            (floor as usize + 1, x - floor)
        };
        if a >= LEVELS - 1 {
            return Mip {
                a: LEVELS - 1,
                b: LEVELS - 1,
                w: 0.0,
            };
        }
        Mip { a, b: a + 1, w }
    }
}
