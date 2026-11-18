//! Compiled automation targets and their bounded playback state.

use std::sync::Arc;

use komponist_model::automation::{
    AutomationMode, AutomationTarget, Point, Shape, value_at_fractional,
};
use komponist_model::params::{ParamKind, Scale, db_to_p, p_to_db, to_normalized, to_plain};
use komponist_model::{SendId, Tick};

use crate::processor::{Event, EventKind, ParameterEditKind};
use crate::program::SlotId;
use crate::sequencer::{EventList, Window};

/// A target resolved against the installed program. This identity crosses the RT queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EngineTarget {
    /// A parameter in a processor slot.
    Slot {
        /// Processor slot.
        slot: SlotId,
        /// Processor parameter id.
        wire_id: u32,
    },
    /// Owner's fader.
    Gain,
    /// Owner's balance.
    Pan,
    /// Level of a send.
    Send(SendId),
    /// Value consumed by the track's macro stage.
    Macro(u8),
}

/// Compact RT-safe conversion from a lane position to a parameter's plain value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlainMap {
    min: f64,
    max: f64,
    scale: Scale,
    steps: u32,
}

impl PlainMap {
    /// A linear range, with zero steps for a continuous value.
    pub fn linear(min: f64, max: f64, steps: u32) -> Self {
        Self {
            min,
            max,
            steps,
            scale: Scale::Lin,
        }
    }

    /// Copy the numeric portion of a parameter's model descriptor.
    pub fn from_kind(kind: &ParamKind) -> Self {
        match *kind {
            ParamKind::Float {
                min,
                max,
                scale,
                steps,
            } => Self {
                min,
                max,
                scale,
                steps,
            },
            ParamKind::Int { min, max } => Self::linear(
                f64::from(min),
                f64::from(max),
                max.saturating_sub(min).max(0) as u32,
            ),
            ParamKind::Enum(ref labels) => {
                let steps = labels.len().saturating_sub(1) as u32;
                Self::linear(0.0, f64::from(steps), steps)
            }
            ParamKind::Bool => Self::linear(0.0, 1.0, 1),
        }
    }

    /// Normalised position of a plain value.
    pub fn normalize(self, value: f64) -> f32 {
        if !value.is_finite() || self.max <= self.min {
            return 0.0;
        }
        to_normalized(
            self.scale,
            self.min,
            self.max,
            value.clamp(self.min, self.max),
        ) as f32
    }

    /// Plain value of a normalised position, snapped to a step where applicable.
    pub fn plain(self, value: f32) -> f64 {
        if !value.is_finite() || self.max <= self.min {
            return self.min;
        }
        let value = to_plain(
            self.scale,
            self.min,
            self.max,
            f64::from(value.clamp(0.0, 1.0)),
        );
        if self.steps == 0 {
            return value;
        }
        let step = (self.max - self.min) / f64::from(self.steps);
        (self.min + ((value - self.min) / step).round() * step).clamp(self.min, self.max)
    }
}

/// An automatable parameter resolved by the compile environment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Parameter {
    /// Processor wire id.
    pub wire_id: u32,
    /// Normalised-to-plain conversion.
    pub map: PlainMap,
    /// Current base value, in the parameter's plain domain.
    pub base: f64,
}

/// One target's immutable playback and record data.
#[derive(Debug, Clone)]
pub struct CompiledTarget {
    /// Resolved target.
    pub target: EngineTarget,
    /// Original project identity. Used by the compiler and UI, not cloned on the audio thread.
    pub model: AutomationTarget,
    /// Conversion for device parameters.
    pub map: PlainMap,
    /// Normalised base value.
    pub base: f32,
    /// Stored per-lane mode.
    pub mode: AutomationMode,
    /// Merged arrangement and clip curve.
    pub points: Arc<[Point]>,
    pub(crate) jumps: Arc<[Tick]>,
    /// Fingerprint of the device descriptor's base value as returned by the compile environment.
    pub(crate) parameter: Option<Parameter>,
}

impl CompiledTarget {
    pub(crate) fn new(
        target: EngineTarget,
        model: AutomationTarget,
        parameter: Option<Parameter>,
        base: f32,
        mode: AutomationMode,
        points: Vec<Point>,
    ) -> Self {
        let mut jumps = points
            .windows(2)
            .filter_map(|pair| {
                let [left, right] = [pair[0], pair[1]];
                (left.tick == right.tick || left.shape == Shape::Hold).then_some(right.tick)
            })
            .collect::<Vec<_>>();
        jumps.dedup();
        Self {
            target,
            model,
            map: parameter.map_or(PlainMap::linear(0.0, 1.0, 0), |p| p.map),
            base,
            mode,
            points: points.into(),
            jumps: jumps.into(),
            parameter,
        }
    }

    pub(crate) fn value(&self, tick: f64) -> f32 {
        value_at_fractional(&self.points, tick).unwrap_or(self.base)
    }

    fn changed_since(&self, from: f64, reference: f32, to: f64) -> bool {
        if to <= from {
            return false;
        }
        if (self.value(to) - reference).abs() > VALUE_EPSILON {
            return true;
        }
        let first = self
            .points
            .partition_point(|point| point.tick.0 as f64 <= from);
        let last = self
            .points
            .partition_point(|point| point.tick.0 as f64 <= to);
        self.points[first..last]
            .iter()
            .any(|point| (point.value - reference).abs() > VALUE_EPSILON)
    }

    pub(crate) fn reads(&self) -> bool {
        self.mode != AutomationMode::Off && !self.points.is_empty()
    }
}

/// Transport change that ended an automation write segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationBoundary {
    /// Transport stopped.
    Stop,
    /// Playhead moved to a new point.
    Locate,
    /// Playhead reached the loop end.
    LoopWrap,
}

/// One plugin GUI edit with its absolute song position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PluginParam {
    /// Position according to the engine clock.
    pub tick: Tick,
    /// Wire id and edit data.
    pub kind: ParameterEditKind,
}

/// A small inline batch of plugin GUI edits to keep the engine event ring bounded.
#[derive(Debug, Clone, Copy)]
pub struct PluginParamBatch {
    events: [PluginParam; 8],
    len: usize,
}

impl PluginParamBatch {
    pub(crate) fn new() -> Self {
        Self {
            events: [PluginParam {
                tick: Tick::ZERO,
                kind: ParameterEditKind::Gesture {
                    id: 0,
                    begin: false,
                },
            }; 8],
            len: 0,
        }
    }

    /// Changes in plugin output order.
    pub fn as_slice(&self) -> &[PluginParam] {
        &self.events[..self.len]
    }

    /// Appends an event and reports when the batch is full.
    pub(crate) fn push(&mut self, event: PluginParam) -> bool {
        self.events[self.len] = event;
        self.len += 1;
        self.len == self.events.len()
    }
}

pub(crate) fn fader_to_normalized(gain: f32) -> f32 {
    1.0 - db_to_p(20.0 * gain.log10())
}

pub(crate) fn normalized_to_gain(value: f32) -> f32 {
    let db = p_to_db(1.0 - value);
    10.0_f32.powf(db / 20.0)
}

pub(crate) const MAX_TARGETS: usize = crate::MAX_EVENTS_PER_BLOCK;
const TOUCH_RETURN_SECONDS: f32 = 0.25;
const IMPLICIT_TOUCH_SECONDS: f32 = 0.5;
const VALUE_EPSILON: f32 = 1e-5;

#[derive(Default, Clone, Copy, Debug)]
enum Gesture {
    #[default]
    Idle,
    Held {
        writes: bool,
        implicit_end: Option<u64>,
    },
    Latch,
    ReadOverride {
        reference: f32,
        at: f64,
    },
    Return {
        from: f32,
        start: u64,
    },
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Cursor {
    last: f32,
    chase: bool,
    gesture: Gesture,
    manual: f32,
    pub(crate) pending_value: Option<(Tick, f32)>,
}

impl Cursor {
    fn new() -> Self {
        Self {
            last: f32::NAN,
            chase: true,
            gesture: Gesture::Idle,
            manual: f32::NAN,
            pending_value: None,
        }
    }

    pub(crate) fn chase(&mut self) {
        self.chase = true;
        if let Gesture::ReadOverride { .. } | Gesture::Return { .. } = self.gesture {
            self.gesture = Gesture::Idle;
        }
    }

    pub(crate) fn begin(
        &mut self,
        mode: AutomationMode,
        playing: bool,
        implicit_end: Option<u64>,
    ) -> bool {
        let writes = playing && matches!(mode, AutomationMode::Touch | AutomationMode::Latch);
        let notify = writes
            && !matches!(
                self.gesture,
                Gesture::Held { writes: true, .. } | Gesture::Latch
            );
        self.gesture = Gesture::Held {
            writes,
            implicit_end,
        };
        notify
    }

    pub(crate) fn implicit_touch(
        &mut self,
        mode: AutomationMode,
        playing: bool,
        time: u64,
        rate: f32,
    ) -> bool {
        if matches!(
            self.gesture,
            Gesture::Held {
                implicit_end: None,
                ..
            }
        ) {
            return false; // This parameter is already in an explicit gesture.
        }
        self.begin(
            mode,
            playing,
            Some(time + (rate * IMPLICIT_TOUCH_SECONDS) as u64),
        )
    }

    pub(crate) fn implicit_end(&self) -> Option<u64> {
        match self.gesture {
            Gesture::Held { implicit_end, .. } => implicit_end,
            _ => None,
        }
    }

    pub(crate) fn end(&mut self, lane: &CompiledTarget, tick: f64, time: u64) -> bool {
        let Gesture::Held { writes, .. } = self.gesture else {
            return false;
        };
        self.gesture = match (lane.mode, writes, self.manual.is_finite()) {
            (AutomationMode::Latch, true, _) => Gesture::Latch,
            (AutomationMode::Touch, true, true) => {
                self.chase = true;
                Gesture::Return {
                    from: self.manual,
                    start: time,
                }
            }
            (_, _, true) if lane.reads() => Gesture::ReadOverride {
                reference: lane.value(tick),
                at: tick,
            },
            _ => Gesture::Idle,
        };
        writes
    }

    pub(crate) fn writes(&self) -> bool {
        matches!(
            self.gesture,
            Gesture::Held { writes: true, .. } | Gesture::Latch
        )
    }

    pub(crate) fn manual(&self) -> Option<f32> {
        self.manual.is_finite().then_some(self.manual)
    }

    pub(crate) fn is_held(&self) -> bool {
        matches!(self.gesture, Gesture::Held { .. } | Gesture::Latch)
    }

    pub(crate) fn reassert(&mut self) {
        self.last = f32::NAN;
        self.chase = true;
    }

    pub(crate) fn manual_value(&mut self, tick: Tick, value: f32) {
        self.manual = value;
        self.last = value;
        if matches!(
            self.gesture,
            Gesture::Held { writes: true, .. } | Gesture::Latch
        ) {
            self.pending_value = Some((tick, value));
        }
    }

    pub(crate) fn interrupt(&mut self, keep_touch: bool, mode: AutomationMode) -> bool {
        let restarted = keep_touch
            && mode == AutomationMode::Touch
            && matches!(self.gesture, Gesture::Held { writes: true, .. });
        if !restarted {
            self.gesture = match self.gesture {
                Gesture::Held { implicit_end, .. } if keep_touch => Gesture::Held {
                    writes: false,
                    implicit_end,
                },
                _ => Gesture::Idle,
            };
        }
        self.chase();
        restarted
    }

    fn return_end(&self, rate: f64) -> Option<u64> {
        match self.gesture {
            Gesture::Return { start, .. } => Some(start + return_frames(rate)),
            _ => None,
        }
    }

    pub(crate) fn mixer_value(
        &self,
        lane: &CompiledTarget,
        window: Window<'_>,
        sample: i64,
        time: u64,
    ) -> Option<f32> {
        if lane.mode == AutomationMode::Off {
            return None;
        }
        if let Gesture::Held { .. } | Gesture::Latch = self.gesture {
            return self.manual.is_finite().then_some(self.manual);
        }
        if !lane.reads() && !matches!(self.gesture, Gesture::Return { .. }) {
            return None;
        }
        let tick = tick_at(window, sample);
        Some(match self.gesture {
            Gesture::ReadOverride { reference, at } if !lane.changed_since(at, reference, tick) => {
                self.manual
            }
            Gesture::Return { from, start } => {
                let frames = return_frames(window.rate);
                let progress = time.saturating_sub(start) as f32 / frames.max(1) as f32;
                if progress < 1.0 {
                    let end = sample
                        .saturating_add(frames.saturating_sub(time.saturating_sub(start)) as i64);
                    from + (lane.value(tick_at(window, end)) - from) * progress
                } else {
                    lane.value(tick)
                }
            }
            _ => lane.value(tick),
        })
    }
}

#[derive(Clone, Copy)]
struct SlotEvent {
    slot: SlotId,
    event: Event,
    important: bool,
    sequence: u32,
}

/// All per-owner runtime data, allocated when constructing the engine.
pub(crate) struct State {
    cursors: Vec<Cursor>,
    reuse: Vec<Cursor>,
    params: Vec<SlotEvent>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            cursors: Vec::with_capacity(MAX_TARGETS),
            reuse: Vec::with_capacity(MAX_TARGETS),
            params: Vec::with_capacity(crate::MAX_EVENTS_PER_BLOCK),
        }
    }

    pub(crate) fn install(&mut self, old: &[CompiledTarget], new: &[CompiledTarget]) {
        self.reuse.clear();
        for lane in new.iter().take(MAX_TARGETS) {
            let state = index(old, lane.target).and_then(|i| {
                let prev = &old[i];
                let mut cursor = *self.cursors.get(i)?;
                if prev.model != lane.model {
                    return None;
                }
                if !Arc::ptr_eq(&prev.points, &lane.points)
                    || prev.base != lane.base
                    || prev.map != lane.map
                {
                    cursor.chase = true;
                }
                if prev.mode != lane.mode {
                    cursor = Cursor::new();
                }
                Some(cursor)
            });
            self.reuse.push(state.unwrap_or_else(Cursor::new));
        }
        std::mem::swap(&mut self.cursors, &mut self.reuse);
    }

    pub(crate) fn cursor(
        &mut self,
        lanes: &[CompiledTarget],
        target: EngineTarget,
    ) -> Option<(&mut Cursor, usize)> {
        let i = index(lanes, target)?;
        Some((self.cursors.get_mut(i)?, i))
    }

    pub(crate) fn cursors(&mut self) -> &mut [Cursor] {
        &mut self.cursors
    }

    pub(crate) fn mixer_lane<'a>(
        &'a self,
        lanes: &'a [CompiledTarget],
        target: EngineTarget,
    ) -> Option<(&'a CompiledTarget, &'a Cursor)> {
        let i = index(lanes, target)?;
        Some((&lanes[i], self.cursors.get(i)?))
    }

    pub(crate) fn chase(&mut self) {
        for cursor in &mut self.cursors {
            cursor.chase();
        }
    }

    pub(crate) fn chase_slot(&mut self, lanes: &[CompiledTarget], slot: SlotId) {
        for (lane, cursor) in lanes.iter().zip(&mut self.cursors) {
            if matches!(lane.target, EngineTarget::Slot { slot: s, .. } if s == slot) {
                cursor.last = f32::NAN;
                cursor.chase();
            }
        }
    }

    pub(crate) fn prepare(
        &mut self,
        lanes: &[CompiledTarget],
        window: Window<'_>,
        time: u64,
        mut running: impl FnMut(SlotId) -> bool,
    ) -> u32 {
        self.params.clear();
        let mut dropped = 0;
        let mut sequence = 0u32;
        for (lane, cursor) in lanes.iter().zip(&mut self.cursors) {
            let EngineTarget::Slot { slot, wire_id } = lane.target else {
                continue;
            };
            if !running(slot) {
                cursor.last = f32::NAN;
                cursor.chase = true;
                continue;
            }
            let mut last_emitted = None;
            let mut emit = |last: &mut f32, sample: i64, value: f32, important: bool| {
                if (*last - value).abs() <= VALUE_EPSILON {
                    return;
                }
                let plain = lane.map.plain(value);
                if lane.map.steps != 0 && last.is_finite() && lane.map.plain(*last) == plain {
                    return;
                }
                *last = value;
                let event = SlotEvent {
                    slot,
                    event: Event {
                        offset: (sample - window.start) as u32,
                        kind: EventKind::Param {
                            id: wire_id,
                            value: plain,
                        },
                    },
                    important,
                    sequence,
                };
                sequence += 1;
                last_emitted = Some(event);
                dropped += push_event(&mut self.params, event);
            };
            if lane.mode == AutomationMode::Off {
                if std::mem::take(&mut cursor.chase) {
                    emit(&mut cursor.last, window.start, lane.base, true);
                }
                continue;
            }
            if (lane.points.is_empty() && !matches!(cursor.gesture, Gesture::Return { .. }))
                || matches!(cursor.gesture, Gesture::Held { .. } | Gesture::Latch)
            {
                continue;
            }
            let chase = std::mem::take(&mut cursor.chase);
            // Copy gesture state so the event sink can update the last emitted value independently.
            let view = Cursor {
                gesture: cursor.gesture,
                manual: cursor.manual,
                ..Cursor::new()
            };
            if chase && let Some(value) = view.mixer_value(lane, window, window.start, time) {
                let value = if matches!(view.gesture, Gesture::Idle) && !window.playing {
                    let next = lane.jumps.partition_point(|&tick| {
                        window.map.tick_to_sample(tick, window.rate) <= window.start
                    });
                    if let Some(tick) = next.checked_sub(1).and_then(|i| lane.jumps.get(i))
                        && window.map.tick_to_sample(*tick, window.rate) == window.start
                    {
                        lane.value(tick.0 as f64)
                    } else {
                        value
                    }
                } else {
                    value
                };
                emit(&mut cursor.last, window.start, value, true);
            }
            if window.playing {
                let step = ramp_step(window.rate);
                let end = window.start + i64::from(window.frames);
                let mut grid = window.start + (-window.start).rem_euclid(step);
                let next_jump = lane.jumps.partition_point(|&tick| {
                    window.map.tick_to_sample(tick, window.rate) < window.start
                });
                let mut jumps = lane.jumps[next_jump..].iter().peekable();
                let return_end = cursor.return_end(window.rate);
                let mut return_at = return_end.map_or(end, |at| {
                    window.start.saturating_add(at.saturating_sub(time) as i64)
                });
                while grid < end || jumps.peek().is_some() || return_at < end {
                    let jump = jumps
                        .peek()
                        .map_or(end, |&&tick| window.map.tick_to_sample(tick, window.rate));
                    let sample = grid.min(jump).min(return_at);
                    if sample >= end {
                        break;
                    }
                    let elapsed = time + (sample - window.start) as u64;
                    if jump == sample {
                        let tick = jumps.next().expect("the jump sample was in the segment");
                        let value = if matches!(view.gesture, Gesture::Idle) {
                            lane.value(tick.0 as f64)
                        } else {
                            view.mixer_value(lane, window, sample, elapsed)
                                .unwrap_or(lane.base)
                        };
                        emit(&mut cursor.last, sample, value, true);
                    } else if let Some(value) = view.mixer_value(lane, window, sample, elapsed) {
                        emit(&mut cursor.last, sample, value, sample == return_at);
                    }
                    if return_at == sample {
                        return_at = end;
                    }
                    if grid == sample {
                        grid += step;
                    }
                }
                if return_end.is_some_and(|at| at < time + u64::from(window.frames)) {
                    cursor.gesture = Gesture::Idle;
                }
            }
            // Reserve every target's final emitted value even if some intermediate steps were lost.
            if let Some(last) = last_emitted {
                if let Some(event) = self
                    .params
                    .iter_mut()
                    .rev()
                    .find(|event| event.sequence == last.sequence)
                {
                    event.important = true;
                } else {
                    dropped += push_event(
                        &mut self.params,
                        SlotEvent {
                            important: true,
                            ..last
                        },
                    );
                }
            }
            if let Gesture::ReadOverride { reference, at } = cursor.gesture
                && window.playing
                && lane.changed_since(
                    at,
                    reference,
                    tick_at(window, window.start + i64::from(window.frames - 1)),
                )
            {
                cursor.gesture = Gesture::Idle;
            }
        }
        self.params
            .sort_unstable_by_key(|entry| (entry.slot, entry.event.offset, entry.sequence));
        dropped
    }

    fn slot_events(&self, slot: SlotId) -> &[SlotEvent] {
        let start = self.params.partition_point(|event| event.slot < slot);
        let end = start + self.params[start..].partition_point(|event| event.slot == slot);
        &self.params[start..end]
    }

    pub(crate) fn events(&self, slot: SlotId) -> impl Iterator<Item = Event> + '_ {
        self.slot_events(slot).iter().map(|entry| entry.event)
    }

    pub(crate) fn add_events(&self, slot: SlotId, out: &mut EventList) -> u32 {
        let events = self.slot_events(slot);
        let available = out.room();
        let skip = events.len().saturating_sub(available);
        // Prefer jumps and each target's final value, retaining the existing note-offs.
        let mut skip_ramps = skip;
        for event in events {
            if skip_ramps > 0 && !event.important {
                skip_ramps -= 1;
            } else {
                out.push(event.event.offset, event.event.kind);
            }
        }
        skip as u32
    }
}

fn push_event(events: &mut Vec<SlotEvent>, event: SlotEvent) -> u32 {
    if events.len() < events.capacity() {
        events.push(event);
        return 0;
    }
    if event.important
        && let Some(index) = events.iter().rposition(|event| !event.important)
    {
        events[index] = event;
    }
    1
}

fn index(lanes: &[CompiledTarget], target: EngineTarget) -> Option<usize> {
    lanes.binary_search_by_key(&target, |lane| lane.target).ok()
}

pub(crate) fn tick_at(window: Window<'_>, sample: i64) -> f64 {
    window.map.samples_to_ticks(sample as f64, window.rate)
}

fn return_frames(rate: f64) -> u64 {
    (rate * f64::from(TOUCH_RETURN_SECONDS)).round().max(1.0) as u64
}

pub(crate) fn ramp_step(rate: f64) -> i64 {
    (rate / 750.0).round().max(1.0) as i64
}

/// Position and elapsed time at a mixer tap. The tap hears earlier input by the
/// accumulated latency, including the end of a loop as those samples drain after wrap.
#[derive(Clone, Copy)]
pub(crate) struct MixerClock<'a> {
    pub(crate) window: Window<'a>,
    pub(crate) time: u64,
    pub(crate) delay: u32,
    pub(crate) wrapped_loop: Option<(i64, i64)>,
}

impl MixerClock<'_> {
    pub(crate) fn delayed(self, frames: u32) -> Self {
        Self {
            delay: self.delay.saturating_add(frames),
            ..self
        }
    }

    fn sample(&self, offset: u32) -> i64 {
        if !self.window.playing {
            return self.window.start;
        }
        let source = self.window.start + i64::from(offset) - i64::from(self.delay);
        if let Some((lo, hi)) = self.wrapped_loop
            && source < lo
        {
            return lo + (source - lo).rem_euclid(hi - lo);
        }
        source
    }
}

/// Deterministic per-sample interpolation between values on the absolute ramp grid.
pub(crate) struct MixerValues<'a> {
    lane: &'a CompiledTarget,
    cursor: &'a Cursor,
    clock: MixerClock<'a>,
    step: i64,
    grid: i64,
    offset: u32,
    left: f32,
    right: f32,
}

impl<'a> MixerValues<'a> {
    pub(crate) fn new(
        state: &'a State,
        lanes: &'a [CompiledTarget],
        target: EngineTarget,
        clock: MixerClock<'a>,
    ) -> Option<Self> {
        let (lane, cursor) = state.mixer_lane(lanes, target)?;
        if lane.mode == AutomationMode::Off || (!lane.reads() && cursor.manual().is_none()) {
            return None;
        }
        Some(Self {
            lane,
            cursor,
            clock,
            step: ramp_step(clock.window.rate),
            grid: i64::MIN,
            offset: 0,
            left: 0.0,
            right: 0.0,
        })
    }

    fn scale(&self, value: f32) -> f32 {
        match self.lane.target {
            EngineTarget::Gain | EngineTarget::Send(_) => normalized_to_gain(value),
            EngineTarget::Pan => value.mul_add(2.0, -1.0),
            _ => value,
        }
    }

    pub(crate) fn value(&mut self, fallback: f32) -> f32 {
        let sample = self.clock.sample(self.offset);
        let time = self.clock.time + u64::from(self.offset);
        self.offset += 1;
        if !self.clock.window.playing {
            return self
                .cursor
                .mixer_value(self.lane, self.clock.window, sample, time)
                .map_or(fallback, |v| self.scale(v));
        }
        let grid = sample - sample.rem_euclid(self.step);
        if grid != self.grid {
            self.grid = grid;
            let back = (sample - grid) as u64;
            let value_at = |sample, time| {
                self.cursor
                    .mixer_value(self.lane, self.clock.window, sample, time)
                    .map_or(fallback, |v| self.scale(v))
            };
            let left = value_at(grid, time.saturating_sub(back));
            self.right = value_at(grid + self.step, time + self.step as u64 - back);
            self.left = left;
        }
        self.left + (self.right - self.left) * (sample - grid) as f32 / self.step as f32
    }
}
