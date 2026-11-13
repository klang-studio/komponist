//! kp-test-fx: the effect fixture for komponist's plugin hosting tests (PLAN WP-27).
//!
//! A stereo gain that reports a latency of [`LATENCY_SAMPLES`] and really has it: the output is the
//! input delayed by that many samples, times `gain`. So latency compensation tests see the delay
//! they are told about.

use nice_plug::prelude::*;
use std::sync::Arc;

/// The latency the plugin reports and applies, in samples.
pub const LATENCY_SAMPLES: u32 = 64;
const DELAY: usize = LATENCY_SAMPLES as usize;

pub struct KpTestFx {
    params: Arc<FxParams>,
    /// One ring buffer per channel; `pos` is the next slot to read and overwrite.
    delay: [[f32; DELAY]; 2],
    pos: usize,
}

/// Parameter IDs are part of saved states and must never change.
#[derive(Params)]
pub struct FxParams {
    /// Gain in dB.
    #[id = "gain"]
    pub gain: FloatParam,
}

impl Default for FxParams {
    fn default() -> Self {
        Self {
            // No smoother: a change applies exactly at the next block (predictable for tests).
            gain: FloatParam::new(
                "Gain",
                0.0,
                FloatRange::Linear {
                    min: -60.0,
                    max: 12.0,
                },
            )
            .with_step_size(0.1)
            .with_unit(" dB"),
        }
    }
}

impl Default for KpTestFx {
    fn default() -> Self {
        Self {
            params: Arc::new(FxParams::default()),
            delay: [[0.0; DELAY]; 2],
            pos: 0,
        }
    }
}

impl KpTestFx {
    pub fn params(&self) -> &FxParams {
        &self.params
    }
}

impl Plugin for KpTestFx {
    const NAME: &'static str = "kp-test-fx";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio";
    const EMAIL: &'static str = "info@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn activate(
        &mut self,
        _audio_io_layout: &AudioIOLayout,
        _buffer_config: &BufferConfig,
        context: &mut impl ActivateContext<Self>,
    ) -> bool {
        context.set_latency_samples(LATENCY_SAMPLES);
        true
    }

    fn reset(&mut self) {
        self.delay = [[0.0; DELAY]; 2];
        self.pos = 0;
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let gain = util::db_to_gain(self.params.gain.value());
        for frame in buffer.iter_samples() {
            for (line, sample) in self.delay.iter_mut().zip(frame) {
                let delayed = line[self.pos];
                line[self.pos] = *sample;
                *sample = delayed * gain;
            }
            self.pos = (self.pos + 1) % DELAY;
        }
        ProcessStatus::Tail(LATENCY_SAMPLES)
    }
}

impl ClapPlugin for KpTestFx {
    const CLAP_ID: &'static str = "de.klang-studio.kp-test-fx";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("komponist test effect: stereo gain with 64 samples latency");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Utility,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for KpTestFx {
    const VST3_CLASS_ID: [u8; 16] = *b"kpTestFx00000001";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nice_export_clap!(KpTestFx);
nice_export_vst3!(KpTestFx);

#[cfg(test)]
mod tests {
    use super::*;
    use fake_host::FakeHost;

    /// A stereo block with ramps that differ per channel.
    fn ramps(start: usize, samples: usize) -> Vec<Vec<f32>> {
        let ramp = |offset: f32| {
            (start..start + samples)
                .map(|i| i as f32 + offset)
                .collect()
        };
        vec![ramp(1.0), ramp(-0.5)]
    }

    #[test]
    fn reports_its_latency_on_activate() {
        let host = FakeHost::<KpTestFx>::activate();
        assert_eq!(host.latency(), 64);
    }

    #[test]
    fn output_is_the_input_delayed_by_the_latency() {
        let mut host = FakeHost::<KpTestFx>::activate();
        let input = ramps(0, 1000);
        let mut out = Vec::new();
        // Blocks shorter and longer than the delay, so the ring wraps inside and across blocks.
        for (start, len) in [(0, 10), (10, 300), (310, 64), (374, 626)] {
            let mut block = ramps(start, len);
            host.process(&mut block, &[]);
            out.push(block);
        }
        for channel in 0..2 {
            let joined: Vec<f32> = out.iter().flat_map(|b| b[channel].clone()).collect();
            assert!(joined[..DELAY].iter().all(|&s| s == 0.0));
            assert_eq!(joined[DELAY..], input[channel][..1000 - DELAY]);
        }
    }

    #[test]
    fn gain_scales_the_output() {
        let mut host = FakeHost::<KpTestFx>::activate();
        // Normalized 0.5 of −60…+12 dB is −24 dB.
        host.set_param(&host.plugin.params().gain, 0.5);
        assert_eq!(host.plugin.params().gain.value(), -24.0);

        let mut block = vec![vec![1.0; 128]; 2];
        host.process(&mut block, &[]);
        let expected = util::db_to_gain(-24.0);
        assert!(block[0][DELAY..].iter().all(|&s| s == expected));
    }

    #[test]
    fn reset_clears_the_delay_line() {
        let mut host = FakeHost::<KpTestFx>::activate();
        host.process(&mut vec![vec![1.0; 32]; 2], &[]);
        host.plugin.reset();
        let mut block = vec![vec![0.0; 128]; 2];
        host.process(&mut block, &[]);
        assert!(block.iter().flatten().all(|&s| s == 0.0));
    }
}
