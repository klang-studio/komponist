//! Building one [`Mesh`] for many timeline objects: crisp pixel-snapped quads, 1 px borders,
//! dashed outlines, and rounded fills and rings whose corners alone are anti-aliased.
//!
//! Painting thousands of notes as separate [`egui::Shape`]s would make egui tessellate each one
//! every frame. [`Quads`] writes the triangles directly instead; straight edges need no
//! anti-aliasing because every edge sits on a physical pixel edge, so a rounded rect is a crisp
//! cross of quads plus four small anti-aliased corner fans.

use egui::{Color32, Mesh, Pos2, Rect, pos2};

use crate::px;
use crate::widgets::metrics::timeline as metrics;

/// Unit directions of a quarter circle from +x to +y (screen coordinates, y down) split into
/// 1–4 segments, as `(cos, sin)`.
const QUARTER: [&[(f32, f32)]; 4] = [
    &[(1.0, 0.0), (0.0, 1.0)],
    &[
        (1.0, 0.0),
        (
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ),
        (0.0, 1.0),
    ],
    &[
        (1.0, 0.0),
        (0.866_025_4, 0.5),
        (0.5, 0.866_025_4),
        (0.0, 1.0),
    ],
    &[
        (1.0, 0.0),
        (0.923_879_5, 0.382_683_43),
        (
            std::f32::consts::FRAC_1_SQRT_2,
            std::f32::consts::FRAC_1_SQRT_2,
        ),
        (0.382_683_43, 0.923_879_5),
        (0.0, 1.0),
    ],
];

/// The directions of the quarter circle starting at `quadrant` × 90° (0: +x, 1: +y, 2: −x,
/// 3: −y) for a corner of `radius_px` device pixels: more segments for larger corners, 2 for
/// the 2 px radius of a note at 1×.
fn quarter(quadrant: u8, radius_px: f32) -> impl Iterator<Item = egui::Vec2> {
    let n = if radius_px <= 2.5 {
        2
    } else if radius_px <= 4.5 {
        3
    } else {
        4
    };
    QUARTER[n - 1].iter().map(move |&(c, s)| match quadrant {
        0 => egui::vec2(c, s),
        1 => egui::vec2(-s, c),
        2 => egui::vec2(-c, -s),
        _ => egui::vec2(s, -c),
    })
}

/// A mesh under construction at a fixed scale.
pub(crate) struct Quads {
    pub mesh: Mesh,
    ppp: f32,
    /// Feathering width in points (one physical pixel), for the corner fans.
    feather: f32,
}

impl Quads {
    /// An empty mesh for `pixels_per_point`.
    pub fn new(pixels_per_point: f32) -> Self {
        Self {
            mesh: Mesh::default(),
            ppp: pixels_per_point,
            feather: 1.0 / pixels_per_point,
        }
    }

    /// Reserves room for `n` more quads.
    pub fn reserve(&mut self, n: usize) {
        self.mesh.reserve_vertices(4 * n);
        self.mesh.reserve_triangles(2 * n);
    }

    /// The scale.
    pub fn pixels_per_point(&self) -> f32 {
        self.ppp
    }

    /// `r` snapped to physical pixels.
    pub fn snap(&self, r: Rect) -> Rect {
        px::snap_rect(r, self.ppp)
    }

    /// A quad exactly at `r` (the caller snapped it); nothing if empty.
    pub fn quad(&mut self, r: Rect, color: Color32) {
        if r.width() > 0.0 && r.height() > 0.0 && color != Color32::TRANSPARENT {
            self.mesh.add_colored_rect(r, color);
        }
    }

    /// A quad at `r` snapped to pixels.
    pub fn rect(&mut self, r: Rect, color: Color32) {
        let r = self.snap(r);
        self.quad(r, color);
    }

    /// A filled rect with corner radius `radius`, snapped to pixels. Straight edges are crisp;
    /// the corners are small anti-aliased fans. A radius below one physical pixel is a plain
    /// quad.
    pub fn rounded(&mut self, r: Rect, radius: f32, color: Color32) {
        self.rounded_corners(r, radius, [true; 4], color);
    }

    /// Like [`Self::rounded`], but only the corners set in `corners` (top left, top right,
    /// bottom right, bottom left) are rounded; the others stay square.
    pub fn rounded_corners(&mut self, r: Rect, radius: f32, corners: [bool; 4], color: Color32) {
        let r = self.snap(r);
        let k = radius.min(r.width() / 2.0).min(r.height() / 2.0);
        if k * self.ppp < 1.0 || !corners.contains(&true) {
            self.quad(r, color);
            return;
        }
        let (x0, x1, y0, y1) = (r.min.x, r.max.x, r.min.y, r.max.y);
        // The cross: full height between the corner columns, the side columns between the
        // corner rows.
        self.quad(
            Rect::from_min_max(pos2(x0 + k, y0), pos2(x1 - k, y1)),
            color,
        );
        self.quad(
            Rect::from_min_max(pos2(x0, y0 + k), pos2(x0 + k, y1 - k)),
            color,
        );
        self.quad(
            Rect::from_min_max(pos2(x1 - k, y0 + k), pos2(x1, y1 - k)),
            color,
        );
        let c = [
            (pos2(x0 + k, y0 + k), 2, pos2(x0, y0)),
            (pos2(x1 - k, y0 + k), 3, pos2(x1 - k, y0)),
            (pos2(x1 - k, y1 - k), 0, pos2(x1 - k, y1 - k)),
            (pos2(x0 + k, y1 - k), 1, pos2(x0, y1 - k)),
        ];
        for ((center, quadrant, square), round) in c.into_iter().zip(corners) {
            if round {
                self.corner_fan(center, k, quadrant, color);
            } else {
                self.quad(Rect::from_min_size(square, egui::vec2(k, k)), color);
            }
        }
    }

    /// A quarter disc at `center` with `radius` in `quadrant`, with a one-pixel feather on the
    /// arc.
    fn corner_fan(&mut self, center: Pos2, radius: f32, quadrant: u8, color: Color32) {
        let f = self.feather;
        let inner_r = (radius - f / 2.0).max(0.0);
        let outer_r = radius + f / 2.0;
        let m = &mut self.mesh;
        let base = m.vertices.len() as u32;
        m.colored_vertex(center, color);
        let mut n = 0;
        for dir in quarter(quadrant, radius * self.ppp) {
            m.colored_vertex(center + dir * inner_r, color);
            m.colored_vertex(center + dir * outer_r, Color32::TRANSPARENT);
            n += 1;
        }
        for i in 0..n - 1 {
            let (i0, o0) = (base + 1 + 2 * i, base + 2 + 2 * i);
            let (i1, o1) = (i0 + 2, o0 + 2);
            m.add_triangle(base, i0, i1);
            m.add_triangle(i0, o0, i1);
            m.add_triangle(i1, o0, o1);
        }
    }

    /// A 1 px ring outside `r` at distance `offset`, following the radius `radius` (grown by the
    /// offset), as `0 0 0 1px` box shadows and outlines draw it.
    pub fn ring(&mut self, r: Rect, radius: f32, offset: f32, color: Color32) {
        let w = px::line_width(self.ppp);
        let inner = self.snap(r.expand(offset));
        let k = if radius > 0.0 {
            radius + offset + w
        } else {
            0.0
        };
        self.frame(inner.expand(w), w, k, color);
    }

    /// A 1 px border inside `r` following the radius `radius` (`inset 0 0 0 1px`).
    pub fn inner_ring(&mut self, r: Rect, radius: f32, color: Color32) {
        let w = px::line_width(self.ppp);
        let outer = self.snap(r);
        self.frame(outer, w, radius, color);
    }

    /// A border of width `w` inside `outer` (snapped) whose outer corners have radius `k`:
    /// straight sides as crisp quads, rounded corners as feathered quarter rings.
    fn frame(&mut self, outer: Rect, w: f32, k: f32, color: Color32) {
        let rounded = k * self.ppp >= 1.0;
        let k = if rounded { k.max(w) } else { 0.0 };
        let side = k.max(w);
        let (x0, x1, y0, y1) = (outer.min.x, outer.max.x, outer.min.y, outer.max.y);
        self.quad(
            Rect::from_min_max(pos2(x0 + k, y0), pos2(x1 - k, y0 + w)),
            color,
        );
        self.quad(
            Rect::from_min_max(pos2(x0 + k, y1 - w), pos2(x1 - k, y1)),
            color,
        );
        self.quad(
            Rect::from_min_max(pos2(x0, y0 + side), pos2(x0 + w, y1 - side)),
            color,
        );
        self.quad(
            Rect::from_min_max(pos2(x1 - w, y0 + side), pos2(x1, y1 - side)),
            color,
        );
        if !rounded {
            return;
        }
        let corners = [
            (pos2(x0 + k, y0 + k), 2),
            (pos2(x1 - k, y0 + k), 3),
            (pos2(x1 - k, y1 - k), 0),
            (pos2(x0 + k, y1 - k), 1),
        ];
        for (center, quadrant) in corners {
            self.corner_arc(center, k - w, k, quadrant, color);
        }
    }

    /// A quarter ring from radius `r0` to `r1` around `center` in `quadrant`, with both arcs
    /// feathered.
    fn corner_arc(&mut self, center: Pos2, r0: f32, r1: f32, quadrant: u8, color: Color32) {
        let f = self.feather / 2.0;
        let radii = [(r0 - f).max(0.0), r0 + f, (r1 - f).max(r0 + f), r1 + f];
        let colors = [Color32::TRANSPARENT, color, color, Color32::TRANSPARENT];
        let m = &mut self.mesh;
        let base = m.vertices.len() as u32;
        let mut n = 0;
        for dir in quarter(quadrant, r1 * self.ppp) {
            for (radius, c) in radii.iter().zip(colors) {
                m.colored_vertex(center + dir * *radius, c);
            }
            n += 1;
        }
        for i in 0..n - 1 {
            let a = base + 4 * i;
            let b = a + 4;
            for j in 0..3 {
                m.add_triangle(a + j, a + j + 1, b + j);
                m.add_triangle(a + j + 1, b + j + 1, b + j);
            }
        }
    }

    /// A dashed 1 px border along the inside of `r` (CSS `1px dashed`): dashes and gaps of
    /// [`metrics::DASH`], each side starting with a dash at its corner.
    pub fn dashed_border(&mut self, r: Rect, color: Color32) {
        let r = self.snap(r);
        let w = px::line_width(self.ppp);
        let (x0, x1, y0, y1) = (r.min.x, r.max.x, r.min.y, r.max.y);
        for (a, b) in dashes(x0, x1, self.ppp) {
            self.quad(Rect::from_min_max(pos2(a, y0), pos2(b, y0 + w)), color);
            self.quad(Rect::from_min_max(pos2(a, y1 - w), pos2(b, y1)), color);
        }
        for (a, b) in dashes(y0 + w, y1 - w, self.ppp) {
            self.quad(Rect::from_min_max(pos2(x0, a), pos2(x0 + w, b)), color);
            self.quad(Rect::from_min_max(pos2(x1 - w, a), pos2(x1, b)), color);
        }
    }

    /// Appends `other` (drawn on top).
    pub fn append(&mut self, other: Quads) {
        self.mesh.append(other.mesh);
    }

    /// Adds the mesh to `painter`, if not empty.
    pub fn paint(self, painter: &egui::Painter) {
        if !self.mesh.is_empty() {
            painter.add(egui::Shape::mesh(self.mesh));
        }
    }
}

/// Dash segments `[a, b)` from `from` to `to`, snapped to pixels.
fn dashes(from: f32, to: f32, ppp: f32) -> impl Iterator<Item = (f32, f32)> {
    let dash = metrics::DASH;
    let n = ((to - from) / (2.0 * dash)).ceil().max(0.0) as usize;
    (0..n).map(move |i| {
        let s = from + i as f32 * 2.0 * dash;
        (px::snap(s, ppp), px::snap((s + dash).min(to), ppp))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    #[test]
    fn small_radius_is_a_plain_quad() {
        let mut q = Quads::new(1.0);
        q.rounded(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(10.0, 2.0)),
            0.6,
            Color32::RED,
        );
        assert_eq!(q.mesh.vertices.len(), 4);
    }

    #[test]
    fn rounded_rect_is_a_cross_and_four_fans() {
        let mut q = Quads::new(1.0);
        q.rounded(
            Rect::from_min_size(pos2(0.3, 0.0), vec2(10.0, 10.0)),
            2.0,
            Color32::RED,
        );
        // Radius 2 at 1×: two segments, three directions per corner.
        let fan = 1 + 2 * 3;
        assert_eq!(q.mesh.vertices.len(), 3 * 4 + 4 * fan);
        // Snapped: every crisp vertex lies on whole pixels.
        for v in &q.mesh.vertices[..12] {
            assert_eq!(v.pos.x, v.pos.x.round());
            assert_eq!(v.pos.y, v.pos.y.round());
        }
    }

    #[test]
    fn dashes_cover_about_half_the_border() {
        let mut q = Quads::new(1.0);
        q.dashed_border(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(60.0, 12.0)),
            Color32::RED,
        );
        let area: f32 = q
            .mesh
            .vertices
            .chunks(4)
            .map(|v| (v[3].pos.x - v[0].pos.x).abs() * (v[3].pos.y - v[0].pos.y).abs())
            .sum();
        let border = 2.0 * 60.0 + 2.0 * 10.0;
        assert!(area > 0.4 * border && area < 0.6 * border, "{area}");
    }
}
