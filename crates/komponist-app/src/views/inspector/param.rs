//! The parameter section (WP-89): the parameter of a device the user touched last, with its name,
//! where it sits (device · band or operator · track), the value, the default and the range
//! (device handoff §9). Read-only; the buttons for automation, MIDI learn and macros are prepared
//! and stay hidden until a later wave specifies them.

use egui::{Rangef, Rect, Sense, Ui, pos2, vec2};
use komponist_model::automation::{AutomationOwner, AutomationTarget};
use komponist_tokens::{color, typography};

use super::{
    Meta, access_label, field_grid, field_key, frame, label, metrics as m, read_only_field,
};
use crate::views::ViewCtx;
use crate::views::devices::model::{self as devices, TouchedParam};

/// The parameter of the selected track the user touched last, if its device still runs.
pub(super) fn touched(ctx: &ViewCtx<'_>, track: komponist_model::TrackId) -> Option<TouchedParam> {
    let touch = ctx.ui.devices.touched.as_ref()?;
    let found = devices::touched(
        ctx.lang,
        ctx.project,
        ctx.engine.host(),
        &ctx.ui.plugin_windows,
        touch,
    )?;
    (found.track == Some(track)).then_some(found)
}

/// The section for `param`.
pub(super) fn section(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    x: Rangef,
    y: &mut f32,
    param: &TouchedParam,
) {
    let t = ctx.lang;
    let owner = param
        .track
        .map_or(AutomationOwner::Master, AutomationOwner::Track);
    let Some(touch) = &ctx.ui.devices.touched else {
        return;
    };
    let target = AutomationTarget::Param {
        device: touch.device,
        param: touch.param.clone(),
    };
    let resolved = crate::views::automation::Catalog::new(ctx, owner)
        .and_then(|catalog| catalog.resolve(&target).ok());
    let value = crate::views::automation::automation_controls(
        ctx.project,
        owner,
        &target,
        ctx.engine.playhead(),
    )
    .and_then(|value| resolved.as_ref().map(|target| target.format(value)));
    let lane = crate::views::automation::lanes(ctx.project, owner)
        .and_then(|a| a.lane(&target))
        .map(|lane| (lane.id, lane.points.as_slice().len(), lane.mode));
    let mut actions = Vec::new();
    frame(
        ui,
        x,
        y,
        t.inspector_section_parameter(),
        Meta::None,
        |ui, col| {
            let name = typography::UI;
            let row = col.take(name.line_height);
            label(
                ui,
                row,
                &param.name,
                &name,
                Some(komponist_ui::widgets::metrics::SEMIBOLD),
                color::TEXT_1,
            );
            access_label(ui, row, "param_name", &param.name);
            let small = typography::SMALL;
            let row = col.take(small.line_height);
            label(ui, row, &param.place, &small, None, color::TEXT_3);
            access_label(ui, row, "param_place", &param.place);

            let cells = field_grid(col, 2);
            for ((key, field), (k, v)) in cells.into_iter().zip([
                (
                    t.inspector_param_value(),
                    value.as_ref().unwrap_or(&param.value),
                ),
                (t.inspector_param_default(), &param.default),
            ]) {
                field_key(ui, key, k);
                read_only_field(ui, field, k, v, "");
            }
            let area = col.take(m::FIELD_HEIGHT);
            let key = Rect::from_min_size(area.min, vec2(area.width(), small.line_height));
            let field = Rect::from_min_max(
                egui::pos2(area.left(), key.bottom() + m::FIELD_KEY_GAP),
                area.max,
            );
            field_key(ui, key, t.inspector_param_range());
            read_only_field(ui, field, t.inspector_param_range(), &param.range, "");
            if let Some(target) = &resolved {
                let area = col.take(crate::views::automation::CHIP_HEIGHT);
                let left = Rect::from_min_max(
                    area.min,
                    pos2(
                        area.right() - crate::views::automation::MODE_WIDTH,
                        area.bottom(),
                    ),
                );
                let r = ui.interact(left, ui.scope_id().with("show-automation"), Sense::click());
                r.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::Role::Button, true, t.automation_show())
                });
                let title = if let Some((_, points, _)) = lane {
                    format!("{} · {}", t.automation_title(), t.automation_points(points))
                } else {
                    t.automation_show().to_owned()
                };
                let galley = crate::views::elided_galley(
                    ui,
                    &title,
                    komponist_ui::fonts::font_id(&small),
                    small.line_height,
                    left.width(),
                );
                let y = area.center().y - small.line_height / 2.0;
                ui.painter().galley(
                    pos2(left.left(), y),
                    galley,
                    komponist_tokens::egui_support::color32(color::TEXT_2),
                );
                if r.clicked() {
                    actions.push(crate::views::automation::Action::ShowTarget {
                        owner,
                        replace: None,
                        target: target.key.clone(),
                        label: target.label.clone(),
                    });
                }
                if let Some((id, _, mode)) = lane {
                    let chip = Rect::from_min_max(pos2(left.right(), area.top()), area.max);
                    let widget = ui.scope_id().with("automation-mode");
                    if let Some(mode) = crate::views::automation::mode_chip(
                        ui,
                        chip,
                        widget,
                        t,
                        &target.label,
                        mode,
                    ) {
                        actions.push(crate::views::automation::Action::Mode(owner, id, mode));
                    }
                }
            }
        },
    );
    for action in actions {
        ctx.send(crate::actions::Action::Automation(action));
    }
}
