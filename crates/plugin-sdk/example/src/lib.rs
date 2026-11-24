//! A stereo gain effect with a bilingual native componist device panel.

use std::sync::Arc;

use nice_plug::prelude::*;

#[cfg(feature = "native-layout")]
mod native;

#[derive(Params)]
struct GainParams {
    #[id = "gain"]
    gain: FloatParam,
    #[id = "mix"]
    mix: FloatParam,
}

impl Default for GainParams {
    fn default() -> Self {
        Self {
            gain: FloatParam::new(
                "Gain",
                0.0,
                FloatRange::Linear {
                    min: -36.0,
                    max: 12.0,
                },
            )
            .with_unit(" dB"),
            mix: FloatParam::new("Mix", 1.0, FloatRange::Linear { min: 0.0, max: 1.0 }),
        }
    }
}

#[derive(Default)]
struct NativeGain {
    params: Arc<GainParams>,
}

impl Plugin for NativeGain {
    const NAME: &'static str = "Native Gain Example";
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = &[AudioIOLayout {
        main_input_channels: NonZeroU32::new(2),
        main_output_channels: NonZeroU32::new(2),
        ..AudioIOLayout::const_default()
    }];

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        _aux: &mut AuxiliaryBuffers,
        _context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        let mix = self.params.mix.value();
        let multiplier = 1.0 + mix * (util::db_to_gain(self.params.gain.value()) - 1.0);
        for frame in buffer.iter_samples() {
            for sample in frame {
                *sample *= multiplier;
            }
        }
        ProcessStatus::Normal
    }
}

impl ClapPlugin for NativeGain {
    const CLAP_ID: &'static str = "org.klang-studio.komponist.native-clap-example";
    const CLAP_DESCRIPTION: Option<&'static str> =
        Some("Stereo gain with a bilingual device panel");
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = &[ClapFeature::AudioEffect, ClapFeature::Stereo];

    #[cfg(feature = "native-layout")]
    fn clap_custom_extension_data(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        Some(self.params.clone())
    }

    #[cfg(feature = "native-layout")]
    unsafe fn clap_custom_extension(id: &std::ffi::CStr) -> *const std::ffi::c_void {
        if id == komponist_native_clap_sdk::display::EXTENSION_ID {
            return komponist_native_clap_sdk::display::extension::<Self, GainParams>(id);
        }
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

nice_export_clap!(NativeGain);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_unity_and_full_mix() {
        let params = GainParams::default();
        assert_eq!(params.gain.value(), 0.0);
        assert_eq!(params.gain.default_normalized_value(), 0.75);
        assert_eq!(params.mix.value(), 1.0);
        assert_eq!(params.mix.default_normalized_value(), 1.0);
    }
}
