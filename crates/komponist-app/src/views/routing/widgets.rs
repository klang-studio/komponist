//! The widgets of the routing UI: the send row, the output button, the target popover and the
//! dock's routing chip. They send [`EditAction`]s; none changes the document itself.

use egui::{
    Align2, Area, Context, Id, Key, Modifiers, Order, Rect, Response, Role, Sense, Ui, UiKind,
    WidgetInfo, pos2, vec2,
};
use komponist_i18n::Lang;
use komponist_model::edits::SendProp;
use komponist_model::format::fmt_db;
use komponist_model::project::{SEND_LEVEL_DEFAULT_DB, VOLUME_DB_MAX, VOLUME_DB_MIN};
use komponist_model::{ChannelRef, Project, Tap, Track, TrackId, TrackOutput};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, space, typography};
use komponist_ui::drag::{self, ClickAction};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry, row_look};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, accesskit, floating, menu_frame, pressed_outside};
use komponist_ui::{focus, fonts, icons, px};

use super::metrics as m;
use super::{SendInfo, Target};
use crate::actions::{Action, EditAction};
use crate::platform::Platform;
use crate::views::mixer::model::base_color;
use crate::views::{ViewCtx, elided_galley};

/// The undo label of a send level drag (`undo.ftl`).
pub const UNDO_SEND_LEVEL: &str = "undo-send-level";

/// The fill fraction (0 ..= 1) of a send level.
pub fn fill_fraction(db: f32) -> f32 {
    let [lo, hi] = m::FILL_RANGE_DB;
    ((db - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// A dot in the colour of hue `hue` (`alpha` fades it for disabled rows).
fn dot(ui: &Ui, center: egui::Pos2, hue: Option<u16>, enabled: bool) {
    let painter = ui.painter();
    match hue {
        Some(h) => {
            let mut c = base_color(h);
            if !enabled {
                c[3] = 100;
            }
            painter.circle_filled(center, m::DOT / 2.0, color32(c));
        }
        None => {
            painter.circle_stroke(
                center,
                m::DOT / 2.0 - 0.5,
                egui::Stroke::new(1.0, color32(color::TEXT_3)),
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The target popover
// ------------------------------------------------------------------------------------------------

/// What the popover's user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    /// Row `i` of the list.
    Row(usize),
    /// Extra entry `i` (below the separator: „Neuer Bus …“).
    Extra(usize),
}

/// The keyboard row of an open popover.
#[derive(Clone, Copy, Default)]
struct Open {
    highlight: Option<usize>,
}

/// Opens the popover `id` below `anchor` with the row `current` highlighted for the keys.
pub fn open_popover(ctx: &Context, id: Id, current: Option<usize>) {
    floating::open(ctx, id);
    ctx.data_mut(|d| {
        d.insert_temp(
            id,
            Open {
                highlight: current.or(Some(0)),
            },
        )
    });
}

/// Shows the target popover `id` while it is open: a list of `rows` (the current one checked,
/// blocked ones grey with their reason as tooltip), a separator, and the `extras`. Returns what
/// was chosen in this pass; the popover closes then (and on Esc or a press outside).
///
/// Keys: ↑ ↓ move over the rows (greyed ones too, so their reason is reachable), Enter or Space
/// chooses, Esc closes.
pub fn target_popover(
    ctx: &Context,
    id: Id,
    anchor: &Response,
    name: &str,
    rows: &[Target],
    current: Option<usize>,
    extras: &[&str],
) -> Option<Picked> {
    if !floating::is_open(ctx, id) {
        return None;
    }
    let mut st: Open = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let total = rows.len() + extras.len();
    let pressed = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
    let (down, up) = (pressed(Key::ArrowDown), pressed(Key::ArrowUp));
    let accept = pressed(Key::Enter) | pressed(Key::Space);
    let mut close = pressed(Key::Escape);
    if down || up {
        let flags = vec![true; total];
        st.highlight =
            w::step_enabled(&flags, st.highlight, if down { 1 } else { -1 }).or(st.highlight);
    }
    let mut picked = None;
    if accept && let Some(h) = st.highlight {
        picked = match rows.get(h) {
            Some(r) if r.blocked.is_none() => Some(Picked::Row(h)),
            Some(_) => None,
            None if h < total => Some(Picked::Extra(h - rows.len())),
            None => None,
        };
    }
    let area = Area::new(id)
        .order(Order::Foreground)
        .kind(UiKind::Menu)
        .pivot(Align2::LEFT_TOP)
        .fixed_pos(anchor.rect.left_bottom() + vec2(0.0, space::SPACE_1))
        .constrain(true)
        .accessibility_parent(anchor.id)
        .accessible_name(name)
        .show(ctx, |ui| {
            menu_frame()
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
                    let (mut clicked, mut hovered) = (None, None);
                    for (i, row) in rows.iter().enumerate() {
                        let out = target_row(ui, row, current == Some(i), st.highlight == Some(i));
                        if out.hovered() && moved {
                            hovered = Some(i);
                        }
                        if out.clicked() && row.blocked.is_none() {
                            clicked = Some(Picked::Row(i));
                        }
                    }
                    if !extras.is_empty() {
                        separator(ui);
                    }
                    for (i, label) in extras.iter().enumerate() {
                        let at = rows.len() + i;
                        let out = extra_row(ui, label, st.highlight == Some(at));
                        if out.hovered() && moved {
                            hovered = Some(at);
                        }
                        if out.clicked() {
                            clicked = Some(Picked::Extra(i));
                        }
                    }
                    (clicked, hovered)
                })
                .inner
        });
    let (clicked, hovered) = area.inner;
    if let Some(h) = hovered {
        st.highlight = Some(h);
    }
    picked = clicked.or(picked);
    if picked.is_some() {
        close = true;
    }
    if !close && pressed_outside(ctx, &[area.response.rect, anchor.rect]) {
        close = true;
    }
    if close {
        floating::close(ctx, id);
        ctx.data_mut(|d| d.remove::<Open>(id));
        if picked.is_none() {
            anchor.request_focus();
        }
    } else {
        ctx.data_mut(|d| d.insert_temp(id, st));
    }
    picked
}

/// The separator between the rows and the extras.
fn separator(ui: &mut Ui) {
    let height = 1.0 + 2.0 * space::SPACE_1;
    let (rect, _) = ui.allocate_exact_size(vec2(size::MENU_WIDTH, height), Sense::hover());
    let inset = space::SPACE_1_5;
    px::hline(
        ui.painter(),
        rect.left() + inset..=rect.right() - inset,
        rect.top() + space::SPACE_1,
        color32(color::LINE_2),
    );
}

/// One target row: ✓ column, the dot, the name. Blocked rows are grey with the reason as tooltip
/// and as accessible description.
fn target_row(ui: &mut Ui, t: &Target, checked: bool, highlighted: bool) -> Response {
    let enabled = t.blocked.is_none();
    let (rect, r) = ui.allocate_exact_size(
        vec2(size::MENU_WIDTH, size::MENU_ROW),
        w::sense(ui, Sense::click()),
    );
    let lang = komponist_ui::lang(ui.ctx());
    let name = match &t.blocked {
        Some(_) => lang.routing_blocked(&t.label),
        None => t.label.clone(),
    };
    r.widget_info(|| WidgetInfo::selected(Role::MenuItemRadio, enabled, checked, &name));
    if let Some(reason) = &t.blocked {
        Tip::new(reason).attach(&r);
        let reason = reason.clone();
        accesskit(&r, |n| n.set_description(reason));
    }
    let look = row_look(
        highlighted && enabled || (r.hovered() && enabled),
        false,
        enabled,
    );
    if let Some(fill) = look.fill {
        px::fill(ui.painter(), rect, radius::S, fill);
    } else if highlighted {
        px::fill(ui.painter(), rect, radius::S, color32(color::SURFACE_4));
    }
    let ppp = ui.pixels_per_point();
    let check_left = rect.left() + space::SPACE_1;
    if checked {
        w::paint_check(ui, rect, check_left, look.check);
    }
    let mut x = check_left + komponist_ui::widgets::metrics::CHECK_COLUMN + space::SPACE_1;
    dot(ui, pos2(x + m::DOT / 2.0, rect.center().y), t.hue, enabled);
    x += m::DOT + m::DOT_GAP * 2.0;
    let label = if t.group {
        format!("{} ({})", t.label, lang.routing_kind_group())
    } else {
        t.label.clone()
    };
    let g = w::galley(
        ui,
        &label,
        fonts::font_id(&typography::UI),
        typography::UI.line_height,
    );
    ui.painter()
        .galley(w::text_pos(rect, &g, x, ppp), g, look.text);
    r
}

/// An extra entry below the separator („Neuer Bus …“): no check, no dot.
fn extra_row(ui: &mut Ui, label: &str, highlighted: bool) -> Response {
    let (rect, r) = ui.allocate_exact_size(
        vec2(size::MENU_WIDTH, size::MENU_ROW),
        w::sense(ui, Sense::click()),
    );
    r.widget_info(|| WidgetInfo::labeled(Role::MenuItem, true, label));
    let look = row_look(highlighted || r.hovered(), false, true);
    if let Some(fill) = look.fill {
        px::fill(ui.painter(), rect, radius::S, fill);
    }
    let ppp = ui.pixels_per_point();
    let x = rect.left()
        + space::SPACE_1
        + komponist_ui::widgets::metrics::CHECK_COLUMN
        + space::SPACE_1;
    let g = w::galley(
        ui,
        label,
        fonts::font_id(&typography::UI),
        typography::UI.line_height,
    );
    ui.painter()
        .galley(w::text_pos(rect, &g, x, ppp), g, look.text);
    r
}

// ------------------------------------------------------------------------------------------------
// The output button
// ------------------------------------------------------------------------------------------------

/// What the output button's popover offers below the separator.
fn output_extras(lang: Lang) -> [&'static str; 2] {
    [lang.routing_new_bus(), lang.routing_new_group()]
}

/// The output of `track` as a button (`rect` high 20): the bus's dot, its name, a caret. A click
/// opens the list of [`super::output_targets`] („Master“, the groups and buses, then „Neuer Bus
/// …“ and „Neue Gruppe …“); the choice goes out as `SetOutput` (or a new bus around the track).
pub fn output_button(ui: &mut Ui, ctx: &mut ViewCtx<'_>, track: &Track, rect: Rect, salt: &str) {
    let project = std::sync::Arc::clone(ctx.project);
    let t = ctx.lang;
    let target = super::output_of(t, &project, track);
    let r = ui.interact(rect, ui.scope_id().with((salt, "output")), Sense::click());
    let egui_ctx = ui.ctx().clone();
    let popover = r.id.with("komponist_output_popover");
    let open = floating::is_open(&egui_ctx, popover);
    let name = t.routing_output_name(&target.label);
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    accesskit(&r, |n| {
        n.set_has_popup(egui::accesskit::HasPopup::Menu);
        n.set_expanded(open);
    });
    if !open {
        let tip = t.routing_output_tip(&track.name);
        Tip::new(&tip).attach(&r);
    }
    let st = State::of(ui, &r);
    let fill = if open {
        color::SURFACE_5
    } else {
        match (st.pressed, st.hovered) {
            (true, _) => color::SURFACE_5,
            (_, true) => color::SURFACE_4,
            _ => color::SURFACE_3,
        }
    };
    px::fill(ui.painter(), rect, radius::S, color32(fill));
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    let ppp = ui.pixels_per_point();
    let mut x = rect.left() + m::ROW_PAD_X;
    if target.hue.is_some() {
        dot(
            ui,
            pos2(x + m::DOT / 2.0, rect.center().y),
            target.hue,
            true,
        );
        x += m::DOT + m::DOT_GAP;
    }
    let caret = Rect::from_center_size(
        pos2(rect.right() - m::ROW_PAD_X - 3.0, rect.center().y),
        vec2(6.0, 4.0),
    );
    let style = crate::views::mixer::metrics::TEXT_10;
    let g = elided_galley(
        ui,
        &target.label,
        fonts::font_id(&style),
        style.line_height,
        caret.left() - x - m::DOT_GAP,
    );
    let pos = w::text_pos(rect, &g, x, ppp);
    ui.painter().galley(pos, g, color32(color::TEXT_1));
    icons::paint_caret(
        ui.painter(),
        caret,
        icons::Caret::Down,
        color32(color::TEXT_2),
    );

    if r.clicked() {
        if open {
            floating::close(&egui_ctx, popover);
        } else {
            let rows = super::output_targets(t, &project, track.id);
            let current = rows.iter().position(|t| t.id == target.id);
            open_popover(&egui_ctx, popover, current);
            egui_ctx.memory_mut(|mem| mem.surrender_focus(r.id));
        }
    }
    if floating::is_open(&egui_ctx, popover) {
        let rows = super::output_targets(t, &project, track.id);
        let current = rows.iter().position(|t| t.id == target.id);
        let picked = target_popover(
            &egui_ctx,
            popover,
            &r,
            &t.routing_output_list(&track.name),
            &rows,
            current,
            &output_extras(t),
        );
        match picked {
            Some(Picked::Row(i)) => ctx.send(Action::Edit(EditAction::SetOutput {
                tracks: vec![track.id],
                output: rows[i].output(),
            })),
            Some(Picked::Extra(0)) => ctx.send(Action::Edit(EditAction::AddBus(
                komponist_model::edits::NewBus {
                    members: vec![track.id],
                    ..Default::default()
                },
            ))),
            Some(Picked::Extra(_)) => {
                ctx.send(Action::Edit(EditAction::GroupTracks(vec![track.id])))
            }
            None => {}
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The send row
// ------------------------------------------------------------------------------------------------

/// How much of a send the row shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The mixer strip: dot, name, level fill and value (`02-mixer-voll`).
    Compact,
    /// Inspector and dock popover: the compact row, a „pre“ / „post“ chip and a „×“ button
    /// (`04-mixer-dock`, `07-inspektor`).
    Wide,
}

/// The level drag in progress: the unquantised level at the press.
#[derive(Clone, Copy)]
struct LevelDrag {
    raw: f32,
}

/// One send row at `rect` (the strip's rows are [`m::ROW_HEIGHT`] high).
///
/// - Drag ↔ sets the level (relative; Shift fine, Ctrl coarse), one undo step; inside the
///   gesture the engine gets the level on the fast path. Arrow keys on the focused row and the
///   wheel step it; Alt-click or double-click sets the default (−6 dB).
/// - A click on the name opens the list of buses to point the send at.
/// - Right click: „Vor Fader“, „Nach Fader“, switch off / on, „Entfernen“.
pub fn send_row(ui: &mut Ui, ctx: &mut ViewCtx<'_>, info: &SendInfo, rect: Rect, kind: RowKind) {
    let wide = kind == RowKind::Wide;
    let tail = if wide {
        m::TAP_CHIP_WIDTH + m::REMOVE_WIDTH + 2.0 * m::PART_GAP
    } else {
        0.0
    };
    let body = Rect::from_min_max(rect.min, pos2(rect.right() - tail, rect.bottom()));
    let id_salt = ("komponist_send_row", info.id.0, wide);
    let r = ui.interact(body, ui.scope_id().with(id_salt), Sense::click_and_drag());
    let egui_ctx = ui.ctx().clone();
    let t = ctx.lang;
    let tap_key = if info.tap == Tap::Pre { "pre" } else { "post" };
    let name = if info.enabled {
        t.routing_send_on(&info.name, fmt_db(info.level_db), tap_key)
    } else {
        t.routing_send_off(&info.name, tap_key)
    };
    let db = info.level_db;
    r.widget_info(|| {
        let mut i = WidgetInfo::slider(true, f64::from(db), &name);
        i.label = Some(name.clone());
        i
    });
    let retarget = r.id.with("komponist_send_retarget");

    // Level: drag, keys, wheel, default.
    let mods = ui.input(|i| i.modifiers);
    let [lo, hi] = [VOLUME_DB_MIN, VOLUME_DB_MAX];
    let per_px = (m::FILL_RANGE_DB[1] - m::FILL_RANGE_DB[0]) / body.width().max(1.0);
    let mut new_db = None;
    if r.drag_started() {
        egui_ctx.data_mut(|d| d.insert_temp(r.id, LevelDrag { raw: db }));
    }
    if r.dragged() {
        let mut st: LevelDrag = egui_ctx
            .data(|d| d.get_temp(r.id))
            .unwrap_or(LevelDrag { raw: db });
        st.raw = (st.raw + r.drag_delta().x * per_px * drag::speed(mods) as f32).clamp(lo, hi);
        egui_ctx.data_mut(|d| d.insert_temp(r.id, st));
        new_db = Some(round_tenth(st.raw));
    }
    let mut reset = false;
    if r.has_focus() && !r.dragged() {
        let step = if mods.shift {
            m::KEY_STEP_DB / 5.0
        } else {
            m::KEY_STEP_DB
        };
        let key = |k| {
            egui_ctx.input_mut(|i| {
                i.consume_key(Modifiers::NONE, k) || i.consume_key(Modifiers::SHIFT, k)
            })
        };
        let up = key(Key::ArrowRight) | key(Key::ArrowUp);
        let down = key(Key::ArrowLeft) | key(Key::ArrowDown);
        if up != down {
            let d = if up { step } else { -step };
            new_db = Some(round_tenth((db + d).clamp(lo, hi)));
        }
    }
    match ClickAction::of_control(&r) {
        ClickAction::Reset => reset = true,
        ClickAction::None | ClickAction::TextEntry => {}
    }
    let level = |v: f32| {
        Action::Edit(EditAction::SetSend {
            send: info.id,
            prop: SendProp::LevelDb(v),
        })
    };
    if r.drag_started() {
        ctx.send(Action::Edit(EditAction::BeginGesture {
            label: UNDO_SEND_LEVEL.to_owned(),
        }));
    }
    if reset {
        ctx.send(level(SEND_LEVEL_DEFAULT_DB));
    } else if let Some(v) = new_db.filter(|v| *v != db) {
        ctx.send(level(v));
    }
    if r.drag_stopped() {
        ctx.send(Action::Edit(EditAction::EndGesture));
        egui_ctx.data_mut(|d| d.remove::<LevelDrag>(r.id));
    }
    if r.hovered() || r.dragged() {
        egui_ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    // A plain click on the name changes the bus.
    let name_zone = Rect::from_min_max(
        body.min,
        pos2(body.left() + body.width() * 0.62, body.bottom()),
    );
    if r.clicked()
        && !mods.alt
        && r.interact_pointer_pos()
            .is_some_and(|p| name_zone.contains(p))
    {
        if floating::is_open(&egui_ctx, retarget) {
            floating::close(&egui_ctx, retarget);
        } else {
            let rows = super::retarget_rows(t, ctx.project, info.track, info.target);
            let current = rows.iter().position(|t| t.id == Some(info.target));
            open_popover(&egui_ctx, retarget, current);
        }
    }

    paint_row(ui, &r, body, info, !wide);

    // The bus list.
    if floating::is_open(&egui_ctx, retarget) {
        let rows = super::retarget_rows(t, ctx.project, info.track, info.target);
        let current = rows.iter().position(|t| t.id == Some(info.target));
        if let Some(Picked::Row(i)) = target_popover(
            &egui_ctx,
            retarget,
            &r,
            t.routing_sends(),
            &rows,
            current,
            &[],
        ) && let Some(target) = rows[i].id
            && target != info.target
        {
            ctx.send(Action::Edit(EditAction::SetSend {
                send: info.id,
                prop: SendProp::Target(target),
            }));
        }
    }

    // Context menu.
    #[derive(Clone, Copy)]
    enum Choice {
        Tap(Tap),
        Enabled(bool),
        Remove,
    }
    let toggle = if info.enabled {
        t.routing_switch_off()
    } else {
        t.routing_switch_on()
    };
    let entries = [
        Entry::action(t.routing_pre(), Choice::Tap(Tap::Pre)).checked(info.tap == Tap::Pre),
        Entry::action(t.routing_post(), Choice::Tap(Tap::Post)).checked(info.tap == Tap::Post),
        Entry::separator(),
        Entry::action(toggle, Choice::Enabled(!info.enabled)),
        Entry::action(t.routing_remove(), Choice::Remove),
    ];
    match ContextMenu::new(t.routing_sends(), &entries).show_on(&r) {
        Some(Choice::Tap(tap)) => ctx.send(Action::Edit(EditAction::SetSend {
            send: info.id,
            prop: SendProp::Tap(tap),
        })),
        Some(Choice::Enabled(on)) => ctx.send(Action::Edit(EditAction::SetSend {
            send: info.id,
            prop: SendProp::Enabled(on),
        })),
        Some(Choice::Remove) => ctx.send(Action::Edit(EditAction::RemoveSend(info.id))),
        None => {}
    }

    if wide {
        let chip = Rect::from_min_size(
            pos2(body.right() + m::PART_GAP, rect.top()),
            vec2(m::TAP_CHIP_WIDTH, rect.height()),
        );
        let remove = Rect::from_min_size(
            pos2(chip.right() + m::PART_GAP, rect.top()),
            vec2(m::REMOVE_WIDTH, rect.height()),
        );
        tap_chip(ui, ctx, info, chip);
        remove_button(ui, ctx, info, remove);
    }
}

/// A level rounded to a tenth of a dB (what the value shows).
fn round_tenth(db: f32) -> f32 {
    (db * 10.0).round() / 10.0
}

/// Paints the row: background, level fill, dot, name, value (orange before the fader).
fn paint_row(ui: &Ui, r: &Response, rect: Rect, info: &SendInfo, compact: bool) {
    let st = State::of(ui, r);
    px::fill(ui.painter(), rect, radius::S, state::surface_for(2, st));
    if info.enabled {
        let w_fill = px::snap(
            rect.width() * fill_fraction(info.level_db),
            ui.pixels_per_point(),
        );
        if w_fill > 0.0 {
            let bar = Rect::from_min_size(rect.min, vec2(w_fill, rect.height()));
            px::fill(ui.painter(), bar, radius::S, color32(color::ACCENT_SURFACE));
        }
    }
    let ppp = ui.pixels_per_point();
    let mut x = rect.left() + m::ROW_PAD_X;
    dot(
        ui,
        pos2(x + m::DOT / 2.0, rect.center().y),
        Some(info.hue),
        info.enabled,
    );
    x += m::DOT + m::DOT_GAP;
    let value = info.value_text(komponist_ui::lang(ui.ctx()), compact);
    let vstyle = typography::VALUE_S;
    let vg = w::galley(ui, &value, fonts::font_id(&vstyle), vstyle.line_height);
    let vx = rect.right() - m::ROW_PAD_X - vg.size().x;
    let ink = match (info.enabled, info.tap) {
        (false, _) => color::TEXT_3,
        (true, Tap::Pre) => color::METER_LOUD,
        (true, Tap::Post) => color::TEXT_2,
    };
    let vpos = w::text_pos(rect, &vg, vx, ppp);
    ui.painter().galley(vpos, vg, color32(ink));
    let style = crate::views::mixer::metrics::TEXT_10;
    let ng = elided_galley(
        ui,
        &info.name,
        fonts::font_id(&style),
        style.line_height,
        vx - x - m::DOT_GAP,
    );
    let npos = w::text_pos(rect, &ng, x, ppp);
    let name_ink = if info.enabled {
        color::TEXT_1
    } else {
        color::TEXT_3
    };
    ui.painter().galley(npos, ng, color32(name_ink));
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
}

/// The „pre“ / „post“ chip of a wide row: „pre“ is the lit state (`04-mixer-dock`).
fn tap_chip(ui: &mut Ui, ctx: &mut ViewCtx<'_>, info: &SendInfo, rect: Rect) {
    let r = ui.interact(
        rect,
        ui.scope_id().with(("komponist_send_tap", info.id.0)),
        Sense::click(),
    );
    let track_name = ctx
        .project
        .track(info.track)
        .map_or("", |t| t.name.as_str())
        .to_owned();
    let t = ctx.lang;
    let pre = info.tap == Tap::Pre;
    let name = t.routing_send_tap(&track_name, &info.name, if pre { "pre" } else { "post" });
    r.widget_info(|| WidgetInfo::selected(Role::Button, true, pre, &name));
    Tip::new(if pre {
        t.routing_pre_tip()
    } else {
        t.routing_post_tip()
    })
    .attach(&r);
    let st = State::of(ui, &r);
    let fill = match (pre, st.hovered) {
        (true, _) => color::TEXT_3,
        (false, true) => color::SURFACE_4,
        (false, false) => color::SURFACE_3,
    };
    px::fill(ui.painter(), rect, radius::S, color32(fill));
    let ink = if pre { color::SURFACE_0 } else { color::TEXT_3 };
    let style = typography::VALUE_S;
    let text = if pre {
        t.routing_pre_short()
    } else {
        t.routing_post_short()
    };
    let g = w::galley(ui, text, fonts::font_id(&style), style.line_height);
    let pos = w::text_pos_centered(rect, &g, ui.pixels_per_point());
    ui.painter().galley(pos, g, color32(ink));
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    if r.clicked() {
        ctx.send(Action::Edit(EditAction::SetSend {
            send: info.id,
            prop: SendProp::Tap(if pre { Tap::Post } else { Tap::Pre }),
        }));
    }
}

/// The „×“ of a wide row.
fn remove_button(ui: &mut Ui, ctx: &mut ViewCtx<'_>, info: &SendInfo, rect: Rect) {
    let r = ui.interact(
        rect,
        ui.scope_id().with(("komponist_send_remove", info.id.0)),
        Sense::click(),
    );
    let t = ctx.lang;
    let name = t.routing_remove_send(&info.name);
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    Tip::new(t.routing_remove()).attach(&r);
    let st = State::of(ui, &r);
    let ink = if st.hovered {
        color::TEXT_1
    } else {
        color::TEXT_3
    };
    icons::paint_in(
        ui,
        Rect::from_center_size(rect.center(), vec2(10.0, 10.0)),
        komponist_tokens::Icon::X,
        color32(ink),
    );
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    if r.clicked() {
        ctx.send(Action::Edit(EditAction::RemoveSend(info.id)));
    }
}

/// The „+“ row of a send list: a dashed frame; a click opens the list of buses (the unusable ones
/// grey) with „Neuer Effekt-Bus“ below (alone while the project has no bus); the choice goes out as `AddSend` / `SendToNewBus`.
pub fn add_send_row(ui: &mut Ui, ctx: &mut ViewCtx<'_>, track: &Track, rect: Rect, wide: bool) {
    let r = ui.interact(
        rect,
        ui.scope_id().with(("komponist_add_send", track.id.0, wide)),
        Sense::click(),
    );
    let t = ctx.lang;
    let name = t.routing_add_send_name(&track.name);
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    Tip::new(t.routing_add_send()).attach(&r);
    let st = State::of(ui, &r);
    if st.hovered || st.pressed {
        px::fill(ui.painter(), rect, radius::S, state::surface_for(1, st));
    }
    crate::views::mixer::dashed_rect(ui, rect, color32(color::LINE_1));
    let style = typography::SMALL;
    let label = if wide { t.routing_add_send() } else { "+" };
    let g = w::galley(ui, label, fonts::font_id(&style), style.line_height);
    let pos = w::text_pos_centered(rect, &g, ui.pixels_per_point());
    ui.painter()
        .galley(pos, g, state::text_for(color::TEXT_3, st));
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    let egui_ctx = ui.ctx().clone();
    let popover = r.id.with("komponist_add_send_popover");
    if r.clicked() {
        if floating::is_open(&egui_ctx, popover) {
            floating::close(&egui_ctx, popover);
        } else {
            open_popover(&egui_ctx, popover, None);
            egui_ctx.memory_mut(|mem| mem.surrender_focus(r.id));
        }
    }
    if floating::is_open(&egui_ctx, popover) {
        let rows = super::send_targets(t, ctx.project, track.id);
        let picked = target_popover(
            &egui_ctx,
            popover,
            &r,
            t.routing_send_to(),
            &rows,
            None,
            &[t.routing_new_effect_bus()],
        );
        match picked {
            Some(Picked::Row(i)) => {
                if let Some(target) = rows[i].id {
                    ctx.send(Action::Edit(EditAction::AddSend {
                        track: track.id,
                        target,
                    }));
                }
            }
            Some(Picked::Extra(_)) => {
                ctx.send(Action::Edit(EditAction::SendToNewBus(vec![track.id])));
            }
            None => {}
        }
    }
}

// ------------------------------------------------------------------------------------------------
// The dock chip and its popover
// ------------------------------------------------------------------------------------------------

/// The routing chip of a lean strip (`04-mixer-dock`): an arrow and the output bus's dot, or the
/// dots of the sends. Returns `true` while its popover is open. A click toggles the popover;
/// Strg+click shows the track in the inspector (selects it).
pub fn routing_chip(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    track: &Track,
    rect: Rect,
    chip: &super::ChipInfo,
) {
    let project = std::sync::Arc::clone(ctx.project);
    let r = ui.interact(
        rect,
        ui.scope_id().with(("komponist_routing_chip", track.id.0)),
        Sense::click(),
    );
    let egui_ctx = ui.ctx().clone();
    let popover = r.id.with("komponist_routing_popover");
    let open = floating::is_open(&egui_ctx, popover);
    let t = ctx.lang;
    let out = super::output_of(t, &project, track);
    let name = t.routing_chip(track.sends.len(), &track.name, &out.label);
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    accesskit(&r, |n| {
        n.set_has_popup(egui::accesskit::HasPopup::Dialog);
        n.set_expanded(open);
    });
    if !open {
        Tip::new(t.routing_routing())
            .detail(
                Platform::current().pick(t.routing_popover_hint(), t.routing_popover_hint_mac()),
            )
            .attach(&r);
    }
    let st = State::of(ui, &r);
    let fill = if open {
        color::SURFACE_5
    } else if st.hovered {
        color::SURFACE_4
    } else {
        color::SURFACE_3
    };
    px::fill(ui.painter(), rect, radius::S, color32(fill));
    if st.focused {
        focus::paint_ring(ui.painter(), rect, radius::S);
    }
    let cy = rect.center().y;
    let d = m::CHIP_DOT;
    let hue_dot = |x: f32, hue: u16| {
        ui.painter()
            .circle_filled(pos2(x, cy), d / 2.0, color32(base_color(hue)));
    };
    match chip.output {
        Some(hue) => {
            // Arrow + dot.
            let a = pos2(rect.left() + 5.0, cy);
            let stroke = egui::Stroke::new(1.0, color32(color::TEXT_2));
            ui.painter().line_segment([a, a + vec2(5.0, 0.0)], stroke);
            ui.painter()
                .line_segment([a + vec2(5.0, 0.0), a + vec2(3.0, -2.0)], stroke);
            ui.painter()
                .line_segment([a + vec2(5.0, 0.0), a + vec2(3.0, 2.0)], stroke);
            hue_dot(rect.right() - 5.0, hue);
        }
        None => {
            let n = chip.sends.len().min(2);
            let total = n as f32 * d + (n.saturating_sub(1)) as f32 * 2.0;
            let mut x = rect.center().x - total / 2.0 + d / 2.0;
            for &h in chip.sends.iter().take(2) {
                hue_dot(x, h);
                x += d + 2.0;
            }
        }
    }
    if r.clicked() {
        if ctx_command(&egui_ctx) {
            ctx.send(Action::Ui(crate::actions::UiAction::Select(
                crate::actions::SelectionChange::Tracks {
                    tracks: vec![track.id],
                    mode: crate::actions::SelectMode::Replace,
                },
            )));
        } else if open {
            floating::close(&egui_ctx, popover);
        } else {
            floating::open(&egui_ctx, popover);
        }
    }
    if floating::is_open(&egui_ctx, popover) {
        show_popover(ctx, track, &r, popover);
    }
}

/// Whether Strg is held.
fn ctx_command(ctx: &Context) -> bool {
    ctx.input(|i| i.modifiers.command || i.modifiers.ctrl)
}

/// The routing popover of the dock: title, the output button, the send rows, „+“, a hint.
fn show_popover(ctx: &mut ViewCtx<'_>, track: &Track, chip: &Response, id: Id) {
    let egui_ctx = chip.ctx.clone();
    let project = std::sync::Arc::clone(ctx.project);
    let t = ctx.lang;
    let area = Area::new(id)
        .order(Order::Foreground)
        .kind(UiKind::Menu)
        .pivot(Align2::LEFT_TOP)
        .fixed_pos(chip.rect.left_bottom() + vec2(0.0, space::SPACE_1))
        .constrain(true)
        .accessibility_parent(chip.id)
        .accessible_name(t.routing_popover_title(&track.name))
        .show(&egui_ctx, |ui| {
            menu_frame()
                .show(ui, |ui| {
                    ui.set_width(m::POPOVER_WIDTH);
                    ui.spacing_mut().item_spacing.y = space::SPACE_1;
                    let pad = space::SPACE_1;
                    let title = t.routing_popover_title(&track.name).to_uppercase();
                    ui.add_space(pad);
                    section_label(ui, &title);
                    labelled_row(ui, t.routing_output(), |ui, rect| {
                        output_button(ui, ctx, track, rect, "popover");
                    });
                    section_label(ui, t.routing_sends());
                    let sends = super::sends_of(&project, track);
                    let width = m::POPOVER_WIDTH - 2.0 * pad;
                    for info in &sends {
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(width, m::ROW_HEIGHT), Sense::hover());
                        send_row(ui, ctx, info, rect, RowKind::Wide);
                    }
                    if !track.is_bus() {
                        let (rect, _) =
                            ui.allocate_exact_size(vec2(width, m::ROW_HEIGHT), Sense::hover());
                        add_send_row(ui, ctx, track, rect, true);
                    }
                    ui.add_space(pad);
                })
                .inner
        });
    let rect = area.response.rect;
    // The output list and the send lists are floating areas of their own: a press on them is not
    // a press outside.
    let open_child = floating::current(&egui_ctx).is_some_and(|c| c != id);
    if !open_child
        && (pressed_outside(&egui_ctx, &[rect, chip.rect])
            || egui_ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)))
    {
        floating::close(&egui_ctx, id);
    }
}

/// A small section label in the popover (`type.label`, `text.3`).
fn section_label(ui: &mut Ui, text: &str) {
    let g = w::styled_galley(ui, text, &typography::LABEL);
    let (rect, _) = ui.allocate_exact_size(
        vec2(m::POPOVER_WIDTH - 2.0 * space::SPACE_1, g.size().y),
        Sense::hover(),
    );
    let pos = w::text_pos(
        rect,
        &g,
        rect.left() + space::SPACE_0_5,
        ui.pixels_per_point(),
    );
    ui.painter().galley(pos, g, color32(color::TEXT_3));
}

/// A row with a key on the left and a control in the rest.
fn labelled_row(ui: &mut Ui, key: &str, add: impl FnOnce(&mut Ui, Rect)) {
    let width = m::POPOVER_WIDTH - 2.0 * space::SPACE_1;
    let (rect, _) = ui.allocate_exact_size(vec2(width, m::ROW_HEIGHT), Sense::hover());
    let g = w::galley(
        ui,
        key,
        fonts::font_id(&typography::SMALL),
        typography::SMALL.line_height,
    );
    let key_w = 56.0;
    let pos = w::text_pos(
        rect,
        &g,
        rect.left() + space::SPACE_0_5,
        ui.pixels_per_point(),
    );
    ui.painter().galley(pos, g, color32(color::TEXT_2));
    let field = Rect::from_min_max(pos2(rect.left() + key_w, rect.top()), rect.max);
    add(ui, field);
}

/// The track `id`'s hue as the strips draw it, for callers that only have the project.
pub fn track_hue(project: &Project, id: TrackId) -> Option<u16> {
    project.track(id).map(|t| t.hue.0)
}

/// Whether the channel is a bus of the project (helper for views).
pub fn is_bus(project: &Project, channel: ChannelRef) -> bool {
    match channel {
        ChannelRef::Track(t) => project.track(t).is_some_and(|t| t.is_bus()),
        ChannelRef::Master => false,
    }
}

/// The output as a `TrackOutput` for a [`Target`] id.
pub fn output_for(id: Option<TrackId>) -> TrackOutput {
    id.map_or(TrackOutput::Master, TrackOutput::Bus)
}
