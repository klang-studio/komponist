//! The UI-side automation take. Engine events carry the same clock used for playback.
//! Captured edits stay in a preview until the pass can be applied as one document edit.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};

use komponist_engine::automation::{AutomationBoundary, EngineTarget};
use komponist_engine::{Command, EngineEvent, TransportState};
use komponist_model::automation::{
    self, AutomationMode, AutomationOwner, AutomationTarget, LaneId, Point, Points, Shape,
};
use komponist_model::edits::{EditResult, automation as edits};
use komponist_model::params::ParamId;
use komponist_model::{ClipId, DeviceId, Project, Tick};

use crate::app::App;

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
    /// Engine writes started by a native edit that was consumed for macro learning.
    learned: BTreeSet<(DeviceId, u32)>,
    finished: HashMap<Destination, CompletedCurve>,
    completed: usize,
    published: Option<(usize, u64)>,
}

impl AutomationCapture {
    pub(crate) fn active(&self) -> bool {
        !(self.writing.is_empty() && self.finished.is_empty())
    }

    /// Whether a target's lane is currently writing.
    pub fn is_writing(&self, owner: AutomationOwner, lane: LaneId) -> bool {
        self.writing
            .iter()
            .any(|w| w.owner == owner && w.lane == lane)
    }

    pub(crate) fn owner_writing(&self, owner: AutomationOwner) -> bool {
        self.writing.iter().any(|write| write.owner == owner)
    }

    pub(crate) fn plugin_parameter_writing(&self, device: DeviceId, id: u32) -> bool {
        self.writing.iter().any(|write| {
            matches!(&write.target, AutomationTarget::Param { device: d, param }
                if *d == device && param.plugin_id() == Some(id))
        })
    }

    pub(crate) fn current_value(
        &self,
        owner: AutomationOwner,
        target: &AutomationTarget,
    ) -> Option<f32> {
        self.writing
            .iter()
            .find(|w| w.owner == owner && &w.target == target)?
            .segment
            .as_ref()?
            .raw
            .last()
            .map(|point| point.value)
    }

    pub(crate) fn preview_ranges(
        &self,
        source: &crate::views::automation::Source,
    ) -> Vec<(Tick, Tick)> {
        use crate::views::automation::Source;
        let destination = match source {
            Source::Lane(owner, lane) => Destination::Lane(*owner, *lane),
            Source::Clip(clip, target) => Destination::Clip(*clip, target.clone()),
        };
        self.finished
            .get(&destination)
            .into_iter()
            .flat_map(|curve| curve.ranges.iter().copied())
            .chain(self.writing.iter().filter_map(|write| {
                let segment = write
                    .segment
                    .as_ref()
                    .filter(|segment| segment.destination == destination)?;
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
                let preview = RecordedSegment {
                    start: segment.start,
                    points: Points::new(segment.raw.clone()),
                    base: write.base,
                };
                result = Some(preview.replace(result.as_ref().unwrap_or(original)));
            }
        }
        result
    }

    pub(crate) fn references_lane(&self, owner: AutomationOwner, lane: LaneId) -> bool {
        self.is_writing(owner, lane) || self.finished.contains_key(&Destination::Lane(owner, lane))
    }

    pub(crate) fn was_published(&self) -> bool {
        self.published.is_some()
    }

    pub(crate) fn merge(&mut self, other: Self) {
        debug_assert!(other.writing.is_empty());
        for (destination, curve) in other.finished {
            let current = self.finished.entry(destination).or_default();
            for segment in curve.segments {
                current.push(segment);
            }
        }
        self.completed += other.completed;
        self.published = other.published.or(self.published);
    }

    pub(crate) fn current_clip_value(
        &self,
        clip: ClipId,
        target: &AutomationTarget,
    ) -> Option<f32> {
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

    fn open(
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

    fn value(
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
                self.completed += 1;
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

    fn release(
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
                    self.completed += 1;
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
                    self.completed += 1;
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

    pub(crate) fn end_segments(&mut self, project: &Project, tick: Tick) {
        self.end_writes(project, tick, |_| true);
    }

    fn end_writes(
        &mut self,
        project: &Project,
        tick: Tick,
        matches: impl FnMut(&mut Write) -> bool,
    ) {
        for mut write in self.writing.extract_if(.., matches) {
            let finish = if write.mode == AutomationMode::Latch {
                Finish::LatchEnd
            } else {
                Finish::Boundary
            };
            if let Some(segment) = write.segment.take()
                && let Some(done) = finish_segment(project, segment, &write, tick, finish)
            {
                let (destination, points) = done;
                self.finished.entry(destination).or_default().push(points);
                self.completed += 1;
            }
        }
    }

    /// Applies the completed segments to the current project. A lane/clip removed during
    /// capture is left removed.
    pub(crate) fn apply(&self, project: &Project) -> EditResult {
        let mut next = project.clone();
        for (destination, curve) in &self.finished {
            if let Some(original) = destination.original(project) {
                next = destination.store(&next, curve.merged(original))?;
            }
        }
        Ok(next)
    }

    pub(crate) fn project_at(
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

    pub(crate) fn has_points(&self) -> bool {
        !self.finished.is_empty()
            || self
                .writing
                .iter()
                .any(|w| w.segment.as_ref().is_some_and(|s| !s.raw.is_empty()))
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
        RecordedSegment {
            start: segment.start,
            points: Points::new(points),
            base: write.base,
        },
    ))
}

impl App {
    /// Closes a live write at the mode change while keeping it in the current undoable pass.
    pub(crate) fn close_writes_for_mode_change(
        &mut self,
        owner: AutomationOwner,
        lane: Option<LaneId>,
        mode: AutomationMode,
    ) {
        self.drain_events();
        let tick = self.engine.playhead();
        let project = self.project().clone();
        self.automation_capture.end_writes(&project, tick, |write| {
            write.owner == owner && lane.is_none_or(|id| write.lane == id) && write.mode != mode
        });
    }

    pub(crate) fn capture_native_automation(
        &mut self,
        device: DeviceId,
        id: u32,
        owner: AutomationOwner,
        target: EngineTarget,
        tick: Tick,
        learned: bool,
    ) {
        if learned {
            // Successful mappings already removed this lane from the compiled program.
            // A rejected mapping can still produce samples and loop restarts.
            if self
                .engine
                .program()
                .is_some_and(|program| program.automation_model_target(owner, target).is_some())
            {
                self.automation_capture.learned.insert((device, id));
            }
        } else {
            self.automation_capture.learned.remove(&(device, id));
            self.capture_automation(&EngineEvent::AutomationTouch {
                owner,
                target,
                begin: true,
                tick,
            });
        }
    }

    pub(crate) fn capture_automation(&mut self, event: &EngineEvent) {
        let (owner, target, tick) = match *event {
            EngineEvent::AutomationTouch {
                owner,
                target,
                tick,
                ..
            }
            | EngineEvent::AutomationValue {
                owner,
                target,
                tick,
                ..
            } => (owner, target, tick),
            EngineEvent::AutomationBoundary { tick, kind } => {
                let project = self.project().clone();
                self.automation_capture.end_segments(&project, tick);
                if kind != AutomationBoundary::LoopWrap {
                    self.automation_capture.learned.clear();
                }
                if kind == AutomationBoundary::Stop
                    && std::mem::take(&mut self.automation_stop_pending)
                {
                    self.finish_take();
                } else if matches!(kind, AutomationBoundary::Stop | AutomationBoundary::Locate)
                    && self.recording.is_none()
                    && !self.audio_recording.is_active()
                {
                    self.commit_automation_capture();
                }
                return;
            }
            _ => return,
        };
        let Some(model) = self
            .engine
            .program()
            .and_then(|p| p.automation_model_target(owner, target))
            .cloned()
        else {
            return;
        };
        if let AutomationTarget::Param { device, param } = &model
            && let Some(id) = param.plugin_id()
        {
            let key = (*device, id);
            if matches!(event, EngineEvent::AutomationTouch { begin: false, .. }) {
                self.automation_capture.learned.remove(&key);
            } else if self.automation_capture.learned.contains(&key) {
                // Ignore samples and automatic loop restarts for this intercepted write.
                return;
            }
        }
        let project = self.project().clone();
        match *event {
            EngineEvent::AutomationTouch { begin: true, .. } => {
                let stepped = self.automation_stepped(target);
                let base = self.automation_base(owner, &model);
                self.automation_capture
                    .open(&project, owner, model, stepped, base);
            }
            EngineEvent::AutomationTouch { begin: false, .. } => {
                self.automation_capture
                    .release(&project, owner, &model, tick);
            }
            EngineEvent::AutomationValue { value, .. } => {
                self.automation_capture
                    .value(&project, owner, &model, tick, value);
            }
            _ => {}
        }
    }

    fn automation_stepped(&self, target: EngineTarget) -> bool {
        let EngineTarget::Slot { slot, wire_id } = target else {
            return false;
        };
        let Some(device) = self.engine.program().and_then(|p| p.slots.device(slot)) else {
            return false;
        };
        let Some((_, _, model)) = self.project().find_device(device) else {
            return false;
        };
        if crate::device_host::is_hexa(model) {
            return komponist_builtin::device::params()
                .iter()
                .find(|p| p.wire == wire_id)
                .is_some_and(|p| p.info.is_stepped());
        }
        if let Some(kind) = crate::device_host::internal_kind(model) {
            return kind
                .params()
                .get(wire_id as usize)
                .is_some_and(|p| p.is_stepped());
        }
        self.engine
            .host()
            .params(device)
            .and_then(|params| params.iter().find(|p| p.id == wire_id))
            .is_some_and(|p| p.step_count != 0)
    }

    fn automation_base(&self, owner: AutomationOwner, target: &AutomationTarget) -> f32 {
        self.engine
            .program()
            .and_then(|p| p.automation_base(owner, target))
            .unwrap_or_default()
    }

    pub(crate) fn take_automation_capture(&mut self) -> AutomationCapture {
        let project = self.project().clone();
        self.automation_capture
            .end_segments(&project, self.engine.playhead());
        std::mem::take(&mut self.automation_capture)
    }

    /// Gives playback the completed portions of a pass so later laps play the new curve.
    pub(crate) fn sync_automation_preview(&mut self) {
        let completed = self.automation_capture.completed;
        let last_program = self.engine.programs_sent();
        if completed == 0
            || self.automation_capture.published == Some((completed, last_program))
            || self.engine.is_detached()
        {
            return;
        }
        if let Ok(draft) = self.automation_capture.apply(self.project()) {
            self.engine.send_project(&draft);
            self.automation_capture.published = Some((completed, self.engine.programs_sent()));
        }
    }

    pub(crate) fn commit_automation_capture(&mut self) {
        let pass = self.take_automation_capture();
        if pass.has_points() {
            self.end_gesture();
            self.apply_with("undo-automation-record", |project| {
                pass.apply(project).map(|p| (p, ()))
            });
        }
    }

    pub(crate) fn discard_automation_capture(&mut self) {
        if self.automation_capture.was_published()
            || self.audio_recording.pending_automation_published()
        {
            self.engine.send_project(self.doc.project());
        }
        self.automation_capture = AutomationCapture::default();
        self.automation_controls.clear();
        self.automation_stop_pending = false;
        self.engine.send(Command::CancelAutomationWrite);
    }

    pub(crate) fn device_automation_target(
        &self,
        device: DeviceId,
        id: u32,
    ) -> Option<(AutomationOwner, AutomationTarget)> {
        let (owner, _, d) = self.project().find_device(device)?;
        let param = if crate::device_host::is_hexa(d) {
            ParamId::new(komponist_builtin::device::Field::from_wire(id)?.key())
        } else if let Some(kind) = crate::device_host::internal_kind(d) {
            kind.params().get(id as usize)?.id.clone()
        } else {
            ParamId::plugin(id)
        };
        Some((owner.into(), AutomationTarget::Param { device, param }))
    }

    /// Returns true when the automation engine owns this control's temporary edit.
    pub(crate) fn manual_automation(
        &mut self,
        owner: AutomationOwner,
        target: &AutomationTarget,
        command: Command,
    ) -> bool {
        let Some(lane) = edits::lanes(self.project(), owner)
            .ok()
            .and_then(|a| a.lane(target))
        else {
            return false;
        };
        if lane.mode == AutomationMode::Off {
            return false;
        }
        let playing = matches!(
            self.transport(),
            TransportState::Playing | TransportState::Recording
        );
        let Some(program) = self.engine.program() else {
            return false;
        };
        if program.automation_mode(owner, target) != Some(lane.mode) {
            return false;
        }
        if !program.automation_reads(owner, target)
            && !(playing && matches!(lane.mode, AutomationMode::Touch | AutomationMode::Latch))
        {
            return false;
        }
        let Some(target) = program.automation_target(owner, target) else {
            return false;
        };
        let temporary = lane.mode == AutomationMode::Read || !playing;
        if temporary && let Some(hint) = self.lang.by_id("automation-override-hint") {
            self.set_status(hint, crate::ui_state::StatusKind::Info);
        }
        if let Command::SetParam { slot, id, value } = command
            && let Some(device) = self.engine.program().and_then(|p| p.slots.device(slot))
        {
            self.engine.host_mut().preview_param(device, id, value);
        }
        let gesture = self.doc.in_gesture();
        if !self.automation_controls.contains(&(owner, target)) {
            self.engine.send(Command::Touch {
                owner,
                target,
                begin: true,
            });
            if gesture {
                self.automation_controls.push((owner, target));
            }
        }
        self.engine.send(command);
        if !gesture {
            self.engine.send(Command::Touch {
                owner,
                target,
                begin: false,
            });
        }
        true
    }

    pub(crate) fn release_automation_controls(&mut self) {
        for (owner, target) in self.automation_controls.drain(..) {
            self.engine.send(Command::Touch {
                owner,
                target,
                begin: false,
            });
        }
    }
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
    fn changing_mode_during_latch_closes_the_write_at_that_tick() {
        use crate::actions::{Action, EditAction, TransportAction};
        use crate::views::automation::Action as Automation;
        use komponist_model::edits::ChannelProp;

        for bulk in [false, true] {
            let project = Project::new("Capture");
            let owner = AutomationOwner::Master;
            let target = AutomationTarget::Pan;
            let (project, lane) =
                edits::add_lane(&project, owner, target.clone(), "Pan".into()).unwrap();
            let project =
                edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();
            let mut app = App::headless(project);
            app.engine.hold_engine().unwrap();
            let run = |app: &mut App, frames| {
                assert!(app.engine.run_held(frames, &[], 0));
                app.step();
            };
            app.dispatch(Action::Transport(TransportAction::TogglePlay));
            run(&mut app, 1024);
            app.dispatch(Action::Edit(EditAction::SetMasterProp(ChannelProp::Pan(
                0.5,
            ))));
            run(&mut app, 1024);
            assert!(app.automation_capture.is_writing(owner, lane));

            let end = app.engine.playhead();
            let action = if bulk {
                Automation::AllModes(owner, AutomationMode::Off)
            } else {
                Automation::Mode(owner, lane, AutomationMode::Off)
            };
            app.dispatch(Action::Automation(action));
            assert!(!app.automation_capture.is_writing(owner, lane));
            app.step();
            run(&mut app, 2048);
            assert!(app.engine.playhead() > end + Tick(1));
            app.dispatch(Action::Transport(TransportAction::Stop));
            run(&mut app, 256);

            let lane = app.project().master_automation.lane(&target).unwrap();
            assert_eq!(lane.mode, AutomationMode::Off);
            assert_eq!(
                automation::value_at(lane.points.as_slice(), end),
                Some(0.75)
            );
            assert_eq!(
                automation::value_at(lane.points.as_slice(), end + Tick(2)),
                Some(0.5)
            );
            assert_eq!(app.doc.undo_label(), Some("undo-automation-record"));
        }
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
    fn master_fader_latch_commits_at_engine_stop_without_changing_the_base() {
        use crate::actions::{Action, EditAction, TransportAction};
        use komponist_model::edits::ChannelProp;

        let p = Project::new("Capture");
        let owner = AutomationOwner::Master;
        let target = AutomationTarget::Volume;
        let (p, id) = edits::add_lane(&p, owner, target.clone(), "Volume".into()).unwrap();
        let p = edits::set_lane_mode(&p, owner, id, AutomationMode::Latch).unwrap();
        let master_gain = p.master.volume_db;
        let mut app = App::headless(p.clone());
        app.engine.hold_engine().unwrap();
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
        assert!(app.engine.run_held(1024, &[], 0));
        app.step();
        app.dispatch(Action::Edit(EditAction::BeginGesture {
            label: "Volume".into(),
        }));
        app.dispatch(Action::Edit(EditAction::SetMasterProp(
            ChannelProp::VolumeDb(-16.0),
        )));
        assert!(app.engine.run_held(1024, &[], 0));
        app.step();
        app.dispatch(Action::Edit(EditAction::EndGesture));
        assert!(app.engine.run_held(1024, &[], 0));
        app.step();
        assert!(app.automation_capture.is_writing(owner, id));
        assert_eq!(app.project().master.volume_db, master_gain);
        assert!(app.doc.undo_label().is_none());
        app.dispatch(Action::Transport(TransportAction::Stop));
        assert!(app.engine.run_held(256, &[], 0));
        app.step();
        let lane = app.project().master_automation.lane(&target).unwrap();
        assert!(lane.points.as_slice().iter().any(|point| {
            (point.value - (1.0 - komponist_model::params::db_to_p(-16.0))).abs() < 1e-5
        }));
        assert_eq!(app.doc.undo_label(), Some("undo-automation-record"));
        assert!(app.doc.undo());
        assert_eq!(app.project().as_ref(), &p);
        assert!(app.doc.undo_label().is_none());
    }

    #[test]
    fn looped_clip_write_can_be_discarded_and_then_undone_in_one_step() {
        use crate::actions::{Action, EditAction, TransportAction};
        use komponist_model::edits::{ChannelProp, TrackProp};

        let mut project = fixtures::demo_project();
        project.loop_on = false;
        let track = project
            .tracks
            .iter()
            .find(|track| track.name == "Lead")
            .unwrap();
        let (track, clip, start) = (track.id, track.clips[0].id, track.clips[0].start);
        let owner = AutomationOwner::Track(track);
        let target = AutomationTarget::Pan;
        let period = Tick(480);
        let project =
            komponist_model::edits::set_clip_loop(&project, clip, Some(period), Tick(1440))
                .unwrap();
        let project =
            edits::add_clip_automation(&project, clip, target.clone(), "Pan".into()).unwrap();
        let project = edits::set_clip_points(
            &project,
            clip,
            &target,
            Points::new(vec![Point::linear(Tick::ZERO, 0.5)]),
        )
        .unwrap();
        let lane = edits::lanes(&project, owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .id;
        let project = edits::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();

        let mut app = App::headless(project.clone());
        app.engine.hold_engine().unwrap();
        let advance = |app: &mut App, frames| {
            assert!(app.engine.run_held(frames, &[], 0));
            app.step();
        };
        let transport = |app: &mut App, action| app.dispatch(Action::Transport(action));
        let write = |app: &mut App, pan| {
            transport(app, TransportAction::Locate(start + Tick(32)));
            advance(app, 64);
            if app.transport() == TransportState::Stopped {
                transport(app, TransportAction::TogglePlay);
                advance(app, 64);
            }
            app.dispatch(Action::Edit(EditAction::BeginGesture {
                label: "Pan".into(),
            }));
            app.dispatch(Action::Edit(EditAction::SetTrackProp {
                tracks: vec![track],
                prop: TrackProp::Channel(ChannelProp::Pan(pan)),
            }));
            advance(app, 1024);
            app.dispatch(Action::Edit(EditAction::EndGesture));
            advance(app, 1024);
            assert!(app.automation_capture.is_writing(owner, lane));
        };
        write(&mut app, -0.6);
        assert!(app.automation_capture.has_points());
        // Locate has reached the engine but the app has not drained its boundary yet.
        transport(&mut app, TransportAction::Locate(start + Tick(100)));
        assert!(app.engine.run_held(64, &[], 0));
        transport(&mut app, TransportAction::DiscardTake);
        advance(&mut app, 64);
        assert!(!app.automation_capture.active());
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.doc.undo_label().is_none());

        // A published preview can also be owned by the audio writer while it drains.
        write(&mut app, -0.6);
        let playhead = app.engine.playhead();
        app.automation_capture.end_segments(&project, playhead);
        app.sync_automation_preview();
        let deferred = app.take_automation_capture();
        assert!(deferred.was_published());
        app.audio_recording.defer_automation(deferred);
        assert!(app.audio_recording.pending_automation_published());
        let published_program = app.engine.programs_sent();
        transport(&mut app, TransportAction::DiscardTake);
        assert!(app.engine.programs_sent() > published_program);
        assert!(!app.audio_recording.pending_automation_published());
        advance(&mut app, 64);
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.doc.undo_label().is_none());

        write(&mut app, 0.6);
        transport(&mut app, TransportAction::Stop);
        advance(&mut app, 256);
        let track_after = app.project().track(track).unwrap();
        assert!(
            track_after
                .automation
                .lane(&target)
                .unwrap()
                .points
                .is_empty()
        );
        let tick = start + Tick(56);
        let first = automation::effective_value_at(track_after, &target, tick).unwrap();
        assert!((first - 0.8).abs() < 1e-6, "captured pan {first}");
        assert_eq!(
            automation::effective_value_at(track_after, &target, tick + period),
            Some(first)
        );
        assert_eq!(app.doc.undo_label(), Some("undo-automation-record"));
        app.dispatch(Action::Edit(EditAction::Undo));
        assert_eq!(app.project().as_ref(), &project);
        assert!(app.doc.undo_label().is_none());
    }
}
