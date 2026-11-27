//! The device widgets (WP-83; device handoff §4, `geraete_page_generator.py` `W_*`): what a
//! device's cells hold, drawn in the grid of `komponist_tokens::device::grid` (cell 56 × 64, gap 4,
//! half cell 56 × 30).
//!
//! | Widget | Cell | Module |
//! |---|---|---|
//! | knob, knob_large | 1 × 1, 2 × 1 | [`knob_cell`] |
//! | number | ½ × n | [`number`] |
//! | segment, toggle_button, dropdown | ½ × n | [`choice`] |
//! | mini_fader | 1 × 2 / 1 × 3 | [`mini_fader`] |
//! | meter_gr | 1 × 3 | [`meter_gr`] |
//! | pad_grid | n × m | [`pad_grid`] |
//! | fm_algorithm | n × m | [`fm_algorithm`] |
//! | graph (envelope, eq, filter, transfer, waveform, xy, lfo) | min 2 × 1 | [`graph`] |
//!
//! Base rules of the design system apply to all of them: hover one surface up, focus ring, Shift
//! fine / Ctrl coarse drag, Alt-click and (knobs, faders) double-click the default, Enter or the
//! right-click menu text entry (number fields: double-click), value tooltip while dragging; labels in `type.device-label` `text.2`. Every widget has an AccessKit role and the
//! German name the caller gives. Automation dot, modulation arc and MIDI-learn look exist as
//! options ([`knob_cell`], [`super::knob::ModArc`]) that nothing sets yet (owner decision
//! 2026-11-14: automation and modulation are only prepared).

pub mod choice;
pub mod fm_algorithm;
pub mod gallery;
pub mod graph;
pub mod knob_cell;
pub mod meter_gr;
pub mod mini_fader;
pub mod number;
pub mod pad_grid;

use std::sync::Arc;

use egui::{Color32, Galley, Painter, Pos2, Rect, Vec2, pos2, vec2};
use komponist_model::params::layout::{Cell, Half};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, device, radius, widget};

use super::knob::ModArc;
use crate::{fonts, px};

/// `n` cells with their gaps: `n · 56 + (n − 1) · 4` (`device.grid.span_w`).
pub fn cell_width(n: u8) -> f32 {
    span(device::grid::CELL[0], n)
}

/// `n` cell rows with their gaps (`device.grid.span_h`).
pub fn cell_height(n: u8) -> f32 {
    span(device::grid::CELL[1], n)
}

fn span(cell: f32, n: u8) -> f32 {
    let n = f32::from(n.max(1));
    n * cell + (n - 1.0) * device::grid::GAP
}

/// Width of a device body, including padding on both sides of the grid.
pub fn device_width(cols: u8) -> f32 {
    2.0 * device::grid::PADDING + cell_width(cols)
}

/// Rectangle of a cell relative to the top-left corner of the device body.
pub fn cell_rect(cell: &Cell) -> Rect {
    let [cw, ch] = device::grid::CELL;
    let (gap, pad) = (device::grid::GAP, device::grid::PADDING);
    let x = pad + f32::from(cell.col) * (cw + gap);
    let mut y = pad + f32::from(cell.row) * (ch + gap);
    let (w, h) = (span(cw, cell.cols), span(ch, cell.rows));
    let h = match cell.half {
        // A half cell over several rows (`graph_half`): the full rows above it and the top
        // half of the last one.
        Some(Half::Top) if cell.rows > 1 => {
            span(ch, cell.rows - 1) + gap + device::grid::HALF_CELL_H
        }
        Some(half) => {
            if half == Half::Bottom {
                y += device::grid::HALF_OFFSET_BOTTOM;
            }
            device::grid::HALF_CELL_H
        }
        None => h,
    };
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

/// The size of a half cell `cols` wide: `56 · cols … × 30`.
pub fn half_cell(cols: u8) -> Vec2 {
    vec2(cell_width(cols), device::grid::HALF_CELL_H)
}

/// The text style of labels and the colour a label has: `type.device-label`.
pub const LABEL: TypeStyle = typography::DEVICE_LABEL;
/// The style of values in a cell: `type.value.s` (Mono 10/12) in `text.1` (`val()`).
pub const VALUE: TypeStyle = typography::VALUE_S;

/// `text` as one line in `style` (at `weight` if given), cut with „…“ to fit `max_width`.
pub fn elided_galley(
    painter: &Painter,
    text: &str,
    style: &TypeStyle,
    weight: Option<u16>,
    max_width: f32,
) -> Arc<Galley> {
    let lay = |t: &str| {
        let mut job = fonts::layout_job(t, style, Color32::PLACEHOLDER);
        if let Some(w) = weight {
            for section in &mut job.sections {
                section.format.font_id = super::font_at(style, w);
            }
        }
        painter.layout_job(job)
    };
    let full = lay(text);
    if full.size().x <= max_width {
        return full;
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0, chars.len());
    // Longest prefix that fits with the ellipsis (binary search; widths grow with the prefix).
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let candidate: String = chars[..mid].iter().collect::<String>() + "\u{2026}";
        if lay(&candidate).size().x <= max_width {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lay(&(chars[..lo].iter().collect::<String>() + "\u{2026}"))
}

/// Paints a label (`lbl()`: `type.device-label`, `text.2`, cut to the width) at the top left of
/// `rect`; `disabled` uses `text.4`.
pub fn paint_label(painter: &Painter, rect: Rect, text: &str, disabled: bool) {
    if text.is_empty() {
        return;
    }
    let ppp = painter.pixels_per_point();
    let g = elided_galley(painter, text, &LABEL, None, rect.width());
    let col = if disabled {
        color::TEXT_4
    } else {
        color::TEXT_2
    };
    let pos = pos2(px::snap(rect.left(), ppp), px::snap(rect.top(), ppp));
    painter.galley(pos, g, color32(col));
}

/// Paints `text` in `style` centred horizontally on `at.x`, its top at `at.y`; returns its height.
pub fn paint_centered(
    painter: &Painter,
    at: Pos2,
    text: &str,
    style: &TypeStyle,
    weight: Option<u16>,
    max_width: f32,
    color: Color32,
) -> f32 {
    let ppp = painter.pixels_per_point();
    let g = elided_galley(painter, text, style, weight, max_width);
    let h = g.size().y;
    let pos = pos2(px::snap(at.x - g.size().x / 2.0, ppp), px::snap(at.y, ppp));
    painter.galley(pos, g, color);
    h
}

/// The look options of a knob cell that nothing sets yet (automation, MIDI learn, modulation).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct KnobCellOptions {
    /// The 5 px dot in front of the name (`knob.automation_dot`, `track.base`): the colour.
    pub automation_dot: Option<Color32>,
    /// MIDI learn: `accent.surface` cell with a 1 px accent ring (`knob.learn`).
    pub learn: bool,
    /// The modulation arc outside the knob.
    pub modulation: Option<ModArc>,
    /// The knob does nothing (`dis`): `text.4` name and value, dark knob.
    pub disabled: bool,
}

/// What a knob cell is: the name above the value, below a knob of Ø 28 at the top
/// (`W_knob`: `padding-top:2px`, `gap:2px`, name `max-width:52px`).
#[derive(Clone, Copy, Debug)]
pub struct KnobCellLayout {
    /// The knob's square.
    pub knob: Rect,
    /// The name line (centred in it).
    pub name_y: f32,
    /// The value line.
    pub value_y: f32,
}

impl KnobCellLayout {
    /// The layout in the cell `cell` (56 wide).
    pub fn new(cell: Rect) -> Self {
        let svg = widget::knob::SVG;
        let d = widget::knob::DIAMETER;
        let top = cell.top() + super::metrics::device::KNOB_CELL_PAD_TOP;
        let knob = Rect::from_center_size(pos2(cell.center().x, top + svg / 2.0), vec2(d, d));
        let name_y = top + svg + super::metrics::device::KNOB_CELL_GAP;
        let value_y = name_y + LABEL.line_height + super::metrics::device::KNOB_CELL_GAP;
        Self {
            knob,
            name_y,
            value_y,
        }
    }
}

/// Paints the static parts of a knob cell (learn ground, name with its dot, value) for a knob
/// that [`Knob`] draws in `layout.knob`; `value` in `text.1`.
pub fn paint_knob_cell(
    painter: &Painter,
    cell: Rect,
    layout: &KnobCellLayout,
    name: &str,
    value: &str,
    options: &KnobCellOptions,
) {
    let ppp = painter.pixels_per_point();
    if options.learn {
        px::fill(painter, cell, radius::M, color32(color::ACCENT_SURFACE));
        px::outline(painter, cell, radius::M, -1.0, color32(color::ACCENT));
    }
    let max_w = cell.width() - 2.0 * super::metrics::device::KNOB_CELL_NAME_INSET;
    let name_col = if options.disabled {
        color::TEXT_4
    } else {
        color::TEXT_2
    };
    let dot = options
        .automation_dot
        .map(|c| (widget::knob::automation_dot::SIZE, c));
    let dot_w = dot.map_or(0.0, |(s, _)| s + super::metrics::device::KNOB_CELL_DOT_GAP);
    let g = elided_galley(painter, name, &LABEL, None, max_w - dot_w);
    let total = dot_w + g.size().x;
    let x0 = cell.center().x - total / 2.0;
    if let Some((s, c)) = dot {
        let center = pos2(x0 + s / 2.0, layout.name_y + LABEL.line_height / 2.0);
        painter.circle_filled(center, s / 2.0, c);
    }
    painter.galley(
        pos2(px::snap(x0 + dot_w, ppp), px::snap(layout.name_y, ppp)),
        g,
        color32(name_col),
    );
    let val_col = if options.disabled {
        color::TEXT_4
    } else {
        color::TEXT_1
    };
    paint_centered(
        painter,
        pos2(cell.center().x, layout.value_y),
        value,
        &VALUE,
        None,
        cell.width(),
        color32(val_col),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::params::ParamId;
    use komponist_model::params::layout::{
        CellParam, GraphBinding, GraphConfig, MAX_PAGES, WidgetKind,
    };

    #[test]
    fn device_widths_match_the_grid() {
        assert_eq!(device_width(4), 252.0);
        assert_eq!(device_width(7), 432.0);
        assert_eq!(device_width(10), 612.0);
        assert_eq!(cell_width(2), 116.0);
    }

    #[test]
    fn page_limit_matches_the_design_tokens() {
        assert_eq!(MAX_PAGES as f32, widget::pages::MAX_PAGES);
    }

    #[test]
    fn full_and_half_cells_sit_on_the_grid() {
        let knob = Cell::knob(2, 1, ParamId::new("a"));
        assert_eq!(
            cell_rect(&knob),
            Rect::from_min_size(pos2(128.0, 76.0), vec2(56.0, 64.0))
        );
        let top = Cell::half(0, 2, 2, Half::Top, WidgetKind::Number, CellParam::None);
        let bottom = Cell::half(0, 2, 2, Half::Bottom, WidgetKind::Segment, CellParam::None);
        assert_eq!(
            cell_rect(&top),
            Rect::from_min_size(pos2(8.0, 144.0), vec2(116.0, 30.0))
        );
        assert_eq!(
            cell_rect(&bottom),
            Rect::from_min_size(pos2(8.0, 178.0), vec2(116.0, 30.0))
        );
        let tall = Cell::span(0, 0, 1, 3, WidgetKind::MiniFader, CellParam::None);
        assert_eq!(cell_rect(&tall).height(), 200.0);
    }

    #[test]
    fn a_graph_spans_the_full_row_and_top_half_of_the_next() {
        let graph = Cell::graph_half(0, 0, 4, 1, GraphBinding::new(GraphConfig::FilterVa));
        assert_eq!(
            cell_rect(&graph),
            Rect::from_min_size(pos2(8.0, 8.0), vec2(236.0, 98.0))
        );
    }
}
