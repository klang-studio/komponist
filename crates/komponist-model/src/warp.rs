//! Affine tempo-follow warp data and native source-frame/tick mapping (#14 WP-C).
//!
//! V1 has no markers: the shared `AudioClip::original_tempo` is the single time-scale authority.
//! A persisted tick phase makes split/trim shifts telescope exactly even across different take
//! rates. Mapping uses the exact rational value of the validated f64 tempo, not accumulated
//! rounded frame deltas. No stretcher, engine or UI dependency belongs here.

use crate::{AudioTake, PPQ, Tempo, Tick};
use serde::{Deserialize, Serialize};

/// Maximum absolute mapping phase/end, keeping all rational products within u128.
pub const MAX_WARP_TICK: i64 = 1 << 40;

/// Optional v1 warp state. Its presence alone does not enable warp.
/// Serialized inside `AudioClip` while `time_mode` remains `fixed` when enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "StoredWarp", into = "StoredWarp")]
pub struct AudioWarp {
    enabled: bool,
    phase: Tick,
}
#[derive(Serialize, Deserialize)]
struct StoredWarp {
    version: u32,
    enabled: bool,
    #[serde(default)]
    phase: Tick,
}
impl From<AudioWarp> for StoredWarp {
    fn from(w: AudioWarp) -> Self {
        Self {
            version: 1,
            enabled: w.enabled,
            phase: w.phase,
        }
    }
}
impl TryFrom<StoredWarp> for AudioWarp {
    type Error = WarpError;
    fn try_from(w: StoredWarp) -> Result<Self, Self::Error> {
        if w.version != 1 {
            return Err(WarpError::Version);
        }
        check_phase(w.phase)?;
        Ok(Self {
            enabled: w.enabled,
            phase: w.phase,
        })
    }
}
impl AudioWarp {
    /// A fresh local mapping, retaining the clip's existing original tempo.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            phase: Tick::ZERO,
        }
    }
    /// Whether tempo-follow Warp is selected.
    pub fn enabled(self) -> bool {
        self.enabled
    }
    /// Mapping reference tick at the current window start; source-preserving cuts advance it,
    /// while moving a clip on the arrangement leaves it unchanged.
    pub fn phase(self) -> Tick {
        self.phase
    }
    pub(crate) fn shifted(self, delta: Tick) -> Result<Self, WarpError> {
        let phase = Tick(self.phase.0.checked_add(delta.0).ok_or(WarpError::Phase)?);
        check_phase(phase)?;
        Ok(Self { phase, ..self })
    }
}

/// A checked mapping error; no source IO is performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarpError {
    Version,
    Phase,
    Tempo,
    SampleRate,
    SourcePosition,
    Inactive,
    Take,
    Mode,
}
impl WarpError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::Version => "unsupported audio warp version",
            Self::Phase => "audio warp tick position out of range",
            Self::Tempo => "audio warp original tempo out of range",
            Self::SampleRate => "audio warp source has no sample rate",
            Self::SourcePosition => "audio warp source frame out of range",
            Self::Inactive => "audio warp is not enabled",
            Self::Take => "audio warp has no such take",
            Self::Mode => "enabled audio warp requires fixed time mode",
        }
    }
}
impl std::fmt::Display for WarpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}
impl std::error::Error for WarpError {}
fn check_phase(phase: Tick) -> Result<(), WarpError> {
    if (-MAX_WARP_TICK..=MAX_WARP_TICK).contains(&phase.0) {
        Ok(())
    } else {
        Err(WarpError::Phase)
    }
}

/// One take's immutable affine mapping. Positions are clip-local ticks and absolute file frames.
/// The same tempo/phase with each take's own rate preserves multi-take alignment.
#[derive(Debug, Clone, Copy)]
pub struct WarpMap {
    offset: u64,
    phase: Tick,
    numerator: u128,
    denominator: u128,
    original: Tempo,
}
impl WarpMap {
    pub(crate) fn new(
        take: &AudioTake,
        warp: AudioWarp,
        original: Tempo,
    ) -> Result<Self, WarpError> {
        if !warp.enabled {
            return Err(WarpError::Inactive);
        }
        check_phase(warp.phase)?;
        if !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&original.bpm) {
            return Err(WarpError::Tempo);
        }
        if take.file.sample_rate == 0 {
            return Err(WarpError::SampleRate);
        }
        // A valid BPM is normal, exponent 5..7: BPM = significand / 2^(45..47).
        // At most 32 + 6 + 47 + 40 bits in the tick product, comfortably inside u128.
        let bits = original.bpm.to_bits();
        let shift = 1075 - ((bits >> 52) & 0x7ff);
        let significand = (bits & ((1_u64 << 52) - 1)) | (1_u64 << 52);
        let numerator = (u128::from(take.file.sample_rate) * 60) << shift;
        let denominator = u128::from(significand) * PPQ as u128;
        Ok(Self {
            offset: take.offset,
            phase: warp.phase,
            numerator,
            denominator,
            original,
        })
    }
    fn project_tick(self, tick: i64) -> i128 {
        let product = u128::from(tick.unsigned_abs()) * self.numerator;
        let whole = product / self.denominator;
        let round = u128::from(product % self.denominator >= self.denominator.div_ceil(2));
        let frames = (whole + round) as i128;
        if tick < 0 { -frames } else { frames }
    }
    fn position(self, local: Tick) -> Result<i128, WarpError> {
        let tick = self.phase.0.checked_add(local.0).ok_or(WarpError::Phase)?;
        check_phase(Tick(tick))?;
        Ok(i128::from(self.offset) + self.project_tick(tick) - self.project_tick(self.phase.0))
    }
    /// Nearest native frame, ties away from zero at the shared mapping origin. Negative local
    /// ticks are permitted for trim extension; a result before file frame zero is an error.
    pub fn source_frame(self, local: Tick) -> Result<u64, WarpError> {
        u64::try_from(self.position(local)?).map_err(|_| WarpError::SourcePosition)
    }
    /// Whether the next map continues this one from `at` for the following `length` ticks.
    pub fn continues_with(self, at: Tick, next: Self, length: Tick) -> bool {
        let Some(end) = at.0.checked_add(length.0).map(Tick) else {
            return false;
        };
        let Ok(source_end) = self.source_frame(end) else {
            return false;
        };
        if length < Tick::ZERO
            || self.numerator != next.numerator
            || self.denominator != next.denominator
            || self.source_frame(at) != Ok(next.offset)
            || next.source_frame(length) != Ok(source_end)
        {
            return false;
        }

        // At this point the anchor is valid and both maps have the same slope. Translating the
        // phase by a multiple of the reduced denominator preserves its fractional frame phase.
        let phase = self.phase.0 + at.0;
        if phase == next.phase.0 {
            return true;
        }
        let (mut a, mut b) = (self.numerator, self.denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let period = (self.denominator / a) as i128;
        let first = i128::from(phase.min(next.phase.0));
        let shift = i128::from(phase.max(next.phase.0)) - first;
        if shift % period != 0 {
            return false;
        }

        // An exact half-frame rounds in opposite directions on opposite sides of tick zero.
        // In the earlier phase, those signs differ on [-shift, -1]. Check for a tie there.
        let crossing_start = first.max(-shift);
        let crossing_end = (first + i128::from(length.0)).min(-1);
        period % 2 != 0
            || (period / 2 - crossing_start).rem_euclid(period) > crossing_end - crossing_start
    }

    /// Native frames consumed by a nonnegative clip-local duration.
    pub fn source_frames(self, length: Tick) -> Result<u64, WarpError> {
        if length < Tick::ZERO {
            return Err(WarpError::Phase);
        }
        let delta = self.position(length)? - i128::from(self.offset);
        u64::try_from(delta).map_err(|_| WarpError::SourcePosition)
    }
    /// Continuous native source position for waveform projection. The exact phase remainder
    /// avoids subtracting huge floating-point origins after repeated cuts. Rendering only:
    /// edits and source readouts use the checked, rounded `source_frame` contract above.
    pub fn source_position(self, local: f64) -> f64 {
        let product = u128::from(self.phase.0.unsigned_abs()) * self.numerator;
        let remainder = product % self.denominator;
        let nearest = u128::from(remainder >= self.denominator.div_ceil(2));
        let fraction = remainder as f64 / self.denominator as f64 - nearest as f64;
        let correction = if self.phase.0 < 0 {
            -fraction
        } else {
            fraction
        };
        self.offset as f64 + correction + local * self.numerator as f64 / self.denominator as f64
    }
    /// Inverse of `source_position`, for sub-tick waveform layout and pointer anchoring.
    pub fn tick_at_source_position(self, frame: f64) -> f64 {
        (frame - self.source_position(0.0)) * self.denominator as f64 / self.numerator as f64
    }

    /// Latest local tick whose rounded source position does not exceed `frame`, clamped to
    /// the bounded mapping domain. Errors when `frame` precedes the entire domain.
    pub fn tick_at_source_frame(self, frame: u64) -> Result<Tick, WarpError> {
        let mut low = -MAX_WARP_TICK - self.phase.0;
        let mut high = MAX_WARP_TICK - self.phase.0;
        if self.position(Tick(low))? > i128::from(frame) {
            return Err(WarpError::SourcePosition);
        }
        while low < high {
            let mid = low + (high - low + 1) / 2;
            if self.position(Tick(mid))? <= i128::from(frame) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        Ok(Tick(low))
    }
    /// Longest nonnegative duration consuming at most this many native source frames.
    pub fn ticks_for_frames(self, frames: u64) -> Tick {
        let mut low = 0;
        let mut high = MAX_WARP_TICK - self.phase.0;
        while low < high {
            let mid = low + (high - low + 1) / 2;
            // Every midpoint lies inside the checked domain; differences fit u64 by construction.
            let delta = self.project_tick(self.phase.0 + mid) - self.project_tick(self.phase.0);
            if delta <= i128::from(frames) {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        Tick(low)
    }
    /// Maximum backwards trim preserving a nonnegative native offset.
    pub fn available_before(self) -> Tick {
        let mut low = 0;
        let mut high = MAX_WARP_TICK + self.phase.0;
        while low < high {
            let mid = low + (high - low + 1) / 2;
            let position = i128::from(self.offset) + self.project_tick(self.phase.0 - mid)
                - self.project_tick(self.phase.0);
            if position >= 0 {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
        Tick(low)
    }
    /// Output/input duration ratio at the project tempo (1/6–6 for valid model tempos).
    /// Callers must also check backend sample-rate and source-input capabilities.
    pub fn duration_ratio(self, project: Tempo) -> Result<f64, WarpError> {
        if !(Tempo::MIN_BPM..=Tempo::MAX_BPM).contains(&project.bpm) {
            return Err(WarpError::Tempo);
        }
        Ok(self.original.bpm / project.bpm)
    }
}

#[cfg(test)]
#[path = "warp_tests.rs"]
mod tests;
