//! Where everything of the arrangement lies, and what is under a point: pure rect math over the
//! project and the view state, tested without a frame.
//!
//! The view (`Screens.dc.html` `arrangement()` markup): a ruler row (`size.ruler` 22 + a 1 px
//! `surface.0` line), the "6 Spuren" box over the 224 px header column; below it one row per
//! track (header left, lane right), scrolled by [`ArrangementView::scroll_y`], then the "Neue
//! Spur" area down to the bottom. The lanes map ticks to x with a [`TimeAxis`] scrolled by
//! [`ArrangementView::scroll_tick`].

use egui::{Pos2, Rangef, Rect, pos2};
use komponist_model::project::{TRACK_HEIGHT_MAX, TRACK_HEIGHT_MIN};
use komponist_model::{Clip, ClipId, Project, Tick, TrackId};
use komponist_tokens::{line, size};
use komponist_ui::timeline::{TimeAxis, clip_rect};
use komponist_ui::widgets::metrics::timeline as tm;

use super::metrics;
use crate::ui_state::ArrangementView;

/// Displayed height of a track of stored `height` at vertical zoom `v_zoom`, in whole points,
/// within `size.track.min ..= size.track.max`.
pub fn row_height(height: u16, v_zoom: f32) -> f32 {
    (f32::from(height) * v_zoom)
        .round()
        .clamp(f32::from(TRACK_HEIGHT_MIN), f32::from(TRACK_HEIGHT_MAX))
}

/// The stored height for a displayed height `display` at `v_zoom` (height drag), clamped to the
/// model's range.
pub fn stored_height(display: f32, v_zoom: f32) -> u16 {
    let h = (display / v_zoom.max(f32::EPSILON)).round();
    h.clamp(f32::from(TRACK_HEIGHT_MIN), f32::from(TRACK_HEIGHT_MAX)) as u16
}

/// Height of everything below the ruler: the rows and the room kept for "Neue Spur".
pub fn content_height(project: &Project, v_zoom: f32) -> f32 {
    let rows: f32 = project
        .tracks
        .iter()
        .map(|t| row_height(t.height, v_zoom))
        .sum();
    rows + metrics::NEW_TRACK_MIN_HEIGHT
}

/// One track row on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    /// The track.
    pub track: TrackId,
    /// Top and bottom on screen (the bottom includes the 1 px separator).
    pub y: Rangef,
}

/// Which part of a clip a point is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Zone {
    /// Body and head: select, move, open.
    Body,
    /// Right end: drag the length (`ix.clip` "Kante ziehen").
    End,
    /// Top right corner of the head: drag loop repetitions (`ix.clip` "Ecke oben rechts").
    Loop,
}

/// A clip under a point.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipHit {
    /// Index of its track (= row).
    pub row: usize,
    /// The clip.
    pub clip: ClipId,
    /// Its rect on screen.
    pub rect: Rect,
    /// The part under the point.
    pub zone: Zone,
}

/// The part of a clip at `rect` under `pos`: the loop corner (top right of the head,
/// `CLIP_LOOP_HANDLE_WIDTH` × `size.clip-header`), the end (the last [`metrics::CLIP_EDGE_GRAB`]
/// px), else the body. On narrow clips both handles shrink to a third of the width, so the body
/// stays grabbable.
pub fn zone(rect: Rect, pos: Pos2) -> Zone {
    let third = rect.width() / 3.0;
    let loop_w = tm::CLIP_LOOP_HANDLE_WIDTH.min(third);
    let edge_w = metrics::CLIP_EDGE_GRAB.min(third);
    if pos.x >= rect.right() - loop_w && pos.y < rect.top() + size::CLIP_HEADER {
        Zone::Loop
    } else if pos.x >= rect.right() - edge_w {
        Zone::End
    } else {
        Zone::Body
    }
}

/// The arrangement's layout for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// The whole view: the panel below its header.
    pub body: Rect,
    /// Left part of the ruler row: the track count.
    pub count: Rect,
    /// The ruler over the lanes.
    pub ruler: Rect,
    /// Headers, lanes and "Neue Spur" below the ruler's bottom line (clip rect of the rows).
    pub tracks: Rect,
    /// The header column.
    pub header_x: Rangef,
    /// The lanes.
    pub lane_x: Rangef,
    /// Tick ↔ x of the lanes and the ruler.
    pub axis: TimeAxis,
    /// One row per track in display order, scrolled (also the invisible ones).
    pub rows: Vec<Row>,
    /// Screen y below the last row (top of "Neue Spur").
    pub rows_end: f32,
}

impl Geometry {
    /// The layout of `project` in `body` with the view state `view`.
    pub fn new(body: Rect, project: &Project, view: &ArrangementView) -> Geometry {
        let header_x = Rangef::new(body.left(), body.left() + size::TRACK_HEADER);
        let lane_x = Rangef::new(header_x.max, body.right().max(header_x.max));
        let ruler_y = Rangef::new(body.top(), body.top() + size::RULER);
        let count = Rect::from_x_y_ranges(header_x, ruler_y);
        let ruler = Rect::from_x_y_ranges(lane_x, ruler_y);
        let tracks = Rect::from_min_max(
            pos2(body.left(), ruler_y.max + line::WIDTH),
            pos2(body.right(), body.bottom().max(ruler_y.max + line::WIDTH)),
        );
        let px_per_tick = view.px_per_bar / project.meter.ticks_per_bar() as f32;
        let axis = TimeAxis {
            x0: lane_x.min - (view.scroll_tick * f64::from(px_per_tick)) as f32,
            px_per_tick,
        };
        let mut y = tracks.top() - view.scroll_y;
        let rows = project
            .tracks
            .iter()
            .map(|t| {
                let h = row_height(t.height, view.v_zoom);
                let row = Row {
                    track: t.id,
                    y: Rangef::new(y, y + h),
                };
                y += h;
                row
            })
            .collect();
        Geometry {
            body,
            count,
            ruler,
            tracks,
            header_x,
            lane_x,
            axis,
            rows,
            rows_end: y,
        }
    }

    /// The header of row `i`.
    pub fn header(&self, i: usize) -> Rect {
        Rect::from_x_y_ranges(self.header_x, self.rows[i].y)
    }

    /// The lane of row `i`.
    pub fn lane(&self, i: usize) -> Rect {
        Rect::from_x_y_ranges(self.lane_x, self.rows[i].y)
    }

    /// The lanes area (rows and "Neue Spur", right of the headers).
    pub fn lanes(&self) -> Rect {
        Rect::from_x_y_ranges(self.lane_x, self.tracks.y_range())
    }

    /// The "Neue Spur" area below the rows, down to the bottom (empty when the rows fill the view).
    pub fn new_track(&self) -> Rect {
        let top = self.rows_end.clamp(self.tracks.top(), self.tracks.bottom());
        Rect::from_min_max(pos2(self.tracks.left(), top), self.tracks.max)
    }

    /// Whether row `i` is at least partly visible.
    pub fn row_visible(&self, i: usize) -> bool {
        let y = self.rows[i].y;
        y.max > self.tracks.top() && y.min < self.tracks.bottom()
    }

    /// Indices of the visible rows.
    pub fn visible_rows(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&i| self.row_visible(i))
    }

    /// The row under screen `y`, if any.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        self.rows.iter().position(|r| r.y.min <= y && y < r.y.max)
    }

    /// The row under `y`, or the first / last row when `y` lies above / below them.
    pub fn nearest_row(&self, y: f32) -> Option<usize> {
        let last = self.rows.len().checked_sub(1)?;
        if y < self.rows[0].y.min {
            Some(0)
        } else {
            Some(self.row_at(y).unwrap_or(last))
        }
    }

    /// The insert position (0 ..= track count) for a track dragged to `y`: before the first row
    /// whose middle lies below `y`.
    pub fn insert_slot(&self, y: f32) -> usize {
        self.rows
            .iter()
            .position(|r| y < r.y.center())
            .unwrap_or(self.rows.len())
    }

    /// Screen y of insert position `slot` (the top of that row, or the bottom of the last).
    pub fn slot_y(&self, slot: usize) -> f32 {
        match self.rows.get(slot) {
            Some(r) => r.y.min,
            None => self.rows_end,
        }
    }

    /// The rect of `clip` in row `i` (`clip_rect`: 1 px in from both ends, 3 px from top and
    /// bottom).
    pub fn clip_rect(&self, i: usize, clip: &Clip) -> Rect {
        clip_rect(
            self.lane(i),
            self.axis.x(clip.start),
            self.axis.x(clip.end()),
        )
    }

    /// The ticks visible in the lanes.
    pub fn visible_ticks(&self) -> (Tick, Tick) {
        self.axis.ticks_in(self.lane_x)
    }

    /// The tick at screen `x` (fractional).
    pub fn tick_at(&self, x: f32) -> f64 {
        self.axis.tick_at(x)
    }

    /// The topmost clip under `pos` (a later clip in a track lies on top), with the part hit.
    pub fn clip_at(&self, project: &Project, pos: Pos2) -> Option<ClipHit> {
        if !self.lane_x.contains(pos.x) || !self.tracks.contains(pos) {
            return None;
        }
        let row = self.row_at(pos.y)?;
        let track = project.tracks.get(row)?;
        track.clips.iter().rev().find_map(|c| {
            let rect = self.clip_rect(row, c);
            rect.contains(pos).then(|| ClipHit {
                row,
                clip: c.id,
                rect,
                zone: zone(rect, pos),
            })
        })
    }

    /// The clips whose rects touch `area` (box selection), in display order.
    pub fn clips_in(&self, project: &Project, area: Rect) -> Vec<ClipId> {
        let mut out = Vec::new();
        for (i, track) in project.tracks.iter().enumerate() {
            let Some(row) = self.rows.get(i) else { break };
            if row.y.max <= area.top() || row.y.min >= area.bottom() {
                continue;
            }
            out.extend(
                track
                    .clips
                    .iter()
                    .filter(|c| self.clip_rect(i, c).intersects(area))
                    .map(|c| c.id),
            );
        }
        out
    }

    /// Largest vertical scroll for `project` at `v_zoom` in this view.
    pub fn max_scroll_y(&self, project: &Project, v_zoom: f32) -> f32 {
        (content_height(project, v_zoom) - self.tracks.height()).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;
    use komponist_model::fixtures::demo_project;

    fn view() -> ArrangementView {
        ArrangementView::default()
    }

    fn body() -> Rect {
        Rect::from_min_size(pos2(261.0, 73.0), vec2(1378.0, 663.0))
    }

    #[test]
    fn layout_follows_the_screen() {
        let p = demo_project();
        let g = Geometry::new(body(), &p, &view());
        assert_eq!(g.count.width(), 224.0);
        assert_eq!(g.ruler.left(), 261.0 + 224.0);
        assert_eq!(g.ruler.height(), 22.0);
        // The ruler's bottom line, then the first row.
        assert_eq!(g.tracks.top(), 73.0 + 23.0);
        assert_eq!(g.rows.len(), 6);
        assert_eq!(g.rows[0].y, Rangef::new(96.0, 152.0));
        assert_eq!(g.rows[5].y.max, 96.0 + 6.0 * 56.0);
        assert_eq!(g.rows_end, 96.0 + 6.0 * 56.0);
        // 72 px per bar from the lanes' left edge.
        assert_eq!(g.axis.x(Tick(8 * 3840)), 485.0 + 8.0 * 72.0);
        // Lead A (bar 9–10) on Lead.
        let lead_a = &p.tracks[2].clips[0];
        let r = g.clip_rect(2, lead_a);
        assert_eq!(
            r,
            Rect::from_min_size(
                pos2(485.0 + 576.0 + 1.0, 96.0 + 112.0 + 3.0),
                vec2(142.0, 50.0)
            )
        );
    }

    #[test]
    fn heights_follow_the_vertical_zoom() {
        assert_eq!(row_height(56, 1.0), 56.0);
        assert_eq!(row_height(56, 2.0), 112.0);
        assert_eq!(row_height(56, 3.0), 160.0);
        assert_eq!(row_height(40, 0.5), 32.0);
        assert_eq!(stored_height(112.0, 2.0), 56);
        assert_eq!(stored_height(500.0, 1.0), 160);
        assert_eq!(stored_height(10.0, 1.0), 32);
    }

    #[test]
    fn hits_find_clips_and_their_parts() {
        let p = demo_project();
        let g = Geometry::new(body(), &p, &view());
        let lead_a = &p.tracks[2].clips[0];
        let r = g.clip_rect(2, lead_a);
        let hit = g.clip_at(&p, r.center()).unwrap();
        assert_eq!((hit.row, hit.clip, hit.zone), (2, lead_a.id, Zone::Body));
        let end = g
            .clip_at(&p, pos2(r.right() - 2.0, r.bottom() - 4.0))
            .unwrap();
        assert_eq!(end.zone, Zone::End);
        let corner = g.clip_at(&p, pos2(r.right() - 2.0, r.top() + 4.0)).unwrap();
        assert_eq!(corner.zone, Zone::Loop);
        // Lead has no clip in bars 1–8.
        assert!(g.clip_at(&p, pos2(500.0, r.center().y)).is_none());
        // Headers are no clip area.
        assert!(g.clip_at(&p, pos2(300.0, r.center().y)).is_none());
    }

    #[test]
    fn rows_slots_and_boxes() {
        let p = demo_project();
        let g = Geometry::new(body(), &p, &view());
        assert_eq!(g.row_at(100.0), Some(0));
        assert_eq!(g.row_at(2000.0), None);
        assert_eq!(g.nearest_row(10.0), Some(0));
        assert_eq!(g.nearest_row(2000.0), Some(5));
        assert_eq!(g.insert_slot(96.0 + 20.0), 0);
        assert_eq!(g.insert_slot(96.0 + 40.0), 1);
        assert_eq!(g.insert_slot(1000.0), 6);
        assert_eq!(g.slot_y(1), 152.0);
        assert_eq!(g.slot_y(6), g.rows_end);
        // A box over bars 9–10 of Drums and Bass.
        let area = Rect::from_min_max(
            pos2(g.axis.x(Tick(8 * 3840)) + 5.0, 100.0),
            pos2(g.axis.x(Tick(9 * 3840)), 200.0),
        );
        let ids = g.clips_in(&p, area);
        assert_eq!(ids, vec![p.tracks[0].clips[2].id, p.tracks[1].clips[1].id]);
    }

    #[test]
    fn narrow_clips_keep_a_body() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(12.0, 50.0));
        assert_eq!(zone(r, pos2(3.0, 30.0)), Zone::Body);
        assert_eq!(zone(r, pos2(9.0, 30.0)), Zone::End);
        assert_eq!(zone(r, pos2(9.0, 4.0)), Zone::Loop);
    }

    #[test]
    fn scrolling_moves_rows_and_axis() {
        let p = demo_project();
        let mut v = view();
        v.scroll_y = 56.0;
        v.scroll_tick = 3840.0 * 2.0;
        let g = Geometry::new(body(), &p, &v);
        assert_eq!(g.rows[1].y.min, 96.0);
        assert!(!g.row_visible(0));
        assert_eq!(g.axis.x(Tick(2 * 3840)), 485.0);
        assert_eq!(g.max_scroll_y(&p, 1.0), 0.0);
    }
}
