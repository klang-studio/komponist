//! The compressor's static curve as plain `f64` math (WP-93): the gain computer with a soft knee
//! and the auto-makeup reference.
//!
//! The device `presse` (`komponist-builtin`) runs the same formulas on `f32` per sample, and the
//! transfer graph in the device frame draws [`output_db`], so the curve on screen is the curve the
//! device applies (Giannoulis, Massberg, Reiss: "Digital Dynamic Range Compressor Design", JAES
//! 2012, the quadratic knee).

/// Largest ratio; at this value the compressor limits (a ratio of ∞).
pub const RATIO_MAX: f64 = 20.0;

/// The output level in dB for the input level `x` dB, with threshold `t` dB, ratio `r` : 1 and a
/// knee `w` dB wide (0 = hard) centred on the threshold.
pub fn output_db(x: f64, t: f64, r: f64, w: f64) -> f64 {
    let r = r.clamp(1.0, RATIO_MAX);
    let w = w.max(0.0);
    let over = x - t;
    if 2.0 * over < -w {
        x
    } else if w > 0.0 && 2.0 * over.abs() <= w {
        x + (1.0 / r - 1.0) * (over + w / 2.0).powi(2) / (2.0 * w)
    } else {
        t + over / r
    }
}

/// The gain change in dB (≤ 0) the curve applies at input level `x`.
pub fn reduction_db(x: f64, t: f64, r: f64, w: f64) -> f64 {
    output_db(x, t, r, w) - x
}

/// The makeup in dB that auto-makeup applies: it undoes the reduction at the reference level
/// `t / 2` (halfway between the threshold and 0 dBFS). Always ≥ 0.
pub fn auto_makeup_db(t: f64, r: f64, w: f64) -> f64 {
    -reduction_db(t / 2.0, t, r, w)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_and_soft_knee_follow_the_formulas() {
        // Below the threshold: unity; above: slope 1/R.
        assert_eq!(output_db(-40.0, -18.0, 4.0, 0.0), -40.0);
        assert!((output_db(0.0, -18.0, 4.0, 0.0) - (-18.0 + 18.0 / 4.0)).abs() < 1e-12);
        // At the threshold the soft knee has already reduced by (1/R − 1)·(W/2)² / (2W).
        let w = 8.0;
        let at_t = reduction_db(-18.0, -18.0, 4.0, w);
        assert!((at_t - (0.25 - 1.0) * (w / 2.0f64).powi(2) / (2.0 * w)).abs() < 1e-12);
        // Continuous at both ends of the knee.
        for edge in [-18.0 - w / 2.0, -18.0 + w / 2.0] {
            let a = output_db(edge - 1e-9, -18.0, 4.0, w);
            let b = output_db(edge + 1e-9, -18.0, 4.0, w);
            assert!((a - b).abs() < 1e-6, "edge {edge}");
        }
    }

    #[test]
    fn auto_makeup_matches_the_proposals_example() {
        // T = −18, R = 4: reference −9 dB, reduction −6.75 dB, makeup +6.75 dB.
        assert!((auto_makeup_db(-18.0, 4.0, 0.0) - 6.75).abs() < 1e-12);
        assert_eq!(auto_makeup_db(-18.0, 1.0, 6.0), 0.0);
    }
}
