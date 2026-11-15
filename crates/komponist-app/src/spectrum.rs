//! The live spectrum behind the EQ graph (WP-92): the UI side of a device's
//! [`SampleTap`](komponist_engine::SampleTap).
//!
//! Once per [`INTERVAL`] a [`SpectrumAnalyzer`] reads the latest window of the tap (4096 samples
//! at 44.1 and 48 kHz, 8192 at 96 kHz, so the resolution in Hz stays about the same), applies a
//! Hann window, runs a real FFT (`realfft`; the plan and every buffer are made once and reused)
//! and turns the magnitudes into dB. Those are mapped onto [`POINTS`] points that are evenly
//! spaced on the graph's logarithmic 20 Hz – 20 kHz axis, which is the layout the graph's `Data`
//! layer expects: at low frequencies, where a point is narrower than a bin, the dB values of the
//! two nearest bins are interpolated, at high frequencies the power of all bins a point covers is
//! averaged (that smooths the noisy top end). Time ballistics then take over: a rise shows at
//! once, a fall is [`FALL_DB_PER_S`].
//!
//! A full-scale sine reads 0 dB at its peak ([`DB_CEIL`]); [`DB_FLOOR`] is the bottom of the
//! graph. The analyzer only works while someone calls [`SpectrumAnalyzer::update`], and every call
//! renews the tap's lease ([`SampleTap::want`]), so a hidden EQ costs the audio thread nothing.

use std::sync::Arc;

use komponist_engine::SampleTap;
use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};

/// Lowest and highest frequency of the graph's axis.
pub const F_MIN: f32 = 20.0;
/// See [`F_MIN`].
pub const F_MAX: f32 = 20_000.0;
/// Points of the result, evenly spaced on the log axis from [`F_MIN`] to [`F_MAX`].
pub const POINTS: usize = 256;
/// The level of the bottom of the graph in dBFS.
pub const DB_FLOOR: f32 = -90.0;
/// The level of the top of the graph in dBFS (a full-scale sine).
pub const DB_CEIL: f32 = 0.0;
/// How fast a level falls, dB per second.
pub const FALL_DB_PER_S: f32 = 20.0;
/// Seconds between two analyses (about 30 per second).
pub const INTERVAL: f64 = 1.0 / 30.0;
/// A gap in the updates longer than this (seconds) means the EQ was hidden: the analyzer starts
/// over rather than show an old picture.
const STALE: f64 = 0.4;
/// FFT size at 48 kHz.
const BASE_WINDOW: usize = 4096;

/// What a device card shows of the live analysis.
#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    /// Sample rate the device runs at (the EQ curve is the filters' response at this rate).
    pub rate: f32,
    /// The spectrum as 0–1 values for the graph's `Data` layer; empty while nothing is shown
    /// (hidden, bypassed, „Analyse“ off).
    pub spectrum: Arc<[f32]>,
}

/// FFT size for `rate`: 4096 up to 48 kHz, doubling above (a power of two).
pub fn window_len(rate: f32) -> usize {
    (rate / 48_000.0 * BASE_WINDOW as f32)
        .ceil()
        .max(BASE_WINDOW as f32) as usize
}

/// The frequency in Hz of point `i` of [`POINTS`].
pub fn point_hz(i: usize) -> f32 {
    F_MIN * (F_MAX / F_MIN).powf(i as f32 / (POINTS - 1) as f32)
}

/// The index of the point closest to `hz`.
pub fn point_of(hz: f32) -> usize {
    ((hz / F_MIN).ln() / (F_MAX / F_MIN).ln() * (POINTS - 1) as f32)
        .round()
        .clamp(0.0, (POINTS - 1) as f32) as usize
}

/// Which bins make up one point.
#[derive(Debug, Clone, Copy)]
enum Span {
    /// Narrower than a bin: interpolate bins `lo` and `lo + 1` by `frac`.
    Between { lo: usize, frac: f32 },
    /// Average the power of bins `lo ..= hi`.
    Mean { lo: usize, hi: usize },
}

/// The tables that depend on the FFT size and the sample rate.
struct Plan {
    len: usize,
    rate: f32,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    spans: Vec<Span>,
    /// Scales an FFT magnitude to the amplitude of a sine (2 / sum of the window).
    scale: f32,
}

/// Analyzes one tap. Keep one per displayed device; drop it when the display goes away.
pub struct SpectrumAnalyzer {
    planner: RealFftPlanner<f32>,
    plan: Option<Plan>,
    samples: Vec<f32>,
    input: Vec<f32>,
    bins: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    power: Vec<f32>,
    /// The level of every point right now and the one it falls towards, in dB.
    shown: Vec<f32>,
    target: Vec<f32>,
    out: Arc<[f32]>,
    last_tick: Option<f64>,
    /// `written` of the tap at the last analysis, and at the start (older samples are stale).
    last_written: u64,
    started_at: u64,
}

impl core::fmt::Debug for SpectrumAnalyzer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SpectrumAnalyzer")
            .field("len", &self.plan.as_ref().map(|p| p.len))
            .finish_non_exhaustive()
    }
}

impl Default for SpectrumAnalyzer {
    fn default() -> SpectrumAnalyzer {
        SpectrumAnalyzer {
            planner: RealFftPlanner::new(),
            plan: None,
            samples: Vec::new(),
            input: Vec::new(),
            bins: Vec::new(),
            scratch: Vec::new(),
            power: Vec::new(),
            shown: vec![DB_FLOOR; POINTS],
            target: vec![DB_FLOOR; POINTS],
            out: vec![0.0; POINTS].into(),
            last_tick: None,
            last_written: 0,
            started_at: 0,
        }
    }
}

impl SpectrumAnalyzer {
    /// An analyzer showing silence.
    pub fn new() -> SpectrumAnalyzer {
        SpectrumAnalyzer::default()
    }

    /// The spectrum as 0–1 values ([`DB_FLOOR`] to [`DB_CEIL`]), [`POINTS`] of them.
    pub fn values(&self) -> &Arc<[f32]> {
        &self.out
    }

    /// Whether any point is above the floor (the UI keeps repainting while it falls).
    pub fn is_active(&self) -> bool {
        self.shown.iter().any(|db| *db > DB_FLOOR)
    }

    /// Back to silence; the next [`SpectrumAnalyzer::update`] starts afresh.
    pub fn reset(&mut self) {
        self.shown.fill(DB_FLOOR);
        self.target.fill(DB_FLOOR);
        Arc::make_mut(&mut self.out).fill(0.0);
        self.last_tick = None;
    }

    /// Call every frame the spectrum is visible, with the time in seconds. Renews the tap's lease
    /// and, at most every [`INTERVAL`], analyzes the tap's latest samples and advances the
    /// ballistics. Silence (no new samples since the last analysis, so the transport is stopped)
    /// lets the picture fall.
    pub fn update(&mut self, now: f64, tap: &SampleTap) {
        tap.want();
        let rate = tap.rate();
        if rate <= 0.0 {
            return;
        }
        if self.last_tick.is_some_and(|t| now - t > STALE) {
            self.reset();
        }
        let dt = match self.last_tick {
            None => {
                // Samples from before the display started are stale: wait for a fresh window.
                self.started_at = tap.written();
                self.last_written = self.started_at;
                0.0
            }
            Some(t) if now - t < INTERVAL * 0.9 => return,
            Some(t) => (now - t) as f32,
        };
        self.last_tick = Some(now);
        self.ensure_plan(rate);
        let len = self.plan.as_ref().map_or(0, |p| p.len);
        let written = tap.written();
        let fresh = written != self.last_written;
        self.last_written = written;
        if fresh && written >= self.started_at + len as u64 {
            self.samples.resize(len, 0.0);
            // If the audio thread overwrote the window while it was copied, keep the picture.
            if tap.read_latest(&mut self.samples).is_some() {
                self.analyze();
            }
        } else {
            self.target.fill(DB_FLOOR);
        }
        self.advance(dt);
    }

    /// Builds the FFT plan and tables for `rate` unless they exist.
    fn ensure_plan(&mut self, rate: f32) {
        if self.plan.as_ref().is_some_and(|p| p.rate == rate) {
            return;
        }
        let len = window_len(rate).next_power_of_two();
        let fft = self.planner.plan_fft_forward(len);
        let window: Vec<f32> = (0..len)
            .map(|i| 0.5 - 0.5 * (core::f32::consts::TAU * i as f32 / len as f32).cos())
            .collect();
        let scale = 2.0 / window.iter().sum::<f32>();
        let bin_hz = rate / len as f32;
        let half = (F_MAX / F_MIN).powf(0.5 / (POINTS - 1) as f32);
        let last = len / 2;
        let spans = (0..POINTS)
            .map(|i| {
                let hz = point_hz(i);
                let (lo, hi) = (hz / half / bin_hz, hz * half / bin_hz);
                if hi - lo < 1.0 {
                    let at = (hz / bin_hz).min(last as f32 - 1.0);
                    Span::Between {
                        lo: at as usize,
                        frac: at.fract(),
                    }
                } else {
                    let lo = (lo.ceil() as usize).min(last);
                    Span::Mean {
                        lo,
                        hi: (hi.floor() as usize).clamp(lo, last),
                    }
                }
            })
            .collect();
        self.input = vec![0.0; len];
        self.bins = fft.make_output_vec();
        self.scratch = fft.make_scratch_vec();
        self.power = vec![0.0; len / 2 + 1];
        self.samples = vec![0.0; len];
        self.plan = Some(Plan {
            len,
            rate,
            fft,
            window,
            spans,
            scale,
        });
    }

    /// `self.samples` into `self.target`, in dB per point.
    fn analyze(&mut self) {
        let Some(plan) = &self.plan else { return };
        for ((x, s), w) in self.input.iter_mut().zip(&self.samples).zip(&plan.window) {
            *x = if s.is_finite() { s * w } else { 0.0 };
        }
        if plan
            .fft
            .process_with_scratch(&mut self.input, &mut self.bins, &mut self.scratch)
            .is_err()
        {
            return;
        }
        for (p, c) in self.power.iter_mut().zip(&self.bins) {
            *p = c.norm_sqr() * plan.scale * plan.scale;
        }
        let db = |power: f32| (10.0 * power.max(1e-12).log10()).clamp(DB_FLOOR, DB_CEIL);
        for (t, span) in self.target.iter_mut().zip(&plan.spans) {
            *t = match *span {
                Span::Between { lo, frac } => {
                    let (a, b) = (db(self.power[lo]), db(self.power[lo + 1]));
                    a + (b - a) * frac
                }
                Span::Mean { lo, hi } => {
                    let sum: f32 = self.power[lo..=hi].iter().sum();
                    db(sum / (hi - lo + 1) as f32)
                }
            };
        }
    }

    /// Moves the shown levels towards the targets over `dt` seconds and refreshes the output.
    fn advance(&mut self, dt: f32) {
        let fall = FALL_DB_PER_S * dt;
        for (shown, target) in self.shown.iter_mut().zip(&self.target) {
            *shown = if target >= shown {
                *target
            } else {
                (*shown - fall).max(*target)
            };
        }
        let out = Arc::make_mut(&mut self.out);
        for (o, db) in out.iter_mut().zip(&self.shown) {
            *o = (db - DB_FLOOR) / (DB_CEIL - DB_FLOOR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, rate: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (core::f32::consts::TAU * hz * i as f32 / rate).sin())
            .collect()
    }

    /// A tap that has `samples` in it, with the rate set.
    fn tap_with(samples: &[f32], rate: f32) -> SampleTap {
        let tap = SampleTap::new(samples.len().next_power_of_two() * 2);
        tap.set_rate(rate);
        tap.push(samples);
        tap
    }

    /// An analyzer that has analyzed `samples` played at `rate`.
    fn analyzed(samples: &[f32], rate: f32) -> SpectrumAnalyzer {
        let mut a = SpectrumAnalyzer::new();
        a.ensure_plan(rate);
        let len = a.plan.as_ref().unwrap().len;
        a.samples = samples[samples.len() - len..].to_vec();
        a.analyze();
        a.shown.copy_from_slice(&a.target);
        a
    }

    fn peak_point(a: &SpectrumAnalyzer) -> usize {
        let (i, _) =
            a.shown.iter().enumerate().fold(
                (0, f32::MIN),
                |m, (i, v)| {
                    if *v > m.1 { (i, *v) } else { m }
                },
            );
        i
    }

    #[test]
    fn a_sine_peaks_at_its_frequency_on_the_axis_at_every_rate() {
        for rate in [44_100.0, 48_000.0, 96_000.0] {
            // Within one FFT bin or one point of the axis, whichever is wider (at 100 Hz a bin
            // is four points wide).
            let bin_hz = rate / window_len(rate) as f32;
            let step = (F_MAX / F_MIN).powf(1.0 / (POINTS - 1) as f32);
            for hz in [100.0, 1000.0, 10_000.0] {
                let a = analyzed(&sine(hz, rate, 16_384), rate);
                let got = peak_point(&a);
                let tolerance = bin_hz.max(hz * (step - 1.0)) + 0.01;
                assert!(
                    (point_hz(got) - hz).abs() <= tolerance,
                    "{hz} Hz at {rate}: point {got} = {} Hz",
                    point_hz(got)
                );
                if hz >= 1000.0 {
                    assert!(
                        got.abs_diff(point_of(hz)) <= 1,
                        "{hz} Hz at {rate}: point {got}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_full_scale_sine_reads_close_to_0_db_and_silence_sits_on_the_floor() {
        let a = analyzed(&sine(1000.0, 48_000.0, 8192), 48_000.0);
        let top = a.shown.iter().fold(f32::MIN, |m, v| m.max(*v));
        assert!((-4.0..=0.0).contains(&top), "{top}");
        // Far from the sine the Hann window's leakage is below the floor.
        assert!(a.shown[point_of(10_000.0)] < -80.0);
        let silent = analyzed(&vec![0.0; 8192], 48_000.0);
        assert!(silent.shown.iter().all(|v| *v == DB_FLOOR));
    }

    #[test]
    fn the_window_scales_with_the_rate() {
        assert_eq!(window_len(44_100.0), 4096);
        assert_eq!(window_len(48_000.0), 4096);
        assert_eq!(window_len(96_000.0), 8192);
        assert!(point_hz(0) - F_MIN < 1e-3 && (point_hz(POINTS - 1) - F_MAX).abs() < 1.0);
    }

    #[test]
    fn a_rise_shows_at_once_and_a_fall_takes_20_db_per_second() {
        let mut a = SpectrumAnalyzer::new();
        a.target.fill(-10.0);
        a.advance(0.033);
        assert!(a.shown.iter().all(|v| *v == -10.0), "attack is instant");
        assert!((a.values()[0] - 80.0 / 90.0).abs() < 1e-6);
        a.target.fill(DB_FLOOR);
        for _ in 0..30 {
            a.advance(1.0 / 30.0);
        }
        // One second later: 20 dB lower.
        assert!((a.shown[0] + 30.0).abs() < 1e-3, "{}", a.shown[0]);
        for _ in 0..30 * 5 {
            a.advance(1.0 / 30.0);
        }
        assert_eq!(a.shown[0], DB_FLOOR);
        assert!(!a.is_active());
    }

    #[test]
    fn update_follows_the_tap_waits_for_fresh_samples_and_falls_when_they_stop() {
        let rate = 48_000.0;
        let tap = SampleTap::new(16_384);
        tap.set_rate(rate);
        // Old samples from before the display started do not count.
        tap.push(&sine(1000.0, rate, 8192));
        let mut a = SpectrumAnalyzer::new();
        a.update(0.0, &tap);
        assert!(tap.is_wanted(), "the update renews the lease");
        assert!(!a.is_active(), "stale samples are not shown");
        // 4096 fresh samples arrive, 1 kHz.
        tap.push(&sine(1000.0, rate, 4096));
        a.update(0.04, &tap);
        assert!(a.is_active());
        assert!(peak_point(&a).abs_diff(point_of(1000.0)) <= 1);
        // Calls closer than an interval do nothing.
        let before = a.values().to_vec();
        a.update(0.05, &tap);
        assert_eq!(**a.values(), *before);
        // Nothing new arrives (stopped transport): the picture falls by 20 dB/s.
        a.update(0.08, &tap);
        let top = |a: &SpectrumAnalyzer| a.shown.iter().fold(f32::MIN, |m, v| m.max(*v));
        let high = top(&a);
        for k in 1..=15 {
            a.update(0.08 + k as f64 / 30.0, &tap);
        }
        assert!((high - top(&a) - 10.0).abs() < 0.5, "{high} -> {}", top(&a));
        // After a long gap (the EQ was hidden) it starts over.
        a.update(10.0, &tap);
        assert!(!a.is_active());
    }

    #[test]
    fn broken_samples_do_not_poison_the_picture() {
        let mut s = sine(1000.0, 48_000.0, 4096);
        s[10] = f32::NAN;
        s[11] = f32::INFINITY;
        let a = analyzed(&s, 48_000.0);
        assert!(a.shown.iter().all(|v| v.is_finite()));
        let tap = tap_with(&s, 48_000.0);
        assert_eq!(tap.written(), 4096);
    }
    /// Timing, not a check: `cargo test --release -p komponist-app --lib spectrum::tests::cost
    /// -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing: run in release with --ignored --nocapture"]
    fn cost_of_one_analysis() {
        let rate = 48_000.0;
        let tap = SampleTap::new(16_384);
        tap.set_rate(rate);
        let mut a = SpectrumAnalyzer::new();
        a.update(0.0, &tap);
        let signal = sine(1000.0, rate, 4096);
        let runs = 2000;
        let mut now = 0.0;
        let started = std::time::Instant::now();
        for _ in 0..runs {
            tap.push(&signal);
            now += INTERVAL;
            a.update(now, &tap);
        }
        println!(
            "one analysis (tap read + window + FFT 4096 + mapping + ballistics): {:?}",
            started.elapsed() / runs
        );
    }
}
