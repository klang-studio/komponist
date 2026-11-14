//! Internal devices: effects that run in process as an engine [`Processor`] (WP-85).
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
//!   a state (the app stores a knob turn this way, as for pisynth's macros).
//! - **Processor:** [`Internal::processor`] (from decoded values) gives an [`InternalProcessor`];
//!   `activate` it and send it with `Command::InsertProcessor`. Knob turns reach it as
//!   `Command::SetParam { id: wire, value }`; the engine delivers them as `EventKind::Param` at
//!   their frame. Creating a processor allocates (UI thread); `process` and `reset` never do.
//!
//! Adding a device: a module with `spec()`, one variant of [`Internal`] and its arms in
//! [`Internal::spec`] — nothing else in the workspace names the device.
//!
//! The DSP of the four effects is a placeholder (EQ, Hall: pass-through, EQ with its output gain;
//! Delay: a plain stereo delay; Limiter: a hard clip at the ceiling). The real algorithms are
//! WP-90 (`klang-dsp`).

use std::sync::OnceLock;

use komponist_engine::{
    Event, EventKind, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_model::params::layout::{Cell, CellParam, DeviceLayout, Half, Page, WidgetKind};
use komponist_model::params::{ParamId, ParamInfo};
use komponist_model::{PluginFormat, PluginRef};

use crate::SAMPLE_RATES;

pub mod delay;
pub mod eq;
pub mod hall;
pub mod limiter;

/// The DSP of an internal device. Created and activated on the UI thread, then used by one thread
/// at a time on the audio side.
pub trait Dsp: Send {
    /// Non-RT, may allocate (delay lines). Prepares for `sample_rate`; keeps the parameters.
    fn activate(&mut self, sample_rate: f32);

    /// RT. Parameter `index` (the wire id) now has plain value `value`, already snapped.
    fn set_param(&mut self, index: usize, value: f64);

    /// RT. Processes `input` into `output` (same length, never aliasing).
    fn process(&mut self, input: [&[f32]; 2], output: [&mut [f32]; 2]);

    /// RT. Clears tails and filter state without allocating.
    fn reset(&mut self);

    /// RT-safe, cached. Latency in frames.
    fn latency(&self) -> u32 {
        0
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
}

/// The internal devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Internal {
    /// Parametric EQ.
    Eq,
    /// Stereo delay.
    Delay,
    /// Reverb.
    Hall,
    /// Peak limiter; an ordinary device, also for the master chain.
    Limiter,
}

impl Internal {
    /// All devices, in browser order.
    pub const ALL: [Internal; 4] = [
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
    ];

    fn spec(self) -> &'static Spec {
        match self {
            Internal::Eq => &eq::SPEC,
            Internal::Delay => &delay::SPEC,
            Internal::Hall => &hall::SPEC,
            Internal::Limiter => &limiter::SPEC,
        }
    }

    fn index(self) -> usize {
        Internal::ALL
            .iter()
            .position(|k| *k == self)
            .unwrap_or_default()
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
        Some(self.encode_state(&values))
    }

    /// A processor with the plain `values` (by wire id). Allocates; not RT-safe. Call
    /// [`Processor::activate`] before sending it to the engine.
    pub fn processor(self, values: &[f64]) -> InternalProcessor {
        let params = self.params();
        let mut dsp = (self.spec().dsp)();
        for (i, p) in params.iter().enumerate() {
            dsp.set_param(i, p.snap(values.get(i).copied().unwrap_or(p.default)));
        }
        InternalProcessor {
            params,
            dsp,
            sample_rate: INITIAL_RATE,
        }
    }

    /// A processor for a device state ([`Internal::decode_state`]); a state that does not load
    /// plays with the defaults, the state stays in the document untouched.
    pub fn processor_from_state(self, state: Option<&[u8]>) -> InternalProcessor {
        let values = self.decode_state(state).unwrap_or_else(|_| self.defaults());
        self.processor(&values)
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

/// An internal effect as an engine [`Processor`].
///
/// `EventKind::Param { id, value }` sets the parameter with wire id `id` (unknown ids are ignored)
/// at its frame: the block is processed in pieces between the events. Note and MIDI events are
/// ignored. [`Processor::latency`] is the DSP's.
pub struct InternalProcessor {
    params: &'static [ParamInfo],
    dsp: Box<dyn Dsp>,
    sample_rate: f32,
}

impl InternalProcessor {
    /// The sample rate the processor was last activated at (48 kHz before that).
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }
}

impl Processor for InternalProcessor {
    fn activate(&mut self, sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        if !SAMPLE_RATES.contains(&sample_rate) {
            return Err(ProcessorError::UnsupportedSampleRate(sample_rate));
        }
        self.sample_rate = sample_rate;
        self.dsp.activate(sample_rate);
        Ok(())
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let frames = io.frames();
        let mut done = 0;
        for event in events {
            let EventKind::Param { id, value } = event.kind else {
                continue;
            };
            let Some(info) = self.params.get(id as usize) else {
                continue;
            };
            let at = (event.offset as usize).clamp(done, frames);
            if at > done {
                self.render(io, done, at);
                done = at;
            }
            self.dsp.set_param(id as usize, info.snap(value));
        }
        if done < frames {
            self.render(io, done, frames);
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        self.dsp.latency()
    }

    fn reset(&mut self) {
        self.dsp.reset();
    }
}

impl InternalProcessor {
    fn render(&mut self, io: &mut StereoIo<'_>, from: usize, to: usize) {
        let [il, ir] = io.input;
        let [ol, or] = &mut io.output;
        self.dsp.process(
            [&il[from..to], &ir[from..to]],
            [&mut ol[from..to], &mut or[from..to]],
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

/// `dB` as a linear gain.
fn db_to_gain(db: f64) -> f32 {
    10f64.powf(db / 20.0) as f32
}

#[cfg(test)]
mod tests;
