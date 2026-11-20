//! Three-band master EQ: low shelf, peaking mid, high shelf.
//!
//! RBJ "Audio EQ Cookbook" biquads from [`klang_dsp::biquad`], the same
//! settings on both channels with separate state. Off (and free) while all three gains are 0 dB; see
//! [`crate::effects`] for the smoothing (coefficients once per block).
//!
//! Direct form I, not the transposed direct form II: the per-block
//! coefficient steps of a gain glide made audible jumps with TDF-II, see
//! [`klang_dsp::biquad`].

use klang_dsp::biquad::{BiquadCoefs, BiquadDf1};

use crate::smooth::chase;

/// Largest boost or cut of a band in dB.
pub const MAX_EQ_GAIN: f32 = 15.0;
/// Corner frequency ranges of the bands in Hz.
pub const MIN_LOW_FREQ: f32 = 30.0;
pub const MAX_LOW_FREQ: f32 = 1000.0;
pub const MIN_MID_FREQ: f32 = 100.0;
pub const MAX_MID_FREQ: f32 = 10_000.0;
pub const MIN_HIGH_FREQ: f32 = 1000.0;
pub const MAX_HIGH_FREQ: f32 = 16_000.0;
/// Range of the mid band's Q.
pub const MIN_MID_Q: f32 = 0.3;
pub const MAX_MID_Q: f32 = 6.0;

/// Patch parameters of the EQ.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct EqParams {
    /// Low shelf gain in dB, ±[`MAX_EQ_GAIN`].
    pub low_gain: f32,
    /// Low shelf corner in Hz.
    pub low_freq: f32,
    /// Mid peak gain in dB, ±[`MAX_EQ_GAIN`].
    pub mid_gain: f32,
    /// Mid peak centre in Hz.
    pub mid_freq: f32,
    /// Mid peak Q (higher = narrower).
    pub mid_q: f32,
    /// High shelf gain in dB, ±[`MAX_EQ_GAIN`].
    pub high_gain: f32,
    /// High shelf corner in Hz.
    pub high_freq: f32,
}

impl EqParams {
    pub const DEFAULT: Self = Self {
        low_gain: 0.0,
        low_freq: 150.0,
        mid_gain: 0.0,
        mid_freq: 1000.0,
        mid_q: 0.9,
        high_gain: 0.0,
        high_freq: 6000.0,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            low_gain: crate::clamp_or(self.low_gain, -MAX_EQ_GAIN, MAX_EQ_GAIN, d.low_gain),
            low_freq: crate::clamp_or(self.low_freq, MIN_LOW_FREQ, MAX_LOW_FREQ, d.low_freq),
            mid_gain: crate::clamp_or(self.mid_gain, -MAX_EQ_GAIN, MAX_EQ_GAIN, d.mid_gain),
            mid_freq: crate::clamp_or(self.mid_freq, MIN_MID_FREQ, MAX_MID_FREQ, d.mid_freq),
            mid_q: crate::clamp_or(self.mid_q, MIN_MID_Q, MAX_MID_Q, d.mid_q),
            high_gain: crate::clamp_or(self.high_gain, -MAX_EQ_GAIN, MAX_EQ_GAIN, d.high_gain),
            high_freq: crate::clamp_or(self.high_freq, MIN_HIGH_FREQ, MAX_HIGH_FREQ, d.high_freq),
        }
    }

    /// All bands flat: the EQ does nothing.
    pub fn is_flat(&self) -> bool {
        self.low_gain == 0.0 && self.mid_gain == 0.0 && self.high_gain == 0.0
    }
}

impl Default for EqParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Highest corner frequency relative to the sample rate (below Nyquist).
const MAX_FREQ_RATIO: f32 = 0.49;

/// Number of bands.
const BANDS: usize = 3;

/// State of the EQ.
pub(crate) struct Eq {
    state: [[BiquadDf1; 2]; BANDS],
    /// Smoothed parameters, chasing the patch values once per block.
    smoothed: EqParams,
    /// The EQ ran in the previous block.
    running: bool,
    rate: f32,
}

impl Eq {
    pub(crate) fn new(sample_rate: f32) -> Self {
        Self {
            state: Default::default(),
            smoothed: EqParams::DEFAULT,
            running: false,
            rate: sample_rate.max(1.0),
        }
    }

    /// Filters one block in place with `target`, smoothed by `smooth`. The
    /// EQ has no mix: it is off while all smoothed gains are 0 dB, and its
    /// state is cleared when it starts again.
    pub(crate) fn process(
        &mut self,
        target: &EqParams,
        smooth: f32,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        let p = &mut self.smoothed;
        if !self.running {
            // Start from the patch's frequencies and Q (the gains glide in
            // from 0 dB) instead of gliding from old ones.
            *p = EqParams {
                low_gain: p.low_gain,
                mid_gain: p.mid_gain,
                high_gain: p.high_gain,
                ..*target
            };
        }
        chase(&mut p.low_gain, target.low_gain, smooth);
        chase(&mut p.mid_gain, target.mid_gain, smooth);
        chase(&mut p.high_gain, target.high_gain, smooth);
        chase(&mut p.low_freq, target.low_freq, smooth);
        chase(&mut p.mid_freq, target.mid_freq, smooth);
        chase(&mut p.mid_q, target.mid_q, smooth);
        chase(&mut p.high_freq, target.high_freq, smooth);
        if p.is_flat() {
            self.running = false;
            return;
        }
        if !self.running {
            self.state = Default::default();
            self.running = true;
        }
        let params = self.smoothed;
        self.filter(&params, left, right);
    }

    /// Stops the EQ; the next block starts it from silence.
    pub(crate) fn stop(&mut self) {
        self.running = false;
        self.smoothed = EqParams::DEFAULT;
    }

    /// Filters `left` and `right` in place with `params` (coefficients for
    /// the whole slice).
    fn filter(&mut self, params: &EqParams, left: &mut [f32], right: &mut [f32]) {
        let rate = self.rate;
        let freq = |f: f32| f.min(MAX_FREQ_RATIO * rate);
        let bands = [
            BiquadCoefs::low_shelf(freq(params.low_freq), params.low_gain, rate),
            BiquadCoefs::peaking(
                freq(params.mid_freq),
                params.mid_q.max(MIN_MID_Q),
                params.mid_gain,
                rate,
            ),
            BiquadCoefs::high_shelf(freq(params.high_freq), params.high_gain, rate),
        ];
        for (channel, samples) in [left, right].into_iter().enumerate() {
            for (c, state) in bands.iter().zip(self.state.iter_mut()) {
                let mut biquad = state[channel];
                for x in samples.iter_mut() {
                    *x = biquad.tick(*x, c);
                }
                biquad.flush_denormals();
                state[channel] = biquad;
            }
        }
    }
}
