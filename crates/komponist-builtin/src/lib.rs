//! Built-in instruments of komponist: pisynth in process (ARCHITECTURE §8.5, D6).
//!
//! - [`Macro`]: the 8 macros of the device card with their stable parameter ids 0–7, ranges,
//!   [`Macro::get`] / [`Macro::set`] on a patch and [`Macro::text`] for the value display. The
//!   mapping to patch fields is documented in [`macros`].
//! - State: [`encode_state`] / [`decode_state`] (the patch as JSON, see [`state`]).
//! - Factory presets: [`presets`] (index, name, category; "Presets · pisynth" in the browser),
//!   [`categories`], [`preset_patch`], [`PRESET_COUNT`].
//! - [`algorithms`]: the 32 fixed algorithms the Algorithmus macro writes.

pub mod algorithms;
pub mod macros;
pub mod state;

pub use macros::Macro;
pub use state::{
    PRESET_COUNT, PresetInfo, StateError, categories, decode_state, default_patch, encode_state,
    preset_patch, presets,
};
pub use synth_core::Patch;
