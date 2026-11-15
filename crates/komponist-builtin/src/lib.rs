//! Built-in instruments of komponist: hexa in process (ARCHITECTURE §8.5, D6).
//!
//! hexa is komponist's 6-operator FM synthesizer (`crates/hexa-core`, derived from the pisynth hardware
//! synth's engine). [`HexaProcessor`] runs its `Synth<1>` as an engine `Processor`. The metronome voice
//! of the architecture sketch lives in the engine instead (`komponist_engine`'s metronome).
//!
//! # API
//!
//! - [`HexaProcessor`]: the processor. [`HexaProcessor::new`] (a [`Patch`]) or
//!   [`HexaProcessor::from_state`] (device state, `None` = "Init"), then `activate` and
//!   `Command::InsertProcessor`.
//! - [`Macro`]: the 8 macros of the device card with their stable parameter ids 0–7, ranges,
//!   [`Macro::get`] / [`Macro::set`] on a patch and [`Macro::text`] for the value display. The
//!   mapping to patch fields is documented in [`macros`].
//! - State: [`encode_state`] / [`decode_state`] (the patch as JSON, see [`state`]).
//! - Factory presets: [`presets`] (index, name, category; "Presets · hexa" in the browser),
//!   [`categories`], [`preset_patch`], [`PRESET_COUNT`].
//! - [`device`]: the device card (WP-86): the parameters of hexa's layout ([`device::params`],
//!   each a view onto a [`Patch`] field, [`device::Field::get`] / [`device::Field::set`]) and the
//!   four-page layout ([`device::layout`]). The macros keep their wire ids 0–7; the card's own
//!   start at 8.
//! - [`algorithms`]: the 32 fixed algorithms the Algorithmus macro writes.
//! - [`internal::knurr`]: the virtual-analog synthesizer „knurr“ (WP-95), an internal instrument
//!   ([`Internal::Knurr`]) written for komponist on `klang-dsp`'s primitives.
//! - [`sampler`]: the drum sampler (WP-87), an internal instrument ([`Internal::Sampler`]) on
//!   `sampler_core`'s engine, with sample links in its state, a loader thread and a test kit.
//!
//! # How the app drives it
//!
//! - **Create:** `HexaProcessor::from_state(device.state.as_deref())`, activate at the engine
//!   rate, insert into the device's slot.
//! - **Card knob (or macro):** `Command::SetParam { slot, id: field.wire(), value }` to the engine,
//!   and `field.set(&mut patch, value)` on the document's copy (state = `encode_state(&patch)`),
//!   with `field = Field::from_wire(id)`. Both run the same function, so the running patch and the
//!   document stay equal.
//! - **Preset or any other state change (also undo/redo):** build a new processor from the new
//!   state and insert it into the same slot; the engine retires the old one. Sounding notes and
//!   effect tails of the old processor end there. A preset is the state
//!   `encode_state(&preset_patch(index)?)`.
//!
//! # Realtime
//!
//! `process` and `reset` never allocate, lock or block (tested inside `assert_no_alloc`). Costs
//! measured on the development machine (12 cores, release build, 48 kHz, 2× oversampling, under
//! heavy parallel load; `tests/hexa/bench.rs`), per call of 256 frames (5.33 ms):
//!
//! | Load | Time | Share |
//! |---|---:|---:|
//! | 8-key chord, "Init" (2 operators) | 52 µs | 1.0 % |
//! | 8-key chord, "E-Piano" (6 operators, stereo) | 116 µs | 2.2 % |
//! | 8-key chord, "Supersaw" / "Strings" (unison, all 16 voices) | 300–330 µs | 5.7–6.1 % |
//! | one macro change (patch copy, `Macro::set`, `Synth::set_patch`) | 1.1 µs | |
//! | `reset` (rewrites the 2 MB synth in place) | 27 µs | |
//!
//! The cost scales linearly with the block length. Every event offset splits the render, and a
//! split costs one extra control-rate step, so many events at distinct offsets in one call cost
//! more than the same events at one offset. Creating a processor (UI thread) takes ~80 µs.

pub mod algorithms;
pub mod device;
pub mod internal;
pub mod macros;
mod processor;
pub mod sampler;
pub mod state;

pub use hexa_core::Patch;
pub use internal::{Internal, InternalProcessor, Preset};
pub use macros::Macro;
pub use processor::{HexaProcessor, SAMPLE_RATES};
pub use state::{
    PRESET_COUNT, PresetInfo, StateError, categories, decode_state, default_patch, encode_state,
    preset_patch, presets,
};
