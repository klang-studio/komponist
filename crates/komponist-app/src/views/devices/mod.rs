//! The device chain (WP-44): the dock's "Geräte" tab for the selected track (`Screens.dc.html`
//! `card()`, `devicesChain()`, the `isDevices` markup, `dvDrop`; `ks-view.js` `devView`;
//! `Prototyp.dc.html` `dropDev`, `devDrag`, `openPlugin`).
//!
//! A header row (track colour, name, "5 Geräte · Signal läuft von links nach rechts", "+ Gerät")
//! above the cards, left to right in signal order, then the drop area. Each card ([`card`]) has
//! the bypass switch, name, format and open button in its head and the knobs in its body:
//! pisynth's 8 macros, or a generic grid of the plugin's first parameters with the plugin's value
//! text. States: Lädt (progress, passthrough), Abgestürzt (red edge, "Neu laden"), Fehlt (red
//! placeholder, state kept), bypassed (dim, knobs off), and the warning when the stored state did
//! not load.
//!
//! | Input | What happens (S05 notes) |
//! |---|---|
//! | switch | `SetDeviceBypass` |
//! | Öffnen / Parameter / Geöffnet | `OpenWindow` / `CloseWindow` |
//! | knob | `SetParam` (pisynth: also into the device state) |
//! | drag a card's head | reorder: a 2 px accent line marks the slot, the drop is `MoveDevice` |
//! | drop a browser plugin | effect: `Add` at the marked slot; instrument: replaces the instrument |
//! | Neu laden | `Reload` |
//! | click on the head / right-click | selects the device / Öffnen, Bypass, Neu laden, Entfernen |
//! | wheel | scrolls the row sideways |

mod card;
pub mod metrics;
pub mod model;

use egui::{Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::{ChannelRef, DeviceId, DeviceRole, PluginRef, Track, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, radius};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px, state};

use self::model::{Card, names};
use crate::actions::{
    Action, DropTarget, EditAction, PluginAction, SelectMode, SelectionChange, UiAction,
};
use crate::ui_state::BrowserSegment;
use crate::views::browser::model::selected_track;
use crate::views::late::LateTracker;
use crate::views::{View, ViewCtx, group_bounds};

/// The drag payload of a card being reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardDrag {
    /// The device.
    pub device: DeviceId,
    /// Its track.
    pub track: TrackId,
    /// Its index in the chain.
    pub from: usize,
}

/// What a drag over the chain would do.
#[derive(Debug, Clone, PartialEq)]
enum Drop {
    /// Insert line at this slot.
    Line(usize),
    /// Replace the instrument (ring around the first card).
    Instrument,
}

/// The device chain view. Its view state is [`crate::ui_state::DevicesView`] (scroll).
#[derive(Debug, Default)]
pub struct Devices {
    /// The "zu spät" state of the cards.
    late: LateTracker,
    /// The track and devices of the last frame: a card that appears is scrolled into view.
    seen: Option<(TrackId, Vec<DeviceId>)>,
}

impl Devices {
    /// The view.
    pub fn new() -> Devices {
        Devices::default()
    }
}

impl View for Devices {
    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        let project = std::sync::Arc::clone(ctx.project);
        let Some(track) =
            selected_track(&project, &ctx.ui.selection.tracks).and_then(|t| project.track(t))
        else {
            return;
        };
        let mut cards = model::cards(track, ctx.engine.host(), &ctx.ui.plugin_windows);
        for c in &mut cards {
            let fit = card::head_width(ui, c).min(metrics::CARD_WIDTH_EFFECT);
            c.width = c.width.max(fit);
        }
        let now = ui.input(|i| i.time);
        for c in &mut cards {
            let running = c.state == model::CardState::Ok;
            c.late = self.late.state(ctx.engine.host(), c.device, running, now);
        }
        let ids: Vec<DeviceId> = cards.iter().map(|c| c.device).collect();
        self.late.retain(&ids);
        let reveal = match &self.seen {
            Some((t, old)) if *t == track.id => ids.iter().find(|d| !old.contains(d)).copied(),
            _ => None,
        };
        self.seen = Some((track.id, ids));
        let rect = ui.max_rect();
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::HEADER_HEIGHT));
        header_row(ui, ctx, header, track, cards.len());
        let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        chain(ui, ctx, body, track, &cards, reveal);
    }
}

/// The header row: colour mark, track name, info, "+ Gerät" (opens the browser on Effekte).
fn header_row(ui: &mut Ui, ctx: &mut ViewCtx<'_>, rect: Rect, track: &Track, count: usize) {
    let ppp = ui.pixels_per_point();
    px::hline(
        ui.painter(),
        rect.x_range(),
        rect.bottom() - px::line_width(ppp),
        color32(color::SURFACE_0),
    );
    let mut x = rect.left() + metrics::HEADER_PAD_X;
    let mark = Rect::from_min_size(
        pos2(x, rect.center().y - metrics::MARK / 2.0),
        vec2(metrics::MARK, metrics::MARK),
    );
    px::fill(
        ui.painter(),
        mark,
        radius::S,
        color32(TrackPalette::new(track.hue.0).base),
    );
    x = mark.right() + metrics::HEADER_GAP;
    let style = typography::UI;
    let info = names::info(count);
    for (text, font, color) in [
        (
            track.name.as_str(),
            w::font_at(&style, wm::SEMIBOLD),
            color::TEXT_1,
        ),
        (info.as_str(), fonts::font_id(&style), color::TEXT_3),
    ] {
        let g = w::galley(ui, text, font, style.line_height);
        let pos = w::text_pos(rect, &g, x, ppp);
        let r = ui.allocate_rect(Rect::from_min_size(pos, g.size()), Sense::hover());
        r.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
        x = pos.x + g.size().x + metrics::HEADER_GAP;
        ui.painter().galley(pos, g, color32(color));
    }
    let right = Rect::from_min_max(
        pos2(x, rect.top()),
        pos2(rect.right() - metrics::HEADER_PAD_X, rect.bottom()),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(right)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    if row.add(Button::new(names::ADD)).clicked() {
        ctx.send(Action::Ui(UiAction::FocusBrowserSearch));
        ctx.send(Action::Ui(UiAction::SetBrowserSegment(
            BrowserSegment::Effects,
        )));
    }
}

/// The cards, the drop area, drag and drop, and the sideways scroll.
fn chain(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    body: Rect,
    track: &Track,
    cards: &[Card],
    reveal: Option<DeviceId>,
) {
    let pad = metrics::BODY_PAD;
    let gap = metrics::CARD_GAP;
    let content: f32 =
        2.0 * pad + cards.iter().map(|c| c.width + gap).sum::<f32>() + metrics::DROP_AREA_WIDTH;
    let max_scroll = (content - body.width()).max(0.0);
    let mut scroll = ctx.ui.devices.scroll_x.clamp(0.0, max_scroll);
    if let Some(device) = reveal {
        scroll = scroll_to_card(cards, device, body.width(), scroll).clamp(0.0, max_scroll);
    }
    ctx.ui.devices.scroll_x = scroll;

    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .id_salt(("komponist_device_chain", track.id.0))
            .accessibility_role(Role::Group)
            .accessibility_label(names::CHAIN),
    );
    cu.set_clip_rect(body.intersect(ui.clip_rect()));
    group_bounds(&cu, body);
    let top = body.top() + pad;
    let bottom = (body.bottom() - pad).max(top);
    let mut x = body.left() + pad - scroll;
    let mut spans = Vec::with_capacity(cards.len());
    for (i, card) in cards.iter().enumerate() {
        let rect = Rect::from_min_max(pos2(x, top), pos2(x + card.width, bottom));
        let selected = ctx.ui.selection.devices.contains(&card.device);
        let head = card::show(&mut cu, ctx, card, rect, selected);
        if head.clicked() {
            ctx.send(Action::Ui(UiAction::Select(SelectionChange::Devices {
                devices: vec![card.device],
                mode: SelectMode::Replace,
            })));
        }
        if !card.instrument {
            head.dnd_set_drag_payload(CardDrag {
                device: card.device,
                track: track.id,
                from: i,
            });
        }
        spans.push((rect.left(), rect.right()));
        x = rect.right() + gap;
    }
    let area = Rect::from_min_max(pos2(x, top), pos2(x + metrics::DROP_AREA_WIDTH, bottom));
    drop_area(&mut cu, area);

    if let Some(drop) = drop_over(&mut cu, ctx, body, track, cards, &spans) {
        match drop {
            Drop::Line(slot) => {
                let x = match spans.get(slot) {
                    Some((left, _)) => left - gap / 2.0,
                    None => spans
                        .last()
                        .map_or(body.left() + pad - gap / 2.0, |(_, r)| r + gap / 2.0),
                };
                let line = Rect::from_min_max(
                    pos2(x - metrics::INSERT_LINE / 2.0, top),
                    pos2(x + metrics::INSERT_LINE / 2.0, bottom),
                );
                px::fill(cu.painter(), line, 0.0, color32(color::ACCENT));
            }
            Drop::Instrument => {
                if let Some((l, r)) = spans.first() {
                    let rect = Rect::from_min_max(pos2(*l, top), pos2(*r, bottom));
                    state::paint_select_ring(cu.painter(), rect, radius::M);
                }
            }
        }
    }

    // The wheel scrolls the row sideways (vertical wheels too: the row has nothing to scroll
    // up and down).
    if max_scroll > 0.0 && ui.rect_contains_pointer(body) {
        let delta = ui.input(|i| i.smooth_scroll_delta);
        let d = delta.x + delta.y;
        if d != 0.0 {
            ctx.ui.devices.scroll_x = (scroll - d).clamp(0.0, max_scroll);
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
}

/// The scroll that brings the card of `device` into a body `width` wide (a newly added card),
/// starting from `scroll`: the least change that shows it whole between the paddings.
pub fn scroll_to_card(cards: &[Card], device: DeviceId, width: f32, scroll: f32) -> f32 {
    let pad = metrics::BODY_PAD;
    let mut left = pad;
    for c in cards {
        let right = left + c.width;
        if c.device == device {
            return if right - scroll > width - pad {
                right - (width - pad)
            } else if left - scroll < pad {
                left - pad
            } else {
                scroll
            };
        }
        left = right + metrics::CARD_GAP;
    }
    scroll
}

/// The drop area after the last card: a dashed `surface.4` frame with the hint.
fn drop_area(ui: &mut Ui, rect: Rect) {
    let ppp = ui.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    let painter = ui.painter();
    let stroke = egui::Stroke::new(px::line_width(ppp), color32(color::SURFACE_4));
    let [dash, gap] = metrics::DASH;
    let half = px::line_width(ppp) / 2.0;
    let (l, t, rr, b) = (
        r.left() + half,
        r.top() + half,
        r.right() - half,
        r.bottom() - half,
    );
    for side in [
        [pos2(l, t), pos2(rr, t)],
        [pos2(rr, t), pos2(rr, b)],
        [pos2(rr, b), pos2(l, b)],
        [pos2(l, b), pos2(l, t)],
    ] {
        painter.extend(egui::Shape::dashed_line(&side, stroke, dash, gap));
    }
    let style = typography::SMALL;
    let mut job = fonts::layout_job(names::DROP_HINT, &style, color32(color::TEXT_3));
    job.wrap.max_width = rect.width() - 2.0 * metrics::DROP_AREA_PAD;
    job.halign = egui::Align::Center;
    let g = ui.painter().layout_job(job);
    let pos = pos2(
        px::snap(rect.center().x, ppp),
        px::snap(rect.center().y - g.size().y / 2.0, ppp),
    );
    let label = Rect::from_center_size(rect.center(), g.size());
    let resp = ui.interact(
        label,
        ui.scope_id().with("komponist_devices_drop_hint"),
        Sense::hover(),
    );
    resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, names::DROP_HINT));
    ui.painter().galley(pos, g, color32(color::TEXT_3));
}

/// A card or a browser plugin dragged over the chain: where it would go; on release, the action.
fn drop_over(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    body: Rect,
    track: &Track,
    cards: &[Card],
    spans: &[(f32, f32)],
) -> Option<Drop> {
    let dnd = ui.ctx().clone();
    let pos = ui.input(|i| i.pointer.hover_pos())?;
    if !body.contains(pos) {
        return None;
    }
    let released = ui.input(|i| i.pointer.any_released());
    let slot = model::slot_at(spans, pos.x);
    let first = model::first_effect_slot(cards);
    if let Some(drag) = egui::DragAndDrop::payload::<CardDrag>(&dnd) {
        if drag.track != track.id {
            return None;
        }
        let to = model::move_target(drag.from, slot, first)?;
        if released {
            egui::DragAndDrop::clear_payload(&dnd);
            ctx.send(Action::Edit(EditAction::MoveDevice {
                device: drag.device,
                to_index: to,
            }));
        }
        return Some(Drop::Line(slot.max(first)));
    }
    let plugin = egui::DragAndDrop::payload::<PluginRef>(&dnd)?;
    let role = ctx.engine.host().role(&plugin)?;
    let (drop, index) = match role {
        DeviceRole::Instrument if first == 1 => (Drop::Instrument, 0),
        DeviceRole::Instrument => (Drop::Line(0), 0),
        DeviceRole::Effect => (Drop::Line(slot.max(first)), slot.max(first)),
    };
    if released {
        egui::DragAndDrop::clear_payload(&dnd);
        ctx.send(Action::Plugin(PluginAction::Add {
            plugin: PluginRef::clone(&plugin),
            target: DropTarget::Chain {
                channel: ChannelRef::Track(track.id),
                index,
            },
        }));
    }
    Some(drop)
}
