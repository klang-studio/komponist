//! The „SC“ chip in the head of a device with a sidechain input and its popover (WP-104, owner
//! decision Q7 C, routing proposal, 08-sidechain-kopf variant C (internal notes)).
//!
//! The chip sits left of the preset button: „SC“, a dot in the colour of the source (an outline
//! while there is none), a caret. Off it is dimmed; with a source it has the accent edge and text.
//! A click or Enter opens the popover below it: the device's own input („— (intern:
//! Eingangssignal)“), the tracks, a separator, the buses, and the section „Abgriff“ with „Vor
//! Fader“ / „Nach Fader“ (the tap; enabled with a source). A source that would close a loop is
//! grey, its tooltip names the reason. Choosing a source sets it in one undo step
//! ([`EditAction::SetSidechain`]) and closes the popover; the tap stays open. A right click on
//! the chip offers „Quelle entfernen“.
//!
//! Keys while the popover is open: ↑ ↓ move over the rows (greyed ones too, so their reason is
//! reachable), Enter or Space chooses, Esc closes. Any device can have the chip: [`Card::has_sidechain`]
//! is the only switch (internal devices say so in their spec, plugins through the host).

use egui::{
    Align2, Area, Context, Id, Key, Modifiers, Order, Rect, Response, Role, Sense, Ui, UiKind,
    WidgetInfo, pos2, vec2,
};
use komponist_model::routing;
use komponist_model::{ChannelRef, DeviceId, Project, Sidechain, Tap, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, device, radius, size, space};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry, row_look};
use komponist_ui::widgets::tabs::{Item, Segmented};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::widgets::{self as w, accesskit, floating, menu_frame, pressed_outside};
use komponist_ui::{focus, fonts, icons, px};

use super::metrics;
use super::model::{Card, names};
use crate::actions::{Action, EditAction};
use crate::views::ViewCtx;
use crate::views::mixer::model::base_color;

/// One row of the source list.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Source {
    /// The track or bus; `None` for the device's own input.
    pub id: Option<TrackId>,
    /// The row's text.
    pub label: String,
    /// The track hue for the dot.
    pub hue: Option<u16>,
    /// Why the row cannot be chosen (feedback loop); `None` if it can.
    pub blocked: Option<String>,
}

/// The rows of the popover for `device`: its own input, the tracks, then the buses (the separator
/// between them is the caller's: `buses_from`). Every track and bus is listed; those
/// [`routing::sidechain_sources`] does not offer are blocked, with the reason.
pub(super) fn sources(project: &Project, device: DeviceId) -> (Vec<Source>, usize) {
    let valid = routing::sidechain_sources(project, device);
    let holder = project.find_device(device).map(|(c, _, _)| c);
    let holder_id = match holder {
        Some(ChannelRef::Track(t)) => Some(t),
        _ => None,
    };
    let holder_name = holder_id
        .and_then(|t| project.track(t))
        .map_or("", |t| t.name.as_str());
    let groups = holder_id
        .map(|t| routing::group_ancestors(project, t))
        .unwrap_or_default();
    let row = |t: &komponist_model::Track| {
        let blocked = (!valid.contains(&t.id)).then(|| {
            if Some(t.id) == holder_id {
                names::sc_reason_self(&t.name)
            } else if groups.contains(&t.id) {
                names::sc_reason_group(&t.name)
            } else {
                names::sc_reason_fed(&t.name, holder_name)
            }
        });
        let label = if Some(t.id) == holder_id {
            format!("{} ({})", t.name, names::SC_THIS_TRACK)
        } else {
            t.name.clone()
        };
        Source {
            id: Some(t.id),
            label,
            hue: Some(t.hue.0),
            blocked,
        }
    };
    let mut rows = vec![Source {
        id: None,
        label: names::SC_INTERNAL.to_owned(),
        hue: None,
        blocked: None,
    }];
    rows.extend(
        project
            .tracks
            .iter()
            .filter(|t| !t.is_bus())
            .map(|t| row(t)),
    );
    let buses_from = rows.len();
    rows.extend(project.tracks.iter().filter(|t| t.is_bus()).map(|t| row(t)));
    (rows, buses_from)
}

/// The width of the chip: padding, „SC“, the dot, the caret and the gaps between.
pub(super) fn chip_width(ui: &Ui) -> f32 {
    let text = w::galley(
        ui,
        names::SC,
        fonts::font_id(&typography::SMALL),
        typography::SMALL.line_height,
    )
    .size()
    .x
    .ceil();
    2.0 * metrics::PRESET_PAD_X
        + text
        + metrics::SC_DOT
        + metrics::SC_CARET[0]
        + 2.0 * metrics::HEAD_GAP
}

/// The chip's accessible name: „Sidechain: Drums“, „Sidechain: aus“.
fn chip_name(project: &Project, sidechain: Option<Sidechain>) -> String {
    match sidechain.and_then(|s| project.track(s.source)) {
        Some(t) => names::sc_chip(&t.name),
        None => names::SC_OFF.to_owned(),
    }
}

/// Open state of the popover: the row the keys are on.
#[derive(Clone, Copy, Default)]
struct Open {
    highlight: Option<usize>,
}

/// Draws the chip into `row` (right to left, so it lands left of what was added before), opens
/// and shows the popover and sends the chosen sidechain.
pub(super) fn chip(row: &mut Ui, ctx: &mut ViewCtx<'_>, card: &Card) {
    let project = std::sync::Arc::clone(ctx.project);
    let width = chip_width(row);
    let (rect, r) = row.allocate_exact_size(
        vec2(width, device::frame::preset_dropdown::H),
        Sense::click(),
    );
    let popover = r.id.with("komponist_sc_popover");
    let egui_ctx = row.ctx().clone();
    let open = floating::is_open(&egui_ctx, popover);
    let name = chip_name(&project, card.sidechain);
    let set = card.sidechain.filter(|s| project.track(s.source).is_some());
    r.widget_info(|| WidgetInfo::labeled(Role::Button, true, &name));
    accesskit(&r, |n| {
        n.set_has_popup(egui::accesskit::HasPopup::Menu);
        n.set_expanded(open);
    });
    if !open {
        Tip::new(names::SC_TIP).attach(&r);
    }

    // Paint.
    let base = if card.instrument {
        color::SURFACE_2
    } else {
        color::SURFACE_3
    };
    let fill = if open {
        color::SURFACE_5
    } else if r.hovered() {
        color::SURFACE_4
    } else {
        base
    };
    let ink = if set.is_some() {
        color::ACCENT
    } else {
        color::TEXT_3
    };
    let painter = row.painter();
    px::fill(painter, rect, metrics::HEAD_RADIUS, color32(fill));
    if set.is_some() {
        px::outline(
            painter,
            rect,
            metrics::HEAD_RADIUS,
            -1.0,
            color32(color::ACCENT),
        );
    }
    if r.has_focus() {
        focus::paint_ring(painter, rect, metrics::HEAD_RADIUS);
    }
    let ppp = row.pixels_per_point();
    let text = w::galley(
        row,
        names::SC,
        fonts::font_id(&typography::SMALL),
        typography::SMALL.line_height,
    );
    let mut x = rect.left() + metrics::PRESET_PAD_X;
    let pos = w::text_pos(rect, &text, x, ppp);
    x += text.size().x.ceil() + metrics::HEAD_GAP;
    row.painter().galley(pos, text, color32(ink));
    let dot = pos2(x + metrics::SC_DOT / 2.0, rect.center().y);
    match set.and_then(|s| project.track(s.source)) {
        Some(t) => {
            row.painter()
                .circle_filled(dot, metrics::SC_DOT / 2.0, color32(base_color(t.hue.0)));
        }
        None => {
            row.painter().circle_stroke(
                dot,
                metrics::SC_DOT / 2.0 - 0.5,
                egui::Stroke::new(1.0, color32(color::TEXT_4)),
            );
        }
    }
    x += metrics::SC_DOT + metrics::HEAD_GAP;
    let [cw, ch] = metrics::SC_CARET;
    icons::paint_caret(
        row.painter(),
        Rect::from_min_size(pos2(x, rect.center().y - ch / 2.0), vec2(cw, ch)),
        icons::Caret::Down,
        color32(ink),
    );

    // Context menu: remove the source.
    if set.is_some() {
        let entries = [Entry::action(names::SC_REMOVE, ())];
        if ContextMenu::new(names::SC_SOURCE, &entries)
            .show_on(&r)
            .is_some()
        {
            ctx.send(Action::Edit(EditAction::SetSidechain {
                device: card.device,
                sidechain: None,
            }));
        }
    }

    if r.clicked() {
        if open {
            floating::close(&egui_ctx, popover);
        } else {
            floating::open(&egui_ctx, popover);
            let current = sources(&project, card.device)
                .0
                .iter()
                .position(|s| s.id == set.map(|c| c.source));
            egui_ctx.data_mut(|d| {
                d.insert_temp(
                    popover,
                    Open {
                        highlight: current.or(Some(0)),
                    },
                )
            });
            // The keys belong to the popover now; Enter must not click the chip again.
            egui_ctx.memory_mut(|m| m.surrender_focus(r.id));
        }
    }
    if floating::is_open(&egui_ctx, popover) {
        show_popover(ctx, card, &project, set, &r, popover);
    }
}

/// The popover below the chip, right edges aligned.
fn show_popover(
    ctx: &mut ViewCtx<'_>,
    card: &Card,
    project: &Project,
    current: Option<Sidechain>,
    chip: &Response,
    id: Id,
) {
    let egui_ctx = chip.ctx.clone();
    let (rows, buses_from) = sources(project, card.device);
    let mut st: Open = egui_ctx.data(|d| d.get_temp(id)).unwrap_or_default();
    let mut chosen: Option<Option<TrackId>> = None;
    let mut close = keys(&egui_ctx, &rows, &mut st, &mut chosen);

    let selected = rows.iter().position(|s| s.id == current.map(|c| c.source));
    let area = Area::new(id)
        .order(Order::Foreground)
        .kind(UiKind::Menu)
        .pivot(Align2::RIGHT_TOP)
        .fixed_pos(chip.rect.right_bottom() + vec2(0.0, space::SPACE_1))
        .constrain(true)
        .accessibility_parent(chip.id)
        .accessible_name(names::SC_SOURCE)
        .show(&egui_ctx, |ui| {
            menu_frame()
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let mut clicked = None;
                    let mut hovered = None;
                    let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO);
                    for (i, s) in rows.iter().enumerate() {
                        if i == 1 || i == buses_from && buses_from > 1 && buses_from < rows.len() {
                            separator(ui);
                        }
                        let out = source_row(ui, s, selected == Some(i), st.highlight == Some(i));
                        if out.hovered() && moved {
                            hovered = Some(i);
                        }
                        if out.clicked() && s.blocked.is_none() {
                            clicked = Some(i);
                        }
                    }
                    separator(ui);
                    let tap = tap_section(ui, current);
                    (clicked, hovered, tap)
                })
                .inner
        });
    let (clicked, hovered, tap) = area.inner;
    if let Some(h) = hovered {
        st.highlight = Some(h);
    }
    if let Some(i) = clicked {
        chosen = Some(rows[i].id);
    }
    if let (Some(tap), Some(c)) = (tap, current) {
        ctx.send(Action::Edit(EditAction::SetSidechain {
            device: card.device,
            sidechain: Some(Sidechain { tap, ..c }),
        }));
    }
    if let Some(source) = chosen {
        let sidechain = source.map(|s| match current {
            // Keep the tap when the source changes.
            Some(c) => Sidechain { source: s, ..c },
            None => Sidechain::new(s),
        });
        ctx.send(Action::Edit(EditAction::SetSidechain {
            device: card.device,
            sidechain,
        }));
        close = true;
    }
    if !close && pressed_outside(&egui_ctx, &[area.response.rect, chip.rect]) {
        close = true;
    }
    if close {
        floating::close(&egui_ctx, id);
        egui_ctx.data_mut(|d| d.remove::<Open>(id));
        if chosen.is_none() {
            chip.request_focus();
        }
    } else {
        egui_ctx.data_mut(|d| d.insert_temp(id, st));
    }
}

/// Keyboard handling; returns `true` if the popover is to close.
fn keys(
    ctx: &Context,
    rows: &[Source],
    st: &mut Open,
    chosen: &mut Option<Option<TrackId>>,
) -> bool {
    let pressed = |key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key));
    let (down, up) = (pressed(Key::ArrowDown), pressed(Key::ArrowUp));
    let accept = pressed(Key::Enter) | pressed(Key::Space);
    if down || up {
        let flags = vec![true; rows.len()];
        let dir = if down { 1 } else { -1 };
        st.highlight = w::step_enabled(&flags, st.highlight, dir).or(st.highlight);
    }
    if accept
        && let Some(s) = st.highlight.and_then(|h| rows.get(h))
        && s.blocked.is_none()
    {
        *chosen = Some(s.id);
    }
    pressed(Key::Escape)
}

/// The separator between source groups: 1 px `line.2`, inset, 4 px above and below.
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

/// One source row: ✓ column, the track dot, the name. Blocked rows are grey with the reason as
/// tooltip and as accessible description.
fn source_row(ui: &mut Ui, s: &Source, checked: bool, highlighted: bool) -> Response {
    let enabled = s.blocked.is_none();
    let (rect, r) = ui.allocate_exact_size(
        vec2(size::MENU_WIDTH, size::MENU_ROW),
        w::sense(ui, Sense::click()),
    );
    let name = match &s.blocked {
        Some(_) => names::sc_blocked(&s.label),
        None => s.label.clone(),
    };
    r.widget_info(|| WidgetInfo::selected(Role::MenuItemRadio, enabled, checked, &name));
    if let Some(reason) = &s.blocked {
        Tip::new(reason).attach(&r);
    }
    let look = row_look(
        highlighted && enabled || (r.hovered() && enabled),
        false,
        enabled,
    );
    if let Some(fill) = look.fill {
        px::fill(ui.painter(), rect, radius::S, fill);
    } else if highlighted {
        // A greyed row the keys are on: a quiet surface instead of the accent.
        px::fill(ui.painter(), rect, radius::S, color32(color::SURFACE_4));
    }
    let ppp = ui.pixels_per_point();
    let check_left = rect.left() + space::SPACE_1;
    if checked {
        w::paint_check(ui, rect, check_left, look.check);
    }
    let mut x = check_left + komponist_ui::widgets::metrics::CHECK_COLUMN + space::SPACE_1;
    if let Some(hue) = s.hue {
        let dot = pos2(x + metrics::SC_DOT / 2.0, rect.center().y);
        let c = if enabled {
            base_color(hue)
        } else {
            {
                let [r, g, b, _] = base_color(hue);
                [r, g, b, 100]
            }
        };
        ui.painter()
            .circle_filled(dot, metrics::SC_DOT / 2.0, color32(c));
        x += metrics::SC_DOT + metrics::HEAD_GAP;
    }
    let label = w::galley(
        ui,
        &s.label,
        fonts::font_id(&typography::UI),
        typography::UI.line_height,
    );
    ui.painter()
        .galley(w::text_pos(rect, &label, x, ppp), label, look.text);
    r
}

/// „Abgriff“ with the segmented „Vor Fader“ / „Nach Fader“ and its hint. Returns the tap the user
/// chose this pass. Without a source the segments are shown but cannot be used.
fn tap_section(ui: &mut Ui, current: Option<Sidechain>) -> Option<Tap> {
    let (label, _) = ui.allocate_exact_size(
        vec2(
            size::MENU_WIDTH,
            typography::VALUE_S.line_height + space::SPACE_1,
        ),
        Sense::hover(),
    );
    let title = w::galley(
        ui,
        names::SC_TAP,
        fonts::font_id(&typography::VALUE_S),
        typography::VALUE_S.line_height,
    );
    ui.painter().galley(
        pos2(
            label.left() + space::SPACE_1_5,
            label.top() + space::SPACE_1,
        ),
        title,
        color32(color::TEXT_3),
    );
    let mut index = match current.map_or(komponist_model::project::SIDECHAIN_TAP_DEFAULT, |c| c.tap)
    {
        Tap::Pre => 0,
        Tap::Post => 1,
    };
    let items = [Item::new(names::SC_PRE), Item::new(names::SC_POST)];
    let before = index;
    let enabled = current.is_some();
    let changed = ui
        .scope(|ui| {
            ui.set_width(size::MENU_WIDTH - 2.0 * space::SPACE_1_5);
            ui.add_enabled(
                enabled,
                Segmented::new(names::SC_TAP, &mut index, &items).equal_width(),
            )
            .changed()
        })
        .inner;
    let hint = if index == 0 {
        names::SC_PRE_HINT
    } else {
        names::SC_POST_HINT
    };
    // The hint wraps inside the menu width (the post-fader hint is longer than one line).
    let mut job = egui::text::LayoutJob::single_section(
        hint.to_owned(),
        egui::TextFormat {
            font_id: fonts::font_id(&typography::SMALL),
            line_height: Some(typography::SMALL.line_height),
            color: egui::Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    job.wrap.max_width = size::MENU_WIDTH - 2.0 * space::SPACE_1_5;
    let galley = ui.painter().layout_job(job);
    let (rect, _) = ui.allocate_exact_size(
        vec2(size::MENU_WIDTH, galley.size().y + 2.0 * space::SPACE_1),
        Sense::hover(),
    );
    ui.painter().galley(
        pos2(rect.left() + space::SPACE_1_5, rect.top() + space::SPACE_1),
        galley,
        color32(color::TEXT_3),
    );
    (changed && index != before).then_some(if index == 0 { Tap::Pre } else { Tap::Post })
}
