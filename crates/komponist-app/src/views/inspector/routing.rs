//! The „Routing“ section (WP-103; routing proposal 07-inspektor, variant A, owner decision 6 A): the
//! output, the sends with their level, tap and „×“, and, if devices on other tracks take this
//! track as their sidechain source, the line „Sidechain-Quelle für:“. A bus shows its inputs
//! („Eingänge (3 Sends)“) as a list that selects the track.
//!
//! The section is always shown for a track, also before the first bus (owner, 2026-11-15).

use egui::{Rangef, Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_i18n::Lang;
use komponist_model::{Project, Track};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, typography};
use komponist_ui::px;
use komponist_ui::state::{self, State};
use komponist_ui::widgets as w;

use super::{Col, Meta, access_label, field_key, frame, label, metrics as m};
use crate::actions::{Action, SelectMode, SelectionChange, UiAction};
use crate::views::ViewCtx;
use crate::views::mixer::model::base_color;
use crate::views::routing::widgets::{self as rw, RowKind};
use crate::views::routing::{self as rt, metrics as rm};

/// The routing section of `track`.
pub(super) fn section(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
    track: &Track,
) {
    let t = ctx.lang;
    frame(ui, x, y, t.routing_routing(), Meta::None, |ui, col| {
        output_row(ui, ctx, col, track);
        if track.is_bus() {
            inputs(ui, ctx, col, project, track);
        } else {
            sends(ui, ctx, col, project, track);
        }
        sidechain_line(ui, t, col, project, track);
    });
}

/// „Ausgang“ and the output button over the rest of the row.
fn output_row(ui: &mut Ui, ctx: &mut ViewCtx<'_>, col: &mut Col, track: &Track) {
    let t = ctx.lang;
    let row = col.take(size::CONTROL);
    let key = Rect::from_x_y_ranges(row.left()..=row.left() + m::DD_KEY_WIDTH, row.y_range());
    field_key(ui, key, t.routing_output());
    let field = Rect::from_x_y_ranges(
        row.left() + m::DD_KEY_WIDTH + m::DD_COLUMN_GAP..=row.right(),
        row.y_range(),
    );
    rw::output_button(ui, ctx, track, field, "inspector");
}

/// „Sends“, the rows and „+ Send hinzufügen“.
fn sends(ui: &mut Ui, ctx: &mut ViewCtx<'_>, col: &mut Col, project: &Project, track: &Track) {
    let infos = rt::sends_of(project, track);
    let n = infos.len() + 1;
    let height = n as f32 * rm::ROW_HEIGHT + (n - 1) as f32 * rm::ROW_GAP;
    let area = col.take(height);
    let row_at = |i: usize| {
        let top = area.top() + i as f32 * (rm::ROW_HEIGHT + rm::ROW_GAP);
        Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), rm::ROW_HEIGHT))
    };
    for (i, info) in infos.iter().enumerate() {
        rw::send_row(ui, ctx, info, row_at(i), RowKind::Wide);
    }
    rw::add_send_row(ui, ctx, track, row_at(infos.len()), true);
}

/// A bus's inputs: „Eingänge (3 Sends)“ and one row per input, a click selects the track.
fn inputs(ui: &mut Ui, ctx: &mut ViewCtx<'_>, col: &mut Col, project: &Project, bus: &Track) {
    let t = ctx.lang;
    let lang = t;
    let title = format!(
        "{} ({})",
        t.routing_inputs(),
        rt::inputs_line(t, project, bus)
    );
    let head = col.take(typography::SMALL.line_height);
    label(ui, head, &title, &typography::SMALL, None, color::TEXT_3);
    access_label(ui, head, "inputs", &title);
    let inputs = rt::inputs_of(project, bus.id);
    if inputs.is_empty() {
        let row = col.take(rm::ROW_HEIGHT);
        label(
            ui,
            row,
            t.routing_no_inputs(),
            &typography::SMALL,
            None,
            color::TEXT_4,
        );
        return;
    }
    for (track, send) in inputs {
        let Some(t) = project.track(track) else {
            continue;
        };
        let row = col.take(rm::ROW_HEIGHT);
        let r = ui.interact(
            row,
            ui.scope_id().with(("input", track.0, send.map(|s| s.0))),
            Sense::click(),
        );
        let st = State::of(ui, &r);
        px::fill(ui.painter(), row, radius::S, state::surface_for(2, st));
        let dot = pos2(row.left() + rm::ROW_PAD_X + rm::DOT / 2.0, row.center().y);
        ui.painter()
            .circle_filled(dot, rm::DOT / 2.0, color32(base_color(t.hue.0)));
        let name_rect = Rect::from_x_y_ranges(
            dot.x + rm::DOT / 2.0 + rm::DOT_GAP..=row.right() - 2.0 * rm::ROW_PAD_X - 60.0,
            row.y_range(),
        );
        label(ui, name_rect, &t.name, &typography::UI, None, color::TEXT_1);
        let detail = match send.and_then(|s| t.sends.iter().find(|x| x.id == s)) {
            Some(s) => format!(
                "{} dB {}",
                komponist_model::format::fmt_db(s.level_db),
                match s.tap {
                    komponist_model::Tap::Pre => lang.routing_pre_short(),
                    komponist_model::Tap::Post => "",
                }
            )
            .trim_end()
            .to_owned(),
            None => lang.routing_output().to_owned(),
        };
        let g = w::galley(
            ui,
            &detail,
            komponist_ui::fonts::font_id(&typography::VALUE_S),
            typography::VALUE_S.line_height,
        );
        let pos = w::text_pos(
            row,
            &g,
            row.right() - rm::ROW_PAD_X - g.size().x,
            ui.pixels_per_point(),
        );
        ui.painter().galley(pos, g, color32(color::TEXT_3));
        let name = lang.inspector_input_row(&t.name, &detail);
        r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
        if r.clicked() {
            ctx.send(Action::Ui(UiAction::Select(SelectionChange::Tracks {
                tracks: vec![track],
                mode: SelectMode::Replace,
            })));
        }
    }
}

/// „Sidechain-Quelle für:“ and the devices that take this track as their source.
fn sidechain_line(ui: &mut Ui, t: Lang, col: &mut Col, project: &Project, track: &Track) {
    let users = rt::sidechain_users(t, project, track.id);
    if users.is_empty() {
        return;
    }
    let head = col.take(typography::SMALL.line_height);
    field_key(ui, head, t.routing_sidechain_for());
    for user in users {
        let row = col.take(typography::SMALL.line_height);
        label(ui, row, &user, &typography::SMALL, None, color::TEXT_2);
        let r = ui.interact(
            row,
            ui.scope_id().with(("sidechain_user", &user)),
            Sense::hover(),
        );
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &user));
    }
}
