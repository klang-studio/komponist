//! One device card (`Screens.dc.html` `card()`, `ks-view.js` `devView`): head with the bypass
//! switch, name, format tag and open button; body with the knobs, or the message of a loading,
//! crashed or missing device.

use std::sync::Arc;

use egui::{
    Align, CornerRadius, Galley, Layout, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo,
    pos2, vec2,
};
use komponist_builtin::Macro;
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Srgba, color, radius, size};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::knob::{self, Knob};
use komponist_ui::widgets::switch::Switch;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px, state};

use super::metrics;
use super::model::{Card, CardState, Param, format_value, names};
use crate::actions::{Action, EditAction, PluginAction};
use crate::views::{ViewCtx, elided_galley, group_bounds};

/// The card's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Open,
    Bypass,
    Reload,
    Remove,
}

/// Draws `card` into `rect` and handles its controls; returns the head's response (click selects,
/// drag reorders: the caller's part).
pub(super) fn show(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    rect: Rect,
    selected: bool,
) -> Response {
    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("komponist_device_card", card.device.0))
            .accessibility_role(Role::Group)
            .accessibility_label(names::card(&card.name)),
    );
    cu.set_clip_rect(rect.intersect(ui.clip_rect()));
    group_bounds(&cu, rect);
    cu.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let ppp = cu.pixels_per_point();
    let r = radius::M.round() as u8;
    let top = CornerRadius {
        nw: r,
        ne: r,
        sw: 0,
        se: 0,
    };
    {
        let painter = cu.painter();
        px::fill(painter, rect, radius::M, color32(color::SURFACE_2));
    }
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::HEAD_HEIGHT));
    if card.instrument {
        cu.painter()
            .rect_filled(px::snap_rect(head, ppp), top, color32(color::SURFACE_3));
    }
    let line_y = head.bottom() - px::line_width(ppp);
    px::hline(
        cu.painter(),
        head.x_range(),
        line_y,
        color32(color::SURFACE_1),
    );
    if matches!(card.state, CardState::Crashed(_)) {
        let edge = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::CRASH_EDGE));
        cu.painter()
            .rect_filled(px::snap_rect(edge, ppp), top, color32(color::STATE_ERROR));
    }

    let id = cu.scope_id().with("head");
    let sense = if card.instrument {
        Sense::click()
    } else {
        Sense::click_and_drag()
    };
    let head_response = cu.interact(head, id, sense);
    head_controls(&mut cu, ctx, card, head);
    menu(ctx, card, &head_response);

    let body = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
    match &card.state {
        CardState::Ok => {
            let mut y = body.top();
            if let Some(reason) = &card.warning {
                y = message(
                    &mut cu,
                    card,
                    body,
                    names::STATE_WARNING,
                    None,
                    color::STATE_ERROR,
                );
                let tip_rect = Rect::from_min_max(body.min, pos2(body.right(), y));
                let r = cu.interact(tip_rect, cu.scope_id().with("warning"), Sense::hover());
                Tip::new(names::STATE_WARNING).detail(reason).attach(&r);
            }
            params(&mut cu, ctx, card, rect, y);
        }
        CardState::Loading => {
            let y = message(
                &mut cu,
                card,
                body,
                names::LOADING,
                Some(names::LOADING_TEXT),
                color::TEXT_1,
            );
            let bar = Rect::from_min_size(
                pos2(body.left() + metrics::MESSAGE_PAD, y),
                vec2(
                    body.width() - 2.0 * metrics::MESSAGE_PAD,
                    metrics::PROGRESS_HEIGHT,
                ),
            );
            let pill = metrics::PROGRESS_HEIGHT / 2.0;
            px::fill(cu.painter(), bar, pill, color32(color::SURFACE_3));
            let fill = Rect::from_min_size(
                bar.min,
                vec2(bar.width() * metrics::PROGRESS_FILL, bar.height()),
            );
            px::fill(cu.painter(), fill, pill, color32(color::TEXT_2));
            let r = cu.interact(bar, cu.scope_id().with("progress"), Sense::hover());
            r.widget_info(|| WidgetInfo::labeled(Role::ProgressIndicator, true, names::LOADING));
        }
        CardState::Crashed(reason) => {
            let text = match reason {
                komponist_plugins::FailReason::LoadFailed(_) => names::LOAD_FAILED_TEXT,
                _ => names::CRASHED_TEXT,
            };
            let y = message(
                &mut cu,
                card,
                body,
                names::CRASHED,
                Some(text),
                color::STATE_ERROR,
            );
            let at = Rect::from_min_size(
                pos2(body.left() + metrics::MESSAGE_PAD, y),
                vec2(body.width(), size::CONTROL),
            );
            let mut row = cu.new_child(
                UiBuilder::new()
                    .max_rect(at)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            if row.add(Button::new(names::RELOAD)).clicked() {
                ctx.send(Action::Plugin(PluginAction::Reload(card.device)));
            }
        }
        CardState::Missing => {
            message(
                &mut cu,
                card,
                body,
                names::MISSING,
                Some(names::MISSING_TEXT),
                color::STATE_ERROR,
            );
        }
    }
    if selected {
        state::paint_select_ring(cu.painter(), rect, radius::M);
    }
    head_response
}

/// The width the head needs so the name keeps at least [`metrics::NAME_MIN_WIDTH`] (or all of
/// it, if shorter) next to switch, tag and open button. The design system's switch is wider than
/// the screen's 10 px square, so a narrow card with "Parameter" would cut a name to "kp…"; such a
/// card widens by what it lacks.
pub(super) fn head_width(ui: &Ui, card: &Card) -> f32 {
    let style = typography::UI;
    let name = w::galley(
        ui,
        &card.name,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
    )
    .size()
    .x
    .ceil();
    let tag = w::styled_galley(ui, card.tag, &typography::VALUE_S)
        .size()
        .x
        + 2.0 * metrics::TAG_PAD_X;
    let open = card.open_label().map_or(0.0, |label| {
        let g = w::galley(
            ui,
            label,
            fonts::emphasis_font_id(&style),
            style.line_height,
        );
        metrics::HEAD_GAP + g.size().x.ceil() + 2.0 * wm::BUTTON_PADDING_X
    });
    metrics::HEAD_PAD_LEFT
        + size::SWITCH[0]
        + metrics::HEAD_GAP
        + name.min(metrics::NAME_MIN_WIDTH)
        + metrics::HEAD_GAP
        + tag.ceil()
        + open
        + metrics::HEAD_PAD_RIGHT
}

/// Switch, name, format tag and open button.
fn head_controls(cu: &mut Ui, ctx: &mut ViewCtx<'_>, card: &Card, head: Rect) {
    let [sw, sh] = size::SWITCH;
    let switch = Rect::from_min_size(
        pos2(
            head.left() + metrics::HEAD_PAD_LEFT,
            head.center().y - sh / 2.0,
        ),
        vec2(sw, sh),
    );
    let mut on = !card.bypass;
    let r = cu.put(switch, Switch::new(&mut on, names::ACTIVE));
    Tip::new(names::BYPASS).attach(&r);
    if r.changed() {
        ctx.send(Action::Edit(EditAction::SetDeviceBypass {
            device: card.device,
            bypass: !on,
        }));
    }

    let rest = Rect::from_min_max(
        pos2(switch.right() + metrics::HEAD_GAP, head.top()),
        pos2(head.right() - metrics::HEAD_PAD_RIGHT, head.bottom()),
    );
    let mut row = cu.new_child(
        UiBuilder::new()
            .max_rect(rest)
            .layout(Layout::right_to_left(Align::Center)),
    );
    row.spacing_mut().item_spacing = vec2(metrics::HEAD_GAP, 0.0);
    if let Some(label) = card.open_label() {
        let b = row.add(Button::new(label).compact().on(card.open));
        if b.clicked() {
            let action = if card.open {
                PluginAction::CloseWindow(card.device)
            } else {
                PluginAction::OpenWindow(card.device)
            };
            ctx.send(Action::Plugin(action));
        }
    }
    let missing = card.state == CardState::Missing;
    let ppp = row.pixels_per_point();

    // Format tag on `surface.1` (`padding:1px 4px`); Mono 9 has no token: `value.s`.
    let tag = w::styled_galley(&row, card.tag, &typography::VALUE_S);
    let tag_size = vec2(
        tag.size().x + 2.0 * metrics::TAG_PAD_X,
        tag.size().y + 2.0 * metrics::TAG_PAD_Y,
    );
    let (tag_rect, tag_r) = row.allocate_exact_size(tag_size, Sense::hover());
    tag_r.widget_info(|| WidgetInfo::labeled(Role::Label, true, card.tag));
    px::fill(
        row.painter(),
        tag_rect,
        radius::S,
        color32(color::SURFACE_1),
    );
    let tag_color = if missing {
        color::STATE_ERROR
    } else {
        color::TEXT_3
    };
    let pos = w::text_pos_centered(tag_rect, &tag, ppp);
    row.painter().galley(pos, tag, color32(tag_color));

    // Name: `type.ui` semibold, cut with "…".
    let style = typography::UI;
    let width = row.available_width();
    let name = elided_galley(
        &row,
        &card.name,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
        width,
    );
    let (name_rect, name_r) = row.allocate_exact_size(vec2(width, name.size().y), Sense::hover());
    name_r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &card.name));
    let text = if missing {
        color::STATE_ERROR
    } else if card.bypass || card.state == CardState::Loading {
        color::TEXT_3
    } else {
        color::TEXT_1
    };
    let pos = w::text_pos(name_rect, &name, name_rect.left(), ppp);
    row.painter().galley(pos, name, color32(text));
}

/// The context menu of the head: Öffnen, Bypass, Neu laden, Entfernen.
fn menu(ctx: &mut ViewCtx<'_>, card: &Card, head: &Response) {
    let mut open = Entry::action(names::OPEN, Menu::Open);
    if card.open_label().is_none() {
        open = open.disabled();
    }
    let mut reload = Entry::action(names::RELOAD, Menu::Reload);
    if card.state == CardState::Missing {
        reload = reload.disabled();
    }
    let entries = [
        open,
        Entry::action(names::BYPASS, Menu::Bypass).checked(card.bypass),
        reload,
        Entry::separator(),
        Entry::action(names::REMOVE, Menu::Remove),
    ];
    let device = card.device;
    match ContextMenu::new(names::DEVICE, &entries).show_on(head) {
        Some(Menu::Open) => ctx.send(Action::Plugin(PluginAction::OpenWindow(device))),
        Some(Menu::Bypass) => ctx.send(Action::Edit(EditAction::SetDeviceBypass {
            device,
            bypass: !card.bypass,
        })),
        Some(Menu::Reload) => ctx.send(Action::Plugin(PluginAction::Reload(device))),
        Some(Menu::Remove) => ctx.send(Action::Edit(EditAction::RemoveDevice(device))),
        None => {}
    }
}

/// A message block (`msgS`: padding 12, gap 8): title in `type.ui` semibold `title_color`, the
/// text in `type.small` `text.2`. Returns the y where the next part goes.
fn message(
    cu: &mut Ui,
    card: &Card,
    body: Rect,
    title: &str,
    text: Option<&str>,
    title_color: Srgba,
) -> f32 {
    let pad = metrics::MESSAGE_PAD;
    let x = body.left() + pad;
    let width = card.width - 2.0 * pad;
    let mut y = body.top() + pad;
    let style = typography::UI;
    let g = w::galley(
        cu,
        title,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
    );
    y = place(cu, g, title, x, y, title_color) + metrics::MESSAGE_GAP;
    if let Some(text) = text {
        let small = typography::SMALL;
        let mut job = fonts::layout_job(text, &small, color32(color::TEXT_2));
        job.wrap.max_width = width;
        let g = cu.painter().layout_job(job);
        y = place(cu, g, text, x, y, color::TEXT_2) + metrics::MESSAGE_GAP;
    }
    y
}

/// Paints `g` at (`x`, `y`) as a label named `text`; returns its bottom.
fn place(cu: &mut Ui, g: Arc<Galley>, text: &str, x: f32, y: f32, color: Srgba) -> f32 {
    let ppp = cu.pixels_per_point();
    let pos = pos2(px::snap(x, ppp), px::snap(y, ppp));
    let r = cu.allocate_rect(Rect::from_min_size(pos, g.size()), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    let bottom = pos.y + g.size().y;
    cu.painter().galley(pos, g, color32(color));
    bottom
}

/// The knob grid (`paramsS`): columns of knob, name and value; disabled while bypassed.
fn params(cu: &mut Ui, ctx: &mut ViewCtx<'_>, card: &Card, rect: Rect, top: f32) {
    if card.params.is_empty() {
        return;
    }
    let cols = card.columns.max(1);
    let gap = metrics::GRID_COLUMN_GAP;
    let cell_w = (card.width - 2.0 * metrics::GRID_PAD_X - (cols - 1) as f32 * gap) / cols as f32;
    let name_style = typography::LABEL;
    let value_style = typography::VALUE_S;
    let row_h = size::KNOB
        + metrics::PARAM_GAP
        + name_style.line_height
        + metrics::PARAM_GAP
        + value_style.line_height;
    let x0 = rect.left() + metrics::GRID_PAD_X;
    let y0 = top + metrics::GRID_PAD_Y;
    let ppp = cu.pixels_per_point();
    let (name_color, value_color) = if card.bypass {
        (color::TEXT_3, color::TEXT_3)
    } else {
        (color::TEXT_2, color::TEXT_3)
    };
    for (i, p) in card.params.iter().enumerate() {
        let (col, row) = (i % cols, i / cols);
        let cell = Rect::from_min_size(
            pos2(
                x0 + col as f32 * (cell_w + gap),
                y0 + row as f32 * (row_h + metrics::GRID_ROW_GAP),
            ),
            vec2(cell_w, row_h),
        );
        let knob = Rect::from_center_size(
            pos2(cell.center().x, cell.top() + size::KNOB / 2.0),
            vec2(size::KNOB, size::KNOB),
        );
        if let Some(v) = knob_ui(cu, card, p, knob) {
            ctx.send(Action::Plugin(PluginAction::SetParam {
                device: card.device,
                id: p.id,
                value: v,
            }));
        }
        let name_font = w::font_at(&name_style, 400);
        let mut y = knob.bottom() + metrics::PARAM_GAP;
        for (text, font, style, color) in [
            (&p.name, name_font, &name_style, name_color),
            (
                &p.text,
                fonts::font_id(&value_style),
                &value_style,
                value_color,
            ),
        ] {
            let g = elided_galley(cu, text, font, style.line_height, cell_w);
            let pos = pos2(
                px::snap(cell.center().x - g.size().x / 2.0, ppp),
                px::snap(y, ppp),
            );
            cu.painter().galley(pos, g, color32(color));
            y += style.line_height + metrics::PARAM_GAP;
        }
    }
}

/// One knob; the new (snapped) value if it was turned.
fn knob_ui(cu: &mut Ui, card: &Card, p: &Param, rect: Rect) -> Option<f64> {
    let mut value = p.value;
    let current = p.value;
    let text = p.text.clone();
    let id = p.id;
    let (min, max, steps) = (p.min, p.max, p.steps);
    let macros = card.macros;
    let format = move |v: f64| {
        if macros && let Some(m) = Macro::from_id(id) {
            m.text(v)
        } else if (v - current).abs() < 1e-9 {
            text.clone()
        } else {
            format_value(min, max, steps, v)
        }
    };
    if card.bypass {
        bypassed_knob(cu, p, rect);
        return None;
    }
    let knob = Knob::new(&mut value, p.name.clone())
        .range(min..=max)
        .default_value(p.default)
        .format(format);
    let r = cu.put(rect, knob);
    let snapped = p.snap(value);
    (r.changed() && snapped != current).then_some(snapped)
}

/// The knob of a bypassed device: still, in the card's bypass colours (`card()` with `byp`: arc
/// `text.4`, indicator `text.3`, cap `surface.3`, track `surface.4`), not the design system's
/// disabled knob, whose `surface.2` track would vanish on the card's `surface.2`.
fn bypassed_knob(cu: &mut Ui, p: &Param, rect: Rect) {
    let look = knob::Look {
        track: color32(color::SURFACE_4),
        arc: color32(color::TEXT_4),
        cap: color32(color::SURFACE_3),
        indicator: color32(color::TEXT_3),
    };
    let span = p.max - p.min;
    let fraction = if span > 0.0 {
        ((p.value - p.min) / span) as f32
    } else {
        0.0
    };
    knob::paint(cu.painter(), rect, &look, fraction, false);
    let r = cu.interact(rect, cu.scope_id().with(("bypassed", p.id)), Sense::hover());
    let (name, text, value) = (p.name.clone(), p.text.clone(), p.value);
    r.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Slider, false, &name);
        info.value = Some(value);
        info.current_text_value = Some(text.clone());
        info
    });
}
