//! Bit depth and sample rate reduction on top of [`klang_dsp::crush`].

use klang_dsp::crush::{Quantizer, SampleHold};

#[derive(Debug, Clone, Copy, Default)]
pub struct LoFi {
    hold: SampleHold<2>,
}

impl LoFi {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `bits >= 24` disables quantization, `downsample <= 1` disables sample-and-hold.
    #[inline]
    pub fn process(&mut self, bits: f32, downsample: f32, input: (f32, f32)) -> (f32, f32) {
        let mut x = [input.0, input.1];

        if downsample > 1.0 {
            x = self.hold.tick(1.0 / downsample, || x);
        }

        if bits < 24.0 {
            let q = Quantizer::new(bits);
            x = x.map(|v| q.apply(v));
        }

        (x[0], x[1])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_transparent() {
        let mut l = LoFi::default();
        assert_eq!(l.process(24.0, 1.0, (0.123, -0.456)), (0.123, -0.456));
    }

    #[test]
    fn quantizes_to_bit_depth() {
        let mut l = LoFi::default();
        // 2 bits: steps of 0.5
        assert_eq!(l.process(2.0, 1.0, (0.3, -0.8)), (0.5, -1.0));
    }

    #[test]
    fn holds_samples() {
        let mut l = LoFi::default();
        let out: Vec<f32> = (0..4)
            .map(|i| l.process(24.0, 2.0, (i as f32, 0.0)).0)
            .collect();
        // The first sample is taken right away, then every second one.
        assert_eq!(out, vec![0.0, 1.0, 1.0, 3.0]);
    }
}
