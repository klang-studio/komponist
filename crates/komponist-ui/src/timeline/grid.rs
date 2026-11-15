//! Vertical grid lines of the timeline canvases: bars, beats and the snap grid.
//!
//! Weights follow the design system's line tokens: bar lines `line.2` ("Taktlinien"), beat lines
//! `line.1` ("Schlaglinien"), grid lines between beats the fainter `line.0` (a komponist addition
//! from the piano roll of the screens, `design/README.md`). A level is left out when its lines
//! would be closer than the view's minimum spacing; the chosen raster is then drawn one musical
//! level coarser at a time ([`display_step`]), shared by arrangement and piano roll.

use egui::{Color32, Painter, Rangef, Rect, pos2};
use komponist_model::{Grid, Meter, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::color;

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
    /// Grid line between beats: `line.0`.
    Sub,
}

impl LineWeight {
    /// The colour of the line.
    pub fn color(self) -> Color32 {
        color32(match self {
            Self::Bar => color::LINE_2,
            Self::Beat => color::LINE_1,
            Self::Sub => color::LINE_0,
        })
    }
}

/// Which lines a canvas shows: the chosen raster, drawn as long as its lines keep `min_spacing`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GridSpec {
    /// The raster chosen in the "Raster" dropdown (the snap grid; only the display adapts).
    pub grid: Grid,
    /// Minimum distance in px between two drawn lines (the view's `GRID_MIN_SPACING`).
    pub min_spacing: f32,
}

impl GridSpec {
    /// The raster `grid`, thinned out where lines would be closer than `min_spacing` px.
    pub const fn new(grid: Grid, min_spacing: f32) -> Self {
        Self { grid, min_spacing }
    }
}

/// The next coarser musical level above `step`: straight rasters double up to one bar; triplet
/// rasters double up to one beat, then jump to one bar; from a bar on, the bar count doubles
/// (2, 4, 8 bars, and beyond, so that the spacing guarantee holds at any zoom).
fn coarser_step(step: i64, triplet: bool, meter: Meter) -> i64 {
    let (per_bar, per_beat) = (meter.ticks_per_bar(), meter.ticks_per_beat());
    if step >= per_bar {
        step * 2
    } else if triplet {
        if step < per_beat {
            (step * 2).min(per_beat)
        } else {
            per_bar
        }
    } else {
        (step * 2).min(per_bar)
    }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// The spacing in ticks of the lines drawn for `spec`: the raster itself, or the first coarser
/// musical level whose lines are at least `spec.min_spacing` px apart on `axis`. Snapping uses
/// the raster, never this.
pub fn display_step(axis: &TimeAxis, meter: Meter, spec: &GridSpec) -> Tick {
    let triplet = matches!(
        spec.grid,
        Grid::QuarterTriplet | Grid::EighthTriplet | Grid::SixteenthTriplet
    );
    let mut step = spec.grid.ticks(meter).0.max(1);
    let (per_bar, per_beat) = (meter.ticks_per_bar(), meter.ticks_per_beat());
    let width = |t: i64| axis.width(Tick(t));
    if !width(per_bar).is_finite() || width(per_bar) <= 0.0 {
        return Tick(step);
    }
    // Bars and (below a beat) beats are drawn between the step's lines, so the smallest gap is the
    // step's gcd with them (a 1/4 triplet and the beat lines are 1/8 triplet apart).
    let gap = |s: i64| {
        let mut g = if s < per_bar { gcd(s, per_bar) } else { s };
        if s < per_beat {
            g = gcd(g, per_beat);
        }
        g
    };
    while width(gap(step)) < spec.min_spacing {
        step = coarser_step(step, triplet, meter);
    }
    Tick(step)
}

/// The x positions and weights of the lines of `spec` inside the screen range `x`, left to right.
/// Lines sit at every multiple of [`display_step`]; bar lines (`line.2`) are always drawn and a
/// line on a bar is a bar line, on a beat a beat line (`line.1`), else a grid line (`line.0`).
/// Beats are drawn as context while the step is finer than a beat, bars while it is finer than a
/// bar.
pub fn grid_lines(
    axis: &TimeAxis,
    meter: Meter,
    x: Rangef,
    spec: &GridSpec,
) -> Vec<(f32, LineWeight)> {
    let per_bar = meter.ticks_per_bar();
    let per_beat = meter.ticks_per_beat();
    let bar_px = axis.width(Tick(per_bar));
    if bar_px <= 0.0 || !bar_px.is_finite() {
        return Vec::new();
    }
    let step = display_step(axis, meter, spec).0;
    let (t0, t1) = axis.ticks_in(x);
    // Candidate positions of every level; triplet rasters need not divide the beat, so the levels
    // are collected separately and merged.
    let mut ticks: Vec<i64> = Vec::new();
    let mut level = |step: i64| {
        let mut t = t0.0.div_euclid(step).max(0) * step;
        while t <= t1.0 {
            ticks.push(t);
            t += step;
        }
    };
    level(step);
    if step < per_bar {
        level(per_bar);
    }
    if step < per_beat {
        level(per_beat);
    }
    ticks.sort_unstable();
    ticks.dedup();
    ticks
        .into_iter()
        .map(|t| {
            let weight = if t % per_bar == 0 {
                LineWeight::Bar
            } else if t % per_beat == 0 {
                LineWeight::Beat
            } else {
                LineWeight::Sub
            };
            (axis.x(Tick(t)), weight)
        })
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
    const MIN: f32 = 8.0;
    const ALL: [Grid; 9] = [
        Grid::Bar,
        Grid::Half,
        Grid::Quarter,
        Grid::Eighth,
        Grid::Sixteenth,
        Grid::ThirtySecond,
        Grid::QuarterTriplet,
        Grid::EighthTriplet,
        Grid::SixteenthTriplet,
    ];

    fn lines(axis: &TimeAxis, grid: Grid, width: f32) -> Vec<(f32, LineWeight)> {
        grid_lines(axis, M, Rangef::new(0.0, width), &GridSpec::new(grid, MIN))
    }

    #[test]
    fn bar_raster_shows_bars() {
        let axis = TimeAxis::per_bar(0.0, 72.0, M);
        let l = lines(&axis, Grid::Bar, 300.0);
        let xs: Vec<f32> = l.iter().map(|l| l.0.round()).collect();
        assert_eq!(xs, [0.0, 72.0, 144.0, 216.0, 288.0]);
        assert!(l.iter().all(|l| l.1 == LineWeight::Bar));
    }

    #[test]
    fn every_raster_changes_the_line_set() {
        // 480 px per bar: every raster fits (1/32 = 15 px, 1/16 triplet = 20 px).
        let axis = TimeAxis::per_bar(0.0, 960.0, M);
        let sets: Vec<Vec<i32>> = ALL
            .iter()
            .map(|g| {
                lines(&axis, *g, 1920.0)
                    .iter()
                    .map(|l| l.0.round() as i32)
                    .collect()
            })
            .collect();
        for i in 0..sets.len() {
            for j in i + 1..sets.len() {
                assert_ne!(sets[i], sets[j], "{:?} vs {:?}", ALL[i], ALL[j]);
            }
        }
        // Line counts over two bars at 960 px/bar (inclusive ends).
        assert_eq!(sets[0].len(), 3, "bar");
        assert_eq!(sets[1].len(), 5, "1/2");
        assert_eq!(sets[2].len(), 9, "1/4");
        assert_eq!(sets[5].len(), 65, "1/32");
    }

    #[test]
    fn hierarchy_of_weights() {
        let axis = TimeAxis::per_step(0.0, 20.0);
        let l = lines(&axis, Grid::Sixteenth, 320.0);
        assert_eq!(l.len(), 17);
        assert_eq!(l[0], (0.0, LineWeight::Bar));
        assert_eq!(l[1], (20.0, LineWeight::Sub));
        assert_eq!(l[4], (80.0, LineWeight::Beat));
        assert_eq!(l[16], (320.0, LineWeight::Bar));
        // 1/8: every 1/8 and every beat.
        assert_eq!(lines(&axis, Grid::Eighth, 320.0).len(), 9);
    }

    #[test]
    fn triplet_grid_keeps_beats() {
        let axis = TimeAxis::per_step(0.0, 40.0);
        let l = lines(&axis, Grid::QuarterTriplet, 640.0);
        let beats = l.iter().filter(|l| l.1 == LineWeight::Beat).count();
        let subs = l.iter().filter(|l| l.1 == LineWeight::Sub).count();
        // Bar 0 and 1, beats 1–3, triplet lines at 640, 1280, 2560, 3200 ticks.
        assert_eq!(beats, 3);
        assert_eq!(subs, 4);
    }

    #[test]
    fn display_step_coarsens_musically() {
        let step = |px_per_step: f32, g: Grid| {
            display_step(&TimeAxis::per_step(0.0, px_per_step), M, &GridSpec::new(g, MIN)).0
        };
        // Fits: unchanged.
        assert_eq!(step(20.0, Grid::Sixteenth), 240);
        assert_eq!(step(20.0, Grid::SixteenthTriplet), 160);
        // 1/32 at 3 px per 1/16 is 1.5 px: doubles to 1/16 (3 px), 1/8 (6 px), 1/4 (12 px).
        assert_eq!(step(3.0, Grid::ThirtySecond), 960);
        // Straight rasters climb to bars: 1/16 at 1 px, a bar is 16 px; 2 bars 32 px.
        assert_eq!(step(0.5, Grid::Sixteenth), 3840);
        assert_eq!(step(0.1, Grid::Sixteenth), 3840 * 8);
        // Triplets: within the family up to a beat, then bars.
        assert_eq!(step(3.0, Grid::SixteenthTriplet), 960);
        assert_eq!(step(1.5, Grid::SixteenthTriplet), 3840);
        assert_eq!(step(2.5, Grid::QuarterTriplet), 960);
    }

    #[test]
    fn lines_never_crowd() {
        for g in ALL {
            for px_per_step in [0.05_f32, 0.2, 0.5, 1.0, 3.0, 7.0, 20.0, 80.0] {
                let axis = TimeAxis::per_step(0.0, px_per_step);
                let width = (px_per_step * 16.0 * 40.0).min(4000.0);
                let l = lines(&axis, g, width);
                for w in l.windows(2) {
                    assert!(
                        w[1].0 - w[0].0 >= MIN - 1e-3 || w[0].1 == LineWeight::Bar,
                        "{g:?} at {px_per_step}: {:?}",
                        w
                    );
                }
            }
        }
    }

    #[test]
    fn scrolled_axis() {
        let axis = TimeAxis::per_bar(-1000.0, 72.0, M);
        let l = lines(&axis, Grid::Bar, 100.0);
        for (x, _) in &l {
            assert!((0.0..=100.0).contains(x));
            let rem = (x + 1000.0).rem_euclid(72.0);
            assert!(rem < 1e-3 || 72.0 - rem < 1e-3, "{x}");
        }
        assert_eq!(l.len(), 2);
    }
}
