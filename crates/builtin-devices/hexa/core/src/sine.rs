//! Polynomial sine on a `u32` phase: the scalar [`sine`] from
//! [`klang_dsp::sine`] and its four-lane version.

use crate::simd::{F32x4, U32x4};

pub use klang_dsp::sine::sine;
use klang_dsp::sine::{C1, C3, C5, C7, C9};

/// [`sine`] of four phases, bit-identical per lane.
#[inline(always)]
pub(crate) fn sine4(phase: U32x4) -> F32x4 {
    let s = F32x4::splat;
    let x = phase.to_i32_f32() * s(1.0 / 2_147_483_648.0);
    let folded = x.abs().gt(s(0.5)).select(s(1.0).copysign(x) - x, x);
    let x2 = folded * folded;
    folded * (s(C1) + x2 * (s(C3) + x2 * (s(C5) + x2 * (s(C7) + x2 * s(C9)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_lanes_match_the_scalar_sine() {
        for i in 0..(1u32 << 16) {
            let phases = [
                i.wrapping_mul(65_537),
                i.wrapping_mul(0x9e37_79b9),
                !i,
                i << 16,
            ];
            let lanes = sine4(U32x4::from_array(phases)).to_array();
            for (p, y) in phases.iter().zip(lanes) {
                assert_eq!(sine(*p).to_bits(), y.to_bits(), "phase {p}");
            }
        }
    }
}
