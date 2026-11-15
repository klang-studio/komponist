//! Band-limited oscillators: PolyBLEP for the saw and the pulse, PolyBLAMP for the triangle, the
//! polynomial sine of `klang_dsp`. All of them are stateless functions of a `u32` phase
//! (`klang_dsp::phase`) and its increment, so a voice keeps its phases in plain arrays.

use klang_dsp::phase::to_unit;
use klang_dsp::sine::sine;
use komponist_model::params::va_curve::{PULSE, SAW, TRIANGLE};

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
