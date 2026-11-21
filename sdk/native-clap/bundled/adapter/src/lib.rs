//! Adapts the shared device specs and DSP to the public native CLAP SDK.

mod instrument;
mod midi_effect;
mod parameter;

use std::ffi::{CStr, c_void};
use std::sync::Arc;

use komponist_device_core::{Dsp, SAMPLE_RATES, Spec};
use komponist_i18n::Lang;
use komponist_model::params::ParamInfo;
use komponist_native_clap_sdk::{DeviceLayout, NativeClapLayout, PageLabels, Panel};
use nice_plug::prelude::*;

pub use instrument::Instrument;
pub use midi_effect::MidiEffect;
pub use parameter::{Binding, Parameters};

const STEREO: NonZeroU32 = NonZeroU32::new(2).unwrap();

/// A spec-based device and its stable CLAP identity.
pub trait DeviceDefinition: Send + 'static {
    const SPEC: &'static Spec;
    const ID: &'static str;
    const NAME: &'static str;
    const SIDECHAIN: bool = false;
    /// A parameter that can only take effect on reactivation because it changes latency.
    const LATENCY_PARAMETER: Option<&'static str> = None;
    const VERSION: &'static str;
    const FEATURES: &'static [ClapFeature];

    /// Explicit enum types set stable choice IDs. Continuous, integer, and boolean parameters
    /// are derived from the device spec.
    fn choice(_info: &'static ParamInfo) -> Option<Binding> {
        None
    }

    /// Override only when a device has additional controls outside the public v1 layout.
    fn layout(lang: Lang) -> DeviceLayout {
        Self::SPEC.layout_in(lang).clone()
    }

    /// Adjust labels when a native device card uses view switches outside public v1.
    fn configure_panel(_panel: &mut Panel) {}
}

/// Shared definition for spec-based audio effects.
pub use DeviceDefinition as EffectDefinition;

/// A regular stereo CLAP effect backed by an existing device processor.
pub struct Effect<T> {
    params: Arc<parameter::Parameters>,
    dsp: Box<dyn Dsp>,
    applied: Vec<Option<f64>>,
    input: [Vec<f32>; 2],
    latency_param: Option<usize>,
    restart_requested: bool,
    definition: std::marker::PhantomData<T>,
}

impl<T: EffectDefinition> Default for Effect<T> {
    fn default() -> Self {
        let params = parameter::Parameters::new(T::SPEC, T::choice);
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(params),
            dsp: (T::SPEC.dsp)(),
            input: Default::default(),
            latency_param: T::LATENCY_PARAMETER.map(|id| {
                T::SPEC
                    .wire(&komponist_model::params::ParamId::new(id))
                    .unwrap_or_else(|| panic!("unknown latency parameter {id}"))
                    as usize
            }),
            restart_requested: false,
            definition: std::marker::PhantomData,
        }
    }
}

impl<T: EffectDefinition> Effect<T> {
    fn apply_params(&mut self, activating: bool) {
        for (index, applied) in self.applied.iter_mut().enumerate() {
            if (activating || self.latency_param != Some(index))
                && let Some(value) = self.params.changed_value(index, applied)
            {
                self.dsp.set_param(index, value);
            }
        }
    }
}

impl<T: EffectDefinition> Plugin for Effect<T> {
    const NAME: &'static str = T::NAME;
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = T::VERSION;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = if T::SIDECHAIN {
        &[
            AudioIOLayout {
                main_input_channels: Some(STEREO),
                main_output_channels: Some(STEREO),
                aux_input_ports: &[STEREO],
                ..AudioIOLayout::const_default()
            },
            AudioIOLayout {
                main_input_channels: Some(STEREO),
                main_output_channels: Some(STEREO),
                ..AudioIOLayout::const_default()
            },
        ]
    } else {
        &[AudioIOLayout {
            main_input_channels: Some(STEREO),
            main_output_channels: Some(STEREO),
            ..AudioIOLayout::const_default()
        }]
    };

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn params(&self) -> Arc<dyn Params> {
        self.params.clone()
    }

    fn activate(
        &mut self,
        _layout: &AudioIOLayout,
        config: &BufferConfig,
        context: &mut impl ActivateContext<Self>,
    ) -> bool {
        if !SAMPLE_RATES.contains(&config.sample_rate) {
            return false;
        }

        for input in &mut self.input {
            input.resize(config.max_buffer_size as usize, 0.0);
        }
        self.apply_params(true);
        self.restart_requested = false;
        self.dsp.activate(config.sample_rate);
        context.set_latency_samples(self.dsp.latency());
        true
    }

    fn reset(&mut self) {
        self.apply_params(false);
        self.dsp.reset();
    }

    fn process(
        &mut self,
        buffer: &mut Buffer,
        aux: &mut AuxiliaryBuffers,
        context: &mut impl ProcessContext<Self>,
    ) -> ProcessStatus {
        self.apply_params(false);
        if !self.restart_requested
            && let Some(index) = self.latency_param
            && self.applied[index] != Some(self.params.raw_value(index))
        {
            // Until the host reactivates us, retain the old DSP delay and advertised latency.
            self.restart_requested = true;
            context.request_restart();
        }
        if let Some(tempo) = context.transport().tempo {
            self.dsp.set_tempo(tempo);
        }
        let count = buffer.samples();
        let [left, right] = buffer.as_slice() else {
            return ProcessStatus::Error("expected stereo main outputs");
        };
        let [input_l, input_r] = &mut self.input;
        input_l[..count].copy_from_slice(left);
        input_r[..count].copy_from_slice(right);
        let input = [&input_l[..count], &input_r[..count]];
        let output = [&mut **left, &mut **right];

        if let Some(sidechain) = aux.inputs.first() {
            let [sidechain_l, sidechain_r] = sidechain.as_slice_immutable() else {
                return ProcessStatus::Error("expected stereo sidechain inputs");
            };
            self.dsp
                .process_sidechain(input, [sidechain_l, sidechain_r], output);
        } else {
            self.dsp.process(input, output);
        }
        // Keep delay lines and tails running when a quiet block precedes an audible one.
        ProcessStatus::KeepAlive
    }
}

impl<T: EffectDefinition> ClapPlugin for Effect<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl<T: EffectDefinition> NativeClapLayout for Effect<T> {
    fn panel() -> Panel {
        panel_for::<T>()
    }
}

fn panel_for<T: DeviceDefinition>() -> Panel {
    let mut panel = panel_from_parts(
        T::layout,
        T::SPEC.params_in(Lang::En),
        T::SPEC.params_in(Lang::De),
    );
    T::configure_panel(&mut panel);
    panel
}

/// Construct the bilingual public SDK panel from a device's model metadata.
pub fn panel_from_parts(
    layout: impl Fn(Lang) -> DeviceLayout,
    english_params: &[ParamInfo],
    german_params: &[ParamInfo],
) -> Panel {
    let english = layout(Lang::En);
    let german = layout(Lang::De);
    let label =
        |en: &str, de: &str| [("en".into(), en.to_owned()), ("de".into(), de.to_owned())].into();
    let pages = english
        .pages
        .iter()
        .zip(&german.pages)
        .enumerate()
        .map(|(index, (en, de))| PageLabels {
            key: format!("page-{index}"),
            labels: label(
                if en.name.is_empty() { "Main" } else { &en.name },
                if de.name.is_empty() {
                    "Hauptseite"
                } else {
                    &de.name
                },
            ),
        })
        .collect();
    let mut parameter_labels = std::collections::BTreeMap::new();
    let mut choice_labels = std::collections::BTreeMap::new();
    for (en, de) in english_params.iter().zip(german_params) {
        parameter_labels.insert(en.id.clone(), label(&en.name, &de.name));
        if let (
            komponist_model::params::ParamKind::Enum(en_choices),
            komponist_model::params::ParamKind::Enum(de_choices),
        ) = (&en.kind, &de.kind)
        {
            choice_labels.insert(
                en.id.clone(),
                en_choices
                    .iter()
                    .zip(de_choices)
                    .map(|(en, de)| label(en, de))
                    .collect(),
            );
        }
    }
    Panel {
        default_locale: "en".into(),
        locales: vec!["en".into(), "de".into()],
        grid: english,
        pages,
        parameter_labels,
        choice_labels,
    }
}
