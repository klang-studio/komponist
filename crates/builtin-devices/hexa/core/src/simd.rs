//! Four `f32` / `u32` lanes, one per voice: NEON on AArch64, plain arrays
//! everywhere else (the desktop, where the compiler may vectorize them).
//!
//! Every operation rounds exactly like the scalar Rust operation it stands
//! for (no fused multiply-add, `as` conversions with their saturation), so
//! code written with these types gives bit-identical results to the same
//! arithmetic on `f32` / `u32`, on both backends.
//!
//! Masks are [`U32x4`] lanes of all ones (true) or zero (false).
//!
//! # Safety
//!
//! The only `unsafe` code is in the NEON backend, and it is of two kinds.
//! The intrinsics are `unsafe` to call only because they need the NEON target
//! feature, which that module's `cfg` guarantees at compile time; almost all
//! of them take and return values in vector registers and access no memory.
//! The exceptions are the loads and stores (`from_array`, `to_array`), which
//! read or write exactly four lanes through a pointer to an owned `[T; 4]`,
//! and one `transmute` between `[f32; 4]` and `float32x4_t`, which have the
//! same size and every bit pattern valid. The portable backend is safe code.

pub(crate) const LANES: usize = 4;

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
pub(crate) use neon::{F32x4, U32x4};
#[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
pub(crate) use portable::{F32x4, U32x4};

impl F32x4 {
    pub(crate) const ZERO: Self = Self::splat_const(0.0);

    /// `self.clamp(min, max)` per lane (NaN stays NaN, like `f32::clamp`).
    #[inline(always)]
    pub(crate) fn clamp(self, min: f32, max: f32) -> Self {
        let (min, max) = (Self::splat(min), Self::splat(max));
        let low = self.lt(min).select(min, self);
        low.gt(max).select(max, low)
    }

    /// `x - (x as i32) as f32`: the fraction of non-negative values.
    #[inline(always)]
    pub(crate) fn fract(self) -> Self {
        self - self.to_i32_f32()
    }
}

impl U32x4 {
    /// Lanes as a fraction of a period, like [`crate::phase::to_unit`].
    #[inline(always)]
    pub(crate) fn to_unit(self) -> F32x4 {
        self.to_f32() * F32x4::splat(1.0 / crate::phase::PERIOD_F32)
    }
}

#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
    use core::arch::aarch64::*;
    use core::ops::{Add, BitAnd, BitXor, Div, Mul, Sub};

    #[derive(Clone, Copy)]
    pub(crate) struct F32x4(float32x4_t);

    #[derive(Clone, Copy)]
    pub(crate) struct U32x4(uint32x4_t);

    impl F32x4 {
        pub(crate) const fn splat_const(x: f32) -> Self {
            // SAFETY: `float32x4_t` is four `f32`s.
            Self(unsafe { core::mem::transmute::<[f32; 4], float32x4_t>([x; 4]) })
        }

        #[inline(always)]
        pub(crate) fn splat(x: f32) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vdupq_n_f32(x) })
        }

        #[inline(always)]
        pub(crate) fn from_array(a: [f32; 4]) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); `a` is four lanes, exactly what the load reads.
            Self(unsafe { vld1q_f32(a.as_ptr()) })
        }

        #[inline(always)]
        pub(crate) fn to_array(self) -> [f32; 4] {
            let mut a = [0.0; 4];
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); `a` is four lanes, exactly what the store writes.
            unsafe { vst1q_f32(a.as_mut_ptr(), self.0) };
            a
        }

        #[inline(always)]
        pub(crate) fn abs(self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vabsq_f32(self.0) })
        }

        /// The magnitude of `self` with the sign of `sign`.
        #[inline(always)]
        pub(crate) fn copysign(self, sign: Self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsics work on values in registers, no memory is accessed.
            unsafe {
                let sign_bit = vdupq_n_u32(0x8000_0000);
                Self(vbslq_f32(sign_bit, sign.0, self.0))
            }
        }

        #[inline(always)]
        pub(crate) fn lt(self, other: Self) -> U32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            U32x4(unsafe { vcltq_f32(self.0, other.0) })
        }

        #[inline(always)]
        pub(crate) fn gt(self, other: Self) -> U32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            U32x4(unsafe { vcgtq_f32(self.0, other.0) })
        }

        /// `(x as i32) as f32`.
        #[inline(always)]
        pub(crate) fn to_i32_f32(self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vcvtq_f32_s32(vcvtq_s32_f32(self.0)) })
        }

        /// `x as u32`.
        #[inline(always)]
        pub(crate) fn to_u32(self) -> U32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            U32x4(unsafe { vcvtq_u32_f32(self.0) })
        }

        /// `x as i64 as u32`: wraps instead of saturating at the `u32` range,
        /// for phase offsets of many periods. Exact through `f64`.
        #[inline(always)]
        pub(crate) fn to_u32_wrapping(self) -> U32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            unsafe {
                let low = vcvtq_s64_f64(vcvt_f64_f32(vget_low_f32(self.0)));
                let high = vcvtq_s64_f64(vcvt_high_f64_f32(self.0));
                U32x4(vreinterpretq_u32_s32(vmovn_high_s64(vmovn_s64(low), high)))
            }
        }
    }

    impl U32x4 {
        #[inline(always)]
        pub(crate) fn from_array(a: [u32; 4]) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); `a` is four lanes, exactly what the load reads.
            Self(unsafe { vld1q_u32(a.as_ptr()) })
        }

        #[inline(always)]
        pub(crate) fn to_array(self) -> [u32; 4] {
            let mut a = [0; 4];
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); `a` is four lanes, exactly what the store writes.
            unsafe { vst1q_u32(a.as_mut_ptr(), self.0) };
            a
        }

        #[inline(always)]
        pub(crate) fn wrapping_add(self, other: Self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vaddq_u32(self.0, other.0) })
        }

        #[inline(always)]
        pub(crate) fn shl<const N: i32>(self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vshlq_n_u32::<N>(self.0) })
        }

        #[inline(always)]
        pub(crate) fn shr<const N: i32>(self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vshrq_n_u32::<N>(self.0) })
        }

        /// `a < b`, unsigned.
        #[inline(always)]
        pub(crate) fn lt(self, other: Self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vcltq_u32(self.0, other.0) })
        }

        #[inline(always)]
        pub(crate) fn eq(self, other: Self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vceqq_u32(self.0, other.0) })
        }

        /// `x as i32 > 0`.
        #[inline(always)]
        pub(crate) fn positive_i32(self) -> Self {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            Self(unsafe { vcgtzq_s32(vreinterpretq_s32_u32(self.0)) })
        }

        /// `x as f32`.
        #[inline(always)]
        pub(crate) fn to_f32(self) -> F32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            F32x4(unsafe { vcvtq_f32_u32(self.0) })
        }

        /// `(x as i32) as f32`.
        #[inline(always)]
        pub(crate) fn to_i32_f32(self) -> F32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            F32x4(unsafe { vcvtq_f32_s32(vreinterpretq_s32_u32(self.0)) })
        }

        /// Whether any lane of this mask is set.
        #[inline(always)]
        pub(crate) fn any(self) -> bool {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            unsafe { vmaxvq_u32(self.0) != 0 }
        }

        /// Per lane of this mask: `a` where set, else `b`.
        #[inline(always)]
        pub(crate) fn select(self, a: F32x4, b: F32x4) -> F32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            F32x4(unsafe { vbslq_f32(self.0, a.0, b.0) })
        }

        /// Like [`Self::select`], for `u32` lanes.
        #[inline(always)]
        pub(crate) fn select_u32(self, a: U32x4, b: U32x4) -> U32x4 {
            // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsic works on values in registers, no memory is accessed.
            U32x4(unsafe { vbslq_u32(self.0, a.0, b.0) })
        }
    }

    macro_rules! binary {
        ($ty:ident, $trait:ident, $method:ident, $intrinsic:ident) => {
            impl $trait for $ty {
                type Output = Self;
                #[inline(always)]
                fn $method(self, other: Self) -> Self {
                    // SAFETY: NEON is enabled at compile time (see the module's `cfg`); the intrinsics work on values in registers, no memory is accessed.
                    Self(unsafe { $intrinsic(self.0, other.0) })
                }
            }
        };
    }

    binary!(F32x4, Add, add, vaddq_f32);
    binary!(F32x4, Sub, sub, vsubq_f32);
    binary!(F32x4, Mul, mul, vmulq_f32);
    binary!(F32x4, Div, div, vdivq_f32);
    binary!(U32x4, BitAnd, bitand, vandq_u32);
    binary!(U32x4, BitXor, bitxor, veorq_u32);
}

#[cfg(not(all(target_arch = "aarch64", target_feature = "neon")))]
mod portable {
    //! Lane by lane, written out (no closures), so unoptimized test builds
    //! stay fast.

    use core::ops::{Add, BitAnd, BitXor, Div, Mul, Sub};

    #[derive(Clone, Copy)]
    pub(crate) struct F32x4([f32; 4]);

    #[derive(Clone, Copy)]
    pub(crate) struct U32x4([u32; 4]);

    /// `[f(a[0]), …, f(a[3])]` for the lanes of `a` (and `b`).
    macro_rules! lanes {
        (|$x:ident| $e:expr, $a:expr) => {{
            let a = $a;
            [
                {
                    let $x = a[0];
                    $e
                },
                {
                    let $x = a[1];
                    $e
                },
                {
                    let $x = a[2];
                    $e
                },
                {
                    let $x = a[3];
                    $e
                },
            ]
        }};
        (|$x:ident, $y:ident| $e:expr, $a:expr, $b:expr) => {{
            let (a, b) = ($a, $b);
            [
                {
                    let ($x, $y) = (a[0], b[0]);
                    $e
                },
                {
                    let ($x, $y) = (a[1], b[1]);
                    $e
                },
                {
                    let ($x, $y) = (a[2], b[2]);
                    $e
                },
                {
                    let ($x, $y) = (a[3], b[3]);
                    $e
                },
            ]
        }};
    }

    /// A mask lane from a condition.
    #[inline(always)]
    fn mask(condition: bool) -> u32 {
        if condition { u32::MAX } else { 0 }
    }

    impl F32x4 {
        pub(crate) const fn splat_const(x: f32) -> Self {
            Self([x; 4])
        }

        #[inline(always)]
        pub(crate) fn splat(x: f32) -> Self {
            Self([x; 4])
        }

        #[inline(always)]
        pub(crate) fn from_array(a: [f32; 4]) -> Self {
            Self(a)
        }

        #[inline(always)]
        pub(crate) fn to_array(self) -> [f32; 4] {
            self.0
        }

        #[inline(always)]
        pub(crate) fn abs(self) -> Self {
            Self(lanes!(|x| x.abs(), self.0))
        }

        #[inline(always)]
        pub(crate) fn copysign(self, sign: Self) -> Self {
            Self(lanes!(|x, s| x.copysign(s), self.0, sign.0))
        }

        #[inline(always)]
        pub(crate) fn lt(self, other: Self) -> U32x4 {
            U32x4(lanes!(|x, y| mask(x < y), self.0, other.0))
        }

        #[inline(always)]
        pub(crate) fn gt(self, other: Self) -> U32x4 {
            U32x4(lanes!(|x, y| mask(x > y), self.0, other.0))
        }

        #[inline(always)]
        pub(crate) fn to_i32_f32(self) -> Self {
            Self(lanes!(|x| (x as i32) as f32, self.0))
        }

        #[inline(always)]
        pub(crate) fn to_u32(self) -> U32x4 {
            U32x4(lanes!(|x| x as u32, self.0))
        }

        #[inline(always)]
        pub(crate) fn to_u32_wrapping(self) -> U32x4 {
            U32x4(lanes!(|x| x as i64 as u32, self.0))
        }
    }

    impl U32x4 {
        #[inline(always)]
        pub(crate) fn from_array(a: [u32; 4]) -> Self {
            Self(a)
        }

        #[inline(always)]
        pub(crate) fn to_array(self) -> [u32; 4] {
            self.0
        }

        #[inline(always)]
        pub(crate) fn wrapping_add(self, other: Self) -> Self {
            Self(lanes!(|x, y| x.wrapping_add(y), self.0, other.0))
        }

        #[inline(always)]
        pub(crate) fn shl<const N: i32>(self) -> Self {
            Self(lanes!(|x| x << N, self.0))
        }

        #[inline(always)]
        pub(crate) fn shr<const N: i32>(self) -> Self {
            Self(lanes!(|x| x >> N, self.0))
        }

        #[inline(always)]
        pub(crate) fn lt(self, other: Self) -> Self {
            Self(lanes!(|x, y| mask(x < y), self.0, other.0))
        }

        #[inline(always)]
        pub(crate) fn eq(self, other: Self) -> Self {
            Self(lanes!(|x, y| mask(x == y), self.0, other.0))
        }

        #[inline(always)]
        pub(crate) fn positive_i32(self) -> Self {
            Self(lanes!(|x| mask(x as i32 > 0), self.0))
        }

        #[inline(always)]
        pub(crate) fn to_f32(self) -> F32x4 {
            F32x4(lanes!(|x| x as f32, self.0))
        }

        #[inline(always)]
        pub(crate) fn to_i32_f32(self) -> F32x4 {
            F32x4(lanes!(|x| (x as i32) as f32, self.0))
        }

        #[inline(always)]
        pub(crate) fn any(self) -> bool {
            self.0 != [0; 4]
        }

        #[inline(always)]
        pub(crate) fn select(self, a: F32x4, b: F32x4) -> F32x4 {
            let (m, a, b) = (self.0, a.0, b.0);
            F32x4([
                pick(m[0], a[0], b[0]),
                pick(m[1], a[1], b[1]),
                pick(m[2], a[2], b[2]),
                pick(m[3], a[3], b[3]),
            ])
        }

        #[inline(always)]
        pub(crate) fn select_u32(self, a: U32x4, b: U32x4) -> U32x4 {
            let (m, a, b) = (self.0, a.0, b.0);
            U32x4([
                pick(m[0], a[0], b[0]),
                pick(m[1], a[1], b[1]),
                pick(m[2], a[2], b[2]),
                pick(m[3], a[3], b[3]),
            ])
        }
    }

    /// `a` where the mask lane `m` is set, else `b`.
    #[inline(always)]
    fn pick<T>(m: u32, a: T, b: T) -> T {
        if m != 0 { a } else { b }
    }

    macro_rules! binary {
        ($ty:ident, $trait:ident, $method:ident, $op:tt) => {
            impl $trait for $ty {
                type Output = Self;
                #[inline(always)]
                fn $method(self, other: Self) -> Self {
                    Self(lanes!(|x, y| x $op y, self.0, other.0))
                }
            }
        };
    }

    binary!(F32x4, Add, add, +);
    binary!(F32x4, Sub, sub, -);
    binary!(F32x4, Mul, mul, *);
    binary!(F32x4, Div, div, /);
    binary!(U32x4, BitAnd, bitand, &);
    binary!(U32x4, BitXor, bitxor, ^);
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALUES: [f32; 12] = [
        0.0,
        -0.0,
        0.3,
        -0.7,
        1.5,
        -2.5e9,
        3.1e9,
        7.3e10,
        -9.9e12,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
    ];

    fn same(a: f32, b: f32) -> bool {
        a.to_bits() == b.to_bits() || a.is_nan() && b.is_nan()
    }

    /// Every lane against the scalar operation it stands for.
    #[test]
    fn lanes_match_scalar_operations() {
        type Case = (F32x4, fn(f32, f32) -> f32);
        for chunk in VALUES.chunks(2) {
            for &y in &VALUES {
                let a = [chunk[0], chunk[1], y, -y];
                let b = [y, -y, chunk[1], chunk[0]];
                let (va, vb) = (F32x4::from_array(a), F32x4::from_array(b));
                let ops: [Case; 6] = [
                    (va + vb, |x, y| x + y),
                    (va - vb, |x, y| x - y),
                    (va * vb, |x, y| x * y),
                    (va / vb, |x, y| x / y),
                    (va.copysign(vb), f32::copysign),
                    (va.clamp(-1.0, 2.0), |x, _| x.clamp(-1.0, 2.0)),
                ];
                for (v, f) in ops {
                    for k in 0..LANES {
                        assert!(same(v.to_array()[k], f(a[k], b[k])), "{a:?} {b:?} lane {k}");
                    }
                }
                for k in 0..LANES {
                    let x = a[k];
                    assert!(same(va.abs().to_array()[k], x.abs()));
                    assert!(same(va.to_i32_f32().to_array()[k], (x as i32) as f32));
                    assert_eq!(va.to_u32().to_array()[k], x as u32, "{x}");
                    assert_eq!(va.to_u32_wrapping().to_array()[k], x as i64 as u32, "{x}");
                    assert_eq!(va.lt(vb).to_array()[k] != 0, x < b[k]);
                    assert_eq!(va.gt(vb).to_array()[k] != 0, x > b[k]);
                }
            }
        }
    }

    #[test]
    fn integer_lanes_match_scalar_operations() {
        let a = [0, 1, 0x8000_0001, u32::MAX];
        let b = [1, 1, 7, 0x7fff_ffff];
        let (va, vb) = (U32x4::from_array(a), U32x4::from_array(b));
        for k in 0..LANES {
            assert_eq!(va.wrapping_add(vb).to_array()[k], a[k].wrapping_add(b[k]));
            assert_eq!((va ^ vb).to_array()[k], a[k] ^ b[k]);
            assert_eq!(va.shl::<13>().to_array()[k], a[k] << 13);
            assert_eq!(va.shr::<17>().to_array()[k], a[k] >> 17);
            assert_eq!(va.lt(vb).to_array()[k] != 0, a[k] < b[k]);
            assert_eq!(va.eq(vb).to_array()[k] != 0, a[k] == b[k]);
            assert_eq!(va.positive_i32().to_array()[k] != 0, a[k] as i32 > 0);
            assert_eq!(va.to_f32().to_array()[k].to_bits(), (a[k] as f32).to_bits());
            assert_eq!(
                va.to_i32_f32().to_array()[k].to_bits(),
                ((a[k] as i32) as f32).to_bits()
            );
        }
        let mask = va.lt(vb);
        assert!(mask.any());
        assert!(!U32x4::from_array([0; 4]).any());
        let picked = mask.select(F32x4::splat(1.0), F32x4::splat(2.0)).to_array();
        assert_eq!(picked, [1.0, 2.0, 2.0, 2.0]);
        assert_eq!(mask.select_u32(va, vb).to_array(), [0, 1, 7, 0x7fff_ffff]);
    }
}
