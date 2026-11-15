//! Stereo biquad on top of [`klang_dsp::biquad`], used for the per-pad parametric EQ band.

pub use klang_dsp::biquad::BiquadCoefs as BiquadCoeffs;

/// Peaking EQ band: flat below 0.01 dB of gain, frequency kept within 20 Hz and 0.45 × the
/// sample rate.
pub fn peaking(freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> BiquadCoeffs {
    if gain_db.abs() < 0.01 {
        return BiquadCoeffs::IDENTITY;
    }

    BiquadCoeffs::peaking(
        freq.clamp(20.0, sample_rate * 0.45),
        q.max(0.05),
        gain_db,
        sample_rate,
    )
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    channels: [klang_dsp::biquad::Biquad; 2],
}

impl Biquad {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    #[inline]
    pub fn process(&mut self, c: &BiquadCoeffs, input: (f32, f32)) -> (f32, f32) {
        if c.is_identity() {
            return input;
        }

        let [left, right] = &mut self.channels;
        (left.tick(input.0, c), right.tick(input.1, c))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn peak_boosts_at_center_frequency() {
        let sr = 48000.0;
        let c = peaking(1000.0, 1.0, 12.0, sr);
        let mut f = Biquad::default();
        let mut peak = 0.0f32;
        for i in 0..48000 {
            let x = (2.0 * PI * 1000.0 * i as f32 / sr).sin();
            let (y, _) = f.process(&c, (x, x));
            if i > 24000 {
                peak = peak.max(y.abs());
            }
        }
        // +12 dB is a factor of ~3.98
        assert!((peak - 3.98).abs() < 0.1, "peak was {peak}");
    }
}
