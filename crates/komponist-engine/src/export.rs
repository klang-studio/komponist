//! WAV export (ARCHITECTURE §6.7, screen S08): the sample processing after the render — peak
//! normalisation ([`normalize_gain`]), TPDF dither ([`tpdf`]) and PCM quantisation
//! ([`quantize`]). Dither and quantisation stay in the engine until a second project needs them
//! (ARCHITECTURE §10.4).

use klang_dsp::rng::Rng;
use klang_dsp::units::{MIN_GAIN, db_to_gain};

/// Peak level of "Normalisieren" (S08) in dBFS.
pub const NORMALIZE_TARGET_DB: f32 = -1.0;

/// The gain that brings a sample peak `peak` (linear) to `target_db` dBFS. 1.0 when there is
/// nothing to normalise: a peak at or below −120 dBFS ([`MIN_GAIN`], silence) or not finite.
pub fn normalize_gain(peak: f32, target_db: f32) -> f32 {
    if peak > MIN_GAIN && peak.is_finite() {
        db_to_gain(target_db) / peak
    } else {
        1.0
    }
}

/// One TPDF dither value in LSB: the difference of two independent uniform values in `[0, 1)`,
/// triangular over `(−1, 1)`, mean 0, variance 1/6 (RMS 1/√6 ≈ 0.408 LSB).
///
/// Added before rounding, it makes the total error of [`quantize`] independent of the signal in
/// mean and power: mean 0, variance 1/6 + 1/12 = 1/4 (RMS 0.5 LSB), white, without harmonics.
pub fn tpdf(rng: &mut Rng) -> f32 {
    rng.unipolar() - rng.unipolar()
}

/// `x` (full scale ±1.0) as a signed `bits`-bit integer (16 or 24): `x × 2^(bits−1) + dither`
/// rounded to the nearest integer (halves away from zero) and clipped to
/// `−2^(bits−1) ..= 2^(bits−1) − 1`. `dither` is in LSB ([`tpdf`], or 0). NaN gives 0.
pub fn quantize(x: f32, bits: u32, dither: f32) -> i32 {
    let full = f64::from(1_u32 << (bits - 1));
    let value = (f64::from(x) * full + f64::from(dither)).round();
    value.clamp(-full, full - 1.0) as i32
}
#[cfg(test)]
mod tests {
    use super::*;

    use klang_dsp::units::gain_to_db;

    #[test]
    fn tpdf_is_triangular_over_one_lsb_with_zero_mean() {
        let mut rng = Rng::new(3);
        let n = 400_000;
        let values: Vec<f64> = (0..n).map(|_| f64::from(tpdf(&mut rng))).collect();
        assert!(values.iter().all(|v| v.abs() < 1.0));
        let mean = values.iter().sum::<f64>() / n as f64;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
        // Standard error of the mean: √(1/6 / n) ≈ 0.00065.
        assert!(mean.abs() < 0.003, "mean {mean}");
        assert!((variance - 1.0 / 6.0).abs() < 0.002, "variance {variance}");
        // Triangular density 1 − |x|: P(|x| < 0.5) = 3/4, P(|x| < 0.25) = 7/16, and the four
        // quarters of (−1, 1) hold 1/8, 3/8, 3/8, 1/8.
        let share =
            |f: &dyn Fn(f64) -> bool| values.iter().filter(|&&v| f(v)).count() as f64 / n as f64;
        assert!((share(&|v| v.abs() < 0.5) - 0.75).abs() < 0.005);
        assert!((share(&|v| v.abs() < 0.25) - 7.0 / 16.0).abs() < 0.005);
        let quarters = [
            (-1.0, -0.5, 0.125),
            (-0.5, 0.0, 0.375),
            (0.0, 0.5, 0.375),
            (0.5, 1.0, 0.125),
        ];
        for (lo, hi, expected) in quarters {
            let got = share(&|v| lo <= v && v < hi);
            assert!((got - expected).abs() < 0.005, "{lo}..{hi}: {got}");
        }
        // White: neighbouring values are uncorrelated.
        let lag1 = values.windows(2).map(|w| w[0] * w[1]).sum::<f64>() / n as f64;
        assert!((lag1 / variance).abs() < 0.01, "lag-1 correlation {lag1}");
    }

    #[test]
    fn quantize_rounds_and_clips_16_bit() {
        let lsb = 1.0 / 32768.0;
        assert_eq!(quantize(0.0, 16, 0.0), 0);
        assert_eq!(quantize(0.5, 16, 0.0), 16384);
        assert_eq!(quantize(-0.5, 16, 0.0), -16384);
        assert_eq!(quantize(0.49 * lsb, 16, 0.0), 0);
        assert_eq!(quantize(0.51 * lsb, 16, 0.0), 1);
        assert_eq!(quantize(-0.51 * lsb, 16, 0.0), -1);
        assert_eq!(quantize(0.0, 16, 0.6), 1);
        assert_eq!(quantize(0.0, 16, -0.6), -1);
        assert_eq!(quantize(0.0, 16, 0.4), 0);
        // Full scale: +1.0 clips to the largest code, −1.0 is the smallest exactly.
        assert_eq!(quantize(1.0, 16, 0.0), 32767);
        assert_eq!(quantize(1.0 - lsb, 16, 0.0), 32767);
        assert_eq!(quantize(1.0 - lsb, 16, 0.9), 32767);
        assert_eq!(quantize(-1.0, 16, 0.0), -32768);
        assert_eq!(quantize(-1.0, 16, -0.9), -32768);
        assert_eq!(quantize(3.5, 16, 0.0), 32767);
        assert_eq!(quantize(-3.5, 16, 0.0), -32768);
        assert_eq!(quantize(f32::INFINITY, 16, 0.0), 32767);
        assert_eq!(quantize(f32::NEG_INFINITY, 16, 0.0), -32768);
        assert_eq!(quantize(f32::NAN, 16, 0.0), 0);
    }

    #[test]
    fn quantize_rounds_and_clips_24_bit() {
        let lsb = 1.0 / 8_388_608.0;
        assert_eq!(quantize(0.5, 24, 0.0), 4_194_304);
        assert_eq!(quantize(0.25 + 3.0 * lsb, 24, 0.0), 2_097_155);
        assert_eq!(quantize(-0.25 - 3.0 * lsb, 24, 0.0), -2_097_155);
        assert_eq!(quantize(0.0, 24, 0.7), 1);
        assert_eq!(quantize(1.0, 24, 0.0), 8_388_607);
        assert_eq!(quantize(-1.0, 24, 0.0), -8_388_608);
        assert_eq!(quantize(2.0, 24, 0.0), 8_388_607);
        assert_eq!(quantize(-2.0, 24, 0.0), -8_388_608);
        // The dither reaches the LSB even near full scale (computed in f64).
        assert_eq!(quantize(0.75, 24, 0.6), 6_291_457);
    }

    #[test]
    fn normalize_gain_brings_the_peak_to_the_target() {
        let target = db_to_gain(NORMALIZE_TARGET_DB);
        for peak in [0.5_f32, 0.001, 1.0, 1.7] {
            let gain = normalize_gain(peak, NORMALIZE_TARGET_DB);
            assert!((peak * gain - target).abs() < 1e-6, "{peak}");
            assert!((gain_to_db(peak * gain) - NORMALIZE_TARGET_DB).abs() < 1e-4);
        }
        assert_eq!(normalize_gain(0.0, -1.0), 1.0);
        assert_eq!(normalize_gain(MIN_GAIN, -1.0), 1.0);
        assert_eq!(normalize_gain(f32::INFINITY, -1.0), 1.0);
        assert_eq!(normalize_gain(f32::NAN, -1.0), 1.0);
        assert!((normalize_gain(0.25, -6.0) - db_to_gain(-6.0) * 4.0).abs() < 1e-6);
    }

    /// Power of `signal` at `cycles` cycles over its length (one DFT bin), normalised so that a
    /// sine of amplitude `a` on the bin gives `a² / 2`.
    fn bin_power(signal: &[f64], cycles: f64) -> f64 {
        let n = signal.len() as f64;
        let (mut re, mut im) = (0.0, 0.0);
        for (i, x) in signal.iter().enumerate() {
            let phase = core::f64::consts::TAU * cycles * i as f64 / n;
            re += x * phase.cos();
            im += x * phase.sin();
        }
        2.0 * (re * re + im * im) / (n * n)
    }

    #[test]
    fn dither_turns_quantisation_distortion_into_white_noise() {
        // A sine of 3 LSB at 16 bit, exactly 101 cycles in 2^16 samples, quantised with and
        // without dither. Error in LSB.
        let n = 1 << 16;
        let cycles = 101.0;
        let amplitude = 3.0 / 32768.0;
        let sine: Vec<f32> = (0..n)
            .map(|i| {
                let phase = core::f64::consts::TAU * cycles * i as f64 / n as f64;
                (amplitude * phase.sin()) as f32
            })
            .collect();
        let error = |dither: &mut dyn FnMut() -> f32| -> Vec<f64> {
            sine.iter()
                .map(|&x| f64::from(quantize(x, 16, dither())) - f64::from(x) * 32768.0)
                .collect()
        };
        let plain = error(&mut || 0.0);
        let mut rng = Rng::new(11);
        let dithered = error(&mut || tpdf(&mut rng));
        let rms = |e: &[f64]| (e.iter().map(|x| x * x).sum::<f64>() / e.len() as f64).sqrt();
        let mean = |e: &[f64]| e.iter().sum::<f64>() / e.len() as f64;
        // Total error of TPDF-dithered rounding: variance 1/6 + 1/12 = 1/4.
        assert!(
            (rms(&dithered) - 0.5).abs() < 0.01,
            "rms {}",
            rms(&dithered)
        );
        assert!(mean(&dithered).abs() < 0.01);
        // Undithered, the error of this low-level sine is periodic: its odd harmonics stand far
        // above the white floor (expected bin power of white noise with variance σ²: 2σ²/n;
        // measured: ≈ 250×).
        let floor = 2.0 * 0.25 / n as f64;
        let harmonic_peak = |e: &[f64]| {
            [3.0, 5.0, 7.0]
                .iter()
                .map(|h| bin_power(e, h * cycles))
                .fold(0.0, f64::max)
        };
        assert!(
            harmonic_peak(&plain) > 100.0 * floor,
            "{}",
            harmonic_peak(&plain)
        );
        // Dithered, the harmonics vanish into the floor (an exponential bin exceeds 10× its mean
        // with probability e^−10).
        assert!(
            harmonic_peak(&dithered) < 10.0 * floor,
            "{}",
            harmonic_peak(&dithered)
        );
        // And the signal itself comes through unchanged on average.
        let signal_power = bin_power(
            &sine
                .iter()
                .map(|&x| f64::from(x) * 32768.0)
                .collect::<Vec<_>>(),
            cycles,
        );
        let restored: Vec<f64> = dithered
            .iter()
            .zip(&sine)
            .map(|(e, &x)| e + f64::from(x) * 32768.0)
            .collect();
        assert!((bin_power(&restored, cycles) / signal_power - 1.0).abs() < 0.02);
    }
}
