//! `number`: the compact number field of a half cell (`W_num`): label above (`type.device-label`),
//! below it a field of 18 px (`surface.3`, hover `surface.4`, text entry `surface.0` with a 1 px
//! accent ring), the value in `type.value.s` `text.1` left, the unit in `type.mono-xxs` `text.2`
//! right, 5 px padding.
//!
//! Operation as the number field of the design system ([`super::super::number_field`]): drag ↕
//! (1 px = 1 step with a `step`, else 200 px = the range), Shift fine, Ctrl coarse, Alt-click the
//! default, double-click or Enter text entry, ↑ ↓ and wheel one step. AccessKit: `SpinButton`.

use std::ops::RangeInclusive;

use egui::emath::Numeric;
use egui::{CursorIcon, Key, Painter, Rect, Response, Role, Sense, Ui, Widget, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, space, widget};

use super::{VALUE, half_cell, paint_label};
use crate::drag::{ClickAction, ValueDrag};
use crate::state::State;
use crate::widgets::metrics::device as metrics;
use crate::widgets::number_field::{
    self, Formatter, GetSet, Parser, Quantise, accesskit_requests, clamp_finite, drag_gesture,
    drag_gesture_with, entry, expose_value, format_number, get_set, is_still, key_steps,
    range_bounds, sense, snap_to, update_pointer_focus, wheel_steps,
};
use crate::{focus, fonts, px};

/// The unit's style: `type.mono-xxs` (`W_num` `font-size:9px`).
pub const UNIT: typography::TypeStyle = typography::MONO_XXS;

/// Share of the range one keyboard step moves without a `step` (as the knob).
const KEY_STEP: f64 = 0.01;

/// A compact number field: `ui.add(Number::new(&mut voices, "Stimmen"))`.
pub struct Number<'a> {
    get_set: GetSet<'a>,
    label: String,
    range: RangeInclusive<f64>,
    step: Option<f64>,
    default: f64,
    unit: String,
    cols: u8,
    format: Option<Formatter<'a>>,
    parse: Option<Parser<'a>>,
}

impl<'a> Number<'a> {
    /// A field for `value` in 0..=1 with the label and German accessible name `label`.
    pub fn new<N: Numeric>(value: &'a mut N, label: impl Into<String>) -> Self {
        Self {
            get_set: get_set(value),
            label: label.into(),
            range: 0.0..=1.0,
            step: None,
            default: 0.0,
            unit: String::new(),
            cols: 1,
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

    /// One step: a drag of 1 px moves by it; without, 200 px are the range.
    #[must_use]
    pub fn step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }

    /// The value Alt-click sets.
    #[must_use]
    pub fn default_value(mut self, default: f64) -> Self {
        self.default = default;
        self
    }

    /// The unit after the value ("ms").
    #[must_use]
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// Width in cells (default 1).
    #[must_use]
    pub fn cols(mut self, cols: u8) -> Self {
        self.cols = cols;
        self
    }

    /// The text of a value (without unit).
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

    fn text(&self, value: f64) -> String {
        match &self.format {
            Some(f) => f(value),
            None => format_number(value, self.step.map_or(2, number_field::decimals_for)),
        }
    }
}

/// Colours of a number field in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Field ground.
    pub fill: egui::Color32,
    /// Value text.
    pub value: egui::Color32,
    /// Unit text.
    pub unit: egui::Color32,
    /// Text entry: `surface.0` ground with the 1 px accent ring.
    pub editing: bool,
    /// Keyboard focus ring.
    pub focus_ring: bool,
}

/// The look in `state`, `editing` or not: `surface.3`, hover and drag `surface.4`, text entry
/// `surface.0` + ring, disabled `text.4`.
pub fn look(state: State, editing: bool) -> Look {
    let fill = if state.disabled {
        color::SURFACE_2
    } else if editing {
        color::SURFACE_0
    } else if state.hovered || state.pressed {
        color::SURFACE_4
    } else {
        color::SURFACE_3
    };
    let (value, unit) = if state.disabled {
        (color::TEXT_4, color::TEXT_4)
    } else {
        (color::TEXT_1, color::TEXT_2)
    };
    Look {
        fill: color32(fill),
        value: color32(value),
        unit: color32(unit),
        editing: editing && !state.disabled,
        focus_ring: state.focused && !state.disabled && !editing,
    }
}

/// Paints the field (not the label) into `field`: ground, value left, unit right. An empty
/// `value` with `look.editing` leaves room for the live text edit.
pub fn paint_field(painter: &Painter, field: Rect, look: &Look, value: &str, unit: &str) {
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(field, ppp);
    px::fill(painter, r, metrics::FIELD_RADIUS, look.fill);
    let pad = widget::number::PADDING_X;
    let mut right = r.right() - pad;
    let inner = painter.with_clip_rect(r);
    if !unit.is_empty() {
        let g = painter.layout_job(fonts::layout_job(unit, &UNIT, egui::Color32::PLACEHOLDER));
        let pos = crate::widgets::text_pos(r, &g, right - g.size().x, ppp);
        right -= g.size().x + space::SPACE_1;
        inner.galley(pos, g, look.unit);
    }
    if !value.is_empty() {
        let g = super::elided_galley(
            painter,
            value,
            &VALUE,
            None,
            (right - r.left() - pad).max(0.0),
        );
        let pos = crate::widgets::text_pos(r, &g, r.left() + pad, ppp);
        inner.galley(pos, g, look.value);
    }
    if look.editing {
        number_field::paint_entry_ring(painter, r, metrics::FIELD_RADIUS);
    }
    if look.focus_ring {
        focus::paint_ring(painter, r, metrics::FIELD_RADIUS);
    }
}

impl Widget for Number<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let (min, max) = range_bounds(&self.range);
        let (cell, outer) = ui.allocate_exact_size(half_cell(self.cols), Sense::hover());
        let field = Rect::from_min_size(
            pos2(cell.left(), cell.top() + widget::number::LABEL_H),
            vec2(cell.width(), widget::number::FIELD_H),
        );
        let id = outer.id.with("field");
        let mut response = ui.interact(field, id, sense(ui, Sense::click_and_drag()));
        let ctx = ui.ctx().clone();
        let old = (self.get_set)(None);
        let mut value = old;
        let live = !is_still(ui) && response.enabled();
        let step = self.step.unwrap_or((max - min) * KEY_STEP);

        if live && !entry::is_open(&ctx, id) {
            update_pointer_focus(&response);
            match ClickAction::of(&response) {
                ClickAction::Reset => value = self.default,
                ClickAction::TextEntry => entry::open(&ctx, id, self.text(value)),
                ClickAction::None => {}
            }
            let dragged = match self.step {
                Some(s) => drag_gesture_with(
                    &response,
                    || ValueDrag::number(value, min, max, s),
                    Some(Quantise { step: s, min, max }),
                ),
                None => drag_gesture(&response, || ValueDrag::knob(value, min, max)),
            };
            if let Some(v) = dragged {
                value = v;
            }
            let steps = key_steps(ui, &response) + wheel_steps(ui, &response);
            if steps != 0.0 {
                value = snap_to(value + steps * step, step * crate::drag::FINE);
            }
            if response.has_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                entry::open(&ctx, id, self.text(value));
            }
            let (ak_steps, ak_set) = accesskit_requests(ui, id);
            value += f64::from(ak_steps) * step;
            if let Some(v) = ak_set {
                value = v;
            }
            if response.hovered() || response.dragged() {
                ui.set_cursor_icon(CursorIcon::ResizeVertical);
            }
        }

        let st = State::of(ui, &response);
        let editing = live && entry::is_open(&ctx, id);
        let lk = look(st, editing);
        let shown = clamp_finite(value, min, max, old);
        let text = self.text(shown);
        paint_label(ui.painter(), cell, &self.label, st.disabled);
        paint_field(
            ui.painter(),
            field,
            &lk,
            if editing { "" } else { &text },
            &self.unit,
        );
        if editing {
            let unit_w = if self.unit.is_empty() {
                0.0
            } else {
                crate::widgets::styled_galley(ui, &self.unit, &UNIT)
                    .size()
                    .x
                    + space::SPACE_1
            };
            let pad = widget::number::PADDING_X;
            let text_rect = Rect::from_min_max(
                pos2(field.left() + pad, field.top()),
                pos2(field.right() - pad - unit_w, field.bottom()),
            );
            if let entry::Outcome::Commit(t) = entry::show(ui, id, text_rect, &self.label) {
                let parsed = match &self.parse {
                    Some(p) => p(&t),
                    None => number_field::parse_number(&t, &self.unit),
                };
                if let Some(v) = parsed {
                    value = v;
                }
            }
        }

        let value = clamp_finite(value, min, max, old);
        if value != old {
            (self.get_set)(Some(value));
            response.mark_changed();
        }
        let value = (self.get_set)(None);
        let text = self.text(value);
        let spoken = if self.unit.is_empty() {
            text
        } else {
            format!("{text} {}", self.unit)
        };
        expose_value(
            &response,
            Role::SpinButton,
            &self.label,
            value,
            spoken,
            min..=max,
            step,
        );
        response
    }
}

/// A still field in `state` for the gallery and snapshots: label, value and unit, not
/// interactive; `editing` shows the text entry (value preselected as `accent.surface`).
pub fn still(
    ui: &mut Ui,
    label: &str,
    value: &str,
    unit: &str,
    state: State,
    editing: bool,
    cols: u8,
) -> Response {
    let (cell, outer) = ui.allocate_exact_size(half_cell(cols), Sense::hover());
    let field = Rect::from_min_size(
        pos2(cell.left(), cell.top() + widget::number::LABEL_H),
        vec2(cell.width(), widget::number::FIELD_H),
    );
    outer.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(Role::SpinButton, !state.disabled, label);
        info.current_text_value = Some(if unit.is_empty() {
            value.to_owned()
        } else {
            format!("{value} {unit}")
        });
        info
    });
    paint_label(ui.painter(), cell, label, state.disabled);
    let lk = look(state, editing);
    paint_field(ui.painter(), field, &lk, value, unit);
    outer
}
