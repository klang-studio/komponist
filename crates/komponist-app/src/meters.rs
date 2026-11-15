//! Meter ballistics of the master and every track, fed from the engine once per frame.
//!
//! The engine's `TrackMeter`s hold the peaks since the last read and reset on read, so exactly
//! one place takes them: [`Meters::update`] at the start of a frame. It runs the ballistics for
//! every strip, visible or not, so a clip latches even while the mixer is hidden. Widgets then
//! draw a [`MeterState`] with zero new peaks (`Meter::new(state, Meters::NO_PEAKS, …)`), which
//! changes nothing in the same frame.

use std::collections::HashMap;

use std::sync::Arc;

use komponist_engine::{DeviceMeter, SampleTap};
use komponist_model::{DeviceId, TrackId};
use komponist_ui::widgets::meter::{MeterState, fraction};

use crate::engine_sync::EngineSync;
use crate::spectrum::{Analysis, SpectrumAnalyzer};

/// The largest gain reduction the display scale shows by default (the GR bar is full at this many
/// dB); a device can set its own range ([`DeviceMeter::gr_range_db`], the limiter shows 12 dB).
pub const GR_RANGE_DB: f32 = DeviceMeter::DEFAULT_GR_RANGE_DB;
/// How fast the gain reduction bar falls back, dB per second.
pub const GR_FALL_DB_PER_S: f32 = 40.0;

/// What a device's level meter (`meter_gr`) shows: the bars In · GR · Out as 0–1 and the
/// reduction in dB for the read-out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceLevels {
    /// In, GR, Out, each 0–1.
    pub bars: [f32; 3],
    /// The reduction shown, dB ≥ 0.
    pub reduction_db: f32,
}

/// Ballistics of one device's meter: In and Out as a [`MeterState`] (channel 0 and 1), the
/// reduction falling at [`GR_FALL_DB_PER_S`].
#[derive(Debug, Clone)]
struct DeviceBallistics {
    io: MeterState,
    reduction_db: f32,
    /// Reduction in dB at which the GR bar is full.
    range_db: f32,
    last: Option<f64>,
}

impl Default for DeviceBallistics {
    fn default() -> Self {
        DeviceBallistics {
            io: MeterState::default(),
            reduction_db: 0.0,
            range_db: GR_RANGE_DB,
            last: None,
        }
    }
}

impl DeviceBallistics {
    fn tick(&mut self, (input, reduction, output): (f32, f32, f32), now: f64) {
        let dt = self.last.map_or(0.0, |t| (now - t).max(0.0) as f32);
        self.last = Some(now);
        self.io.tick([input, output], now);
        self.reduction_db = reduction
            .max(self.reduction_db - GR_FALL_DB_PER_S * dt)
            .max(0.0);
    }

    fn levels(&self) -> DeviceLevels {
        DeviceLevels {
            bars: [
                fraction(self.io.level_db(0)),
                (self.reduction_db / self.range_db).clamp(0.0, 1.0),
                fraction(self.io.level_db(1)),
            ],
            reduction_db: self.reduction_db,
        }
    }

    fn is_active(&self) -> bool {
        self.io.is_active() || self.reduction_db > 0.0
    }
}

/// Ballistics of the master, the tracks of the current program and the internal devices.
#[derive(Debug, Default)]
pub struct Meters {
    /// The master strip (transport bar, mixer master).
    pub master: MeterState,
    tracks: HashMap<TrackId, MeterState>,
    devices: HashMap<DeviceId, DeviceBallistics>,
    /// The sample taps of the devices that have one (WP-92) and the analyzers of the ones whose
    /// spectrum is visible right now.
    taps: HashMap<DeviceId, Arc<SampleTap>>,
    spectra: HashMap<DeviceId, SpectrumAnalyzer>,
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
        // The internal devices' meters; devices that left the host are dropped.
        let mut seen = Vec::new();
        engine.host().for_each_meter(&mut |id, meter| {
            let ballistics = self.devices.entry(id).or_default();
            ballistics.range_db = meter.gr_range_db();
            ballistics.tick(meter.take(), now);
            if let Some(tap) = meter.tap() {
                // A new processor (sample rate change) brings a new tap and a fresh analyzer.
                if self.taps.get(&id).is_none_or(|old| !Arc::ptr_eq(old, tap)) {
                    self.taps.insert(id, Arc::clone(tap));
                    self.spectra.remove(&id);
                }
            }
            seen.push(id);
        });
        self.devices.retain(|id, _| seen.contains(id));
        self.taps.retain(|id, _| seen.contains(id));
        self.spectra.retain(|id, _| seen.contains(id));
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

    /// What the level meter of internal device `device` shows; `None` for a device without one.
    pub fn device(&self, device: DeviceId) -> Option<DeviceLevels> {
        self.devices.get(&device).map(DeviceBallistics::levels)
    }

    /// What the card of `device` shows of its signal (WP-92); `None` for a device without a sample
    /// tap. `show` says whether the spectrum is visible now (the EQ's frame is on screen, running,
    /// not bypassed, „Analyse“ on): only then the analyzer runs and the tap's lease is renewed, so
    /// everything else costs nothing; otherwise the spectrum is empty and the analyzer is dropped.
    pub fn analysis(&mut self, device: DeviceId, show: bool, now: f64) -> Option<Analysis> {
        let tap = self.taps.get(&device)?;
        let rate = tap.rate();
        if !show {
            self.spectra.remove(&device);
            return Some(Analysis {
                rate,
                spectrum: Arc::default(),
            });
        }
        let analyzer = self.spectra.entry(device).or_default();
        analyzer.update(now, tap);
        Some(Analysis {
            rate,
            spectrum: Arc::clone(analyzer.values()),
        })
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
        self.master.is_active()
            || self.tracks.values().any(MeterState::is_active)
            || self.devices.values().any(DeviceBallistics::is_active)
            || self.spectra.values().any(SpectrumAnalyzer::is_active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gr_bar_is_full_at_the_devices_range() {
        let mut b = DeviceBallistics {
            range_db: 12.0,
            ..DeviceBallistics::default()
        };
        b.tick((0.0, 6.0, 0.0), 0.0);
        assert!((b.levels().bars[1] - 0.5).abs() < 1e-6);
        b.tick((0.0, 20.0, 0.0), 0.0);
        assert_eq!(b.levels().bars[1], 1.0);
        let mut default = DeviceBallistics::default();
        default.tick((0.0, 6.0, 0.0), 0.0);
        assert!((default.levels().bars[1] - 0.25).abs() < 1e-6);
    }
}
