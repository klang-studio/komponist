//! Meter ballistics of the master and every track, fed from the engine once per frame.
//!
//! The engine's `TrackMeter`s hold the peaks since the last read and reset on read, so exactly
//! one place takes them: [`Meters::update`] at the start of a frame. It runs the ballistics for
//! every strip, visible or not, so a clip latches even while the mixer is hidden. Widgets then
//! draw a [`MeterState`] with zero new peaks (`Meter::new(state, Meters::NO_PEAKS, …)`), which
//! changes nothing in the same frame.

use std::collections::HashMap;

use komponist_model::TrackId;
use komponist_ui::widgets::meter::MeterState;

use crate::engine_sync::EngineSync;

/// Ballistics of the master and the tracks of the current program.
#[derive(Debug, Clone, Default)]
pub struct Meters {
    /// The master strip (transport bar, mixer master).
    pub master: MeterState,
    tracks: HashMap<TrackId, MeterState>,
}

impl Meters {
    /// The peaks to pass to a `Meter` widget drawn after [`Meters::update`].
    pub const NO_PEAKS: [f32; 2] = [0.0, 0.0];

    /// No strips yet.
    pub fn new() -> Meters {
        Meters::default()
    }

    /// Takes the peaks of the master and every track of the engine's program and advances the
    /// ballistics to `now` (seconds, monotonic). Tracks that left the program are dropped.
    pub fn update(&mut self, engine: &EngineSync, now: f64) {
        let (l, r) = engine.telemetry().master_meter().take();
        self.master.tick([l, r], now);
        let Some(program) = engine.program() else {
            self.tracks.clear();
            return;
        };
        self.tracks
            .retain(|id, _| program.tracks.iter().any(|t| t.id == *id));
        for track in &program.tracks {
            let (l, r) = track.meter.take();
            self.tracks.entry(track.id).or_default().tick([l, r], now);
        }
    }

    /// The meter of `track`, if it is in the program.
    pub fn track(&self, track: TrackId) -> Option<&MeterState> {
        self.tracks.get(&track)
    }

    /// The meter of `track`, mutable (click resets).
    pub fn track_mut(&mut self, track: TrackId) -> Option<&mut MeterState> {
        self.tracks.get_mut(&track)
    }

    /// Resets the peak hold and clip latch of `track` (`None` = master), or of every strip.
    pub fn reset(&mut self, track: Option<TrackId>, all: bool) {
        if all {
            self.master.reset();
            self.tracks.values_mut().for_each(MeterState::reset);
            return;
        }
        match track {
            None => self.master.reset(),
            Some(id) => {
                if let Some(m) = self.tracks.get_mut(&id) {
                    m.reset();
                }
            }
        }
    }

    /// Whether any strip still shows something (the UI keeps repainting while it falls).
    pub fn is_active(&self) -> bool {
        self.master.is_active() || self.tracks.values().any(MeterState::is_active)
    }
}
