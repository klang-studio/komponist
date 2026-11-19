//! Plain ↔ normalised value per [`Scale`], and the fader taper the `Db` scale shares with the
//! mixer's faders.

use super::Scale;
use crate::project::{VOLUME_DB_MAX, VOLUME_DB_MIN};

/// The fader taper (`DBT` in `ks-engine.js`): dB → position from the top, 0 at +6 dB, 1 at the
/// bottom (−70 dB = −∞), linear between the points. The scale labels sit on these points.
pub const DBT: [(f32, f32); 7] = [
    (6.0, 0.0),
    (0.0, 0.14),
    (-6.0, 0.30),
    (-12.0, 0.45),
    (-24.0, 0.65),
    (-48.0, 0.85),
    (-70.0, 1.0),
];

/// Positions at or below this are −∞ (`pToDb`: `p >= .995`).
pub const SILENCE_POSITION: f32 = 0.995;

/// dB → fader position from the top, 0–1 (`dbToP`): ≤ −70 dB (and NaN) → 1, above +6 dB → 0.
pub fn db_to_p(db: f32) -> f32 {
    if db.is_nan() || db <= VOLUME_DB_MIN {
        return 1.0;
    }
    for w in DBT.windows(2) {
        let ((a, pa), (b, pb)) = (w[0], w[1]);
        if db <= a && db >= b {
            return pa + (a - db) / (a - b) * (pb - pa);
        }
    }
    0.0
}

/// Fader position from the top → dB (`pToDb`): `f32::NEG_INFINITY` from [`SILENCE_POSITION`]
/// on, +6 dB at the top; the position is clamped to 0–1.
pub fn p_to_db(p: f32) -> f32 {
    let p = if p.is_nan() { 1.0 } else { p.clamp(0.0, 1.0) };
    if p >= SILENCE_POSITION {
        return f32::NEG_INFINITY;
    }
    for w in DBT.windows(2) {
        let ((a, pa), (b, pb)) = (w[0], w[1]);
        if p >= pa && p <= pb {
            return a + (p - pa) / (pb - pa) * (b - a);
        }
    }
    VOLUME_DB_MAX
}

/// The scale actually used: `Log` needs a positive minimum, `Db` a range inside the taper.
fn effective(scale: Scale, min: f64, max: f64) -> Scale {
    match scale {
        Scale::Log if min <= 0.0 => Scale::Lin,
        Scale::Db if min < f64::from(VOLUME_DB_MIN) || max > f64::from(VOLUME_DB_MAX) => Scale::Lin,
        s => s,
    }
}

/// Maps a plain value to 0–1. The caller supplies finite bounds with `min < max` and a
/// finite `v` inside `min ..= max`. Use [`super::ParamInfo::to_normalized`] for validation
/// and clamping.
pub fn to_normalized(scale: Scale, min: f64, max: f64, v: f64) -> f64 {
    match effective(scale, min, max) {
        Scale::Lin | Scale::Pan => (v - min) / (max - min),
        Scale::Log => (v / min).ln() / (max / min).ln(),
        Scale::Db => {
            // Position from the bottom of the range; the taper counts from the top.
            let (top, bottom) = (
                f64::from(db_to_p(max as f32)),
                f64::from(db_to_p(min as f32)),
            );
            if top == bottom {
                (v - min) / (max - min)
            } else {
                (bottom - f64::from(db_to_p(v as f32))) / (bottom - top)
            }
        }
    }
}

/// Maps a position to plain units. The caller supplies finite bounds with `min < max` and a
/// finite `n` inside `0 ..= 1`. Use [`super::ParamInfo::to_plain`] for validation, clamping
/// and snapping to discrete values.
pub fn to_plain(scale: Scale, min: f64, max: f64, n: f64) -> f64 {
    match effective(scale, min, max) {
        Scale::Lin | Scale::Pan => min + n * (max - min),
        Scale::Log => min * (max / min).powf(n),
        Scale::Db => {
            let (top, bottom) = (
                f64::from(db_to_p(max as f32)),
                f64::from(db_to_p(min as f32)),
            );
            if top == bottom {
                return min + n * (max - min);
            }
            let db = f64::from(p_to_db((bottom - n * (bottom - top)) as f32));
            if db.is_finite() {
                db.clamp(min, max)
            } else {
                min
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn narrow_db_range_with_equal_taper_positions_uses_linear_mapping() {
        let min = -6.00000001;
        let max = -6.0;
        let mid = (min + max) / 2.0;
        assert_eq!(db_to_p(min as f32), db_to_p(max as f32));
        assert_eq!(to_normalized(Scale::Db, min, max, min), 0.0);
        assert_eq!(to_normalized(Scale::Db, min, max, max), 1.0);
        assert!((to_normalized(Scale::Db, min, max, mid) - 0.5).abs() < 1e-6);
        assert_eq!(to_plain(Scale::Db, min, max, 0.0), min);
        assert_eq!(to_plain(Scale::Db, min, max, 0.5), mid);
        assert_eq!(to_plain(Scale::Db, min, max, 1.0), max);
    }
}
