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
use komponist_model::{DeviceId, TrackId};
use komponist_plugins::signal_taps::SignalTap;
use komponist_ui::widgets::meter::{MeterState, fraction};

use crate::engine_sync::EngineSync;
use crate::gain_trace::{self, Dynamics, GainTrace};
use crate::master_scope::MasterScope;
use crate::spectrum::{Analysis, SpectrumAnalyzer};

/// The largest gain reduction the display scale shows by default (the GR bar is full at this many
/// dB); a device can set its own range ([`DeviceMeter::gr_range_db`], the limiter shows 12 dB).
pub const GR_RANGE_DB: f32 = DeviceMeter::DEFAULT_GR_RANGE_DB;
/// How fast the gain reduction bar falls back, dB per second.
pub const GR_FALL_DB_PER_S: f32 = 40.0;
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
        if self.tap.as_ptr() != Arc::as_ptr(tap) {
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

        if self.tap.as_ptr() != Arc::as_ptr(tap) || now - self.visible_at > gain_trace::STALE {
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

/// Ballistics of the master, the tracks of the current program and the internal devices.
#[derive(Debug, Default)]
pub struct Meters {
    /// The master strip (transport bar, mixer master).
    pub master: MeterState,
    tracks: HashMap<TrackId, MeterState>,
    inputs: HashMap<TrackId, MeterState>,
    devices: HashMap<DeviceId, DeviceBallistics>,
    /// The sample taps of the devices that have one (WP-92) and the analyzers of the ones whose
    /// spectrum is visible right now.
    taps: HashMap<DeviceId, Arc<SampleTap>>,
    spectra: HashMap<DeviceId, SpectrumAnalyzer>,
    named_spectra: HashMap<(DeviceId, String), NamedSpectrum>,
    named_reductions: HashMap<(DeviceId, String), ReductionReading>,
    named_histories: HashMap<(DeviceId, String), SignalHistory>,
    signal_samples: Vec<f32>,
    /// The traces of the compressors whose card is visible right now (WP-93).
    traces: HashMap<DeviceId, GainTrace>,
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
                    self.traces.remove(&id);
                }
            }
            seen.push(id);
        });
        self.devices.retain(|id, _| seen.contains(id));
        self.named_reductions
            .retain(|_, reading| now - reading.visible_at < REDUCTION_VISIBILITY_SECONDS);
        self.taps.retain(|id, _| seen.contains(id));
        self.spectra.retain(|id, _| seen.contains(id));
        self.named_spectra
            .retain(|_, reading| now - reading.visible_at < 0.5);
        self.named_histories
            .retain(|_, reading| now - reading.visible_at <= gain_trace::STALE);
        self.traces.retain(|id, _| seen.contains(id));
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

    /// What the level meter of internal device `device` shows; `None` for a device without one.
    pub fn device(&self, device: DeviceId) -> Option<DeviceLevels> {
        self.devices.get(&device).map(DeviceBallistics::levels)
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
        let ballistics = self.devices.get(&device);
        let range = ballistics.map_or(GR_RANGE_DB, |meter| meter.range_db);
        let mut levels = ballistics.map_or(
            DeviceLevels {
                bars: [0.0; 3],
                reduction_db: 0.0,
            },
            DeviceBallistics::levels,
        );
        levels.reduction_db = reduction;
        levels.bars[1] = (reduction / range).clamp(0.0, 1.0);
        Some(levels)
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

    /// What the card of `device` shows of its compressor's signal (WP-93): the live level point and
    /// the four seconds of reduction history; `None` for a device without a sample tap. `show` says
    /// whether the card is visible now (on screen, running, not bypassed): only then the trace
    /// reads the tap and renews its lease, so a hidden compressor costs nothing; otherwise the
    /// picture is empty and the trace is dropped.
    pub fn dynamics(&mut self, device: DeviceId, show: bool, now: f64) -> Option<Dynamics> {
        let tap = self.taps.get(&device)?;
        if !show {
            self.traces.remove(&device);
            return Some(GainTrace::new().view());
        }
        let trace = self.traces.entry(device).or_default();
        trace.update(now, tap);
        Some(trace.view())
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
            || self.devices.values().any(DeviceBallistics::is_active)
            || self
                .named_reductions
                .values()
                .any(|r| r.reduction_db.is_some_and(|db| db > 0.0))
            || self.spectra.values().any(SpectrumAnalyzer::is_active)
            || self
                .named_spectra
                .values()
                .any(|reading| reading.analyzer.is_active())
            || self
                .named_histories
                .values()
                .any(|reading| reading.latest.is_some())
            || self.traces.values().any(GainTrace::is_active)
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
    #[test]
    fn a_hidden_spectrum_costs_nothing_and_a_visible_one_renews_the_taps_lease() {
        let mut meters = Meters::new();
        let device = DeviceId(1);
        assert!(
            meters.analysis(device, true, 0.0).is_none(),
            "no tap, no analysis"
        );
        let tap = Arc::new(SampleTap::new(16_384));
        tap.set_rate(44_100.0);
        meters.taps.insert(device, Arc::clone(&tap));
        let hidden = meters.analysis(device, false, 0.0).unwrap();
        assert_eq!((hidden.rate, hidden.spectrum.len()), (44_100.0, 0));
        assert!(!tap.is_wanted() && meters.spectra.is_empty());
        let shown = meters.analysis(device, true, 0.1).unwrap();
        assert_eq!(shown.spectrum.len(), crate::spectrum::POINTS);
        assert!(tap.is_wanted());
        // Hiding it again drops the analyzer (the lease runs out by itself).
        meters.analysis(device, false, 0.2);
        assert!(meters.spectra.is_empty());
    }
}
