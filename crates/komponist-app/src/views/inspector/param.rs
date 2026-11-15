//! The parameter section (WP-89): the parameter of a device the user touched last, with its name,
//! where it sits (device · band or operator · track), the value, the default and the range
//! (device handoff §9). Read-only; the buttons for automation, MIDI learn and macros are prepared
//! and stay hidden until a later wave specifies them.

use egui::{Rangef, Rect, Ui, vec2};
use komponist_tokens::{color, typography};

use super::model::names;
use super::{
    Meta, access_label, field_grid, field_key, frame, label, metrics as m, read_only_field,
};
use crate::views::ViewCtx;
use crate::views::devices::model::{self as devices, TouchedParam};

/// The parameter of the selected track the user touched last, if its device still runs.
pub(super) fn touched(ctx: &ViewCtx<'_>, track: komponist_model::TrackId) -> Option<TouchedParam> {
    let touch = ctx.ui.devices.touched.as_ref()?;
    let found = devices::touched(
        ctx.project,
        ctx.engine.host(),
        &ctx.ui.plugin_windows,
        touch,
    )?;
    (found.track == Some(track)).then_some(found)
}

/// The section for `param`.
pub(super) fn section(ui: &mut Ui, x: Rangef, y: &mut f32, param: &TouchedParam) {
    frame(ui, x, y, names::PARAMETER, Meta::None, |ui, col| {
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
            (names::VALUE, &param.value),
            (names::DEFAULT, &param.default),
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
        field_key(ui, key, names::RANGE);
        read_only_field(ui, field, names::RANGE, &param.range, "");
    });
}
