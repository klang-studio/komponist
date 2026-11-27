//! The LFO as it was before #362 (`Lfo` of `modulation.rs` at commit `01db1cd8`), copied verbatim
//! as a differential oracle: `lfo_baseline` drives it and the production
//! [`crate::modulation::Lfo`] with the same inputs and compares their outputs bit for bit. Never
//! change the logic below; it pins the behaviour the LFO adoption (#362) must keep.

use klang_dsp::{phase, wave};

use crate::modulation::{LfoShape, Rng};

/// A bipolar LFO on a `u32` phase (one period = 2^32, see [`klang_dsp::phase`]).
#[derive(Debug, Clone, Copy, Default)]
pub struct Lfo {
    phase: u32,
    held: f32,
}

impl Lfo {
    pub fn reset(&mut self, phase: f32, rng: &mut Rng) {
        self.phase = phase::from_unit(phase);
        self.held = rng.bipolar();
    }

    /// The value at the current phase without advancing.
    #[inline]
    pub fn value(&self, shape: LfoShape) -> f32 {
        match shape {
            LfoShape::Sine => wave::sine(self.phase),
            LfoShape::Triangle => wave::triangle(self.phase),
            LfoShape::SawUp => wave::saw_up(self.phase),
            LfoShape::SawDown => wave::saw_down(self.phase),
            LfoShape::Square => wave::square(self.phase),
            LfoShape::SampleHold => self.held,
        }
    }

    /// Returns the current value and advances by one sample.
    #[inline]
    pub fn next(&mut self, shape: LfoShape, freq_hz: f32, sample_rate: f32, rng: &mut Rng) -> f32 {
        let out = self.value(shape);
        let old = self.phase;
        self.phase = old.wrapping_add(phase::increment(freq_hz, sample_rate));
        if self.phase < old {
            self.held = rng.bipolar();
        }
        out
    }
}
