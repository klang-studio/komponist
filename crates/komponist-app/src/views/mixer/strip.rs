//! One channel strip: painted top to bottom ([`Rows`]), with the WP-20/21 widgets (knob,
//! toggles, fader, meter, level value) on their places, the insert slots and the reserved sends.

use egui::{
    Align, Layout, Modifiers, Pos2, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2,
    vec2,
};
use komponist_model::edits::{ChannelProp, TrackProp};
use komponist_model::format::{fmt_db, fmt_pan};
use komponist_model::{ChannelRef, DeviceId, PluginRef};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, size, typography};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::fader::{DbValue, Fader};
use komponist_ui::widgets::knob::Knob;
use komponist_ui::widgets::meter::{Meter, MeterState};
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::value_menu;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px};

use super::metrics::{self as m, Tier};
use super::model::{Insert, SlotState, Strip, move_target_in, names, slot_at};
use super::send_gesture;
use crate::actions::{
    Action, DropTarget, EditAction, PluginAction, SelectMode, SelectionChange, UiAction,
};
use crate::meters::Meters;
use crate::views::devices::add_menu;
use crate::views::devices::model::{Placement, place_plugin};
use crate::views::late;
use crate::views::{ViewCtx, elided_galley, group_bounds};

/// The drag payload of an insert slot being reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotDrag {
    /// The device.
    pub device: DeviceId,
    /// Its channel.
    pub channel: ChannelRef,
    /// Its index in the chain.
    pub from: usize,
}

/// Where the parts of a strip lie (the `isMixer` markup, top to bottom).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rows {
    /// The colour bar.
    pub bar: Rect,
    /// The name row with its padding (`accent.surface` when selected).
    pub name: Rect,
    /// The instrument line.
    pub line: Rect,
    /// "Inserts", if shown.
    pub inserts_label: Option<Rect>,
    /// The insert box.
    pub inserts: Rect,
    /// "Sends" and the two reserved rows, if shown.
    pub sends: Option<(Rect, Rect)>,
    /// The pan knob (28 × 28 right of the pan value; Ø 20 at the right end of the values row in
    /// the lean strip).
    pub knob: Rect,
    /// The pan value next to the knob (empty in the lean strip).
    pub pan_value: Rect,
    /// The toggles row.
    pub toggles: Rect,
    /// The fader area (fader and meter).
    pub fader: Rect,
    /// The values row (level left; peak right, or the pan knob in the lean strip).
    pub values: Rect,
}

impl Rows {
    /// The rows of a strip at `rect` in `tier`.
    pub fn new(rect: Rect, tier: Tier) -> Rows {
        use typography::{LABEL, UI, VALUE_M, VALUE_S};
        let (l, r) = (rect.left(), rect.right());
        let inner =
            |top: f32, h: f32| Rect::from_x_y_ranges(l + m::PAD_X..=r - m::PAD_X, top..=top + h);
        let full = |top: f32, h: f32| Rect::from_x_y_ranges(l..=r, top..=top + h);
        let bar = full(rect.top(), m::COLOR_BAR);
        let name = full(bar.bottom(), m::NAME_TOP + UI.line_height + m::NAME_BOTTOM);
        if tier.lean {
            return Rows::lean(rect, bar, name);
        }
        let line = inner(name.bottom() + m::INSTRUMENT_TOP, m::TEXT_10.line_height);
        let mut y = line.bottom() + m::INSERTS_LABEL_TOP;
        let inserts_label = tier.inserts_label.then(|| {
            let r = inner(y, LABEL.line_height);
            y = r.bottom() + m::LABEL_BOTTOM;
            r
        });
        let inserts = inner(y, tier.inserts_height());
        y = inserts.bottom();
        let sends = tier.sends.then(|| {
            let label = inner(y + m::SENDS_LABEL_TOP, LABEL.line_height);
            let rows = inner(
                label.bottom() + m::LABEL_BOTTOM,
                m::rows_height(m::SEND_ROWS),
            );
            y = rows.bottom();
            (label, rows)
        });
        // The pan value stands right of the knob, the pair spans the inner width.
        let knob = Rect::from_min_size(
            pos2(l + m::PAD_X, y + m::PAN_TOP),
            vec2(size::KNOB, size::KNOB),
        );
        let pan_value = Rect::from_x_y_ranges(
            knob.right() + m::PAN_GAP..=r - m::PAD_X,
            knob.center().y - VALUE_S.line_height / 2.0
                ..=knob.center().y + VALUE_S.line_height / 2.0,
        );
        let toggles = full(knob.bottom() + m::PAN_BOTTOM, m::TOGGLE[1]);
        let values_h = m::VALUES_TOP + VALUE_M.line_height + m::VALUES_BOTTOM;
        let values = inner(
            rect.bottom() - values_h + m::VALUES_TOP,
            VALUE_M.line_height,
        );
        let fader_top = toggles.bottom() + m::TOGGLES_BOTTOM;
        let fader_bottom = (rect.bottom() - values_h).max(fader_top);
        let fader = full(fader_top, fader_bottom - fader_top);
        Rows {
            bar,
            name,
            line,
            inserts_label,
            inserts,
            sends,
            knob,
            pan_value,
            toggles,
            fader,
            values,
        }
    }

    /// Name, toggles, the fader down to the values row; the parts left out are
    /// empty rects at the name's foot.
    fn lean(rect: Rect, bar: Rect, name: Rect) -> Rows {
        use typography::VALUE_M;
        let l = rect.left();
        let r = rect.right();
        let d = size::KNOB_SMALL;
        let nothing = Rect::from_min_size(
            pos2(l + m::PAD_X, name.bottom()),
            vec2(rect.width() - 2.0 * m::PAD_X, 0.0),
        );
        let toggles = Rect::from_x_y_ranges(
            l..=r,
            name.bottom() + m::INSERTS_LABEL_TOP
                ..=name.bottom() + m::INSERTS_LABEL_TOP + m::TOGGLE[1],
        );
        let values_h = m::VALUES_TOP + VALUE_M.line_height + m::VALUES_BOTTOM;
        let values = Rect::from_x_y_ranges(
            l + m::PAD_X..=r - m::PAD_X,
            rect.bottom() - values_h + m::VALUES_TOP
                ..=rect.bottom() - values_h + m::VALUES_TOP + VALUE_M.line_height,
        );
        // The knob (Ø 20) is centred on the values row (16 px) and reaches into the paddings above
        // and below it, so it costs the fader no height.
        let knob =
            Rect::from_center_size(pos2(r - m::PAD_X - d / 2.0, values.center().y), vec2(d, d));
        let fader_top = toggles.bottom() + m::TOGGLES_BOTTOM;
        let fader_bottom = (rect.bottom() - values_h).max(fader_top);
        Rows {
            bar,
            name,
            line: nothing,
            inserts_label: None,
            inserts: nothing,
            sends: None,
            knob,
            pan_value: nothing,
            toggles,
            fader: Rect::from_x_y_ranges(l..=r, fader_top..=fader_bottom),
            values,
        }
    }
}

/// Runs `add` in a child of `ui` filling `rect`, laid out left to right from its top-left corner,
/// with a stable id.
fn place<R>(
    ui: &mut Ui,
    rect: Rect,
    salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut Ui) -> R,
) -> R {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(salt)
            .layout(Layout::left_to_right(Align::Min)),
    );
    add(&mut child)
}

/// Paints `text` in `font` left-aligned at `x` (or centred when `x` is `None`) in `rect`, cut
/// with "…" to the rect; returns the text's rect.
fn text(
    ui: &Ui,
    rect: Rect,
    text: &str,
    font: egui::FontId,
    line_height: f32,
    color: egui::Color32,
    centred: bool,
) -> Rect {
    let g = elided_galley(ui, text, font, line_height, rect.width());
    let ppp = ui.pixels_per_point();
    let pos = if centred {
        w::text_pos_centered(rect, &g, ppp)
    } else {
        w::text_pos(rect, &g, rect.left(), ppp)
    };
    let r = Rect::from_min_size(pos, g.size());
    ui.painter().galley(pos, g, color);
    r
}

/// A 1 px dashed border inside `rect` (`1px dashed`).
pub(super) fn dashed_rect(ui: &Ui, rect: Rect, color: egui::Color32) {
    let ppp = ui.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    let lw = px::line_width(ppp);
    let stroke = egui::Stroke::new(lw, color);
    let half = lw / 2.0;
    let (l, t, rr, b) = (
        r.left() + half,
        r.top() + half,
        r.right() - half,
        r.bottom() - half,
    );
    let [dash, gap] = m::DASH;
    for side in [
        [pos2(l, t), pos2(rr, t)],
        [pos2(rr, t), pos2(rr, b)],
        [pos2(rr, b), pos2(l, b)],
        [pos2(l, b), pos2(l, t)],
    ] {
        ui.painter()
            .extend(egui::Shape::dashed_line(&side, stroke, dash, gap));
    }
}

/// The property a toggle switches on a track.
pub(super) fn toggle_prop(kind: ToggleKind, value: bool) -> TrackProp {
    match kind {
        ToggleKind::Mute => TrackProp::Channel(ChannelProp::Mute(value)),
        ToggleKind::Solo => TrackProp::Channel(ChannelProp::Solo(value)),
        ToggleKind::Arm => TrackProp::Arm(value),
        ToggleKind::Monitor => TrackProp::Monitor(value),
    }
}

/// The edit setting `prop` on the strip's channel.
fn channel_edit(s: &Strip, prop: ChannelProp) -> Action {
    Action::Edit(match s.channel {
        ChannelRef::Track(t) => EditAction::SetTrackProp {
            tracks: vec![t],
            prop: TrackProp::Channel(prop),
        },
        ChannelRef::Master => EditAction::SetMasterProp(prop),
    })
}

/// The selection mode of a click with `mods`: Strg toggles, Shift extends, else replace.
fn click_mode(mods: Modifiers) -> SelectMode {
    if mods.command || mods.ctrl {
        SelectMode::Toggle
    } else if mods.shift {
        SelectMode::Extend
    } else {
        SelectMode::Replace
    }
}

/// Selects the strip's track ("Kanalzug Klick"); a plain click also clears the clip selection,
/// so the inspector shows the track (as a header click does).
fn select(ctx: &mut ViewCtx<'_>, s: &Strip, mods: Modifiers) {
    let Some(track) = s.track() else {
        return;
    };
    let mode = click_mode(mods);
    if mode == SelectMode::Replace {
        ctx.send(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: Vec::new(),
            mode: SelectMode::Replace,
        })));
    }
    ctx.send(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode,
    })));
}

/// Draws strip `s` at `rect` (clipped to `clip`) and handles its controls. Returns the toggle
/// events of a track strip; the mixer applies them (drag across strips needs all strips).
pub(super) fn show(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    s: &Strip,
    rect: Rect,
    clip: Rect,
    tier: Tier,
) -> Vec<(ToggleKind, ToggleEvent)> {
    let key = match s.channel {
        ChannelRef::Track(t) => t.0,
        ChannelRef::Master => u64::MAX,
    };
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("komponist_mixer_strip", key))
            .accessibility_role(Role::Group)
            .accessibility_label(names::strip(&s.name)),
    );
    let ui = &mut ui;
    ui.set_clip_rect(rect.intersect(clip));
    group_bounds(ui, rect);
    let selected = s.selected;
    ui.ctx()
        .accesskit_node_builder(ui.unique_id(), |n| n.set_selected(selected));
    let mods = ui.input(|i| i.modifiers);
    let rows = Rows::new(rect, tier);

    // The strip itself: a click selects the track.
    let bg = ui.interact(rect, ui.scope_id().with("strip"), Sense::CLICK);
    if bg.clicked() {
        select(ctx, s, mods);
    }
    paint_frame(ui, s, &rows, rect);
    name_and_line(ui, ctx, s, &rows, mods, tier);
    if let Some(label) = rows.inserts_label {
        let g = w::styled_galley(ui, names::INSERTS, &typography::LABEL);
        let pos = w::text_pos(label, &g, label.left(), ui.pixels_per_point());
        ui.painter()
            .galley(pos, g, fonts::style_color(&typography::LABEL));
        let r = ui.interact(label, ui.scope_id().with("inserts_label"), Sense::hover());
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, names::INSERTS));
    }
    inserts(ui, ctx, s, rows.inserts, tier);
    if let Some((label, sends)) = rows.sends {
        sends_area(ui, label, sends);
    }
    pan(ui, ctx, s, &rows, tier);
    let events = toggles(ui, ctx, s, rows.toggles);
    fader_and_meter(ui, ctx, s, rows.fader, mods);
    values(ui, ctx, s, rows.values, mods, tier);
    events
}

/// Background, colour bar and the name row's selection fill.
fn paint_frame(ui: &Ui, s: &Strip, rows: &Rows, rect: Rect) {
    let painter = ui.painter();
    let ground = if s.is_master() {
        color::SURFACE_2
    } else if s.selected {
        color::CANVAS_STRIP_SELECTED
    } else {
        color::SURFACE_1
    };
    px::fill(painter, rect, 0.0, color32(ground));
    let bar = match s.hue {
        Some(h) if s.dim => super::model::dim_color(h),
        Some(h) => super::model::base_color(h),
        None => color::TEXT_2,
    };
    px::fill(painter, rows.bar, 0.0, color32(bar));
    if s.selected {
        px::fill(painter, rows.name, 0.0, color32(color::ACCENT_SURFACE));
    }
}

/// Name (a button that selects the track) and the instrument line.
fn name_and_line(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    s: &Strip,
    rows: &Rows,
    mods: Modifiers,
    tier: Tier,
) {
    let style = typography::UI;
    let text_x = if tier.lean {
        rows.name.left() + m::PAD_X..=rows.name.right() - m::PAD_X
    } else {
        rows.line.x_range().into()
    };
    let line_rect = Rect::from_x_y_ranges(
        text_x,
        rows.name.top() + m::NAME_TOP..=rows.name.top() + m::NAME_TOP + style.line_height,
    );
    let ink = if s.dim { color::TEXT_3 } else { color::TEXT_1 };
    text(
        ui,
        line_rect,
        &s.name,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
        color32(ink),
        false,
    );
    let r = ui.interact(rows.name, ui.scope_id().with("name"), Sense::CLICK);
    if s.is_master() {
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &s.name));
    } else {
        let selected = s.selected;
        r.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &s.name));
        if r.clicked() {
            select(ctx, s, mods);
        }
    }
    // The lean strip has no instrument line (owner decision 2026-11-14): the name's tooltip and
    // its AccessKit description carry it.
    if tier.lean && !s.line.is_empty() {
        Tip::new(&s.line).attach(&r);
        let line = s.line.clone();
        ui.ctx()
            .accesskit_node_builder(r.id, |n| n.set_description(line));
    }
    let small = m::TEXT_10;
    if !s.line.is_empty() && !tier.lean {
        text(
            ui,
            rows.line,
            &s.line,
            fonts::font_id(&small),
            small.line_height,
            color32(color::TEXT_3),
            false,
        );
        let r = ui.interact(rows.line, ui.scope_id().with("line"), Sense::hover());
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &s.line));
    }
}

/// The insert slots, "+", drag to reorder and drops from the browser.
fn inserts(ui: &mut Ui, ctx: &mut ViewCtx<'_>, s: &Strip, area: Rect, tier: Tier) {
    let rows = tier.insert_rows;
    let mut spans = Vec::with_capacity(s.inserts.len());
    let alt = ui.input(|i| i.modifiers.alt);
    for (i, insert) in s.inserts.iter().enumerate() {
        let top = area.top() + i as f32 * (m::SLOT_HEIGHT + m::SLOT_GAP);
        let slot = Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), m::SLOT_HEIGHT));
        if i >= rows || slot.bottom() > area.bottom() + 0.5 {
            break;
        }
        spans.push((slot.top(), slot.bottom()));
        let response = slot_widget(ui, s, insert, slot);
        if response.clicked() {
            if alt {
                ctx.send(Action::Edit(EditAction::SetDeviceBypass {
                    device: insert.device,
                    bypass: !insert.bypass,
                }));
            } else if insert.internal {
                ctx.send(Action::Ui(UiAction::RevealDevice(insert.device)));
            } else {
                ctx.send(Action::Plugin(PluginAction::OpenWindow(insert.device)));
            }
        }
        if !insert.instrument {
            response.dnd_set_drag_payload(SlotDrag {
                device: insert.device,
                channel: s.channel,
                from: i,
            });
        }
    }
    let n = s.inserts.len();
    let plus_top = area.top() + n as f32 * (m::SLOT_HEIGHT + m::SLOT_GAP);
    let plus = Rect::from_min_size(
        pos2(area.left(), plus_top),
        vec2(area.width(), m::SLOT_HEIGHT),
    );
    if n < rows && plus.bottom() <= area.bottom() + 0.5 {
        add_slot(ui, ctx, s, plus);
    }
    drop_over(ui, ctx, s, area, &spans);
}

/// One insert slot: `surface.3` for the instrument (semibold), `surface.2` for effects, the
/// plugin name in Sans 10 ([`m::TEXT_10`]); bypassed, loading and dim in `text.3`, crashed and missing in
/// `state.error`.
fn slot_widget(ui: &mut Ui, s: &Strip, insert: &Insert, rect: Rect) -> Response {
    let sense = if insert.instrument {
        Sense::click()
    } else {
        Sense::click_and_drag()
    };
    let response = ui.interact(rect, ui.scope_id().with(("slot", insert.device.0)), sense);
    let st = State::of(ui, &response);
    let base = if insert.instrument { 3 } else { 2 };
    px::fill(ui.painter(), rect, radius::S, state::surface_for(base, st));
    let ink = match insert.state {
        SlotState::Crashed
        | SlotState::LoadFailed
        | SlotState::Unresponsive
        | SlotState::Missing => color::STATE_ERROR,
        SlotState::Loading => color::TEXT_3,
        SlotState::Ok if insert.late.is_some() => color::METER_LOUD,
        SlotState::Ok if insert.bypass || s.dim => color::TEXT_3,
        SlotState::Ok => color::TEXT_1,
    };
    let style = m::TEXT_10;
    let font = if insert.instrument {
        w::font_at(&style, wm::SEMIBOLD)
    } else {
        fonts::font_id(&style)
    };
    text(
        ui,
        rect.shrink2(vec2(m::SLOT_PAD_X, 0.0)),
        &insert.name,
        font,
        style.line_height,
        color32(ink),
        false,
    );
    if st.focused {
        komponist_ui::focus::paint_ring(ui.painter(), rect, radius::S);
    }
    let name = names::insert(&insert.name);
    let bypass = insert.bypass;
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Button, true, &name);
        info.current_text_value = match insert.late {
            Some(_) => Some(late::LATE.to_owned()),
            None => bypass.then(|| names::BYPASS.to_owned()),
        };
        info
    });
    let late_text = insert.late.map(late::late_blocks);
    Tip::new(&insert.name)
        .detail(late_text.as_deref().unwrap_or(insert.detail()))
        .attach(&response);
    if insert.late.is_some() {
        late::repaint_when_expired(ui.ctx());
    }
    if response.hovered() || response.dragged() {
        ui.ctx().set_cursor_icon(if response.dragged() {
            egui::CursorIcon::Grabbing
        } else {
            egui::CursorIcon::PointingHand
        });
    }
    response
}

/// The "+" slot: a dashed `line.1` frame; opens the add menu ([`add_menu`]) below it for the
/// strip's chain (audio effects only on the master).
fn add_slot(ui: &mut Ui, ctx: &mut ViewCtx<'_>, s: &Strip, rect: Rect) {
    let response = ui.interact(rect, ui.scope_id().with("add"), Sense::click());
    let st = State::of(ui, &response);
    if st.hovered || st.pressed {
        px::fill(ui.painter(), rect, radius::S, state::surface_for(1, st));
    }
    dashed_rect(ui, rect, color32(color::LINE_1));
    let style = typography::SMALL;
    text(
        ui,
        rect,
        names::ADD,
        fonts::font_id(&style),
        style.line_height,
        state::text_for(color::TEXT_3, st),
        true,
    );
    if st.focused {
        komponist_ui::focus::paint_ring(ui.painter(), rect, radius::S);
    }
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, names::ADD_EFFECT));
    Tip::new(names::ADD_EFFECT)
        .detail(names::ADD_EFFECT_DETAIL)
        .attach(&response);
    add_menu::show(ui, ctx, &response, s.channel, add_menu::Anchor::Below);
}

/// A slot of this strip or a browser plugin dragged over the strip: the 2 px accent line at the
/// insert position (a ring around the instrument for an instrument), the action on release.
fn drop_over(ui: &mut Ui, ctx: &mut ViewCtx<'_>, s: &Strip, area: Rect, spans: &[(f32, f32)]) {
    let dnd = ui.ctx().clone();
    let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
        return;
    };
    let strip = ui.max_rect();
    if !strip.contains(pos) || !ui.clip_rect().contains(pos) {
        return;
    }
    let released = ui.input(|i| i.pointer.any_released());
    let first = s.first_effect_slot();
    let slot = if area.contains(pos) {
        slot_at(spans, pos.y)
    } else {
        s.inserts.len()
    };
    let line_at = |ui: &Ui, slot: usize| {
        let y = match spans.get(slot) {
            Some((top, _)) => top - m::SLOT_GAP / 2.0,
            None => spans
                .last()
                .map_or(area.top(), |(_, bottom)| bottom + m::SLOT_GAP / 2.0),
        };
        let line = Rect::from_x_y_ranges(
            area.x_range(),
            y - m::INSERT_LINE / 2.0..=y + m::INSERT_LINE / 2.0,
        );
        px::fill(ui.painter(), line, 0.0, color32(color::ACCENT));
    };
    if let Some(drag) = egui::DragAndDrop::payload::<SlotDrag>(&dnd) {
        if drag.channel != s.channel || !area.contains(pos) {
            return;
        }
        // A MIDI effect stays in front of the instrument, any other effect behind it.
        let (lo, hi) = if s.inserts.get(drag.from).is_some_and(|i| i.midi) {
            (0, s.midi_effect_count())
        } else {
            (first, usize::MAX)
        };
        let Some(to) = move_target_in(drag.from, slot, lo, hi) else {
            return;
        };
        line_at(ui, slot.clamp(lo, hi.max(lo)));
        if released {
            egui::DragAndDrop::clear_payload(&dnd);
            ctx.send(Action::Edit(EditAction::MoveDevice {
                device: drag.device,
                to_index: to,
            }));
        }
        return;
    }
    let Some(plugin) = egui::DragAndDrop::payload::<PluginRef>(&dnd) else {
        return;
    };
    let Some(role) = ctx.engine.host().role(&plugin) else {
        return;
    };
    let midi = s.midi_effect_count();
    let Some(place) = place_plugin(role, s.is_master(), slot, first, midi) else {
        return;
    };
    match place {
        Placement::Instrument(at) => match spans.get(at) {
            Some((top, bottom)) => {
                let r = Rect::from_x_y_ranges(area.x_range(), *top..=*bottom);
                state::paint_select_ring(ui.painter(), r, radius::S);
            }
            None => line_at(ui, at),
        },
        Placement::Line(at) => line_at(ui, at),
    }
    let index = place.index();
    if released {
        egui::DragAndDrop::clear_payload(&dnd);
        ctx.send(Action::Plugin(PluginAction::Add {
            plugin: PluginRef::clone(&plugin),
            target: DropTarget::Chain {
                channel: s.channel,
                index,
            },
        }));
    }
}

/// "Sends" in `text.4` and two empty dashed rows: the place is reserved for busses.
fn sends_area(ui: &mut Ui, label: Rect, rows: Rect) {
    let g = w::styled_galley(ui, names::SENDS, &typography::LABEL);
    let pos = w::text_pos(label, &g, label.left(), ui.pixels_per_point());
    ui.painter().galley(pos, g, color32(color::TEXT_4));
    for i in 0..m::SEND_ROWS {
        let top = rows.top() + i as f32 * (m::SLOT_HEIGHT + m::SLOT_GAP);
        let r = Rect::from_min_size(pos2(rows.left(), top), vec2(rows.width(), m::SLOT_HEIGHT));
        dashed_rect(ui, r, color32(color::LINE_1));
    }
    let area = label.union(rows);
    let r = ui.interact(area, ui.scope_id().with("sends"), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(Role::Label, true, names::SENDS));
    Tip::new(names::SENDS).detail(names::SENDS_LATER).attach(&r);
}

/// The pan knob ("Ziehen relativ, Shift fein, Doppelklick oder Alt-Klick Standard, Enter oder
/// Rechtsklick Texteingabe") and its value.
fn pan(ui: &mut Ui, ctx: &mut ViewCtx<'_>, s: &Strip, rows: &Rows, tier: Tier) {
    let mut pan = s.pan;
    let name = names::pan(&s.name);
    let response = place(ui, rows.knob, "pan", |ui| {
        let knob = Knob::pan(&mut pan, name);
        ui.add(if tier.lean { knob.small() } else { knob })
    });
    send_gesture(ctx, &response, names::UNDO_PAN, || {
        channel_edit(s, ChannelProp::Pan(pan))
    });
    if value_menu::show(&response, || fmt_pan(pan)) {
        pan = 0.0;
        ctx.send(channel_edit(s, ChannelProp::Pan(pan)));
    }
    if tier.lean {
        return;
    }
    let style = typography::VALUE_S;
    text(
        ui,
        rows.pan_value,
        &fmt_pan(pan),
        fonts::font_id(&style),
        style.line_height,
        color32(color::TEXT_2),
        true,
    );
}

/// M S R (track) or M (master), 22 × 18, centred.
fn toggles(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    s: &Strip,
    row: Rect,
) -> Vec<(ToggleKind, ToggleEvent)> {
    let kinds: &[ToggleKind] = if s.is_master() {
        &[ToggleKind::Mute]
    } else {
        &[ToggleKind::Mute, ToggleKind::Solo, ToggleKind::Arm]
    };
    let [tw, th] = m::TOGGLE;
    // M S, then R apart from them by the group gap (owner decision 2026-11-14).
    let offset = |i: usize| ToggleKind::row_offset(i, tw, m::TOGGLE_GAP);
    let total = offset(kinds.len() - 1) + tw;
    let ppp = ui.pixels_per_point();
    let left = px::snap(row.center().x - total / 2.0, ppp);
    let mut out = Vec::new();
    for (i, &kind) in kinds.iter().enumerate() {
        let r = Rect::from_min_size(pos2(left + offset(i), row.top()), vec2(tw, th));
        let on = match kind {
            ToggleKind::Mute => s.mute,
            ToggleKind::Solo => s.solo,
            ToggleKind::Arm => s.arm,
            ToggleKind::Monitor => false,
        };
        let shown = place(ui, r, ("toggle", kind.letter()), |ui| {
            TrackToggle::new(kind, on)
                .implied(kind == ToggleKind::Mute && s.implied)
                .track(&s.name)
                .size(vec2(tw, th))
                .show(ui)
        });
        match (shown.event, s.channel) {
            (Some(ToggleEvent::Set { value, .. }), ChannelRef::Master) => {
                ctx.send(Action::Edit(EditAction::SetMasterProp(ChannelProp::Mute(
                    value,
                ))));
            }
            (Some(event), ChannelRef::Track(_)) => out.push((kind, event)),
            _ => {}
        }
    }
    out
}

/// The fader with its scale, and the meter next to it whose bars span the fader's track.
fn fader_and_meter(ui: &mut Ui, ctx: &mut ViewCtx<'_>, s: &Strip, area: Rect, mods: Modifiers) {
    let ppp = ui.pixels_per_point();
    let thumb = m::FADER.thumb();
    let length = (area.height() - thumb.y).max(1.0);
    let scale = m::scale_fits(length);
    let left = px::snap(area.center().x - m::fader_group_width(scale) / 2.0, ppp);
    let fader_w = komponist_ui::widgets::fader::Layout::desired_size(m::FADER, scale, length).x;
    let fader_rect = Rect::from_min_size(pos2(left, area.top()), vec2(fader_w, area.height()));
    let mut db = s.volume_db;
    let name = names::volume(&s.name);
    let response = place(ui, fader_rect, "fader", |ui| {
        ui.add(
            Fader::new(&mut db, name)
                .size(m::FADER)
                .scale(scale)
                .selected(s.selected)
                .track_length(length),
        )
    });
    send_gesture(ctx, &response, names::UNDO_VOLUME, || {
        channel_edit(s, ChannelProp::VolumeDb(db))
    });
    if value_menu::show(&response, || fmt_db(db)) {
        db = 0.0;
        ctx.send(channel_edit(s, ChannelProp::VolumeDb(db)));
    }

    let meter_left = fader_rect.right() + m::FADER_METER_GAP;
    let top = area.top() + thumb.y / 2.0 - m::METER_LED;
    let bottom = (area.bottom() - thumb.y / 2.0).max(top + 1.0);
    let meter_rect =
        Rect::from_x_y_ranges(meter_left..=meter_left + m::METER.thickness(), top..=bottom);
    let mut silent = MeterState::new();
    let state = match s.track() {
        Some(t) => ctx.meters.track_mut(t).unwrap_or(&mut silent),
        None => &mut ctx.meters.master,
    };
    let response = place(ui, meter_rect, "meter", |ui| {
        ui.add(
            Meter::new(state, Meters::NO_PEAKS, names::meter(&s.name))
                .style(m::METER)
                .length(meter_rect.height()),
        )
    });
    if response.clicked() {
        ctx.send(Action::Ui(UiAction::ResetMeters {
            track: s.track(),
            all: mods.alt,
        }));
    }
}

/// The level (double-click: text entry, Alt-click: 0 dB, click: clears the clip indicator) and
/// the peak value (click resets peak and clip, Alt-click all strips), red after clipping. The
/// lean strip has the level only.
fn values(ui: &mut Ui, ctx: &mut ViewCtx<'_>, s: &Strip, row: Rect, mods: Modifiers, tier: Tier) {
    let mut db = s.volume_db;
    let name = names::volume_value(&s.name);
    // The level stands left in the row (S04 `justify-content:space-between`).
    let value = komponist_ui::widgets::fader::VALUE_TEXT;
    let text_w = |t: &str| {
        w::galley(ui, t, fonts::font_id(&value), value.line_height)
            .size()
            .x
    };
    let widest = format!("{}48.0", komponist_model::format::MINUS);
    let box_w = text_w(&widest) + 2.0 * komponist_tokens::space::SPACE_1;
    let level = Rect::from_min_size(row.min, vec2(box_w, row.height()));
    let response = place(ui, level, "level", |ui| {
        ui.add(DbValue::new(&mut db, name).width(box_w).align_left(true))
    });
    if response.changed() {
        ctx.send(channel_edit(s, ChannelProp::VolumeDb(db)));
    } else if response.clicked() && !mods.alt {
        ctx.send(Action::Ui(UiAction::ResetMeters {
            track: s.track(),
            all: false,
        }));
    }

    // The lean strip shows no peak value (the meter LED carries peak and clip; its click resets
    // them) and puts the pan knob at the end of the row instead.
    if tier.lean {
        return;
    }
    let (peak, clipped) = {
        let state = match s.track() {
            Some(t) => ctx.meters.track(t),
            None => Some(&ctx.meters.master),
        };
        state.map_or((f32::NEG_INFINITY, false), |m| (m.peak_db(), m.clipped()))
    };
    // The design sets both values in Mono 11, which has no token; the peak, the secondary value,
    // takes `value.s` (Mono 10), so "−12.0" and "−19.1" fit side by side in the 68 px row.
    let style = typography::VALUE_S;
    let shown = fmt_db(peak);
    let g = w::galley(ui, &shown, fonts::font_id(&style), style.line_height);
    let ppp = ui.pixels_per_point();
    let pos = w::text_pos(row, &g, row.right() - g.size().x, ppp);
    let r = Rect::from_min_size(pos, g.size());
    let ink = if clipped {
        color::METER_CLIP
    } else {
        color::TEXT_3
    };
    ui.painter().galley(pos, g, color32(ink));
    let response = ui.interact(r, ui.scope_id().with("peak"), Sense::click());
    let label = names::peak(&s.name);
    let value = format!("{shown} dB{}", if clipped { ", Clip" } else { "" });
    response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::Button, true, &label);
        info.current_text_value = Some(value.clone());
        info
    });
    Tip::new(names::PEAK_TIP)
        .detail(names::PEAK_DETAIL)
        .attach(&response);
    if response.clicked() {
        ctx.send(Action::Ui(UiAction::ResetMeters {
            track: s.track(),
            all: mods.alt,
        }));
    }
}

/// The x range each track strip occupies, for drags across the toggles.
pub(super) fn crosses(span: egui::Rangef, from: Pos2, to: Pos2) -> bool {
    let (a, b) = (from.x.min(to.x), from.x.max(to.x));
    span.min <= b && a < span.max
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_follow_the_markup() {
        let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 980.0));
        let r = Rows::new(rect, Tier::FULL);
        assert_eq!(r.bar.height(), 3.0);
        // Name: padding 4 (tightened, owner decision 2026-11-14), line 16, padding 2.
        assert_eq!(r.name.height(), 22.0);
        assert_eq!(r.line.left(), 6.0);
        assert_eq!(r.inserts.height(), m::INSERTS_HEIGHT);
        // The reserved sends are hidden until busses exist.
        assert!(r.sends.is_none());
        assert_eq!(r.knob.size(), vec2(28.0, 28.0));
        assert_eq!(r.knob.center().x, 20.0);
        // The fader takes the rest; the values row ends 6 above the bottom.
        assert_eq!(r.values.bottom(), 974.0);
        assert!(r.fader.height() > 500.0);
        assert_eq!(r.fader.bottom(), r.values.top() - m::VALUES_TOP);
        // The normal dock: no sends.
        let small = Rows::new(
            Rect::from_min_size(pos2(0.0, 0.0), vec2(80.0, 292.0)),
            Tier::for_height(292.0),
        );
        assert!(small.sends.is_none() && small.inserts_label.is_none());
        // The lean strip's pan knob sits at the end of the values row and adds no height.
        assert_eq!(small.knob.size(), vec2(size::KNOB_SMALL, size::KNOB_SMALL));
        assert_eq!(small.knob.center().y, small.values.center().y);
        assert_eq!(small.knob.right(), small.values.right());
        assert!(small.knob.top() >= small.fader.bottom());
        assert!(small.knob.bottom() <= 292.0);
        assert!(small.fader.height() >= m::FADER_AREA_MIN - 0.5);
    }

    #[test]
    fn sweeps_cross_strips() {
        let span = egui::Rangef::new(81.0, 161.0);
        assert!(crosses(span, pos2(20.0, 5.0), pos2(100.0, 5.0)));
        assert!(crosses(span, pos2(200.0, 5.0), pos2(150.0, 5.0)));
        assert!(!crosses(span, pos2(0.0, 5.0), pos2(80.0, 5.0)));
    }
}
