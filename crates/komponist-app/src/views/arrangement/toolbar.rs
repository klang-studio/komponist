//! The arrangement's tools in its panel header (`Screens.dc.html` shell markup): "Raster" with
//! the grid dropdown, the Snap toggle, then on the right the selection info, a separator and the
//! zoom buttons − +.

use egui::{Align, Layout, Role, Sense, Ui, WidgetInfo, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::{px, widgets as w};

use super::{GRIDS, metrics, names};
use crate::actions::{Action, UiAction};
use crate::views::ViewCtx;

/// The tools, laid out left to right in `ui` (the header right of the title column).
pub fn show(ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
    ui.spacing_mut().item_spacing = vec2(metrics::TOOL_GAP, 0.0);
    let view = ctx.ui.arrangement;

    caption(ui, names::GRID, color::TEXT_3);
    let items: Vec<Item<'_>> = GRIDS.iter().map(|(_, l)| Item::new(l)).collect();
    let current = GRIDS.iter().position(|(g, _)| *g == view.grid).unwrap_or(0);
    let mut selected = current;
    if ui
        .add(Dropdown::new(names::GRID, &mut selected, &items))
        .changed()
        && selected != current
    {
        ctx.send(Action::Ui(UiAction::SetGrid(GRIDS[selected].0)));
    }

    let snap = Button::new(names::SNAP).on(view.snap).compact();
    if ui.add(snap).clicked() {
        ctx.send(Action::Ui(UiAction::ToggleSnap));
    }

    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = metrics::ZOOM_BUTTON_GAP;
        let zoom = |steps: i32| {
            Action::Ui(UiAction::Zoom {
                vertical: false,
                steps,
            })
        };
        for (icon, name, steps) in [
            (Icon::Plus, names::ZOOM_IN, 1),
            (Icon::Minus, names::ZOOM_OUT, -1),
        ] {
            let action = zoom(steps);
            let shortcut = ctx.keymap.label_for(&action);
            let mut button = Button::icon(icon, name).compact();
            if let Some(sc) = shortcut.as_deref() {
                button = button.shortcut(sc);
            }
            if ui.add(button).clicked() {
                ctx.send(action);
            }
        }
        ui.add_space(metrics::TOOL_GAP - metrics::ZOOM_BUTTON_GAP);
        let (rect, _) = ui.allocate_exact_size(
            vec2(
                px::line_width(ui.pixels_per_point()),
                metrics::TOOL_SEPARATOR_HEIGHT,
            ),
            Sense::hover(),
        );
        px::fill(ui.painter(), rect, 0.0, color32(color::LINE_1));
        ui.add_space(metrics::TOOL_GAP - metrics::ZOOM_BUTTON_GAP);
        let n = ctx
            .ui
            .selection
            .clips
            .iter()
            .filter(|c| ctx.project.find_clip(**c).is_some())
            .count();
        if n > 0 {
            info(ui, &names::clips_selected(n));
        }
    });
}

/// A caption in `type.ui` and `color` ("Raster", `text.3`).
fn caption(ui: &mut Ui, text: &str, c: [u8; 4]) {
    let style = typography::UI;
    let galley = w::styled_galley(ui, text, &style);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x, ui.available_height()), Sense::hover());
    let pos = w::text_pos(rect, &galley, rect.left(), ui.pixels_per_point());
    ui.painter().galley(pos, galley, color32(c));
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
}

/// The selection info (`value.s`, `text.3`); AccessKit reads it as a status.
fn info(ui: &mut Ui, text: &str) {
    let style = typography::VALUE_S;
    let galley = w::styled_galley(ui, text, &style);
    let (rect, response) =
        ui.allocate_exact_size(vec2(galley.size().x, ui.available_height()), Sense::hover());
    let pos = w::text_pos(rect, &galley, rect.left(), ui.pixels_per_point());
    ui.painter().galley(pos, galley, color32(color::TEXT_3));
    response.widget_info(|| WidgetInfo::labeled(Role::Status, true, text));
}
