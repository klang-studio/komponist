//! Stereo delay with feedback, damping and ping-pong.
//!
//! Two delay lines (left, right). Normal: each channel echoes itself.
//! Ping-pong: the sum of both channels enters the left line, and each line
//! feeds the other, so the echoes alternate left, right, left, ... at the
//! same spacing. The feedback runs through a one-pole low-pass (damping), so
//! later echoes get darker, like a tape or analog delay.
//!
//! Changing the time glides the read position (smoothed), which bends the
//! pitch of the echoes briefly instead of clicking. The lines are sized for
//! [`MAX_TIME`] at the sample rate the delay is built for; nothing is
//! allocated while it runs.

use klang_dsp::one_pole::time_coeff;

use crate::delay_line::DelayLines;
use crate::effects::{DryWet, Effect, one_pole, sweep};
use crate::smooth::chase;

/// Ranges of [`DelayParams`].
pub const MIN_TIME: f32 = 0.01;
pub const MAX_TIME: f32 = 2.0;
pub const MAX_FEEDBACK: f32 = 0.95;

/// Samples per line at `rate`: enough for [`MAX_TIME`] plus the
/// interpolation.
fn line_len(rate: f32) -> usize {
    (MAX_TIME * rate) as usize + 4
}

/// Damping 0..1 moves the feedback low-pass cutoff from this ...
const DAMPING_MAX_HZ: f32 = 20_000.0;
/// ... down to this.
const DAMPING_MIN_HZ: f32 = 800.0;
/// Time constant of the time glide.
const TIME_GLIDE_SECONDS: f32 = 0.1;
/// Keeps the feedback loop above the denormal range in silence.
const ANTI_DENORMAL: f32 = 1e-20;

/// Patch parameters of the delay.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct DelayParams {
    /// 0..=1, see [`crate::effects::mix_gains`]; 0 = off.
    pub mix: f32,
    /// Seconds between echoes, [`MIN_TIME`]..=[`MAX_TIME`].
    pub time: f32,
    /// 0..=[`MAX_FEEDBACK`]: level of each echo relative to the previous one.
    pub feedback: f32,
    /// 0..=1: how much darker each echo gets.
    pub damping: f32,
    /// Echoes alternate between left and right.
    pub ping_pong: bool,
}

impl DelayParams {
    pub const DEFAULT: Self = Self {
        mix: 0.0,
        time: 0.375,
        feedback: 0.4,
        damping: 0.3,
        ping_pong: false,
    };

    /// Clamps all values into their valid range; non-finite values fall back
    /// to the defaults.
    pub fn sanitized(self) -> Self {
        let d = Self::DEFAULT;
        Self {
            mix: crate::clamp_or(self.mix, 0.0, 1.0, d.mix),
            time: crate::clamp_or(self.time, MIN_TIME, MAX_TIME, d.time),
            feedback: crate::clamp_or(self.feedback, 0.0, MAX_FEEDBACK, d.feedback),
            damping: crate::clamp_or(self.damping, 0.0, 1.0, d.damping),
            ..self
        }
    }
}

impl Default for DelayParams {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// State of the delay.
pub(crate) struct Delay {
    lines: DelayLines<2>,
    /// Current delay in samples (gliding towards the set time).
    delay: f32,
    /// Smoothed feedback, chasing the patch value once per block.
    feedback: f32,
    /// Low-pass state per line.
    damp_state: [f32; 2],
    rate: f32,
}

impl Delay {
    /// Writes a silent delay for `sample_rate` to `this` without building it
    /// on the stack. Allocates the lines (about 1.5 MB at 96 kHz).
    ///
    /// # Safety
    /// `this` must be valid for writes and suitably aligned.
    pub(crate) unsafe fn write_new(this: *mut Self, sample_rate: f32) {
        let rate = sample_rate.max(1.0);
        let d = DelayParams::DEFAULT;
        // SAFETY: guaranteed by the caller.
        unsafe {
            core::ptr::addr_of_mut!((*this).lines).write(DelayLines::new(line_len(rate)));
            core::ptr::addr_of_mut!((*this).delay).write(d.time * rate);
            core::ptr::addr_of_mut!((*this).feedback).write(d.feedback);
            core::ptr::addr_of_mut!((*this).damp_state).write([0.0; 2]);
            core::ptr::addr_of_mut!((*this).rate).write(rate);
        }
    }

    /// The delay of `params` in samples, within the line.
    fn target(&self, params: &DelayParams) -> f32 {
        (params.time * self.rate).clamp(1.0, (self.lines.len() - 3) as f32)
    }
}

impl Effect for Delay {
    type Params = DelayParams;

    /// Silences the echoes and jumps to the time and feedback of `params`.
    fn clear(&mut self, params: &DelayParams) {
        self.lines.clear();
        self.damp_state = [0.0; 2];
        self.delay = self.target(params);
        self.feedback = params.feedback;
    }

    fn process(
        &mut self,
        params: &DelayParams,
        smooth: f32,
        mix: &DryWet,
        left: &mut [f32],
        right: &mut [f32],
    ) {
        // 0 = no low-pass at all (even 20 kHz would soften each echo a bit).
        let damping = if params.damping > 0.0 {
            one_pole(
                sweep(params.damping, DAMPING_MAX_HZ, DAMPING_MIN_HZ),
                self.rate,
            )
        } else {
            0.0
        };
        chase(&mut self.feedback, params.feedback, smooth);
        let feedback = self.feedback.clamp(0.0, MAX_FEEDBACK);
        // The time glides per sample (one-pole), so long glides stay smooth.
        let target = self.target(params);
        let glide = 1.0 - time_coeff(1.0, TIME_GLIDE_SECONDS * self.rate);
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            let (dry_gain, wet_gain) = mix.at(i);
            self.delay += (target - self.delay) * glide;

            // Read before this sample is written: one less back.
            let back = self.delay - 1.0;
            let echo = [self.lines.read(0, back), self.lines.read(1, back)];
            let mut fed = [0.0; 2];
            for (k, state) in self.damp_state.iter_mut().enumerate() {
                *state = echo[k] + (*state - echo[k]) * damping;
                fed[k] = *state * feedback + ANTI_DENORMAL;
            }
            let (in_l, in_r) = if params.ping_pong {
                (0.5 * (*l + *r) + fed[1], fed[0])
            } else {
                (*l + fed[0], *r + fed[1])
            };
            self.lines.push([in_l, in_r]);

            *l = *l * dry_gain + echo[0] * wet_gain;
            *r = *r * dry_gain + echo[1] * wet_gain;
        }
    }
}
