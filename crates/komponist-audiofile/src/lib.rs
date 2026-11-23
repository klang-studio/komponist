//! Audio files for komponist (#10): decoding, sample-rate conversion, waveform peaks and the one
//! file pool that the Sampler and the audio clips share.
//!
//! A leaf crate (no studio crate, no egui, no engine): `komponist-builtin` (the Sampler's loader)
//! uses it today, the engine's disk streaming (#11) and the arrangement's waveforms (#12) build
//! on it. Nothing here runs inside the audio thread: decoding, resampling and peak computation
//! allocate and do file IO, so they run on loader or UI-adjacent worker threads and hand finished
//! `Arc`s over. See `docs/ARCHITECTURE.md` §6.10.
//!
//! - [`AudioFile`]: a seekable, chunked decoder (WAV through `hound`; FLAC, OGG Vorbis and MP3
//!   through `symphonia`) delivering interleaved `f32` frames from any frame offset.
//! - [`decode_all`] and [`Decoded`]: the whole file in memory as planar `f32`.
//! - [`resample`] and [`StreamResampler`]: sample-rate conversion (rubato's FFT resampler).
//! - [`PeakData`] and [`PeakBuilder`]: multi-resolution min/max peaks per channel, with a disk
//!   cache ([`peaks::cache`]).
//! - [`FilePool`] and [`PoolFile`]: one entry per file on disk, shared by path and identity,
//!   with the decoded audio, derived data and peaks behind it.

#![forbid(unsafe_code)]

mod decode;
mod error;
pub mod peaks;
mod pool;
mod resample;
mod tempo;

pub use decode::{AudioFile, AudioInfo, Decoded, Format, decode_all};
pub use error::AudioError;
pub use peaks::{Peak, PeakBuilder, PeakData};
pub use pool::{FileId, FilePool, PeakState, PoolFile, TempoState};
pub use resample::{StreamResampler, resample};
pub use tempo::TempoEstimate;

/// Longest channel count the decoders deliver: files with more channels give their first two.
pub const MAX_CHANNELS: usize = 2;
