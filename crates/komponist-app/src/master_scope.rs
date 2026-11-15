//! The master display of the transport bar (#34): the UI side of the engine's master tap
//! ([`komponist_engine::Telemetry::master_tap`], the mono sum of the master output).
//!
//! Two pictures, switched by a click and remembered in the settings ([`MasterDisplay`]):
//!
//! - **Oscilloscope:** the latest [`m::SCOPE_WINDOW_S`] of the signal. The window starts at a
//!   rising zero crossing, the steepest one in the window before it (a periodic signal has that
//!   crossing once per period at the same place), with sub-sample precision, so a periodic signal
//!   stands still. Without a crossing the latest window runs free. The samples are resampled to
//!   [`WAVE_POINTS`] values in −1 … 1 (full scale fills the height).
//! - **Spectrum:** the EQ's analysis ([`SpectrumAnalyzer`]: log axis 20 Hz – 20 kHz, dB, instant
//!   rise and a fall of [`crate::spectrum::FALL_DB_PER_S`]), shown from [`m::SCOPE_DB_FLOOR`] to
//!   0 dBFS.
//!
//! **Silence** is a picture of its own ([`Picture::Silent`], the flat `text.4` line): no fresh
//! samples for [`HOLD`] (stopped, no device, an export has the engine) or a signal that stays
//! below the display's floor. Every [`MasterScope::update`] renews the tap's lease, so the engine
//! copies samples only while the display is drawn.

use std::sync::Arc;

use komponist_engine::SampleTap;
use serde::{Deserialize, Serialize};

use crate::metrics::transport as m;
use crate::spectrum::{self, SpectrumAnalyzer};

/// Values of the oscilloscope picture: enough for 82 px at 2× (164 device pixels).
pub const WAVE_POINTS: usize = 256;
/// Seconds without fresh samples after which the display shows silence. Longer than the largest
/// device buffer (4096 frames at 44.1 kHz are 93 ms), so a frame between two callbacks keeps the
/// picture.
pub const HOLD: f64 = 0.2;
/// A gap in the updates longer than this (seconds) means the display was hidden: it starts over
/// rather than show old samples.
const STALE: f64 = 0.4;

/// Which picture the master display shows; saved in the settings (`"oscilloscope"`,
/// `"spectrum"`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MasterDisplay {
    /// The waveform.
    #[default]
    Oscilloscope,
    /// The frequency spectrum.
    Spectrum,
}

/// An unknown value means [`MasterDisplay::Oscilloscope`]; it must not fail the settings file.
impl<'de> Deserialize<'de> for MasterDisplay {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<MasterDisplay, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(match value.as_str() {
            Some("spectrum") => MasterDisplay::Spectrum,
            _ => MasterDisplay::Oscilloscope,
        })
    }
}

impl MasterDisplay {
    /// The other picture (what a click switches to).
    pub fn toggled(self) -> MasterDisplay {
        match self {
            MasterDisplay::Oscilloscope => MasterDisplay::Spectrum,
            MasterDisplay::Spectrum => MasterDisplay::Oscilloscope,
        }
    }
}

/// What the display draws this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Picture<'a> {
    /// No signal: the flat line.
    Silent,
    /// The oscilloscope: [`WAVE_POINTS`] values in −1 … 1, oldest first.
    Wave(&'a [f32]),
    /// The spectrum: [`spectrum::POINTS`] values in 0 … 1 ([`m::SCOPE_DB_FLOOR`] … 0 dBFS),
    /// evenly spaced on the log axis from [`spectrum::F_MIN`] to [`spectrum::F_MAX`].
    Spectrum(&'a [f32]),
}

/// The state behind the master display. One lives in [`crate::meters::Meters`].
#[derive(Debug)]
pub struct MasterScope {
    tap: Option<Arc<SampleTap>>,
    analyzer: SpectrumAnalyzer,
    /// The picture the state was made for (a switch starts over).
    shown: Option<MasterDisplay>,
    /// The window read from the tap, the waveform and the spectrum in display units.
    samples: Vec<f32>,
    wave: Vec<f32>,
    spectrum: Vec<f32>,
    /// Whether the waveform and the spectrum hold a signal.
    wave_live: bool,
    spectrum_live: bool,
    last_tick: Option<f64>,
    /// When fresh samples last came in, and the tap's `written` then.
    last_fresh: f64,
    last_written: u64,
    /// `written` when the display started (older samples are stale).
    started_at: u64,
}

impl Default for MasterScope {
    fn default() -> MasterScope {
        MasterScope {
            tap: None,
            analyzer: SpectrumAnalyzer::new(),
            shown: None,
            samples: Vec::new(),
            wave: vec![0.0; WAVE_POINTS],
            spectrum: vec![0.0; spectrum::POINTS],
            wave_live: false,
            spectrum_live: false,
            last_tick: None,
            last_fresh: 0.0,
            last_written: 0,
            started_at: 0,
        }
    }
}

impl MasterScope {
    /// A display without a tap (silent until [`MasterScope::set_tap`]).
    pub fn new() -> MasterScope {
        MasterScope::default()
    }

    /// Uses `tap` (the engine's master tap; tests pass their own). A different tap starts over.
    pub fn set_tap(&mut self, tap: &Arc<SampleTap>) {
        if self.tap.as_ref().is_none_or(|old| !Arc::ptr_eq(old, tap)) {
            self.tap = Some(Arc::clone(tap));
            self.restart();
        }
    }

    /// Whether the picture shows a signal (the UI keeps repainting while it does).
    pub fn is_active(&self) -> bool {
        match self.shown {
            Some(MasterDisplay::Oscilloscope) => self.wave_live,
            Some(MasterDisplay::Spectrum) => self.spectrum_live,
            None => false,
        }
    }

    fn restart(&mut self) {
        self.analyzer.reset();
        self.wave_live = false;
        self.spectrum_live = false;
        self.spectrum.fill(0.0);
        self.last_tick = None;
    }

    /// Call every frame the display is drawn, with the time in seconds: renews the tap's lease,
    /// reads it and returns the picture for `mode`.
    pub fn update(&mut self, now: f64, mode: MasterDisplay) -> Picture<'_> {
        let Some(tap) = self.tap.clone() else {
            return Picture::Silent;
        };
        if self.shown != Some(mode) || self.last_tick.is_some_and(|t| now - t > STALE) {
            self.shown = Some(mode);
            self.restart();
        }
        tap.want();
        let rate = tap.rate();
        if self.last_tick.is_none() {
            // Samples from before the display started are stale: wait for fresh ones.
            self.started_at = tap.written();
            self.last_written = self.started_at;
            self.last_fresh = now;
        }
        self.last_tick = Some(now);
        match mode {
            MasterDisplay::Oscilloscope => {
                if rate > 0.0 {
                    self.update_wave(now, &tap, rate);
                }
                if self.wave_live {
                    Picture::Wave(&self.wave)
                } else {
                    Picture::Silent
                }
            }
            MasterDisplay::Spectrum => {
                self.analyzer.update(now, &tap);
                let floor = spectrum::DB_FLOOR;
                let span = spectrum::DB_CEIL - floor;
                let mut any = false;
                for (out, v) in self.spectrum.iter_mut().zip(self.analyzer.values().iter()) {
                    let db = floor + v * span;
                    *out = ((db - m::SCOPE_DB_FLOOR) / -m::SCOPE_DB_FLOOR).clamp(0.0, 1.0);
                    any |= *out > 0.0;
                }
                self.spectrum_live = any;
                if any {
                    Picture::Spectrum(&self.spectrum)
                } else {
                    Picture::Silent
                }
            }
        }
    }

    /// Reads the latest two windows, finds the trigger and resamples the window after it.
    fn update_wave(&mut self, now: f64, tap: &SampleTap, rate: f32) {
        let written = tap.written();
        if written == self.last_written {
            if now - self.last_fresh > HOLD {
                self.wave_live = false;
            }
            return;
        }
        let window = ((rate * m::SCOPE_WINDOW_S).round() as usize).clamp(2, tap.capacity() / 4);
        let len = 2 * window;
        if written < self.started_at + len as u64 {
            return;
        }
        self.samples.resize(len, 0.0);
        // If the audio thread overwrote the window while it was copied, keep the picture.
        if tap.read_latest(&mut self.samples).is_none() {
            return;
        }
        self.last_written = written;
        self.last_fresh = now;
        for s in &mut self.samples {
            if !s.is_finite() {
                *s = 0.0;
            }
        }
        let floor = 10f32.powf(m::SCOPE_DB_FLOOR / 20.0);
        if self.samples.iter().all(|s| s.abs() < floor) {
            self.wave_live = false;
            return;
        }
        let start = trigger(&self.samples[..=window]).unwrap_or(window as f32);
        let step = (window - 1) as f32 / (WAVE_POINTS - 1) as f32;
        for (k, out) in self.wave.iter_mut().enumerate() {
            let at = start + k as f32 * step;
            let i = (at as usize).min(len - 2);
            let frac = at - i as f32;
            let (a, b) = (self.samples[i], self.samples[i + 1]);
            *out = (a + (b - a) * frac).clamp(-1.0, 1.0);
        }
        self.wave_live = true;
    }
}

/// The position (in samples, with fraction) of the steepest rising zero crossing in `samples`;
/// of equally steep ones the latest. `None` without a crossing.
pub fn trigger(samples: &[f32]) -> Option<f32> {
    let (mut steepest, mut at) = (0.0_f32, None);
    for (i, pair) in samples.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        if a < 0.0 && b >= 0.0 {
            let slope = b - a;
            // Equal up to rounding: the same crossing one period later.
            if slope >= steepest * (1.0 - 1e-3) {
                at = Some(i as f32 - a / slope);
            }
            steepest = steepest.max(slope);
        }
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.0;

    fn sine(hz: f32, phase: usize, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| 0.8 * (core::f32::consts::TAU * hz * (phase + i) as f32 / RATE).sin())
            .collect()
    }

    fn tap() -> Arc<SampleTap> {
        let tap = Arc::new(SampleTap::new(komponist_engine::tap::DEFAULT_CAPACITY));
        tap.set_rate(RATE);
        tap
    }

    #[test]
    fn the_trigger_finds_the_rising_crossing_with_sub_sample_precision() {
        // −1 → 3 crosses at a quarter of the step.
        assert_eq!(trigger(&[-1.0, 3.0]), Some(0.25));
        assert_eq!(trigger(&[1.0, -1.0, -0.5]), None, "falling only");
        // Two equal crossings: the later one.
        assert_eq!(trigger(&[-1.0, 1.0, -1.0, 1.0]), Some(2.5));
        // The steeper one wins over a later, shallow one.
        assert_eq!(trigger(&[-1.0, 1.0, -0.1, 0.1]), Some(0.5));
    }

    #[test]
    fn a_sine_stands_still_whatever_its_phase() {
        let tap = tap();
        let mut scope = MasterScope::new();
        scope.set_tap(&tap);
        let mode = MasterDisplay::Oscilloscope;
        assert_eq!(scope.update(0.0, mode), Picture::Silent);
        let mut pictures = Vec::new();
        // 440 Hz in blocks of 3100 samples (more than two windows): every frame sees another
        // phase.
        let mut phase = 0;
        for frame in 1..12 {
            let block = sine(440.0, phase, 3100);
            phase += block.len();
            tap.push(&block);
            match scope.update(frame as f64 / 60.0, mode) {
                Picture::Wave(w) => pictures.push(w.to_vec()),
                other => panic!("frame {frame}: {other:?}"),
            }
        }
        for p in &pictures[1..] {
            for (a, b) in p.iter().zip(&pictures[0]) {
                assert!((a - b).abs() < 0.01, "{a} vs {b}");
            }
        }
        // It starts on the rising crossing.
        assert!(pictures[0][0].abs() < 0.01 && pictures[0][1] > 0.0);
        assert!(scope.is_active());
        assert!(tap.is_wanted(), "the update renews the lease");
    }

    #[test]
    fn silence_and_stopped_audio_give_the_flat_line() {
        let tap = tap();
        let mut scope = MasterScope::new();
        scope.set_tap(&tap);
        let mode = MasterDisplay::Oscilloscope;
        scope.update(0.0, mode);
        tap.push(&sine(100.0, 0, 4096));
        assert!(matches!(scope.update(0.02, mode), Picture::Wave(_)));
        // No new samples for a moment: the picture holds (a large buffer between two callbacks).
        assert!(matches!(scope.update(0.1, mode), Picture::Wave(_)));
        // Longer: stopped.
        assert_eq!(scope.update(0.3, mode), Picture::Silent);
        assert!(!scope.is_active());
        // Samples below the floor (−72 dBFS) count as silence.
        tap.push(&vec![1e-5; 4096]);
        assert_eq!(scope.update(0.32, mode), Picture::Silent);
    }

    #[test]
    fn the_spectrum_peaks_at_the_sines_frequency_and_switching_starts_over() {
        let tap = tap();
        let mut scope = MasterScope::new();
        scope.set_tap(&tap);
        let mode = MasterDisplay::Spectrum;
        assert_eq!(scope.update(0.0, mode), Picture::Silent);
        tap.push(&sine(1000.0, 0, 8192));
        let Picture::Spectrum(values) = scope.update(0.05, mode) else {
            panic!("no spectrum");
        };
        let peak = (0..values.len())
            .max_by(|a, b| values[*a].total_cmp(&values[*b]))
            .unwrap();
        assert!(peak.abs_diff(spectrum::point_of(1000.0)) <= 1, "{peak}");
        assert!(values[peak] > 0.9, "a −2 dBFS sine is near the top");
        assert!(
            values[spectrum::point_of(20.0)] == 0.0,
            "far away: the floor"
        );
        // The other picture starts over: stale samples are not shown.
        assert_eq!(
            scope.update(0.07, MasterDisplay::Oscilloscope),
            Picture::Silent
        );
    }

    #[test]
    fn the_mode_reads_leniently_from_the_settings() {
        let read = |s: &str| serde_json::from_str::<MasterDisplay>(s).unwrap();
        assert_eq!(read("\"spectrum\""), MasterDisplay::Spectrum);
        assert_eq!(read("\"oscilloscope\""), MasterDisplay::Oscilloscope);
        assert_eq!(read("\"vectorscope\""), MasterDisplay::Oscilloscope);
        assert_eq!(read("3"), MasterDisplay::Oscilloscope);
        assert_eq!(
            serde_json::to_string(&MasterDisplay::Spectrum).unwrap(),
            "\"spectrum\""
        );
        assert_eq!(
            MasterDisplay::Spectrum.toggled(),
            MasterDisplay::Oscilloscope
        );
    }
}
