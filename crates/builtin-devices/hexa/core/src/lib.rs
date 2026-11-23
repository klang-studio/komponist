//! Platform-agnostic 6-operator FM synthesis engine of hexa.
//!
//! Copied from pisynth's `hexa-core` (commit `b621aec`, 2026-11-15) and developed independently since.
//!
//! `no_std` with `alloc`: the master effects' delay lines are allocated at
//! construction, sized from the sample rate; rendering allocates nothing.
//! Must not depend on any HAL or OS facility.
// Missing SAFETY comments: 24 (`simd.rs`). Remove this allow when the crate is clean (#241 follow-up).
#![allow(clippy::undocumented_unsafe_blocks)]
#![cfg_attr(not(test), no_std)]

extern crate alloc;

// Tests run the effects inside `assert_no_alloc`, which only reports with this allocator.
#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod allocator;
pub mod bank;
pub mod chorus;
pub mod controls;
pub mod delay;
mod delay_line;
pub mod distortion;
pub mod effects;
mod engine;
pub mod envelope;
pub mod eq;
pub mod filter;
pub mod lfo;
pub mod midi;
pub mod mod_envelope;
pub mod modulation;
pub mod operator;
pub mod oversampling;
mod patch;
pub mod phase;
pub mod pitch;
pub mod presets;
pub mod reverb;
mod simd;
pub mod sine;
mod smooth;
mod synth;
mod voicing;
pub mod waveform;

pub use chorus::ChorusParams;
pub use delay::DelayParams;
pub use distortion::{BitcrusherParams, DistortionKind, DistortionParams};
pub use effects::{EffectKind, EffectParams};
pub use engine::{ENVELOPE_POSITIONS, Engine, EnvelopePositions, clamp};
pub use envelope::{EnvelopeParams, MAX_TIME};
pub use eq::EqParams;
pub use filter::{FILTERS, FilterKind, FilterParams, FilterSlope};
pub use lfo::{LFO_POSITIONS, LfoBank, LfoParams, LfoPositions, LfoWaveform, MAX_LFOS};
pub use midi::MidiMessage;
pub use mod_envelope::{EnvelopeBank, MAX_ENVELOPES};
pub use modulation::{
    ENVELOPE_STAGES, MAX_PITCH_DEPTH, MAX_ROUTES, MODULATION_WORDS, ModCounts, ModMatrix, ModRoute,
    ModSlot, ModSource, ModTarget, ModulationValues, time_offset,
};
pub use operator::{
    KeyScaling, MAX_FEEDBACK, MAX_FOLD, MAX_FREQUENCY, MAX_KEY_SCALING, MAX_MODULATION, MAX_RATIO,
    MAX_SEMITONES, MAX_SYNC_OCTAVES, ModulationMode, OPERATORS, OperatorKind, OperatorParams,
    PitchMode,
};
pub use patch::{MAX_DETUNE, MAX_TUNE, Patch};
pub use presets::{
    PITCH_BEND_RANGE, PRESET_COUNT, Preset, categories, preset, preset_category, preset_name,
    presets,
};
pub use reverb::ReverbParams;
pub use synth::{Synth, add};
pub use voicing::{MAX_PORTAMENTO, MAX_UNISON, VoiceMode};
pub use waveform::Waveform;

/// Maximum polyphony.
pub const MAX_VOICES: usize = 16;

/// Samples per control-rate block. Envelopes and smoothed parameters are
/// updated once per block and linearly interpolated within it.
pub const BLOCK_SIZE: usize = 32;

/// Clamps `x` into `min..=max`, or returns `fallback` if `x` is not finite.
fn clamp_or(x: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(min, max)
    } else {
        fallback
    }
}
