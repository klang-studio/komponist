//! Pixel snapping: crisp 1 px lines and rect edges at every scale.
//!
//! Design px are egui points; a point is `pixels_per_point` physical pixels (1× or 2×). The
//! design wants every line to be 1 px, at 2× two device pixels ("Alle Linien sind 1 px; bei 2× 2
//! Gerätepixel"), never smeared over a third one. The helpers here therefore draw lines and
//! borders as *filled rects whose edges sit on physical pixel edges*:
//!
//! - [`line_width`]: the token line width rounded to whole physical pixels (at least one).
//! - [`snap`], [`snap_rect`]: coordinates to the nearest pixel edge.
//! - [`hline`], [`vline`]: a line whose top (left) edge is snapped to `y` (`x`).
//! - [`fill`], [`inner_border`], [`outline`]: snapped fills, borders inside a rect, rings outside.
//!
//! Only round shapes (knob arcs, fader thumbs, the rounded corners of a ring) rely on egui's
//! anti-aliasing (feathering). Axis-aligned straight edges never need it.

use egui::{Color32, CornerRadius, Painter, Pos2, Rangef, Rect, Stroke, StrokeKind, pos2};
use komponist_tokens::line;

use crate::theme::corner_radius;

/// Rounds `v` (points) to the nearest physical pixel edge.
pub fn snap(v: f32, pixels_per_point: f32) -> f32 {
    (v * pixels_per_point).round() / pixels_per_point
}

/// `rect` with every edge on the nearest physical pixel edge.
pub fn snap_rect(rect: Rect, pixels_per_point: f32) -> Rect {
    Rect::from_min_max(
        pos2(
            snap(rect.min.x, pixels_per_point),
            snap(rect.min.y, pixels_per_point),
        ),
        pos2(
            snap(rect.max.x, pixels_per_point),
            snap(rect.max.y, pixels_per_point),
        ),
    )
}

/// Width in points of a token line (`line.width`, 1 px) at this scale: a whole number of
/// physical pixels, at least one. 1 at 1×, 1 (= 2 device px) at 2×, 2/1.5 (= 2 device px)
/// at 1.5×.
pub fn line_width(pixels_per_point: f32) -> f32 {
    width(line::WIDTH, pixels_per_point)
}

/// `w` points rounded to a whole number of physical pixels, at least one.
pub fn width(w: f32, pixels_per_point: f32) -> f32 {
    (w * pixels_per_point).round().max(1.0) / pixels_per_point
}

/// The rect of a horizontal line from `x.min` to `x.max` whose top edge is at `y`, snapped.
pub fn hline_rect(x: Rangef, y: f32, pixels_per_point: f32) -> Rect {
    let top = snap(y, pixels_per_point);
    Rect::from_x_y_ranges(
        Rangef::new(snap(x.min, pixels_per_point), snap(x.max, pixels_per_point)),
        Rangef::new(top, top + line_width(pixels_per_point)),
    )
}

/// The rect of a vertical line from `y.min` to `y.max` whose left edge is at `x`, snapped.
pub fn vline_rect(x: f32, y: Rangef, pixels_per_point: f32) -> Rect {
    let left = snap(x, pixels_per_point);
    Rect::from_x_y_ranges(
        Rangef::new(left, left + line_width(pixels_per_point)),
        Rangef::new(snap(y.min, pixels_per_point), snap(y.max, pixels_per_point)),
    )
}

/// Paints a crisp horizontal 1 px line, top edge at `y` (see [`hline_rect`]).
pub fn hline(painter: &Painter, x: impl Into<Rangef>, y: f32, color: Color32) {
    let rect = hline_rect(x.into(), y, painter.pixels_per_point());
    painter.rect_filled(rect, CornerRadius::ZERO, color);
}

/// Paints a crisp vertical 1 px line, left edge at `x` (see [`vline_rect`]).
pub fn vline(painter: &Painter, x: f32, y: impl Into<Rangef>, color: Color32) {
    let rect = vline_rect(x, y.into(), painter.pixels_per_point());
    painter.rect_filled(rect, CornerRadius::ZERO, color);
}

/// Fills `rect` snapped to pixels, with corner radius `radius` (a `radius.*` token or 0).
pub fn fill(painter: &Painter, rect: Rect, radius: f32, color: Color32) {
    let rect = snap_rect(rect, painter.pixels_per_point());
    painter.rect_filled(rect, corner_radius(radius), color);
}

/// Paints 1 px lines along the inside of `rect`'s four edges (a square border without
/// radius), e.g. the implied-mute contour of a toggle.
pub fn inner_border(painter: &Painter, rect: Rect, color: Color32) {
    let ppp = painter.pixels_per_point();
    let r = snap_rect(rect, ppp);
    let w = line_width(ppp);
    let edges = [
        Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + w)),
        Rect::from_min_max(pos2(r.min.x, r.max.y - w), r.max),
        Rect::from_min_max(pos2(r.min.x, r.min.y + w), pos2(r.min.x + w, r.max.y - w)),
        Rect::from_min_max(pos2(r.max.x - w, r.min.y + w), pos2(r.max.x, r.max.y - w)),
    ];
    for e in edges {
        painter.rect_filled(e, CornerRadius::ZERO, color);
    }
}

/// Paints a 1 px ring *outside* `rect` at distance `offset`, following the corner radius
/// `radius` of the object (like CSS `outline` with `outline-offset`, whose radius grows by the
/// offset). Straight parts are crisp; only the rounded corners are anti-aliased.
///
/// Focus ring: `offset` = `focus.offset` (1); selection ring: `offset` = 0.
pub fn outline(painter: &Painter, rect: Rect, radius: f32, offset: f32, color: Color32) {
    let ppp = painter.pixels_per_point();
    let ring = snap_rect(rect.expand(offset), ppp);
    let r = if radius > 0.0 { radius + offset } else { 0.0 };
    painter.rect_stroke(
        ring,
        corner_radius(r),
        Stroke::new(line_width(ppp), color),
        StrokeKind::Outside,
    );
}

/// `p` moved to the centre of its physical pixel, for 1-pixel-wide strokes drawn as line
/// segments (e.g. a playhead drawn with [`Painter::line_segment`]).
pub fn pixel_center(p: Pos2, pixels_per_point: f32) -> Pos2 {
    let c = |v: f32| ((v * pixels_per_point).floor() + 0.5) / pixels_per_point;
    pos2(c(p.x), c(p.y))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_are_whole_device_pixels() {
        assert_eq!(line_width(1.0), 1.0);
        assert_eq!(line_width(2.0), 1.0);
        assert_eq!(line_width(1.5) * 1.5, 2.0);
        assert_eq!(line_width(1.25) * 1.25, 1.0);
        assert_eq!(width(0.2, 1.0), 1.0);
        assert_eq!(width(3.0, 2.0), 3.0);
    }

    #[test]
    fn lines_land_on_pixel_edges() {
        for ppp in [1.0_f32, 1.25, 1.5, 2.0, 3.0] {
            for y in [0.0_f32, 10.3, 10.5, 10.75, 99.9] {
                let r = hline_rect(Rangef::new(0.2, 50.7), y, ppp);
                for v in [r.min.x, r.max.x, r.min.y, r.max.y] {
                    let device = v * ppp;
                    assert!(
                        (device - device.round()).abs() < 1e-3,
                        "{v} at {ppp}× is not on a pixel edge"
                    );
                }
                let device_height = r.height() * ppp;
                assert!((device_height - (ppp.round().max(1.0))).abs() < 1e-3);
                assert!((r.min.y - y).abs() <= 0.5 / ppp + 1e-6);
            }
        }
        let v = vline_rect(3.4, Rangef::new(0.0, 10.0), 2.0);
        assert_eq!((v.min.x, v.max.x), (3.5, 4.5));
    }

    #[test]
    fn snapping() {
        assert_eq!(snap(10.4, 1.0), 10.0);
        assert_eq!(snap(10.4, 2.0), 10.5);
        assert_eq!(
            snap_rect(Rect::from_min_max(pos2(0.3, 0.6), pos2(9.6, 9.4)), 1.0),
            Rect::from_min_max(pos2(0.0, 1.0), pos2(10.0, 9.0))
        );
        assert_eq!(pixel_center(pos2(10.9, 3.0), 1.0), pos2(10.5, 3.5));
        assert_eq!(pixel_center(pos2(10.9, 3.0), 2.0), pos2(10.75, 3.25));
    }
}
