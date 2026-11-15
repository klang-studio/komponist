//! One device frame (device handoff §3, `geraete_page_generator.py` `frame()` and `collapsed()`):
//! 244 high, a 28 px head and a 216 px body that draws the layout page's cells on the 56 × 64
//! grid. Foreign plugins get the auto-layout in the same frame; a frame that is loading, crashed
//! or missing shows its message in the body.
//!
//! The head holds the on/off square (10 × 10, `state.play` on), for instruments a 3 × 14 track
//! colour bar and the `surface.3` head, the name, the page switch (two pages or more), the preset
//! dropdown (hexa), the format tag and „Öffnen“ (foreign plugins) and the ⋯ menu. States:
//! selected (1 px accent outline), bypassed (the body desaturated and at half opacity, **still
//! operable**), collapsed (a 28 px strip with the name set vertically).

use std::sync::Arc;

use egui::epaint::{ColorMode, TextShape};
use egui::{
    Align, Color32, CornerRadius, Galley, Layout, Rect, Response, Role, Sense, Shape, Ui,
    UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_i18n::Lang;
use komponist_model::Track;
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Srgba, TrackPalette, color, device, radius, size};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{focus, fonts, px, state};

use super::layout::Page;
use super::metrics;
use super::model::{self, Card, CardState, failure_texts};
use super::sidechain;
use crate::actions::{Action, EditAction, PluginAction, UiAction};
use crate::views::late;
use crate::views::{ViewCtx, elided_galley, group_bounds};

/// The frame's context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Open,
    Bypass,
    Reload,
    Collapse,
    Duplicate,
    Remove,
}

/// What the chain needs to know about the frame it draws.
pub(super) struct Placed<'a> {
    /// The track the device belongs to.
    pub track: &'a Track,
    /// Its index in the chain.
    pub index: usize,
    /// Selected (accent outline).
    pub selected: bool,
}

/// Draws `card` into `rect` (its width, 244 high) and handles its controls; returns the head's
/// response (click selects, double-click collapses, drag reorders: the caller's part).
pub(super) fn show(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    rect: Rect,
    at: &Placed<'_>,
) -> Response {
    let t = ctx.lang;
    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("komponist_device_card", card.device.0))
            .accessibility_role(Role::Group)
            .accessibility_label(t.devices_card(&card.name)),
    );
    cu.set_clip_rect(rect.intersect(ui.clip_rect()));
    group_bounds(&cu, rect);
    cu.spacing_mut().item_spacing = vec2(0.0, 0.0);
    px::fill(cu.painter(), rect, radius::M, color32(color::SURFACE_2));
    let response = if card.collapsed {
        collapsed(&mut cu, ctx, card, rect, at)
    } else {
        expanded(&mut cu, ctx, card, rect, at)
    };
    if at.selected {
        // `0 0 0 1px accent` lies outside the frame: paint with the chain's clip, not the frame's.
        state::paint_select_ring(ui.painter(), rect, radius::M);
    }
    response
}

fn top_corners() -> CornerRadius {
    let r = radius::M.round() as u8;
    CornerRadius {
        nw: r,
        ne: r,
        sw: 0,
        se: 0,
    }
}

/// The 28 px strip: the power square and the name set vertically (`collapsed()`).
fn collapsed(
    cu: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    rect: Rect,
    at: &Placed<'_>,
) -> Response {
    let t = ctx.lang;
    let ppp = cu.pixels_per_point();
    let id = cu.scope_id().with("head");
    let label = t.devices_expand_name(&card.name);
    let response = cu.interact(rect, id, Sense::click_and_drag());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, &label));
    let [pw, ph] = device::frame::POWER;
    let power = Rect::from_min_size(
        pos2(
            px::snap(rect.center().x - pw / 2.0, ppp),
            rect.top() + metrics::COLLAPSED_TOP,
        ),
        vec2(pw, ph),
    );
    power_switch(cu, ctx, card, power);
    let style = typography::UI;
    let g = w::galley(
        cu,
        &card.name,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
    );
    let start = pos2(
        px::snap(rect.center().x - g.size().y / 2.0, ppp),
        px::snap(power.bottom() + metrics::COLLAPSED_GAP + g.size().x, ppp),
    );
    let text = Rect::from_min_size(
        pos2(start.x, start.y - g.size().x),
        vec2(g.size().y, g.size().x),
    );
    cu.allocate_rect(text, Sense::hover())
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, &card.name));
    let ink = if card.bypass {
        color::TEXT_3
    } else {
        color::TEXT_1
    };
    cu.painter().add(Shape::Text(
        TextShape::new(start, g, color32(ink)).with_angle(-std::f32::consts::FRAC_PI_2),
    ));
    menu(ctx, card, &response, None, at);
    response
}

/// Head and body of an expanded frame.
fn expanded(
    cu: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    rect: Rect,
    at: &Placed<'_>,
) -> Response {
    let t = ctx.lang;
    let ppp = cu.pixels_per_point();
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), device::frame::HEAD_H));
    if card.instrument {
        cu.painter().rect_filled(
            px::snap_rect(head, ppp),
            top_corners(),
            color32(color::SURFACE_3),
        );
    }
    px::hline(
        cu.painter(),
        head.x_range(),
        head.bottom() - px::line_width(ppp),
        color32(color::SURFACE_1),
    );
    if matches!(card.state, CardState::Crashed(_)) {
        let edge = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::CRASH_EDGE));
        cu.painter().rect_filled(
            px::snap_rect(edge, ppp),
            top_corners(),
            color32(color::STATE_ERROR),
        );
    }
    let id = cu.scope_id().with("head");
    let head_response = cu.interact(head, id, Sense::click_and_drag());
    let menu_button = head_controls(cu, ctx, card, head, at);
    menu(ctx, card, &head_response, Some(menu_button), at);

    let body = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
    match &card.state {
        CardState::Ok => {
            let mut y = body.top();
            if let Some(reason) = &card.warning {
                y = message(
                    cu,
                    card,
                    body,
                    t.devices_state_warning(),
                    None,
                    color::STATE_ERROR,
                );
                let tip_rect = Rect::from_min_max(body.min, pos2(body.right(), y));
                let r = cu.interact(tip_rect, cu.scope_id().with("warning"), Sense::hover());
                Tip::new(t.devices_state_warning())
                    .detail(reason)
                    .attach(&r);
            }
            let cells = Rect::from_min_max(pos2(body.left(), y.max(body.top())), body.max);
            if card.bypass {
                bypassed(cu, |cu| cells_body(cu, ctx, card, at, body, cells));
            } else {
                cells_body(cu, ctx, card, at, body, cells);
            }
        }
        CardState::Loading => {
            let y = message(
                cu,
                card,
                body,
                t.devices_loading(),
                Some(t.devices_loading_text()),
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
            r.widget_info(|| {
                WidgetInfo::labeled(Role::ProgressIndicator, true, t.devices_loading())
            });
        }
        CardState::Crashed(reason) => {
            let (title, text) = failure_texts(t, reason);
            let y = message(cu, card, body, title, Some(&text), color::STATE_ERROR);
            let at = Rect::from_min_size(
                pos2(body.left() + metrics::MESSAGE_PAD, y),
                vec2(body.width(), size::CONTROL),
            );
            let mut row = cu.new_child(
                UiBuilder::new()
                    .max_rect(at)
                    .layout(Layout::left_to_right(Align::Center)),
            );
            if row.add(Button::new(t.devices_reload())).clicked() {
                ctx.send(Action::Plugin(PluginAction::Reload(card.device)));
            }
        }
        CardState::Missing => {
            message(
                cu,
                card,
                body,
                t.devices_missing(),
                Some(t.devices_missing_text()),
                color::STATE_ERROR,
            );
        }
    }
    head_response
}

/// Runs `add` and then desaturates and halves what it drew (`filter:saturate(0.15);
/// opacity:0.5`). The controls inside stay operable.
fn bypassed(cu: &mut Ui, add: impl FnOnce(&mut Ui)) {
    let layer = cu.layer_id();
    let first = cu.ctx().graphics_mut(|g| g.entry(layer).next_idx());
    cu.scope(|cu| {
        cu.multiply_opacity(metrics::BYPASS_OPACITY);
        add(cu);
    });
    cu.ctx().graphics_mut(|g| {
        let list = g.entry(layer);
        let end = list.next_idx();
        for i in first.0..end.0 {
            list.mutate_shape(egui::layers::ShapeIdx(i), |s| {
                desaturate(&mut s.shape, metrics::BYPASS_SATURATION);
            });
        }
    });
}

/// `c` with its saturation scaled by `amount` (0 grey, 1 unchanged), around the Rec. 709 luma.
fn grey(c: Color32, amount: f32) -> Color32 {
    let [r, g, b, a] = c.to_array();
    let luma = 0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b);
    let mix = |v: u8| {
        (luma + (f32::from(v) - luma) * amount)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color32::from_rgba_premultiplied(mix(r), mix(g), mix(b), a)
}

fn desaturate(shape: &mut Shape, amount: f32) {
    match shape {
        Shape::Vec(shapes) => shapes.iter_mut().for_each(|s| desaturate(s, amount)),
        Shape::Circle(c) => {
            c.fill = grey(c.fill, amount);
            c.stroke.color = grey(c.stroke.color, amount);
        }
        Shape::LineSegment { stroke, .. } => stroke.color = grey(stroke.color, amount),
        Shape::Path(p) => {
            p.fill = grey(p.fill, amount);
            if let ColorMode::Solid(c) = &mut p.stroke.color {
                *c = grey(*c, amount);
            }
        }
        Shape::Rect(r) => {
            r.fill = grey(r.fill, amount);
            r.stroke.color = grey(r.stroke.color, amount);
        }
        Shape::Mesh(mesh) => {
            for v in &mut Arc::make_mut(mesh).vertices {
                v.color = grey(v.color, amount);
            }
        }
        _ => {}
    }
}

/// The width the head needs so the name keeps at least [`metrics::NAME_MIN_WIDTH`] (or all of
/// it, if shorter) next to its other parts; a narrower frame widens by what it lacks.
pub(super) fn head_width(ui: &Ui, card: &Card) -> f32 {
    let t = komponist_ui::lang(ui.ctx());
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
    let gap = metrics::HEAD_GAP;
    let mut width = metrics::HEAD_PAD_LEFT
        + device::frame::POWER[0]
        + gap
        + if card.instrument {
            device::frame::TRACK_BAR[0] + gap
        } else {
            0.0
        }
        + name.min(metrics::NAME_MIN_WIDTH)
        + gap
        + device::frame::MENU_BUTTON[0]
        + metrics::HEAD_PAD_RIGHT;
    if card.presets {
        width += gap + device::frame::preset_dropdown::MIN_W;
    } else {
        let tag = w::styled_galley(ui, card.tag, &typography::VALUE_S)
            .size()
            .x
            + 2.0 * metrics::TAG_PAD_X;
        width += gap + tag.ceil();
        if let Some(label) = card.open_label(t) {
            let g = w::galley(
                ui,
                label,
                fonts::emphasis_font_id(&style),
                style.line_height,
            );
            width += gap + g.size().x.ceil() + 2.0 * wm::BUTTON_PADDING_X;
        }
    }
    if card.has_sidechain {
        width += gap + sidechain::chip_width(ui);
    }
    if let Some(pages) =
        (card.layout.pages.len() > 1).then(|| page_switch_width(ui, &card.layout.pages))
    {
        width += gap + pages;
    }
    width
}

/// The page switch's width: padding, the segments (text and `padding:0 7px`), gaps.
fn page_switch_width(ui: &Ui, pages: &[Page]) -> f32 {
    let style = typography::SMALL;
    let segments: f32 = pages
        .iter()
        .map(|p| {
            w::galley(ui, &p.name, fonts::font_id(&style), style.line_height)
                .size()
                .x
                .ceil()
                + 2.0 * komponist_tokens::widget::pages::SEG_PADDING_X
        })
        .sum();
    2.0 * metrics::PAGES_PAD + segments + (pages.len() - 1) as f32 * metrics::PAGES_GAP
}

/// The on/off square: `state.play` on, `surface.4` with a 1 px `text.3` edge off. A switch for
/// AccessKit; the hit area is [`metrics::POWER_HIT`] wide.
fn power_switch(cu: &mut Ui, ctx: &mut ViewCtx<'_>, card: &Card, square: Rect) {
    let t = ctx.lang;
    let hit = Rect::from_center_size(
        square.center(),
        vec2(metrics::POWER_HIT, metrics::POWER_HIT),
    );
    let id = cu.scope_id().with("power");
    let r = cu.interact(hit, id, Sense::click());
    let on = !card.bypass;
    r.widget_info(|| WidgetInfo::selected(Role::Switch, true, on, t.devices_active()));
    Tip::new(t.devices_bypass()).attach(&r);
    let radius = komponist_tokens::radius::S;
    if on {
        px::fill(cu.painter(), square, radius, color32(color::STATE_PLAY));
    } else {
        px::fill(cu.painter(), square, radius, color32(color::SURFACE_4));
        px::inner_border(cu.painter(), square, color32(color::TEXT_3));
    }
    if r.has_focus() {
        focus::paint_ring(cu.painter(), square.expand(2.0), radius);
    }
    if r.clicked() {
        ctx.send(Action::Edit(EditAction::SetDeviceBypass {
            device: card.device,
            bypass: on,
        }));
    }
}

/// Power square, track bar, name, page switch, preset, tag, „Öffnen“ and the ⋯ button. Returns the
/// ⋯ button's response.
fn head_controls(
    cu: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    head: Rect,
    at: &Placed<'_>,
) -> Response {
    let t = ctx.lang;
    let ppp = cu.pixels_per_point();
    let inner = Rect::from_min_max(
        pos2(head.left() + metrics::HEAD_PAD_LEFT, head.top()),
        pos2(head.right() - metrics::HEAD_PAD_RIGHT, head.bottom()),
    );
    let mut row = cu.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    row.spacing_mut().item_spacing = vec2(metrics::HEAD_GAP, 0.0);

    // Right side, from the right: ⋯, preset (hexa, internal devices with presets) or „Öffnen“ and format tag (foreign).
    let more = more_button(&mut row);
    if card.presets {
        preset_button(&mut row, ctx, card);
        if card.has_sidechain {
            sidechain::chip(&mut row, ctx, card);
        }
    } else {
        if card.has_sidechain {
            sidechain::chip(&mut row, ctx, card);
        }
        if let Some(label) = card.open_label(t) {
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
        format_tag(&mut row, card);
    }

    // Left side: power, track bar, name, page switch in what is left.
    let rest = row.available_rect_before_wrap();
    let [pw, ph] = device::frame::POWER;
    let power = Rect::from_min_size(
        pos2(rest.left(), px::snap(head.center().y - ph / 2.0, ppp)),
        vec2(pw, ph),
    );
    power_switch(cu, ctx, card, power);
    let mut x = power.right() + metrics::HEAD_GAP;
    if card.instrument {
        let [bw, bh] = device::frame::TRACK_BAR;
        let bar = Rect::from_min_size(
            pos2(x, px::snap(head.center().y - bh / 2.0, ppp)),
            vec2(bw, bh),
        );
        px::fill(
            cu.painter(),
            bar,
            komponist_tokens::radius::S / 2.0,
            color32(TrackPalette::new(at.track.hue.0).base),
        );
        x = bar.right() + metrics::HEAD_GAP;
    }
    let pages = (card.layout.pages.len() > 1).then(|| page_switch_width(cu, &card.layout.pages));
    let pages_room = pages.map_or(0.0, |p| p + metrics::HEAD_GAP);
    let style = typography::UI;
    let room = (rest.right() - x - pages_room - metrics::HEAD_GAP).max(0.0);
    let name = elided_galley(
        cu,
        &card.name,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
        room,
    );
    let name_rect =
        Rect::from_min_size(pos2(x, head.center().y - name.size().y / 2.0), name.size());
    cu.allocate_rect(name_rect, Sense::hover())
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, &card.name));
    let ink = if card.bypass || card.state == CardState::Loading {
        color::TEXT_3
    } else if card.state == CardState::Missing {
        color::STATE_ERROR
    } else {
        color::TEXT_1
    };
    let pos = w::text_pos(name_rect, &name, name_rect.left(), ppp);
    cu.painter().galley(pos, name.clone(), color32(ink));
    if let Some(width) = pages {
        let left = name_rect.right() + metrics::HEAD_GAP;
        let switch = Rect::from_min_size(
            pos2(left, head.center().y - device::frame::PAGES_SEGMENT_H / 2.0),
            vec2(width, device::frame::PAGES_SEGMENT_H),
        );
        page_switch(cu, ctx, card, switch);
    }
    more
}

/// The ⋯ button (`20 × 20`, `radius.m`, `text.2`): opens the frame's menu.
fn more_button(row: &mut Ui) -> Response {
    let t = komponist_ui::lang(row.ctx());
    let [mw, mh] = device::frame::MENU_BUTTON;
    let (rect, r) = row.allocate_exact_size(vec2(mw, mh), Sense::click());
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, t.devices_more()));
    Tip::new(t.devices_more()).attach(&r);
    let hovered = r.hovered() || r.is_pointer_button_down_on();
    if hovered {
        px::fill(
            row.painter(),
            rect,
            metrics::HEAD_RADIUS,
            color32(color::SURFACE_4),
        );
    }
    if r.has_focus() {
        focus::paint_ring(row.painter(), rect, metrics::HEAD_RADIUS);
    }
    let ink = if hovered {
        color::TEXT_1
    } else {
        color::TEXT_2
    };
    komponist_ui::icons::paint(
        row,
        rect,
        komponist_tokens::icon::Icon::Ellipsis,
        color32(ink),
    );
    r
}

/// The format tag on `surface.1` (`padding:1px 4px`); „zu spät“ while the device is late.
fn format_tag(row: &mut Ui, card: &Card) {
    let t = komponist_ui::lang(row.ctx());
    let missing = card.state == CardState::Missing;
    let tag_text = if card.late.is_some() {
        t.late_label()
    } else {
        card.tag
    };
    let tag = w::styled_galley(row, tag_text, &typography::VALUE_S);
    let tag_size = vec2(
        tag.size().x + 2.0 * metrics::TAG_PAD_X,
        tag.size().y + 2.0 * metrics::TAG_PAD_Y,
    );
    let (tag_rect, tag_r) = row.allocate_exact_size(tag_size, Sense::hover());
    tag_r.widget_info(|| WidgetInfo::labeled(Role::Label, true, tag_text));
    if let Some(n) = card.late {
        Tip::new(tag_text).detail(&t.late_blocks(n)).attach(&tag_r);
        late::repaint_when_expired(row.ctx());
    }
    px::fill(
        row.painter(),
        tag_rect,
        radius::S,
        color32(color::SURFACE_1),
    );
    let tag_color = if card.late.is_some() {
        color::METER_LOUD
    } else if missing {
        color::STATE_ERROR
    } else {
        color::TEXT_3
    };
    let pos = w::text_pos_centered(tag_rect, &tag, row.pixels_per_point());
    row.painter().galley(pos, tag, color32(tag_color));
}

/// The preset dropdown (`h 20`, `96 … 150` wide, `surface.2` on an instrument's head, `padding:0
/// 6px`, caret `text.2`): opens a menu of the factory presets by category.
fn preset_button(row: &mut Ui, ctx: &mut ViewCtx<'_>, card: &Card) {
    let t = ctx.lang;
    // The current preset's name; „ *“ behind it once a value differs (internal devices).
    let known: Option<String> = match card.internal_presets {
        Some(kind) => card
            .preset
            .and_then(|i| kind.presets().get(i))
            .map(|p| p.name.to_owned()),
        None => card
            .preset
            .and_then(|i| komponist_builtin::presets().nth(i))
            .map(|p| p.name.to_owned()),
    }
    .map(|name| {
        if card.preset_changed {
            format!("{name}{}", model::CHANGED)
        } else {
            name
        }
    });
    let current = known.as_deref().unwrap_or(t.devices_preset());
    let style = typography::SMALL;
    let name = known.as_deref().map_or_else(
        || t.devices_preset().to_owned(),
        |n| t.devices_preset_name(n),
    );
    let natural = w::galley(row, current, fonts::font_id(&style), style.line_height)
        .size()
        .x
        .ceil()
        + 2.0 * metrics::PRESET_PAD_X
        + metrics::HEAD_GAP
        + CARET_WIDTH;
    let width = natural.clamp(
        device::frame::preset_dropdown::MIN_W,
        device::frame::preset_dropdown::MAX_W,
    );
    let (rect, r) = row.allocate_exact_size(
        vec2(width, device::frame::preset_dropdown::H),
        Sense::click(),
    );
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    let base = if card.instrument {
        color::SURFACE_2
    } else {
        color::SURFACE_3
    };
    let open = ContextMenu::<usize>::is_open(row.ctx(), r.id);
    let fill = if open {
        color::SURFACE_5
    } else if r.hovered() {
        color::SURFACE_4
    } else {
        base
    };
    px::fill(row.painter(), rect, metrics::HEAD_RADIUS, color32(fill));
    if r.has_focus() {
        focus::paint_ring(row.painter(), rect, metrics::HEAD_RADIUS);
    }
    let text_room = rect.width() - 2.0 * metrics::PRESET_PAD_X - metrics::HEAD_GAP - CARET_WIDTH;
    let g = elided_galley(
        row,
        current,
        fonts::font_id(&style),
        style.line_height,
        text_room,
    );
    let pos = w::text_pos(
        rect,
        &g,
        rect.left() + metrics::PRESET_PAD_X,
        row.pixels_per_point(),
    );
    row.painter().galley(pos, g, color32(color::TEXT_1));
    let caret_rect = Rect::from_center_size(
        pos2(
            rect.right() - metrics::PRESET_PAD_X - CARET_WIDTH / 2.0,
            rect.center().y,
        ),
        vec2(CARET_WIDTH, CARET_HEIGHT),
    );
    komponist_ui::icons::paint_caret(
        row.painter(),
        caret_rect,
        komponist_ui::icons::Caret::Down,
        color32(color::TEXT_2),
    );
    if r.clicked() {
        ContextMenu::<usize>::open_at(row.ctx(), r.id, rect.left_bottom() + vec2(0.0, 2.0));
    }
    if open || r.clicked() {
        let chosen = match card.internal_presets {
            Some(kind) => internal_preset_menu(t, &r, kind),
            None => hexa_preset_menu(t, &r),
        };
        if let Some(preset) = chosen {
            let action = match card.internal_presets {
                Some(_) => PluginAction::ApplyInternalPreset {
                    device: card.device,
                    index: preset,
                },
                None => PluginAction::ApplyHexaPreset {
                    device: card.device,
                    preset,
                },
            };
            ctx.send(Action::Plugin(action));
        }
    }
}

/// hexa's menu: its factory bank by category; the chosen index.
fn hexa_preset_menu(t: Lang, r: &Response) -> Option<usize> {
    let categories: Vec<&str> = komponist_builtin::categories().collect();
    let presets: Vec<_> = komponist_builtin::presets().collect();
    let groups: Vec<Vec<Entry<'_, usize>>> = categories
        .iter()
        .map(|c| {
            presets
                .iter()
                .filter(|p| p.category == *c)
                .map(|p| Entry::action(p.name, p.index))
                .collect()
        })
        .collect();
    let entries: Vec<Entry<'_, usize>> = categories
        .iter()
        .zip(&groups)
        .map(|(c, g)| Entry::submenu(komponist_builtin::category_text(t, c), g))
        .collect();
    ContextMenu::new(t.devices_preset(), &entries).show_on(r)
}

/// The menu of an internal device (WP-96): its presets grouped by category if they have one, else
/// a flat list; the chosen index in [`Internal::presets`](komponist_builtin::Internal::presets).
fn internal_preset_menu(t: Lang, r: &Response, kind: komponist_builtin::Internal) -> Option<usize> {
    let presets = kind.presets();
    let mut categories: Vec<&str> = Vec::new();
    for c in presets.iter().filter_map(|p| p.category) {
        if !categories.contains(&c) {
            categories.push(c);
        }
    }
    let groups: Vec<Vec<Entry<'_, usize>>> = categories
        .iter()
        .map(|c| {
            presets
                .iter()
                .enumerate()
                .filter(|(_, p)| p.category == Some(*c))
                .map(|(i, p)| Entry::action(p.name, i))
                .collect()
        })
        .collect();
    let mut entries: Vec<Entry<'_, usize>> = categories
        .iter()
        .zip(&groups)
        .map(|(c, g)| Entry::submenu(komponist_builtin::category_text(t, c), g))
        .collect();
    entries.extend(
        presets
            .iter()
            .enumerate()
            .filter(|(_, p)| p.category.is_none())
            .map(|(i, p)| Entry::action(p.name, i)),
    );
    ContextMenu::new(t.devices_preset(), &entries).show_on(r)
}

/// The caret of the preset button (`border-left/right:3px`, `border-top:4px`): 6 × 4.
const CARET_WIDTH: f32 = 6.0;
/// … and its height.
const CARET_HEIGHT: f32 = 4.0;

/// The page switch (`h 20`, padding 1, `surface.1`, segments `h 18`, `padding:0 7px`, the page
/// shown `surface.4` / `text.1`, the others `text.2`).
fn page_switch(cu: &mut Ui, ctx: &mut ViewCtx<'_>, card: &Card, rect: Rect) {
    let t = ctx.lang;
    let group = cu.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt("komponist_device_pages")
            .accessibility_role(Role::RadioGroup)
            .accessibility_label(t.devices_pages()),
    );
    group_bounds(&group, rect);
    px::fill(
        group.painter(),
        rect,
        metrics::HEAD_RADIUS,
        color32(color::SURFACE_1),
    );
    let style = typography::SMALL;
    let mut x = rect.left() + metrics::PAGES_PAD;
    let shown = card.page.min(card.layout.pages.len().saturating_sub(1));
    for (i, page) in card.layout.pages.iter().enumerate() {
        let g = w::galley(
            &group,
            &page.name,
            fonts::font_id(&style),
            style.line_height,
        );
        let seg_w = g.size().x.ceil() + 2.0 * komponist_tokens::widget::pages::SEG_PADDING_X;
        let seg = Rect::from_min_size(
            pos2(x, rect.top() + metrics::PAGES_PAD),
            vec2(seg_w, komponist_tokens::widget::pages::SEG_H),
        );
        let id = group.scope_id().with(("page", i));
        let r = group.interact(seg, id, Sense::click());
        let on = i == shown;
        r.widget_info(|| WidgetInfo::selected(Role::RadioButton, true, on, &page.name));
        if on {
            px::fill(
                group.painter(),
                seg,
                metrics::PAGE_SEGMENT_RADIUS,
                color32(color::SURFACE_4),
            );
        } else if r.hovered() {
            px::fill(
                group.painter(),
                seg,
                metrics::PAGE_SEGMENT_RADIUS,
                color32(color::SURFACE_3),
            );
        }
        if r.has_focus() {
            focus::paint_ring(group.painter(), seg, metrics::PAGE_SEGMENT_RADIUS);
        }
        let ink = if on { color::TEXT_1 } else { color::TEXT_2 };
        let pos = w::text_pos_centered(seg, &g, group.pixels_per_point());
        group.painter().galley(pos, g, color32(ink));
        if r.clicked() && !on {
            ctx.send(Action::Ui(UiAction::SetDevicePage {
                device: card.device,
                page: i,
            }));
        }
        x = seg.right() + metrics::PAGES_GAP;
    }
}

/// The context menu of the head: Öffnen, Bypass, Neu laden, Einklappen, Duplizieren, Entfernen;
/// opened by a right-click on the head or the ⋯ button (`more`).
fn menu(
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    head: &Response,
    more: Option<Response>,
    at: &Placed<'_>,
) {
    let t = ctx.lang;
    if let Some(button) = &more
        && button.clicked()
    {
        ContextMenu::<Menu>::open_at(
            &head.ctx,
            head.id,
            button.rect.left_bottom() + vec2(0.0, 2.0),
        );
    }
    let mut open = Entry::action(t.devices_open(), Menu::Open);
    if card.open_label(t).is_none() {
        open = open.disabled();
    }
    let mut reload = Entry::action(t.devices_reload(), Menu::Reload);
    if card.state == CardState::Missing {
        reload = reload.disabled();
    }
    let collapse_label = if card.collapsed {
        t.devices_expand()
    } else {
        t.devices_collapse()
    };
    let mut duplicate = Entry::action(t.devices_duplicate(), Menu::Duplicate);
    if card.instrument {
        duplicate = duplicate.disabled();
    }
    let entries = [
        open,
        Entry::action(t.devices_bypass(), Menu::Bypass).checked(card.bypass),
        reload,
        Entry::separator(),
        Entry::action(collapse_label, Menu::Collapse),
        duplicate,
        Entry::separator(),
        Entry::action(t.devices_remove(), Menu::Remove),
    ];
    let device = card.device;
    match ContextMenu::new(t.devices_device(), &entries).show_on(head) {
        Some(Menu::Open) => ctx.send(Action::Plugin(PluginAction::OpenWindow(device))),
        Some(Menu::Bypass) => ctx.send(Action::Edit(EditAction::SetDeviceBypass {
            device,
            bypass: !card.bypass,
        })),
        Some(Menu::Reload) => ctx.send(Action::Plugin(PluginAction::Reload(device))),
        Some(Menu::Collapse) => ctx.send(Action::Ui(UiAction::ToggleDeviceCollapsed {
            device,
            all: false,
        })),
        Some(Menu::Duplicate) => ctx.send(Action::Edit(EditAction::CopyDevice {
            device,
            to: komponist_model::ChannelRef::Track(at.track.id),
            index: at.index + 1,
        })),
        Some(Menu::Remove) => ctx.send(Action::Edit(EditAction::RemoveDevice(device))),
        None => {}
    }
}

/// The cells of the current page on the grid, and the dividers between function groups.
fn cells_body(
    cu: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    at: &Placed<'_>,
    body: Rect,
    area: Rect,
) {
    let Some(page) = card.current_page() else {
        return;
    };
    let ppp = cu.pixels_per_point();
    let pad = device::grid::PADDING;
    let step = device::grid::CELL[0] + device::grid::GAP;
    let top = area.top();
    for d in &page.dividers {
        let x = body.left() + pad + f32::from(*d + 1) * step - device::grid::GAP / 2.0;
        px::vline(
            cu.painter(),
            px::snap(x, ppp),
            top + pad..=body.bottom() - pad,
            color32(color::LINE_1),
        );
    }
    // The state warning above the cells pushes them down; cells that no longer fit are not drawn
    // (no clipped knobs).
    let palette = TrackPalette::new(at.track.hue.0);
    let origin = pos2(body.left(), area.top());
    for cell in &page.cells {
        let rect = super::layout::cell_rect(cell).translate(origin.to_vec2());
        if rect.bottom() <= body.bottom() {
            let mut out = Vec::new();
            super::cells::draw_cell(cu, &mut out, card, &palette, cell, rect);
            for action in out {
                ctx.send(action);
            }
        }
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
