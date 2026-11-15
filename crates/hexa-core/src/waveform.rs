//! Operator waveforms on a `u32` phase.
//!
//! Sawtooth and square jump, the triangle has corners; computed naively,
//! their harmonics above Nyquist alias back. PolyBLEP (jumps) and PolyBLAMP
//! (corners) smooth the two samples around each discontinuity with a
//! polynomial residual, which removes most of the aliasing at low cost.
//! Under strong phase modulation the instantaneous frequency deviates from
//! the nominal one used here, so the correction gets less exact.
//!
//! The engine renders four voices at once ([`sample4`], [`noise4`]); the
//! scalar versions are the readable reference the tests compare them with.

use crate::simd::{F32x4, U32x4};
#[cfg(test)]
use crate::sine::sine;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(rename_all = "lowercase"))]
pub enum Waveform {
    #[default]
    Sine,
    Triangle,
    Sawtooth,
    Square,
    /// White noise, a new random value every sample; the pitch does not
    /// matter (shape it with the filter). Needs state, see [`noise4`].
    Noise,
    /// Half sine (TX81Z style): the positive half of a sine, then flat.
    /// Shifted to a mean of 0 like the other waveforms and scaled to a peak
    /// of 1, so it spans [`HALF_FLOOR`]..=1.
    Half,
}

/// Mean of a half sine (the positive half, then 0): 1/π.
const HALF_MEAN: f32 = core::f32::consts::FRAC_1_PI;
/// Gain that lifts the shifted half sine's peak back to 1.
const HALF_GAIN: f32 = 1.0 / (1.0 - HALF_MEAN);
/// The flat part of [`Waveform::Half`].
pub const HALF_FLOOR: f32 = -HALF_MEAN * HALF_GAIN;
/// Change of the half sine's slope at both corners, per period.
const HALF_KINK: f32 = core::f32::consts::TAU * HALF_GAIN;

/// Four noise generators at once: per lane bit-identical to
/// [`klang_dsp::rng::Rng::bipolar`] (xorshift32, never 0), uniform in -1..1.
#[inline(always)]
pub(crate) fn noise4(state: &mut U32x4) -> F32x4 {
    let mut x = *state;
    x = x ^ x.shl::<13>();
    x = x ^ x.shr::<17>();
    x = x ^ x.shl::<5>();
    *state = x;
    x.shr::<8>().to_f32() * F32x4::splat(2.0 / (1 << 24) as f32) - F32x4::splat(1.0)
}

/// Start value of the noise generator of `operator` on `voice`: distinct and
/// never 0, so voices and operators do not rustle in step.
pub(crate) fn noise_seed(operator: usize, voice: usize) -> u32 {
    let n = (operator * 64 + voice) as u32 + 1;
    n.wrapping_mul(0x9e37_79b9) | 1
}

/// One sample of `waveform` at `phase`. `dt` is the phase increment per
/// sample as a fraction of a period (frequency / sample rate). All waveforms
/// start at 0 rising, like the sine, and span -1..=1.
#[cfg(test)]
#[inline]
pub(crate) fn sample(waveform: Waveform, phase: u32, dt: f32) -> f32 {
    let t = phase as f32 * (1.0 / 4_294_967_296.0);
    // The corrections need at least two samples per period.
    let dt = dt.clamp(1e-6, 0.5);
    match waveform {
        Waveform::Sine => sine(phase),
        Waveform::Triangle => {
            // Shifted by a quarter period so it starts at 0 rising; corners
            // at s = 0 (minimum) and s = 0.5 (maximum).
            let s = fract(t + 0.25);
            let naive = 1.0 - 4.0 * (s - 0.5).abs();
            // The slope changes by ±8 per period at each corner.
            naive + 8.0 * dt * (poly_blamp(s, dt) - poly_blamp(fract(s + 0.5), dt))
        }
        Waveform::Sawtooth => {
            // Rises from -1 to 1, jumps back at s = 0; shifted by half a
            // period so it starts at 0.
            let s = fract(t + 0.5);
            2.0 * s - 1.0 - poly_blep(s, dt)
        }
        Waveform::Square => {
            let naive = if t < 0.5 { 1.0 } else { -1.0 };
            naive + poly_blep(t, dt) - poly_blep(fract(t + 0.5), dt)
        }
        // Stateful: the engine calls `noise` instead.
        Waveform::Noise => 0.0,
        Waveform::Half => {
            // Corners at 0 (flat → rising) and 1/2 (falling → flat); the
            // slope grows by HALF_KINK at both. Each correction averages
            // dt/12 over a period, both together add HALF_KINK · dt²/6,
            // which is taken off again to keep the mean at 0.
            let naive = if t < 0.5 { sine(phase) } else { 0.0 };
            let corners = poly_blamp(t, dt) + poly_blamp(fract(t + 0.5), dt) - dt / 6.0;
            (naive - HALF_MEAN) * HALF_GAIN + HALF_KINK * dt * corners
        }
    }
}

/// `sample` for four phases (and `dt`s), bit-identical per lane.
#[inline(always)]
pub(crate) fn sample4(waveform: Waveform, phase: U32x4, dt: F32x4) -> F32x4 {
    let s = F32x4::splat;
    let t = phase.to_unit();
    let dt = dt.clamp(1e-6, 0.5);
    match waveform {
        Waveform::Sine => crate::sine::sine4(phase),
        Waveform::Triangle => {
            let s0 = (t + s(0.25)).fract();
            let naive = s(1.0) - s(4.0) * (s0 - s(0.5)).abs();
            naive + s(8.0) * dt * (poly_blamp4(s0, dt) - poly_blamp4((s0 + s(0.5)).fract(), dt))
        }
        Waveform::Sawtooth => {
            let s0 = (t + s(0.5)).fract();
            s(2.0) * s0 - s(1.0) - poly_blep4(s0, dt)
        }
        Waveform::Square => {
            let naive = t.lt(s(0.5)).select(s(1.0), s(-1.0));
            naive + poly_blep4(t, dt) - poly_blep4((t + s(0.5)).fract(), dt)
        }
        Waveform::Noise => F32x4::ZERO,
        Waveform::Half => {
            let naive = t.lt(s(0.5)).select(crate::sine::sine4(phase), F32x4::ZERO);
            let corners = poly_blamp4(t, dt) + poly_blamp4((t + s(0.5)).fract(), dt) - dt / s(6.0);
            (naive - s(HALF_MEAN)) * s(HALF_GAIN) + s(HALF_KINK) * dt * corners
        }
    }
}

/// `poly_blep` per lane: both branches, then selected.
#[inline(always)]
fn poly_blep4(t: F32x4, dt: F32x4) -> F32x4 {
    let s = F32x4::splat;
    let x = t / dt;
    let start = s(2.0) * x - x * x - s(1.0);
    let x = (t - s(1.0)) / dt;
    let end = x * x + s(2.0) * x + s(1.0);
    t.lt(dt)
        .select(start, t.gt(s(1.0) - dt).select(end, F32x4::ZERO))
}

/// `poly_blamp` per lane: both branches, then selected.
#[inline(always)]
fn poly_blamp4(t: F32x4, dt: F32x4) -> F32x4 {
    let s = F32x4::splat;
    let x = s(1.0) - t / dt;
    let start = x * x * x / s(6.0);
    let x = (t - s(1.0)) / dt + s(1.0);
    let end = x * x * x / s(6.0);
    t.lt(dt)
        .select(start, t.gt(s(1.0) - dt).select(end, F32x4::ZERO))
}

#[cfg(test)]
#[inline]
fn fract(x: f32) -> f32 {
    x - (x as i32) as f32
}

/// Residual of a band-limited step of height 2 at `t = 0`, for `t` in 0..1.
#[cfg(test)]
#[inline]
fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

/// Residual of a band-limited corner (slope change of 1 per sample) at
/// `t = 0`, for `t` in 0..1: the integral of the PolyBLEP residual of a
/// unit step.
#[cfg(test)]
#[inline]
fn poly_blamp(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = 1.0 - t / dt;
        x * x * x / 6.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x / 6.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Waveform; 5] = [
        Waveform::Sine,
        Waveform::Triangle,
        Waveform::Sawtooth,
        Waveform::Square,
        Waveform::Half,
    ];

    fn at(waveform: Waveform, t: f64, dt: f32) -> f32 {
        sample(waveform, (t * 4_294_967_296.0) as u64 as u32, dt)
    }

    #[test]
    fn shapes_away_from_discontinuities() {
        // Tiny dt: the corrections only touch the immediate neighborhood.
        let dt = 1e-5;
        for w in ALL {
            assert!(
                at(w, 0.0, dt).abs() < 1e-3 || matches!(w, Waveform::Square | Waveform::Half),
                "{w:?} does not start at 0"
            );
        }
        assert!((at(Waveform::Half, 0.25, dt) - 1.0).abs() < 1e-3);
        assert!((at(Waveform::Half, 0.75, dt) - HALF_FLOOR).abs() < 1e-3);
        assert!((at(Waveform::Triangle, 0.25, dt) - 1.0).abs() < 1e-3);
        assert!((at(Waveform::Triangle, 0.75, dt) + 1.0).abs() < 1e-3);
        assert!((at(Waveform::Triangle, 0.125, dt) - 0.5).abs() < 1e-3);
        assert!((at(Waveform::Sawtooth, 0.25, dt) - 0.5).abs() < 1e-3);
        assert!((at(Waveform::Sawtooth, 0.75, dt) + 0.5).abs() < 1e-3);
        assert_eq!(at(Waveform::Square, 0.25, dt), 1.0);
        assert_eq!(at(Waveform::Square, 0.75, dt), -1.0);
    }

    #[test]
    fn four_lanes_match_the_scalar_waveforms() {
        let dts = [0.0, 1e-7, 0.003, 0.02, 0.3, 0.7];
        for w in ALL {
            for i in 0..4096u32 {
                // Dense around the discontinuities at 0, 1/4, 1/2, 3/4.
                let phases = [
                    i << 20,
                    (i << 16).wrapping_sub(1 << 27),
                    (1 << 31) - i * 997,
                    (3 << 30) + i * 4099,
                ];
                let dt = core::array::from_fn(|k| dts[(i as usize + k) % dts.len()]);
                let lanes = sample4(w, U32x4::from_array(phases), F32x4::from_array(dt)).to_array();
                for k in 0..4 {
                    assert_eq!(
                        sample(w, phases[k], dt[k]).to_bits(),
                        lanes[k].to_bits(),
                        "{w:?} {} {}",
                        phases[k],
                        dt[k]
                    );
                }
            }
        }
        let seeds = [1, 2, 0x9e37_79b9, u32::MAX];
        let mut scalar = seeds.map(klang_dsp::rng::Rng::new);
        let mut lanes = U32x4::from_array(seeds);
        for _ in 0..1000 {
            let y = noise4(&mut lanes).to_array();
            for k in 0..4 {
                assert_eq!(scalar[k].bipolar().to_bits(), y[k].to_bits());
            }
        }
        assert_eq!(lanes.to_array(), scalar.map(|r| r.state()));
    }

    #[test]
    fn bounded_and_zero_mean() {
        for w in ALL {
            for dt in [0.001f32, 0.01, 0.1, 0.5] {
                let n = 4096;
                let samples: Vec<f32> = (0..n).map(|i| at(w, i as f64 / n as f64, dt)).collect();
                assert!(
                    samples.iter().all(|x| x.abs() <= 1.1),
                    "{w:?} at dt {dt} out of range"
                );
                let mean = samples.iter().sum::<f32>() / n as f32;
                assert!(mean.abs() < 1e-2, "{w:?} at dt {dt} has mean {mean}");
            }
        }
    }
}
