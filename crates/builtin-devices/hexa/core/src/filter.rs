//! Voice filter: soft-clipping drive followed by a state-variable filter.
//!
//! The patch has [`FILTERS`] filter settings (shown as one card each); every
//! voice runs its own instance with its own state, so envelopes, keytracking
//! and velocity act per note. Several filters run in series.
//!
//! The filter is the state-variable filter from [`klang_dsp::svf`]: low-,
//! band-, high-pass and notch from one structure, stable under fast cutoff
//! modulation. 24 dB/octave cascades two stages.

use klang_dsp::svf::{BUTTERWORTH_K, Svf};

use crate::modulation::{ModTarget, Offsets};
use crate::smooth::chase;

pub(crate) use klang_dsp::svf::{SvfCoefs as Coefs, warp};

/// Number of filters per voice (in series).
pub const FILTERS: usize = 1;

/// Cutoff range in Hz.
pub const MIN_CUTOFF: f32 = 20.0;
pub const MAX_CUTOFF: f32 = 20_000.0;

/// Largest cutoff modulation depth in semitones (8 octaves).
pub const MAX_CUTOFF_DEPTH: f32 = 96.0;

/// Keytracking range: -1 (-100 %) to 2 (+200 %) octaves per octave.
pub const MIN_KEYTRACK: f32 = -1.0;
pub const MAX_KEYTRACK: f32 = 2.0;

/// Note at which keytracking leaves the cutoff unchanged (C4).
pub const KEYTRACK_CENTER: f32 = 60.0;

/// Resonance 0 → Q 0.707 (no peak), 1 → Q 25 (self-oscillation edge).
const MIN_Q: f32 = core::f32::consts::FRAC_1_SQRT_2;
const MAX_Q: f32 = 25.0;

/// Drive 1 → 24 dB into the soft clipper.
const MAX_DRIVE_GAIN: f32 = 16.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum FilterKind {
    /// Bypassed, including the drive.
    #[default]
    Off,
    LowPass,
    BandPass,
    HighPass,
    Notch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "snake_case"))]
pub enum FilterSlope {
    /// One 2-pole stage.
    #[default]
    Db12,
    /// Two stages; the resonance applies to the second.
    Db24,
}

/// Patch parameters of one filter.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct FilterParams {
    pub kind: FilterKind,
    pub slope: FilterSlope,
    /// Hz, [`MIN_CUTOFF`]..=[`MAX_CUTOFF`].
    pub cutoff: f32,
    /// 0..=1, see `MIN_Q`/`MAX_Q`.
    pub resonance: f32,
    /// 0..=1: soft saturation before the filter, up to 24 dB of gain.
    pub drive: f32,
    /// How far the cutoff follows the note: 1.0 = one octave per octave,
    /// relative to [`KEYTRACK_CENTER`].
    pub keytrack: f32,
}

impl FilterParams {
    pub const DEFAULT: Self = Self {
        kind: FilterKind::Off,
        slope: FilterSlope::Db12,
        cutoff: 5_000.0,
        resonance: 0.0,
        drive: 0.0,
        keytrack: 0.0,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            cutoff: crate::clamp_or(self.cutoff, MIN_CUTOFF, MAX_CUTOFF, d.cutoff),
            resonance: crate::clamp_or(self.resonance, 0.0, 1.0, d.resonance),
            drive: crate::clamp_or(self.drive, 0.0, 1.0, d.drive),
            keytrack: crate::clamp_or(self.keytrack, MIN_KEYTRACK, MAX_KEYTRACK, d.keytrack),
            ..self
        }
    }
}

impl Default for FilterParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Damping k = 1/Q for a resonance of 0..=1 (exponential in Q).
pub(crate) fn damping(resonance: f32) -> f32 {
    1.0 / (MIN_Q * libm::powf(MAX_Q / MIN_Q, resonance.clamp(0.0, 1.0)))
}

/// Pre-gain for a drive of 0..=1.
pub(crate) fn drive_gain(drive: f32) -> f32 {
    1.0 + (MAX_DRIVE_GAIN - 1.0) * drive.clamp(0.0, 1.0)
}

/// Cheap tanh-like soft clipper: odd, unity slope at 0, ±1 beyond ±3.
#[inline]
pub(crate) fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// Runs one 2-pole stage on one sample.
#[inline]
fn stage(svf: &mut Svf, v0: f32, c: &Coefs, kind: FilterKind) -> f32 {
    let out = svf.tick(v0, c);
    match kind {
        FilterKind::Off => v0,
        FilterKind::LowPass => out.low(),
        // Normalized to unity gain at the centre frequency.
        FilterKind::BandPass => out.band_normalized(),
        FilterKind::HighPass => out.high(),
        FilterKind::Notch => out.notch(),
    }
}

/// Per-voice state of one filter: two stages for 24 dB.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct FilterState {
    pub(crate) stages: [Svf; 2],
}

/// State magnitude below which a filter without input counts as silent
/// (about -100 dB).
pub(crate) const SILENT_STATE: f32 = 1e-5;

impl FilterState {
    /// Whether the filter still rings, see [`SILENT_STATE`].
    pub(crate) fn ringing(&self) -> bool {
        self.stages.iter().any(|s| s.magnitude() > SILENT_STATE)
    }
}

/// Filter parameters that glide on patch changes; the cutoff in octaves.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FilterSmooth {
    log2_cutoff: f32,
    resonance: f32,
    drive: f32,
}

impl FilterSmooth {
    pub(crate) fn new(params: &FilterParams) -> Self {
        Self {
            log2_cutoff: libm::log2f(params.cutoff),
            resonance: params.resonance,
            drive: params.drive,
        }
    }

    pub(crate) fn chase(&mut self, params: &FilterParams, amount: f32) {
        let target = Self::new(params);
        chase(&mut self.log2_cutoff, target.log2_cutoff, amount);
        chase(&mut self.resonance, target.resonance, amount);
        chase(&mut self.drive, target.drive, amount);
    }
}

/// One filter of one voice for one block: coefficients ramped per sample.
pub(crate) struct FilterRun {
    pub(crate) kind: FilterKind,
    slope: FilterSlope,
    /// The resonant stage and, for 24 dB, the Butterworth stage before it,
    /// with their per-sample steps.
    resonant: Coefs,
    resonant_step: Coefs,
    butterworth: Coefs,
    butterworth_step: Coefs,
    /// Soft-clip pre-gain and the factor keeping full scale at full scale;
    /// `drive_gain` 0 = no drive. `drive_wet` < 1 blends in the dry signal
    /// near drive 0 (see [`DRIVE_FADE`]).
    drive_gain: f32,
    drive_norm: f32,
    drive_wet: f32,
}

/// Drive up to this fades from the dry signal into the driven one, so the
/// level does not jump when drive (or its modulation) leaves 0: the
/// normalization lifts small signals by up to 2.2 dB.
const DRIVE_FADE: f32 = 0.1;

impl FilterRun {
    /// Filter `index` with `params`, the smoothed values at the block's
    /// start and end and the modulation offsets there.
    pub(crate) fn new(
        index: usize,
        params: &FilterParams,
        (from, to): (&FilterSmooth, &FilterSmooth),
        (o0, o1): (&Offsets, &Offsets),
        note: f32,
        rate: f32,
        inv_n: f32,
    ) -> Self {
        if params.kind == FilterKind::Off {
            let none = Coefs::default();
            return Self {
                kind: FilterKind::Off,
                slope: params.slope,
                resonant: none,
                resonant_step: none,
                butterworth: none,
                butterworth_step: none,
                drive_gain: 0.0,
                drive_norm: 0.0,
                drive_wet: 0.0,
            };
        }
        let key = params.keytrack * (note - KEYTRACK_CENTER) / 12.0;
        let g_at = |smooth: &FilterSmooth, o: &Offsets| {
            let octaves = smooth.log2_cutoff + key + o.get(ModTarget::Cutoff(index)) / 12.0;
            warp(libm::exp2f(octaves), rate)
        };
        let k_at = |smooth: &FilterSmooth, o: &Offsets| {
            damping(smooth.resonance + o.get(ModTarget::Resonance(index)))
        };
        let (g0, g1) = (g_at(from, o0), g_at(to, o1));
        let resonant = Coefs::new(g0, k_at(from, o0));
        let resonant_step = resonant.step_to(&Coefs::new(g1, k_at(to, o1)), inv_n);
        let (butterworth, butterworth_step) = if params.slope == FilterSlope::Db24 {
            let start = Coefs::new(g0, BUTTERWORTH_K);
            (start, start.step_to(&Coefs::new(g1, BUTTERWORTH_K), inv_n))
        } else {
            (Coefs::default(), Coefs::default())
        };
        let drive = (from.drive + o0.get(ModTarget::Drive(index))).clamp(0.0, 1.0);
        let (drive_gain, drive_norm) = if drive > 0.0 {
            let gain = drive_gain(drive);
            (gain, 1.0 / soft_clip(gain))
        } else {
            (0.0, 0.0)
        };
        let drive_wet = (drive / DRIVE_FADE).min(1.0);
        Self {
            kind: params.kind,
            slope: params.slope,
            resonant,
            resonant_step,
            butterworth,
            butterworth_step,
            drive_gain,
            drive_norm,
            drive_wet,
        }
    }

    /// Filters one sample with the current coefficients; `advance` moves
    /// them on. Split so both channels of a stereo voice use the same ramp.
    #[inline]
    pub(crate) fn filter(&self, state: &mut FilterState, x: f32) -> f32 {
        if self.kind == FilterKind::Off {
            return x;
        }
        let mut v = x;
        if self.drive_gain > 0.0 {
            let driven = soft_clip(v * self.drive_gain) * self.drive_norm;
            v = if self.drive_wet < 1.0 {
                v + (driven - v) * self.drive_wet
            } else {
                driven
            };
        }
        v = match self.slope {
            FilterSlope::Db12 => stage(&mut state.stages[0], v, &self.resonant, self.kind),
            // The first stage without resonance (Butterworth), the second with.
            FilterSlope::Db24 => {
                let y = stage(&mut state.stages[0], v, &self.butterworth, self.kind);
                stage(&mut state.stages[1], y, &self.resonant, self.kind)
            }
        };
        v
    }

    #[inline]
    pub(crate) fn advance(&mut self) {
        self.resonant.advance(&self.resonant_step);
        if self.slope == FilterSlope::Db24 {
            self.butterworth.advance(&self.butterworth_step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 96_000.0;

    /// Steady-state gain (dB) of a filter for a sine at `freq`.
    fn gain_db(
        kind: FilterKind,
        slope: FilterSlope,
        cutoff: f32,
        resonance: f32,
        freq: f32,
    ) -> f32 {
        let resonant = Coefs::new(warp(cutoff, RATE), damping(resonance));
        let butterworth = Coefs::new(warp(cutoff, RATE), damping(0.0));
        let mut state = FilterState::default();
        let n = (RATE * 0.5) as usize;
        let mut peak = 0.0f32;
        for i in 0..n {
            // Phase kept within one period: f32 sines of large arguments are noisy.
            let phase = (freq as f64 * i as f64 / RATE as f64).fract() as f32;
            let x = libm::sinf(core::f32::consts::TAU * phase);
            let first = if slope == FilterSlope::Db24 {
                &butterworth
            } else {
                &resonant
            };
            let mut y = stage(&mut state.stages[0], x, first, kind);
            if slope == FilterSlope::Db24 {
                y = stage(&mut state.stages[1], y, &resonant, kind);
            }
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        20.0 * libm::log10f(peak.max(1e-9))
    }

    #[test]
    fn responses() {
        use FilterKind::*;
        // 2-pole low-pass, Q 0.707: -3 dB at the cutoff, -12 dB per octave above.
        assert!(gain_db(LowPass, FilterSlope::Db12, 1000.0, 0.0, 100.0).abs() < 0.1);
        assert!((gain_db(LowPass, FilterSlope::Db12, 1000.0, 0.0, 1000.0) + 3.0).abs() < 0.2);
        let octave_up = gain_db(LowPass, FilterSlope::Db12, 1000.0, 0.0, 8000.0);
        assert!((octave_up + 36.0).abs() < 1.5, "{octave_up}");
        // 24 dB: twice as steep.
        let steep = gain_db(LowPass, FilterSlope::Db24, 1000.0, 0.0, 8000.0);
        assert!((steep + 72.0).abs() < 3.0, "{steep}");
        // High-pass mirrors, band-pass peaks at 0 dB, notch removes the centre.
        assert!(gain_db(HighPass, FilterSlope::Db12, 1000.0, 0.0, 10_000.0).abs() < 0.1);
        assert!(gain_db(HighPass, FilterSlope::Db12, 1000.0, 0.0, 125.0) < -35.0);
        assert!(gain_db(BandPass, FilterSlope::Db12, 1000.0, 0.5, 1000.0).abs() < 0.2);
        assert!(gain_db(Notch, FilterSlope::Db12, 1000.0, 0.0, 1000.0) < -40.0);
        // Resonance raises a peak at the cutoff (about Q).
        let peak = gain_db(LowPass, FilterSlope::Db12, 1000.0, 1.0, 1000.0);
        assert!((peak - 20.0 * libm::log10f(MAX_Q)).abs() < 1.0, "{peak}");
    }

    #[test]
    fn stays_finite_under_wild_modulation() {
        let mut state = Svf::default();
        let mut x = 0.3f32;
        for i in 0..200_000 {
            // Cutoff jumping between 20 Hz and Nyquist, maximal resonance.
            let cutoff = if i % 7 < 3 {
                MAX_CUTOFF * 2.0
            } else {
                MIN_CUTOFF
            };
            x = -x;
            let y = stage(
                &mut state,
                x,
                &Coefs::new(warp(cutoff, RATE), damping(1.0)),
                FilterKind::LowPass,
            );
            assert!(y.is_finite() && y.abs() < 100.0, "{y} at {i}");
        }
    }

    #[test]
    fn soft_clip_is_tanh_like() {
        assert_eq!(soft_clip(0.0), 0.0);
        assert_eq!(soft_clip(10.0), 1.0);
        assert_eq!(soft_clip(-10.0), -1.0);
        assert!((soft_clip(0.01) - 0.01).abs() < 1e-5);
        for i in 0..100 {
            let x = i as f32 * 0.03;
            assert!(soft_clip(x + 0.03) >= soft_clip(x), "not monotonic at {x}");
            assert!((soft_clip(x) - libm::tanhf(x)).abs() < 0.03, "{x}");
        }
    }

    #[test]
    fn sanitized_clamps() {
        let p = FilterParams {
            cutoff: 1e6,
            resonance: -1.0,
            keytrack: f32::NAN,
            ..Default::default()
        }
        .sanitized();
        assert_eq!(p.cutoff, MAX_CUTOFF);
        assert_eq!(p.resonance, 0.0);
        assert_eq!(p.keytrack, 0.0);
    }
}
