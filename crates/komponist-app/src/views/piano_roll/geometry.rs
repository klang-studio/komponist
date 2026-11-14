//! Where everything of the piano roll lies: the rects of toolbar, ruler, keyboard, note grid and
//! velocity lane inside the dock body, the time axis and the key rows. Pure math, no egui state.
//!
//! The time axis is in **absolute** ticks (timeline), so the ruler shows the bar numbers of the
//! song ("9, 10, 11, 12" in S01) and AccessKit names carry absolute positions; notes are
//! clip-local and are placed at `clip.start + note.start`. Key rows run from
//! [`PianoRollView::top_key`] at the top edge downwards, one [`PianoRollView::key_height`] each.

use egui::{Pos2, Rect, pos2};
use komponist_model::{Meter, Tick};
use komponist_tokens::size;
use komponist_ui::timeline::TimeAxis;

use super::metrics as m;
use crate::ui_state::PianoRollView;

/// The rects and mappings of one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    /// The toolbar row (with its bottom line).
    pub toolbar: Rect,
    /// The empty corner left of the ruler, above the keyboard.
    pub corner: Rect,
    /// The editor ruler (with its bottom line).
    pub ruler: Rect,
    /// The keyboard column (with its 1 px right line).
    pub keys: Rect,
    /// The note grid.
    pub grid: Rect,
    /// The velocity lane's label column, if the lane is shown.
    pub lane_label: Option<Rect>,
    /// The velocity lane (with its 1 px top line), if shown.
    pub lane: Option<Rect>,
    /// Absolute tick ↔ x for ruler, grid and lane.
    pub axis: TimeAxis,
    /// Height of a key row.
    pub key_height: f32,
    /// Key at the top edge.
    pub top_key: u8,
    /// Start of the open clip on the timeline (notes are relative to it).
    pub clip_start: Tick,
}

impl Geometry {
    /// The layout of `body` (the dock below its tab strip) for `view` and a clip at `clip_start`.
    pub fn new(body: Rect, view: &PianoRollView, clip_start: Tick, meter: Meter) -> Geometry {
        let (left, right) = (body.left(), body.right());
        let split = left + size::KEYBOARD;
        let toolbar = Rect::from_min_max(
            body.min,
            pos2(right, (body.top() + m::TOOLBAR_HEIGHT).min(body.bottom())),
        );
        let ruler_bottom = (toolbar.bottom() + m::RULER_HEIGHT).min(body.bottom());
        let lane_top = if view.velocity_lane {
            (body.bottom() - size::VELOCITY_LANE).max(ruler_bottom)
        } else {
            body.bottom()
        };
        let corner = Rect::from_min_max(pos2(left, toolbar.bottom()), pos2(split, ruler_bottom));
        let ruler = Rect::from_min_max(pos2(split, toolbar.bottom()), pos2(right, ruler_bottom));
        let keys = Rect::from_min_max(pos2(left, ruler_bottom), pos2(split, lane_top));
        let grid = Rect::from_min_max(pos2(split, ruler_bottom), pos2(right, lane_top));
        let (lane_label, lane) = if view.velocity_lane {
            (
                Some(Rect::from_min_max(
                    pos2(left, lane_top),
                    pos2(split, body.bottom()),
                )),
                Some(Rect::from_min_max(
                    pos2(split, lane_top),
                    pos2(right, body.bottom()),
                )),
            )
        } else {
            (None, None)
        };
        let px_per_tick = view.px_per_bar / meter.ticks_per_bar() as f32;
        let left_tick = clip_start.0 as f64 + view.scroll_tick;
        let axis = TimeAxis {
            x0: grid.left() - (left_tick * f64::from(px_per_tick)) as f32,
            px_per_tick,
        };
        Geometry {
            toolbar,
            corner,
            ruler,
            keys,
            grid,
            lane_label,
            lane,
            axis,
            key_height: view.key_height,
            top_key: view.top_key,
            clip_start,
        }
    }

    /// Number of key rows the grid shows (the last one may be cut).
    pub fn rows(&self) -> usize {
        (self.grid.height() / self.key_height).ceil().max(0.0) as usize
    }

    /// Number of key rows that fit completely.
    pub fn full_rows(&self) -> usize {
        (self.grid.height() / self.key_height).floor().max(1.0) as usize
    }

    /// The lowest key with a (partly) visible row.
    pub fn bottom_key(&self) -> u8 {
        let rows = self.rows().max(1) as i32;
        (i32::from(self.top_key) - rows + 1).clamp(0, 127) as u8
    }

    /// Top edge of the row of `pitch`.
    pub fn row_top(&self, pitch: u8) -> f32 {
        self.grid.top() + f32::from(self.top_key as i16 - pitch as i16) * self.key_height
    }

    /// The row rect of `pitch` across the grid.
    pub fn row(&self, pitch: u8) -> Rect {
        let top = self.row_top(pitch);
        Rect::from_min_max(
            pos2(self.grid.left(), top),
            pos2(self.grid.right(), top + self.key_height),
        )
    }

    /// The key whose row contains `y` (rows above key 127 or below key 0 have none).
    pub fn pitch_at(&self, y: f32) -> Option<u8> {
        let row = ((y - self.grid.top()) / self.key_height).floor() as i32;
        let pitch = i32::from(self.top_key) - row;
        (0..=127).contains(&pitch).then_some(pitch as u8)
    }

    /// The key at `y`, clamped into the keyboard's range (for drags that leave the grid).
    pub fn pitch_at_clamped(&self, y: f32) -> u8 {
        let row = ((y - self.grid.top()) / self.key_height).floor() as i32;
        (i32::from(self.top_key) - row).clamp(0, 127) as u8
    }

    /// The absolute tick at screen x.
    pub fn tick_at(&self, x: f32) -> f64 {
        self.axis.tick_at(x)
    }

    /// The clip-local tick at screen x.
    pub fn local_tick_at(&self, x: f32) -> f64 {
        self.axis.tick_at(x) - self.clip_start.0 as f64
    }

    /// Screen x of the clip-local tick `local`.
    pub fn x_of_local(&self, local: Tick) -> f32 {
        self.axis.x(self.clip_start + local)
    }

    /// The clip-local ticks visible across the grid, widened to whole ticks.
    pub fn visible_local_ticks(&self) -> (Tick, Tick) {
        let (t0, t1) = self.axis.ticks_in(self.grid.x_range());
        (t0 - self.clip_start, t1 - self.clip_start)
    }

    /// The rect of a note (clip-local `start`, `length`, `pitch`), as the design system draws it.
    pub fn note_rect(&self, start: Tick, length: Tick, pitch: u8) -> Rect {
        komponist_ui::timeline::note_rect(
            &self.axis,
            self.clip_start + start,
            length,
            self.row_top(pitch),
            self.key_height,
        )
    }

    /// The pointer position as (clip-local tick, key row offset in rows from the grid top).
    pub fn local_pos(&self, p: Pos2) -> (f64, f32) {
        (
            self.local_tick_at(p.x),
            (p.y - self.grid.top()) / self.key_height,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn view() -> PianoRollView {
        PianoRollView {
            px_per_bar: 320.0,
            top_key: 74,
            key_height: 12.0,
            ..PianoRollView::default()
        }
    }

    fn body() -> Rect {
        // The S01 dock body at the default dock height: 1378 × 292 below the tab strip.
        Rect::from_min_size(pos2(261.0, 765.0), vec2(1378.0, 292.0))
    }

    #[test]
    fn layout_of_the_dock_body() {
        let g = Geometry::new(body(), &view(), Tick(8 * 3840), Meter::FOUR_FOUR);
        assert_eq!(g.toolbar.height(), 28.0);
        assert_eq!(g.ruler.top(), 793.0);
        assert_eq!(g.ruler.height(), 18.0);
        assert_eq!(g.keys.width(), 52.0);
        assert_eq!(g.grid.left(), 261.0 + 52.0);
        assert_eq!(g.grid.top(), 811.0);
        let lane = g.lane.unwrap();
        assert_eq!(lane.height(), 64.0);
        assert_eq!(g.grid.bottom(), lane.top());
        assert_eq!(g.grid.height(), 292.0 - 28.0 - 18.0 - 64.0);
        assert_eq!(g.lane_label.unwrap().width(), 52.0);
        // Without the lane the grid reaches the bottom.
        let v = PianoRollView {
            velocity_lane: false,
            ..view()
        };
        let g2 = Geometry::new(body(), &v, Tick::ZERO, Meter::FOUR_FOUR);
        assert_eq!(g2.grid.bottom(), body().bottom());
        assert!(g2.lane.is_none());
    }

    #[test]
    fn axis_starts_at_the_clip() {
        let g = Geometry::new(body(), &view(), Tick(8 * 3840), Meter::FOUR_FOUR);
        assert_eq!(g.axis.x(Tick(8 * 3840)), g.grid.left());
        assert_eq!(g.x_of_local(Tick(3840)), g.grid.left() + 320.0);
        assert!((g.local_tick_at(g.grid.left() + 20.0) - 240.0).abs() < 1e-3);
        let scrolled = PianoRollView {
            scroll_tick: 3840.0,
            ..view()
        };
        let g = Geometry::new(body(), &scrolled, Tick(8 * 3840), Meter::FOUR_FOUR);
        assert_eq!(g.x_of_local(Tick(3840)), g.grid.left());
        let (t0, _) = g.visible_local_ticks();
        assert!((t0 - Tick(3840)).0.abs() <= 1, "{t0:?}");
    }

    #[test]
    fn key_rows() {
        let g = Geometry::new(body(), &view(), Tick::ZERO, Meter::FOUR_FOUR);
        assert_eq!(g.row_top(74), g.grid.top());
        assert_eq!(g.row_top(72), g.grid.top() + 24.0);
        assert_eq!(g.pitch_at(g.grid.top() + 0.5), Some(74));
        assert_eq!(g.pitch_at(g.grid.top() + 12.0), Some(73));
        assert_eq!(g.pitch_at(g.grid.top() - 1.0), Some(75));
        // 182 px at 12 per key: 15 full rows, 16 started.
        assert_eq!(g.full_rows(), 15);
        assert_eq!(g.rows(), 16);
        assert_eq!(g.bottom_key(), 59);
        let r = g.note_rect(Tick(240), Tick(480), 72);
        assert_eq!(r.min, pos2(g.grid.left() + 21.0, g.grid.top() + 25.0));
        assert_eq!(r.size(), vec2(38.0, 10.0));
        // Above 127 / below 0 there is no key.
        let top = Geometry::new(
            body(),
            &PianoRollView {
                top_key: 127,
                ..view()
            },
            Tick::ZERO,
            Meter::FOUR_FOUR,
        );
        assert_eq!(top.pitch_at(top.grid.top() - 1.0), None);
        assert_eq!(top.pitch_at_clamped(top.grid.top() - 100.0), 127);
    }
}
