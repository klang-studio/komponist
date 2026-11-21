//! Bounded CLAP gain-reduction and detector telemetry. Shared by the author SDK and both hosts.

use core::ffi::{CStr, c_void};

/// Plugin extension ID. See `docs/clap-device-layout/audio-meter.md`.
pub const EXTENSION_ID: &CStr = c"org.klang-studio.komponist.audio-meter/1";
/// The largest detector frame count one block can report.
pub const DETECTOR_FRAMES: usize = 64;

/// A block's gain reduction, and optionally the ordered detector readings from that block.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockMeter {
    /// Maximum gain reduction in dB (positive).
    pub reduction_db: f32,
    /// Number of initialized detector pairs in `detector`.
    pub detector_frames: u32,
    /// For each detector frame, peak level in dBFS and maximum gain reduction in positive dB.
    pub detector: [[f32; 2]; DETECTOR_FRAMES],
}

impl BlockMeter {
    /// The zero block.
    pub const EMPTY: Self = Self {
        reduction_db: 0.0,
        detector_frames: 0,
        detector: [[0.0; 2]; DETECTOR_FRAMES],
    };

    /// Check that a remote plugin supplied finite levels and a detector count within capacity.
    pub fn validated(&self) -> bool {
        self.reduction_db.is_finite()
            && self.reduction_db >= 0.0
            && self
                .detector
                .get(..self.detector_frames as usize)
                .is_some_and(|pairs| {
                    pairs
                        .iter()
                        .all(|&[level, gain]| level.is_finite() && gain.is_finite() && gain >= 0.0)
                })
    }
}

/// The plugin's C callback table. Both callbacks run on the audio thread before/after one process
/// call. The opaque pointer is the original `clap_plugin_t`.
#[repr(C)]
pub struct Extension {
    /// Start a meter interval. Nonzero requests detector samples in addition to reduction.
    pub begin: Option<unsafe extern "C" fn(plugin: *const c_void, detector: u32)>,
    /// Copy the interval into the supplied meter buffer and return 1, or return 0 if unavailable.
    pub read: Option<unsafe extern "C" fn(plugin: *const c_void, data: *mut BlockMeter) -> u32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_lengths_and_levels_are_validated() {
        let mut readings = BlockMeter::EMPTY;
        assert!(readings.validated());
        readings.detector[0] = [-9.0, 3.0];
        readings.detector_frames = 1;
        readings.reduction_db = 3.0;
        assert!(readings.validated());
        readings.detector_frames = DETECTOR_FRAMES as u32 + 1;
        assert!(!readings.validated());
        readings.detector_frames = 1;
        readings.detector[0][0] = f32::NAN;
        assert!(!readings.validated());
        readings.detector_frames = 0;
        readings.reduction_db = -3.0;
        assert!(!readings.validated());
    }
}
