//! Project automation and pure curve evaluation, shared by the editors and engine.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::ser::SerializeTuple;
use serde::{Deserialize, Serialize};

pub use crate::ids::LaneId;
use crate::params::ParamId;
use crate::project::{ChannelRef, Clip, ClipContent, Track};
use crate::time::{PPQ, Tick};
use crate::{DeviceId, SendId, TrackId};

pub const LANE_HEIGHT_MIN: u16 = 24;
pub const LANE_HEIGHT_MAX: u16 = 160;
pub const LANE_HEIGHT_DEFAULT: u16 = 40;
pub const THIN_EPSILON: f32 = 0.004;
pub const THIN_MIN_GAP: Tick = Tick(10);

/// A parameter relative to the track or master that owns a lane.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum AutomationTarget {
    Volume,
    Pan,
    Send { send: SendId },
    Param { device: DeviceId, param: ParamId },
    Macro { index: u8 },
}

/// The channel whose automation is played or edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AutomationOwner {
    Track(TrackId),
    Master,
}

impl From<ChannelRef> for AutomationOwner {
    fn from(channel: ChannelRef) -> Self {
        match channel {
            ChannelRef::Track(id) => Self::Track(id),
            ChannelRef::Master => Self::Master,
        }
    }
}

/// The playback and capture mode of one lane.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutomationMode {
    Off,
    #[default]
    Read,
    Touch,
    Latch,
}

impl AutomationMode {
    fn is_read(&self) -> bool {
        *self == Self::Read
    }
}

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackAutomation {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shown: bool,
    #[serde(default)]
    pub lanes: Vec<Arc<Lane>>,
}

impl TrackAutomation {
    /// Whether there is no lane or visibility state to save.
    pub fn is_empty(&self) -> bool {
        !self.shown && self.lanes.is_empty()
    }

    pub fn lane(&self, target: &AutomationTarget) -> Option<&Lane> {
        self.lanes
            .iter()
            .find(|lane| &lane.target == target)
            .map(Arc::as_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lane {
    pub id: LaneId,
    pub target: AutomationTarget,
    pub label: String,
    #[serde(default, skip_serializing_if = "AutomationMode::is_read")]
    pub mode: AutomationMode,
    pub points: Points,
    pub height: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClipAutomation {
    pub target: AutomationTarget,
    pub label: String,
    pub points: Points,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub tick: Tick,
    pub value: f32,
    /// Interpolation from this point to the next point.
    pub shape: Shape,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Shape {
    #[default]
    Linear,
    Curve {
        tension: f32,
    },
    Hold,
}

impl Point {
    pub fn linear(tick: Tick, value: f32) -> Self {
        Self {
            tick,
            value,
            shape: Shape::Linear,
        }
    }
}

impl Serialize for Point {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut tuple =
            serializer.serialize_tuple(if self.shape == Shape::Linear { 2 } else { 3 })?;
        tuple.serialize_element(&self.tick)?;
        tuple.serialize_element(&self.value)?;
        match self.shape {
            Shape::Linear => {}
            Shape::Curve { tension } => tuple.serialize_element(&tension)?,
            Shape::Hold => tuple.serialize_element("hold")?,
        }
        tuple.end()
    }
}

impl<'de> Deserialize<'de> for Point {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Encoded {
            Shaped(Tick, f32, Encoding),
            Linear(Tick, f32),
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Encoding {
            Curve(f32),
            Name(ShapeName),
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "lowercase")]
        enum ShapeName {
            Hold,
        }
        Ok(match Encoded::deserialize(deserializer)? {
            Encoded::Shaped(tick, value, Encoding::Curve(tension)) => Self {
                tick,
                value,
                shape: Shape::Curve { tension },
            },
            Encoded::Shaped(tick, value, Encoding::Name(ShapeName::Hold)) => Self {
                tick,
                value,
                shape: Shape::Hold,
            },
            Encoded::Linear(tick, value) => Self::linear(tick, value),
        })
    }
}

/// Sorted points with an optional left and right value at each tick. Shared by undo snapshots.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Points(Arc<Vec<Point>>);

impl Points {
    /// Clamps values and bend, then keeps the first and last point of each tick.
    pub fn new(mut points: Vec<Point>) -> Self {
        points.sort_by_key(|point| point.tick);
        for point in &mut points {
            point.value = normalized(point.value);
            if let Shape::Curve { tension } = point.shape {
                point.shape = curve_shape(tension);
            }
        }
        if points
            .windows(3)
            .any(|window| window[0].tick == window[2].tick)
        {
            points = points
                .chunk_by(|a, b| a.tick == b.tick)
                .flat_map(|group| {
                    [
                        Some(group[0]),
                        group.last().copied().filter(|_| group.len() > 1),
                    ]
                    .into_iter()
                    .flatten()
                })
                .collect();
        }
        Self(Arc::new(points))
    }

    pub fn as_slice(&self) -> &[Point] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl AsRef<[Point]> for Points {
    fn as_ref(&self) -> &[Point] {
        self.as_slice()
    }
}

impl Serialize for Points {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // A lane may contain thousands of short tuples. Keep the points compact while the
        // surrounding project stays pretty-printed.
        let encoded = serde_json::to_string(self.as_slice()).map_err(serde::ser::Error::custom)?;
        serde_json::value::RawValue::from_string(encoded)
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Points {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let points = Vec::<Point>::deserialize(deserializer)?;
        if !points.is_sorted_by_key(|point| point.tick) {
            eprintln!("Reordered unsorted automation points");
        }
        Ok(Self::new(points))
    }
}

fn normalized(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn curve_shape(tension: f32) -> Shape {
    if tension.is_nan() || tension == 0.0 {
        Shape::Linear
    } else {
        Shape::Curve {
            tension: tension.clamp(-1.0, 1.0),
        }
    }
}

/// The automation and macro bend. Positive tension starts slowly and ends quickly.
pub fn bend(t: f32, tension: f32) -> f32 {
    let t = normalized(t);
    let k = 6.0 * tension.clamp(-1.0, 1.0);
    if k.abs() < 1e-6 || k.is_nan() {
        t
    } else {
        (k * t).exp_m1() / k.exp_m1()
    }
}

fn interpolate(first: Point, last: Point, offset: f64) -> f32 {
    let length = (last.tick.0 as f64 - first.tick.0 as f64).max(1.0);
    let t = (offset / length).clamp(0.0, 1.0) as f32;
    let progress = match first.shape {
        Shape::Linear => t,
        Shape::Curve { tension } => bend(t, tension),
        Shape::Hold => 0.0,
    };
    first.value + (last.value - first.value) * progress
}

/// The normalized value at a tick. A double point jumps to its right value at that tick.
pub fn value_at(points: &[Point], tick: Tick) -> Option<f32> {
    let next = points.partition_point(|point| point.tick <= tick);
    evaluate(points, next, |first| {
        (tick.0 as i128 - first.0 as i128) as f64
    })
}

/// Like [`value_at`], with fractional ticks for sample-accurate engine evaluation.
pub fn value_at_fractional(points: &[Point], tick: f64) -> Option<f32> {
    let next = points.partition_point(|point| point.tick.0 as f64 <= tick);
    evaluate(points, next, |first| tick - first.0 as f64)
}

fn evaluate(points: &[Point], next: usize, offset: impl FnOnce(Tick) -> f64) -> Option<f32> {
    if next == 0 {
        return points.first().map(|point| point.value);
    }
    let first = points[next - 1];
    Some(match points.get(next) {
        Some(&last) => interpolate(first, last, offset(first.tick)),
        None => first.value,
    })
}

/// The start-to-end time of a clip automation repetition. Step envelopes use the repeating pattern.
pub fn clip_content_length(clip: &Clip) -> Tick {
    match &clip.content {
        ClipContent::Step(step) => Tick(step.resolution.ticks().0 * i64::from(step.length_steps)),
        _ => clip.loop_length.unwrap_or(clip.length),
    }
    .max(Tick(1))
}

/// Points inside the clip's content range. Other points are retained for later length changes.
pub fn clip_points<'a>(clip: &Clip, envelope: &'a ClipAutomation) -> &'a [Point] {
    let points = envelope.points.as_slice();
    let first = points.partition_point(|point| point.tick < Tick::ZERO);
    let last = points.partition_point(|point| point.tick <= clip_content_length(clip));
    &points[first..last]
}

fn clip_envelope<'a>(clip: &'a Clip, target: &AutomationTarget) -> Option<&'a ClipAutomation> {
    if clip.mute || clip.kind() == crate::ClipKind::Audio {
        return None;
    }
    clip.automation
        .iter()
        .find(|envelope| &envelope.target == target && !clip_points(clip, envelope).is_empty())
}

/// The winning clip, envelope and content-relative tick at the timeline position.
pub fn active_clip_automation<'a>(
    track: &'a Track,
    target: &AutomationTarget,
    tick: Tick,
) -> Option<(&'a Clip, &'a ClipAutomation, Tick)> {
    let end = track.clips.partition_point(|clip| clip.start <= tick);
    track.clips[..end]
        .iter()
        .rev()
        .filter(|clip| tick < clip.end())
        .find_map(|clip| {
            let envelope = clip_envelope(clip, target)?;
            let offset = Tick((tick - clip.start).0 % clip_content_length(clip).0);
            Some((clip.as_ref(), envelope, offset))
        })
}

/// The value of the winning curve, if a curve drives the target here.
pub fn effective_value_at(track: &Track, target: &AutomationTarget, tick: Tick) -> Option<f32> {
    let lane = track.automation.lane(target)?;
    if lane.mode == AutomationMode::Off {
        return None;
    }
    if let Some((clip, envelope, offset)) = active_clip_automation(track, target, tick) {
        return value_at(clip_points(clip, envelope), offset);
    }
    value_at(lane.points.as_slice(), tick)
}

/// One winning half-open override span. Several repetitions can occupy the same span.
pub struct ClipOverrideSpan<'a> {
    pub start: Tick,
    pub end: Tick,
    pub clip: &'a Clip,
    pub envelope: &'a ClipAutomation,
}

/// All winning spans in timeline order, ordered by clip start and id on overlap.
pub fn clip_override_spans<'a>(
    track: &'a Track,
    target: &AutomationTarget,
) -> Vec<ClipOverrideSpan<'a>> {
    let clips: Vec<_> = track
        .clips
        .iter()
        .filter_map(|clip| clip_envelope(clip, target).map(|envelope| (clip.as_ref(), envelope)))
        .collect();
    let mut events = Vec::with_capacity(clips.len() * 2);
    for (index, &(clip, _)) in clips.iter().enumerate() {
        if clip.start < clip.end() {
            events.extend([(clip.start, index, true), (clip.end(), index, false)]);
        }
    }
    events.sort_unstable_by_key(|event| event.0);
    let mut active = BTreeSet::new();
    let mut spans: Vec<ClipOverrideSpan<'a>> = Vec::new();
    let mut groups = events.chunk_by(|a, b| a.0 == b.0).peekable();
    while let Some(at) = groups.next() {
        for &(_, index, begin) in at {
            let clip = clips[index].0;
            let key = (clip.start, clip.id, index);
            if begin {
                active.insert(key);
            } else {
                active.remove(&key);
            }
        }
        let (Some(next), Some(&(_, _, index))) = (groups.peek(), active.last()) else {
            continue;
        };
        let (clip, envelope) = clips[index];
        if let Some(last) = spans.last_mut()
            && last.clip.id == clip.id
            && last.end == at[0].0
        {
            last.end = next[0].0;
        } else {
            spans.push(ClipOverrideSpan {
                start: at[0].0,
                end: next[0].0,
                clip,
                envelope,
            });
        }
    }
    spans
}

/// An upper bound on the number of points emitted by the winning clip spans. One
/// repetition contributes at most two points for every stored point plus its outer endpoints.
pub(crate) fn clip_expansion_points(track: &Track, target: &AutomationTarget) -> u128 {
    clip_override_spans(track, target)
        .into_iter()
        .map(|span| {
            let points = clip_points(span.clip, span.envelope);
            if constant_value(points).is_some() {
                return 2;
            }
            let content = i128::from(clip_content_length(span.clip).0);
            let origin = i128::from(span.clip.start.0);
            let first = (i128::from(span.start.0) - origin) / content;
            let last = (i128::from(span.end.0) - 1 - origin) / content;
            let repetitions = (last - first + 1) as u128;
            repetitions * (2 * points.len() as u128 + 2)
        })
        .fold(0, u128::saturating_add)
}

/// Flattens the winning lane and clip curves, including loop discontinuities and returns to
/// `base` in gaps when the lane exists only to hold a clip envelope's mode.
pub fn effective_points(track: &Track, target: &AutomationTarget, base: f32) -> Vec<Point> {
    let Some(lane) = track
        .automation
        .lane(target)
        .filter(|lane| lane.mode != AutomationMode::Off)
    else {
        return Vec::new();
    };
    merged_clip_points(track, lane, base).unwrap_or_else(|| lane.points.as_slice().to_vec())
}

/// Combines a lane with its winning clip overrides, including when its mode is Off. Callers
/// preserving automation during a destructive edit retain the mode separately.
pub(crate) fn merged_clip_points(track: &Track, lane: &Lane, base: f32) -> Option<Vec<Point>> {
    let spans = clip_override_spans(track, &lane.target);
    if spans.is_empty() {
        return None;
    }
    let points = lane.points.as_slice();
    let fallback = [Point::linear(Tick::ZERO, normalized(base))];
    let points = if points.is_empty() { &fallback } else { points };
    let mut cursor = points[0].tick.min(spans[0].start);
    // Before either curve starts, hold the first lane point, including the left value of a jump.
    let mut result = vec![Point::linear(cursor, points[0].value)];
    for span in spans {
        if cursor < span.start {
            append_slice(&mut result, points, cursor, span.start, Tick::ZERO);
        }
        append_clip_span(
            &mut result,
            span.clip,
            span.envelope,
            span.start,
            span.end,
            Tick::ZERO,
        );
        cursor = span.end;
    }
    if cursor < points.last().unwrap().tick {
        append_slice(
            &mut result,
            points,
            cursor,
            points.last().unwrap().tick,
            Tick::ZERO,
        );
        push_point(&mut result, *points.last().unwrap());
    } else {
        push_point(
            &mut result,
            Point::linear(cursor, value_at(points, cursor).unwrap_or(base)),
        );
    }
    Some(result)
}

fn push_point(out: &mut Vec<Point>, point: Point) {
    if let Some(last) = out.last_mut()
        && last.tick == point.tick
        && last.value == point.value
    {
        *last = point;
        return;
    }
    out.push(point);
}

fn constant_value(points: &[Point]) -> Option<f32> {
    let value = points.first()?.value;
    points
        .iter()
        .all(|point| point.value == value)
        .then_some(value)
}

fn append_clip_span(
    out: &mut Vec<Point>,
    clip: &Clip,
    envelope: &ClipAutomation,
    mut start: Tick,
    end: Tick,
    shift: Tick,
) {
    let content = clip_content_length(clip);
    let points = clip_points(clip, envelope);
    if points.is_empty() {
        return;
    }
    // A flat envelope has the same value in every repetition. Only its outer boundaries
    // matter, even for a long clip with a one-tick content length.
    if let Some(value) = constant_value(points) {
        push_point(out, Point::linear(start + shift, value));
        push_point(out, Point::linear(end + shift, value));
        return;
    }
    while start < end {
        let loop_start = start - Tick((start - clip.start).0 % content.0);
        let stop = Tick(loop_start.0.saturating_add(content.0)).min(end);
        append_slice(
            out,
            points,
            start - loop_start,
            stop - loop_start,
            loop_start + shift,
        );
        start = stop;
    }
}

/// Extracts [start, end), appending the left limit at end. For an exponential segment, reparameterizing
/// a subinterval is exact: the exponent scales by the ratio of the segment lengths.
fn append_slice(out: &mut Vec<Point>, points: &[Point], start: Tick, end: Tick, shift: Tick) {
    let mut previous = points
        .partition_point(|point| point.tick <= start)
        .checked_sub(1);
    let mut cursor = start;
    loop {
        let next = previous.map_or(0, |index| index + 1);
        let stop = points.get(next).map_or(end, |point| point.tick.min(end));
        let value = match previous {
            Some(_) => evaluate(points, next, |first| (cursor - first).0 as f64).unwrap(),
            None => points[0].value,
        };
        let shape = match (previous, points.get(next)) {
            (Some(index), Some(last)) => {
                let first = points[index];
                match first.shape {
                    Shape::Curve { tension } => {
                        let ratio = (stop - cursor).0 as f64 / (last.tick - first.tick).0 as f64;
                        curve_shape(tension * ratio as f32)
                    }
                    shape => shape,
                }
            }
            _ => Shape::Linear,
        };
        push_point(
            out,
            Point {
                tick: cursor + shift,
                value,
                shape,
            },
        );
        if stop == end {
            let value = match (previous, points.get(next)) {
                (Some(index), Some(&last)) => {
                    interpolate(points[index], last, (end - points[index].tick).0 as f64)
                }
                _ => value,
            };
            push_point(out, Point::linear(end + shift, value));
            break;
        }
        if let Some(index) = previous
            && points[index].shape == Shape::Hold
        {
            push_point(out, Point::linear(stop + shift, points[index].value));
        } else if points.get(next + 1).is_some_and(|point| point.tick == stop) {
            push_point(out, Point::linear(stop + shift, points[next].value));
        }
        previous =
            Some(next + usize::from(points.get(next + 1).is_some_and(|point| point.tick == stop)));
        cursor = stop;
    }
}

/// Splits an envelope into two content-relative curves at a timeline offset.
pub(crate) fn split_envelope(clip: &Clip, envelope: &ClipAutomation, at: Tick) -> (Points, Points) {
    let mut left = Vec::new();
    let mut right = Vec::new();
    append_clip_span(
        &mut left,
        clip,
        envelope,
        clip.start,
        clip.start + at,
        -clip.start,
    );
    append_clip_span(
        &mut right,
        clip,
        envelope,
        clip.start + at,
        clip.end(),
        -(clip.start + at),
    );
    (Points::new(left), Points::new(right))
}

/// Expands an envelope when converting a step clip to an unlooped MIDI clip.
pub(crate) fn unroll_envelope(clip: &Clip, envelope: &ClipAutomation) -> Points {
    let mut points = Vec::new();
    append_clip_span(
        &mut points,
        clip,
        envelope,
        clip.start,
        clip.end(),
        -clip.start,
    );
    Points::new(points)
}

/// Reduces a captured segment or a freehand stroke. Stepped values retain only change points,
/// while continuous values use Ramer–Douglas–Peucker with ticks measured in quarter notes.
pub fn thin(points: &[Point], stepped: bool) -> Points {
    let sorted = Points::new(points.to_vec());
    let points = sorted.as_slice();
    if points.len() < 2 {
        return if stepped {
            Points::new(
                points
                    .iter()
                    .map(|point| Point {
                        shape: Shape::Hold,
                        ..*point
                    })
                    .collect(),
            )
        } else {
            sorted
        };
    }
    if stepped {
        let last = points.len() - 1;
        return Points::new(
            points
                .iter()
                .copied()
                .enumerate()
                .filter(|&(index, point)| {
                    index == 0 || index == last || points[index - 1].value != point.value
                })
                .map(|(_, point)| Point {
                    shape: Shape::Hold,
                    ..point
                })
                .collect(),
        );
    }
    let mut spaced = Vec::with_capacity(points.len());
    spaced.push(points[0]);
    let last = points[points.len() - 1];
    for &point in &points[1..points.len() - 1] {
        if point.tick.0.saturating_sub(spaced.last().unwrap().tick.0) >= THIN_MIN_GAP.0
            && last.tick.0.saturating_sub(point.tick.0) >= THIN_MIN_GAP.0
        {
            spaced.push(point);
        }
    }
    spaced.push(last);
    let mut keep = vec![false; spaced.len()];
    keep[0] = true;
    keep[spaced.len() - 1] = true;
    let mut pending = vec![(0, spaced.len() - 1)];
    while let Some((start, end)) = pending.pop() {
        let a = spaced[start];
        let b = spaced[end];
        let (dx, dy) = (beat_delta(b, a), f64::from(b.value - a.value));
        let length_squared = dx * dx + dy * dy;
        let mut furthest = None;
        let mut max_distance = f64::from(THIN_EPSILON).powi(2);
        for (offset, &point) in spaced[start + 1..end].iter().enumerate() {
            let (px, py) = (beat_delta(point, a), f64::from(point.value - a.value));
            let t = if length_squared == 0.0 {
                0.0
            } else {
                ((px * dx + py * dy) / length_squared).clamp(0.0, 1.0)
            };
            let distance = (px - dx * t).powi(2) + (py - dy * t).powi(2);
            if distance > max_distance {
                max_distance = distance;
                furthest = Some(start + 1 + offset);
            }
        }
        if let Some(index) = furthest {
            keep[index] = true;
            pending.extend([(start, index), (index, end)]);
        }
    }
    Points::new(
        spaced
            .into_iter()
            .zip(keep)
            .filter(|(_, keep)| *keep)
            .map(|(point, _)| Point::linear(point.tick, point.value))
            .collect(),
    )
}

fn beat_delta(later: Point, earlier: Point) -> f64 {
    (later.tick.0 as i128 - earlier.tick.0 as i128) as f64 / PPQ as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edits::{self, automation as edit};
    use crate::fixtures::{automation_project, demo_song};
    use crate::time::Tempo;
    use proptest::prelude::*;

    #[test]
    fn bend_is_monotone_and_mirrors_negative_tension() {
        for tension in [0.25, 0.5, 1.0] {
            assert_eq!(bend(0.0, tension), 0.0);
            assert_eq!(bend(1.0, tension), 1.0);
            assert!(bend(0.2, tension) < bend(0.5, tension));
            assert!(bend(0.5, tension) < 0.5);
            assert!((bend(0.1, tension) + bend(0.9, -tension) - 1.0).abs() < 1e-6);
        }
    }

    #[test]
    fn duplicate_tick_has_a_left_limit_and_a_right_value() {
        let points = [
            Point::linear(Tick(0), 0.1),
            Point::linear(Tick(10), 0.2),
            Point::linear(Tick(10), 0.8),
            Point::linear(Tick(20), 0.9),
        ];
        assert_eq!(value_at(&points, Tick(-99)), Some(0.1));
        assert!(value_at_fractional(&points, 9.99).unwrap() < 0.2);
        assert_eq!(value_at(&points, Tick(10)), Some(0.8));
        assert_eq!(value_at(&points, Tick(99)), Some(0.9));
    }

    #[test]
    fn winner_and_exact_curves_survive_clip_edges_loops_and_overlap() {
        let mut project = demo_song();
        project.tempo = Tempo::new(120.0);
        let track_id = project.tracks[1].id;
        let first = project.tracks[1].clips[0].id;
        let second = project.tracks[1].clips[1].id;
        let owner = AutomationOwner::Track(track_id);
        let target = AutomationTarget::Pan;
        let (p, lane) = edit::add_lane(&project, owner, target.clone(), String::new()).unwrap();
        let curve = |tension| {
            Points::new(vec![
                Point {
                    tick: Tick::ZERO,
                    value: 0.1,
                    shape: Shape::Curve { tension },
                },
                Point::linear(Tick(1500), 1.0),
            ])
        };
        let mut p = edit::set_lane_points(&p, owner, lane, curve(0.75)).unwrap();
        p = edits::move_clips(&p, &[first], Tick(150), 0).unwrap();
        p = edits::move_clips(
            &p,
            &[second],
            Tick(540) - project.tracks[1].clips[1].start,
            0,
        )
        .unwrap();
        p = edits::set_clip_loop(&p, first, Some(Tick(480)), Tick(1130)).unwrap();
        p = edits::set_clip_length(&p, second, Tick(120)).unwrap();
        for (clip, value) in [(first, 0.4), (second, 0.7)] {
            p = edit::add_clip_automation(&p, clip, target.clone(), String::new()).unwrap();
            p = edit::set_clip_points(
                &p,
                clip,
                &target,
                Points::new(vec![
                    Point {
                        tick: Tick::ZERO,
                        value,
                        shape: Shape::Curve { tension: -0.4 },
                    },
                    Point::linear(Tick(90), 0.8),
                ]),
            )
            .unwrap();
        }
        for freeze in [false, true] {
            let track = p.track(track_id).unwrap();
            if !freeze {
                let spans = clip_override_spans(track, &target);
                assert_eq!(
                    spans.iter().map(|s| s.clip.id).collect::<Vec<_>>(),
                    [first, second, first]
                );
            }
            let flattened = effective_points(track, &target, 0.5);
            for t in -1..1550 {
                let tick = Tick(t);
                let live = effective_value_at(track, &target, tick).unwrap();
                let compiled = value_at(&flattened, tick).unwrap();
                assert!(
                    (live - compiled).abs() < 2e-6,
                    "{tick:?}: {live} vs {compiled}"
                );
            }
            p = edits::set_clips_muted(&p, &[second], true).unwrap();
        }
    }

    #[test]
    fn clip_only_automation_returns_to_base_between_overrides() {
        let project = automation_project();
        let track = &project.tracks[0];
        let clip = &track.clips[0];
        let target = AutomationTarget::Pan;
        let output = effective_points(track, &target, 0.5);
        assert_eq!(value_at(&output, Tick(-1)), Some(0.5));
        assert_eq!(value_at(&output, clip.start), Some(0.25));
        assert_eq!(
            value_at(&output, clip.start + clip_content_length(clip)),
            Some(0.25)
        );
        assert_eq!(value_at(&output, clip.end()), Some(0.5));
        let project = edit::set_all_modes(
            &project,
            AutomationOwner::Track(track.id),
            AutomationMode::Off,
        )
        .unwrap();
        assert!(effective_points(&project.tracks[0], &target, 0.5).is_empty());
    }

    #[test]
    fn flattened_curve_preserves_the_first_jump_before_a_later_override() {
        let project = automation_project();
        let track = &project.tracks[2];
        let target = &track.clips[0].automation[0].target;
        let lane = track.automation.lane(target).unwrap().id;
        let project = edit::set_lane_points(
            &project,
            AutomationOwner::Track(track.id),
            lane,
            Points::new(vec![
                Point::linear(Tick(10), 0.2),
                Point::linear(Tick(10), 0.8),
            ]),
        )
        .unwrap();
        let track = &project.tracks[2];
        let compiled = effective_points(track, target, 0.5);
        for tick in [Tick(9), Tick(10), Tick(11), track.clips[0].start] {
            assert_eq!(
                value_at(&compiled, tick),
                effective_value_at(track, target, tick)
            );
        }
        assert_eq!(value_at(&compiled, Tick(9)), Some(0.2));
    }

    #[test]
    fn constant_loop_envelope_compiles_without_expanding_repetitions() {
        let project = automation_project();
        let track = &project.tracks[2];
        let clip = &track.clips[0];
        let target = clip.automation[0].target.clone();
        let project =
            edits::set_clip_loop(&project, clip.id, Some(Tick(1)), Tick(1_000_000_000)).unwrap();
        let track = &project.tracks[2];
        let clip = &track.clips[0];
        let compiled = effective_points(track, &target, 0.5);
        assert!(
            !compiled
                .iter()
                .any(|point| clip.start < point.tick && point.tick < clip.end())
        );
        for tick in [
            clip.start - Tick(1),
            clip.start,
            clip.start + Tick(500_000_000),
            clip.end() - Tick(1),
            clip.end(),
        ] {
            assert_eq!(
                value_at(&compiled, tick),
                effective_value_at(track, &target, tick)
            );
        }
    }

    #[test]
    fn thinning_uses_hold_for_discrete_changes_and_preserves_continuous_endpoints() {
        let raw: Vec<_> = (0..=100)
            .map(|n| Point::linear(Tick(n * 20), n as f32 / 100.0))
            .collect();
        assert_eq!(thin(&raw, false).as_slice(), [raw[0], raw[100]]);
        let steps = [0.0, 0.0, 1.0, 0.0, 0.0];
        let raw: Vec<_> = steps
            .iter()
            .enumerate()
            .map(|(i, v)| Point::linear(Tick(i as i64), *v))
            .collect();
        let thinned = thin(&raw, true);
        assert_eq!(
            thinned
                .as_slice()
                .iter()
                .map(|p| p.tick.0)
                .collect::<Vec<_>>(),
            [0, 2, 3, 4]
        );
        assert!(thinned.as_slice().iter().all(|p| p.shape == Shape::Hold));
    }

    proptest! {
        #[test]
        fn point_sets_keep_sorted_ticks_and_clamp_values(
            raw in prop::collection::vec((-15_i64..65, -2.0_f32..3.0, -3.0_f32..3.0), 0..250)
        ) {
            let points = Points::new(raw.iter().map(|&(tick, value, tension)| Point {
                tick: Tick(tick),
                value,
                shape: Shape::Curve { tension },
            }).collect());
            let points = points.as_slice();
            prop_assert!(points.is_sorted_by_key(|p| p.tick));
            prop_assert!(!points.windows(3).any(|p| p[0].tick == p[2].tick));
            prop_assert!(points.iter().all(|p| (0.0..=1.0).contains(&p.value)));
        }

        #[test]
        fn thinning_bounds_distance_on_grid(values in prop::collection::vec(0.0_f32..1.0, 2..75)) {
            let raw: Vec<_> = values.into_iter().enumerate().map(|(index, value)| {
                Point::linear(Tick(index as i64 * 15), value)
            }).collect();
            let reduced = thin(&raw, false);
            prop_assert_eq!(reduced.as_slice().first(), raw.first());
            prop_assert_eq!(reduced.as_slice().last(), raw.last());
            for p in raw {
                let minimum = reduced.as_slice().windows(2).map(|window| {
                    let (a, b) = (window[0], window[1]);
                    let (dx, dy) = (beat_delta(b, a), f64::from(b.value - a.value));
                    let (px, py) = (beat_delta(p, a), f64::from(p.value - a.value));
                    let t = ((px * dx + py * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
                    (px - dx * t).hypot(py - dy * t)
                }).fold(f64::INFINITY, f64::min);
                prop_assert!(minimum < f64::from(THIN_EPSILON) + 1e-10);
            }
        }
    }
}
