//! The arrangement's tools in its panel header (`Screens.dc.html` shell markup): "Raster" with
//! the grid dropdown, the Snap icon toggle, then on the right the selection info ("Aufnahme · Lead"
//! while recording), a separator and the zoom buttons − +.

use egui::{Align, Layout, Role, Sense, Ui, WidgetInfo, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::dropdown::Dropdown;
use komponist_ui::{px, widgets as w};

use super::metrics;
use crate::actions::{Action, GridTarget, UiAction};
use crate::views::{ViewCtx, grid};

/// The tools, laid out left to right in `ui` (the header right of the title column).
pub fn show(ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
    ui.spacing_mut().item_spacing = vec2(metrics::TOOL_GAP, 0.0);
    let view = ctx.ui.arrangement;
    let t = ctx.lang;

    let items = grid::items(t);
    let current = grid::index(view.grid);
    let mut selected = current;
    if ui
        .add(
            Dropdown::new(t.view_grid(), &mut selected, &items)
                .label(t.view_grid())
                .compact(),
        )
        .changed()
        && selected != current
    {
        ctx.send(Action::Ui(UiAction::SetGrid {
            target: GridTarget::Arrangement,
            grid: grid::GRIDS[selected],
        }));
    }

    // Icon toggle (owner, 2026-11-15): "Snap" stays the AccessKit name and the tooltip; no
    // shortcut is bound to it, so the tooltip is the name alone.
    let snap = Button::icon(Icon::Magnet, t.view_snap())
        .on(view.snap)
        .compact();
    if ui.add(snap).clicked() {
        ctx.send(Action::Ui(UiAction::ToggleSnap(GridTarget::Arrangement)));
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
            (Icon::Plus, t.view_zoom_in(), 1),
            (Icon::Minus, t.view_zoom_out(), -1),
        ] {
            let action = zoom(steps);
            let shortcut = ctx.keymap.label_for(&action, t);
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
        if let Some(rec) = ctx.recording {
            let tracks: Vec<&str> = rec
                .tracks()
                .filter_map(|t| ctx.project.track(t))
                .map(|t| t.name.as_str())
                .collect();
            info(ui, &ctx.lang.record_info(tracks.join(", ")));
        } else if n > 0 {
            info(ui, &t.status_clips_selected(n));
        }
    });
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
