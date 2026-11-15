//! kp-test-sc: the sidechain fixture for komponist's plugin hosting tests (#6).
//!
//! A stereo effect with a stereo sidechain input (CLAP: a second, non-main audio input port; VST3:
//! a `kAux` input bus, both named "Sidechain"). Output = main input + sidechain × `level`, sample
//! by sample, without latency. At the default level of 0 dB the factor is exactly 1, so a host
//! test can check what arrived at the sidechain port, and when: without a sidechain the output is
//! the input, on a track without other signal it is the sidechain.

use nice_plug::prelude::*;
use std::sync::Arc;

pub struct KpTestSc {
    params: Arc<ScParams>,
}

/// Parameter IDs are part of saved states and must never change.
#[derive(Params)]
pub struct ScParams {
    /// Level of the sidechain in the output, in dB.
    #[id = "level"]
    pub level: FloatParam,
}

impl Default for ScParams {
    fn default() -> Self {
        Self {
            // No smoother: a change applies exactly at the next block (predictable for tests).
            level: FloatParam::new(
                "Level",
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

impl Default for KpTestSc {
    fn default() -> Self {
        Self {
            params: Arc::new(ScParams::default()),
        }
    }
}

impl KpTestSc {
    pub fn params(&self) -> &ScParams {
        &self.params
    }
}

impl Plugin for KpTestSc {
    const NAME: &'static str = "kp-test-sc";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio";
    const EMAIL: &'static str = "info@example.com";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        aux_input_ports: &[new_nonzero_u32(2)],
        names: PortNames {
            aux_inputs: &["Sidechain"],
            ..PortNames::const_default()
        },
        ..AudioIOLayout::const_default()
    }];

    const SAMPLE_ACCURATE_AUTOMATION: bool = false;

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let level = util::db_to_gain(self.params.level.value());
        let Some(sidechain) = aux.inputs.first_mut() else {
            return ProcessStatus::Normal;
        };
        for (main, side) in buffer
            .as_slice()
            .iter_mut()
            .zip(sidechain.as_slice_immutable())
        {
            for (out, sc) in main.iter_mut().zip(side.iter()) {
                *out += sc * level;
            }
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for KpTestSc {
    const CLAP_ID: &'static str = "de.klang-studio.kp-test-sc";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("komponist test effect: adds its sidechain input to the main input");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[
        ClapFeature::AudioEffect,
        ClapFeature::Utility,
        ClapFeature::Stereo,
    ];
}

impl Vst3Plugin for KpTestSc {
    const VST3_CLASS_ID: [u8; 16] = *b"kpTestSc00000001";
    const VST3_SUBCATEGORIES: &'static [Vst3SubCategory] =
        &[Vst3SubCategory::Fx, Vst3SubCategory::Tools];
}

nice_export_clap!(KpTestSc);
nice_export_vst3!(KpTestSc);

#[cfg(test)]
mod tests {
    use super::*;
    use fake_host::FakeHost;

    fn ramp(offset: f32, samples: usize) -> Vec<f32> {
        (0..samples).map(|i| i as f32 * 0.25 + offset).collect()
    }

    #[test]
    fn has_one_stereo_sidechain_input() {
        let layout = KpTestSc::AUDIO_IO_LAYOUTS[0];
        assert_eq!(layout.aux_input_ports, &[new_nonzero_u32(2)]);
        assert_eq!(layout.aux_input_name(0).as_deref(), Some("Sidechain"));
        assert!(layout.aux_output_ports.is_empty());
    }

    #[test]
    fn adds_the_sidechain_to_the_input() {
        let mut host = FakeHost::<KpTestSc>::activate();
        let mut block = vec![ramp(1.0, 100), ramp(-3.0, 100)];
        let mut sidechain = vec![ramp(0.5, 100), ramp(7.0, 100)];
        host.process_with_sidechain(&mut block, &mut sidechain, &[]);
        for (channel, (main, side)) in [(1.0, 0.5), (-3.0, 7.0)].into_iter().enumerate() {
            let sum: Vec<f32> = ramp(main, 100)
                .iter()
                .zip(ramp(side, 100))
                .map(|(a, b)| a + b)
                .collect();
            assert_eq!(block[channel], sum);
        }
    }

    #[test]
    fn without_a_sidechain_buffer_the_input_passes() {
        let mut host = FakeHost::<KpTestSc>::activate();
        let mut block = vec![ramp(1.0, 64), ramp(2.0, 64)];
        host.process(&mut block, &[]);
        assert_eq!(block, vec![ramp(1.0, 64), ramp(2.0, 64)]);
    }

    #[test]
    fn level_scales_the_sidechain() {
        let mut host = FakeHost::<KpTestSc>::activate();
        // Normalized 0.5 of −60…+12 dB is −24 dB.
        host.set_param(&host.plugin.params().level, 0.5);
        let mut block = vec![vec![0.0; 32]; 2];
        let mut sidechain = vec![vec![1.0; 32]; 2];
        host.process_with_sidechain(&mut block, &mut sidechain, &[]);
        let expected = util::db_to_gain(-24.0);
        assert!(block.iter().flatten().all(|&s| s == expected));
    }
}
