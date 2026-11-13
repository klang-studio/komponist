//! oklch → sRGB, exactly like `hex()` in the prototype (`klang-studio-handoff/reference/ks-engine.js`).

use crate::Srgba;

/// Converts an oklch colour to 8-bit sRGB (alpha 255).
///
/// `l` is the Oklab lightness (0–1), `c` the chroma, `h` the hue in degrees. This is a
/// line-by-line port of the prototype's `hex(L, C, h)`, which produced the precomputed track
/// colours in `tokens.json`:
///
/// 1. oklch → Oklab (`a = c·cos h`, `b = c·sin h`),
/// 2. Oklab → LMS (cubed) → linear sRGB with Björn Ottosson's matrices,
/// 3. **per-channel clamp** to 0–1 (no gamut mapping, so out-of-gamut colours shift in hue the
///    same way as in the design),
/// 4. sRGB transfer function, × 255, rounded half up (`Math.round`).
pub fn oklch_to_srgb(l: f64, c: f64, h: f64) -> Srgba {
    // Same operation order as the JavaScript, so the doubles round the same way.
    let a = c * (h * std::f64::consts::PI / 180.0).cos();
    let b = c * (h * std::f64::consts::PI / 180.0).sin();

    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powf(3.0);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powf(3.0);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powf(3.0);

    let r = 4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_;
    let g = -1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_;
    let bb = -0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701_0 * s_;

    [channel(r), channel(g), channel(bb), 255]
}

/// Clamp, sRGB transfer, quantise: the prototype's inner `f`.
fn channel(x: f64) -> u8 {
    let x = x.clamp(0.0, 1.0);
    let x = if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    // `Math.round` rounds half up; for x ≥ 0 that is what `f64::round` does too.
    (x * 255.0).round() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grey_axis() {
        assert_eq!(oklch_to_srgb(0.0, 0.0, 0.0), [0, 0, 0, 255]);
        assert_eq!(oklch_to_srgb(1.0, 0.0, 0.0), [255, 255, 255, 255]);
        // Chroma 0 is grey whatever the hue (`hex(0.5, 0, 123)` = #636363).
        assert_eq!(oklch_to_srgb(0.5, 0.0, 123.0), [0x63, 0x63, 0x63, 255]);
    }

    #[test]
    fn out_of_gamut_is_clamped_per_channel() {
        // Far out of gamut, every channel saturates instead of wrapping (`hex(0.7, 0.5, 30)` = #ff0000).
        assert_eq!(oklch_to_srgb(0.7, 0.5, 30.0), [255, 0, 0, 255]);
    }

    #[test]
    fn matches_prototype_spot_values() {
        // Values printed by the original JavaScript `hex()` (node, ks-engine.js).
        assert_eq!(oklch_to_srgb(0.70, 0.12, 25.0), [0xdf, 0x7f, 0x78, 255]);
        assert_eq!(oklch_to_srgb(0.30, 0.06, 250.0), [0x14, 0x2f, 0x4b, 255]);
        assert_eq!(oklch_to_srgb(0.92, 0.06, 175.0), [0xbb, 0xf2, 0xe2, 255]);
    }
}
