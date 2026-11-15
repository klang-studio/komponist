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
//! Adding a device: a module with `spec()`, one variant of [`Internal`] and its arms in
//! [`Internal::spec`] — nothing else in the workspace names the device.
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
//! **Instruments (WP-87, WP-95):** the sampler and [`knurr`] are [`Internal`]s whose `Dsp` is an
//! instrument ([`Dsp::is_instrument`]): the processor hands them the note events at their frame
//! and, from `EventKind::Midi`, pitch bend, the mod wheel (CC 1) and channel pressure
//! ([`Dsp::pitch_bend`], [`Dsp::mod_wheel`], [`Dsp::pressure`]).
//!
//! **Sample tap (WP-92):** a [`Dsp`] can offer a [`komponist_engine::SampleTap`] ([`Dsp::tap`]); the
//! processor puts it into its [`DeviceMeter`] and the UI reads the latest samples from there (the EQ's
//! live spectrum). The DSP copies only while the UI looks (`SampleTap::begin`).

use std::sync::{Arc, OnceLock};

use komponist_engine::{
    DeviceMeter, Event, EventKind, EventProcessor, ProcessContext, ProcessStatus, Processor,
    ProcessorError, SampleTap, StereoIo,
};
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, Page, WidgetKind};
use komponist_model::params::{ParamId, ParamInfo};
use komponist_model::{PluginFormat, PluginRef};

use crate::SAMPLE_RATES;

pub mod arpeggiator;
pub mod delay;
pub mod dsp;
pub mod eq;
pub mod hall;
pub mod knurr;
pub mod limiter;
pub mod midi;
pub mod presse;
pub mod tonleiter;

/// The DSP of an internal device. Created and activated on the UI thread, then used by one thread
/// at a time on the audio side.
pub trait Dsp: Send {
    /// Non-RT, may allocate (delay lines). Prepares for `sample_rate`; keeps the parameters.
    fn activate(&mut self, sample_rate: f32);

    /// RT. Parameter `index` (the wire id) now has plain value `value`, already snapped.
    fn set_param(&mut self, index: usize, value: f64);

    /// RT. Processes `input` into `output` (same length, never aliasing).
    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]);

    /// RT. Like [`Dsp::process`] with an external sidechain signal (WP-104), aligned with `input`
    /// and of the same length. Only called on devices whose [`Spec::sidechain`] is set; the default
    /// ignores the sidechain.
    fn process_sidechain(
        &mut self,
        input: [&[f32]; 2],
        sidechain: [&[f32]; 2],
        output: [&mut [f32]; 2],
    ) {
        let _ = sidechain;
        self.process(input, output);
    }

    /// RT. Clears tails and filter state without allocating.
    fn reset(&mut self);

    /// RT-safe, cached. Latency in frames.
    fn latency(&self) -> u32 {
        0
    }

    /// Non-RT. The reduction in dB at which the device's gain-reduction bar is full.
    fn gr_range_db(&self) -> f32 {
        DeviceMeter::DEFAULT_GR_RANGE_DB
    }

    /// RT. The tempo of the transport in quarter notes per minute, delivered at the start of every
    /// call (constant within one). Tempo-synced effects follow it; the default ignores it.
    fn set_tempo(&mut self, _bpm: f64) {}

    /// Non-RT. The tap of the device's output samples for a live display (WP-92), if it has one.
    /// The DSP copies into it only while the UI looks ([`komponist_engine::SampleTap::begin`]).
    fn tap(&self) -> Option<Arc<SampleTap>> {
        None
    }

    /// RT. The largest gain reduction in dB (≥ 0) since the last call, then resets it. Only
    /// dynamics devices (the limiter) report one; the meter feed ([`InternalProcessor::meter`])
    /// calls it once per rendered piece.
    fn take_reduction_db(&mut self) -> f32 {
        0.0
    }

    /// Whether the device is an instrument: it ignores its input, overwrites the output and gets
    /// the note events ([`Dsp::note_on`] …) at their frame (the sampler, WP-87).
    fn is_instrument(&self) -> bool {
        false
    }

    /// RT, instruments only. A note starts: `key` 0 ..= 127, `velocity` 1 ..= 127.
    fn note_on(&mut self, key: u8, velocity: u8) {
        let _ = (key, velocity);
    }

    /// RT, instruments only. A note ends.
    fn note_off(&mut self, key: u8) {
        let _ = key;
    }

    /// RT, instruments only. Every sounding note ends now.
    fn all_notes_off(&mut self) {}

    /// RT, instruments only. The pitch wheel moved: −1 (all the way down) … +1 (all the way up),
    /// 0 at rest. The processor turns the 14-bit MIDI value into this (WP-95).
    fn pitch_bend(&mut self, value: f32) {
        let _ = value;
    }

    /// RT, instruments only. The mod wheel (CC 1) moved: 0 … 1 (WP-95).
    fn mod_wheel(&mut self, value: f32) {
        let _ = value;
    }

    /// RT, instruments only. Channel pressure (aftertouch): 0 … 1 (WP-95).
    fn pressure(&mut self, value: f32) {
        let _ = value;
    }

    /// The sampler's UI side (loading samples, what the pads show); `None` for every other
    /// device. Not RT.
    fn sampler(&self) -> Option<crate::sampler::SamplerHandle> {
        None
    }
}

/// What defines an internal device (see the module docs).
pub struct Spec {
    /// Catalog id, saved in projects (never changes).
    pub id: &'static str,
    /// Name in the browser and on the card.
    pub name: &'static str,
    /// CLAP-style features for the browser's category and search.
    pub features: &'static [&'static str],
    /// The parameters; the index is the wire id.
    pub params: fn() -> Vec<ParamInfo>,
    /// The body of the device frame.
    pub layout: fn() -> DeviceLayout,
    /// The DSP with its parameters at their defaults.
    pub dsp: fn() -> Box<dyn Dsp>,
    /// The factory presets in menu order (WP-96); none for [`no_presets`].
    pub presets: fn() -> &'static [Preset],
    /// Whether the device has a sidechain input (WP-104): the UI offers the "SC" chip and the DSP
    /// gets the signal through [`Dsp::process_sidechain`].
    pub sidechain: bool,
}

/// A factory preset of an internal device (WP-96): the parameters that differ from the defaults,
/// by model id, so a preset keeps working when a parameter is added later (it gets its default).
#[derive(Clone, Copy, Debug)]
pub struct Preset {
    /// Name in the menu; also what a device state stores as its loaded preset, so it stays unique
    /// in its device and never changes.
    pub name: &'static str,
    /// Folder in the menu, `None` for a flat list.
    pub category: Option<&'static str>,
    /// The parameters that differ from the defaults: model id, plain value.
    pub changes: &'static [(&'static str, f64)],
}

impl Preset {
    /// A preset without a category.
    pub const fn new(name: &'static str, changes: &'static [(&'static str, f64)]) -> Preset {
        Preset {
            name,
            category: None,
            changes,
        }
    }
}

/// `Spec::presets` of a device without presets.
pub fn no_presets() -> &'static [Preset] {
    &[]
}

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
    Knurr,
}

impl Internal {
    /// All devices, in browser order.
    pub const ALL: [Internal; 9] = [
        Internal::Sampler,
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
        Internal::Presse,
        Internal::Arpeggiator,
        Internal::Tonleiter,
        Internal::Knurr,
    ];

    fn spec(self) -> &'static Spec {
        match self {
            Internal::Sampler => &crate::sampler::SPEC,
            Internal::Eq => &eq::SPEC,
            Internal::Delay => &delay::SPEC,
            Internal::Hall => &hall::SPEC,
            Internal::Limiter => &limiter::SPEC,
            Internal::Presse => &presse::SPEC,
            Internal::Arpeggiator => &arpeggiator::SPEC,
            Internal::Tonleiter => &tonleiter::SPEC,
            Internal::Knurr => &knurr::SPEC,
        }
    }

    /// The event stage of a MIDI effect, `None` for the audio effects. Allocates; not RT-safe.
    fn stage(self) -> Option<Box<dyn EventProcessor + Send>> {
        match self {
            Internal::Arpeggiator => Some(arpeggiator::stage()),
            Internal::Tonleiter => Some(tonleiter::stage()),
            _ => None,
        }
    }

    /// Whether this is a MIDI effect: it rewrites note events in front of the instrument
    /// ([`DeviceRole::MidiEffect`](komponist_model::DeviceRole::MidiEffect)) instead of
    /// processing audio.
    pub fn is_midi_effect(self) -> bool {
        matches!(self, Internal::Arpeggiator | Internal::Tonleiter)
    }

    /// The role of the device in a chain.
    pub fn role(self) -> komponist_model::DeviceRole {
        if self.is_midi_effect() {
            komponist_model::DeviceRole::MidiEffect
        } else if self.is_instrument() {
            komponist_model::DeviceRole::Instrument
        } else {
            komponist_model::DeviceRole::Effect
        }
    }

    /// Whether this is an instrument (the sampler, knurr): notes in, audio out.
    pub fn is_instrument(self) -> bool {
        matches!(self, Internal::Sampler | Internal::Knurr)
    }
    fn index(self) -> usize {
        Internal::ALL
            .iter()
            .position(|k| *k == self)
            .unwrap_or_default()
    }

    /// Whether the device has a sidechain input (the "SC" chip in its head, WP-104).
    pub fn has_sidechain(self) -> bool {
        self.spec().sidechain
    }

    /// The catalog id.
    pub fn id(self) -> &'static str {
        self.spec().id
    }

    /// The name shown in the browser and on the card.
    pub fn name(self) -> &'static str {
        self.spec().name
    }

    /// The browser features (CLAP style: `equalizer`, `delay`, `reverb`, `limiter`).
    pub fn features(self) -> &'static [&'static str] {
        self.spec().features
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
        PluginRef {
            format: PluginFormat::Builtin,
            id: self.id().into(),
            name: self.name().into(),
            vendor: "komponist".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            module: Box::default(),
        }
    }

    /// The parameters; the index is the wire id.
    pub fn params(self) -> &'static [ParamInfo] {
        static CELLS: [OnceLock<Vec<ParamInfo>>; Internal::ALL.len()] =
            [const { OnceLock::new() }; Internal::ALL.len()];
        CELLS[self.index()].get_or_init(self.spec().params)
    }

    /// The layout of the device frame's body.
    pub fn layout(self) -> &'static DeviceLayout {
        static CELLS: [OnceLock<DeviceLayout>; Internal::ALL.len()] =
            [const { OnceLock::new() }; Internal::ALL.len()];
        CELLS[self.index()].get_or_init(self.spec().layout)
    }

    /// The factory presets in menu order (empty: the device has none).
    pub fn presets(self) -> &'static [Preset] {
        (self.spec().presets)()
    }

    /// All plain values (by wire id) of preset `index`: the defaults with the preset's changes
    /// (snapped). `None` if there is no such preset.
    pub fn preset_values(self, index: usize) -> Option<Vec<f64>> {
        let preset = self.presets().get(index)?;
        let mut values = self.defaults();
        for (key, value) in preset.changes {
            let wire = self.wire(&ParamId::new(*key))? as usize;
            values[wire] = self.params()[wire].snap(*value);
        }
        Some(values)
    }

    /// The device state after loading preset `index` into a device with state `old`: the preset's
    /// values plus its name under `"preset"`, and the other keys of `old` kept (the sampler's
    /// links). `None` if there is no such preset.
    pub fn preset_state(self, old: Option<&[u8]>, index: usize) -> Option<Vec<u8>> {
        let values = self.preset_values(index)?;
        let name = self.presets()[index].name;
        let mut state: serde_json::Value =
            serde_json::from_slice(&self.encode_state(&values)).expect("an encoded state is JSON");
        state["preset"] = name.into();
        let encoded = serde_json::to_vec(&state).expect("a state encodes as JSON");
        Some(self.keep_extras(old, encoded))
    }

    /// The preset a device state says it loaded (`"preset"`) and whether the values differ from it
    /// now („ *“). `None` for a state without a (known) preset, as every project saved before
    /// WP-96.
    pub fn loaded_preset(self, state: Option<&[u8]>) -> Option<(usize, bool)> {
        let value: serde_json::Value = serde_json::from_slice(state?).ok()?;
        let name = value.get("preset")?.as_str()?;
        let index = self.presets().iter().position(|p| p.name == name)?;
        let now = self.decode_state(state).ok()?;
        Some((index, Some(&now) != self.preset_values(index).as_ref()))
    }

    /// The wire id of parameter `id`.
    pub fn wire(self, id: &ParamId) -> Option<u32> {
        self.params()
            .iter()
            .position(|p| p.id == *id)
            .map(|i| i as u32)
    }

    /// The default plain values, by wire id.
    pub fn defaults(self) -> Vec<f64> {
        self.params().iter().map(|p| p.default).collect()
    }

    /// Decodes a device state into plain values by wire id; `None` (a device without state) gives
    /// the defaults. See the module docs for the format.
    pub fn decode_state(self, state: Option<&[u8]>) -> Result<Vec<f64>, StateError> {
        let Some(bytes) = state else {
            return Ok(self.defaults());
        };
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(StateError::Json)?;
        let params = value
            .get("params")
            .and_then(serde_json::Value::as_object)
            .ok_or(StateError::NoParams)?;
        Ok(self
            .params()
            .iter()
            .map(|p| {
                params
                    .get(p.id.as_str())
                    .and_then(serde_json::Value::as_f64)
                    .map_or(p.default, |v| p.snap(v))
            })
            .collect())
    }

    /// Encodes plain values (by wire id; missing ones are the default) as device state.
    pub fn encode_state(self, values: &[f64]) -> Vec<u8> {
        let params: serde_json::Map<String, serde_json::Value> = self
            .params()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let v = p.snap(values.get(i).copied().unwrap_or(p.default));
                (p.id.as_str().to_owned(), serde_json::json!(v))
            })
            .collect();
        // Numbers and strings only: encoding into memory cannot fail.
        serde_json::to_vec(&serde_json::json!({ "params": params }))
            .expect("a parameter map encodes as JSON")
    }

    /// `state` with parameter `wire` set to `value` (a broken or missing state counts as the
    /// defaults). `None` for an unknown wire id.
    pub fn with_value(self, state: Option<&[u8]>, wire: u32, value: f64) -> Option<Vec<u8>> {
        let info = self.params().get(wire as usize)?;
        let mut values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
        values[wire as usize] = info.snap(value);
        Some(self.keep_extras(state, self.encode_state(&values)))
    }

    /// `encoded` (a state of just parameters) plus every other top-level key of `old` (the
    /// sampler's sample links), so a knob turn does not lose them.
    pub fn keep_extras(self, old: Option<&[u8]>, encoded: Vec<u8>) -> Vec<u8> {
        let Some(serde_json::Value::Object(extras)) =
            old.and_then(|s| serde_json::from_slice(s).ok())
        else {
            return encoded;
        };
        let Ok(serde_json::Value::Object(mut new)) = serde_json::from_slice(&encoded) else {
            return encoded;
        };
        for (key, value) in extras {
            new.entry(key).or_insert(value);
        }
        serde_json::to_vec(&new).unwrap_or(encoded)
    }

    /// A processor with the plain `values` (by wire id). Allocates; not RT-safe. Call
    /// [`Processor::activate`] before sending it to the engine.
    pub fn processor(self, values: &[f64]) -> InternalProcessor {
        let params = self.params();
        let mut dsp = (self.spec().dsp)();
        for (i, p) in params.iter().enumerate() {
            dsp.set_param(i, p.snap(values.get(i).copied().unwrap_or(p.default)));
        }
        let mut stage = self.stage().map(|s| midi::Stage::new(params, s));
        if let Some(stage) = &mut stage {
            for (i, p) in params.iter().enumerate() {
                stage.set_param(i as u32, values.get(i).copied().unwrap_or(p.default));
            }
        }
        let meter = Arc::new(DeviceMeter::new());
        if let Some(tap) = dsp.tap() {
            meter.set_tap(tap);
        }
        InternalProcessor {
            params,
            dsp,
            stage,
            sample_rate: INITIAL_RATE,
            meter,
        }
    }

    /// A processor for a device state ([`Internal::decode_state`]); a state that does not load
    /// plays with the defaults, the state stays in the document untouched.
    pub fn processor_from_state(self, state: Option<&[u8]>) -> InternalProcessor {
        let values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
        self.processor(&values)
    }
}

/// Hands a channel message to an instrument: pitch bend (14 bit, −1 … +1), the mod wheel (CC 1),
/// channel pressure and "all sound/notes off" (CC 120, 123). Other controllers and polyphonic
/// aftertouch are ignored; the channel does not matter (WP-95).
fn midi_to_dsp(dsp: &mut dyn Dsp, [status, d1, d2]: [u8; 3]) {
    match status & 0xF0 {
        0xE0 => {
            let value = (i32::from(d2 & 0x7F) << 7 | i32::from(d1 & 0x7F)) - 8192;
            dsp.pitch_bend((value as f32 / 8192.0).clamp(-1.0, 1.0));
        }
        0xB0 => match d1 {
            1 => dsp.mod_wheel(f32::from(d2 & 0x7F) / 127.0),
            120 | 123 => dsp.all_notes_off(),
            _ => {}
        },
        0xD0 => dsp.pressure(f32::from(d1 & 0x7F) / 127.0),
        _ => {}
    }
}

/// Sample rate a processor is built for before its first [`Processor::activate`].
const INITIAL_RATE: f32 = 48_000.0;

/// A device state that is not an internal device's.
#[derive(Debug)]
pub enum StateError {
    /// Not JSON.
    Json(serde_json::Error),
    /// JSON without a `params` object.
    NoParams,
}

impl core::fmt::Display for StateError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StateError::Json(e) => write!(f, "invalid device state: {e}"),
            StateError::NoParams => f.write_str("invalid device state: no params"),
        }
    }
}

impl std::error::Error for StateError {}

/// An internal device as an engine [`Processor`]: an audio effect, or a MIDI effect whose event
/// stage the engine runs in front of the instrument ([`Processor::event_processor`]).
///
/// `EventKind::Param { id, value }` sets the parameter with wire id `id` (unknown ids are ignored)
/// at its frame: the block is processed in pieces between the events. Note and MIDI events are
/// ignored, except for an instrument: it gets them, plus pitch bend, mod wheel and channel pressure
/// (`EventKind::Midi`, WP-95). [`Processor::latency`] is the DSP's.
pub struct InternalProcessor {
    params: &'static [ParamInfo],
    dsp: Box<dyn Dsp>,
    /// The event stage of a MIDI effect ([`Processor::event_processor`]).
    stage: Option<midi::Stage>,
    sample_rate: f32,
    /// Input peak, gain reduction and output peak for the UI (WP-89).
    meter: Arc<DeviceMeter>,
}

impl InternalProcessor {
    /// The level meter the processor feeds every block; the UI takes the values every frame
    /// ([`DeviceMeter::take`]).
    pub fn meter(&self) -> Arc<DeviceMeter> {
        Arc::clone(&self.meter)
    }

    /// The sample rate the processor was last activated at (48 kHz before that).
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// The sampler's UI side: load samples into pads, read what they show; `None` for any other
    /// device.
    pub fn sampler(&self) -> Option<crate::sampler::SamplerHandle> {
        self.dsp.sampler()
    }
}

impl Processor for InternalProcessor {
    fn activate(&mut self, sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        if !SAMPLE_RATES.contains(&sample_rate) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        self.sample_rate = sample_rate;
        self.dsp.activate(sample_rate);
        self.meter.set_latency(self.dsp.latency());
        self.meter.set_gr_range_db(self.dsp.gr_range_db());
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        self.run(ctx, events, io, None)
    }

    fn process_sidechain(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sidechain: [&[f32]; 2],
    ) -> ProcessStatus {
        self.run(ctx, events, io, Some(sidechain))
    }

    fn latency(&self) -> u32 {
        self.dsp.latency()
    }

    fn reset(&mut self) {
        self.dsp.reset();
        if let Some(stage) = &mut self.stage {
            stage.reset();
        }
    }

    fn event_processor(&mut self) -> Option<&mut dyn EventProcessor> {
        self.stage.as_mut().map(|s| s as &mut dyn EventProcessor)
    }
}

impl InternalProcessor {
    /// `process` and `process_sidechain`: the block in pieces between the parameter events.
    fn run(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
        sc: Option<[&[f32]; 2]>,
    ) -> ProcessStatus {
        self.dsp.set_tempo(ctx.transport.tempo_bpm);
        let frames = io.frames();
        let mut done = 0;
        let instrument = self.dsp.is_instrument();
        for event in events {
            // Parameters for every device; notes only reach an instrument.
            if !matches!(event.kind, EventKind::Param { .. })
                && !(instrument
                    && matches!(
                        event.kind,
                        EventKind::NoteOn { .. }
                            | EventKind::NoteOff { .. }
                            | EventKind::AllNotesOff
                            | EventKind::Midi(_)
                    ))
            {
                continue;
            }
            let at = (event.offset as usize).clamp(done, frames);
            if at > done {
                self.render(io, sc, done, at);
                done = at;
            }
            match event.kind {
                EventKind::Param { id, value } => {
                    if let Some(info) = self.params.get(id as usize) {
                        self.dsp.set_param(id as usize, info.snap(value));
                        // A parameter can change the latency (limiter lookahead); the UI thread
                        // reads it from the meter and recompiles the delay compensation.
                        self.meter.set_latency(self.dsp.latency());
                    }
                }
                EventKind::NoteOn { key, velocity, .. } => self.dsp.note_on(key, velocity),
                EventKind::NoteOff { key, .. } => self.dsp.note_off(key),
                EventKind::AllNotesOff => self.dsp.all_notes_off(),
                EventKind::Midi(bytes) => midi_to_dsp(self.dsp.as_mut(), bytes),
            }
        }
        if done < frames {
            self.render(io, sc, done, frames);
        }
        ProcessStatus::Continue
    }

    fn render(&mut self, io: &mut StereoIo<'_>, sc: Option<[&[f32]; 2]>, from: usize, to: usize) {
        let [il, ir] = io.input;
        let [ol, or] = &mut io.output;
        let input = [&il[from..to], &ir[from..to]];
        let output = [&mut ol[from..to], &mut or[from..to]];
        match sc {
            Some([sl, sr]) if sl.len() >= to && sr.len() >= to => {
                self.dsp
                    .process_sidechain(input, [&sl[from..to], &sr[from..to]], output);
            }
            _ => self.dsp.process(input, output),
        }
        let peak = |a: &[f32], b: &[f32]| a.iter().chain(b).fold(0.0_f32, |m, s| m.max(s.abs()));
        self.meter.record(
            peak(&il[from..to], &ir[from..to]),
            self.dsp.take_reduction_db(),
            peak(&ol[from..to], &or[from..to]),
        );
    }
}

// ------------------------------------------------------------------------------------------------
// Helpers for the layouts of the devices.

/// `CellParam::Id` of `key`.
fn param(key: &str) -> CellParam {
    CellParam::Id(ParamId::new(key))
}

/// `CellParam::Template` of `template` (`b{band}_freq`).
fn template(template: &str) -> CellParam {
    CellParam::Template(template.into())
}

/// A knob in one cell.
fn knob(col: u8, row: u8, key: &str) -> Cell {
    Cell::knob(col, row, ParamId::new(key))
}

/// A large knob over two cells.
fn knob_large(col: u8, row: u8, key: &str) -> Cell {
    Cell::span(col, row, 2, 1, WidgetKind::KnobLarge, param(key))
}

/// A toggle button in a half cell.
fn toggle(col: u8, row: u8, cols: u8, half: Half, p: CellParam) -> Cell {
    Cell::half(col, row, cols, half, WidgetKind::ToggleButton, p)
}

/// A one-page layout.
fn single_page(cols: u8, cells: Vec<Cell>) -> DeviceLayout {
    DeviceLayout::single(Page {
        name: String::new(),
        cols,
        cells,
        dividers: Vec::new(),
    })
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
