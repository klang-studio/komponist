//! Compiled automation targets and their bounded playback state.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use komponist_model::automation::{
    AutomationMode, AutomationTarget, Point, Shape, TrackAutomation, value_at_fractional,
};
use komponist_model::params::{ParamKind, Scale, db_to_p, p_to_db, to_normalized, to_plain};
use komponist_model::{SendId, Tick};

use crate::processor::{Event, EventKind, ParameterEditKind};
use crate::program::{ChainSlot, SlotId};
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
    applied_override: Arc<AtomicU32>,
    value_ranges: Arc<[(f32, f32)]>,
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
        let value_ranges = value_ranges(&points);
        Self {
            target,
            model,
            map: parameter.map_or(PlainMap::linear(0.0, 1.0, 0), |p| p.map),
            base,
            mode,
            points: points.into(),
            jumps: jumps.into(),
            applied_override: Arc::new(AtomicU32::new(f32::NAN.to_bits())),
            value_ranges,
            parameter,
        }
    }

    pub(crate) fn applied_override(&self) -> Option<f32> {
        let value = f32::from_bits(self.applied_override.load(Ordering::Relaxed));
        value.is_finite().then_some(value)
    }

    pub(crate) fn publish_override(&self, value: Option<f32>) {
        self.applied_override
            .store(value.unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn value(&self, tick: f64) -> f32 {
        value_at_fractional(&self.points, tick).unwrap_or(self.base)
    }

    /// The value just before a point, using the incoming segment.
    fn point_left(&self, tick: Tick) -> f32 {
        let first = self.points.partition_point(|point| point.tick < tick);
        if let Some(previous) = first.checked_sub(1).and_then(|i| self.points.get(i))
            && previous.shape == Shape::Hold
        {
            return previous.value;
        }
        self.points
            .get(first)
            .map_or(self.base, |point| point.value)
    }

    fn read_override(&self, tick: f64) -> ReadOverride {
        let reference = self.value(tick);
        let first = self
            .points
            .partition_point(|point| point.tick.0 as f64 <= tick);
        let next = self.next_change(first, reference);
        ReadOverride {
            reference,
            at: tick,
            next: next.map_or(f64::INFINITY, |tick| tick.0 as f64),
        }
    }

    fn next_change(&self, start: usize, reference: f32) -> Option<Tick> {
        let find = |start, end| {
            (start..end)
                .find(|&i| {
                    reachable_point(&self.points, i) && outside(self.points[i].value, reference)
                })
                .map(|i| self.points[i].tick)
        };
        let first_block = start.div_ceil(VALUE_RANGE_BLOCK);
        let first_end = (first_block * VALUE_RANGE_BLOCK).min(self.points.len());
        find(start, first_end).or_else(|| {
            let leaves = self.value_ranges.len() / 2;
            if leaves == 0 {
                return find(first_end, self.points.len());
            }
            let block = find_value_block(&self.value_ranges, 1, 0, leaves, first_block, reference)?;
            let start = block * VALUE_RANGE_BLOCK;
            find(start, (start + VALUE_RANGE_BLOCK).min(self.points.len()))
        })
    }

    pub(crate) fn reads(&self) -> bool {
        self.mode != AutomationMode::Off && !self.points.is_empty()
    }
}

/// A duplicate tick has separate incoming and outgoing values. Its incoming value is
/// unreachable when the preceding segment holds a different value up to the jump.
fn reachable_point(points: &[Point], i: usize) -> bool {
    !(i > 0
        && points
            .get(i + 1)
            .is_some_and(|next| next.tick == points[i].tick)
        && points[i - 1].shape == Shape::Hold)
}

/// Off-thread min/max index. Each leaf summarizes a short run of points so a release
/// on a dense flat curve can find its first meaningful change without scanning the curve.
const VALUE_RANGE_BLOCK: usize = 32;

fn value_ranges(points: &[Point]) -> Arc<[(f32, f32)]> {
    let blocks = points.len().div_ceil(VALUE_RANGE_BLOCK);
    if blocks <= 1 {
        return Arc::from([]);
    }
    let leaves = blocks.next_power_of_two();
    let mut tree = vec![(f32::INFINITY, f32::NEG_INFINITY); 2 * leaves];
    for (i, chunk) in points.chunks(VALUE_RANGE_BLOCK).enumerate() {
        for (j, point) in chunk.iter().enumerate() {
            if !reachable_point(points, i * VALUE_RANGE_BLOCK + j) {
                continue;
            }
            let (min, max) = &mut tree[leaves + i];
            *min = min.min(point.value);
            *max = max.max(point.value);
        }
    }
    for i in (1..leaves).rev() {
        let (l, r) = (tree[i * 2], tree[i * 2 + 1]);
        tree[i] = (l.0.min(r.0), l.1.max(r.1));
    }
    tree.into()
}

fn outside(value: f32, reference: f32) -> bool {
    (value - reference).abs() > VALUE_EPSILON
}

fn find_value_block(
    tree: &[(f32, f32)],
    node: usize,
    lo: usize,
    hi: usize,
    start: usize,
    reference: f32,
) -> Option<usize> {
    let (min, max) = tree[node];
    if hi <= start || min > max || (!outside(min, reference) && !outside(max, reference)) {
        return None;
    }
    if hi - lo == 1 {
        return Some(lo);
    }
    let mid = lo + (hi - lo) / 2;
    find_value_block(tree, node * 2, lo, mid, start, reference)
        .or_else(|| find_value_block(tree, node * 2 + 1, mid, hi, start, reference))
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
    ReadOverride(ReadOverride),
    Return {
        from: f32,
        start: u64,
    },
}

#[derive(Clone, Copy, Debug)]
struct ReadOverride {
    reference: f32,
    at: f64,
    next: f64,
}

impl ReadOverride {
    fn expired(self, lane: &CompiledTarget, tick: f64) -> bool {
        tick >= self.next
            || (tick > self.at
                && tick < self.next
                && self.next.is_finite()
                && outside(lane.value(tick), self.reference))
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Cursor {
    last: f32,
    chase: bool,
    restore_base: bool,
    gesture: Gesture,
    /// A newly held control keeps following the previous state until the first edit.
    before_edit: Option<Gesture>,
    manual: f32,
    pub(crate) pending_value: Option<(Tick, f32)>,
}

impl Cursor {
    fn new() -> Self {
        Self {
            last: f32::NAN,
            chase: true,
            restore_base: false,
            gesture: Gesture::Idle,
            before_edit: None,
            manual: f32::NAN,
            pending_value: None,
        }
    }

    pub(crate) fn chase(&mut self) {
        self.chase = true;
        if matches!(
            self.playback_gesture(),
            Gesture::ReadOverride(_) | Gesture::Return { .. }
        ) {
            self.clear_playback_override();
        }
    }

    fn playback_gesture(&self) -> Gesture {
        self.before_edit.unwrap_or(self.gesture)
    }

    fn clear_playback_override(&mut self) {
        if self.before_edit.is_some() {
            self.before_edit = Some(Gesture::Idle);
        } else {
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
        if !matches!(self.gesture, Gesture::Held { .. }) {
            self.before_edit = Some(self.gesture);
        }
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
        if let Some(previous) = self.before_edit.take() {
            self.gesture = previous;
            return writes;
        }
        self.gesture = match (lane.mode, writes, self.manual.is_finite()) {
            (AutomationMode::Latch, true, _) => Gesture::Latch,
            (AutomationMode::Touch, true, true) => {
                self.chase = true;
                Gesture::Return {
                    from: self.manual,
                    start: time,
                }
            }
            (_, _, true) if lane.reads() => Gesture::ReadOverride(lane.read_override(tick)),
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
        (self
            .before_edit
            .is_none_or(|previous| matches!(previous, Gesture::Latch))
            && self.manual.is_finite())
        .then_some(self.manual)
    }

    pub(crate) fn is_held(&self) -> bool {
        matches!(self.gesture, Gesture::Held { .. } | Gesture::Latch)
    }

    pub(crate) fn reassert(&mut self) {
        self.last = f32::NAN;
        self.chase = true;
    }

    pub(crate) fn manual_value(&mut self, tick: Tick, value: f32) {
        self.before_edit = None;
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
            self.before_edit = None;
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
        match self.playback_gesture() {
            Gesture::Return { start, .. } => Some(start + return_frames(rate)),
            _ => None,
        }
    }

    pub(crate) fn override_value(
        &self,
        lane: &CompiledTarget,
        window: Window<'_>,
        time: u64,
    ) -> Option<f32> {
        let offset = if window.playing {
            i64::from(window.frames - 1)
        } else {
            0
        };
        let sample = window.start + offset;
        match self.playback_gesture() {
            Gesture::Idle => return None,
            Gesture::Held { .. } | Gesture::Latch if !self.manual.is_finite() => return None,
            Gesture::ReadOverride(state) if state.expired(lane, tick_at(window, sample)) => {
                return None;
            }
            Gesture::Return { .. }
                if self
                    .return_end(window.rate)
                    .is_some_and(|end| end <= time + offset as u64) =>
            {
                return None;
            }
            _ => {}
        }
        if matches!(lane.target, EngineTarget::Slot { .. }) {
            return self.last.is_finite().then_some(self.last);
        }
        self.mixer_value(lane, window, sample, time + offset as u64)
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
        let gesture = self.playback_gesture();
        if let Gesture::Held { .. } | Gesture::Latch = gesture {
            return self.manual.is_finite().then_some(self.manual);
        }
        if !lane.reads() && !matches!(gesture, Gesture::Return { .. }) {
            return None;
        }
        let tick = tick_at(window, sample);
        Some(match gesture {
            Gesture::ReadOverride(state) if !state.expired(lane, tick) => self.manual,
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
    delivery: Delivery,
}

#[derive(Clone, Copy)]
enum Delivery {
    Lane {
        index: usize,
        value: f32,
        last: bool,
    },
    Restore(usize),
}

struct BaseRestore {
    target: EngineTarget,
    value: f64,
    delivered: bool,
}

fn delivered(event: SlotEvent, cursors: &mut [Cursor], restores: &mut [BaseRestore]) {
    match event.delivery {
        Delivery::Lane { index, value, last } => {
            let cursor = &mut cursors[index];
            cursor.last = value;
            if last {
                cursor.chase = false;
                cursor.restore_base = false;
            }
        }
        Delivery::Restore(index) => restores[index].delivered = true,
    }
}

/// All per-owner runtime data, allocated when constructing the engine.
pub(crate) struct State {
    cursors: Vec<Cursor>,
    reuse: Vec<Cursor>,
    params: Vec<SlotEvent>,
    restores: Vec<BaseRestore>,
}

impl State {
    pub(crate) fn new() -> Self {
        Self {
            cursors: Vec::with_capacity(MAX_TARGETS),
            reuse: Vec::with_capacity(MAX_TARGETS),
            params: Vec::with_capacity(crate::MAX_EVENTS_PER_BLOCK),
            restores: Vec::with_capacity(MAX_TARGETS),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.cursors.clear();
        self.restores.clear();
    }

    pub(crate) fn install(
        &mut self,
        old: &[CompiledTarget],
        new: &[CompiledTarget],
        chain: &[ChainSlot],
        source: &TrackAutomation,
    ) {
        let ongoing = |target| {
            matches!(target, EngineTarget::Slot { slot, .. }
            if chain.iter().any(|device| device.slot == slot))
        };
        self.restores.retain(|restore| {
            !restore.delivered && ongoing(restore.target) && index(new, restore.target).is_none()
        });
        for (lane, cursor) in old.iter().zip(&self.cursors) {
            let missing = index(new, lane.target).is_none();
            if missing
                && source.lane(&lane.model).is_none()
                && ongoing(lane.target)
                && (lane.reads()
                    || cursor.restore_base
                    || (cursor.writes() && cursor.manual().is_some()))
                && let Some(parameter) = lane.parameter
                && self.restores.len() < self.restores.capacity()
            {
                self.restores.push(BaseRestore {
                    target: lane.target,
                    value: parameter.base,
                    delivered: false,
                });
            }
        }
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
                    if matches!(cursor.playback_gesture(), Gesture::ReadOverride(_)) {
                        cursor.clear_playback_override();
                    }
                }
                if prev.mode != lane.mode {
                    cursor = Cursor::new();
                    cursor.restore_base =
                        lane.mode == AutomationMode::Off || (prev.reads() && !lane.reads());
                } else if lane.mode == AutomationMode::Off && prev.base != lane.base
                    || prev.reads() && !lane.reads()
                {
                    cursor.restore_base = true;
                }
                Some(cursor)
            });
            self.reuse.push(state.unwrap_or_else(|| Cursor {
                restore_base: lane.mode == AutomationMode::Off,
                ..Cursor::new()
            }));
        }
        std::mem::swap(&mut self.cursors, &mut self.reuse);
    }

    /// A newer manual edit supersedes a pending one-shot base restore.
    pub(crate) fn cancel_restore(&mut self, lanes: &[CompiledTarget], target: EngineTarget) {
        if let Some((cursor, _)) = self.cursor(lanes, target) {
            cursor.restore_base = false;
        }
        self.restores.retain(|restore| restore.target != target);
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
        self.restores.retain(|restore| !restore.delivered);
        for (index, restore) in self.restores.iter().enumerate() {
            if let EngineTarget::Slot { slot, wire_id } = restore.target
                && running(slot)
            {
                dropped += push_event(
                    &mut self.params,
                    SlotEvent {
                        slot,
                        event: Event {
                            offset: 0,
                            kind: EventKind::Param {
                                id: wire_id,
                                value: restore.value,
                            },
                        },
                        important: true,
                        sequence,
                        delivery: Delivery::Restore(index),
                    },
                );
                sequence += 1;
            }
        }
        for (index, (lane, cursor)) in lanes.iter().zip(&mut self.cursors).enumerate() {
            let EngineTarget::Slot { slot, wire_id } = lane.target else {
                continue;
            };
            if !running(slot) {
                cursor.last = f32::NAN;
                cursor.chase = true;
                continue;
            }
            let mut last_emitted = None;
            // This probe advances only while staging. The committed cursor advances when
            // the processor's list (or a MIDI stage's setter) accepts the event.
            let mut last = cursor.last;
            let mut emit = |sample: i64, value: f32, important: bool| {
                if lane.map.steps == 0 && !outside(last, value) && last.is_finite() {
                    return;
                }
                let plain = lane.map.plain(value);
                if lane.map.steps != 0 && last.is_finite() && lane.map.plain(last) == plain {
                    return;
                }
                last = value;
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
                    delivery: Delivery::Lane {
                        index,
                        value,
                        last: false,
                    },
                };
                sequence += 1;
                last_emitted = Some(event);
                dropped += push_event(&mut self.params, event);
            };
            if cursor.restore_base && !lane.reads() {
                let value = lane
                    .parameter
                    .map_or_else(|| lane.map.plain(lane.base), |p| p.base);
                dropped += push_event(
                    &mut self.params,
                    SlotEvent {
                        slot,
                        event: Event {
                            offset: 0,
                            kind: EventKind::Param { id: wire_id, value },
                        },
                        important: true,
                        sequence,
                        delivery: Delivery::Lane {
                            index,
                            value: lane.base,
                            last: true,
                        },
                    },
                );
                sequence += 1;
                continue;
            }
            let gesture = cursor.playback_gesture();
            if lane.mode == AutomationMode::Off
                || (lane.points.is_empty() && !matches!(gesture, Gesture::Return { .. }))
                || matches!(gesture, Gesture::Held { .. } | Gesture::Latch)
            {
                continue;
            }
            let chase = cursor.chase;
            // Copy gesture state so the event sink can update the last emitted value independently.
            let view = Cursor {
                gesture,
                manual: cursor.manual,
                ..Cursor::new()
            };
            if chase && let Some(value) = view.mixer_value(lane, window, window.start, time) {
                let value = if matches!(view.gesture, Gesture::Idle) && !window.playing {
                    let next = lane.points.partition_point(|point| {
                        window.map.tick_to_sample(point.tick, window.rate) <= window.start
                    });
                    if let Some(point) = next.checked_sub(1).and_then(|i| lane.points.get(i))
                        && window.map.tick_to_sample(point.tick, window.rate) == window.start
                    {
                        lane.value(point.tick.0 as f64)
                    } else {
                        value
                    }
                } else {
                    value
                };
                emit(window.start, value, true);
            }
            if window.playing {
                let step = ramp_step(window.rate);
                let end = window.start + i64::from(window.frames);
                let mut grid = window.start + (-window.start).rem_euclid(step);
                let first = lane.points.partition_point(|point| {
                    window.map.tick_to_sample(point.tick, window.rate) < window.start
                });
                let mut points = lane.points[first..].iter().peekable();
                let return_end = cursor.return_end(window.rate);
                let mut return_at = return_end.map_or(end, |at| {
                    window.start.saturating_add(at.saturating_sub(time) as i64)
                });
                loop {
                    let point = points.peek().map_or(end, |point| {
                        window.map.tick_to_sample(point.tick, window.rate)
                    });
                    let sample = grid.min(point).min(return_at);
                    if sample >= end {
                        break;
                    }
                    let elapsed = time + (sample - window.start) as u64;
                    if point == sample {
                        // Points that round onto the same sample use the rightmost value.
                        let mut tick = points.next().expect("scheduled point").tick;
                        while let Some(next) = points.next_if(|point| {
                            window.map.tick_to_sample(point.tick, window.rate) == sample
                        }) {
                            tick = next.tick;
                        }
                        let value = if matches!(view.gesture, Gesture::Idle) {
                            lane.value(tick.0 as f64)
                        } else {
                            view.mixer_value(lane, window, sample, elapsed)
                                .unwrap_or(lane.base)
                        };
                        let important =
                            sample == return_at || lane.jumps.binary_search(&tick).is_ok();
                        emit(sample, value, important);
                    } else if let Some(value) = view.mixer_value(lane, window, sample, elapsed) {
                        emit(sample, value, sample == return_at);
                    }
                    if return_at == sample {
                        return_at = end;
                    }
                    if grid == sample {
                        grid += step;
                    }
                }
                if return_end.is_some_and(|at| at < time + u64::from(window.frames)) {
                    cursor.clear_playback_override();
                }
            }
            // Reserve every target's final emitted value even if some intermediate steps were lost.
            cursor.chase = last_emitted.is_some();
            if let Some(mut last) = last_emitted {
                if let Delivery::Lane { last, .. } = &mut last.delivery {
                    *last = true;
                }
                if let Some(event) = self
                    .params
                    .iter_mut()
                    .rev()
                    .find(|event| event.sequence == last.sequence)
                {
                    event.important = true;
                    event.delivery = last.delivery;
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
            if let Gesture::ReadOverride(state) = gesture
                && window.playing
                && state.expired(
                    lane,
                    tick_at(window, window.start + i64::from(window.frames - 1)),
                )
            {
                cursor.clear_playback_override();
            }
        }
        self.params
            .sort_unstable_by_key(|entry| (entry.slot, entry.event.offset, entry.sequence));
        dropped
    }

    fn slot_range(&self, slot: SlotId) -> std::ops::Range<usize> {
        let start = self.params.partition_point(|event| event.slot < slot);
        let end = start + self.params[start..].partition_point(|event| event.slot == slot);
        start..end
    }

    /// MIDI stages accept every prepared event directly through their setter.
    pub(crate) fn stage_events(&mut self, slot: SlotId) -> impl Iterator<Item = Event> + '_ {
        let range = self.slot_range(slot);
        self.params[range].iter().map(|&entry| {
            delivered(entry, &mut self.cursors, &mut self.restores);
            entry.event
        })
    }

    pub(crate) fn add_events(&mut self, slot: SlotId, out: &mut EventList) -> u32 {
        let events = &self.params[self.slot_range(slot)];
        let available = out.room();
        let skip = events.len().saturating_sub(available);
        // Prefer points and each target's final value, retaining the existing note-offs.
        let mut skip_ramps = skip;
        for &event in events {
            if skip_ramps > 0 && !event.important {
                skip_ramps -= 1;
            } else if out.push(event.event.offset, event.event.kind) {
                delivered(event, &mut self.cursors, &mut self.restores);
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
    left_sample: i64,
    right_sample: i64,
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
        if lane.mode == AutomationMode::Off
            || (!lane.reads()
                && cursor.manual().is_none()
                && cursor.return_end(clock.window.rate).is_none())
        {
            return None;
        }
        Some(Self {
            lane,
            cursor,
            clock,
            step: ramp_step(clock.window.rate),
            left_sample: i64::MIN,
            right_sample: i64::MIN,
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
        if sample < self.left_sample || sample >= self.right_sample {
            self.prepare_interval(sample, time, fallback);
        }
        self.left
            + (self.right - self.left) * (sample - self.left_sample) as f32
                / (self.right_sample - self.left_sample) as f32
    }

    fn prepare_interval(&mut self, sample: i64, time: u64, fallback: f32) {
        let clock = self.clock.window;
        let grid = sample - sample.rem_euclid(self.step);
        let end = grid + self.step;
        let gesture = self.cursor.playback_gesture();
        let mut left_point = None;
        let mut right_point = None;
        if matches!(gesture, Gesture::Idle | Gesture::ReadOverride(_)) {
            let frame = |tick| clock.map.tick_to_sample(tick, clock.rate);
            let after = self
                .lane
                .points
                .partition_point(|point| frame(point.tick) <= sample);
            left_point = after
                .checked_sub(1)
                .and_then(|i| self.lane.points.get(i))
                .map(|point| (frame(point.tick), point.tick))
                .filter(|(frame, _)| *frame >= grid);
            right_point = self
                .lane
                .points
                .get(after)
                .map(|point| (frame(point.tick), point.tick))
                .filter(|(frame, _)| *frame <= end);
        }
        self.left_sample = left_point.map_or(grid, |(frame, _)| frame);
        self.right_sample = right_point.map_or(end, |(frame, _)| frame);

        let curve_at = |frame, time| {
            self.cursor
                .mixer_value(self.lane, clock, frame, time)
                .map_or(fallback, |value| self.scale(value))
        };
        let point_value = |tick: Tick, left_side| {
            let at = tick.0 as f64;
            let check_at = if left_side { at.next_down() } else { at };
            if let Gesture::ReadOverride(state) = gesture
                && !state.expired(self.lane, check_at)
            {
                return self.scale(self.cursor.manual);
            }
            self.scale(if left_side {
                self.lane.point_left(tick)
            } else {
                self.lane.value(at)
            })
        };
        let left = left_point.map_or_else(
            || curve_at(grid, time.saturating_sub((sample - grid) as u64)),
            |(_, tick)| point_value(tick, false),
        );
        self.right = right_point.map_or_else(
            || curve_at(end, time + (end - sample) as u64),
            |(_, tick)| point_value(tick, true),
        );
        self.left = left;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::params::ParamId;
    use komponist_model::{DeviceId, Meter, Tempo, TempoMap};

    fn target(points: Vec<Point>) -> CompiledTarget {
        CompiledTarget::new(
            EngineTarget::Slot {
                slot: SlotId(0),
                wire_id: 0,
            },
            AutomationTarget::Param {
                device: DeviceId(1),
                param: ParamId::plugin(0),
            },
            None,
            0.25,
            AutomationMode::Read,
            points,
        )
    }

    #[test]
    fn read_expiry_search_finds_a_change_after_dense_flat_points() {
        let mut points = (0..4096)
            .map(|tick| Point::linear(Tick(tick), 0.25))
            .collect::<Vec<_>>();
        points[3600].value = 0.75;
        let target = target(points);
        let state = assert_no_alloc::assert_no_alloc(|| target.read_override(97.5));
        assert_eq!(state.next, 3600.0);
        assert!(!state.expired(&target, 3599.0));
        assert!(state.expired(&target, 3601.0));
        let after = target.read_override(4000.0);
        assert!(!after.expired(&target, 50_000.0));
    }

    #[test]
    fn read_ignores_a_duplicate_point_shadowed_by_an_incoming_hold() {
        // One pair stays in the immediate search, the other crosses the index's leaf boundary.
        for (at, len) in [(2, 10), (VALUE_RANGE_BLOCK * 2 - 1, VALUE_RANGE_BLOCK * 3)] {
            for shape in [Shape::Hold, Shape::Linear] {
                let mut points: Vec<_> = (0..len)
                    .map(|tick| Point::linear(Tick(tick as i64), 0.2))
                    .collect();
                points[at - 1].shape = shape;
                points[len - 1].value = 0.6;
                points.insert(at, Point::linear(Tick(at as i64), 0.8));
                let lane = target(points);
                let state = assert_no_alloc::assert_no_alloc(|| lane.read_override(0.0));
                assert_eq!(
                    state.expired(&lane, at as f64 + 2.0),
                    shape == Shape::Linear,
                );
                assert!(state.expired(&lane, (len - 1) as f64));
            }
        }

        let jump = target(vec![
            Point {
                tick: Tick::ZERO,
                value: 0.2,
                shape: Shape::Hold,
            },
            Point::linear(Tick(2), 0.2),
            Point::linear(Tick(2), 0.8),
        ]);
        let state = jump.read_override(0.0);
        assert!(!state.expired(&jump, 1.99));
        assert!(state.expired(&jump, 2.0));
    }

    #[test]
    fn a_full_destination_list_does_not_commit_the_staged_value() {
        let mut state = State::new();
        let lanes = [target(vec![Point::linear(Tick::ZERO, 0.8)])];
        state.install(&[], &lanes, &[], &TrackAutomation::default());
        let map = TempoMap::constant(Tempo::DEFAULT, Meter::FOUR_FOUR);
        let window = Window {
            map: &map,
            rate: 48_000.0,
            start: 0,
            frames: 64,
            playing: false,
        };
        let mut full = EventList::new(0);
        let mut ready = EventList::new(1);
        assert_no_alloc::assert_no_alloc(|| {
            state.prepare(&lanes, window, 0, |_| true);
            assert_eq!(state.add_events(SlotId(0), &mut full), 1);
            state.prepare(&lanes, window, 64, |_| true);
            assert_eq!(state.add_events(SlotId(0), &mut ready), 0);
            assert!(
                matches!(ready.finish(), [Event { offset: 0, kind: EventKind::Param {id: 0, value} }] if *value == f64::from(0.8_f32))
            );
            ready.clear();
            state.prepare(&lanes, window, 128, |_| true);
            state.add_events(SlotId(0), &mut ready);
            assert!(ready.finish().is_empty());
        });
    }
}
