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
use komponist_model::{Clip, ClipId, Project, Tick, TrackId, routing};
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

/// What a row of the arrangement is (wave 9, routing proposal 05-arrangement).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A MIDI track (clips; a member of a group when its `depth` is above 0).
    Track,
    /// A group: a bus shown as a folder, with its members behind it.
    Group {
        /// Its members are folded away.
        folded: bool,
    },
    /// An effect bus, in the „Busse“ section below „Neue Spur“.
    EffectBus,
    /// A track inside a folded group: no height, nothing drawn.
    Hidden,
}

/// The rows of `project` in the order of its tracks: height, kind and depth (how many groups the
/// row is inside). Effect buses leave the flow of the tracks and go into the „Busse“ section.
fn plan(project: &Project, v_zoom: f32) -> Vec<(f32, RowKind, u8)> {
    project
        .tracks
        .iter()
        .map(|t| {
            let groups = routing::group_ancestors(project, t.id);
            let depth = groups.len().min(u8::MAX as usize) as u8;
            let hidden = groups
                .iter()
                .any(|g| project.track(*g).is_some_and(|g| g.folded));
            if hidden {
                (0.0, RowKind::Hidden, depth)
            } else if t.is_group() {
                let h = if t.folded {
                    metrics::GROUP_ROW_FOLDED
                } else {
                    metrics::GROUP_ROW
                };
                (h, RowKind::Group { folded: t.folded }, depth)
            } else if t.is_bus() {
                (metrics::BUS_ROW, RowKind::EffectBus, 0)
            } else {
                (row_height(t.height, v_zoom), RowKind::Track, depth)
            }
        })
        .collect()
}

/// Height of everything below the ruler: the rows, the room kept for "Neue Spur" and, with effect
/// buses, the „Busse“ section.
pub fn content_height(project: &Project, v_zoom: f32) -> f32 {
    let rows = plan(project, v_zoom);
    let main: f32 = rows
        .iter()
        .filter(|(_, k, _)| *k != RowKind::EffectBus)
        .map(|(h, _, _)| h)
        .sum();
    let buses: f32 = rows
        .iter()
        .filter(|(_, k, _)| *k == RowKind::EffectBus)
        .map(|(h, _, _)| h)
        .sum();
    let section = if buses > 0.0 {
        metrics::SECTION_LABEL + buses
    } else {
        0.0
    };
    main + metrics::NEW_TRACK_MIN_HEIGHT + section
}

/// One track row on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    /// The track.
    pub track: TrackId,
    /// Top and bottom on screen (the bottom includes the 1 px separator).
    pub y: Rangef,
    /// What the row is.
    pub kind: RowKind,
    /// How many groups the row is inside (0 at the top level): its header is indented that many
    /// times.
    pub depth: u8,
}

impl Row {
    /// Whether the row is drawn at all (a folded group's members are not).
    pub fn shown(&self) -> bool {
        self.kind != RowKind::Hidden
    }

    /// Whether clips can lie on the row: tracks only (a bus holds none).
    pub fn takes_clips(&self) -> bool {
        self.kind == RowKind::Track
    }

    /// Whether the row is part of the flow of tracks (not hidden, not in the „Busse“ section): the
    /// rows a header drag can be inserted between.
    pub fn in_flow(&self) -> bool {
        !matches!(self.kind, RowKind::Hidden | RowKind::EffectBus)
    }
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
    /// Screen y below the last row of the flow of tracks (top of "Neue Spur").
    pub rows_end: f32,
    /// The label row of the „Busse“ section, when the project has effect buses.
    pub section: Option<Rect>,
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
        let plan = plan(project, view.v_zoom);
        let mut y = tracks.top() - view.scroll_y;
        let mut rows: Vec<Row> = Vec::with_capacity(plan.len());
        for (t, &(h, kind, depth)) in project.tracks.iter().zip(&plan) {
            if kind == RowKind::EffectBus {
                // Placed below "Neue Spur" once the flow of tracks is laid out.
                rows.push(Row {
                    track: t.id,
                    y: Rangef::new(0.0, 0.0),
                    kind,
                    depth,
                });
                continue;
            }
            rows.push(Row {
                track: t.id,
                y: Rangef::new(y, y + h),
                kind,
                depth,
            });
            y += h;
        }
        let rows_end = y;
        let mut section = None;
        if rows.iter().any(|r| r.kind == RowKind::EffectBus) {
            let mut by = rows_end + metrics::NEW_TRACK_MIN_HEIGHT;
            section = Some(Rect::from_x_y_ranges(
                Rangef::new(body.left(), body.right()),
                Rangef::new(by, by + metrics::SECTION_LABEL),
            ));
            by += metrics::SECTION_LABEL;
            for (row, &(h, kind, _)) in rows.iter_mut().zip(&plan) {
                if kind == RowKind::EffectBus {
                    row.y = Rangef::new(by, by + h);
                    by += h;
                }
            }
        }
        Geometry {
            body,
            count,
            ruler,
            tracks,
            header_x,
            lane_x,
            axis,
            rows,
            rows_end,
            section,
        }
    }

    /// The header of row `i`, indented for the members of a group.
    pub fn header(&self, i: usize) -> Rect {
        let indent = f32::from(self.rows[i].depth) * metrics::MEMBER_INDENT;
        Rect::from_x_y_ranges(
            Rangef::new(self.header_x.min + indent, self.header_x.max),
            self.rows[i].y,
        )
    }

    /// The whole header column of row `i`, indent included (where a click selects the track).
    pub fn header_full(&self, i: usize) -> Rect {
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

    /// The "Neue Spur" area below the rows, down to the bottom (empty when the rows fill the view);
    /// with effect buses it is as high as the room kept for it, and the „Busse“ section follows.
    pub fn new_track(&self) -> Rect {
        let top = self.rows_end.clamp(self.tracks.top(), self.tracks.bottom());
        let bottom = match self.section {
            Some(s) => s.top().clamp(top, self.tracks.bottom()),
            None => self.tracks.bottom(),
        };
        Rect::from_min_max(
            pos2(self.tracks.left(), top),
            pos2(self.tracks.right(), bottom),
        )
    }

    /// Whether row `i` is shown and at least partly visible.
    pub fn row_visible(&self, i: usize) -> bool {
        let r = &self.rows[i];
        r.shown() && r.y.max > self.tracks.top() && r.y.min < self.tracks.bottom()
    }

    /// Indices of the visible rows.
    pub fn visible_rows(&self) -> impl Iterator<Item = usize> + '_ {
        (0..self.rows.len()).filter(|&i| self.row_visible(i))
    }

    /// The row under screen `y`, if any.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        self.rows.iter().position(|r| r.y.min <= y && y < r.y.max)
    }

    /// The row a clip dragged to `y` lands on: the row under `y` if it takes clips, else the clip row
    /// nearest to `y` (a bus under the pointer is refused, so the clip stays on the nearest track).
    pub fn nearest_row(&self, y: f32) -> Option<usize> {
        if let Some(i) = self.row_at(y).filter(|&i| self.rows[i].takes_clips()) {
            return Some(i);
        }
        let distance = |r: &Row| {
            if y < r.y.min {
                r.y.min - y
            } else {
                y - r.y.max
            }
        };
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.takes_clips())
            .min_by(|(_, a), (_, b)| distance(a).total_cmp(&distance(b)))
            .map(|(i, _)| i)
    }

    /// Whether `y` is over a bus row (a group or an effect bus), where a clip is refused.
    pub fn bus_row_at(&self, y: f32) -> Option<usize> {
        self.row_at(y).filter(|&i| {
            matches!(
                self.rows[i].kind,
                RowKind::Group { .. } | RowKind::EffectBus
            )
        })
    }

    /// The insert position (0 ..= track count) for a track dragged to `y`: before the first row of
    /// the flow whose middle lies below `y`, else behind the last row of the flow.
    pub fn insert_slot(&self, y: f32) -> usize {
        self.rows
            .iter()
            .position(|r| r.in_flow() && y < r.y.center())
            .unwrap_or_else(|| self.flow_end())
    }

    /// Index behind the last row of the flow of tracks (before the effect buses of the end).
    fn flow_end(&self) -> usize {
        self.rows
            .iter()
            .rposition(|r| r.kind != RowKind::EffectBus)
            .map_or(0, |i| i + 1)
    }

    /// Screen y of insert position `slot` (the top of the next row of the flow, or the bottom of
    /// the last).
    pub fn slot_y(&self, slot: usize) -> f32 {
        self.rows
            .iter()
            .skip(slot)
            .find(|r| r.in_flow())
            .map_or(self.rows_end, |r| r.y.min)
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
            if !row.takes_clips() || row.y.max <= area.top() || row.y.min >= area.bottom() {
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
