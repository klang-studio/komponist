//! The value field of the step sequencer's toolbar as the screens draw it (`Screens.dc.html`
//! dock markup: "Länge 16 Steps", "Swing 12 %", "Vel 52", "Wahr. 50 %", "Ratch. 1"): 24 high,
//! `surface.2`, `radius.m`, padding 0 8, the key left (`type.value.s`, `text.3`) and the value
//! with its unit right (`type.value.m`, `text.1`).
//!
//! The design system's number field (`komponist_ui::widgets::number_field`) has no key inside, so
//! this is a local variant with the number field's looks ([`number_field::look`]) and its drag
//! rules (`ix.number`, [`drag`]): drag ↕ 1 px = 1 step with the pointer hidden and grabbed (Shift
//! fine, Ctrl coarse), the value bar while dragging, Alt-click the default, ↑ ↓ when focused and
//! the mouse wheel when hovered one step, AccessKit increment, decrement and set-value. It has no
//! text entry (contract change request in the report: a key option for `NumberField`).

use egui::accesskit::{Action as AkAction, ActionData};
use egui::{
    Color32, CornerRadius, CursorIcon, Event, EventFilter, Id, Key, Rect, Response, Role, Sense,
    Ui, WidgetInfo, vec2,
};
use komponist_tokens::{radius, size, space, typography};
use komponist_ui::drag::{self, ClickAction, ValueDrag};
use komponist_ui::widgets::number_field;
use komponist_ui::{State, focus, fonts, px, widgets};

/// One field: what it shows and its range.
#[derive(Debug, Clone, Copy)]
pub struct Spec<'a> {
    /// The key shown inside ("Länge").
    pub key: &'a str,
    /// The accessible name ("Länge", "Wahrscheinlichkeit").
    pub name: &'a str,
    /// The value.
    pub value: f64,
    /// Lowest value.
    pub min: f64,
    /// Highest value.
    pub max: f64,
    /// Alt-click value.
    pub default: f64,
    /// Unit after the value ("Steps", "%"), `""` for none.
    pub unit: &'a str,
    /// Width in points.
    pub width: f32,
}

impl Spec<'_> {
    /// The value text with the unit ("16 Steps").
    pub fn text(&self, value: f64) -> String {
        let v = value.round();
        if self.unit.is_empty() {
            format!("{v}")
        } else {
            format!("{v} {}", self.unit)
        }
    }
}

/// What the field did in this pass. Values are whole numbers within the range.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Out {
    /// A drag started (the caller opens a gesture).
    pub drag_started: bool,
    /// The value while dragging.
    pub dragging: Option<f64>,
    /// The drag ended (the caller closes the gesture).
    pub drag_stopped: bool,
    /// A single change: wheel, ↑ ↓, Alt-click, AccessKit.
    pub set: Option<f64>,
}

fn drag_key(id: Id) -> Id {
    id.with("komponist_step_field_drag")
}

/// Adds the field to `ui` (in its layout) and reports what the user did.
pub fn show(ui: &mut Ui, spec: Spec<'_>) -> Out {
    let (rect, response) =
        ui.allocate_exact_size(vec2(spec.width, size::CONTROL), Sense::click_and_drag());
    let mut out = Out::default();
    let ctx = ui.ctx().clone();
    let id = response.id;
    let clamp = |v: f64| v.round().clamp(spec.min, spec.max);
    let mut shown = spec.value;

    if response.clicked() || response.drag_started() {
        response.request_focus();
    }
    if ClickAction::of(&response) == ClickAction::Reset {
        out.set = Some(clamp(spec.default));
    }
    let modifiers = ui.input(|i| i.modifiers);
    if response.drag_started() {
        drag::grab_pointer(&ctx);
        let moved = ui.input(
            |i| match (i.pointer.press_origin(), i.pointer.latest_pos()) {
                (Some(a), Some(b)) => b - a,
                _ => egui::Vec2::ZERO,
            },
        );
        let mut d = ValueDrag::number(spec.value, spec.min, spec.max, 1.0);
        d.drag(moved, modifiers);
        ctx.data_mut(|m| m.insert_temp(drag_key(id), d));
        out.drag_started = true;
        out.dragging = Some(clamp(d.value()));
    } else if response.dragged() {
        let mut d = ctx
            .data(|m| m.get_temp::<ValueDrag>(drag_key(id)))
            .unwrap_or_else(|| ValueDrag::number(spec.value, spec.min, spec.max, 1.0));
        d.drag(drag::pointer_delta(&ctx), modifiers);
        ctx.data_mut(|m| m.insert_temp(drag_key(id), d));
        out.dragging = Some(clamp(d.value()));
    }
    if response.drag_stopped() {
        drag::release_pointer(&ctx);
        ctx.data_mut(|m| m.remove::<ValueDrag>(drag_key(id)));
        out.drag_stopped = true;
    }
    if response.hovered() || response.dragged() {
        ui.set_cursor_icon(CursorIcon::ResizeVertical);
    }

    // Single steps.
    let mut steps = 0.0;
    if response.hovered() {
        steps += f64::from(widgets::wheel_steps(ui, id));
    }
    if response.has_focus() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(
                id,
                EventFilter {
                    vertical_arrows: true,
                    ..Default::default()
                },
            );
        });
        steps += ui.input(|i| {
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
                .sum::<f64>()
        });
    }
    let (ak_steps, ak_set) = ui.input(|i| {
        let steps = i.num_accesskit_action_requests(id, AkAction::Increment) as f64
            - i.num_accesskit_action_requests(id, AkAction::Decrement) as f64;
        let mut set = None;
        for request in i.accesskit_action_requests(id, AkAction::SetValue) {
            if let Some(ActionData::NumericValue(v)) = request.data {
                set = Some(v);
            }
        }
        (steps, set)
    });
    steps += ak_steps;
    if steps != 0.0 {
        // Fine steps accumulate below one; the value changes by whole steps only.
        let v = clamp(spec.value + steps.trunc());
        if v != spec.value {
            out.set = Some(v);
        }
    }
    if let Some(v) = ak_set {
        out.set = Some(clamp(v));
    }
    if let Some(v) = out.dragging.or(out.set) {
        shown = v;
    }

    paint(ui, rect, &response, &spec, shown);
    expose(&response, &spec, shown);
    out
}

fn paint(ui: &Ui, rect: Rect, response: &Response, spec: &Spec<'_>, value: f64) {
    let st = State::of(ui, response);
    let look = number_field::look(st, false);
    let painter = ui.painter();
    let ppp = ui.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    px::fill(painter, r, radius::M, look.fill);
    if let (Some(bar), true) = (look.bar, response.dragged()) {
        let span = (spec.max - spec.min).max(f64::MIN_POSITIVE);
        let fraction = ((value - spec.min) / span).clamp(0.0, 1.0) as f32;
        let w = px::snap(r.width() * fraction, ppp);
        if w > 0.0 {
            let rad = radius::M.round() as u8;
            let full = w >= r.width();
            let cr = CornerRadius {
                nw: rad,
                sw: rad,
                ne: if full { rad } else { 0 },
                se: if full { rad } else { 0 },
            };
            painter.rect_filled(Rect::from_min_size(r.min, vec2(w, r.height())), cr, bar);
        }
    }
    let pad = space::SPACE_2;
    let key = painter.layout_job(fonts::layout_job(
        spec.key,
        &number_field::UNIT_TEXT,
        Color32::PLACEHOLDER,
    ));
    let pos = widgets::text_pos(r, &key, r.left() + pad, ppp);
    painter.galley(pos, key, look.unit);
    let text = spec.text(value);
    let v = painter.layout_job(fonts::layout_job(
        &text,
        &typography::VALUE_M,
        Color32::PLACEHOLDER,
    ));
    let pos = widgets::text_pos(r, &v, r.right() - pad - v.size().x, ppp);
    painter.galley(pos, v, look.value);
    if look.focus_ring {
        focus::paint_ring(painter, r, radius::M);
    }
}

fn expose(response: &Response, spec: &Spec<'_>, value: f64) {
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::SpinButton, true, spec.name);
        info.value = Some(value);
        info.current_text_value = Some(spec.text(value));
        info
    });
    widgets::accesskit(response, |node| {
        node.set_min_numeric_value(spec.min);
        node.set_max_numeric_value(spec.max);
        node.set_numeric_value_step(1.0);
        node.add_action(AkAction::SetValue);
        if value < spec.max {
            node.add_action(AkAction::Increment);
        }
        if value > spec.min {
            node.add_action(AkAction::Decrement);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_text_carries_the_unit() {
        let spec = Spec {
            key: "Länge",
            name: "Länge",
            value: 16.0,
            min: 1.0,
            max: 32.0,
            default: 16.0,
            unit: "Steps",
            width: 108.0,
        };
        assert_eq!(spec.text(16.0), "16 Steps");
        let spec = Spec { unit: "", ..spec };
        assert_eq!(spec.text(52.0), "52");
    }
}
