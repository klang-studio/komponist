//! Realtime sampler engine of komponist: voices, pads, envelopes, interpolation.
//!
//! Copied from the `dsp` crate of klopfer (commit `190581c`, 2026-11-15) and developed independently since.
//!
//! Nothing in here depends on the plugin framework, so everything can be unit tested in isolation.
//! None of the `process`/`render` style functions allocate.

pub mod biquad;
pub mod engine;
pub mod envelope;
pub mod interp;
pub mod layers;
pub mod lofi;
pub mod modulation;
pub mod pad;
mod pitch;
mod playback;
pub mod sample;
pub mod slicing;
pub mod stretch;
pub mod svf;
mod voice;

pub use engine::{
    Engine, MAX_VOICES, NUM_OUTPUTS, NUM_PADS, NUM_SAMPLE_SLOTS, PADS_PER_BANK, SampleSource,
    slot_index,
};
pub use layers::{LayerInfo, LayerMode, NUM_LAYERS, PadLayers};
pub use modulation::{LfoShape, ModSlot, ModSource, ModTarget};
pub use pad::{EnvParams, FilterMode, PadParams, PadSettings};
pub use playback::{PlaybackDirection, PlaybackLoop, PlaybackRegion, SampleTrigger};
pub use sample::Sample;
