//! Adapts the shared device specs and DSP to the public native CLAP SDK.

mod instrument;
mod meter;
mod midi_effect;
mod parameter;
#[cfg(test)]
mod parity_tests;
#[doc(hidden)]
pub mod presets;

use std::ffi::{CStr, c_void};
use std::sync::Arc;

use komponist_device_core::{CatalogSpec, Dsp, SAMPLE_RATES, Spec};
use komponist_i18n::Lang;
use komponist_model::params::{ParamId, ParamInfo, ParamKind, Scale, Unit};
use komponist_native_clap_sdk::{
    DeviceLayout, DisplayScale, DisplayUnit, NativeClapLayout, PageLabels, Panel, PanelV2,
    ParameterDisplay, PluginLabels,
};
use nice_plug::prelude::*;

pub use instrument::{Instrument, to_device_note};
pub use komponist_native_clap_sdk::signal_taps;
pub use midi_effect::MidiEffect;
pub use parameter::{Binding, Choice, Parameters, assert_choice_bindings};

const STEREO: NonZeroU32 = NonZeroU32::new(2).unwrap();

/// A spec-based device and its stable CLAP identity.
pub trait DeviceDefinition: Send + 'static {
    const SPEC: &'static Spec;
    const ID: &'static str;
    const NAME: &'static str;
    const SIDECHAIN: bool = false;
    /// Let the host supply the detector signal from the main input without a sidechain source.
    const SIDECHAIN_FROM_MAIN: bool = false;
    /// A parameter that can only take effect on reactivation because it changes latency.
    const LATENCY_PARAMETER: Option<&'static str> = None;
    /// Publish true DSP gain reduction to hosts that request effect meters.
    const REPORTS_REDUCTION: bool = false;
    /// Enables standard preset loading for devices that export a discovery provider.
    const PUBLISH_FACTORY_PRESETS: bool = false;
    const VERSION: &'static str;
    const FEATURES: &'static [ClapFeature];

    /// Explicit enum types set stable choice IDs. Continuous, integer, and boolean parameters
    /// are derived from the device spec.
    fn choice(_info: &'static ParamInfo) -> Option<Binding> {
        None
    }

    /// Rewrite the saved state of an older release before it is applied, e.g. to rename a stable
    /// choice ID. Runs on every restore, so the rewrite must not touch current IDs.
    fn migrate_state(_state: &mut PluginState) {}

    /// Override only when a device has additional controls outside the public v1 layout.
    fn layout(lang: Lang) -> DeviceLayout {
        Self::SPEC.layout_in(lang).clone()
    }

    /// Adjust labels when a native device card uses view switches outside public v1.
    fn configure_panel(_panel: &mut Panel) {}
}

/// Builds the DSP for an effect or instrument.
pub trait AudioDefinition: DeviceDefinition {
    /// Named streams backed by the DSP's audio tap or gain-reduction reporting.
    const SIGNAL_TAPS: &'static [signal_taps::Descriptor] = &[];

    /// An authored version 2 panel, when the device supplies one.
    fn panel_v2() -> Option<PanelV2> {
        None
    }

    fn dsp() -> Box<dyn Dsp>;
}

/// Builds the event processor for a MIDI effect.
pub trait MidiDefinition: DeviceDefinition {
    fn event_stage() -> Box<dyn komponist_engine::EventProcessor + Send>;
}

/// A regular stereo CLAP effect backed by an existing device processor.
pub struct Effect<T> {
    params: Arc<parameter::Parameters>,
    dsp: Box<dyn Dsp>,
    applied: Vec<Option<f64>>,
    input: [Vec<f32>; 2],
    latency_param: Option<usize>,
    restart_requested: bool,
    meter: Option<Arc<meter::Meter>>,
    definition: std::marker::PhantomData<T>,
}

impl<T: AudioDefinition> Default for Effect<T> {
    fn default() -> Self {
        let params = parameter::Parameters::new(T::SPEC, T::choice);
        let dsp = T::dsp();
        let meter = (T::REPORTS_REDUCTION || !T::SIGNAL_TAPS.is_empty())
            .then(|| Arc::new(meter::Meter::new(dsp.tap())));
        Self {
            applied: vec![None; params.len()],
            params: Arc::new(params),
            dsp,
            meter,
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

impl<T: AudioDefinition> Effect<T> {
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

impl<T: AudioDefinition> Plugin for Effect<T> {
    const NAME: &'static str = T::NAME;
    const VENDOR: &'static str = "klang-studio";
    const URL: &'static str = "https://github.com/klang-studio/komponist";
    const EMAIL: &'static str = "";
    const VERSION: &'static str = T::VERSION;
    const SAMPLE_ACCURATE_AUTOMATION: bool = true;

    const AUDIO_IO_LAYOUTS: &'static [AudioIOLayout] = {
        const MAIN: AudioIOLayout = AudioIOLayout {
            main_input_channels: Some(STEREO),
            main_output_channels: Some(STEREO),
            ..AudioIOLayout::const_default()
        };
        const AUX: AudioIOLayout = AudioIOLayout {
            aux_input_ports: &[STEREO],
            ..MAIN
        };
        if !T::SIDECHAIN {
            &[MAIN]
        } else if T::SIDECHAIN_FROM_MAIN {
            // The ordinary CLAP host starts with stereo main only. Komponist selects config 1
            // before activation and manages the detector source when routing changes.
            &[MAIN, AUX]
        } else {
            &[AUX, MAIN]
        }
    };

    type Editor = ();
    type SysExMessage = ();
    type BackgroundTask = ();

    fn filter_state(state: &mut PluginState) {
        T::migrate_state(state);
    }

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
        if let Some(meter) = &self.meter {
            meter.set_rate(config.sample_rate);
        }
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
        if T::REPORTS_REDUCTION
            && let Some(meter) = &self.meter
        {
            let reduction = self.dsp.take_reduction_db();
            meter.record(reduction);
            meter.record_named_reduction(count as u32, reduction);
        }
        // Keep delay lines and tails running when a quiet block precedes an audible one.
        ProcessStatus::KeepAlive
    }
}

impl<T: AudioDefinition> ClapPlugin for Effect<T> {
    const CLAP_ID: &'static str = T::ID;
    const CLAP_DESCRIPTION: Option<&'static str> = None;
    const CLAP_MANUAL_URL: Option<&'static str> = None;
    const CLAP_SUPPORT_URL: Option<&'static str> = None;
    const CLAP_FEATURES: &'static [ClapFeature] = T::FEATURES;

    fn clap_custom_extension_data(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.meter.as_ref().map(|meter| meter.clone() as Arc<_>)
    }

    unsafe fn clap_custom_extension(id: &CStr) -> *const c_void {
        let preset = crate::presets::load_extension::<T>(id);
        if !preset.is_null() {
            return preset;
        }
        let named_taps = meter::signal_tap_extension::<T>(id);
        if !named_taps.is_null() {
            return named_taps;
        }
        let meter = meter::extension::<T>(id);
        if !meter.is_null() {
            return meter;
        }
        if T::SIDECHAIN && T::SIDECHAIN_FROM_MAIN {
            let fallback = komponist_native_clap_sdk::sidechain_fallback_to_main::<1>(id);
            if !fallback.is_null() {
                return fallback;
            }
        }
        komponist_native_clap_sdk::extension::<Self>(id)
    }
}

impl<T: AudioDefinition> NativeClapLayout for Effect<T> {
    fn panel() -> Panel {
        panel_for::<T>()
    }

    fn panel_v2() -> Option<PanelV2> {
        let mut panel = T::panel_v2()?;
        panel.panel.device_labels = Some(device_labels(&T::SPEC.catalog));
        Some(panel)
    }
}

/// Describe supported physical parameters using the normalized CLAP adapter's display domains.
pub fn parameter_display(
    params: &[ParamInfo],
) -> std::collections::BTreeMap<ParamId, ParameterDisplay> {
    params
        .iter()
        .filter_map(|param| {
            let ParamKind::Float {
                min,
                max,
                scale,
                steps: 0,
            } = param.kind
            else {
                return None;
            };
            if param.format.is_some() {
                return None;
            }
            let scale = match scale {
                Scale::Lin => DisplayScale::Linear,
                Scale::Log => DisplayScale::Logarithmic,
                _ => return None,
            };
            let unit = match param.unit {
                Unit::Hz => DisplayUnit::Hertz,
                Unit::Ms => DisplayUnit::Milliseconds,
                Unit::Percent => DisplayUnit::Percent,
                Unit::None => DisplayUnit::None,
                Unit::Db => DisplayUnit::Decibels,
                Unit::DbWidth => DisplayUnit::DecibelWidth,
                Unit::Ratio => DisplayUnit::Ratio,
                _ => return None,
            };
            ParameterDisplay::new(min, max, param.default, scale, unit)
                .map(|display| (param.id.clone(), display))
        })
        .collect()
}

pub fn panel_for<T: DeviceDefinition>() -> Panel {
    let mut panel = panel_from_parts(
        T::layout,
        T::SPEC.params_in(Lang::En),
        T::SPEC.params_in(Lang::De),
    );
    panel.device_labels = Some(device_labels(&T::SPEC.catalog));
    T::configure_panel(&mut panel);
    panel
}

/// Publish the device's catalog labels with its CLAP panel.
pub fn device_labels(catalog: &CatalogSpec) -> PluginLabels {
    let labels = |text: fn(Lang) -> &'static str| {
        Lang::ALL
            .into_iter()
            .map(|lang| (lang.code().into(), text(lang).into()))
            .collect()
    };
    PluginLabels {
        names: labels(catalog.name_in),
        descriptions: catalog.description_in.map_or_else(Default::default, labels),
    }
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
    for en in english_params {
        let de = german_params
            .iter()
            .find(|param| param.id == en.id)
            .unwrap_or(en);
        parameter_labels.insert(en.id.clone(), label(&en.name, &de.name));
        if let (Some(en_choices), Some(de_choices)) = (en.switch_choices(), de.switch_choices()) {
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
        device_labels: None,
    }
}
