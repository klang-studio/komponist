//! Rendering constants and data for a dynamics graph.

use std::sync::Arc;

/// Columns of the history.
pub const POINTS: usize = 128;
/// Seconds between two reads (about 30 per second).
pub const INTERVAL: f64 = 1.0 / 30.0;
/// A gap in the updates longer than this (seconds) means the card was hidden: start over.
pub(crate) const STALE: f64 = 0.4;
/// The dB at the bottom of the input level's bumps and the dB range they span; a full-scale input
/// reaches [`LEVEL_HEIGHT`] of the graph's height, so the bumps stay under the reduction line.
pub const LEVEL_FLOOR_DB: f32 = -60.0;
/// See [`LEVEL_FLOOR_DB`].
pub const LEVEL_HEIGHT: f32 = 0.4;

/// What a compressor card shows of its signal.
#[derive(Debug, Clone, PartialEq)]
pub struct Dynamics {
    /// The detector level in dB for the live point; `None` while no samples come in.
    pub level_db: Option<f32>,
    /// The input level per column, 0–1 of the graph's height.
    pub input: Arc<[f32]>,
    /// The reduction per column in dB, ≤ 0.
    pub reduction: Arc<[f32]>,
}
