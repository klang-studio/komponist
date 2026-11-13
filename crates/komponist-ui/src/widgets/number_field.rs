//! The draggable number field ("Zahlenfeld, ziehbar"; design system `nf`, `numbers`,
//! `ix.number`), and what all value controls share.
//!
//! [`NumberField`]: h 24 (`size.control`), px 8 (`space.2`), `radius.m`, value in `type.value.m`,
//! unit right-aligned in `text.3`.
//!
//! | Input | What happens (`ix.number`) |
//! |---|---|
//! | Drag ↕ | 1 px = 1 step; the pointer is hidden and grabbed, the value runs in the field and a value bar (`accent.surface`) shows where it is in the range |
//! | Shift / Ctrl drag | fine (step × 0.1, 1 px = 0.1 step) / coarse (step × 10) |
//! | Double-click | text entry with the value preselected; Enter applies, Esc discards, a click elsewhere applies |
//! | Alt-click | the default value |
//! | ↑ ↓ (focused), mouse wheel (hovered) | one step, Shift fine, Ctrl coarse |
//! | Enter (focused) | text entry, for keyboard users |
//!
//! AccessKit: role `SpinButton`, the German name the caller gives, the value as number and as text
//! with unit ("124.00 BPM"), range and step; increment, decrement and set-value requests work.
//!
//! The rest of this module is shared by the value controls ([`super::knob`], [`super::fader`]):
//! the value accessor, the drag gesture with grabbed pointer ([`crate::drag`]), the text entry,
//! keyboard and wheel steps, the AccessKit value properties and the gallery sample helpers.

use std::ops::RangeInclusive;
use std::sync::Arc;

use egui::accesskit::{Action, ActionData};
use egui::emath::Numeric;
use egui::text::CCursorRange;
use egui::{
    Align, Area, Color32, Context, CornerRadius, CursorIcon, Event, EventFilter, Frame, Galley, Id,
    Key, Margin, MouseWheelUnit, Order, Painter, Rect, Response, Role, Sense, Stroke, StrokeKind,
    TextEdit, Ui, Vec2, Widget, WidgetInfo, pos2, vec2,
};
use komponist_model::format::MINUS;
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, radius, size, space};

use crate::drag::{self, ClickAction, ValueDrag};
use crate::state::{self, State};
use crate::theme::corner_radius;
use crate::{focus, fonts, px, tooltip};

/// Sizes of the design (`nf`) that `tokens.json` has no token for (contract change requests).
pub mod metrics {
    /// Default width of a number field (`width: 96`).
    pub const WIDTH: f32 = 96.0;
}

// ---------------------------------------------------------------------------------------------
// The number field

/// Converts a value to text (without unit).
pub type Formatter<'a> = Box<dyn 'a + Fn(f64) -> String>;
/// Parses typed text into a value; `None` if it is not a value.
pub type Parser<'a> = Box<dyn 'a + Fn(&str) -> Option<f64>>;

/// Reads the value (`None`) or writes and reads it back (`Some`), like egui's `DragValue`, so a
/// control works on any number type.
pub(crate) type GetSet<'a> = Box<dyn 'a + FnMut(Option<f64>) -> f64>;

/// The [`GetSet`] of `value`.
pub(crate) fn get_set<N: Numeric>(value: &mut N) -> GetSet<'_> {
    Box::new(move |v| {
        if let Some(v) = v {
            *value = N::from_f64(v);
        }
        value.to_f64()
    })
}

/// A draggable number field: `ui.add(NumberField::new(&mut bpm, "Tempo").unit("BPM"))`.
///
/// The response is `changed()` when the value changed; `drag_started()` / `drag_stopped()` mark
/// a gesture (one undo step for the caller).
pub struct NumberField<'a> {
    get_set: GetSet<'a>,
    name: String,
    unit: String,
    range: RangeInclusive<f64>,
    step: f64,
    default: f64,
    decimals: Option<usize>,
    width: f32,
    format: Option<Formatter<'a>>,
    parse: Option<Parser<'a>>,
}

impl<'a> NumberField<'a> {
    /// A field for `value` with the German accessible `name` ("Tempo"). Range 0..=100, step 1,
    /// default = the range start, until set otherwise.
    pub fn new<N: Numeric>(value: &'a mut N, name: impl Into<String>) -> Self {
        Self {
            get_set: get_set(value),
            name: name.into(),
            unit: String::new(),
            range: 0.0..=100.0,
            step: 1.0,
            default: 0.0,
            decimals: None,
            width: metrics::WIDTH,
            format: None,
            parse: None,
        }
    }

    /// The unit shown on the right ("BPM", "dB"); typing it in text entry is allowed.
    #[must_use]
    pub fn unit(mut self, unit: impl Into<String>) -> Self {
        self.unit = unit.into();
        self
    }

    /// The range; drags, steps and text entry clamp to it.
    #[must_use]
    pub fn range(mut self, range: RangeInclusive<f64>) -> Self {
        self.range = range;
        self
    }

    /// One step: what 1 px of drag, ↑ ↓ and a wheel notch change.
    #[must_use]
    pub fn step(mut self, step: f64) -> Self {
        self.step = step.abs().max(f64::MIN_POSITIVE);
        self
    }

    /// The value Alt-click sets.
    #[must_use]
    pub fn default_value(mut self, default: f64) -> Self {
        self.default = default;
        self
    }

    /// Decimals of the default text format; without it, as many as the step needs.
    #[must_use]
    pub fn decimals(mut self, decimals: usize) -> Self {
        self.decimals = Some(decimals);
        self
    }

    /// Width in points (default [`metrics::WIDTH`]).
    #[must_use]
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// A custom text format (without unit), e.g. [`komponist_model::format::fmt_db`].
    #[must_use]
    pub fn format(mut self, format: impl 'a + Fn(f64) -> String) -> Self {
        self.format = Some(Box::new(format));
        self
    }

    /// A custom parser for text entry, e.g. [`komponist_model::format::parse_db`].
    #[must_use]
    pub fn parse(mut self, parse: impl 'a + Fn(&str) -> Option<f64>) -> Self {
        self.parse = Some(Box::new(parse));
        self
    }

    fn text(&self, value: f64) -> String {
        match &self.format {
            Some(f) => f(value),
            None => format_number(value, self.decimals.unwrap_or(decimals_for(self.step))),
        }
    }

    fn parsed(&self, text: &str) -> Option<f64> {
        match &self.parse {
            Some(p) => p(text),
            None => parse_number(text, &self.unit),
        }
    }
}

/// How the number field looks in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// Background.
    pub fill: Color32,
    /// Value text.
    pub value: Color32,
    /// Unit text.
    pub unit: Color32,
    /// The value bar while dragging (`accent.surface`), if shown.
    pub bar: Option<Color32>,
    /// Text entry: 1 px accent ring inside the field and the value preselected.
    pub editing: bool,
    /// Keyboard focus ring outside the field.
    pub focus_ring: bool,
}

/// The look of a number field in `state`, `editing` when its text entry is open (`numbers`):
/// rest `surface.2`, hover and drag `surface.3` (drag with value bar), text entry `surface.0`
/// with accent ring, disabled `text.4` for value and unit.
pub fn look(state: State, editing: bool) -> Look {
    let base = Look {
        fill: color32(color::SURFACE_2),
        value: color32(color::TEXT_1),
        unit: color32(color::TEXT_3),
        bar: None,
        editing: false,
        focus_ring: state.focused && !state.disabled,
    };
    if state.disabled {
        Look {
            value: color32(color::TEXT_4),
            unit: color32(color::TEXT_4),
            focus_ring: false,
            ..base
        }
    } else if editing {
        Look {
            fill: color32(color::SURFACE_0),
            editing: true,
            focus_ring: false,
            ..base
        }
    } else if state.pressed {
        // The design shows dragging on surface.3 with the value bar, not two steps up.
        Look {
            fill: color32(color::SURFACE_3),
            bar: Some(color32(color::ACCENT_SURFACE)),
            ..base
        }
    } else if state.hovered {
        Look {
            fill: color32(color::SURFACE_3),
            ..base
        }
    } else {
        base
    }
}

/// Text style of the value.
pub const VALUE_TEXT: TypeStyle = typography::VALUE_M;
/// Text style of the unit. The design sets it in Mono 11, which has no token; `value.s` (Mono 10,
/// `text.3`) is the nearest (contract change request).
pub const UNIT_TEXT: TypeStyle = typography::VALUE_S;

/// Paints a number field into `rect`: background, value bar up to `fraction` (0–1) of the width
/// if the look has one, value text left, unit right, rings. With `look.editing` and `value` not
/// empty, the value is painted preselected (still pictures of the text entry; the live field
/// paints an empty `value` and shows a text edit instead).
pub fn paint(painter: &Painter, rect: Rect, look: &Look, value: &str, unit: &str, fraction: f32) {
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    px::fill(painter, r, radius::M, look.fill);
    if let Some(bar) = look.bar {
        let w = px::snap(r.width() * fraction.clamp(0.0, 1.0), ppp);
        if w > 0.0 {
            let rad = radius::M.round() as u8;
            let full = w >= r.width();
            let cr = CornerRadius {
                nw: rad,
                sw: rad,
                ne: if full { rad } else { 0 },
                se: if full { rad } else { 0 },
            };
            let bar_rect = Rect::from_min_size(r.min, vec2(w, r.height()));
            painter.rect_filled(bar_rect, cr, bar);
        }
    }
    let pad = space::SPACE_2;
    if !unit.is_empty() {
        let g = painter.layout_job(fonts::layout_job(unit, &UNIT_TEXT, Color32::PLACEHOLDER));
        let pos = text_pos(r, &g, r.right() - pad - g.size().x, ppp);
        painter.galley(pos, g, look.unit);
    }
    if !value.is_empty() {
        let g = painter.layout_job(fonts::layout_job(value, &VALUE_TEXT, Color32::PLACEHOLDER));
        let pos = text_pos(r, &g, r.left() + pad, ppp);
        if look.editing {
            // `background: accent.surface; padding: 0 1px` around the preselected value.
            let sel = Rect::from_min_size(pos, g.size()).expand2(vec2(px::line_width(ppp), 0.0));
            painter.rect_filled(sel, CornerRadius::ZERO, color32(color::ACCENT_SURFACE));
        }
        painter.galley(pos, g, look.value);
    }
    if look.editing {
        paint_entry_ring(painter, r, radius::M);
    }
    if look.focus_ring {
        focus::paint_ring(painter, r, radius::M);
    }
}

/// The 1 px accent ring inside a field in text entry (`box-shadow: inset 0 0 0 1px accent`).
pub(crate) fn paint_entry_ring(painter: &Painter, rect: Rect, radius: f32) {
    let ppp = painter.pixels_per_point();
    painter.rect_stroke(
        px::snap_rect(rect, ppp),
        corner_radius(radius),
        Stroke::new(px::line_width(ppp), color32(color::ACCENT)),
        StrokeKind::Inside,
    );
}

/// The pixel-snapped position that centres `galley` vertically in `rect`, its left edge at `x`.
pub(crate) fn text_pos(rect: Rect, galley: &Galley, x: f32, ppp: f32) -> egui::Pos2 {
    let y = rect.center().y - galley.size().y / 2.0;
    pos2(px::snap(x, ppp), px::snap(y, ppp))
}

impl Widget for NumberField<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let (min, max) = range_bounds(&self.range);
        let size = vec2(self.width, size::CONTROL);
        let (rect, mut response) = ui.allocate_exact_size(size, sense(ui, Sense::click_and_drag()));
        let id = response.id;
        let ctx = ui.ctx().clone();
        let old = (self.get_set)(None);
        let mut value = old;
        let live = !is_still(ui) && response.enabled();

        if live && !entry::is_open(&ctx, id) {
            take_focus_on_press(&response);
            match ClickAction::of(&response) {
                ClickAction::Reset => value = self.default,
                ClickAction::TextEntry => entry::open(&ctx, id, self.text(value)),
                ClickAction::None => {}
            }
            let step = self.step;
            let start = || ValueDrag::number(value, min, max, step);
            let quantise = Quantise { step, min, max };
            if let Some(v) = drag_gesture_with(&response, start, Some(quantise)) {
                value = v;
            }
            let steps = key_steps(ui, &response) + wheel_steps(ui, &response);
            if steps != 0.0 {
                value = snap_to(value + steps * step, step * drag::FINE);
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
        let fraction = normalized(value.clamp(min, max), min, max) as f32;
        let text = self.text(value.clamp(min, max));
        let painter = ui.painter();
        paint(
            painter,
            rect,
            &lk,
            if editing { "" } else { &text },
            &self.unit,
            fraction,
        );
        if editing {
            let unit_w = if self.unit.is_empty() {
                0.0
            } else {
                galley(ui, &self.unit, &UNIT_TEXT).size().x + space::SPACE_1
            };
            let text_rect = Rect::from_min_max(
                pos2(rect.left() + space::SPACE_2, rect.top()),
                pos2(rect.right() - space::SPACE_2 - unit_w, rect.bottom()),
            );
            match entry::show(ui, id, text_rect, &self.name) {
                entry::Outcome::Editing => {}
                entry::Outcome::Cancel => {}
                entry::Outcome::Commit(t) => {
                    if let Some(v) = self.parsed(&t) {
                        value = v;
                    }
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
        let shown = if self.unit.is_empty() {
            text
        } else {
            format!("{text} {}", self.unit)
        };
        expose_value(
            &response,
            Role::SpinButton,
            &self.name,
            value,
            shown,
            min..=max,
            self.step,
        );
        response
    }
}

// ---------------------------------------------------------------------------------------------
// Number text

/// Decimals that show a value of `step` exactly enough: 1 → 0, 0.5 → 1, 0.01 → 2.
pub fn decimals_for(step: f64) -> usize {
    let mut d = 0;
    let mut s = step.abs();
    while d < 6 && (s - s.round()).abs() > 1e-9 {
        s *= 10.0;
        d += 1;
    }
    d
}

/// `value` with `decimals` decimals and the typographic minus (U+2212); never "−0.00".
pub fn format_number(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    match text.strip_prefix('-') {
        Some(rest) if rest.chars().any(|c| c.is_ascii_digit() && c != '0') => {
            format!("{MINUS}{rest}")
        }
        Some(rest) => rest.to_owned(),
        None => text,
    }
}

/// Parses typed number text: optional sign (`+`, `-`, `−`), digits with one `.` or `,`, an
/// optional `unit` suffix; whitespace is ignored. `None` for anything else (no exponents, no
/// `inf`).
pub fn parse_number(text: &str, unit: &str) -> Option<f64> {
    let text: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let unit: String = unit.chars().filter(|c| !c.is_whitespace()).collect();
    let body = if !unit.is_empty() && text.to_lowercase().ends_with(&unit.to_lowercase()) {
        &text[..text.len() - unit.len()]
    } else {
        &text
    };
    let (negative, digits) = match body.strip_prefix(['-', MINUS]) {
        Some(rest) => (true, rest),
        None => (false, body.strip_prefix('+').unwrap_or(body)),
    };
    let mut point = false;
    let mut digit = false;
    for c in digits.chars() {
        match c {
            '0'..='9' => digit = true,
            '.' | ',' if !point => point = true,
            _ => return None,
        }
    }
    if !digit {
        return None;
    }
    let v: f64 = digits.replace(',', ".").parse().ok()?;
    Some(if negative { -v } else { v })
}

// ---------------------------------------------------------------------------------------------
// Shared by the value controls

/// `(min, max)` of a range, ordered.
pub(crate) fn range_bounds(range: &RangeInclusive<f64>) -> (f64, f64) {
    let (a, b) = (*range.start(), *range.end());
    if a <= b { (a, b) } else { (b, a) }
}

/// Where `value` lies in `min..=max`, 0–1.
pub(crate) fn normalized(value: f64, min: f64, max: f64) -> f64 {
    if max > min {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// `value` clamped to `min..=max`; `fallback` if it is not a number.
pub(crate) fn clamp_finite(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if value.is_nan() {
        fallback
    } else {
        value.clamp(min, max)
    }
}

/// `value` rounded to a multiple of `quantum` (float noise from repeated steps).
pub(crate) fn snap_to(value: f64, quantum: f64) -> f64 {
    if quantum > 0.0 {
        (value / quantum).round() * quantum
    } else {
        value
    }
}

/// `true` if the controls here are still pictures: a state is forced around them
/// ([`state::force`]), as in the gallery samples and snapshot scenes.
pub(crate) fn is_still(ui: &Ui) -> bool {
    state::forced(ui).is_some()
}

/// `sense`, or hover only for still pictures ([`is_still`]), so samples take no clicks or focus.
pub(crate) fn sense(ui: &Ui, sense: Sense) -> Sense {
    if is_still(ui) { Sense::hover() } else { sense }
}

/// `text` laid out on one line in `style` (case and letter spacing applied), placeholder colour.
pub(crate) fn galley(ui: &Ui, text: &str, style: &TypeStyle) -> Arc<Galley> {
    ui.painter()
        .layout_job(fonts::layout_job(text, style, Color32::PLACEHOLDER))
}

fn drag_key(id: Id) -> Id {
    id.with("komponist_value_drag")
}

/// The drag gesture of a value control behind `response`: on the first pass of a drag it grabs
/// and hides the pointer ([`drag::grab_pointer`]) and starts the [`ValueDrag`] from `start`
/// (counting the movement since the press, which egui needs to decide on a drag); then it adds
/// the raw pointer movement of each pass ([`drag::pointer_delta`]) at the speed of the held
/// modifiers. Returns the new value while dragging. Releases the pointer when the drag stops.
///
/// Quantised drags ([`ValueDrag::number`]) quantise to the step at the current speed, so
/// Shift-drag moves in 0.1 steps ("Schritt × 0,1") and Ctrl-drag in 10 steps.
pub(crate) fn drag_gesture(response: &Response, start: impl FnOnce() -> ValueDrag) -> Option<f64> {
    drag_gesture_with(response, start, None)
}

/// The quantisation of a drag: its step and range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Quantise {
    pub step: f64,
    pub min: f64,
    pub max: f64,
}

/// [`drag_gesture`] for a quantised drag: the step is refined by the modifiers (Shift 0.1 step,
/// Ctrl 10 steps, counted from the start value), and a drag that hits the end of the range
/// reaches it exactly.
pub(crate) fn drag_gesture_with(
    response: &Response,
    start: impl FnOnce() -> ValueDrag,
    quantise: Option<Quantise>,
) -> Option<f64> {
    let ctx = &response.ctx;
    let key = drag_key(response.id);
    let modifiers = ctx.input(|i| i.modifiers);
    let refine = |d: ValueDrag| match quantise {
        Some(q) => d.with_step(q.step * drag::speed(modifiers)),
        None => d,
    };
    let result = |d: &ValueDrag| match quantise {
        Some(q) => {
            // `with_step(0)` drops the quantisation: the accumulated, clamped value.
            let raw = d.with_step(0.0).value();
            if raw <= q.min || raw >= q.max {
                raw
            } else {
                d.value()
            }
        }
        None => d.value(),
    };
    if response.drag_started() {
        drag::grab_pointer(ctx);
        let moved = ctx.input(
            |i| match (i.pointer.press_origin(), i.pointer.latest_pos()) {
                (Some(a), Some(b)) => b - a,
                _ => Vec2::ZERO,
            },
        );
        let mut d = refine(start());
        d.drag(moved, modifiers);
        ctx.data_mut(|m| m.insert_temp(key, d));
        Some(result(&d))
    } else if response.dragged() {
        let d = ctx.data(|m| m.get_temp::<ValueDrag>(key));
        let mut d = refine(d.unwrap_or_else(start));
        d.drag(drag::pointer_delta(ctx), modifiers);
        ctx.data_mut(|m| m.insert_temp(key, d));
        Some(result(&d))
    } else {
        if response.drag_stopped() {
            drag::release_pointer(ctx);
            ctx.data_mut(|m| m.remove::<ValueDrag>(key));
        }
        None
    }
}

/// Gives the control behind `response` the keyboard focus when it is clicked or dragged, so ↑ ↓
/// work after using the mouse (the ring stays hidden: focus-visible, [`crate::focus`]).
pub(crate) fn take_focus_on_press(response: &Response) {
    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
}

/// ↑ ↓ on the focused control behind `response`: the steps of this pass, with the speed of the
/// modifiers ([`drag::speed`]: Shift 0.1, Ctrl 10). Keeps the arrows from moving the focus.
pub(crate) fn key_steps(ui: &Ui, response: &Response) -> f64 {
    if !response.has_focus() {
        return 0.0;
    }
    ui.memory_mut(|m| {
        m.set_focus_lock_filter(
            response.id,
            EventFilter {
                vertical_arrows: true,
                ..Default::default()
            },
        );
    });
    ui.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                Event::Key {
                    key: Key::ArrowUp,
                    pressed: true,
                    modifiers,
                    ..
                } => drag::step_delta(1.0, 1.0, *modifiers),
                Event::Key {
                    key: Key::ArrowDown,
                    pressed: true,
                    modifiers,
                    ..
                } => drag::step_delta(-1.0, 1.0, *modifiers),
                _ => 0.0,
            })
            .sum()
    })
}

/// Mouse-wheel notches over the control behind `response` (+ up), with the speed of the
/// modifiers. Pixel-based wheels (touchpads) accumulate to whole lines. Swallows the scroll while
/// the pointer is over the control (also egui's smoothed rest of it), so a surrounding scroll
/// area stays put.
pub(crate) fn wheel_steps(ui: &mut Ui, response: &Response) -> f64 {
    if !response.hovered() {
        return 0.0;
    }
    let key = response.id.with("komponist_wheel");
    let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
    let mut acc: f64 = ui.data(|d| d.get_temp(key)).unwrap_or(0.0);
    let mut steps = 0.0;
    ui.input_mut(|i| {
        for event in &i.events {
            if let Event::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } = event
            {
                // Some systems turn Shift + wheel into horizontal scrolling.
                let d = if delta.y != 0.0 { delta.y } else { delta.x };
                let speed = drag::speed(*modifiers);
                match unit {
                    MouseWheelUnit::Line | MouseWheelUnit::Page => {
                        steps += f64::from(d.signum()) * speed;
                    }
                    MouseWheelUnit::Point => {
                        acc += f64::from(d / line);
                        let whole = acc.trunc();
                        acc -= whole;
                        steps += whole * speed;
                    }
                }
            }
        }
        i.smooth_scroll_delta = Vec2::ZERO;
    });
    ui.data_mut(|d| d.insert_temp(key, acc));
    steps
}

/// AccessKit requests for the control `id` in this pass: increments minus decrements, and the
/// last requested value.
pub(crate) fn accesskit_requests(ui: &Ui, id: Id) -> (i32, Option<f64>) {
    ui.input(|i| {
        let steps = i.num_accesskit_action_requests(id, Action::Increment) as i32
            - i.num_accesskit_action_requests(id, Action::Decrement) as i32;
        let mut set = None;
        for request in i.accesskit_action_requests(id, Action::SetValue) {
            if let Some(ActionData::NumericValue(v)) = request.data {
                set = Some(v);
            }
        }
        (steps, set)
    })
}

/// Tells AccessKit what the control behind `response` is: `role`, German `name`, the value as
/// number and as `text` (with unit), range and step, and the actions it takes.
pub(crate) fn expose_value(
    response: &Response,
    role: Role,
    name: &str,
    value: f64,
    text: String,
    range: RangeInclusive<f64>,
    step: f64,
) {
    let enabled = response.enabled();
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(role, enabled, name);
        info.value = Some(value);
        info.current_text_value = Some(text.clone());
        info
    });
    let (min, max) = (*range.start(), *range.end());
    response.ctx.accesskit_node_builder(response.id, |node| {
        node.set_min_numeric_value(min);
        node.set_max_numeric_value(max);
        node.set_numeric_value_step(step);
        if enabled {
            node.add_action(Action::SetValue);
            if value < max {
                node.add_action(Action::Increment);
            }
            if value > min {
                node.add_action(Action::Decrement);
            }
        }
    });
}

/// Text entry of a value control: a text edit over the value, with the text preselected. Enter
/// applies, Esc discards, losing the focus otherwise (a click elsewhere) applies. The open entry
/// and its text live in egui memory under the control's id.
pub(crate) mod entry {
    use super::*;

    fn key(id: Id) -> Id {
        id.with("komponist_value_entry")
    }

    /// An open entry: its text, and whether it opened in this pass.
    #[derive(Clone, Debug, Default)]
    struct Open {
        text: String,
        fresh: bool,
    }

    fn select_all(ctx: &Context, edit: Id, text: &str) {
        let mut st = TextEdit::load_state(ctx, edit).unwrap_or_default();
        st.cursor
            .set_char_range(Some(CCursorRange::select_all_str(text)));
        TextEdit::store_state(ctx, edit, st);
    }

    /// The id of the text edit of control `id`.
    pub fn edit_id(id: Id) -> Id {
        id.with("komponist_value_entry_edit")
    }

    /// Opens the text entry of control `id` with `text`, all of it selected; it takes the
    /// keyboard focus when it is shown.
    pub fn open(ctx: &Context, id: Id, text: String) {
        select_all(ctx, edit_id(id), &text);
        ctx.data_mut(|d| d.insert_temp(key(id), Open { text, fresh: true }));
    }

    /// `true` while the text entry of control `id` is open.
    pub fn is_open(ctx: &Context, id: Id) -> bool {
        ctx.data(|d| d.get_temp::<Open>(key(id))).is_some()
    }

    fn load(ctx: &Context, id: Id) -> Open {
        ctx.data(|d| d.get_temp::<Open>(key(id)))
            .unwrap_or_default()
    }

    /// What became of the entry in this pass.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Outcome {
        /// Still typing.
        Editing,
        /// Enter or focus lost: apply the text.
        Commit(String),
        /// Esc: discard.
        Cancel,
    }

    /// Shows the text edit of the open entry of control `id` in `rect` (`type.value.m`,
    /// `text.1`, no frame). Its accessible name is "`name` Eingabe".
    pub fn show(ui: &mut Ui, id: Id, rect: Rect, name: &str) -> Outcome {
        let Open { mut text, fresh } = load(ui.ctx(), id);
        let edit = edit_id(id);
        let response = ui.place(rect, text_edit(&mut text, edit, rect.width()));
        finish(ui.ctx(), id, &response, text, fresh, name)
    }

    /// Shows the open entry of control `id` floating above `anchor` (the knob, the fader thumb),
    /// in the place of the value tooltip: `surface.0`, accent ring, `radius.s`, as high as the
    /// value tooltip, at least `min_width` wide.
    pub fn show_floating(
        ctx: &Context,
        id: Id,
        anchor: Rect,
        name: &str,
        min_width: f32,
    ) -> Outcome {
        let (pos, align) = tooltip::Placement::Above.anchor(anchor);
        let edit = edit_id(id);
        let Open { mut text, fresh } = load(ctx, id);
        let inner = Area::new(id.with("komponist_value_entry_area"))
            .order(Order::Tooltip)
            .fixed_pos(pos)
            .pivot(align)
            .show(ctx, |ui| {
                let margin = tooltip::value_frame().inner_margin;
                Frame::new()
                    .fill(color32(color::SURFACE_0))
                    .corner_radius(corner_radius(radius::S))
                    .stroke(Stroke::new(
                        px::line_width(ui.pixels_per_point()),
                        color32(color::ACCENT),
                    ))
                    .inner_margin(margin)
                    .show(ui, |ui| {
                        let w = galley(ui, &text, &VALUE_TEXT).size().x.max(min_width);
                        ui.add(text_edit(&mut text, edit, w))
                    })
                    .inner
            })
            .inner;
        finish(ctx, id, &inner, text, fresh, name)
    }

    fn text_edit<'t>(text: &'t mut String, edit: Id, width: f32) -> TextEdit<'t> {
        TextEdit::singleline(text)
            .id(edit)
            .frame(Frame::NONE)
            .margin(Margin::ZERO)
            .font(fonts::font_id(&VALUE_TEXT))
            .text_color(color32(color::TEXT_1))
            .desired_width(width)
            .align(egui::Align2::LEFT_CENTER)
    }

    fn finish(
        ctx: &Context,
        id: Id,
        response: &Response,
        text: String,
        fresh: bool,
        name: &str,
    ) -> Outcome {
        let label = format!("{name} Eingabe");
        ctx.accesskit_node_builder(response.id, |n| n.set_label(label));
        if fresh {
            // Focus now, after the text edit exists: the double-click that opened the entry
            // would otherwise count as a click elsewhere and take the focus away again.
            select_all(ctx, response.id, &text);
            ctx.memory_mut(|m| m.request_focus(response.id));
            ctx.data_mut(|d| d.insert_temp(key(id), Open { text, fresh: false }));
            return Outcome::Editing;
        }
        if response.lost_focus() {
            ctx.data_mut(|d| d.remove::<Open>(key(id)));
            let (enter, escape) =
                ctx.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
            if enter || escape {
                // Back to the control, so the keyboard user goes on from there.
                ctx.memory_mut(|m| m.request_focus(id));
            }
            if escape {
                Outcome::Cancel
            } else {
                Outcome::Commit(text)
            }
        } else {
            ctx.data_mut(|d| d.insert_temp(key(id), Open { text, fresh: false }));
            Outcome::Editing
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Gallery

/// A row of gallery sample columns, `gap` apart, top-aligned, wrapping when the card is too
/// narrow.
pub(crate) fn sample_row(ui: &mut Ui, gap: f32, add: impl FnOnce(&mut Ui)) {
    let layout = egui::Layout::left_to_right(Align::Min).with_main_wrap(true);
    ui.with_layout(layout, |ui| {
        ui.spacing_mut().item_spacing = vec2(gap, space::SPACE_4);
        add(ui);
    });
}

/// A gallery sample column exactly `width` wide (so a wrapping row can place it): contents
/// top-down with `align`, `gap` apart.
pub(crate) fn column<R>(
    ui: &mut Ui,
    width: f32,
    align: Align,
    gap: f32,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    ui.allocate_ui_with_layout(vec2(width, 0.0), egui::Layout::top_down(align), |ui| {
        ui.set_width(width);
        ui.spacing_mut().item_spacing.y = gap;
        add(ui)
    })
    .inner
}

/// Width of `text` set in `style` on one line.
pub(crate) fn text_width(ui: &Ui, text: &str, style: &TypeStyle) -> f32 {
    galley(ui, text, style).size().x
}

/// A caption in `type.value.s` (Mono 10, `text.3`), on one line.
pub(crate) fn caption(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Label::new(fonts::rich_text(text, &typography::VALUE_S)).extend())
}

/// Value text in `type.value.m` and `color`, on one line.
pub(crate) fn value_label(ui: &mut Ui, text: &str, color: [u8; 4]) -> Response {
    let rich = fonts::rich_text(text, &typography::VALUE_M).color(color32(color));
    ui.add(egui::Label::new(rich).extend())
}

/// The samples of the design (`numbers`) in a still picture: label, state, value, unit,
/// text entry.
const SAMPLES: [(&str, State, &str, &str, bool); 6] = [
    ("standard", State::REST, "124.00", "BPM", false),
    ("hover · Cursor ↕", State::HOVERED, "124.00", "BPM", false),
    (
        "ziehen · Wertbalken",
        State::PRESSED,
        "131.50",
        "BPM",
        false,
    ),
    ("texteingabe", State::REST, "124.00", "BPM", true),
    ("fokus", State::FOCUSED, "\u{2212}6.0", "dB", false),
    ("deaktiviert", State::DISABLED, "\u{2212}6.0", "dB", false),
];

/// One still number field, for the gallery: `state` (text entry if `editing`) showing `value`,
/// the value bar at `fraction`. Not interactive; AccessKit sees an image named `name`.
pub fn still(
    ui: &mut Ui,
    name: &str,
    state: State,
    value: &str,
    unit: &str,
    editing: bool,
    fraction: f32,
) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(metrics::WIDTH, size::CONTROL), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Image, false, name));
    paint(
        ui.painter(),
        rect,
        &look(state, editing),
        value,
        unit,
        fraction,
    );
    response
}

/// The card content "Zahlenfeld, ziehbar": the six states of the design, then live fields.
pub fn gallery(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    sample_row(ui, space::SPACE_4, |ui| {
        for (label, st, value, unit, editing) in SAMPLES {
            // The design's drag sample fills 46 % of the field.
            let fraction = if st.pressed { 0.46 } else { 0.0 };
            let width = metrics::WIDTH.max(text_width(ui, label, &typography::VALUE_S));
            column(ui, width, Align::Min, space::SPACE_2, |ui| {
                let name = format!("Zahlenfeld · {label}");
                still(ui, &name, st, value, unit, editing, fraction);
                caption(ui, label);
            });
        }
    });
    live_row(ui);
}

/// The live values of the gallery card, kept in egui memory.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Live {
    tempo: f64,
    volume: f32,
    velocity: i32,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            tempo: 124.0,
            volume: -6.0,
            velocity: 100,
        }
    }
}

fn live_row(ui: &mut Ui) {
    let id = Id::unique("komponist_gallery_number_live");
    let mut live: Live = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        ui.add(
            NumberField::new(&mut live.tempo, "Tempo")
                .unit("BPM")
                .range(20.0..=300.0)
                .step(1.0)
                .decimals(2)
                .default_value(120.0),
        );
        ui.add(db_field(&mut live.volume, "Lautstärke"));
        ui.add(
            NumberField::new(&mut live.velocity, "Velocity")
                .range(1.0..=127.0)
                .default_value(100.0),
        );
        caption(
            ui,
            &format!(
                "live · {} BPM · {} dB · Vel {}",
                format_number(live.tempo, 2),
                komponist_model::format::fmt_db(live.volume),
                live.velocity
            ),
        );
    });
    ui.data_mut(|d| d.insert_temp(id, live));
}

/// A number field for a level in dB: −70 (shown as −∞) … +6, step 0.1, default 0.0, text from
/// [`komponist_model::format::fmt_db`] and [`komponist_model::format::parse_db`].
pub fn db_field<'a>(db: &'a mut f32, name: impl Into<String>) -> NumberField<'a> {
    use komponist_model::format::{fmt_db, parse_db};
    use komponist_model::project::{VOLUME_DB_MAX, VOLUME_DB_MIN};
    NumberField::new(db, name)
        .unit("dB")
        .range(f64::from(VOLUME_DB_MIN)..=f64::from(VOLUME_DB_MAX))
        .step(0.1)
        .default_value(0.0)
        .format(|v| fmt_db(v as f32))
        .parse(|t| {
            parse_db(t).map(|v| {
                if v == f32::NEG_INFINITY {
                    f64::from(VOLUME_DB_MIN)
                } else {
                    f64::from(v)
                }
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_and_parse() {
        assert_eq!(format_number(124.0, 2), "124.00");
        assert_eq!(format_number(-6.0, 1), "\u{2212}6.0");
        assert_eq!(format_number(-0.001, 2), "0.00");
        assert_eq!(format_number(3.0, 0), "3");
        assert_eq!(decimals_for(1.0), 0);
        assert_eq!(decimals_for(0.5), 1);
        assert_eq!(decimals_for(0.01), 2);
        assert_eq!(parse_number("131,5", "BPM"), Some(131.5));
        assert_eq!(parse_number(" 131.5 bpm", "BPM"), Some(131.5));
        assert_eq!(parse_number("\u{2212}6", "dB"), Some(-6.0));
        assert_eq!(parse_number("+6", "dB"), Some(6.0));
        assert_eq!(parse_number("-.5", ""), Some(-0.5));
        for bad in ["", "abc", "1e3", "inf", "1.2.3", "--1", "BPM"] {
            assert_eq!(parse_number(bad, "BPM"), None, "{bad:?}");
        }
    }

    #[test]
    fn looks_follow_the_design() {
        let rest = look(State::REST, false);
        assert_eq!(rest.fill, color32(color::SURFACE_2));
        assert_eq!(rest.unit, color32(color::TEXT_3));
        assert_eq!(rest.bar, None);
        assert_eq!(look(State::HOVERED, false).fill, color32(color::SURFACE_3));
        let drag = look(State::PRESSED, false);
        assert_eq!(drag.fill, color32(color::SURFACE_3));
        assert_eq!(drag.bar, Some(color32(color::ACCENT_SURFACE)));
        let edit = look(State::REST, true);
        assert_eq!(edit.fill, color32(color::SURFACE_0));
        assert!(edit.editing && !edit.focus_ring);
        assert!(look(State::FOCUSED, false).focus_ring);
        let dis = look(State::DISABLED, false);
        assert_eq!(
            (dis.value, dis.unit),
            (color32(color::TEXT_4), color32(color::TEXT_4))
        );
        assert_eq!(dis.fill, color32(color::SURFACE_2));
    }

    #[test]
    fn helpers() {
        assert_eq!(range_bounds(&(5.0..=1.0)), (1.0, 5.0));
        assert_eq!(normalized(15.0, 10.0, 20.0), 0.5);
        assert_eq!(normalized(1.0, 1.0, 1.0), 0.0);
        assert_eq!(clamp_finite(f64::NAN, 0.0, 1.0, 0.3), 0.3);
        assert!((snap_to(124.30000000001, 0.1) - 124.3).abs() < 1e-12);
    }
}
