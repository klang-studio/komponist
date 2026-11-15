//! The add menu behind the chain's drop slot (owner request 2026-11-15; not in the handoff): a
//! small popup under the slot's click with a search field and the catalog's devices, so a device
//! can be added without the browser.
//!
//! | Input | What happens |
//! |---|---|
//! | click, Enter or Space on the slot | opens the menu at the slot's top left, the search field focused |
//! | typing | filters with the browser's matching (`browser::model::picker_rows`) |
//! | ↑ ↓ | move the highlighted device (the first hit is highlighted while typing) |
//! | Enter, click on a device | adds it, same rule and same action as dropping it at the end of the chain ([`model::place_plugin`]), one undo step |
//! | Esc, click outside | closes without a change |
//!
//! The list follows the browser's grouping: Instrumente, Effekte under their category sub-headers,
//! MIDI-Effekte; the master chain lists audio effects only. The popup is the dropdown's list
//! (`menu_frame`, rows `size.menu-row`, the highlighted row `accent`), kept inside the window by
//! `Area::constrain`; a long list scrolls.

use std::collections::BTreeSet;

use egui::{
    Area, Key, Modifiers, Order, Rect, Response, Role, ScrollArea, Sense, Ui, UiKind, WidgetInfo,
    pos2, vec2,
};
use komponist_model::{ChannelRef, PluginRef};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{color, radius, size, space};
use komponist_ui::widgets::dropdown::row_look;
use komponist_ui::widgets::text_field::TextField;
use komponist_ui::widgets::{self as w, floating};
use komponist_ui::{fonts, px};

use super::metrics;
use super::model::{self, Card, names};
use crate::actions::{Action, DropTarget, PluginAction};
use crate::ui_state::BrowserSegment;
use crate::views::ViewCtx;
use crate::views::browser::model::{Input, Row, RowKind, picker_rows};
use crate::views::format_tag;

/// Where the menu adds to.
pub struct Target<'a> {
    /// The channel.
    pub channel: ChannelRef,
    /// Its devices.
    pub cards: &'a [Card],
}

impl Target<'_> {
    fn master(&self) -> bool {
        self.channel == ChannelRef::Master
    }
}

/// The open menu's state between frames.
#[derive(Clone, Debug, Default)]
struct Memory {
    /// The search text.
    query: String,
    /// Index among the listed devices.
    highlight: usize,
    /// Where the popup was drawn last frame (for the click outside).
    rect: Option<Rect>,
}

fn rows(ctx: &ViewCtx<'_>, query: &str, effects_only: bool) -> Vec<Row> {
    let no_exports: [String; 0] = [];
    picker_rows(
        &Input {
            catalog: ctx.engine.host().catalog(),
            favorites: &ctx.ui.browser.favorites,
            segment: BrowserSegment::All,
            query,
            collapsed: &BTreeSet::new(),
            project: ctx.project,
            exports: &no_exports,
        },
        effects_only,
    )
}

fn plugins(rows: &[Row]) -> Vec<PluginRef> {
    rows.iter()
        .filter_map(|r| match &r.kind {
            RowKind::Plugin { plugin, .. } => Some(plugin.clone()),
            _ => None,
        })
        .collect()
}

/// Sends the add of `plugin` at the end of the chain, the way dropping it there would.
fn add(ctx: &mut ViewCtx<'_>, target: &Target<'_>, plugin: PluginRef) {
    let Some(role) = ctx.engine.host().role(&plugin) else {
        return;
    };
    let (midi, first) = (
        model::midi_slots(target.cards),
        model::first_effect_slot(target.cards),
    );
    let Some(place) = model::place_plugin(role, target.master(), target.cards.len(), first, midi)
    else {
        return;
    };
    ctx.send(Action::Plugin(PluginAction::Add {
        plugin,
        target: DropTarget::Chain {
            channel: target.channel,
            index: place.index(),
        },
    }));
}

/// Opens, draws and handles the menu of `trigger` (the drop slot); call it every frame.
pub fn show(ui: &mut Ui, ctx: &mut ViewCtx<'_>, trigger: &Response, target: &Target<'_>) {
    let ectx = ui.ctx().clone();
    let id = trigger.id.with("add_menu");
    let was_open = floating::is_open(&ectx, id);
    let mut mem: Memory = if was_open {
        ui.data(|d| d.get_temp(id)).unwrap_or_default()
    } else {
        Memory::default()
    };
    let mut open = was_open;
    let mut choose: Option<PluginRef> = None;
    let mut by_key = false;
    let mut refocus = false;

    if was_open {
        let (down, up, enter, escape) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        let listed = plugins(&rows(ctx, &mem.query, target.master()));
        if down && !listed.is_empty() {
            mem.highlight = (mem.highlight + 1).min(listed.len() - 1);
            by_key = true;
        }
        if up {
            mem.highlight = mem.highlight.saturating_sub(1);
            by_key = true;
        }
        if enter {
            choose = listed.get(mem.highlight).cloned();
            open = false;
            refocus = true;
        }
        if escape {
            open = false;
            refocus = true;
        }
    }
    if trigger.clicked() {
        open = !was_open;
    }
    if open
        && was_open
        && w::pressed_outside(&ectx, &[mem.rect.unwrap_or(Rect::NOTHING), trigger.rect])
    {
        open = false;
    }

    if open {
        floating::open(&ectx, id);
        draw(ui, ctx, trigger, target, id, &mut mem, &mut choose, by_key);
        if choose.is_some() {
            open = false;
            refocus = true;
        }
    }
    if open {
        ui.data_mut(|d| d.insert_temp(id, mem));
    } else {
        floating::close(&ectx, id);
        ui.data_mut(|d| d.remove_temp::<Memory>(id));
    }
    if refocus {
        trigger.request_focus();
    }
    if let Some(plugin) = choose {
        add(ctx, target, plugin);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw(
    ui: &Ui,
    ctx: &ViewCtx<'_>,
    trigger: &Response,
    target: &Target<'_>,
    id: egui::Id,
    mem: &mut Memory,
    choose: &mut Option<PluginRef>,
    by_key: bool,
) {
    let ectx = ui.ctx().clone();
    let width = metrics::ADD_MENU_WIDTH.max(trigger.rect.width());
    let max_h = metrics::ADD_MENU_MAX_HEIGHT
        .min(ectx.content_rect().height() - 2.0 * metrics::ADD_MENU_MARGIN);
    let list_h = max_h - 2.0 * space::SPACE_1 - size::CONTROL - metrics::ADD_MENU_GAP;
    let field_id = id.with("search");
    let out = Area::new(id)
        .order(Order::Foreground)
        .kind(UiKind::Popup)
        .fixed_pos(trigger.rect.left_top())
        .constrain(true)
        .accessibility_parent(trigger.id)
        .accessible_name(names::ADD_DEVICE)
        .role(Role::Menu)
        .show(&ectx, |ui| {
            w::menu_frame().show(ui, |ui| {
                ui.set_width(width - 2.0 * space::SPACE_1);
                ui.spacing_mut().item_spacing.y = metrics::ADD_MENU_GAP;
                // The field keeps the keyboard while the menu is open.
                ui.ctx().memory_mut(|m| m.request_focus(field_id));
                let before = mem.query.clone();
                ui.add(
                    TextField::new(&mut mem.query, names::ADD_SEARCH)
                        .hint(names::ADD_SEARCH)
                        .id(field_id),
                );
                let listed = rows(ctx, &mem.query, target.master());
                let count = plugins(&listed).len();
                if mem.query != before {
                    mem.highlight = 0;
                }
                mem.highlight = mem.highlight.min(count.saturating_sub(1));
                ScrollArea::vertical()
                    .max_height(list_h)
                    .auto_shrink([false, true])
                    .show(ui, |ui| list(ui, &listed, mem, choose, by_key));
            });
        });
    mem.rect = Some(out.response.rect);
}

fn list(ui: &mut Ui, rows: &[Row], mem: &mut Memory, choose: &mut Option<PluginRef>, by_key: bool) {
    let ppp = ui.pixels_per_point();
    let width = ui.available_width();
    ui.spacing_mut().item_spacing.y = 0.0;
    if !rows
        .iter()
        .any(|r| matches!(r.kind, RowKind::Plugin { .. }))
    {
        let (rect, r) = ui.allocate_exact_size(vec2(width, size::MENU_ROW), Sense::hover());
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, names::ADD_NO_MATCH));
        let style = typography::UI;
        let g = w::galley(
            ui,
            names::ADD_NO_MATCH,
            fonts::font_id(&style),
            style.line_height,
        );
        let pos = w::text_pos(rect, &g, rect.left() + space::SPACE_2, ppp);
        ui.painter().galley(pos, g, color32(color::TEXT_3));
        return;
    }
    let moved = ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_pressed());
    let mut index = 0;
    for row in rows {
        let sense = if matches!(row.kind, RowKind::Plugin { .. }) {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, r) = ui.allocate_exact_size(vec2(width, size::MENU_ROW), sense);
        let left = rect.left() + space::SPACE_2;
        match &row.kind {
            RowKind::Plugin { plugin, .. } => {
                let mine = index;
                index += 1;
                if r.hovered() && moved {
                    mem.highlight = mine;
                }
                let highlighted = mem.highlight == mine;
                r.widget_info(|| {
                    WidgetInfo::selected(Role::ListBoxOption, true, highlighted, &row.name)
                });
                w::accesskit(&r, |n| {
                    n.set_selected(highlighted);
                    if !row.tag.is_empty() {
                        n.set_description(row.tag.as_str());
                    }
                });
                if r.clicked() {
                    *choose = Some(plugin.clone());
                }
                if by_key && highlighted {
                    r.scroll_to_me(None);
                }
                let look = row_look(highlighted, true);
                if let Some(fill) = look.fill {
                    px::fill(ui.painter(), rect, radius::S, fill);
                }
                let style = typography::UI;
                let g = w::galley(ui, &row.name, fonts::font_id(&style), style.line_height);
                let pos = w::text_pos(rect, &g, left, ppp);
                ui.painter().galley(pos, g, look.text);
                let tag = format_tag(plugin.format);
                let small = typography::SMALL;
                let g = w::galley(ui, tag, fonts::font_id(&small), small.line_height);
                let x = rect.right() - space::SPACE_2 - g.size().x;
                let ink = if highlighted {
                    look.text
                } else {
                    color32(color::TEXT_3)
                };
                ui.painter().galley(w::text_pos(rect, &g, x, ppp), g, ink);
            }
            RowKind::Section => {
                let g = w::styled_galley(ui, &row.name, &typography::LABEL);
                let y = rect.bottom() - space::SPACE_1 - g.size().y;
                let pos = pos2(px::snap(left, ppp), px::snap(y, ppp));
                ui.painter().galley(pos, g, color32(color::TEXT_3));
            }
            _ => {
                let small = typography::SMALL;
                let g = w::galley(ui, &row.name, fonts::font_id(&small), small.line_height);
                let pos = w::text_pos(rect, &g, left, ppp);
                ui.painter().galley(pos, g, color32(color::TEXT_3));
            }
        }
    }
}
