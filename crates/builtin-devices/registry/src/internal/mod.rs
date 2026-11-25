//! Registry for the built-ins that share the device core's processor and state format.

use komponist_device_core::{CatalogSpec, InternalProcessor, Preset, Spec, StateError};
use komponist_i18n::Lang;
use komponist_model::params::layout::DeviceLayout;
use komponist_model::params::{ParamId, ParamInfo};
use komponist_model::{DeviceRole, PluginFormat, PluginRef};

use komponist_sampler::SamplerHandle;

#[cfg(test)]
use komponist_device_core::{Dsp, dsp};
#[cfg(test)]
use {komponist_eq as eq, komponist_presse as presse};

/// The internal devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Internal {
    /// Drum sampler: eight pads, an instrument (WP-87).
    Sampler,
    /// Parametric EQ.
    Eq,
    /// Stereo delay.
    Delay,
    /// Reverb.
    Hall,
    /// Peak limiter; an ordinary device, also for the master chain.
    Limiter,
    /// Compressor „presse“ (WP-93).
    Presse,
    /// MIDI effect: held notes as a pattern in time with the song (WP-88).
    Arpeggiator,
    /// MIDI effect: forces notes onto a scale (WP-88).
    Tonleiter,
    /// Virtual-analog synthesizer, an instrument (WP-95).
    Volt,
    /// Wavetable synthesizer, an instrument on volt's engine (#107).
    Surf,
    /// Chromatic sampler with waveform editing and slice playback.
    GeneralSampler,
}

impl Internal {
    /// All devices, in browser order.
    pub const ALL: [Internal; 11] = [
        Internal::Sampler,
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
        Internal::Presse,
        Internal::Arpeggiator,
        Internal::Tonleiter,
        Internal::Volt,
        Internal::Surf,
        Internal::GeneralSampler,
    ];

    fn spec(self) -> &'static Spec {
        match self {
            Internal::Sampler => &komponist_sampler::SPEC,
            Internal::GeneralSampler => &komponist_general_sampler::SPEC,
            Internal::Eq => &komponist_eq::SPEC,
            Internal::Delay => &komponist_delay::SPEC,
            Internal::Hall => &komponist_reverb::SPEC,
            Internal::Limiter => &komponist_limiter::SPEC,
            Internal::Presse => &komponist_presse::SPEC,
            Internal::Arpeggiator => &komponist_arpeggiator::SPEC,
            Internal::Tonleiter => &komponist_scale::SPEC,
            Internal::Volt => &komponist_volt::SPEC,
            Internal::Surf => &komponist_surf::SPEC,
        }
    }

    pub(crate) fn catalog(self) -> &'static CatalogSpec {
        &self.spec().catalog
    }

    /// Whether this device rewrites note events in front of the instrument.
    pub fn is_midi_effect(self) -> bool {
        self.role() == DeviceRole::MidiEffect
    }

    /// The role of the device in a chain.
    pub fn role(self) -> DeviceRole {
        self.spec().catalog.role
    }

    /// Whether this is an instrument: notes in, audio out.
    pub fn is_instrument(self) -> bool {
        self.role() == DeviceRole::Instrument
    }

    /// Whether the device has a sidechain input (the "SC" chip in its head, WP-104).
    pub fn has_sidechain(self) -> bool {
        self.spec().catalog.sidechain
    }

    /// The catalog id.
    pub fn id(self) -> &'static str {
        self.spec().catalog.id
    }

    /// The name stored in projects, which is also the German name. Shown names come from
    /// [`Internal::name_in`].
    pub fn name(self) -> &'static str {
        self.spec().catalog.name
    }

    /// The name shown in the browser and on the card in `lang` ("Hall" is "Reverb" in English;
    /// brand names such as hexa, volt and presse stay).
    pub fn name_in(self, lang: Lang) -> &'static str {
        (self.spec().catalog.name_in)(lang)
    }

    /// The browser features (CLAP style: `equalizer`, `delay`, `reverb`, `limiter`).
    pub fn features(self) -> &'static [&'static str] {
        self.spec().catalog.features
    }

    /// The device of catalog id `id`.
    pub fn from_id(id: &str) -> Option<Internal> {
        Internal::ALL.into_iter().find(|k| k.id() == id)
    }

    /// The device a plugin reference names (format built-in, a known id).
    pub fn from_plugin(plugin: &PluginRef) -> Option<Internal> {
        (plugin.format == PluginFormat::Builtin)
            .then(|| Internal::from_id(&plugin.id))
            .flatten()
    }

    /// The reference a device in a project stores.
    pub fn plugin_ref(self) -> PluginRef {
        self.spec().catalog.plugin_ref()
    }

    /// The parameters in German; the index is the wire id. Ids, kinds, ranges and defaults are
    /// the same in every language, so code that does not show a text uses this; views use
    /// [`Internal::params_in`].
    pub fn params(self) -> &'static [ParamInfo] {
        self.spec().params()
    }

    /// The parameters with the names, choices and value texts of `lang`; the index is the wire id.
    pub fn params_in(self, lang: Lang) -> &'static [ParamInfo] {
        self.spec().params_in(lang)
    }

    /// The layout of the device frame's body in German (see [`Internal::layout_in`]).
    pub fn layout(self) -> &'static DeviceLayout {
        self.spec().layout()
    }

    /// The layout of the device frame's body with the page names, graph titles and choice labels
    /// of `lang`.
    pub fn layout_in(self, lang: Lang) -> &'static DeviceLayout {
        self.spec().layout_in(lang)
    }

    /// Shipped v1 bundles whose card used the full grid from the legacy registry.
    pub fn clap_v1_grid_fallback(self) -> bool {
        self.clap_v2_legacy_parameters()
            || matches!(self, Self::Volt | Self::Surf | Self::Eq | Self::Presse)
    }

    /// Bundles whose native v2 grid previously shared the legacy physical parameter model.
    pub fn clap_v2_legacy_parameters(self) -> bool {
        matches!(self, Self::Delay | Self::Limiter)
    }

    /// The factory presets in menu order (empty: the device has none).
    pub fn presets(self) -> &'static [Preset] {
        self.spec().presets()
    }

    /// Names of factory presets saved by older projects and their current names.
    pub fn preset_aliases(self) -> &'static [(&'static str, &'static str)] {
        self.spec().preset_aliases
    }

    /// All plain values (by wire id) of preset `index`: the defaults with the preset's changes
    /// (snapped). `None` if there is no such preset.
    pub fn preset_values(self, index: usize) -> Option<Vec<f64>> {
        self.spec().preset_values(index)
    }

    /// The device state after loading preset `index` into a device with state `old`: the preset's
    /// values plus its name under `"preset"`, and the other keys of `old` kept (the sampler's
    /// links). `None` if there is no such preset.
    pub fn preset_state(self, old: Option<&[u8]>, index: usize) -> Option<Vec<u8>> {
        self.spec().preset_state(old, index)
    }

    /// The preset a device state says it loaded (`"preset"`) and whether the values differ from it
    /// now („ *“). `None` for a state without a (known) preset, as every project saved before
    /// WP-96. A name from before the presets were renamed in English finds
    /// its preset.
    pub fn loaded_preset(self, state: Option<&[u8]>) -> Option<(usize, bool)> {
        self.spec().loaded_preset(state)
    }

    /// The wire id of parameter `id`.
    pub fn wire(self, id: &ParamId) -> Option<u32> {
        self.spec().wire(id)
    }

    /// The default plain values, by wire id.
    pub fn defaults(self) -> Vec<f64> {
        self.spec().defaults()
    }

    /// Decodes a device state into plain values by wire id; `None` (a device without state) gives
    /// the defaults.
    pub fn decode_state(self, state: Option<&[u8]>) -> Result<Vec<f64>, StateError> {
        self.spec().decode_state(state)
    }

    /// Encodes plain values (by wire id; missing ones are the default) as device state.
    pub fn encode_state(self, values: &[f64]) -> Vec<u8> {
        self.spec().encode_state(values)
    }

    /// Why `state` cannot be read (see [`Spec::state_error`]); the processor plays the defaults then.
    pub fn state_error(self, state: Option<&[u8]>) -> Option<StateError> {
        self.spec().state_error(state)
    }

    /// `state` with parameter `wire` set to `value` (a state without `params` counts as the
    /// defaults). `None` for an unknown wire id or an unreadable state.
    pub fn with_value(self, state: Option<&[u8]>, wire: u32, value: f64) -> Option<Vec<u8>> {
        self.spec().with_value(state, wire, value)
    }

    /// `encoded` (a state of just parameters) plus every other top-level key of `old` (the
    /// sampler's sample links), so a knob turn does not lose them.
    pub fn keep_extras(self, old: Option<&[u8]>, encoded: Vec<u8>) -> Vec<u8> {
        self.spec().keep_extras(old, encoded)
    }

    /// A processor with the plain values by wire ID. Activate it before sending it to the engine.
    pub fn processor(self, values: &[f64]) -> InternalProcessor {
        let dsp = match self {
            Self::Arpeggiator => {
                return InternalProcessor::from_event_stage(
                    self.spec(),
                    values,
                    komponist_arpeggiator::stage(),
                );
            }
            Self::Tonleiter => {
                return InternalProcessor::from_event_stage(
                    self.spec(),
                    values,
                    komponist_scale::stage(),
                );
            }
            Self::Delay => komponist_delay::dsp(),
            Self::Eq => komponist_eq::dsp(),
            Self::GeneralSampler => komponist_general_sampler::dsp(),
            Self::Hall => komponist_reverb::dsp(),
            Self::Limiter => komponist_limiter::dsp(),
            Self::Presse => komponist_presse::dsp(),
            Self::Sampler => komponist_sampler::dsp(),
            Self::Volt => komponist_volt::dsp(),
            Self::Surf => komponist_surf::dsp(),
        };
        InternalProcessor::from_dsp(self.spec(), values, dsp)
    }

    pub(crate) fn processor_from_state(
        self,
        state: Option<&[u8]>,
    ) -> (InternalProcessor, Option<SamplerHandle>) {
        match self {
            Self::GeneralSampler => {
                let (processor, sampler) = komponist_general_sampler::processor_from_state(state);
                (processor, Some(sampler))
            }
            Self::Sampler => {
                let values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
                let (dsp, sampler) = komponist_sampler::dsp_with_handle();
                let processor = InternalProcessor::from_dsp(self.spec(), &values, dsp);
                (processor, Some(sampler))
            }
            _ => {
                let values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
                (self.processor(&values), None)
            }
        }
    }
}

#[cfg(test)]
mod fx_tests;
#[cfg(test)]
mod midi_tests;
#[cfg(test)]
mod preset_names;
#[cfg(test)]
mod presse_tests;
#[cfg(test)]
mod tests;
