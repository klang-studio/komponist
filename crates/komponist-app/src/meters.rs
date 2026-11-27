//! Meter ballistics of the master and every track, fed from the engine once per frame.
//!
//! The engine's `TrackMeter`s hold the peaks since the last read and reset on read, so exactly
//! one place takes them: [`Meters::update`] at the start of a frame. It runs the ballistics for
//! every strip, visible or not, so a clip latches even while the mixer is hidden. Widgets then
//! draw a [`MeterState`] with zero new peaks (`Meter::new(state, Meters::NO_PEAKS, …)`), which
//! changes nothing in the same frame.

use std::collections::HashMap;

use std::sync::{Arc, Weak};

use komponist_engine::{DeviceMeter, SampleTap};
use komponist_ipc::signal_taps::PadActivity;
use komponist_model::{DeviceId, TrackId};
use komponist_plugins::signal_taps::SignalTap;
use komponist_ui::widgets::meter::MeterState;

use crate::engine_sync::EngineSync;
use crate::gain_trace::{self, Dynamics};
use crate::master_scope::MasterScope;
use crate::spectrum::{Analysis, SpectrumAnalyzer};

/// The largest gain reduction the display scale shows by default (the GR bar is full at this many
/// dB).
pub const GR_RANGE_DB: f32 = DeviceMeter::DEFAULT_GR_RANGE_DB;
/// How fast the gain reduction bar falls back, dB per second.
pub const GR_FALL_DB_PER_S: f32 = 40.0;
const PLAYHEAD_FRESH_SECONDS: f64 = 0.5;
const REDUCTION_VISIBILITY_SECONDS: f64 = 0.5;

/// What a device's level meter (`meter_gr`) shows: the bars In · GR · Out as 0–1 and the
/// reduction in dB for the read-out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceLevels {
    /// In, GR, Out, each 0–1.
    pub bars: [f32; 3],
    /// The reduction shown, dB ≥ 0.
    pub reduction_db: f32,
}

#[derive(Debug)]
struct PlayheadReading {
    cursor: u64,
    received_at: Option<f64>,
    visible_at: f64,
}

#[derive(Debug)]
struct ReductionReading {
    cursor: u64,
    reduction_db: Option<f32>,
    visible_at: f64,
}

impl ReductionReading {
    fn update(&mut self, sample: Option<(u64, f32)>, now: f64) -> Option<f32> {
        let dt = (now - self.visible_at).max(0.0) as f32;
        self.visible_at = now;
        let mut reduction = self
            .reduction_db
            .map(|db| (db - GR_FALL_DB_PER_S * dt).max(0.0));
        if let Some((cursor, db)) = sample
            && cursor != self.cursor
        {
            self.cursor = cursor;
            reduction = Some(db.max(reduction.unwrap_or(0.0)));
        }
        self.reduction_db = reduction;
        reduction
    }
}

#[derive(Debug, Default)]
struct NamedSpectrum {
    tap: Weak<SampleTap>,
    visible_at: f64,
    analyzer: SpectrumAnalyzer,
}

impl NamedSpectrum {
    fn update(&mut self, tap: &Arc<SampleTap>, now: f64) {
        if !std::ptr::eq(self.tap.as_ptr(), Arc::as_ptr(tap)) {
            self.tap = Arc::downgrade(tap);
            self.analyzer.reset();
        }
        self.visible_at = now;
        self.analyzer.update_samples(now, tap);
    }
}

#[derive(Debug)]
struct SignalHistory {
    tap: Weak<SampleTap>,
    visible_at: f64,
    last_tick: f64,
    started_at: u64,
    cursor: u64,
    latest: Option<f32>,
    columns: Arc<[f32]>,
}

impl SignalHistory {
    fn new(tap: &Arc<SampleTap>, now: f64) -> Self {
        let cursor = tap.written();
        Self {
            tap: Arc::downgrade(tap),
            visible_at: now,
            last_tick: now,
            started_at: cursor,
            cursor,
            latest: None,
            columns: Arc::from([0.0; gain_trace::POINTS]),
        }
    }

    fn update(
        &mut self,
        tap: &Arc<SampleTap>,
        now: f64,
        samples: &mut Vec<f32>,
        value_at: impl Fn(f32, f32) -> f32,
    ) {
        use komponist_ui::widgets::device::graph::configs::GR_HISTORY_SECONDS;

        if !std::ptr::eq(self.tap.as_ptr(), Arc::as_ptr(tap))
            || now - self.visible_at > gain_trace::STALE
        {
            *self = Self::new(tap, now);
            return;
        }
        self.visible_at = now;
        // This also shares the same snapshot between graph cells using the same tap.
        if now - self.last_tick < gain_trace::INTERVAL * 0.9 {
            return;
        }
        self.last_tick = now;
        self.latest = None;
        let written = tap.written();
        if written == self.cursor {
            return;
        }
        let window = (f64::from(tap.rate()) * GR_HISTORY_SECONDS) as usize;
        let count = (written - self.started_at).min(window.min(tap.capacity() / 2) as u64) as usize;
        if count == 0 {
            return;
        }
        samples.resize(count, 0.0);
        let Some(cursor) = tap.read_latest(samples) else {
            return;
        };
        self.cursor = cursor;
        self.latest = samples.last().copied();
        let columns = Arc::make_mut(&mut self.columns);
        columns.fill(0.0);
        for (index, &db) in samples.iter().enumerate() {
            let age = count - 1 - index;
            let column = gain_trace::POINTS - 1 - age * gain_trace::POINTS / window;
            columns[column] = value_at(columns[column], db);
        }
    }
}

impl PlayheadReading {
    fn fresh(&mut self, cursor: u64, now: f64) -> bool {
        if cursor != self.cursor {
            self.cursor = cursor;
            self.received_at = Some(now);
        }
        self.visible_at = now;
        self.received_at
            .is_some_and(|received| now - received < PLAYHEAD_FRESH_SECONDS)
    }

    fn update(&mut self, cursor: u64, position: f32, now: f64) -> Option<f32> {
        (self.fresh(cursor, now) && (0.0..=1.0).contains(&position)).then_some(position)
    }
}

/// Ballistics of the master, the tracks of the current program and the visible plugin signal taps.
#[derive(Debug, Default)]
pub struct Meters {
    /// The master strip (transport bar, mixer master).
    pub master: MeterState,
    tracks: HashMap<TrackId, MeterState>,
    inputs: HashMap<TrackId, MeterState>,
    named_spectra: HashMap<(DeviceId, String), NamedSpectrum>,
    named_reductions: HashMap<(DeviceId, String), ReductionReading>,
    named_histories: HashMap<(DeviceId, String), SignalHistory>,
    signal_samples: Vec<f32>,
    named_playheads: HashMap<(DeviceId, String), PlayheadReading>,
    /// The transport bar's oscilloscope and spectrum of the master (#34).
    pub master_scope: MasterScope,
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
        self.master_scope.set_tap(engine.telemetry().master_tap());
        self.named_reductions
            .retain(|_, reading| now - reading.visible_at < REDUCTION_VISIBILITY_SECONDS);
        self.named_spectra
            .retain(|_, reading| now - reading.visible_at < 0.5);
        self.named_histories
            .retain(|_, reading| now - reading.visible_at <= gain_trace::STALE);
        self.named_playheads
            .retain(|_, reading| now - reading.visible_at < 2.0 * PLAYHEAD_FRESH_SECONDS);
        let Some(program) = engine.program() else {
            self.tracks.clear();
            self.inputs.clear();
            return;
        };
        self.tracks
            .retain(|id, _| program.tracks.iter().any(|t| t.id == *id));
        self.inputs
            .retain(|id, _| program.tracks.iter().any(|t| t.id == *id));
        for track in &program.tracks {
            let (l, r) = track.meter.take();
            self.tracks.entry(track.id).or_default().tick([l, r], now);
            let (l, r) = track.meter.take_input();
            self.inputs.entry(track.id).or_default().tick([l, r], now);
        }
    }

    pub(crate) fn named_device_levels(
        &mut self,
        device: DeviceId,
        source: &SignalTap,
        now: f64,
    ) -> Option<DeviceLevels> {
        use komponist_ipc::signal_taps::REDUCTION;
        if source.descriptor().kind != REDUCTION {
            return None;
        }
        let key = (device, source.descriptor().key()?.to_owned());
        let samples = source.samples();
        let reading = self
            .named_reductions
            .entry(key)
            .or_insert(ReductionReading {
                cursor: samples.written(),
                reduction_db: None,
                visible_at: now,
            });
        source.want();
        let mut latest = [0.0];
        let sample = samples
            .read_latest(&mut latest)
            .map(|cursor| (cursor, latest[0]));
        let reduction = reading.update(sample, now)?;
        Some(DeviceLevels {
            bars: [0.0, (reduction / GR_RANGE_DB).clamp(0.0, 1.0), 0.0],
            reduction_db: reduction,
        })
    }

    pub(crate) fn named_playhead(
        &mut self,
        device: DeviceId,
        source: &SignalTap,
        now: f64,
    ) -> Option<f32> {
        if source.descriptor().kind != komponist_ipc::signal_taps::PLAYHEAD_POSITION {
            return None;
        }
        let reading = self.playhead_reading(device, source, now)?;
        source.want();
        let mut latest = [-1.0];
        let end = source.samples().read_latest(&mut latest)?;
        reading.update(end, latest[0], now)
    }

    pub(crate) fn named_pad_activity(
        &mut self,
        device: DeviceId,
        source: &SignalTap,
        count: usize,
        now: f64,
    ) -> Option<PadActivity> {
        let reading = self.playhead_reading(device, source, now)?;
        let (cursor, frame) = source.pad_activity(count)?;
        reading.fresh(cursor, now).then_some(frame)
    }

    fn playhead_reading(
        &mut self,
        device: DeviceId,
        source: &SignalTap,
        now: f64,
    ) -> Option<&mut PlayheadReading> {
        let key = (device, source.descriptor().key()?.to_owned());
        // A tap retains its last value when its visibility lease expires.
        Some(self.named_playheads.entry(key).or_insert(PlayheadReading {
            cursor: source.samples().written(),
            received_at: None,
            visible_at: now,
        }))
    }

    pub(crate) fn named_analysis(
        &mut self,
        device: DeviceId,
        source: &SignalTap,
        visible: bool,
        now: f64,
    ) -> Option<Analysis> {
        use komponist_ipc::signal_taps::SPECTRUM_SOURCE;
        if source.descriptor().kind != SPECTRUM_SOURCE {
            return None;
        }
        let key = (device, source.descriptor().key()?.to_owned());
        let rate = source.samples().rate();
        if !visible {
            self.named_spectra.remove(&key);
            return Some(Analysis {
                rate,
                spectrum: Arc::default(),
            });
        }
        source.want();
        let reading = self.named_spectra.entry(key).or_default();
        reading.update(source.samples(), now);
        Some(Analysis {
            rate,
            spectrum: Arc::clone(reading.analyzer.values()),
        })
    }

    pub(crate) fn named_dynamics(
        &mut self,
        device: DeviceId,
        detector: Option<&SignalTap>,
        reduction: Option<&SignalTap>,
        now: f64,
    ) -> Dynamics {
        use crate::gain_trace::{LEVEL_FLOOR_DB, LEVEL_HEIGHT, POINTS};
        use komponist_ipc::signal_taps::{LEVEL_HISTORY, REDUCTION};
        let (level_db, input) = detector
            .filter(|tap| tap.descriptor().kind == LEVEL_HISTORY)
            .and_then(|tap| {
                self.signal_history(device, tap, now, |column, db| {
                    let h = ((db - LEVEL_FLOOR_DB) / -LEVEL_FLOOR_DB).clamp(0.0, 1.0);
                    column.max(h * LEVEL_HEIGHT)
                })
            })
            .map_or_else(
                || (None, Arc::from([0.0; POINTS])),
                |reading| (reading.latest, Arc::clone(&reading.columns)),
            );
        let reduction = reduction
            .filter(|tap| tap.descriptor().kind == REDUCTION)
            .and_then(|tap| {
                self.signal_history(device, tap, now, |column, db| column.min(-db.max(0.0)))
            })
            .map_or_else(
                || Arc::from([0.0; POINTS]),
                |reading| Arc::clone(&reading.columns),
            );
        Dynamics {
            level_db,
            input,
            reduction,
        }
    }

    fn signal_history(
        &mut self,
        device: DeviceId,
        source: &SignalTap,
        now: f64,
        value_at: impl Fn(f32, f32) -> f32,
    ) -> Option<&SignalHistory> {
        let key = (device, source.descriptor().key()?.to_owned());
        source.want();
        let tap = source.samples();
        let reading = self
            .named_histories
            .entry(key)
            .or_insert_with(|| SignalHistory::new(tap, now));
        reading.update(tap, now, &mut self.signal_samples, value_at);
        Some(reading)
    }

    /// The meter of `track`, if it is in the program.
    pub fn track(&self, track: TrackId) -> Option<&MeterState> {
        self.tracks.get(&track)
    }

    /// Input peaks, drained exactly once in update and shared by every view.
    pub fn input(&self, track: TrackId) -> Option<&MeterState> {
        self.inputs.get(&track)
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
            self.inputs.values_mut().for_each(MeterState::reset);
            return;
        }
        match track {
            None => self.master.reset(),
            Some(id) => {
                if let Some(input) = self.inputs.get_mut(&id) {
                    input.reset();
                }
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
            || self.inputs.values().any(MeterState::is_active)
            || self
                .named_reductions
                .values()
                .any(|r| r.reduction_db.is_some_and(|db| db > 0.0))
            || self
                .named_spectra
                .values()
                .any(|reading| reading.analyzer.is_active())
            || self
                .named_histories
                .values()
                .any(|reading| reading.latest.is_some())
            || self.master_scope.is_active()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_history_shares_a_frame_and_holds_its_columns_when_audio_stops() {
        let tap = Arc::new(SampleTap::new(256));
        tap.set_rate(32.0);
        tap.push(&[8.0]);
        let mut history = SignalHistory::new(&tap, 0.0);
        let mut samples = Vec::new();
        history.update(&tap, 0.05, &mut samples, f32::max);
        assert_eq!(history.latest, None);
        tap.push(&[2.0, 3.0]);
        history.update(&tap, 0.1, &mut samples, f32::max);
        assert_eq!(samples, [2.0, 3.0]);
        assert_eq!(history.latest, Some(3.0));
        let columns = Arc::clone(&history.columns);
        history.update(&tap, 0.1, &mut samples, f32::max);
        assert_eq!(history.latest, Some(3.0));
        history.update(&tap, 0.15, &mut samples, f32::max);
        assert_eq!(history.latest, None);
        assert!(Arc::ptr_eq(&history.columns, &columns));
        history.update(&tap, 0.6, &mut samples, f32::max);
        assert!(history.columns.iter().all(|&db| db == 0.0));
    }

    #[test]
    fn named_spectrum_starts_over_after_tap_replacement() {
        let old = Arc::new(SampleTap::new(16_384));
        let new = Arc::new(SampleTap::new(16_384));
        old.set_rate(48_000.0);
        new.set_rate(48_000.0);
        let wave: Vec<_> = (0..crate::spectrum::window_len(48_000.0))
            .map(|n| (std::f32::consts::TAU * 1000.0 * n as f32 / 48_000.0).sin())
            .collect();
        old.push(&wave);
        let mut reading = NamedSpectrum::default();
        reading.update(&old, 0.0);
        old.push(&wave);
        reading.update(&old, 0.05);
        assert!(reading.analyzer.is_active());
        reading.update(&new, 0.1);
        assert!(!reading.analyzer.is_active());
        new.push(&wave);
        reading.update(&new, 0.15);
        assert!(reading.analyzer.is_active());
    }

    #[test]
    fn named_reduction_ignores_retained_data_and_decays_at_the_meter_rate() {
        let mut reading = ReductionReading {
            cursor: 8,
            reduction_db: None,
            visible_at: 0.0,
        };
        assert_eq!(reading.update(Some((8, 9.0)), 0.0), None);
        assert_eq!(reading.update(Some((9, 8.0)), 0.125), Some(8.0));
        assert_eq!(reading.update(Some((9, 8.0)), 0.125), Some(8.0));
        assert_eq!(reading.update(Some((9, 8.0)), 0.25), Some(3.0));
        assert_eq!(reading.update(None, 0.375), Some(0.0));
    }

    #[test]
    fn a_playhead_ignores_retained_and_expired_positions_and_hides_on_idle() {
        let mut reading = PlayheadReading {
            cursor: 12,
            received_at: None,
            visible_at: 0.0,
        };
        assert_eq!(reading.update(12, 0.1, 0.0), None);
        assert_eq!(reading.update(13, 0.2, 0.1), Some(0.2));
        assert_eq!(reading.update(13, 0.2, 0.3), Some(0.2));
        assert_eq!(reading.update(13, 0.2, 0.7), None);
        assert_eq!(reading.update(14, 0.4, 0.8), Some(0.4));
        assert_eq!(reading.update(15, -1.0, 0.9), None);
    }
}
