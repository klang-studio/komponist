//! Internal devices: effects and MIDI effects that run in process as an engine [`Processor`] (WP-85,
//! WP-88).
//!
//! An internal device is one [`Internal`] variant plus a [`Spec`]: the catalog name (**the only
//! place** a device's name is written; the names are working names until the owner decides), the
//! parameter list ([`ParamInfo`]), the layout the device frame draws ([`DeviceLayout`]) and the
//! DSP ([`Dsp`]). Everything else is generic:
//!
//! - **Catalog:** [`Internal::plugin_ref`] is the reference a device stores, [`Internal::name`] and
//!   [`Internal::features`] feed the browser (`komponist-plugins`' built-in entries, tag "INTERN").
//! - **Parameters:** [`Internal::params`]; the *wire id* (`Command::SetParam::id`,
//!   `EventKind::Param::id`) is the index in that list, the *model id* ([`ParamId`]) is the stable
//!   name a project saves. Values are plain (Hz, dB, a step index), as everywhere.
//! - **State:** [`Internal::encode_state`] / [`Internal::decode_state`]: JSON
//!   `{"params": {"<id>": value, …}}` keyed by [`ParamId`]. Missing ids get their default, unknown
//!   ids are ignored, values are snapped to their range, so a state written before a parameter
//!   existed (or after one was removed) still loads. [`Internal::with_value`] changes one value of
//!   a state (the app stores a knob turn this way, as for hexa's macros).
//! - **Processor:** [`Internal::processor`] (from decoded values) gives an [`InternalProcessor`];
//!   `activate` it and send it with `Command::InsertProcessor`. Knob turns reach it as
//!   `Command::SetParam { id: wire, value }`; the engine delivers them as `EventKind::Param` at
//!   their frame. Creating a processor allocates (UI thread); `process` and `reset` never do.
//!
//! Devices declare a static [`Spec`]. [`Internal`] adapts the device specs to the catalog and
//! the existing app API during the migration to individual device crates.
//!
//! **MIDI effects (WP-88):** [`arpeggiator`] and [`tonleiter`] are [`Internal`]s whose
//! [`Internal::is_midi_effect`] is true. They have the same spec, parameters, layout and state as
//! the audio effects, but an event stage ([`komponist_engine::EventProcessor`], via
//! [`Processor::event_processor`]) instead of a [`Dsp`]: their `Spec::dsp` is a pass-through the
//! engine never calls. Their role in a chain is [`Internal::role`] (`MidiEffect`); the engine runs
//! them in front of the instrument (ARCHITECTURE §6.4.2) and keeps notes from hanging.
//!
//! **Effect DSP (WP-90):** [`eq`] (six RBJ biquad bands), [`delay`] (tempo-synced stereo delay with
//! filtered feedback), [`hall`] (8-line feedback delay network), [`limiter`] (lookahead peak
//! limiter) and [`presse`] (compressor, WP-93) are written for komponist on top of the small parts in [`dsp`] and `klang-dsp`'s
//! biquad. Parameters are smoothed at a control rate of [`dsp::CONTROL`] samples that counts across
//! calls, so the output does not depend on how the host splits blocks.
//!
//! **Instruments (WP-87, WP-95):** the sampler and [`volt`] are [`Internal`]s whose `Dsp` is an
//! instrument ([`Dsp::is_instrument`]): the processor hands them the note events at their frame
//! and, from `EventKind::Midi`, pitch bend, the mod wheel (CC 1) and channel pressure
//! ([`Dsp::pitch_bend`], [`Dsp::mod_wheel`], [`Dsp::pressure`]).
//!
//! **Sample tap (WP-92):** a [`Dsp`] can offer a [`komponist_engine::SampleTap`] ([`Dsp::tap`]); the
//! processor puts it into its [`DeviceMeter`] and the UI reads the latest samples from there (the EQ's
//! live spectrum). The DSP copies only while the UI looks (`SampleTap::begin`).

use std::sync::Arc;

use komponist_device_core::InternalProcessor as CoreProcessor;
use komponist_engine::{
    DeviceMeter, Event, EventProcessor, ProcessContext, ProcessStatus, Processor, ProcessorError,
    StereoIo,
};
use komponist_i18n::Lang;
use komponist_model::params::layout::DeviceLayout;
use komponist_model::params::{ParamId, ParamInfo};
use komponist_model::{DeviceRole, PluginFormat, PluginRef};

use crate::sampler::SamplerHandle;

use komponist_device_core::INITIAL_RATE;
use komponist_device_core::layout::{knob, knob_large, param, single_page, template, toggle};
pub use komponist_device_core::{Dsp, Preset, Spec, StateError, dsp, midi, no_presets};

pub mod arpeggiator;
pub mod delay;
pub mod eq;
pub mod hall;
pub mod limiter;
pub mod preset_names;
pub mod presse;
pub mod tonleiter;
pub mod volt;

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
    /// Chromatic sampler with waveform editing and slice playback.
    GeneralSampler,
}

impl Internal {
    /// All devices, in browser order.
    pub const ALL: [Internal; 10] = [
        Internal::Sampler,
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
        Internal::Presse,
        Internal::Arpeggiator,
        Internal::Tonleiter,
        Internal::Volt,
        Internal::GeneralSampler,
    ];

    fn spec(self) -> &'static Spec {
        match self {
            Internal::Sampler => &crate::sampler::SPEC,
            Internal::GeneralSampler => &crate::general_sampler::SPEC,
            Internal::Eq => &eq::SPEC,
            Internal::Delay => &delay::SPEC,
            Internal::Hall => &hall::SPEC,
            Internal::Limiter => &limiter::SPEC,
            Internal::Presse => &presse::SPEC,
            Internal::Arpeggiator => &arpeggiator::SPEC,
            Internal::Tonleiter => &tonleiter::SPEC,
            Internal::Volt => &volt::SPEC,
        }
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

    /// The factory presets in menu order (empty: the device has none).
    pub fn presets(self) -> &'static [Preset] {
        self.spec().presets()
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

    /// `state` with parameter `wire` set to `value` (a broken or missing state counts as the
    /// defaults). `None` for an unknown wire id.
    pub fn with_value(self, state: Option<&[u8]>, wire: u32, value: f64) -> Option<Vec<u8>> {
        self.spec().with_value(state, wire, value)
    }

    /// `encoded` (a state of just parameters) plus every other top-level key of `old` (the
    /// sampler's sample links), so a knob turn does not lose them.
    pub fn keep_extras(self, old: Option<&[u8]>, encoded: Vec<u8>) -> Vec<u8> {
        self.spec().keep_extras(old, encoded)
    }

    /// A processor with the plain `values` (by wire id). Allocates; not RT-safe. Call
    /// [`Processor::activate`] before sending it to the engine.
    pub fn processor(self, values: &[f64]) -> InternalProcessor {
        let dsp_with_handle = match self {
            Internal::Sampler => Some(crate::sampler::dsp_with_handle()),
            Internal::GeneralSampler => Some(crate::general_sampler::dsp_with_handle()),
            _ => None,
        };
        if let Some((dsp, sampler)) = dsp_with_handle {
            InternalProcessor {
                processor: CoreProcessor::from_dsp(self.spec(), values, dsp),
                sampler: Some(sampler),
            }
        } else {
            InternalProcessor {
                processor: CoreProcessor::new(self.spec(), values),
                sampler: None,
            }
        }
    }

    /// A processor for a device state ([`Internal::decode_state`]); a state that does not load
    /// plays with the defaults, the state stays in the document untouched.
    pub fn processor_from_state(self, state: Option<&[u8]>) -> InternalProcessor {
        let values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
        let processor = self.processor(&values);
        if self == Internal::GeneralSampler
            && let Some(handle) = processor.sampler()
        {
            handle.set_general_settings(&crate::general_sampler::read_settings(state));
        }
        processor
    }
}

/// The existing engine processor API, including access to sampler UI handles.
pub struct InternalProcessor {
    processor: CoreProcessor,
    sampler: Option<SamplerHandle>,
}

impl InternalProcessor {
    /// The level meter the processor feeds every block.
    pub fn meter(&self) -> Arc<DeviceMeter> {
        self.processor.meter()
    }

    /// The sample rate the processor was last activated at (48 kHz before that).
    pub fn sample_rate(&self) -> f32 {
        self.processor.sample_rate()
    }

    /// The UI handle for a sampler, when present.
    pub fn sampler(&self) -> Option<SamplerHandle> {
        self.sampler.clone()
    }
}

impl Processor for InternalProcessor {
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        self.processor.activate(sample_rate, max_block)
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        self.processor.process(ctx, events, io)
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        self.processor.process_sidechain(ctx, events, io, sidechain)
    }

    fn latency(&self) -> u32 {
        self.processor.latency()
    }

    fn reset(&mut self) {
        self.processor.reset();
    }

    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor> {
        self.processor.event_processor()
    }
}

#[cfg(test)]
mod eq_tap_tests;
#[cfg(test)]
mod fx_tests;
#[cfg(test)]
mod midi_tests;
#[cfg(test)]
mod presse_tests;
#[cfg(test)]
mod tests;
