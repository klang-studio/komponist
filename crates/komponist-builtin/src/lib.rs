//! Built-in instruments of komponist: pisynth in process (ARCHITECTURE §8.5, D6).
//!
//! - [`Macro`]: the 8 macros of the device card with their stable parameter ids 0–7, ranges,
//!   [`Macro::get`] / [`Macro::set`] on a patch and [`Macro::text`] for the value display. The
//!   mapping to patch fields is documented in [`macros`].
//! - [`algorithms`]: the 32 fixed algorithms the Algorithmus macro writes.

pub mod algorithms;
pub mod macros;

pub use macros::Macro;
pub use synth_core::Patch;
