//! `mini_fader` (1 × 2 or 1 × 3, `W_fader`): the compact fader thumb (24 × 28, from
//! [`super::super::fader`]) on a 4 px track in the middle of the cell, the track filled from the
//! thumb downwards in the track colour (carriers) or `text.2` (modulators); name and value under
//! it, centred.
//!
//! Operation as the knob: drag ↕ (the thumb follows the pointer: the thumb's travel is the range),
//! Shift fine, Ctrl coarse, double-click and Alt-click the default, Enter text entry, ↑ ↓ and wheel,
//! value tooltip above the thumb while dragging. AccessKit: `Slider`.

use std::ops::RangeInclusive;

use egui::{
    Color32, CursorIcon, Id, Key, Painter, Rect, Response, Role, Sense, Ui, Vec2, Widget, pos2,
    vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, widget};

use super::{LABEL, VALUE, cell_height, cell_width, paint_centered};
use crate::drag::{ClickAction, ValueDrag};
use crate::state::State;
use crate::widgets::fader::{self, FaderSize};
use crate::widgets::metrics::device::mini_fader as metrics;
use crate::widgets::number_field::{
    Formatter, GetSet, Parser, accesskit_requests, clamp_finite, drag_gesture, entry, expose_value,
    format_number, get_set, is_still, key_steps, normalized, range_bounds, sense,
    take_focus_on_press, wheel_steps,
};
use crate::{focus, px, tooltip};

/// Share of the range one keyboard step moves.
const KEY_STEP: f64 = 0.01;

/// What the track is filled with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fill {
    /// `text.2`: a modulator.
    Neutral,
    /// The track colour (`track.base`): a carrier.
    Track(Color32),
}

/// The geometry of a mini fader in a cell of `rows` rows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// The cell.
    pub cell: Rect,
    /// Travel of the thumb: `h − 30 − 28` (`trackH`).
    pub travel: f32,
}

impl Layout {
    /// The layout in `cell`.
    pub fn new(cell: Rect) -> Self {
        let [_, th] = widget::mini_fader::THUMB;
        Self {
            cell,
            travel: (cell.height() - widget::mini_fader::LABEL_AREA_H - th).max(1.0),
        }
    }

    /// Top of the thumb at `fraction` (0 = bottom, 1 = top) of the range.
    pub fn thumb_top(&self, fraction: f32) -> f32 {
        self.cell.top() + metrics::TRACK_TOP + (1.0 - fraction.clamp(0.0, 1.0)) * self.travel
    }

    /// The thumb's rectangle at `fraction`.
    pub fn thumb(&self, fraction: f32) -> Rect {
        let [tw, th] = widget::mini_fader::THUMB;
        Rect::from_min_size(
            pos2(self.cell.center().x - tw / 2.0, self.thumb_top(fraction)),
            vec2(tw, th),
        )
    }

    /// The 4 px track: from the thumb's centre at the top to its centre at the bottom.
    pub fn track(&self) -> Rect {
        let [_, th] = widget::mini_fader::THUMB;
        let w = widget::mini_fader::TRACK_W;
        Rect::from_min_size(
            pos2(
                self.cell.center().x - w / 2.0,
                self.cell.top() + metrics::TRACK_TOP + th / 2.0,
            ),
            vec2(w, self.travel),
        )
    }

    /// The filled part of the track: from the thumb's centre to the bottom of the track.
    pub fn filled(&self, fraction: f32) -> Rect {
        let track = self.track();
        let [_, th] = widget::mini_fader::THUMB;
        let top = self.thumb_top(fraction) + th / 2.0;
        Rect::from_min_max(pos2(track.left(), top), track.right_bottom())
    }
}

/// Paints the fader (track, fill, thumb, focus ring) in `cell` at `fraction` (0–1) in `state`.
pub fn paint(painter: &Painter, cell: Rect, fraction: f32, fill: Fill, state: State) {
    let layout = Layout::new(cell);
    let look = fader::look(state);
    let fill = match fill {
        Fill::Neutral => color32(color::TEXT_2),
        Fill::Track(c) => c,
    };
    let fill = if state.disabled {
        color32(color::TEXT_4)
    } else {
        fill
    };
    px::fill(painter, layout.track(), radius::S, look.track);
    px::fill(painter, layout.filled(fraction), radius::S, fill);
    let thumb = layout.thumb(fraction);
    fader::paint_thumb(painter, thumb, FaderSize::Compact, &look);
    if look.focus_ring {
        focus::paint_ring(painter, thumb, 0.0);
    }
}

/// Paints name and value centred at the bottom of the cell (`bottom:0`, `gap:1px`).
pub fn paint_texts(painter: &Painter, cell: Rect, name: &str, value: &str, disabled: bool) {
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
    let value_y = cell.bottom() - VALUE.line_height;
    let name_y = value_y - metrics::TEXT_GAP - LABEL.line_height;
    let w = cell.width();
    paint_centered(
        painter,
        pos2(cell.center().x, name_y),
        name,
        &LABEL,
        None,
        w,
        color32(name_col),
    );
    paint_centered(
        painter,
        pos2(cell.center().x, value_y),
        value,
        &VALUE,
        None,
        w,
        color32(value_col),
    );
}

/// A mini fader: `ui.add(MiniFader::new(&mut level, "Op 1").rows(2).fill(Fill::Neutral))`.
pub struct MiniFader<'a> {
    get_set: GetSet<'a>,
    name: String,
    range: RangeInclusive<f64>,
    default: f64,
    rows: u8,
    fill: Fill,
    format: Option<Formatter<'a>>,
    parse: Option<Parser<'a>>,
}

impl<'a> MiniFader<'a> {
    /// A fader for `value` in 0..=1 named `name` (shown and accessible name).
    pub fn new<N: egui::emath::Numeric>(value: &'a mut N, name: impl Into<String>) -> Self {
        Self {
            get_set: get_set(value),
            name: name.into(),
            range: 0.0..=1.0,
            default: 0.0,
            rows: 2,
            fill: Fill::Neutral,
            format: None,
            parse: None,
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

    /// Cell rows: 2 or 3.
    #[must_use]
    pub fn rows(mut self, rows: u8) -> Self {
        self.rows = rows;
        self
    }

    /// The track's fill.
    #[must_use]
    pub fn fill(mut self, fill: Fill) -> Self {
        self.fill = fill;
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

    /// The size of the cell.
    pub fn size(&self) -> Vec2 {
        vec2(cell_width(1), cell_height(self.rows))
    }

    fn text(&self, v: f64) -> String {
        match &self.format {
            Some(f) => f(v),
            None => format_number(v, 2),
        }
    }
}

impl Widget for MiniFader<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let (min, max) = range_bounds(&self.range);
        let (cell, mut response) =
            ui.allocate_exact_size(self.size(), sense(ui, Sense::click_and_drag()));
        let id: Id = response.id;
        let ctx = ui.ctx().clone();
        let layout = Layout::new(cell);
        let old = (self.get_set)(None);
        let mut value = old;
        let live = !is_still(ui) && response.enabled();
        let key_step = (max - min) * KEY_STEP;

        if live && !entry::is_open(&ctx, id) {
            take_focus_on_press(&response);
            if ClickAction::of_control(&response) == ClickAction::Reset {
                value = self.default;
            }
            let travel = f64::from(layout.travel);
            if let Some(v) = drag_gesture(&response, || ValueDrag::span(value, min, max, travel)) {
                value = v;
            }
            value += (key_steps(ui, &response) + wheel_steps(ui, &response)) * key_step;
            if response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                entry::open(&ctx, id, self.text(value));
            }
            let (ak_steps, ak_set) = accesskit_requests(ui, id);
            value += f64::from(ak_steps) * key_step;
            if let Some(v) = ak_set {
                value = v;
            }
            if response.hovered() || response.dragged() {
                ui.set_cursor_icon(CursorIcon::ResizeVertical);
            }
        }
        let shown = clamp_finite(value, min, max, old);
        let st = State::of(ui, &response);
        let fraction = normalized(shown, min, max) as f32;
        paint(ui.painter(), cell, fraction, self.fill, st);
        paint_texts(
            ui.painter(),
            cell,
            &self.name,
            &self.text(shown),
            st.disabled,
        );
        let thumb = layout.thumb(fraction);
        if live && entry::is_open(&ctx, id) {
            let min_w = thumb.width() * 2.0;
            if let entry::Outcome::Commit(t) =
                entry::show_floating(&ctx, id, thumb, &self.name, min_w)
            {
                let parsed = match &self.parse {
                    Some(p) => p(&t),
                    None => crate::widgets::number_field::parse_number(&t, ""),
                };
                if let Some(v) = parsed {
                    value = v;
                }
            }
        } else if live && response.dragged() {
            tooltip::show_value(
                &ctx,
                id,
                thumb,
                tooltip::Placement::Above,
                &self.text(shown),
            );
        }

        let value = clamp_finite(value, min, max, old);
        if value != old {
            (self.get_set)(Some(value));
            response.mark_changed();
        }
        let value = (self.get_set)(None);
        expose_value(
            &response,
            Role::Slider,
            &self.name,
            value,
            self.text(value),
            min..=max,
            key_step,
        );
        response
    }
}

/// A still fader for the gallery and snapshots: `state`, at `fraction`; with name and value.
pub fn still(
    ui: &mut Ui,
    name: &str,
    value: &str,
    fraction: f32,
    fill: Fill,
    state: State,
    rows: u8,
) -> Response {
    let size = vec2(cell_width(1), cell_height(rows));
    let (cell, response) = ui.allocate_exact_size(size, Sense::hover());
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(Role::Slider, !state.disabled, name);
        info.current_text_value = Some(value.to_owned());
        info
    });
    paint(ui.painter(), cell, fraction, fill, state);
    paint_texts(ui.painter(), cell, name, value, state.disabled);
    if state.pressed {
        // The value tooltip of a drag, as a picture above the thumb.
        let thumb = Layout::new(cell).thumb(fraction);
        still_tooltip(ui, thumb, value);
    }
    response
}

/// The value tooltip as a still picture above `anchor` (the generator's `tip()`: `text.1` ground,
/// `surface.0` text), painted into the layer of `ui`.
pub fn still_tooltip(ui: &mut Ui, anchor: Rect, text: &str) {
    let g = crate::widgets::styled_galley(ui, text, &tooltip::VALUE_TEXT);
    let margin = tooltip::value_frame().inner_margin;
    let size = g.size()
        + vec2(
            f32::from(margin.left + margin.right),
            f32::from(margin.top + margin.bottom),
        );
    let rect = Rect::from_center_size(
        pos2(
            anchor.center().x,
            anchor.top() - tooltip::GAP - size.y / 2.0,
        ),
        size,
    );
    let painter = ui.painter();
    px::fill(painter, rect, radius::S, color32(color::TEXT_1));
    let at = rect.min + vec2(f32::from(margin.left), f32::from(margin.top));
    painter.galley(at, g, color32(color::SURFACE_0));
}
