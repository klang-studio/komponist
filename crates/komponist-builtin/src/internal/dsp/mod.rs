//! Small DSP parts shared by the effects (WP-90). Everything here is allocation-free after
//! construction; the buffers are sized in `activate`.
//!
//! Candidates for `klang-dsp` once a second user exists: [`Ring`] (power-of-two delay line with
//! linear and Hermite reads), [`Smooth`] (control-rate one-pole smoother with settle), the
//! one-pole low/high-pass [`OnePole`], and the sliding minimum [`SlidingMin`].

/// Samples between two control updates (parameter smoothing, coefficient recomputation). The
/// counter runs across calls, so the output does not depend on how the host splits blocks.
pub const CONTROL: usize = 32;

/// Flushes a value to 0 below `1e-20` (about −400 dB), so recursive state never becomes subnormal.
#[inline]
pub fn flush(x: f32) -> f32 {
    if x.abs() < 1e-20 { 0.0 } else { x }
}

/// `x` itself, or 0 when it is NaN or infinite.
#[inline]
pub fn finite(x: f32) -> f32 {
    if x.is_finite() { x } else { 0.0 }
}

/// `dB` as a linear gain.
#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    10f32.powf(db * 0.05)
}

/// `log2(x)` for a positive, finite, normal `x`: the exponent from the bits plus the logarithm of
/// the mantissa from the series of `atanh` (error below `2e-5`, i.e. 0.0001 dB in `20·log10`).
/// Used where `ln` per sample would cost too much (the compressor's level detection).
#[inline]
pub fn fast_log2(x: f32) -> f32 {
    let bits = x.to_bits();
    let exponent = ((bits >> 23) & 0xff) as i32 - 127;
    let m = f32::from_bits((bits & 0x007f_ffff) | 0x3f80_0000);
    let t = (m - 1.0) / (m + 1.0);
    let t2 = t * t;
    // 2/ln 2 · (t + t³/3 + t⁵/5 + t⁷/7)
    let series = t * (2.885_39 + t2 * (0.961_796_7 + t2 * (0.577_078 + t2 * 0.412_198_6)));
    exponent as f32 + series
}

/// `2^x`, for `x` within ±126 (clamped); relative error below `2e-4` (0.002 dB in a gain).
#[inline]
pub fn fast_exp2(x: f32) -> f32 {
    let x = x.clamp(-126.0, 126.0);
    let whole = x.floor();
    let f = x - whole;
    let poly = 1.0
        + f * (0.693_147_2
            + f * (0.240_226_5
                + f * (0.055_504_11
                    + f * (0.009_618_129 + f * (0.001_333_355 + f * 0.000_154_035)))));
    poly * f32::from_bits(((whole as i32 + 127) as u32) << 23)
}

/// Decibels per octave of amplitude: `20·log10(2)`.
pub const DB_PER_OCTAVE: f32 = 6.020_6;

/// Pole of a one-pole smoother that covers 63 % of the distance in `seconds`, updated every
/// `step` samples.
#[inline]
pub fn smooth_coeff(seconds: f32, step: usize, sample_rate: f32) -> f32 {
    if seconds <= 0.0 {
        0.0
    } else {
        (-(step as f32) / (seconds * sample_rate)).exp()
    }
}

/// A parameter that glides to its target and is applied as a linear ramp across each control
/// interval of [`CONTROL`] samples: no zipper noise, and the same output whatever the block size.
///
/// Per interval call [`Glide::start`] (one one-pole step towards `target`) or [`Glide::start_to`],
/// then read [`Glide::at`] with the number of samples already done in the interval.
#[derive(Clone, Copy, Debug)]
pub struct Glide {
    /// Where the value goes.
    pub target: f32,
    /// Value at the start of the interval and at its end.
    pub cur: f32,
    pub next: f32,
    /// Change per sample.
    pub step: f32,
}

impl Glide {
    /// Resting at `v`.
    pub const fn new(v: f32) -> Glide {
        Glide {
            target: v,
            cur: v,
            next: v,
            step: 0.0,
        }
    }

    /// Jumps onto the target.
    pub fn snap(&mut self) {
        self.jump(self.target);
    }

    /// Jumps to `v` (the target stays).
    pub fn jump(&mut self, v: f32) {
        self.cur = v;
        self.next = v;
        self.step = 0.0;
    }

    /// Whether the value rests on its target.
    pub fn settled(&self) -> bool {
        self.cur == self.target && self.next == self.target
    }

    /// Starts an interval towards `next`.
    pub fn start_to(&mut self, next: f32) {
        self.cur = self.next;
        self.next = next;
        self.step = (next - self.cur) / CONTROL as f32;
    }

    /// Starts an interval one one-pole step (`1 − coeff` of the distance) towards the target,
    /// snapping onto it within `eps`.
    pub fn start(&mut self, coeff: f32, eps: f32) {
        let d = self.target - self.next;
        let next = if d.abs() <= eps {
            self.target
        } else {
            self.next + d * (1.0 - coeff)
        };
        self.start_to(next);
    }

    /// As [`Glide::start`], with the change per sample limited to `max_per_sample`.
    pub fn start_limited(&mut self, coeff: f32, eps: f32, max_per_sample: f32) {
        self.start(coeff, eps);
        let max = max_per_sample * CONTROL as f32;
        if (self.next - self.cur).abs() > max {
            let next = self.cur + max.copysign(self.next - self.cur);
            self.start_to_keep(next);
        }
    }

    /// Re-aims the interval that [`Glide::start`] just began at `next`.
    fn start_to_keep(&mut self, next: f32) {
        self.next = next;
        self.step = (next - self.cur) / CONTROL as f32;
    }

    /// The value `done` samples into the interval; exactly `cur` when it rests.
    #[inline]
    pub fn at(&self, done: usize) -> f32 {
        if self.step == 0.0 {
            self.cur
        } else {
            self.cur + self.step * done as f32
        }
    }
}

/// A delay line of a power-of-two length.
#[derive(Clone, Debug, Default)]
pub struct Ring {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
}

impl Ring {
    /// A line that holds at least `min_len` samples (allocates, zeroed).
    pub fn with_len(min_len: usize) -> Ring {
        let len = (min_len + 4).next_power_of_two();
        Ring {
            buf: vec![0.0; len],
            mask: len - 1,
            write: 0,
        }
    }

    /// Longest delay [`Ring::read`] accepts, in samples.
    pub fn max_delay(&self) -> usize {
        self.buf.len().saturating_sub(4)
    }

    /// Zeroes the line.
    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }

    /// Writes one sample and advances.
    #[inline]
    pub fn push(&mut self, x: f32) {
        self.buf[self.write] = x;
        self.write = (self.write + 1) & self.mask;
    }

    /// The sample written `d` pushes ago (`d = 1`: the last one).
    #[inline]
    pub fn tap(&self, d: usize) -> f32 {
        self.buf[self.write.wrapping_sub(d) & self.mask]
    }

    /// The signal `delay` samples ago (`delay >= 1`, fractional), linear interpolation.
    #[inline]
    pub fn read(&self, delay: f32) -> f32 {
        let whole = delay as usize;
        let frac = delay - whole as f32;
        let a = self.tap(whole);
        let b = self.tap(whole + 1);
        a + (b - a) * frac
    }

    /// As [`Ring::read`] with 4-point Hermite interpolation (`delay >= 2`).
    #[inline]
    pub fn read_hermite(&self, delay: f32) -> f32 {
        let whole = delay as usize;
        let t = delay - whole as f32;
        let (xm1, x0, x1, x2) = (
            self.tap(whole - 1),
            self.tap(whole),
            self.tap(whole + 1),
            self.tap(whole + 2),
        );
        let c = (x1 - xm1) * 0.5;
        let v = x0 - x1;
        let w = c + v;
        let a = w + v + (x2 - x0) * 0.5;
        let b = w + a;
        ((a * t - b) * t + c) * t + x0
    }
}

/// One-pole filter state; `lp` follows the input, `hp` is the input minus that.
#[derive(Clone, Copy, Debug, Default)]
pub struct OnePole {
    z: f32,
}

impl OnePole {
    /// Zeroes the state.
    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    /// Low-pass with pole `coeff` (`exp(−2π·f/rate)`): `z += (x − z)(1 − coeff)`.
    #[inline]
    pub fn lp(&mut self, x: f32, coeff: f32) -> f32 {
        self.z = flush(x + (self.z - x) * coeff);
        self.z
    }

    /// High-pass of the same pole.
    #[inline]
    pub fn hp(&mut self, x: f32, coeff: f32) -> f32 {
        x - self.lp(x, coeff)
    }
}

/// Pole of a one-pole filter at `hz`, kept below 0.45 × the sample rate.
#[inline]
pub fn pole(hz: f32, sample_rate: f32) -> f32 {
    (-core::f32::consts::TAU * hz.min(0.45 * sample_rate) / sample_rate).exp()
}

/// Minimum of the last `window` values, O(1) per value (monotonic queue in preallocated arrays).
#[derive(Clone, Debug, Default)]
pub struct SlidingMin {
    values: Vec<f32>,
    stamps: Vec<u64>,
    head: usize,
    len: usize,
    now: u64,
    window: u64,
}

impl SlidingMin {
    /// A queue for windows up to `max_window` values (allocates).
    pub fn with_capacity(max_window: usize) -> SlidingMin {
        SlidingMin {
            values: vec![1.0; max_window + 1],
            stamps: vec![0; max_window + 1],
            head: 0,
            len: 0,
            now: 0,
            window: 1,
        }
    }

    /// Sets the window (at most the capacity) and forgets the history.
    pub fn set_window(&mut self, window: usize) {
        self.window = window.clamp(1, self.values.len() - 1) as u64;
        self.head = 0;
        self.len = 0;
    }

    /// Pushes `x`; returns the minimum of the last `window` values.
    #[inline]
    pub fn push(&mut self, x: f32) -> f32 {
        let cap = self.values.len();
        while self.len > 0 {
            let last = (self.head + self.len - 1) % cap;
            if self.values[last] >= x {
                self.len -= 1;
            } else {
                break;
            }
        }
        let at = (self.head + self.len) % cap;
        self.values[at] = x;
        self.stamps[at] = self.now;
        self.len += 1;
        while self.now - self.stamps[self.head] >= self.window {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.now += 1;
        self.values[self.head]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_min_matches_brute_force() {
        let mut m = SlidingMin::with_capacity(16);
        m.set_window(5);
        let xs: Vec<f32> = (0..200)
            .map(|i| ((i * 37 % 101) as f32 / 100.0).max(0.01))
            .collect();
        for (i, &x) in xs.iter().enumerate() {
            let got = m.push(x);
            let from = i.saturating_sub(4);
            let want = xs[from..=i].iter().copied().fold(f32::MAX, f32::min);
            assert_eq!(got, want, "at {i}");
        }
    }

    #[test]
    fn fast_log2_and_exp2_are_close_to_the_exact_functions() {
        let mut worst_log = 0.0f32;
        let mut worst_exp = 0.0f32;
        for i in 0..20_000 {
            let x = 1e-6 * 1.001f32.powi(i);
            worst_log = worst_log.max((fast_log2(x) - x.log2()).abs());
        }
        for i in -4000..4000 {
            let x = i as f32 * 0.01;
            worst_exp = worst_exp.max((fast_exp2(x) / x.exp2() - 1.0).abs());
        }
        assert!(worst_log < 2e-5, "log2 error {worst_log}");
        assert!(worst_exp < 2e-4, "exp2 error {worst_exp}");
        assert!(fast_exp2(-500.0) > 0.0 && fast_exp2(500.0).is_finite());
    }

    #[test]
    fn ring_reads_what_was_pushed() {
        let mut r = Ring::with_len(10);
        for i in 0..20 {
            r.push(i as f32);
        }
        assert_eq!(r.tap(1), 19.0);
        assert_eq!(r.read(3.5), 16.5);
        assert!((r.read_hermite(4.25) - 15.75).abs() < 1e-5);
    }
}
