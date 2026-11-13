//! Vertical grid lines of the timeline canvases: bars, beats and the snap grid.
//!
//! Weights follow the design system's line tokens: bar lines `line.2` ("Taktlinien"), beat lines
//! `line.1` ("Schlaglinien"), grid lines between beats a fainter line (`#232427` in the piano roll
//! of the screens, no token yet). A level is left out when its lines would be closer than
//! `space.2`; bar lines then thin out to every 2nd, 4th, … bar.

use egui::{Color32, Painter, Rangef, Rect, pos2};
use komponist_model::{Grid, Meter, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, space};

use super::TimeAxis;
use super::mesh::Quads;
use crate::px;

/// How strong a grid line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LineWeight {
    /// Bar line: `line.2`.
    Bar,
    /// Beat line: `line.1`.
    Beat,
    /// Grid line between beats: faint.
    Sub,
}

impl LineWeight {
    /// The colour of the line.
    ///
    /// [`LineWeight::Sub`]: contract change pending (`line.0` = #232427 from the piano roll of
    /// S01); until then `surface.3`, the closest token.
    pub fn color(self) -> Color32 {
        color32(match self {
            Self::Bar => color::LINE_2,
            Self::Beat => color::LINE_1,
            Self::Sub => color::SURFACE_3,
        })
    }
}

/// Which lines a canvas shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSpec {
    /// Weight of the bar lines.
    pub bar: LineWeight,
    /// Weight of the beat lines; `None`: no beat lines.
    pub beat: Option<LineWeight>,
    /// Spacing and weight of the lines between beats; `None`: none.
    pub sub: Option<(Tick, LineWeight)>,
}

impl GridSpec {
    /// The arrangement lanes: bar lines only.
    pub const fn arrangement() -> Self {
        Self {
            bar: LineWeight::Bar,
            beat: None,
            sub: None,
        }
    }

    /// The piano roll at snap grid `grid`: bars, beats and lines at the grid, at least every 1/16
    /// (`prView`: finer grids keep the 1/16 lines).
    pub fn editor(grid: Grid, meter: Meter) -> Self {
        let g = grid.ticks(meter).max(Grid::Sixteenth.ticks(meter));
        Self {
            bar: LineWeight::Bar,
            beat: Some(LineWeight::Beat),
            sub: Some((g, LineWeight::Sub)),
        }
    }
}

/// The x positions and weights of the lines of `spec` inside the screen range `x`, left to right.
/// A line on a bar is a bar line, on a beat a beat line; levels closer than `space.2` are left
/// out, and bar lines thin out to every 2ⁿ-th bar.
pub fn grid_lines(
    axis: &TimeAxis,
    meter: Meter,
    x: Rangef,
    spec: &GridSpec,
) -> Vec<(f32, LineWeight)> {
    let min = space::SPACE_2;
    let per_bar = meter.ticks_per_bar();
    let per_beat = meter.ticks_per_beat();
    let bar_px = axis.width(Tick(per_bar));
    if bar_px <= 0.0 || !bar_px.is_finite() {
        return Vec::new();
    }
    let mut bar_step = 1_i64;
    while (bar_step as f32) * bar_px < min {
        bar_step *= 2;
    }
    let beat = spec
        .beat
        .filter(|_| bar_step == 1 && axis.width(Tick(per_beat)) >= min);
    let sub = spec
        .sub
        .filter(|(t, _)| t.0 > 0 && bar_step == 1 && axis.width(*t) >= min);
    let (t0, t1) = axis.ticks_in(x);
    // Candidate lines of every level; where levels meet, the stronger weight wins (bar, beat,
    // grid). Triplet grids need not divide the beat, so the levels are collected separately.
    let mut lines: Vec<(i64, u8, LineWeight)> = Vec::new();
    let mut level = |step: i64, rank: u8, weight: LineWeight| {
        let mut t = t0.0.div_euclid(step).max(0) * step;
        while t <= t1.0 {
            lines.push((t, rank, weight));
            t += step;
        }
    };
    level(per_bar * bar_step, 0, spec.bar);
    if let Some(w) = beat {
        level(per_beat, 1, w);
    }
    if let Some((t, w)) = sub {
        level(t.0, 2, w);
    }
    lines.sort_unstable_by_key(|&(t, rank, _)| (t, rank));
    lines.dedup_by_key(|l| l.0);
    lines
        .into_iter()
        .map(|(t, _, w)| (axis.x(Tick(t)), w))
        .filter(|(lx, _)| x.contains(*lx))
        .collect()
}

/// Paints `lines` (from [`grid_lines`]) as crisp 1 px vertical lines over the height of `rect`.
pub fn paint_grid(painter: &Painter, rect: Rect, lines: &[(f32, LineWeight)]) {
    let ppp = painter.pixels_per_point();
    let w = px::line_width(ppp);
    let (y0, y1) = (px::snap(rect.min.y, ppp), px::snap(rect.max.y, ppp));
    let mut q = Quads::new(ppp);
    q.reserve(lines.len());
    for &(x, weight) in lines {
        let x = px::snap(x, ppp);
        q.quad(
            Rect::from_min_max(pos2(x, y0), pos2(x + w, y1)),
            weight.color(),
        );
    }
    q.paint(painter);
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: Meter = Meter::FOUR_FOUR;

    #[test]
    fn arrangement_shows_bars() {
        let axis = TimeAxis::per_bar(0.0, 72.0, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 300.0), &GridSpec::arrangement());
        let xs: Vec<f32> = lines.iter().map(|l| l.0.round()).collect();
        assert_eq!(xs, [0.0, 72.0, 144.0, 216.0, 288.0]);
        assert!(lines.iter().all(|l| l.1 == LineWeight::Bar));
    }

    #[test]
    fn bars_thin_out_when_zoomed_out() {
        let axis = TimeAxis::per_bar(0.0, 3.0, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 100.0), &GridSpec::arrangement());
        let xs: Vec<f32> = lines.iter().map(|l| l.0.round()).collect();
        assert_eq!(xs, [0.0, 12.0, 24.0, 36.0, 48.0, 60.0, 72.0, 84.0, 96.0]);
    }

    #[test]
    fn editor_shows_bars_beats_and_grid() {
        // 20 px per 1/16 (S01 piano roll).
        let axis = TimeAxis::per_step(0.0, 20.0);
        let spec = GridSpec::editor(Grid::Sixteenth, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 320.0), &spec);
        assert_eq!(lines.len(), 17);
        assert_eq!(lines[0], (0.0, LineWeight::Bar));
        assert_eq!(lines[1], (20.0, LineWeight::Sub));
        assert_eq!(lines[4], (80.0, LineWeight::Beat));
        assert_eq!(lines[16], (320.0, LineWeight::Bar));
        // A 1/8 grid: lines every 1/8 (and every beat).
        let spec = GridSpec::editor(Grid::Eighth, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 320.0), &spec);
        assert_eq!(lines.len(), 9);
        // Zoomed out to 1 px per 1/16, only beats... and then only bars remain.
        let axis = TimeAxis::per_step(0.0, 1.0);
        let spec = GridSpec::editor(Grid::Sixteenth, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 64.0), &spec);
        assert!(lines.iter().all(|l| l.1 == LineWeight::Bar));
    }

    #[test]
    fn triplet_grid_keeps_beats() {
        let axis = TimeAxis::per_step(0.0, 40.0);
        let spec = GridSpec::editor(Grid::QuarterTriplet, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 640.0), &spec);
        let beats = lines.iter().filter(|l| l.1 == LineWeight::Beat).count();
        let subs = lines.iter().filter(|l| l.1 == LineWeight::Sub).count();
        // Bar 0 and 1, beats 1–3, triplet lines at 640, 1280, 2560, 3200 ticks.
        assert_eq!(beats, 3);
        assert_eq!(subs, 4);
    }

    #[test]
    fn scrolled_axis() {
        let axis = TimeAxis::per_bar(-1000.0, 72.0, M);
        let lines = grid_lines(&axis, M, Rangef::new(0.0, 100.0), &GridSpec::arrangement());
        for (x, _) in &lines {
            assert!((0.0..=100.0).contains(x));
            let rem = (x + 1000.0).rem_euclid(72.0);
            assert!(rem < 1e-3 || 72.0 - rem < 1e-3, "{x}");
        }
        assert_eq!(lines.len(), 2);
    }
}
