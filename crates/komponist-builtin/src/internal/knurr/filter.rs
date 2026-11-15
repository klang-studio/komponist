//! knurr's filters: a ZDF/TPT ladder for "LP 24" and Simper's state-variable filter
//! (`klang_dsp::svf`) for the other types, a one-pole high-pass in front, and the soft clipper of
//! the drive. The transfer functions the graph draws are in `komponist_model::params::va_curve`
//! (the same damping and compensation).

use klang_dsp::svf::{Svf, SvfCoefs, warp};
use komponist_model::params::va_curve::{BP12, HP12, LP24, NOTCH, ladder_comp, ladder_k, svf_k};

use crate::internal::dsp::flush;

/// `tanh` as a rational function, exact in 0 and ±3 where it reaches ±1 (an error under 2 %).
#[inline]
pub fn soft_clip(x: f32) -> f32 {
    let x = x.clamp(-3.0, 3.0);
    x * (27.0 + x * x) / (27.0 + 9.0 * x * x)
}

/// The drive's gain in front of the saturation, 1 … 7 (quadratic, so the low end is fine).
#[inline]
pub fn drive_gain(drive: f32) -> f32 {
    1.0 + 6.0 * drive * drive
}

/// Everything one filter needs for a control interval.
#[derive(Clone, Copy, Debug, Default)]
pub struct Coefs {
    typ: usize,
    /// Gain in front of the saturation and the matching gain after the filter.
    pre: f32,
    post: f32,
    // Ladder: the one-pole gain `G` and its powers, `1 / (1 + g)`, feedback, input compensation
    // and the closed-form feedback divisor.
    gg: f32,
    g2: f32,
    g3: f32,
    g4: f32,
    h: f32,
    k: f32,
    comp: f32,
    inv: f32,
    svf: SvfCoefs,
}

impl Coefs {
    /// Coefficients for `cutoff` Hz, `resonance` 0 … 1 and `drive` 0 … 1.
    pub fn new(typ: usize, cutoff: f32, resonance: f32, drive: f32, rate: f32) -> Coefs {
        let pre = drive_gain(drive);
        let post = 1.0 / pre.sqrt();
        let mut c = Coefs {
            typ,
            pre,
            post,
            ..Coefs::default()
        };
        if typ == LP24 {
            let g = warp(cutoff, rate);
            let big = g / (1.0 + g);
            let k = ladder_k(f64::from(resonance)) as f32;
            c.gg = big;
            c.g2 = big * big;
            c.g3 = c.g2 * big;
            c.g4 = c.g3 * big;
            c.h = 1.0 / (1.0 + g);
            c.k = k;
            c.comp = ladder_comp(f64::from(k)) as f32;
            c.inv = 1.0 / (1.0 + k * c.g4);
        } else {
            c.svf = SvfCoefs::from_cutoff(cutoff, rate, svf_k(f64::from(resonance)) as f32);
        }
        c
    }
}

/// The state of one filter channel (both kinds, only the chosen one runs).
#[derive(Clone, Copy, Debug, Default)]
pub struct Filter {
    s: [f32; 4],
    svf: Svf,
}

impl Filter {
    /// Silence.
    pub fn reset(&mut self) {
        *self = Filter::default();
    }

    /// Keeps the state out of the subnormal range, and clears it if it is not finite.
    pub fn tidy(&mut self) {
        if self.s.iter().any(|s| !s.is_finite()) || !self.svf.magnitude().is_finite() {
            self.reset();
            return;
        }
        for s in &mut self.s {
            *s = flush(*s);
        }
    }

    /// One sample.
    #[inline]
    pub fn tick(&mut self, x: f32, c: &Coefs) -> f32 {
        let x = x * c.pre;
        let y = if c.typ == LP24 {
            let u = x * c.comp;
            let s = &mut self.s;
            let sigma = c.h * (c.g3 * s[0] + c.g2 * s[1] + c.gg * s[2] + s[3]);
            let y4 = (c.g4 * u + sigma) * c.inv;
            // The input stage saturates: the feedback is solved linearly, the clip bounds what
            // goes in, which keeps the self-oscillation level finite.
            let x1 = soft_clip(u - c.k * y4);
            let mut input = x1;
            for state in s.iter_mut() {
                let v = (input - *state) * c.gg;
                input = v + *state;
                *state = input + v;
            }
            input
        } else {
            let out = self.svf.tick(soft_clip(x), &c.svf);
            match c.typ {
                BP12 => out.band_normalized(),
                HP12 => out.high(),
                NOTCH => out.notch(),
                _ => out.low(),
            }
        };
        y * c.post
    }
}

/// A one-pole high-pass (6 dB/oct) for one channel.
#[derive(Clone, Copy, Debug, Default)]
pub struct HighPass {
    lp: f32,
}

impl HighPass {
    /// Silence.
    pub fn reset(&mut self) {
        self.lp = 0.0;
    }

    /// Keeps the state out of the subnormal range.
    pub fn tidy(&mut self) {
        self.lp = if self.lp.is_finite() {
            flush(self.lp)
        } else {
            0.0
        };
    }

    /// One sample; `amount` is `1 − exp(−2π f / rate)`.
    #[inline]
    pub fn tick(&mut self, x: f32, amount: f32) -> f32 {
        self.lp += (x - self.lp) * amount;
        x - self.lp
    }
}
