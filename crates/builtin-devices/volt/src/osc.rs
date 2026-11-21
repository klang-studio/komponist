//! Band-limited oscillators: PolyBLEP for the saw and the pulse, PolyBLAMP for the triangle, the
//! polynomial sine of `klang_dsp`. All of them are stateless functions of a `u32` phase
//! (`klang_dsp::phase`) and its increment, so a voice keeps its phases in plain arrays.

use klang_dsp::phase::to_unit;
use klang_dsp::sine::sine;
use komponist_model::params::va_curve::{MOD_PW, PULSE, SAW, TRIANGLE};

/// The oscillators of a voice ([`crate::engine::Engine`]): what makes the shared engine volt
/// ([`Analog`]) or surf (the wavetable reader). Everything else, the sub, noise, filter,
/// envelopes, LFOs, matrix and the voice handling, is the same.
///
/// Per control interval the voice [`prepare`](Oscillators::prepare)s oscillator A and B of
/// every unison pair from the waveform choice, the phase increment and the *shape* (volt: pulse
/// width, surf: table position) after the matrix, knowing the oscillator of the interval before;
/// per sample it calls [`sample`](Oscillators::sample) with the time inside the interval, so an
/// oscillator can glide from the last interval's state to the new one. Both run on the audio
/// thread: no allocation, no locks.
pub trait Oscillators: Send + 'static {
    /// One oscillator prepared for a control interval.
    type Osc: Copy + Default + Send;
    /// The shape after the matrix stays inside this range.
    const SHAPE_RANGE: (f32, f32);
    /// How far the matrix moves the shape at 100 %.
    const SHAPE_MOD: f32;
    /// The oscillator for an interval after `prev`: waveform choice `wave`, increment `inc` per
    /// sample (`klang_dsp::phase`), `shape`, sample rate `rate`.
    fn prepare(prev: &Self::Osc, wave: usize, inc: u32, shape: f32, rate: f32) -> Self::Osc;
    /// One sample at `phase`, `t` (0 … 1) into the control interval. `wave` is the current
    /// choice (volt switches at once, mid-interval).
    fn sample(osc: &Self::Osc, wave: usize, phase: u32, t: f32) -> f32;
    /// The default of every parameter by wire id: volt's table ([`crate::params`]) first, the
    /// instrument's own parameters after it.
    fn defaults() -> Vec<f64>;
    /// Oscillator A's octave and fine tune in cents (volt has neither: A is the reference).
    fn tune_a(_values: &[f64]) -> (f32, f32) {
        (0.0, 0.0)
    }
}

/// volt's oscillators: [`sample`] with the pulse width as the shape.
pub struct Analog;

/// An analog oscillator for an interval: increment and pulse width.
#[derive(Clone, Copy, Debug)]
pub struct AnalogOsc {
    inc: u32,
    pw: f32,
}

impl Default for AnalogOsc {
    fn default() -> Self {
        AnalogOsc { inc: 0, pw: 0.5 }
    }
}

impl Oscillators for Analog {
    type Osc = AnalogOsc;
    const SHAPE_RANGE: (f32, f32) = (0.02, 0.98);
    const SHAPE_MOD: f32 = MOD_PW as f32;

    #[inline]
    fn prepare(_prev: &AnalogOsc, _wave: usize, inc: u32, shape: f32, _rate: f32) -> AnalogOsc {
        AnalogOsc { inc, pw: shape }
    }

    #[inline]
    fn sample(osc: &AnalogOsc, wave: usize, phase: u32, _t: f32) -> f32 {
        sample(wave, phase, osc.inc, osc.pw)
    }

    fn defaults() -> Vec<f64> {
        crate::params::params(komponist_i18n::Lang::De)
            .iter()
            .map(|p| p.default)
            .collect()
    }
}

/// The correction of a step of height 2 at phase 0 that lasts one sample on each side (2-point
/// PolyBLEP). `t` is the phase, `dt` the increment per sample (both in periods).
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// The correction of a slope change of 1 per period at phase 0 (2-point PolyBLAMP), in units of
/// `dt`: multiply by the change of slope and by `dt`.
#[inline]
fn poly_blamp(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt - 1.0;
        -(x * x * x) * (1.0 / 6.0)
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x * (1.0 / 6.0)
    } else {
        0.0
    }
}

/// One sample of waveform `wave` (`va_curve::SINE` …) at `phase` with the increment `inc` per
/// sample and the pulse width `pw` (0 … 1). Output −1 … +1 (a little more at the corners).
#[inline]
pub fn sample(wave: usize, phase: u32, inc: u32, pw: f32) -> f32 {
    match wave {
        TRIANGLE => {
            let (t, dt) = (to_unit(phase), to_unit(inc));
            // Peak at phase 0 (slope +4 → −4), valley at 0.5 (−4 → +4).
            let naive = 4.0 * (t - 0.5).abs() - 1.0;
            let mut half = t + 0.5;
            if half >= 1.0 {
                half -= 1.0;
            }
            naive + 8.0 * dt * (poly_blamp(half, dt) - poly_blamp(t, dt))
        }
        SAW => {
            let (t, dt) = (to_unit(phase), to_unit(inc));
            2.0 * t - 1.0 - poly_blep(t, dt)
        }
        PULSE => {
            let (t, dt) = (to_unit(phase), to_unit(inc));
            let mut fall = t - pw;
            if fall < 0.0 {
                fall += 1.0;
            }
            let naive = if t < pw { 1.0 } else { -1.0 };
            naive + poly_blep(t, dt) - poly_blep(fall, dt)
        }
        _ => sine(phase),
    }
}
