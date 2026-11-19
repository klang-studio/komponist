//! Vertical dividers shared by clip editor sections.

use std::ops::RangeInclusive;

use egui::accesskit::Action;
use egui::{CursorIcon, Id, Key, Modifiers, PointerButton, Rect, Role, Sense, Ui, WidgetInfo};
use komponist_tokens::{color, egui_support::color32, space};
use komponist_ui::{px, widgets};

/// Resize a section whose bottom is anchored by the layout.
/// Keep the hit area inside the section so the preceding canvas can still be edited to its edge.
pub(super) fn resize_from_top(
    ui: &mut Ui,
    section: Rect,
    id: Id,
    label: &str,
    height: f32,
    limits: RangeInclusive<f32>,
) -> Option<f32> {
    let hit = Rect::from_x_y_ranges(
        section.x_range(),
        section.top()..=(section.top() + space::SPACE_1).min(section.bottom()),
    );
    let response = ui.interact(hit, id, Sense::drag() | Sense::focusable_noninteractive());
    response.widget_info(|| WidgetInfo::labeled(Role::Splitter, true, label));
    let step = space::SPACE_2;
    widgets::accesskit(&response, |node| {
        node.set_numeric_value(f64::from(height));
        node.set_min_numeric_value(f64::from(*limits.start()));
        node.set_max_numeric_value(f64::from(*limits.end()));
        node.set_numeric_value_step(f64::from(step));
        node.add_action(Action::Increment);
        node.add_action(Action::Decrement);
    });
    if response.hovered() || response.dragged() {
        ui.set_cursor_icon(CursorIcon::ResizeVertical);
    }
    if response.hovered() || response.dragged() || response.has_focus() {
        px::hline(
            ui.painter(),
            section.x_range(),
            section.top(),
            color32(color::ACCENT),
        );
    }

    if response.drag_started_by(PointerButton::Primary)
        && let Some(origin) = ui.input(|input| input.pointer.press_origin())
    {
        response.request_focus();
        ui.data_mut(|data| data.insert_temp(id, (height, origin.y)));
    }
    let drag = ui.data(|data| data.get_temp::<(f32, f32)>(id));
    let pointer_height = if response.dragged_by(PointerButton::Primary)
        || response.drag_stopped_by(PointerButton::Primary)
    {
        drag.zip(ui.input(|input| input.pointer.interact_pos()))
            .map(|((height, start_y), pos)| height + start_y - pos.y)
    } else {
        None
    };
    if response.drag_stopped() {
        ui.data_mut(|data| data.remove::<(f32, f32)>(id));
    }
    let focused = response.has_focus();
    let steps = ui.input_mut(|input| {
        let plus = input.num_accesskit_action_requests(id, Action::Increment);
        let minus = input.num_accesskit_action_requests(id, Action::Decrement);
        let up = focused && input.consume_key(Modifiers::NONE, Key::ArrowUp);
        let down = focused && input.consume_key(Modifiers::NONE, Key::ArrowDown);
        plus as f32 - minus as f32 + f32::from(u8::from(up)) - f32::from(u8::from(down))
    });
    let resized =
        (pointer_height.unwrap_or(height) + steps * step).clamp(*limits.start(), *limits.end());
    if resized == height {
        return None;
    }
    ui.ctx().request_repaint();
    Some(resized)
}
