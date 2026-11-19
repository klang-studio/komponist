//! A timeline point sequence whose loop bodies are stored only once.

use super::*;

/// The effective automation curve when materializing all loop repetitions would be too large.
#[derive(Debug)]
pub struct RepeatingPoints {
    runs: Vec<Run>,
    len: u128,
}

#[derive(Debug)]
struct Run {
    start: u128,
    end: u128,
    first: Tick,
    period: Tick,
    points: Vec<Point>,
}

impl Run {
    fn tick_at(&self, index: u128, tick: Tick) -> Tick {
        let repetition = (index - self.start) / self.points.len() as u128;
        self.first + Tick(self.period.0 * repetition as i64) + tick
    }

    fn get(&self, index: u128) -> Point {
        let mut point = self.points[((index - self.start) % self.points.len() as u128) as usize];
        point.tick = self.tick_at(index, point.tick);
        point
    }
}

impl RepeatingPoints {
    fn push(&mut self, points: Vec<Point>, first: Tick, period: Tick, count: u64) {
        if points.is_empty() || count == 0 {
            return;
        }
        let end = self.len + points.len() as u128 * u128::from(count);
        self.runs.push(Run {
            start: self.len,
            end,
            first,
            period,
            points,
        });
        self.len = end;
    }

    fn push_once(&mut self, points: Vec<Point>) {
        self.push(points, Tick::ZERO, Tick::ZERO, 1);
    }

    fn push_slice(&mut self, points: &[Point], start: Tick, end: Tick, shift: Tick) {
        let mut slice = Vec::new();
        append_slice(&mut slice, points, start, end, Tick::ZERO);
        self.push(slice, shift, Tick::ZERO, 1);
    }

    fn push_clip(&mut self, span: ClipOverrideSpan<'_>) {
        let points = clip_points(span.clip, span.envelope);
        if let Some(value) = constant_value(points) {
            self.push_once(vec![
                Point::linear(span.start, value),
                Point::linear(span.end, value),
            ]);
            return;
        }

        let period = clip_content_length(span.clip);
        let first = Tick((span.start - span.clip.start).0.rem_euclid(period.0));
        let mut start = span.start;
        if first != Tick::ZERO {
            let length = (period - first).min(span.end - start);
            self.push_slice(points, first, first + length, start - first);
            start += length;
        }
        let full = (span.end - start).0 / period.0;
        if full > 0 {
            let mut body = Vec::new();
            append_slice(&mut body, points, Tick::ZERO, period, Tick::ZERO);
            self.push(body, start, period, full as u64);
            start += Tick(full * period.0);
        }
        if start < span.end {
            self.push_slice(points, Tick::ZERO, span.end - start, start);
        }
    }

    /// First virtual index whose tick does not satisfy a monotonic predicate.
    pub fn partition_point(&self, mut before: impl FnMut(Tick) -> bool) -> u128 {
        let run = self
            .runs
            .partition_point(|run| before(run.get(run.end - 1).tick));
        let Some(run) = self.runs.get(run) else {
            return self.len;
        };
        let size = run.points.len() as u128;
        let last_tick = run.points.last().unwrap().tick;
        let mut lo = 0;
        let mut hi = (run.end - run.start) / size;
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if before(run.tick_at(run.start + mid * size, last_tick)) {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        let first = run.start + lo * size;
        first
            + run
                .points
                .partition_point(|point| before(run.tick_at(first, point.tick)))
                as u128
    }

    /// Point at a virtual index, in timeline ticks.
    pub fn get(&self, index: u128) -> Option<Point> {
        let run = self.runs.partition_point(|run| run.end <= index);
        Some(self.runs.get(run)?.get(index))
    }

    /// The right-hand value at a fractional timeline position.
    pub fn value(&self, tick: f64) -> Option<f32> {
        let right = self.partition_point(|point| point.0 as f64 <= tick);
        let left = if right == 0 {
            return self.get(0).map(|point| point.value);
        } else {
            self.get(right - 1)?
        };
        Some(self.get(right).map_or(left.value, |next| {
            interpolate(left, next, tick - left.tick.0 as f64)
        }))
    }

    /// First matching point, when the predicate depends on a point and its immediate neighbors.
    /// The pattern within intermediate repetitions is identical, including at loop boundaries.
    pub fn first_matching(
        &self,
        start: u128,
        mut matches: impl FnMut(u128, Point) -> bool,
    ) -> Option<Tick> {
        for run in &self.runs[self.runs.partition_point(|run| run.end <= start)..] {
            let start = start.max(run.start);
            let size = run.points.len() as u128;
            let repetitions = (run.end - run.start) / size;
            let through = ((start - run.start) / size + 2).min(repetitions);
            let end = run.start + through * size;
            // Inspect the last repetition as well because it borders a different run.
            for index in (start..end).chain((run.end - size).max(end)..run.end) {
                let point = run.get(index);
                if matches(index, point) {
                    return Some(point.tick);
                }
            }
        }
        None
    }

    /// Points for the visible timeline. At zoom levels with many points per pixel, sample
    /// the curve over the visible range instead of expanding every repetition.
    pub fn visible_points(&self, left: Tick, right: Tick, limit: usize) -> Vec<Point> {
        let start = self.partition_point(|tick| tick < left).saturating_sub(1);
        let end = (self.partition_point(|tick| tick <= right) + 1).min(self.len);
        if end - start <= limit as u128 {
            return (start..end).filter_map(|index| self.get(index)).collect();
        }
        let steps = limit.max(2) - 1;
        (0..=steps)
            .filter_map(|i| {
                let tick = Tick(
                    left.0
                        + (((i128::from(right.0) - i128::from(left.0)) * i as i128) / steps as i128)
                            as i64,
                );
                self.value(tick.0 as f64)
                    .map(|value| Point::linear(tick, value))
            })
            .collect()
    }
}

/// Returns a compact curve if the lane could otherwise expand beyond the point budget.
pub fn effective_repeating_points(
    track: &Track,
    lane: &Lane,
    base: f32,
) -> Option<RepeatingPoints> {
    if lane.mode == AutomationMode::Off
        || clip_expansion_points(track, &lane.target) <= AUTOMATION_EXPANSION_LIMIT
    {
        return None;
    }
    let spans = clip_override_spans(track, &lane.target);
    let fallback = [Point::linear(Tick::ZERO, normalized(base))];
    let points = if lane.points.is_empty() {
        &fallback
    } else {
        lane.points.as_slice()
    };
    let mut cursor = points[0].tick.min(spans[0].start);
    let mut curve = RepeatingPoints {
        runs: Vec::new(),
        len: 0,
    };
    curve.push_once(vec![Point::linear(cursor, points[0].value)]);
    for span in spans {
        if cursor < span.start {
            curve.push_slice(points, cursor, span.start, Tick::ZERO);
        }
        cursor = span.end;
        curve.push_clip(span);
    }
    let last = *points.last().unwrap();
    if cursor < last.tick {
        curve.push_slice(points, cursor, last.tick, Tick::ZERO);
        curve.push_once(vec![last]);
    } else {
        curve.push_once(vec![Point::linear(
            cursor,
            value_at(points, cursor).unwrap_or(base),
        )]);
    }
    Some(curve)
}
