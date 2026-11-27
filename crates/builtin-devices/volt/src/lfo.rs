//! The LFOs: [`klang_synth::CyclicLfo`] (phase, wrap and the six shapes on a `u32` phase) stepped
//! once per control interval. Volt keeps what the core leaves to the caller: the increment per
//! control interval, one random generator per LFO and its draw order (two values on a (re)start,
//! one per wrap), the fade-in and the unipolar output.

use klang_dsp::phase::increment;
use klang_dsp::rng::Rng;
use klang_synth::{CyclicLfo, Shape};

use komponist_device_core::dsp::CONTROL;

/// One LFO's state (a voice has two; two more run free for the LFOs that do not restart on a
/// key).
#[derive(Clone, Copy, Debug)]
pub struct Lfo {
    /// Phase and the random values of this cycle and of the next.
    core: CyclicLfo,
    rng: Rng,
    fade: f32,
}

impl Lfo {
    /// A free-running LFO with its random numbers from `seed`.
    pub fn new(seed: u32) -> Lfo {
        let mut rng = Rng::new(seed);
        let mut core = CyclicLfo::new();
        let held = rng.bipolar();
        let next = rng.bipolar();
        core.set_random(held, next);
        Lfo {
            core,
            rng,
            fade: 1.0,
        }
    }

    /// Starts a cycle and the fade-in again (a key went down).
    pub fn restart(&mut self) {
        self.core.set_phase(0);
        self.fade = 0.0;
        let held = self.rng.bipolar();
        let next = self.rng.bipolar();
        self.core.set_random(held, next);
    }

    /// The phase increment per control interval of `rate_hz` at `sample_rate`.
    pub fn increment(rate_hz: f32, sample_rate: f32) -> u32 {
        increment(rate_hz, sample_rate / CONTROL as f32)
    }

    /// The shape of the `Shape` parameter's index: sine, triangle, saw up, square, sample and
    /// hold, and smooth random for every other index.
    #[inline]
    pub fn shape(index: usize) -> Shape {
        match index {
            0 => Shape::Sine,
            1 => Shape::Triangle,
            2 => Shape::SawUp,
            3 => Shape::Square,
            4 => Shape::SampleHold,
            _ => Shape::SmoothRandom,
        }
    }

    /// One control interval: advances by `inc`, fades in by `fade_step`, returns the value of
    /// shape index `shape` (−1 … +1, or 0 … 1 for `uni`).
    #[inline]
    pub fn step(&mut self, shape: usize, inc: u32, fade_step: f32, uni: bool) -> f32 {
        if self.core.advance(inc) {
            self.core.push_random(self.rng.bipolar());
        }
        self.fade = (self.fade + fade_step).min(1.0);
        let v = self.core.value(Self::shape(shape));
        if uni {
            (0.5 + 0.5 * v) * self.fade
        } else {
            v * self.fade
        }
    }
}
