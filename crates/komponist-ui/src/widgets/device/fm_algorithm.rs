//! `fm_algorithm` (`W_alg`): the special display of hexa's FM routing, for what the graph does
//! not cover (nodes instead of a function). A `surface.1` display with the title „Algorithmus 5“
//! ([`typography::GRAPH_TITLE`]); each operator is a 22 × 18 box with its number (Mono 10, 600),
//! carriers in the track colour with `surface.0` text, modulators in `surface.4` with `text.1`;
//! `text.2` lines run from a modulator down to the operators it modulates, and from the carriers
//! to a baseline (the output); the operator with feedback has a loop; the selected operator has a
//! 1 px accent outline.
//!
//! The routing is data ([`Topology`]); the layout puts the carriers on the bottom row and every
//! other operator one row above the highest operator it modulates ([`layout`]). A free routing may
//! hold cycles (1 ↔ 6): the edge that closes a cycle does not count for the rows and is drawn as a
//! bracket on the left, so no two operators share a place. An operator that is heard and also
//! modulates is a carrier (track colour, line to the output) with its modulation links. A click on an
//! operator selects it ([`FmOutput::clicked`]). AccessKit: one `Button` per operator, „Operator 3,
//! Träger“ / „Operator 4, Modulator“.

use egui::{
    Color32, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui, WidgetInfo, pos2, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, radius};

use super::{cell_height, cell_width, elided_galley};
use crate::Lang;
use crate::state::{self, State};
use crate::widgets::metrics::{self as wm, device::FIELD_RADIUS, device::fm as metrics};
use crate::widgets::{accesskit, sense};
use crate::{focus, px};

/// The routing of an algorithm: per operator the operators it modulates (bit mask, bit `t` =
/// operator index `t`; carriers modulate nothing) and the operator with feedback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Topology<'a> {
    /// Per operator index: the bit mask of the operator indices it modulates.
    pub modulates: &'a [u8],
    /// Bit mask of the heard operators (hexa's `output` flag). `None`: the operators that
    /// modulate nothing, as in every fixed algorithm.
    pub outputs: Option<u8>,
    /// The operator with feedback, if any.
    pub feedback: Option<usize>,
}

impl Topology<'_> {
    /// Number of operators.
    pub fn len(&self) -> usize {
        self.modulates.len()
    }

    /// `true` without operators.
    pub fn is_empty(&self) -> bool {
        self.modulates.is_empty()
    }

    /// Operator `op` is heard: a carrier, drawn in the track colour with a line to the output.
    pub fn is_carrier(&self, op: usize) -> bool {
        match self.outputs {
            Some(mask) => op < 8 && mask & (1 << op) != 0,
            None => self.modulates[op] == 0,
        }
    }

    /// The operators `op` modulates.
    pub fn targets(&self, op: usize) -> impl Iterator<Item = usize> + '_ {
        let mask = self.modulates[op];
        (0..self.len().min(8)).filter(move |t| mask & (1 << t) != 0)
    }

    /// Rows above the bottom row: 0 for an operator that modulates nothing, else one more than the
    /// highest operator it modulates. The edge that closes a cycle (it points back at an operator
    /// that is still being resolved) does not count, so a cycle still gets distinct rows.
    pub fn depth(&self, op: usize) -> usize {
        self.depths().get(op).copied().unwrap_or(0)
    }

    /// [`Topology::depth`] of every operator.
    fn depths(&self) -> Vec<usize> {
        fn visit(topology: &Topology<'_>, op: usize, state: &mut [u8], depth: &mut [usize]) {
            state[op] = 1;
            let mut deepest = 0;
            for t in topology.targets(op).filter(|&t| t != op) {
                if state[t] == 1 {
                    continue;
                }
                if state[t] == 0 {
                    visit(topology, t, state, depth);
                }
                deepest = deepest.max(1 + depth[t]);
            }
            depth[op] = deepest;
            state[op] = 2;
        }
        let n = self.len();
        let (mut state, mut depth) = (vec![0_u8; n], vec![0_usize; n]);
        for op in 0..n {
            if state[op] == 0 {
                visit(self, op, &mut state, &mut depth);
            }
        }
        depth
    }

    /// The operators `op` modulates that sit on a lower row (the straight links). The others
    /// close a cycle ([`Topology::is_back_edge`]).
    fn forward_targets(&self, op: usize) -> impl Iterator<Item = usize> + '_ {
        let depths = self.depths();
        self.targets(op)
            .filter(move |&t| t != op && depths[t] < depths[op])
    }

    /// Whether the link `from` → `to` closes a cycle: `to` is not on a lower row.
    fn is_back_edge(&self, from: usize, to: usize) -> bool {
        to != from && self.depth(to) >= self.depth(from)
    }
}

/// Where the operators sit in a display of size `size` (relative to its top left), and how big
/// their boxes are.
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The centre of each operator's box.
    pub centers: Vec<Pos2>,
    /// The size of a box.
    pub node: egui::Vec2,
    /// The y of the output baseline.
    pub baseline: f32,
}

/// The layout of `topology` in a display of `size`: carriers evenly on the bottom row, every
/// other operator above the middle of the operators it modulates, rows `row_step` apart (42, or
/// less when there are more rows than fit under the title), boxes shrunk when many operators share
/// a row.
pub fn layout(topology: &Topology<'_>, size: egui::Vec2) -> Layout {
    let n = topology.len();
    let [mut bw, mut bh] = metrics::NODE;
    let carrier_y = size.y - metrics::CARRIER_Y;
    let max_depth = (0..n).map(|o| topology.depth(o)).max().unwrap_or(0);
    let mut step = metrics::ROW_STEP;
    if max_depth > 0 {
        let room = carrier_y - metrics::TITLE_ROOM;
        step = step.min(room / max_depth as f32);
        bh = bh.min((step - 3.0).max(12.0));
    }
    // The bottom row: everything that modulates nothing (forward), heard or not.
    let carriers: Vec<usize> = (0..n).filter(|&o| topology.depth(o) == 0).collect();
    let per_row = carriers.len().max(1) as f32;
    bw = bw.min(((size.x - 6.0) / per_row - 2.0).max(10.0));
    let xs_for = |count: usize| -> Vec<f32> {
        match count {
            0 => Vec::new(),
            1 => vec![0.5 * size.x],
            // Up to three carriers at 20 / 50 / 80 % as the design; more spread edge to edge.
            c if c <= 3 => (0..c)
                .map(|i| size.x * (0.2 + 0.6 * i as f32 / (c as f32 - 1.0)))
                .collect(),
            c => (0..c)
                .map(|i| {
                    let edge = bw / 2.0 + 3.0;
                    edge + i as f32 * (size.x - 2.0 * edge) / (c as f32 - 1.0)
                })
                .collect(),
        }
    };
    let mut centers = vec![pos2(0.0, 0.0); n];
    for (&op, x) in carriers.iter().zip(xs_for(carriers.len())) {
        centers[op] = pos2(x, carrier_y + bh / 2.0);
    }
    // Rows from the bottom up: x above the middle of the targets, then spread apart.
    for depth in 1..=max_depth {
        let mut row: Vec<usize> = (0..n).filter(|&o| topology.depth(o) == depth).collect();
        let y = carrier_y - depth as f32 * step + bh / 2.0;
        let mut xs: Vec<(usize, f32)> = row
            .drain(..)
            .map(|op| {
                let t: Vec<f32> = topology.forward_targets(op).map(|t| centers[t].x).collect();
                (op, t.iter().sum::<f32>() / t.len().max(1) as f32)
            })
            .collect();
        xs.sort_by(|a, b| a.1.total_cmp(&b.1));
        let min_gap = bw + 4.0;
        for i in 1..xs.len() {
            if xs[i].1 < xs[i - 1].1 + min_gap {
                xs[i].1 = xs[i - 1].1 + min_gap;
            }
        }
        // Pull a row that ran past the right edge back.
        if let Some(&(_, last)) = xs.last() {
            let over = last + bw / 2.0 + 3.0 - size.x;
            if over > 0.0 {
                for e in &mut xs {
                    e.1 -= over;
                }
            }
        }
        for (op, x) in xs {
            centers[op] = pos2(x.max(bw / 2.0 + 3.0), y);
        }
    }
    Layout {
        centers,
        node: vec2(bw, bh),
        baseline: size.y - metrics::BASELINE,
    }
}

/// The accessible name of operator `op` (0-based): „Operator 3, Träger“.
pub fn operator_name(t: Lang, topology: &Topology<'_>, op: usize) -> String {
    if topology.is_carrier(op) {
        t.ui_fm_carrier(op + 1)
    } else {
        t.ui_fm_modulator(op + 1)
    }
}

/// What the display did.
pub struct FmOutput {
    /// The display.
    pub response: Response,
    /// The operator (0-based) that was clicked.
    pub clicked: Option<usize>,
}

/// The FM algorithm display: `FmAlgorithm::new(5, &topology, &palette).selected(Some(0)).show(ui)`.
pub struct FmAlgorithm<'a> {
    number: usize,
    topology: Topology<'a>,
    palette: &'a TrackPalette,
    selected: Option<usize>,
    cols: u8,
    rows: u8,
    title: Option<String>,
}

impl<'a> FmAlgorithm<'a> {
    /// The display of algorithm `number` (1-based, shown in the title) with the routing
    /// `topology`; 2 × 2 cells by default.
    pub fn new(number: usize, topology: Topology<'a>, palette: &'a TrackPalette) -> Self {
        Self {
            number,
            topology,
            palette,
            selected: None,
            cols: 2,
            rows: 2,
            title: None,
        }
    }

    /// A title of its own instead of „Algorithmus {number}“ („Algorithmus Frei“ for a free routing,
    /// which has no number).
    #[must_use]
    pub fn titled(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The selected operator (0-based).
    #[must_use]
    pub fn selected(mut self, selected: Option<usize>) -> Self {
        self.selected = selected;
        self
    }

    /// The size in cells.
    #[must_use]
    pub fn cells(mut self, cols: u8, rows: u8) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// The title: „Algorithmus 5“.
    pub fn title(&self, t: Lang) -> String {
        self.title
            .clone()
            .unwrap_or_else(|| t.ui_fm_algorithm(self.number))
    }

    /// Draws the display and handles clicks on the operators.
    pub fn show(self, ui: &mut Ui) -> FmOutput {
        let size = vec2(cell_width(self.cols), cell_height(self.rows));
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        let lay = layout(&self.topology, size);
        let t = crate::lang(ui.ctx());
        let title = self.title(t);
        response.widget_info(|| WidgetInfo::labeled(Role::Group, true, &title));
        let painter = ui.painter().clone();
        px::fill(&painter, rect, FIELD_RADIUS, color32(color::SURFACE_1));
        let painter = painter.with_clip_rect(rect);
        paint_lines(&painter, rect, &self.topology, &lay);
        let mut clicked = None;
        let still = crate::widgets::is_still(ui);
        for op in 0..self.topology.len() {
            let node = Rect::from_center_size(rect.min + lay.centers[op].to_vec2(), lay.node);
            let r = ui.interact(
                node,
                response.id.with(("op", op)),
                sense(ui, Sense::click()),
            );
            let st = State::of(ui, &r);
            let on = self.selected == Some(op);
            let name = operator_name(t, &self.topology, op);
            r.widget_info(|| WidgetInfo::selected(Role::Button, true, on, &name));
            accesskit(&r, |n| n.set_selected(on));
            if !still && r.clicked() {
                r.request_focus();
                clicked = Some(op);
            }
            paint_node(
                &painter,
                node,
                op + 1,
                self.topology.is_carrier(op),
                self.palette,
                st,
            );
            if on {
                state::paint_select_ring(&painter, node, radius::S);
            }
            if st.focused {
                focus::paint_ring(&painter, node, radius::S);
            }
        }
        paint_title(&painter, rect, &title);
        FmOutput { response, clicked }
    }
}

fn paint_title(painter: &Painter, rect: Rect, title: &str) {
    let style = typography::GRAPH_TITLE;
    let g = elided_galley(painter, title, &style, None, rect.width() - 12.0);
    let pos = rect.min + vec2(6.0, 4.0);
    painter.galley(pos, g, color32(color::TEXT_2));
}

fn paint_lines(painter: &Painter, rect: Rect, topology: &Topology<'_>, lay: &Layout) {
    let ppp = painter.pixels_per_point();
    let stroke = Stroke::new(px::line_width(ppp), color32(color::TEXT_2));
    let at = |c: Pos2| rect.min + c.to_vec2();
    let half = lay.node.y / 2.0;
    let n = topology.len();
    for op in 0..n {
        let from = at(lay.centers[op]);
        for t in topology.targets(op).filter(|&t| t != op) {
            let to = at(lay.centers[t]);
            if topology.is_back_edge(op, t) {
                // A link that closes a cycle: a bracket on the left of both operators.
                let left = from.x.min(to.x) - lay.node.x / 2.0 - 4.0;
                let x = left.max(rect.left() + 2.0);
                painter.add(Shape::line(
                    vec![
                        from - vec2(lay.node.x / 2.0, 0.0),
                        pos2(x, from.y),
                        pos2(x, to.y),
                        to - vec2(lay.node.x / 2.0, 0.0),
                    ],
                    stroke,
                ));
            } else {
                painter.line_segment([from + vec2(0.0, half), to - vec2(0.0, half)], stroke);
            }
        }
    }
    // The line of every heard operator to the output. One that also modulates leaves from the
    // right of its box, so the links to its targets (from the middle) stay apart.
    let base_y = rect.top() + lay.baseline;
    let mut taps: Vec<f32> = Vec::new();
    for c in (0..n).filter(|&o| topology.is_carrier(o)) {
        let p = at(lay.centers[c]);
        let x = if topology.depth(c) == 0 {
            p.x
        } else {
            p.x + lay.node.x / 2.0 - 2.0
        };
        painter.line_segment([pos2(x, p.y + half), pos2(x, base_y)], stroke);
        taps.push(x);
    }
    if let (Some(a), Some(b)) = (
        taps.iter().copied().reduce(f32::min),
        taps.iter().copied().reduce(f32::max),
    ) {
        painter.line_segment([pos2(a, base_y), pos2(b, base_y)], stroke);
    }
    if let Some(op) = topology.feedback.filter(|&o| o < topology.len()) {
        // `M cx+bw/2 y+9 h6 v-16 h-(bw/2+6) v7`: out of the right edge, up, back over the box,
        // down into its top.
        let c = at(lay.centers[op]);
        let [reach, rise, drop] = metrics::FEEDBACK;
        let start = pos2(c.x + lay.node.x / 2.0, c.y);
        let p1 = start + vec2(reach, 0.0);
        let p2 = p1 - vec2(0.0, rise);
        let p3 = pos2(p2.x - (lay.node.x / 2.0 + reach), p2.y);
        let p4 = p3 + vec2(0.0, drop);
        painter.add(Shape::line(vec![start, p1, p2, p3, p4], stroke));
    }
}

fn paint_node(
    painter: &Painter,
    rect: Rect,
    number: usize,
    carrier: bool,
    palette: &TrackPalette,
    state: State,
) {
    let ppp = painter.pixels_per_point();
    let (fill, text): (Color32, Color32) = if carrier {
        (color32(palette.base), color32(color::SURFACE_0))
    } else if state.hovered {
        (color32(color::SURFACE_5), color32(color::TEXT_1))
    } else {
        (color32(color::SURFACE_4), color32(color::TEXT_1))
    };
    px::fill(painter, rect, metrics::NODE_RADIUS, fill);
    let style = typography::VALUE_S;
    let g = elided_galley(
        painter,
        &number.to_string(),
        &style,
        Some(wm::SEMIBOLD),
        rect.width(),
    );
    painter.galley(crate::widgets::text_pos_centered(rect, &g, ppp), g, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Algorithm 1 of hexa: 2 → 1, 4 → 3, 6 → 5 (carriers 1, 3, 5), feedback on 6.
    const ALGO_1: Topology<'static> = Topology {
        modulates: &[0b000000, 0b000001, 0b000000, 0b000100, 0b001000, 0b010000],
        outputs: None,
        feedback: Some(5),
    };

    #[test]
    fn carriers_and_depth() {
        assert!(ALGO_1.is_carrier(0) && ALGO_1.is_carrier(2));
        assert!(!ALGO_1.is_carrier(1));
        assert_eq!(ALGO_1.depth(0), 0);
        assert_eq!(ALGO_1.depth(1), 1);
        // 6 → 5 → 4 → 3: three rows above carrier 3.
        assert_eq!(ALGO_1.depth(5), 3);
        assert_eq!(operator_name(Lang::De, &ALGO_1, 2), "Operator 3, Träger");
        assert_eq!(operator_name(Lang::De, &ALGO_1, 3), "Operator 4, Modulator");
    }

    #[test]
    fn layout_keeps_every_box_inside_and_apart() {
        let size = vec2(116.0, 132.0);
        for topo in [
            ALGO_1,
            Topology {
                modulates: &[0, 0, 0, 0, 0, 0],
                outputs: None,
                feedback: None,
            },
            Topology {
                modulates: &[0, 1, 2, 4, 8, 16],
                outputs: None,
                feedback: Some(5),
            },
            // 1 ↔ 6 (free routing), and a longer cycle with everything heard.
            Topology {
                modulates: &[0b100000, 0, 0, 0, 0, 0b000001],
                outputs: Some(0b100001),
                feedback: None,
            },
            Topology {
                modulates: &[0b000010, 0b000100, 0b001000, 0b000001, 0b100000, 0b010000],
                outputs: Some(0b111111),
                feedback: Some(0),
            },
        ] {
            let l = layout(&topo, size);
            let rects: Vec<Rect> = l
                .centers
                .iter()
                .map(|&c| Rect::from_center_size(c, l.node))
                .collect();
            let area = Rect::from_min_size(pos2(0.0, 0.0), size);
            for (i, r) in rects.iter().enumerate() {
                assert!(area.contains_rect(*r), "operator {} outside: {r:?}", i + 1);
                for (j, o) in rects.iter().enumerate().skip(i + 1) {
                    assert!(
                        !r.intersects(*o),
                        "operators {} and {} overlap",
                        i + 1,
                        j + 1
                    );
                }
            }
        }
    }

    #[test]
    fn a_cycle_gets_distinct_rows_and_a_heard_modulator_is_a_carrier() {
        // 1 ↔ 6: one of them is a row above the other, not on the same spot.
        let cycle = Topology {
            modulates: &[0b100000, 0, 0, 0, 0, 0b000001],
            outputs: Some(0b100001),
            feedback: None,
        };
        assert_ne!(cycle.depth(0), cycle.depth(5));
        assert!(cycle.is_back_edge(5, 0) != cycle.is_back_edge(0, 5));
        // Heard and modulating: a carrier although it has targets; unheard and silent: not.
        let mixed = Topology {
            modulates: &[0b10, 0, 0],
            outputs: Some(0b001),
            feedback: None,
        };
        assert!(mixed.is_carrier(0) && !mixed.is_carrier(1));
        assert_eq!(operator_name(Lang::De, &mixed, 0), "Operator 1, Träger");
    }
}
