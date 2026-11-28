//! Automation capture on the control thread. Engine events carry the same clock used for playback.
//! Captured edits stay in a preview until the pass can be applied as one document edit.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use komponist_model::automation::{
    self, AutomationMode, AutomationOwner, AutomationTarget, LaneId, Point, Points, Shape,
};
use komponist_model::edits::{EditResult, automation as edits};
use komponist_model::{ClipId, DeviceId, MAX_DOC_TICK, Project, Tick};

use crate::engine_sync::AutomationDraft;

/// Prevents a single long uninterrupted gesture from retaining unbounded raw data.
const MAX_RAW_POINTS: usize = 100_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Destination {
    Lane(AutomationOwner, LaneId),
    Clip(ClipId, AutomationTarget),
}

impl Destination {
    fn original<'a>(&self, project: &'a Project) -> Option<&'a Points> {
        match self {
            Self::Lane(owner, id) => edits::lanes(project, *owner)
                .ok()?
                .lanes
                .iter()
                .find(|lane| lane.id == *id)
                .map(|lane| &lane.points),
            Self::Clip(clip, target) => project
                .find_clip(*clip)?
                .1
                .automation
                .iter()
                .find(|envelope| &envelope.target == target)
                .map(|envelope| &envelope.points),
        }
    }

    fn store(&self, project: &Project, points: Points) -> EditResult {
        match self {
            Self::Lane(owner, lane) => edits::set_lane_points(project, *owner, *lane, points),
            Self::Clip(clip, target) => edits::set_clip_points(project, *clip, target, points),
        }
    }
}

#[derive(Debug, Clone)]
struct Segment {
    destination: Destination,
    /// Timeline minus content position for the current occurrence of a clip.
    origin: Tick,
    start: Tick,
    limit: Tick,
    raw: Vec<Point>,
    /// An exact source cut, kept independently of the captured sample spacing.
    boundary_anchor: bool,
    release_anchor: Option<Tick>,
}

impl Segment {
    fn end(&self) -> Tick {
        Tick(self.limit.0.saturating_add(self.origin.0))
    }

    fn append(&mut self, tick: Tick, value: f32, stepped: bool) {
        let tick = (tick - self.origin).clamp(self.start, self.limit);
        let point = Point::linear(tick, value);
        if let Some(last) = self.raw.last_mut()
            && last.tick == tick
        {
            *last = point;
        } else {
            self.raw.push(point);
        }
        if self.raw.len() >= MAX_RAW_POINTS {
            self.raw = automation::thin(&self.raw, stepped).as_slice().to_vec();
            if self.raw.len() >= MAX_RAW_POINTS {
                // Keep endpoints and uniformly reduce unusually dense controller streams.
                let last = self.raw.pop().expect("segment has points");
                self.raw = self.raw.iter().step_by(2).copied().collect();
                self.raw.push(last);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct Write {
    owner: AutomationOwner,
    target: AutomationTarget,
    lane: LaneId,
    mode: AutomationMode,
    stepped: bool,
    base: f32,
    segment: Option<Segment>,
}

#[derive(Debug, Clone, Copy)]
enum Finish {
    Boundary,
    LatchEnd,
}

#[derive(Debug, Clone)]
struct RecordedSegment {
    start: Tick,
    points: Points,
    base: f32,
}

impl RecordedSegment {
    fn new(start: Tick, points: Points, base: f32) -> Self {
        // Points clamps ticks to the document range. Keep the recorded interval in that range.
        Self {
            start: start.clamp(-MAX_DOC_TICK, MAX_DOC_TICK),
            points,
            base,
        }
    }

    fn replace(&self, original: &Points) -> Points {
        automation::replace_recorded_points(original, self.start, &self.points, self.base)
    }
}

/// Completed segments for one destination. Keep the splices so they can be replayed if the
/// document curve changes, but extend the cached curve only with new segments in the usual case.
#[derive(Debug, Clone, Default)]
struct CompletedCurve {
    segments: Vec<RecordedSegment>,
    ranges: Vec<(Tick, Tick)>,
    merged: RefCell<Option<(Points, Points, usize)>>,
}

impl CompletedCurve {
    fn push(&mut self, segment: RecordedSegment) {
        let end = segment
            .points
            .as_slice()
            .last()
            .expect("finished segment")
            .tick;
        let mut start = segment.start;
        let mut end = end;
        let first = self.ranges.partition_point(|&(_, right)| right < start);
        let after = self.ranges.partition_point(|&(left, _)| left <= end);
        if first < after {
            start = start.min(self.ranges[first].0);
            end = end.max(self.ranges[after - 1].1);
        }
        self.ranges.splice(first..after, [(start, end)]);
        self.segments.push(segment);
    }

    fn merged(&self, original: &Points) -> Points {
        let mut merged = self.merged.borrow_mut();
        let (source, points, applied) =
            merged.get_or_insert_with(|| (original.clone(), original.clone(), 0));
        if !std::ptr::eq(source.as_slice(), original.as_slice()) {
            *source = original.clone();
            *points = original.clone();
            *applied = 0;
        }
        for segment in &self.segments[*applied..] {
            *points = segment.replace(points);
        }
        *applied = self.segments.len();
        points.clone()
    }
}

/// One pass across one or more targets and loop laps.
#[derive(Debug, Clone, Default)]
pub struct AutomationCapture {
    writing: Vec<Write>,
    finished: HashMap<Destination, CompletedCurve>,
    /// Revision of the completed segments. Zero means there is no playback draft.
    completed_revision: u64,
}

// Captures are replaced and moved between the live pass and the audio writer. Revisions must
// distinguish different passes even if they completed the same number of segments.
fn next_draft_revision() -> u64 {
    static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}

impl AutomationCapture {
    /// Whether this pass has live writes or completed segments.
    pub fn active(&self) -> bool {
        !(self.writing.is_empty() && self.finished.is_empty())
    }

    /// Whether a target's lane is currently writing.
    pub fn is_writing(&self, owner: AutomationOwner, lane: LaneId) -> bool {
        self.writing
            .iter()
            .any(|w| w.owner == owner && w.lane == lane)
    }

    /// Whether any lane on the owner is currently writing.
    pub fn owner_writing(&self, owner: AutomationOwner) -> bool {
        self.writing.iter().any(|write| write.owner == owner)
    }

    /// Whether the plugin parameter is currently writing automation.
    pub fn plugin_parameter_writing(&self, device: DeviceId, id: u32) -> bool {
        self.writing.iter().any(|write| {
            matches!(&write.target, AutomationTarget::Param { device: d, param }
                if *d == device && param.plugin_id() == Some(id))
        })
    }

    /// The last captured value of the live write for this target.
    pub fn current_value(&self, owner: AutomationOwner, target: &AutomationTarget) -> Option<f32> {
        self.writing
            .iter()
            .find(|w| w.owner == owner && &w.target == target)?
            .segment
            .as_ref()?
            .raw
            .last()
            .map(|point| point.value)
    }

    /// The tick ranges written into an arrangement lane, including live samples.
    pub fn preview_lane_ranges(&self, owner: AutomationOwner, lane: LaneId) -> Vec<(Tick, Tick)> {
        self.preview_ranges(&Destination::Lane(owner, lane))
    }

    /// The clip-local tick ranges written into an envelope, including live samples.
    pub fn preview_clip_ranges(
        &self,
        clip: ClipId,
        target: &AutomationTarget,
    ) -> Vec<(Tick, Tick)> {
        self.preview_ranges(&Destination::Clip(clip, target.clone()))
    }

    fn preview_ranges(&self, destination: &Destination) -> Vec<(Tick, Tick)> {
        self.finished
            .get(destination)
            .into_iter()
            .flat_map(|curve| curve.ranges.iter().copied())
            .chain(self.writing.iter().filter_map(|write| {
                let segment = write
                    .segment
                    .as_ref()
                    .filter(|segment| &segment.destination == destination)?;
                Some((segment.start, segment.raw.last()?.tick))
            }))
            .collect()
    }

    /// The project lane with the pass applied, for the live preview.
    pub fn preview_lane(
        &self,
        owner: AutomationOwner,
        lane: LaneId,
        original: &Points,
    ) -> Option<Points> {
        self.preview(&Destination::Lane(owner, lane), original)
    }

    /// The clip envelope with the pass applied, for the live preview.
    pub fn preview_clip(
        &self,
        clip: ClipId,
        target: &AutomationTarget,
        original: &Points,
    ) -> Option<Points> {
        self.preview(&Destination::Clip(clip, target.clone()), original)
    }

    fn preview(&self, destination: &Destination, original: &Points) -> Option<Points> {
        let mut result = self
            .finished
            .get(destination)
            .map(|curve| curve.merged(original));
        for write in &self.writing {
            if let Some(segment) = &write.segment
                && &segment.destination == destination
            {
                let preview = RecordedSegment::new(
                    segment.start,
                    Points::new(segment.raw.clone()),
                    write.base,
                );
                result = Some(preview.replace(result.as_ref().unwrap_or(original)));
            }
        }
        result
    }

    /// Whether this pass still refers to an arrangement lane.
    pub fn references_lane(&self, owner: AutomationOwner, lane: LaneId) -> bool {
        self.is_writing(owner, lane) || self.finished.contains_key(&Destination::Lane(owner, lane))
    }

    /// Appends a completed pass to this pass in capture order.
    pub fn merge(&mut self, other: Self) {
        debug_assert!(other.writing.is_empty());
        for (destination, curve) in other.finished {
            let current = self.finished.entry(destination).or_default();
            for segment in curve.segments {
                current.push(segment);
            }
        }
        if other.completed_revision != 0 {
            self.completed_revision = next_draft_revision();
        }
    }

    /// The last live value written into this clip envelope.
    pub fn current_clip_value(&self, clip: ClipId, target: &AutomationTarget) -> Option<f32> {
        self.writing
            .iter()
            .filter_map(|write| write.segment.as_ref())
            .find_map(|segment| match &segment.destination {
                Destination::Clip(id, param) if *id == clip && param == target => {
                    segment.raw.last().map(|point| point.value)
                }
                _ => None,
            })
    }

    /// Starts writing an eligible Touch or Latch lane.
    pub fn open(
        &mut self,
        project: &Project,
        owner: AutomationOwner,
        target: AutomationTarget,
        stepped: bool,
        base: f32,
    ) {
        let Some(lane) = edits::lanes(project, owner)
            .ok()
            .and_then(|a| a.lane(&target))
        else {
            return;
        };
        if !matches!(lane.mode, AutomationMode::Touch | AutomationMode::Latch)
            || self
                .writing
                .iter()
                .any(|w| w.owner == owner && w.target == target)
        {
            return;
        }
        self.writing.push(Write {
            owner,
            target,
            lane: lane.id,
            mode: lane.mode,
            stepped,
            base,
            segment: None,
        });
    }

    /// Adds a captured value at its absolute song tick.
    pub fn value(
        &mut self,
        project: &Project,
        owner: AutomationOwner,
        target: &AutomationTarget,
        tick: Tick,
        value: f32,
    ) {
        let Some(write) = self
            .writing
            .iter_mut()
            .find(|w| w.owner == owner && &w.target == target)
        else {
            return;
        };
        let mut next_tick = tick;
        // Capture events are block-granular. A clip or an iteration can end between them,
        // so finish each source at its actual boundary before routing the new value.
        while write
            .segment
            .as_ref()
            .is_some_and(|segment| tick >= segment.end() && segment.end() < Tick(i64::MAX))
        {
            let segment = write.segment.take().expect("bounded segment");
            next_tick = segment.end();
            let held = segment.raw.last().map_or(value, |point| point.value);
            if let Some(done) = finish_segment(project, segment, write, next_tick, Finish::Boundary)
            {
                let (destination, points) = done;
                self.finished.entry(destination).or_default().push(points);
                self.completed_revision = next_draft_revision();
            }
            let mut next = segment_at(project, write, next_tick);
            if next_tick < tick {
                next.append(next_tick, held, write.stepped);
                next.boundary_anchor = true;
            }
            write.segment = Some(next);
        }
        if write.segment.is_none() {
            write.segment = Some(segment_at(project, write, next_tick));
        }
        if let Some(segment) = &mut write.segment {
            segment.append(tick, value, write.stepped);
        }
    }

    /// Finishes a Touch write and adds the return to the underlying value.
    pub fn release(
        &mut self,
        project: &Project,
        owner: AutomationOwner,
        target: &AutomationTarget,
        tick: Tick,
    ) {
        if let Some(index) = self.writing.iter().position(|w| {
            w.owner == owner && &w.target == target && w.mode == AutomationMode::Touch
        }) {
            let mut write = self.writing.remove(index);
            let Some(mut segment) = write.segment.take() else {
                return;
            };
            let Some(last) = segment.raw.last().copied() else {
                return;
            };
            let return_ticks = project.tempo_map().seconds_to_ticks(0.250).round() as i64;
            let return_end = Tick(tick.0.saturating_add(return_ticks));
            let end_value = self
                .completed_value_at(project, &write, return_end)
                .unwrap_or(write.base);
            let return_limit = if project.loop_on && tick < project.loop_range.end {
                return_end.min(project.loop_range.end)
            } else {
                return_end
            };
            let value_at = |t: Tick| {
                let fraction = ((t - tick).0 as f32 / return_ticks.max(1) as f32).clamp(0.0, 1.0);
                last.value + (end_value - last.value) * fraction
            };
            while segment.end() <= tick && segment.end() < Tick(i64::MAX) {
                let end = segment.end();
                if let Some((destination, points)) =
                    finish_segment(project, segment, &write, end, Finish::Boundary)
                {
                    self.finished.entry(destination).or_default().push(points);
                    self.completed_revision = next_draft_revision();
                }
                segment = segment_at(project, &write, end);
                segment.append(end, last.value, write.stepped);
                segment.boundary_anchor = true;
            }
            segment.append(tick, last.value, write.stepped);
            segment.release_anchor = Some(tick - segment.origin);
            loop {
                let end = segment.end().min(return_limit);
                segment.append(end, value_at(end), write.stepped);
                if let Some(done) = finish_segment(project, segment, &write, end, Finish::Boundary)
                {
                    let (destination, points) = done;
                    self.finished.entry(destination).or_default().push(points);
                    self.completed_revision = next_draft_revision();
                }
                if end == return_limit {
                    break;
                }
                segment = segment_at(project, &write, end);
                segment.append(end, value_at(end), write.stepped);
                segment.boundary_anchor = true;
            }
        }
    }

    fn completed_value_at(&self, project: &Project, write: &Write, tick: Tick) -> Option<f32> {
        let lane = edits::lanes(project, write.owner)
            .ok()?
            .lane(&write.target)?;
        if matches!(write.owner, AutomationOwner::Track(_)) && lane.mode == AutomationMode::Off {
            return None;
        }
        if let AutomationOwner::Track(track) = write.owner
            && let Some((clip, envelope, offset)) =
                automation::active_clip_automation(project.track(track)?, &write.target, tick)
        {
            let destination = Destination::Clip(clip.id, write.target.clone());
            if let Some(curve) = self.finished.get(&destination) {
                let mut envelope = envelope.clone();
                envelope.points = curve.merged(&envelope.points);
                return automation::value_at(automation::clip_points(clip, &envelope), offset);
            }
            return automation::value_at(automation::clip_points(clip, envelope), offset);
        }
        let destination = Destination::Lane(write.owner, lane.id);
        let merged = self
            .finished
            .get(&destination)
            .map(|curve| curve.merged(&lane.points));
        automation::value_at(merged.as_ref().unwrap_or(&lane.points).as_slice(), tick)
    }

    /// Finishes all active writes at this transport boundary.
    pub fn end_segments(&mut self, project: &Project, tick: Tick) {
        self.end_writes(project, tick, |_| true);
    }

    /// Finishes writes whose mode changed, preserving the captured segments in this pass.
    pub fn close_writes_for_mode_change(
        &mut self,
        project: &Project,
        tick: Tick,
        owner: AutomationOwner,
        lane: Option<LaneId>,
        mode: AutomationMode,
    ) {
        self.end_writes(project, tick, |write| {
            write.owner == owner && lane.is_none_or(|id| write.lane == id) && write.mode != mode
        });
    }

    fn end_writes(
        &mut self,
        project: &Project,
        tick: Tick,
        matches: impl FnMut(&mut Write) -> bool,
    ) {
        for mut write in self.writing.extract_if(.., matches) {
            let Some(mut segment) = write.segment.take() else {
                continue;
            };
            let finish = if write.mode == AutomationMode::Latch {
                if let Some(held) = segment.raw.last().map(|point| point.value) {
                    // The transport can stop beyond a clip edge without delivering a sample there.
                    while segment.end() < tick {
                        let boundary = segment.end();
                        if let Some((destination, points)) =
                            finish_segment(project, segment, &write, boundary, Finish::Boundary)
                        {
                            self.finished.entry(destination).or_default().push(points);
                            self.completed_revision = next_draft_revision();
                        }
                        segment = segment_at(project, &write, boundary);
                        segment.append(boundary, held, write.stepped);
                        segment.boundary_anchor = true;
                    }
                }
                Finish::LatchEnd
            } else {
                Finish::Boundary
            };
            if let Some((destination, points)) =
                finish_segment(project, segment, &write, tick, finish)
            {
                self.finished.entry(destination).or_default().push(points);
                self.completed_revision = next_draft_revision();
            }
        }
    }

    /// Applies the completed segments to the current project. A lane/clip removed during
    /// capture is left removed.
    pub fn apply(&self, project: &Project) -> EditResult {
        let mut next = project.clone();
        for (destination, curve) in &self.finished {
            if let Some(original) = destination.original(project) {
                next = destination.store(&next, curve.merged(original))?;
            }
        }
        Ok(next)
    }

    /// Projects the captured material through this tick without changing the live pass.
    pub fn project_at(
        &self,
        project: &Project,
        tick: Tick,
    ) -> Result<Option<Project>, komponist_model::EditError> {
        if !self.has_points() {
            return Ok(None);
        }
        let mut snapshot = self.clone();
        snapshot.end_segments(project, tick);
        snapshot.apply(project).map(Some)
    }

    /// Whether the pass has sampled any automation values.
    pub fn has_points(&self) -> bool {
        !self.finished.is_empty()
            || self
                .writing
                .iter()
                .any(|w| w.segment.as_ref().is_some_and(|s| !s.raw.is_empty()))
    }
}

impl AutomationDraft for AutomationCapture {
    fn completed_revision(&self) -> u64 {
        self.completed_revision
    }

    fn apply(&self, project: &Project) -> EditResult {
        AutomationCapture::apply(self, project)
    }
}

fn segment_at(project: &Project, write: &Write, tick: Tick) -> Segment {
    let mut origin = Tick::ZERO;
    let mut end = Tick(i64::MAX);
    let mut destination = Destination::Lane(write.owner, write.lane);
    if let AutomationOwner::Track(track) = write.owner
        && let Some(track) = project.track(track)
    {
        for span in automation::clip_override_spans(track, &write.target) {
            if span.end <= tick {
                continue;
            }
            if tick < span.start {
                end = span.start;
                break;
            }
            let length = automation::clip_content_length(span.clip);
            origin = tick - Tick((tick - span.clip.start).0 % length.0);
            end = span.end.min(Tick(origin.0.saturating_add(length.0)));
            destination = Destination::Clip(span.clip.id, write.target.clone());
            break;
        }
    }
    Segment {
        destination,
        origin,
        start: tick - origin,
        limit: end - origin,
        raw: Vec::new(),
        boundary_anchor: false,
        release_anchor: None,
    }
}

fn finish_segment(
    project: &Project,
    mut segment: Segment,
    write: &Write,
    end: Tick,
    finish: Finish,
) -> Option<(Destination, RecordedSegment)> {
    let last = *segment.raw.last()?;
    let end_tick = (end - segment.origin).clamp(last.tick, segment.limit);
    segment.append(end_tick + segment.origin, last.value, write.stepped);
    let keep_anchor = usize::from(segment.boundary_anchor && segment.raw.len() > 1);
    let mut points = Vec::new();
    if keep_anchor != 0 {
        let mut anchor = segment.raw[0];
        if write.stepped {
            anchor.shape = Shape::Hold;
        }
        points.push(anchor);
    }
    let raw = &segment.raw[keep_anchor..];
    if let Some(index) = segment
        .release_anchor
        .and_then(|tick| raw.binary_search_by_key(&tick, |point| point.tick).ok())
    {
        points.extend_from_slice(automation::thin(&raw[..=index], write.stepped).as_slice());
        let tail = automation::thin(&raw[index..], write.stepped);
        points.extend_from_slice(&tail.as_slice()[1..]);
    } else {
        points.extend_from_slice(automation::thin(raw, write.stepped).as_slice());
    }
    let return_tick = match finish {
        Finish::LatchEnd => Tick(end_tick.0.saturating_add(1)),
        Finish::Boundary => end_tick,
    }
    .min(segment.limit);
    if return_tick > end_tick {
        let value = segment
            .destination
            .original(project)
            .and_then(|original| automation::value_at(original.as_slice(), return_tick))
            .unwrap_or(write.base);
        if let Some(last) = points.last_mut() {
            last.shape = Shape::Hold;
        }
        points.push(Point::linear(return_tick, value));
    }
    Some((
        segment.destination,
        RecordedSegment::new(segment.start, Points::new(points), write.base),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::{Document, fixtures};

    #[test]
    fn touch_starts_at_the_first_edit_and_holds_until_release_across_a_clip_boundary() {
        let project = fixtures::demo_project();
        let track = project
            .tracks
            .iter()
            .find(|track| track.name == "Lead")
            .unwrap();
        let track_id = track.id;
        let owner = AutomationOwner::Track(track_id);
        let (clip, edge) = (track.clips[0].id, track.clips[0].start);
        let target = AutomationTarget::Pan;
        let project =
            edits::add_clip_automation(&project, clip, target.clone(), "Pan".into()).unwrap();
        let original = Points::new(vec![Point::linear(Tick::ZERO, 0.3)]);
        let project = edits::set_clip_points(&project, clip, &target, original.clone()).unwrap();
        let lane = edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .id;
        let project = edits::set_lane_points(&project, owner, lane, original.clone()).unwrap();
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Touch).unwrap();

        let mut pass = AutomationCapture::default();
        pass.open(&project, owner, target.clone(), false, 0.3);
        pass.value(&project, owner, &target, edge - Tick(100), 0.8);
        pass.release(&project, owner, &target, edge + Tick(80));
        let next = pass.apply(&project).unwrap();
        let track = next.track(track_id).unwrap();
        assert_eq!(
            automation::effective_value_at(track, &target, edge - Tick(101)),
            Some(0.3)
        );
        assert_eq!(
            automation::effective_value_at(track, &target, edge + Tick(79)),
            Some(0.8)
        );
        assert_eq!(
            automation::effective_value_at(track, &target, edge + Tick(80)),
            Some(0.8)
        );
        assert!(automation::effective_value_at(track, &target, edge + Tick(100)).unwrap() < 0.8);
    }

    #[test]
    fn touch_returns_to_the_completed_capture_from_an_earlier_lap() {
        let project = fixtures::demo_project();
        let track = project
            .tracks
            .iter()
            .find(|track| track.name == "Lead")
            .unwrap();
        let (id, clip) = (track.id, &track.clips[0]);
        let (clip, start) = (clip.id, clip.start);
        let owner = AutomationOwner::Track(id);
        let target = AutomationTarget::Pan;
        let project =
            edits::add_clip_automation(&project, clip, target.clone(), "Pan".into()).unwrap();
        let lane = edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .id;
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Touch).unwrap();
        let original = Points::new(vec![Point::linear(Tick::ZERO, 0.2)]);
        let project = edits::set_clip_points(&project, clip, &target, original).unwrap();
        let release = start + Tick(100);
        let end = release + Tick(project.tempo_map().seconds_to_ticks(0.250).round() as i64);
        let mut pass = AutomationCapture::default();
        pass.open(&project, owner, target.clone(), false, 0.2);
        pass.value(&project, owner, &target, end - Tick(10), 0.8);
        pass.end_segments(&project, end + Tick(10));

        pass.open(&project, owner, target.clone(), false, 0.2);
        pass.value(&project, owner, &target, start + Tick(50), 0.5);
        pass.release(&project, owner, &target, release);
        let result = pass.apply(&project).unwrap();
        let track = result.track(id).unwrap();
        assert_eq!(
            automation::effective_value_at(track, &target, end),
            Some(0.8)
        );
    }

    #[test]
    fn completed_curve_extends_incrementally_and_rebases_after_document_edits() {
        let mut curve = CompletedCurve::default();
        let original = Points::new(vec![Point::linear(Tick::ZERO, 0.2)]);
        for (tick, value) in [(10, 0.4), (20, 0.8)] {
            curve.push(RecordedSegment {
                start: Tick(tick),
                points: Points::new(vec![Point::linear(Tick(tick), value)]),
                base: 0.2,
            });
            assert_eq!(
                automation::value_at(curve.merged(&original).as_slice(), Tick(tick)),
                Some(value)
            );
        }
        let rebased = Points::new(vec![Point::linear(Tick::ZERO, 0.3)]);
        let merged = curve.merged(&rebased);
        assert_eq!(automation::value_at(merged.as_slice(), Tick(1)), Some(0.3));
        assert_eq!(automation::value_at(merged.as_slice(), Tick(10)), Some(0.4));
        assert_eq!(automation::value_at(merged.as_slice(), Tick(20)), Some(0.8));
    }

    #[test]
    fn a_touch_releases_and_a_latch_writes_through_release_in_one_pass() {
        let p = fixtures::demo_project();
        let owner = AutomationOwner::Track(p.tracks[0].id);
        let target = AutomationTarget::Volume;
        let (p, lane) = edits::add_lane(&p, owner, target.clone(), "Volume".into()).unwrap();
        let (p, master) =
            edits::add_lane(&p, AutomationOwner::Master, target.clone(), "Volume".into()).unwrap();
        let p = edits::set_lane_mode(&p, owner, lane, AutomationMode::Touch).unwrap();
        let p = edits::set_lane_mode(&p, AutomationOwner::Master, master, AutomationMode::Latch)
            .unwrap();

        let mut capture = AutomationCapture::default();
        capture.open(&p, owner, target.clone(), false, 0.5);
        capture.open(&p, AutomationOwner::Master, target.clone(), false, 0.7);
        capture.value(&p, owner, &target, Tick(200), 0.2);
        capture.value(&p, AutomationOwner::Master, &target, Tick(200), 0.3);
        capture.release(&p, owner, &target, Tick(210));
        capture.release(&p, AutomationOwner::Master, &target, Tick(210));
        assert!(!capture.is_writing(owner, lane));
        assert!(capture.is_writing(AutomationOwner::Master, master));
        assert!(
            capture
                .preview_lane(owner, lane, &Points::default())
                .is_some()
        );
        capture.end_segments(&p, Tick(800));

        let mut doc = Document::new(p.clone());
        doc.apply("Capture pass", |project| capture.apply(project))
            .unwrap();
        let touch = edits::lanes(doc.project(), owner)
            .unwrap()
            .lane(&target)
            .unwrap();
        let returned = (210.0 + p.tempo_map().seconds_to_ticks(0.25).round()) as i64;
        assert_eq!(
            automation::value_at(touch.points.as_slice(), Tick(returned)),
            Some(0.5)
        );
        let latch = doc.project().master_automation.lane(&target).unwrap();
        assert_eq!(
            automation::value_at(latch.points.as_slice(), Tick(799)),
            Some(0.3)
        );
        assert_eq!(
            automation::value_at(latch.points.as_slice(), Tick(801)),
            Some(0.7)
        );
        assert!(doc.undo());
        assert_eq!(doc.project().as_ref(), &p);
        assert!(doc.undo_label().is_none());
    }

    #[test]
    fn latch_changes_source_at_exact_clip_boundaries() {
        let project = fixtures::demo_project();
        let track = project
            .tracks
            .iter()
            .find(|track| track.name == "Lead")
            .unwrap();
        let owner = AutomationOwner::Track(track.id);
        let clip = &track.clips[0];
        let (clip_id, start, end) = (clip.id, clip.start, clip.end());
        let target = AutomationTarget::Volume;
        let project =
            edits::add_clip_automation(&project, clip_id, target.clone(), "Volume".into()).unwrap();
        let project = edits::set_clip_points(
            &project,
            clip_id,
            &target,
            Points::new(vec![
                Point::linear(Tick::ZERO, 0.6),
                Point::linear(end - start, 0.7),
            ]),
        )
        .unwrap();
        let lane = edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .id;
        let project = edits::set_lane_points(
            &project,
            owner,
            lane,
            Points::new(vec![
                Point {
                    tick: Tick::ZERO,
                    value: 0.5,
                    shape: Shape::Curve { tension: 0.6 },
                },
                Point::linear(end + Tick(400), 0.7),
            ]),
        )
        .unwrap();
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();
        let original = &edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .points;
        let mut pass = AutomationCapture::default();
        pass.open(&project, owner, target.clone(), false, 0.65);
        pass.value(&project, owner, &target, start - Tick(90), 0.2);
        pass.value(&project, owner, &target, start + Tick(8), 0.4);
        pass.value(&project, owner, &target, end + Tick(12), 0.45);
        pass.end_segments(&project, end + Tick(20));
        let written = pass.apply(&project).unwrap();
        let lane = edits::lanes(&written, owner)
            .unwrap()
            .lane(&target)
            .unwrap();
        for tick in [start - Tick(500), start + Tick(100), end + Tick(100)] {
            let expected = automation::value_at(original.as_slice(), tick).unwrap();
            let actual = automation::value_at(lane.points.as_slice(), tick).unwrap();
            assert!(
                (expected - actual).abs() < 1e-6,
                "{tick:?}: {expected} vs {actual}"
            );
        }
        let envelope = &written.find_clip(clip_id).unwrap().1.automation[0];
        assert_eq!(
            automation::value_at(envelope.points.as_slice(), Tick(100)),
            Some(0.4)
        );
    }

    #[test]
    fn stopping_a_latch_after_a_clip_edge_carries_the_held_value_into_the_lane() {
        let project = fixtures::demo_project();
        let track = project
            .tracks
            .iter()
            .find(|track| track.name == "Lead")
            .unwrap();
        let owner = AutomationOwner::Track(track.id);
        let clip = &track.clips[0];
        let (clip_id, start, end) = (clip.id, clip.start, clip.end());
        let target = AutomationTarget::Pan;
        let project =
            edits::add_clip_automation(&project, clip_id, target.clone(), "Pan".into()).unwrap();
        let project = edits::set_clip_points(
            &project,
            clip_id,
            &target,
            Points::new(vec![Point::linear(Tick::ZERO, 0.3)]),
        )
        .unwrap();
        let lane = edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .id;
        let original = Points::new(vec![Point::linear(Tick::ZERO, 0.3)]);
        let project = edits::set_lane_points(&project, owner, lane, original.clone()).unwrap();
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();

        let finish_at = |stop| {
            let mut pass = AutomationCapture::default();
            pass.open(&project, owner, target.clone(), false, 0.3);
            pass.value(&project, owner, &target, end - Tick(10), 0.8);
            pass.end_segments(&project, stop);
            pass.apply(&project).unwrap()
        };
        let at_edge = finish_at(end);
        let lane_points = |project: &Project| {
            edits::lanes(project, owner)
                .unwrap()
                .lane(&target)
                .unwrap()
                .points
                .clone()
        };
        assert_eq!(lane_points(&at_edge), original);

        let stop = end + Tick(20);
        let continued = finish_at(stop);
        let envelope = &continued.find_clip(clip_id).unwrap().1.automation[0];
        assert_eq!(
            automation::value_at(envelope.points.as_slice(), end - start - Tick(1)),
            Some(0.8),
        );
        let points = lane_points(&continued);
        for tick in [end, end + Tick(10), stop] {
            assert_eq!(
                automation::value_at(points.as_slice(), tick),
                Some(0.8),
                "{tick:?}"
            );
        }
        assert_eq!(
            automation::value_at(points.as_slice(), stop + Tick(1)),
            Some(0.3)
        );
    }

    #[test]
    fn repeating_a_latch_beyond_the_document_limit_keeps_a_valid_range() {
        let owner = AutomationOwner::Master;
        let target = AutomationTarget::Pan;
        let (project, lane) = edits::add_lane(
            &Project::new("Capture"),
            owner,
            target.clone(),
            "Pan".into(),
        )
        .unwrap();
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();
        let mut pass = AutomationCapture::default();
        for (offset, value) in [(10, 0.2), (30, 0.8)] {
            let start = MAX_DOC_TICK + Tick(offset);
            pass.open(&project, owner, target.clone(), false, 0.5);
            pass.value(&project, owner, &target, start, value);
            pass.end_segments(&project, start + Tick(10));
            assert_eq!(
                pass.preview_lane_ranges(owner, lane),
                [(MAX_DOC_TICK, MAX_DOC_TICK)]
            );
        }
        let applied = pass.apply(&project).unwrap();
        let points = &edits::lanes(&applied, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .points;
        assert_eq!(points.as_slice().last().unwrap().tick, MAX_DOC_TICK);
    }
}
