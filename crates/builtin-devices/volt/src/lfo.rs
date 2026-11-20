//! The LFOs: six shapes on a `u32` phase (`klang_dsp::wave`), stepped once per control interval,
//! with a fade-in and an optional unipolar output.

use klang_dsp::phase::{increment, to_unit};
use klang_dsp::rng::Rng;
use klang_dsp::wave;

use komponist_device_core::dsp::CONTROL;

/// One LFO's state (a voice has two; two more run free for the LFOs that do not restart on a
/// key).
#[derive(Clone, Copy, Debug)]
pub struct Lfo {
    phase: u32,
    rng: Rng,
    /// The random value of this cycle and of the next.
    held: f32,
    next: f32,
    fade: f32,
}

impl Lfo {
    /// A free-running LFO with its random numbers from `seed`.
    pub fn new(seed: u32) -> Lfo {
        let mut rng = Rng::new(seed);
        Lfo {
            phase: 0,
            held: rng.bipolar(),
            next: rng.bipolar(),
            rng,
            fade: 1.0,
        }
    }

    /// Starts a cycle and the fade-in again (a key went down).
    pub fn restart(&mut self) {
        self.phase = 0;
        self.fade = 0.0;
        self.held = self.rng.bipolar();
        self.next = self.rng.bipolar();
    }

    /// The phase increment per control interval of `rate_hz` at `sample_rate`.
    pub fn increment(rate_hz: f32, sample_rate: f32) -> u32 {
        increment(rate_hz, sample_rate / CONTROL as f32)
    }

    /// One control interval: advances by `inc`, fades in by `fade_step`, returns the value (−1 …
    /// +1, or 0 … 1 for `uni`).
    #[inline]
    pub fn step(&mut self, shape: usize, inc: u32, fade_step: f32, uni: bool) -> f32 {
        let (phase, wrapped) = self.phase.overflowing_add(inc);
        self.phase = phase;
        if wrapped {
            self.held = self.next;
            self.next = self.rng.bipolar();
        }
        self.fade = (self.fade + fade_step).min(1.0);
        let v = match shape {
            0 => wave::sine(phase),
            1 => wave::triangle(phase),
            2 => wave::saw_up(phase),
            3 => wave::square(phase),
            4 => self.held,
            _ => {
                let t = to_unit(phase);
                let s = t * t * (3.0 - 2.0 * t);
                self.held + (self.next - self.held) * s
            }
        };
        if uni {
            (0.5 + 0.5 * v) * self.fade
        } else {
            v * self.fade
        }
    }
}
