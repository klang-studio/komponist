//! Vertical dividers shared by clip editor sections.

use std::ops::RangeInclusive;

use egui::{CursorIcon, Id, PointerButton, Rect, Role, Sense, Ui, WidgetInfo};
use komponist_tokens::{color, egui_support::color32, space};
use komponist_ui::px;

/// Drag the top edge of a section whose bottom is anchored by the layout.
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
    let response = ui.interact(hit, id, Sense::drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Splitter, true, label));
    if response.hovered() || response.dragged() {
        ui.set_cursor_icon(CursorIcon::ResizeVertical);
        px::hline(
            ui.painter(),
            section.x_range(),
            section.top(),
            color32(color::ACCENT),
        );
    }

    if response.drag_started_by(PointerButton::Primary) {
        ui.data_mut(|data| data.insert_temp(id, height));
    }
    if response.drag_stopped() {
        ui.data_mut(|data| data.remove::<f32>(id));
    }
    let delta = response.total_drag_delta()?.y;
    let origin = ui.data(|data| data.get_temp::<f32>(id))?;
    let resized = if delta == 0.0 {
        origin
    } else {
        (origin - delta).clamp(*limits.start(), *limits.end())
    };
    if resized == height {
        return None;
    }
    ui.ctx().request_repaint();
    Some(resized)
}
