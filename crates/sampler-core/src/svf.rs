//! Stereo state variable filter on top of [`klang_dsp::svf`].

use klang_dsp::svf::SvfCoefs;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SvfMode {
    #[default]
    LowPass,
    HighPass,
    BandPass,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Svf {
    channels: [klang_dsp::svf::Svf; 2],
}

impl Svf {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Processes one stereo frame. `resonance` is in `[0, 1]`.
    #[inline]
    pub fn process(
        &mut self,
        mode: SvfMode,
        cutoff_hz: f32,
        resonance: f32,
        sample_rate: f32,
        input: (f32, f32),
    ) -> (f32, f32) {
        // Q from 0.5 to ~25
        let k = 2.0 - 1.92 * resonance.clamp(0.0, 1.0);
        let c = SvfCoefs::from_cutoff(cutoff_hz.max(20.0), sample_rate, k);

        let [left, right] = &mut self.channels;
        let filter = |svf: &mut klang_dsp::svf::Svf, v0| {
            let out = svf.tick(v0, &c);
            match mode {
                SvfMode::LowPass => out.low(),
                SvfMode::BandPass => out.band(),
                SvfMode::HighPass => out.high(),
            }
        };

        (filter(left, input.0), filter(right, input.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn rms_of_sine(mode: SvfMode, cutoff: f32, freq: f32) -> f32 {
        let sr = 48000.0;
        let mut f = Svf::default();
        let n = 48000;
        let mut acc = 0.0;
        for i in 0..n {
            let x = (2.0 * PI * freq * i as f32 / sr).sin();
            let (y, _) = f.process(mode, cutoff, 0.0, sr, (x, x));
            if i > n / 2 {
                acc += y * y;
            }
        }
        (acc / (n / 2) as f32).sqrt()
    }

    #[test]
    fn lowpass_attenuates_highs() {
        assert!(rms_of_sine(SvfMode::LowPass, 200.0, 5000.0) < 0.01);
        assert!(rms_of_sine(SvfMode::LowPass, 5000.0, 100.0) > 0.6);
    }

    #[test]
    fn highpass_attenuates_lows() {
        assert!(rms_of_sine(SvfMode::HighPass, 5000.0, 100.0) < 0.01);
    }
}
