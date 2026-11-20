//! `knob` (1 × 1) and `knob_large` (2 × 1) as device cells: the flat knob of [`super::super::knob`]
//! with its name and value.
//!
//! - **knob** (`W_knob`): Ø 28 at the top (`padding-top:2px`), name (`type.device-label`) and value
//!   (`type.value.s`, `text.1`) centred below, `gap:2px`.
//! - **knob_large** (`W_knobL`): Ø 44 at the left (`padding-left:2px`), name and value in
//!   `type.value.m` (Mono 12/16) to its right, `gap:8px`.
//!
//! Operation is the knob's: drag ↕ (200 px = the range), Shift fine, Ctrl coarse, double-click and
//! Alt-click the default, Enter text entry, value tooltip while dragging. AccessKit: `Slider` with the
//! name and the value text.

use std::ops::RangeInclusive;

use egui::{Rect, Response, Sense, Ui, Widget, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, widget};

use super::{KnobCellLayout, KnobCellOptions, LABEL, cell_height, cell_width, elided_galley};
use crate::px;
use crate::widgets::knob::{Knob, KnobSize};
use crate::widgets::metrics::device as metrics;
use crate::widgets::number_field::{Formatter, Parser, format_number};

/// A knob cell: `ui.add(KnobCell::new(&mut cutoff, "Cutoff").format(fmt_hz))`.
pub struct KnobCell<'a> {
    value: &'a mut f64,
    name: String,
    range: RangeInclusive<f64>,
    default: f64,
    bipolar: bool,
    large: bool,
    format: Option<Formatter<'a>>,
    parse: Option<Parser<'a>>,
    options: KnobCellOptions,
    redirect: bool,
}

impl<'a> KnobCell<'a> {
    /// A cell for `value` in 0..=1 named `name` (also the accessible name).
    pub fn new(value: &'a mut f64, name: impl Into<String>) -> Self {
        Self {
            value,
            name: name.into(),
            range: 0.0..=1.0,
            default: 0.0,
            bipolar: false,
            large: false,
            format: None,
            parse: None,
            options: KnobCellOptions::default(),
            redirect: false,
        }
    }

    /// The range.
    #[must_use]
    pub fn range(mut self, range: RangeInclusive<f64>) -> Self {
        self.range = range;
        self
    }

    /// The value Alt-click sets.
    #[must_use]
    pub fn default_value(mut self, default: f64) -> Self {
        self.default = default;
        self
    }

    /// The arc runs from the centre.
    #[must_use]
    pub fn bipolar(mut self, bipolar: bool) -> Self {
        self.bipolar = bipolar;
        self
    }

    /// The 2 × 1 cell with the Ø 44 knob.
    #[must_use]
    pub fn large(mut self) -> Self {
        self.large = true;
        self
    }

    /// The text of a value.
    #[must_use]
    pub fn format(mut self, format: impl 'a + Fn(f64) -> String) -> Self {
        self.format = Some(Box::new(format));
        self
    }

    /// Parses text entry.
    #[must_use]
    pub fn parse(mut self, parse: impl 'a + Fn(&str) -> Option<f64>) -> Self {
        self.parse = Some(Box::new(parse));
        self
    }

    /// Automation dot, MIDI learn, modulation arc (prepared, nothing sets them yet) and
    /// disabled.
    #[must_use]
    pub fn options(mut self, options: KnobCellOptions) -> Self {
        self.options = options;
        self
    }

    /// Suppress stored changes while reporting an attempted knob turn.
    #[must_use]
    pub fn redirect(mut self, redirect: bool) -> Self {
        self.redirect = redirect;
        self
    }

    /// The size of the cell.
    pub fn size(&self) -> egui::Vec2 {
        vec2(cell_width(if self.large { 2 } else { 1 }), cell_height(1))
    }
}

impl Widget for KnobCell<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = self.size();
        let Self {
            value,
            name,
            range,
            default,
            bipolar,
            large,
            format,
            parse,
            options,
            redirect,
        } = self;
        let (cell, _) = ui.allocate_exact_size(size, Sense::hover());
        let layout = if large {
            let d = widget::knob_large::DIAMETER;
            let knob = Rect::from_min_size(
                pos2(
                    cell.left() + metrics::KNOB_LARGE_PAD_LEFT,
                    cell.center().y - d / 2.0,
                ),
                vec2(d, d),
            );
            KnobCellLayout {
                knob,
                name_y: 0.0,
                value_y: 0.0,
            }
        } else {
            KnobCellLayout::new(cell)
        };
        let text_of = |v: f64| match &format {
            Some(f) => f(v),
            None => format_number(v, 2),
        };
        let mut knob = Knob::new(&mut *value, name.clone())
            .range(range)
            .default_value(default)
            .bipolar(bipolar)
            .size(if large {
                KnobSize::Large
            } else {
                KnobSize::Normal
            })
            .modulation(options.modulation)
            .redirect(redirect)
            .format(text_of);
        if let Some(p) = &parse {
            knob = knob.parse(p);
        }
        if options.learn {
            // The ground lies behind the knob (`knob.learn`: `accent.surface` + inset 1 px accent).
            px::fill(
                ui.painter(),
                cell,
                metrics::FIELD_RADIUS,
                color32(color::ACCENT_SURFACE),
            );
            px::outline(
                ui.painter(),
                cell,
                metrics::FIELD_RADIUS,
                -1.0,
                color32(color::ACCENT),
            );
        }
        // A child `Ui`, not `ui.put`: `put` would move the parent's cursor up to the knob.
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(layout.knob));
        let response = if options.disabled {
            child.add_enabled(false, knob)
        } else {
            child.add(knob)
        };
        let shown = text_of(*value);
        let painter = ui.painter();
        if large {
            paint_large_text(painter, cell, layout.knob, &name, &shown, options.disabled);
        } else {
            let plain = KnobCellOptions {
                learn: false,
                ..options
            };
            super::paint_knob_cell(painter, cell, &layout, &name, &shown, &plain);
        }
        response
    }
}

/// Name and value to the right of a large knob, centred vertically as a block
/// (`W_knobL`: name 12, `gap:2px`, value `type.value.m` 16).
fn paint_large_text(
    painter: &egui::Painter,
    cell: Rect,
    knob: Rect,
    name: &str,
    value: &str,
    disabled: bool,
) {
    let ppp = painter.pixels_per_point();
    let x = knob.right() + metrics::KNOB_LARGE_GAP;
    let room = (cell.right() - x).max(0.0);
    let value_style = typography::VALUE_M;
    let block = LABEL.line_height + metrics::KNOB_LARGE_TEXT_GAP + value_style.line_height;
    let y = cell.center().y - block / 2.0;
    let name_col = if disabled {
        color::TEXT_4
    } else {
        color::TEXT_2
    };
    let value_col = if disabled {
        color::TEXT_4
    } else {
        color::TEXT_1
    };
    let g = elided_galley(painter, name, &LABEL, None, room);
    painter.galley(
        pos2(px::snap(x, ppp), px::snap(y, ppp)),
        g,
        color32(name_col),
    );
    let g = elided_galley(painter, value, &value_style, None, room);
    let y = y + LABEL.line_height + metrics::KNOB_LARGE_TEXT_GAP;
    painter.galley(
        pos2(px::snap(x, ppp), px::snap(y, ppp)),
        g,
        color32(value_col),
    );
}
