//! The live picture of a dynamics processor from its paired telemetry tap.
//!
//! The device appends pairs `[level in dB, reduction in dB (≥ 0)]` while the UI looks.
//! A [`GainTrace`] reads the latest four seconds of pairs at
//! most every [`INTERVAL`] and turns them into what the card draws:
//!
//! - the **level point** on the transfer curve: the loudest level of the last interval,
//! - the **history**: [`POINTS`] columns over [`HISTORY_SECONDS`]; the input level as 0–1 for the
//!   graph's `Data` layer and the reduction as dB (≤ 0) for its `Trace` layer. A column holds the
//!   maximum of the pairs it covers, so a short peak never disappears. Right after the display
//!   starts only the pairs since then count: the left part of the history stays empty.
//!
//! Like the EQ's spectrum the trace only works while someone calls [`GainTrace::update`], and every
//! call renews the tap's lease, so a hidden compressor costs the audio thread nothing. Without new
//! samples (the transport is stopped) the history stays as it is and the level point goes.

use std::sync::Arc;

use komponist_engine::SampleTap;
use komponist_engine::tap::{
    DYNAMICS_HISTORY_SECONDS as HISTORY_SECONDS, dynamics_pair_frames as pair_frames,
};

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

/// Reads one tap. Keep one per displayed device; drop it when the display goes away.
#[derive(Debug)]
pub struct GainTrace {
    samples: Vec<f32>,
    input: Arc<[f32]>,
    reduction: Arc<[f32]>,
    level_db: Option<f32>,
    last_tick: Option<f64>,
    last_written: u64,
    started_at: u64,
}

impl Default for GainTrace {
    fn default() -> GainTrace {
        GainTrace {
            samples: Vec::new(),
            input: vec![0.0; POINTS].into(),
            reduction: vec![0.0; POINTS].into(),
            level_db: None,
            last_tick: None,
            last_written: 0,
            started_at: 0,
        }
    }
}

/// Pairs the history covers at `rate`.
pub fn history_pairs(rate: f32) -> usize {
    (HISTORY_SECONDS * rate / pair_frames(rate) as f32).ceil() as usize
}

impl GainTrace {
    /// A trace showing nothing.
    pub fn new() -> GainTrace {
        GainTrace::default()
    }

    /// What the card shows now.
    pub fn view(&self) -> Dynamics {
        Dynamics {
            level_db: self.level_db,
            input: Arc::clone(&self.input),
            reduction: Arc::clone(&self.reduction),
        }
    }

    /// Whether the picture changes without new input (never: a still history is not animated).
    pub fn is_active(&self) -> bool {
        self.level_db.is_some()
    }

    fn reset(&mut self) {
        Arc::make_mut(&mut self.input).fill(0.0);
        Arc::make_mut(&mut self.reduction).fill(0.0);
        self.level_db = None;
        self.last_tick = None;
    }

    /// Call every frame the card is visible, with the time in seconds. Renews the tap's lease and,
    /// at most every [`INTERVAL`], reads the tap's latest pairs.
    pub fn update(&mut self, now: f64, tap: &SampleTap) {
        tap.want();
        let rate = tap.rate();
        if rate <= 0.0 {
            return;
        }
        if self.last_tick.is_some_and(|t| now - t > STALE) {
            self.reset();
        }
        match self.last_tick {
            None => {
                // Pairs from before the display started are stale.
                self.started_at = tap.written();
                self.last_written = self.started_at;
            }
            Some(t) if now - t < INTERVAL * 0.9 => return,
            Some(_) => {}
        }
        self.last_tick = Some(now);
        let written = tap.written();
        let fresh = written != self.last_written;
        self.last_written = written;
        self.level_db = None;
        if !fresh {
            return;
        }
        // Whole pairs since the display started, at most the history.
        let pairs = (((written - self.started_at) / 2) as usize).min(history_pairs(rate));
        if pairs == 0 {
            return;
        }
        self.samples.resize(2 * pairs, 0.0);
        // The device writes whole pairs, so a window ends between two of them; an odd end (not
        // from the device) would swap level and reduction, so the picture keeps its last state.
        let Some(end) = tap.read_latest(&mut self.samples) else {
            return;
        };
        if end % 2 == 0 {
            let have = std::mem::take(&mut self.samples);
            self.fold(&have, rate);
            self.samples = have;
        }
    }

    /// Folds the pairs `have` (oldest first) into the columns, the newest in the last one.
    fn fold(&mut self, have: &[f32], rate: f32) {
        let pairs = have.len() / 2;
        let all = history_pairs(rate);
        let (input, reduction) = (
            Arc::make_mut(&mut self.input),
            Arc::make_mut(&mut self.reduction),
        );
        input.fill(0.0);
        reduction.fill(0.0);
        for (i, pair) in have.as_chunks::<2>().0.iter().enumerate() {
            // Age in pairs from the newest, then the column in a history that is `all` pairs long.
            let age = pairs - 1 - i;
            let column = POINTS - 1 - (age * POINTS / all).min(POINTS - 1);
            let (level, gr) = (pair[0], pair[1]);
            if level.is_finite() {
                let h = ((level - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0) * LEVEL_HEIGHT;
                input[column] = input[column].max(h);
            }
            if gr.is_finite() {
                reduction[column] = reduction[column].min(-gr.max(0.0));
            }
        }
        // The live point: the loudest level of the last interval.
        let recent = ((INTERVAL * f64::from(rate) / pair_frames(rate) as f64).ceil() as usize)
            .clamp(1, pairs);
        self.level_db = have[2 * (pairs - recent)..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| p[0])
            .filter(|l| l.is_finite())
            .reduce(f32::max);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tap holding `pairs` of `(level, reduction)` at `rate`.
    fn tap_with(pairs: &[[f32; 2]], rate: f32) -> SampleTap {
        let tap = SampleTap::new(32_768);
        tap.set_rate(rate);
        let flat: Vec<f32> = pairs.iter().flatten().copied().collect();
        tap.push(&flat);
        tap
    }

    #[test]
    fn the_history_is_four_seconds_of_pairs() {
        assert_eq!(history_pairs(48_000.0), 6000);
        assert_eq!(history_pairs(96_000.0), 6000);
        assert_eq!(history_pairs(44_100.0), 5513);
    }

    #[test]
    fn a_trace_shows_the_level_point_and_keeps_peaks_in_their_column() {
        let rate = 48_000.0;
        let mut trace = GainTrace::new();
        let tap = tap_with(&[], rate);
        // The display starts: pairs from now on count.
        trace.update(0.0, &tap);
        assert!(tap.is_wanted(), "looking renews the lease");
        // 3 s of pairs: quiet, with one short deep reduction one second ago.
        let mut pairs = vec![[-40.0, 0.0]; 4500];
        let spike = 4500 - 1500;
        pairs[spike] = [-6.0, 14.0];
        let flat: Vec<f32> = pairs.iter().flatten().copied().collect();
        tap.push(&flat);
        trace.update(0.05, &tap);
        let view = trace.view();
        assert_eq!(view.level_db, Some(-40.0));
        assert_eq!(view.reduction.len(), POINTS);
        // The spike is a quarter of the history from the right edge, and not lost.
        let deepest = view.reduction.iter().copied().fold(0.0, f32::min);
        assert_eq!(deepest, -14.0);
        let at = view.reduction.iter().position(|v| *v == -14.0).unwrap();
        let expect = POINTS - 1 - 1500 * POINTS / 6000;
        assert!(at.abs_diff(expect) <= 1, "{at} vs {expect}");
        // Nothing before the display started: the first column is empty.
        assert_eq!(view.input[0], 0.0);
        assert!(view.input[POINTS - 1] > 0.0);
        // No new samples: the point goes, the history stays.
        trace.update(0.2, &tap);
        let still = trace.view();
        assert_eq!(still.level_db, None);
        assert_eq!(still.reduction, view.reduction);
        // A gap restarts the picture.
        trace.update(2.0, &tap);
        assert_eq!(
            trace.view().reduction.iter().copied().fold(0.0, f32::min),
            0.0
        );
    }
}
