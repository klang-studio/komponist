//! The EQ's band filters as plain `f64` math (WP-90): the RBJ biquad coefficients of one band and
//! the magnitude response of a band.
//!
//! The EQ device (`komponist-builtin`) runs these coefficients (rounded to `f32`) and the graph in
//! the device frame samples [`band_response_db`] for its curve, so the curve is the filter's real
//! response, not an approximation. Band types are the indices of `graph::eq`'s `band` constants.

use core::f64::consts::TAU;

/// Band type: low cut (12 dB/oct high-pass, `q` is its resonance).
pub const LOW_CUT: u32 = 0;
/// Band type: low shelf.
pub const LOW_SHELF: u32 = 1;
/// Band type: bell.
pub const BELL: u32 = 2;
/// Band type: high shelf.
pub const HIGH_SHELF: u32 = 3;
/// Band type: high cut (12 dB/oct low-pass, `q` is its resonance).
pub const HIGH_CUT: u32 = 4;

/// Sample rate the graph assumes when it draws a curve (the filters adapt to the real rate).
pub const CURVE_RATE: f64 = 48_000.0;

/// Biquad coefficients normalized to `a0 = 1`, `y = b0·x + b1·x₁ + b2·x₂ − a1·y₁ − a2·y₂`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coefs {
    /// Feed-forward.
    pub b: [f64; 3],
    /// Feedback (`a1`, `a2`).
    pub a: [f64; 2],
}

impl Coefs {
    /// Passes the signal unchanged.
    pub const IDENTITY: Coefs = Coefs {
        b: [1.0, 0.0, 0.0],
        a: [0.0, 0.0],
    };
}

/// Coefficients of a band (Robert Bristow-Johnson's "Audio EQ Cookbook"). `typ` is rounded to the
/// nearest band type (unknown: bell); `freq` is kept inside 10 Hz … 0.49 × `rate`, `q` at 0.05 or
/// more. Shelves use `q` as in the cookbook's Q form (0.71 ≈ slope 1).
pub fn coefs(typ: f64, freq: f64, gain_db: f64, q: f64, rate: f64) -> Coefs {
    let freq = freq.clamp(10.0, 0.49 * rate);
    let q = q.max(0.05);
    let w0 = TAU * freq / rate;
    let (sin, cos) = w0.sin_cos();
    let alpha = sin / (2.0 * q);
    let (b, a) = match typ.round() as i64 {
        0 => {
            let k = 1.0 + cos;
            ([k / 2.0, -k, k / 2.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
        }
        4 => {
            let k = 1.0 - cos;
            ([k / 2.0, k, k / 2.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
        }
        1 | 3 => {
            let amp = 10f64.powf(gain_db / 40.0);
            let t = 2.0 * amp.sqrt() * alpha;
            let (ap1, am1) = (amp + 1.0, amp - 1.0);
            if typ.round() as i64 == 1 {
                (
                    [
                        amp * (ap1 - am1 * cos + t),
                        2.0 * amp * (am1 - ap1 * cos),
                        amp * (ap1 - am1 * cos - t),
                    ],
                    [
                        ap1 + am1 * cos + t,
                        -2.0 * (am1 + ap1 * cos),
                        ap1 + am1 * cos - t,
                    ],
                )
            } else {
                (
                    [
                        amp * (ap1 + am1 * cos + t),
                        -2.0 * amp * (am1 + ap1 * cos),
                        amp * (ap1 + am1 * cos - t),
                    ],
                    [
                        ap1 - am1 * cos + t,
                        2.0 * (am1 - ap1 * cos),
                        ap1 - am1 * cos - t,
                    ],
                )
            }
        }
        _ => {
            let amp = 10f64.powf(gain_db / 40.0);
            (
                [1.0 + alpha * amp, -2.0 * cos, 1.0 - alpha * amp],
                [1.0 + alpha / amp, -2.0 * cos, 1.0 - alpha / amp],
            )
        }
    };
    let inv = 1.0 / a[0];
    Coefs {
        b: [b[0] * inv, b[1] * inv, b[2] * inv],
        a: [a[1] * inv, a[2] * inv],
    }
}

/// Magnitude of `c` at `hz` in dB.
pub fn magnitude_db(c: &Coefs, hz: f64, rate: f64) -> f64 {
    let w = TAU * hz / rate;
    let (s1, c1) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    // |B(e^jw)|² and |A(e^jw)|² with z⁻¹ = cos w − j sin w.
    let num_re = c.b[0] + c.b[1] * c1 + c.b[2] * c2;
    let num_im = -(c.b[1] * s1 + c.b[2] * s2);
    let den_re = 1.0 + c.a[0] * c1 + c.a[1] * c2;
    let den_im = -(c.a[0] * s1 + c.a[1] * s2);
    let num = num_re * num_re + num_im * num_im;
    let den = den_re * den_re + den_im * den_im;
    10.0 * (num.max(1e-30) / den.max(1e-30)).log10()
}

/// Response in dB of one band at `hz` at the graph's sample rate.
pub fn band_response_db(typ: f64, freq: f64, gain_db: f64, q: f64, hz: f64) -> f64 {
    magnitude_db(&coefs(typ, freq, gain_db, q, CURVE_RATE), hz, CURVE_RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bell_reaches_its_gain_at_its_frequency_and_is_flat_far_away() {
        let at = |hz| band_response_db(BELL as f64, 1000.0, 6.0, 1.0, hz);
        assert!((at(1000.0) - 6.0).abs() < 1e-9);
        assert!(at(20.0).abs() < 0.05);
        assert!(at(20_000.0).abs() < 0.5);
    }

    #[test]
    fn shelves_and_cuts_have_the_expected_asymptotes() {
        let ls = |hz| band_response_db(LOW_SHELF as f64, 200.0, 9.0, 0.71, hz);
        assert!((ls(20.0) - 9.0).abs() < 0.1 && ls(10_000.0).abs() < 0.1);
        let hs = |hz| band_response_db(HIGH_SHELF as f64, 4000.0, -6.0, 0.71, hz);
        assert!((hs(20_000.0) + 6.0).abs() < 0.5 && hs(50.0).abs() < 0.1);
        let lc = |hz| band_response_db(LOW_CUT as f64, 100.0, 0.0, 0.7071, hz);
        assert!((lc(100.0) + 3.0).abs() < 0.1, "{}", lc(100.0));
        assert!((lc(25.0) + 24.0).abs() < 1.0, "{}", lc(25.0));
        let hc = |hz| band_response_db(HIGH_CUT as f64, 5000.0, 0.0, 0.7071, hz);
        assert!((hc(5000.0) + 3.0).abs() < 0.1 && hc(100.0).abs() < 0.01);
    }
}
