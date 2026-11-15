//! The flat knob ("Drehregler, flach"; design system `pt`, `arc`, `knob`, `knobs`, `ix.knob`).
//!
//! Ø 28 (`size.knob`) or 20 (`size.knob-small`): a 270° track arc, the value arc on it (from the
//! start, or from the centre for bipolar values such as pan), a cap and an indicator line.
//!
//! | Input | What happens (`ix.knob`) |
//! |---|---|
//! | Drag ↕ | 200 px = the full range; the pointer is hidden and grabbed; the value tooltip shows above the knob at once |
//! | Shift / Ctrl | fine × 0.1 / coarse × 10, as the number field |
//! | Double-click, Alt-click | the default value (pan: centre); owner decision 2026-11-15 |
//! | Enter (focused), context menu "Wert eingeben" ([`super::value_menu`]) | text entry in the place of the tooltip; Enter applies, Esc discards |
//! | Wheel (hovered) | 1 % of the range per notch, Shift fine, Ctrl coarse; touchpads accumulate (owner decision 2026-11-15) |
//! | ↑ ↓ (focused) | 1 % of the range, Shift fine, Ctrl coarse (keyboard access, not in `ix.knob`) |
//!
//! Keyboard focus shows as an accent value arc, as the design's "fokus" sample does, not as a
//! ring. AccessKit: role `Slider`, the caller's German name, value as number and text.
//!
//! [`Knob::bypassed`] shows the knob of a bypassed device (S05 `card()` with `byp`): still, in
//! [`bypassed_look`], and disabled for AccessKit. Unlike the design system's disabled knob, whose
//! `surface.2` track would vanish on a device card's `surface.2`, the track stays visible.

use std::ops::RangeInclusive;

use egui::epaint::PathStroke;
use egui::{
    Align, Color32, CursorIcon, Id, Painter, Pos2, Rect, Response, Role, Sense, Shape, Stroke, Ui,
    Vec2, Widget, WidgetInfo, pos2, vec2,
};
use komponist_model::format::{fmt_pan, parse_pan};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, size, space};

use super::metrics::knob as metrics;
use super::number_field::{
    self, Formatter, GetSet, Parser, accesskit_requests, caption, clamp_finite, column,
    drag_gesture, entry, expose_value, format_number, get_set, is_still, key_steps, normalized,
    range_bounds, sample_row, sense, take_focus_on_press, text_width, value_tooltip_live,
    wheel_steps,
};
use crate::drag::{ClickAction, ValueDrag};
use crate::state::State;
use crate::{fonts, px, tooltip};

/// Keyboard and AccessKit step: this part of the range (not in the design; `ix.knob` has no keys).
pub const KEY_STEP: f64 = 0.01;
/// Degrees per segment when the arcs are drawn as polylines.
const SEGMENT_DEGREES: f32 = 3.0;
/// Minimum width of the text entry above the knob (not in the design): two knob diameters,
/// room for "20.0 kHz".
const ENTRY_MIN_WIDTH: f32 = 2.0 * size::KNOB;

/// The two knob sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KnobSize {
    /// Ø 28 (`size.knob`).
    #[default]
    Normal,
    /// Ø 20 (`size.knob-small`).
    Small,
    /// Ø 44 (`widget.knob_large.diameter`): the large knob of a device cell.
    Large,
}

impl KnobSize {
    /// The diameter in points.
    pub fn diameter(self) -> f32 {
        match self {
            Self::Normal => size::KNOB,
            Self::Small => size::KNOB_SMALL,
            Self::Large => komponist_tokens::widget::knob_large::DIAMETER,
        }
    }
}

/// A flat knob: `ui.add(Knob::new(&mut cutoff, "Cutoff"))`, `ui.add(Knob::pan(&mut pan, "Pan"))`.
///
/// The response is `changed()` when the value changed; `drag_started()` / `drag_stopped()` mark
/// a gesture.
pub struct Knob<'a> {
    get_set: GetSet<'a>,
    name: String,
    range: RangeInclusive<f64>,
    default: f64,
    bipolar: bool,
    size: KnobSize,
    format: Option<Formatter<'a>>,
    parse: Option<Parser<'a>>,
    bypassed: bool,
    modulation: Option<ModArc>,
}

/// The modulation arc outside the track (`knob_svg` `mod`, `widget.knob.mod_ring`): the range a
/// modulation moves the value through, as offsets in the 0–1 fraction of the range, in the
/// track's colour. **Prepared, unused** (owner decision 2026-11-14: modulation is not built yet).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModArc {
    /// Offset of the low end from the value (usually negative), as a fraction of the range.
    pub low: f32,
    /// Offset of the high end from the value, as a fraction of the range.
    pub high: f32,
    /// Colour (`track.base`).
    pub color: Color32,
}

impl<'a> Knob<'a> {
    /// A knob for `value` in 0..=1 (default 0) with the German accessible `name` ("Cutoff").
    pub fn new<N: egui::emath::Numeric>(value: &'a mut N, name: impl Into<String>) -> Self {
        Self {
            get_set: get_set(value),
            name: name.into(),
            range: 0.0..=1.0,
            default: 0.0,
            bipolar: false,
            size: KnobSize::Normal,
            format: None,
            parse: None,
            bypassed: false,
            modulation: None,
        }
    }

    /// A pan knob: −1 (L 50) … +1 (R 50), bipolar, Alt-click = centre, text from
    /// [`fmt_pan`] / [`parse_pan`].
    pub fn pan(pan: &'a mut f32, name: impl Into<String>) -> Self {
        Self::new(pan, name)
            .range(-1.0..=1.0)
            .default_value(0.0)
            .bipolar(true)
            .format(|v| fmt_pan(v as f32))
            .parse(|t| parse_pan(t).map(f64::from))
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

    /// The value arc runs from the centre (pan) instead of from the start.
    #[must_use]
    pub fn bipolar(mut self, bipolar: bool) -> Self {
        self.bipolar = bipolar;
        self
    }

    /// The size.
    #[must_use]
    pub fn size(mut self, size: KnobSize) -> Self {
        self.size = size;
        self
    }

    /// Ø 20.
    #[must_use]
    pub fn small(self) -> Self {
        self.size(KnobSize::Small)
    }

    /// The knob of a bypassed device: [`bypassed_look`], no input, disabled for AccessKit.
    #[must_use]
    pub fn bypassed(mut self, bypassed: bool) -> Self {
        self.bypassed = bypassed;
        self
    }

    /// Shows the modulation arc ([`ModArc`]); prepared, unused.
    #[must_use]
    pub fn modulation(mut self, arc: Option<ModArc>) -> Self {
        self.modulation = arc;
        self
    }

    /// The text of a value for tooltip, text entry and AccessKit ("2.4 kHz").
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
            None => format_number(value, 2),
        }
    }

    fn parsed(&self, text: &str) -> Option<f64> {
        match &self.parse {
            Some(p) => p(text),
            None => number_field::parse_number(text, ""),
        }
    }
}

/// Colours of a knob in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Look {
    /// The 270° track.
    pub track: Color32,
    /// The value arc.
    pub arc: Color32,
    /// The cap.
    pub cap: Color32,
    /// The indicator line.
    pub indicator: Color32,
}

/// The look of a knob in `state` (`knobs`): rest arc `text.2` on cap `surface.3`, hover arc
/// `text.1` on `surface.4`, drag arc accent on `surface.4`, keyboard focus arc accent, disabled
/// `text.4` on `surface.2` with the track `surface.2`.
pub fn look(state: State) -> Look {
    let rest = Look {
        track: color32(color::SURFACE_4),
        arc: color32(color::TEXT_2),
        cap: color32(color::SURFACE_3),
        indicator: color32(color::TEXT_1),
    };
    if state.disabled {
        return Look {
            track: color32(color::SURFACE_2),
            arc: color32(color::TEXT_4),
            cap: color32(color::SURFACE_2),
            indicator: color32(color::TEXT_4),
        };
    }
    let lifted = state.hovered || state.pressed;
    Look {
        arc: if state.pressed || state.focused {
            color32(color::ACCENT)
        } else if state.hovered {
            color32(color::TEXT_1)
        } else {
            rest.arc
        },
        cap: if lifted {
            color32(color::SURFACE_4)
        } else {
            rest.cap
        },
        ..rest
    }
}

/// The look of a bypassed device's knob (`card()` with `byp`): arc `text.4`, indicator `text.3`,
/// cap `surface.3`, track `surface.4`.
pub fn bypassed_look() -> Look {
    Look {
        track: color32(color::SURFACE_4),
        arc: color32(color::TEXT_4),
        cap: color32(color::SURFACE_3),
        indicator: color32(color::TEXT_3),
    }
}

/// The angle of a value at `fraction` (0–1) of the range, in degrees from up, clockwise:
/// −135° … +135°.
pub fn angle(fraction: f32) -> f32 {
    let half = metrics::ARC_DEGREES / 2.0;
    -half + metrics::ARC_DEGREES * fraction.clamp(0.0, 1.0)
}

/// The point at `radius` and `degrees` (from up, clockwise) around `center` (design `pt`).
pub fn point(center: Pos2, radius: f32, degrees: f32) -> Pos2 {
    let t = degrees.to_radians();
    pos2(center.x + radius * t.sin(), center.y - radius * t.cos())
}

/// The value arc from `from` to `to` degrees as polyline points; empty below half a degree, as
/// the design's `arc()`.
pub fn arc_points(center: Pos2, radius: f32, from: f32, to: f32) -> Vec<Pos2> {
    let (a0, a1) = if from <= to { (from, to) } else { (to, from) };
    if a1 - a0 < 0.5 {
        return Vec::new();
    }
    let n = ((a1 - a0) / SEGMENT_DEGREES).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| point(center, radius, a0 + (a1 - a0) * i as f32 / n as f32))
        .collect()
}

/// Paints a knob into the square `rect` at `fraction` (0–1) of its range; `bipolar` arcs start at
/// the centre (0°).
pub fn paint(painter: &Painter, rect: Rect, look: &Look, fraction: f32, bipolar: bool) {
    paint_with(painter, rect, look, fraction, bipolar, None);
}

/// [`paint`] with the optional modulation arc: `mod_ring.r_offset` outside the track, between
/// `fraction + low` and `fraction + high` (each clamped to 0–1), `mod_ring.stroke` wide.
pub fn paint_with(
    painter: &Painter,
    rect: Rect,
    look: &Look,
    fraction: f32,
    bipolar: bool,
    modulation: Option<&ModArc>,
) {
    let ppp = painter.pixels_per_point();
    let rect = px::snap_rect(rect, ppp);
    let c = rect.center();
    let half = rect.width().min(rect.height()) / 2.0;
    let r = half - metrics::ARC_INSET;
    let start = angle(0.0);
    let a = angle(fraction);
    let stroke = |col: Color32| PathStroke::new(metrics::ARC_STROKE, col);
    painter.add(Shape::line(
        arc_points(c, r, start, angle(1.0)),
        stroke(look.track),
    ));
    if let Some(m) = modulation {
        use komponist_tokens::widget::knob::mod_ring;
        let pts = arc_points(
            c,
            r + mod_ring::R_OFFSET,
            angle((fraction + m.low).max(0.0)),
            angle((fraction + m.high).min(1.0)),
        );
        if !pts.is_empty() {
            painter.add(Shape::line(pts, PathStroke::new(mod_ring::STROKE, m.color)));
        }
    }
    let from = if bipolar { 0.0 } else { start };
    let value = arc_points(c, r, from, a);
    if !value.is_empty() {
        painter.add(Shape::line(value, stroke(look.arc)));
    }
    painter.circle_filled(c, half - metrics::CAP_INSET, look.cap);
    let tip = point(c, half - metrics::INDICATOR_INSET, a);
    let w = metrics::INDICATOR_STROKE;
    painter.line_segment([c, tip], Stroke::new(w, look.indicator));
    // Round line caps.
    painter.circle_filled(c, w / 2.0, look.indicator);
    painter.circle_filled(tip, w / 2.0, look.indicator);
}

impl Knob<'_> {
    /// [`Knob::bypassed`]: a still knob in [`bypassed_look`], a disabled slider for AccessKit.
    fn bypassed_ui(mut self, ui: &mut Ui) -> Response {
        let (min, max) = range_bounds(&self.range);
        let d = self.size.diameter();
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(d), Sense::hover());
        let value = (self.get_set)(None);
        let fraction = normalized(value, min, max) as f32;
        paint(ui.painter(), rect, &bypassed_look(), fraction, self.bipolar);
        let text = self.text(value);
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::Slider, false, &self.name);
            info.value = Some(value);
            info.current_text_value = Some(text.clone());
            info
        });
        response.ctx.accesskit_node_builder(response.id, |node| {
            node.set_min_numeric_value(min);
            node.set_max_numeric_value(max);
            node.set_disabled();
        });
        response
    }
}

impl Widget for Knob<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        if self.bypassed {
            return self.bypassed_ui(ui);
        }
        let (min, max) = range_bounds(&self.range);
        let d = self.size.diameter();
        let (rect, mut response) =
            ui.allocate_exact_size(Vec2::splat(d), sense(ui, Sense::click_and_drag()));
        let id = response.id;
        let ctx = ui.ctx().clone();
        let old = (self.get_set)(None);
        let mut value = old;
        let live = !is_still(ui) && response.enabled();
        let key_step = (max - min) * KEY_STEP;

        if live && !entry::is_open(&ctx, id) {
            take_focus_on_press(&response);
            if ClickAction::of_control(&response) == ClickAction::Reset {
                value = self.default;
            }
            if let Some(v) = drag_gesture(&response, || ValueDrag::knob(value, min, max)) {
                value = v;
            }
            value += (key_steps(ui, &response) + wheel_steps(ui, &response)) * key_step;
            if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
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
        let value_now = clamp_finite(value, min, max, old);

        let st = State::of(ui, &response);
        paint_with(
            ui.painter(),
            rect,
            &look(st),
            normalized(value_now, min, max) as f32,
            self.bipolar,
            self.modulation.as_ref(),
        );
        if live && entry::is_open(&ctx, id) {
            let min_w = ENTRY_MIN_WIDTH;
            if let entry::Outcome::Commit(t) =
                entry::show_floating(&ctx, id, rect, &self.name, min_w)
                && let Some(v) = self.parsed(&t)
            {
                value = v;
            }
        } else if live && value_tooltip_live(ui, &response) {
            tooltip::show_value(
                &ctx,
                id,
                rect,
                tooltip::Placement::Above,
                &self.text(value_now),
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

/// One still knob in `rect` (a square): `state`, at `fraction` of its range, for the gallery.
/// Not interactive; AccessKit sees an image named `name`.
pub fn still(
    ui: &mut Ui,
    rect: Rect,
    name: &str,
    state: State,
    fraction: f32,
    bipolar: bool,
) -> Response {
    let response = ui.allocate_rect(rect, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Image, false, name));
    paint(ui.painter(), rect, &look(state), fraction, bipolar);
    response
}

/// The value tooltip as a still picture (inline, not floating).
fn still_tooltip(ui: &mut Ui, text: &str) {
    tooltip::value_frame().show(ui, |ui| {
        let rich = fonts::rich_text(text, &tooltip::VALUE_TEXT).color(color32(color::SURFACE_0));
        ui.add(egui::Label::new(rich).extend());
    });
}

/// The samples of the design (`knobs`): name, label, state, fraction, bipolar, size, tooltip.
const SAMPLES: [(&str, &str, State, f32, bool, KnobSize, &str); 9] = [
    (
        "Cutoff",
        "standard",
        State::REST,
        0.35,
        false,
        KnobSize::Normal,
        "",
    ),
    (
        "Cutoff",
        "hover",
        State::HOVERED,
        0.35,
        false,
        KnobSize::Normal,
        "",
    ),
    (
        "Cutoff",
        "ziehen",
        State::PRESSED,
        0.58,
        false,
        KnobSize::Normal,
        "2.4 kHz",
    ),
    (
        "Pan",
        "bipolar",
        State::REST,
        0.36,
        true,
        KnobSize::Normal,
        "",
    ),
    ("Pan", "mitte", State::REST, 0.5, true, KnobSize::Normal, ""),
    (
        "Res",
        "klein · 20",
        State::REST,
        0.7,
        false,
        KnobSize::Small,
        "",
    ),
    (
        "Cutoff",
        "fokus",
        State::FOCUSED,
        0.35,
        false,
        KnobSize::Normal,
        "",
    ),
    (
        "Cutoff",
        "deaktiviert",
        State::DISABLED,
        0.35,
        false,
        KnobSize::Normal,
        "",
    ),
    // Not a design-system sample: the device card's bypassed knob (S05 `card()` with `byp`).
    (
        "Cutoff",
        "bypass",
        State::REST,
        0.35,
        false,
        KnobSize::Normal,
        "",
    ),
];

/// One sample column (`min-width: 56`, items centred, `space.1.5` apart): tooltip slot, a cell
/// of the normal knob size with the knob of `size` at its bottom centre (the design's columns
/// align at their bottom edge), name, label.
fn sample_column(
    ui: &mut Ui,
    name: &str,
    label: &str,
    tip: &str,
    size: KnobSize,
    add: impl FnOnce(&mut Ui, Rect),
) {
    // The design sets the name in Sans 10 `text.2`, which has no token; `type.small`
    // (Sans 11, `text.2`) is the nearest.
    let name_style = typography::SMALL;
    let width = metrics::SAMPLE_MIN_WIDTH
        .max(text_width(ui, name, &name_style))
        .max(text_width(ui, label, &typography::VALUE_S));
    column(ui, width, Align::Center, space::SPACE_1_5, |ui| {
        let (slot, _) = ui.allocate_exact_size(vec2(width, size::CONTROL_COMPACT), Sense::hover());
        if !tip.is_empty() {
            let layout = egui::Layout::top_down(Align::Center);
            ui.scope_builder(egui::UiBuilder::new().max_rect(slot).layout(layout), |ui| {
                still_tooltip(ui, tip);
            });
        }
        let (cell, _) = ui.allocate_exact_size(Vec2::splat(size::KNOB), Sense::hover());
        let d = size.diameter();
        let knob = Rect::from_min_size(
            pos2(cell.center().x - d / 2.0, cell.bottom() - d),
            Vec2::splat(d),
        );
        add(ui, knob);
        ui.add(egui::Label::new(fonts::rich_text(name, &name_style)).extend());
        caption(ui, label);
    });
}

/// The card content "Drehregler, flach": the eight samples of the design, then live knobs.
pub fn gallery(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    sample_row(ui, space::SPACE_6, |ui| {
        for (name, label, st, fraction, bipolar, size, tip) in SAMPLES {
            sample_column(ui, name, label, tip, size, |ui, rect| {
                let name = format!("{name} · {label}");
                if label == "bypass" {
                    let r = ui.allocate_rect(rect, Sense::hover());
                    r.widget_info(|| WidgetInfo::labeled(Role::Image, false, &name));
                    paint(ui.painter(), rect, &bypassed_look(), fraction, bipolar);
                } else {
                    still(ui, rect, &name, st, fraction, bipolar);
                }
            });
        }
    });
    live_row(ui);
}

/// Cutoff of the live sample: 0–1 → 20 Hz … 20 kHz, exponential.
fn cutoff_hz(v: f64) -> f64 {
    20.0 * 1000f64.powf(v.clamp(0.0, 1.0))
}

fn fmt_hz(v: f64) -> String {
    let hz = cutoff_hz(v);
    if hz >= 1000.0 {
        format!("{} kHz", format_number(hz / 1000.0, 1))
    } else {
        format!("{} Hz", format_number(hz, 0))
    }
}

fn parse_hz(text: &str) -> Option<f64> {
    let lower = text.trim().to_lowercase();
    let (body, scale) = if let Some(b) = lower.strip_suffix("khz") {
        (b, 1000.0)
    } else if let Some(b) = lower.strip_suffix("hz") {
        (b, 1.0)
    } else {
        (lower.as_str(), 1.0)
    };
    let hz = number_field::parse_number(body, "")? * scale;
    (hz > 0.0).then(|| ((hz / 20.0).ln() / 1000f64.ln()).clamp(0.0, 1.0))
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Live {
    cutoff: f64,
    pan: f32,
    res: f64,
}

impl Default for Live {
    fn default() -> Self {
        Self {
            cutoff: 0.58,
            pan: -0.28,
            res: 0.7,
        }
    }
}

fn live_row(ui: &mut Ui) {
    let id = Id::unique("komponist_gallery_knob_live");
    let mut live: Live = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_6;
        let width = metrics::SAMPLE_MIN_WIDTH;
        column(ui, width, Align::Center, space::SPACE_1_5, |ui| {
            ui.add(
                Knob::new(&mut live.cutoff, "Cutoff")
                    .default_value(0.5)
                    .format(fmt_hz)
                    .parse(parse_hz),
            );
            caption(ui, "Cutoff");
        });
        column(ui, width, Align::Center, space::SPACE_1_5, |ui| {
            ui.add(Knob::pan(&mut live.pan, "Pan"));
            caption(ui, "Pan");
        });
        column(ui, width, Align::Center, space::SPACE_1_5, |ui| {
            ui.add(
                Knob::new(&mut live.res, "Res")
                    .small()
                    .format(|v| format!("{} %", format_number(v * 100.0, 0)))
                    .parse(|t| number_field::parse_number(t, "%").map(|p| p / 100.0)),
            );
            caption(ui, "Res");
        });
        caption(
            ui,
            &format!(
                "live · Cutoff {} · Pan {} · Res {} %",
                fmt_hz(live.cutoff),
                fmt_pan(live.pan),
                format_number(live.res * 100.0, 0)
            ),
        );
    });
    ui.data_mut(|d| d.insert_temp(id, live));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_and_points() {
        assert_eq!(angle(0.0), -135.0);
        assert_eq!(angle(0.5), 0.0);
        assert_eq!(angle(1.0), 135.0);
        assert_eq!(angle(2.0), 135.0);
        let c = pos2(14.0, 14.0);
        let up = point(c, 12.0, 0.0);
        assert!((up.x - 14.0).abs() < 1e-5 && (up.y - 2.0).abs() < 1e-5);
        let right = point(c, 12.0, 90.0);
        assert!((right.x - 26.0).abs() < 1e-5 && (right.y - 14.0).abs() < 1e-5);
        // The design's indicator of the standard sample (v = .35, r = c − 6 = 8).
        let p = point(c, 8.0, angle(0.35));
        assert!(
            (p.x - 8.80).abs() < 0.01 && (p.y - 7.92).abs() < 0.01,
            "{p:?}"
        );
    }

    #[test]
    fn arcs() {
        let c = pos2(0.0, 0.0);
        assert!(
            arc_points(c, 10.0, 0.0, 0.2).is_empty(),
            "pan centre: no arc"
        );
        let full = arc_points(c, 10.0, -135.0, 135.0);
        assert_eq!(full.len(), 91);
        assert_eq!(
            arc_points(c, 10.0, 30.0, -30.0).len(),
            arc_points(c, 10.0, -30.0, 30.0).len()
        );
    }

    #[test]
    fn looks_follow_the_design() {
        assert_eq!(look(State::REST).arc, color32(color::TEXT_2));
        assert_eq!(look(State::REST).cap, color32(color::SURFACE_3));
        assert_eq!(look(State::HOVERED).arc, color32(color::TEXT_1));
        assert_eq!(look(State::HOVERED).cap, color32(color::SURFACE_4));
        assert_eq!(look(State::PRESSED).arc, color32(color::ACCENT));
        assert_eq!(look(State::FOCUSED).arc, color32(color::ACCENT));
        assert_eq!(look(State::FOCUSED).cap, color32(color::SURFACE_3));
        let d = look(State::DISABLED);
        assert_eq!(
            (d.track, d.arc, d.cap, d.indicator),
            (
                color32(color::SURFACE_2),
                color32(color::TEXT_4),
                color32(color::SURFACE_2),
                color32(color::TEXT_4)
            )
        );
    }

    #[test]
    fn the_bypassed_look_keeps_the_track_visible() {
        let b = bypassed_look();
        assert_eq!(
            (b.track, b.arc, b.cap, b.indicator),
            (
                color32(color::SURFACE_4),
                color32(color::TEXT_4),
                color32(color::SURFACE_3),
                color32(color::TEXT_3)
            )
        );
        assert_ne!(b.track, look(State::DISABLED).track);
    }

    #[test]
    fn live_cutoff_text_round_trips() {
        assert_eq!(fmt_hz(0.0), "20 Hz");
        assert_eq!(fmt_hz(1.0), "20.0 kHz");
        let v = parse_hz("2.4 kHz").unwrap();
        assert_eq!(fmt_hz(v), "2.4 kHz");
        assert_eq!(parse_hz("abc"), None);
    }
}
