//! Stereo ensemble chorus.
//!
//! Each channel has a short delay line read at [`TAPS`] points whose delays
//! swing around `delay` by up to `depth`, driven by one sine LFO at phases
//! 120° apart; the right channel's taps are shifted by another 90°. The
//! slightly detuned copies thicken the sound, and the different phases on
//! both sides make even a mono signal wide. The wet signal is the sum of
//! the taps over √[`TAPS`], which keeps the level of the (decorrelated)
//! taps; only at depth 0, where they coincide, it is louder.

use klang_dsp::one_pole::time_coeff;

use crate::delay_line::DelayLines;
use crate::effects::{DryWet, Effect, MAX_SAMPLE_RATE};
use crate::{phase, sine::sine};

/// Ranges of [`ChorusParams`].
pub const MIN_RATE: f32 = 0.05;
pub const MAX_RATE: f32 = 5.0;
pub const MIN_DELAY: f32 = 0.005;
pub const MAX_DELAY: f32 = 0.03;
/// Swing of the taps' delay at depth 1, in seconds (less than `MIN_DELAY`).
pub const MAX_DEPTH: f32 = 0.004;

/// Delay taps per channel.
const TAPS: usize = 3;
/// Samples per line: the longest delay plus swing at [`MAX_SAMPLE_RATE`],
/// plus the interpolation. Above that rate the longest delays get shorter.
const LINE: usize = ((MAX_DELAY + MAX_DEPTH) * MAX_SAMPLE_RATE) as usize + 4;
/// 1/√TAPS, see the module documentation.
const TAP_GAIN: f32 = 0.577_350_3;
const _: () = assert!(TAPS == 3, "TAP_GAIN is 1/√3");
/// Time constant of the delay and depth glides.
const DELAY_GLIDE_SECONDS: f32 = 0.05;

/// Patch parameters of the chorus.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ChorusParams {
    /// 0..=1, see [`crate::effects::mix_gains`]; 0 = off.
    pub mix: f32,
    /// LFO rate in Hz, [`MIN_RATE`]..=[`MAX_RATE`].
    pub rate: f32,
    /// 0..=1: swing of the delays, up to [`MAX_DEPTH`].
    pub depth: f32,
    /// Centre delay in seconds, [`MIN_DELAY`]..=[`MAX_DELAY`].
    pub delay: f32,
}

impl ChorusParams {
    pub const DEFAULT: Self = Self {
        mix: 0.0,
        rate: 0.6,
        depth: 0.5,
        delay: 0.012,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            mix: crate::clamp_or(self.mix, 0.0, 1.0, d.mix),
            rate: crate::clamp_or(self.rate, MIN_RATE, MAX_RATE, d.rate),
            depth: crate::clamp_or(self.depth, 0.0, 1.0, d.depth),
            delay: crate::clamp_or(self.delay, MIN_DELAY, MAX_DELAY, d.delay),
        }
    }
}

impl Default for ChorusParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// State of the chorus.
pub(crate) struct Chorus {
    lines: DelayLines<LINE, 2>,
    /// LFO phase.
    lfo: u32,
    /// Current centre delay and swing in samples (gliding towards the set
    /// ones).
    delay: f32,
    swing: f32,
    rate: f32,
}

impl Chorus {
    /// Writes a silent chorus for `sample_rate` to `this` without building it
    /// on the stack.
    ///
    /// # Safety
    /// `this` must be valid for writes and suitably aligned.
    pub(crate) unsafe fn write_new(this: *mut Self, sample_rate: f32) {
        let rate = sample_rate.max(1.0);
        let d = ChorusParams::DEFAULT;
        // SAFETY: guaranteed by the caller.
        unsafe {
            DelayLines::write_new(core::ptr::addr_of_mut!((*this).lines));
            core::ptr::addr_of_mut!((*this).lfo).write(0);
            core::ptr::addr_of_mut!((*this).delay).write(d.delay * rate);
            core::ptr::addr_of_mut!((*this).swing).write(d.depth * MAX_DEPTH * rate);
            core::ptr::addr_of_mut!((*this).rate).write(rate);
        }
    }

    /// The swing of `params` in samples.
    fn swing(&self, params: &ChorusParams) -> f32 {
        params.depth.clamp(0.0, 1.0) * MAX_DEPTH * self.rate
    }
}

impl Effect for Chorus {
    type Params = ChorusParams;

    /// Silences the lines and jumps to the delay and depth of `params`.
    fn clear(&mut self, params: &ChorusParams) {
        self.lines.clear();
        self.delay = params.delay * self.rate;
        self.swing = self.swing(params);
    }

    fn process(
        &mut self,
        params: &ChorusParams,
        _smooth: f32,
        mix: &DryWet,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        /// Phase offsets of the taps (120° apart) and of the right channel (90°).
        const THIRD: u32 = (1u64 << 32).div_ceil(3) as u32;
        const QUARTER: u32 = 1 << 30;
        let increment = phase::increment(params.rate, self.rate);
        let swing = self.swing(params);
        let target = params.delay.clamp(MIN_DELAY, MAX_DELAY) * self.rate;
        // Delay and depth glide per sample: a jump would move every tap.
        let glide = 1.0 - time_coeff(1.0, DELAY_GLIDE_SECONDS * self.rate);
        let max_back = (LINE - 3) as f32;
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dry_gain, wet_gain) = mix.at(i);
            let wet_gain = wet_gain * TAP_GAIN;
            self.delay += (target - self.delay) * glide;
            self.swing += (swing - self.swing) * glide;
            self.lines.push([*l, *r]);

            let mut out = [0.0; 2];
            for (side, sum) in out.iter_mut().enumerate() {
                let side_phase = self.lfo.wrapping_add(QUARTER * side as u32);
                for tap in 0..TAPS {
                    let lfo = sine(side_phase.wrapping_add(THIRD * tap as u32));
                    let back = (self.delay + self.swing * lfo).clamp(1.0, max_back);
                    *sum += self.lines.read(side, back);
                }
            }
            self.lfo = self.lfo.wrapping_add(increment);

            *l = *l * dry_gain + out[0] * wet_gain;
            *r = *r * dry_gain + out[1] * wet_gain;
        }
    }
}
