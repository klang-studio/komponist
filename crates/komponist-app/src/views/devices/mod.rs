//! The device chain (WP-44, frames and chain WP-82): the dock's "Geräte" tab for the selected track
//! (device handoff §3 and §9; `Screens.dc.html` `devicesChain()`, `dvDrop`; `ks-view.js` `devView`).
//!
//! A chain head (track colour, track name, the devices as `a › b › c` with the visible ones on
//! `surface.3`, „+ Gerät“) above the frames ([`frame`]), left to right in signal order, then the
//! drop slot. Each frame is 244 high on a 56 × 64 grid ([`layout`]); foreign plugins get the
//! auto-layout in the same frame. States: Lädt (progress, passthrough), Abgestürzt (red edge,
//! "Neu laden"), Fehlt (red placeholder, state kept), bypassed (the body desaturated, half
//! opacity, still operable), collapsed (28 px strip), selected (accent outline), and the warning
//! when the stored state did not load.
//!
//! | Input | What happens |
//! |---|---|
//! | on/off square | `SetDeviceBypass` |
//! | Öffnen / Parameter / Geöffnet | `OpenWindow` / `CloseWindow` |
//! | page switch | `SetDevicePage` |
//! | preset button (pisynth, every internal device with presets; it replaces the tag and „Öffnen“, which stay in the ⋯ menu) | menu of the factory presets: `ApplyPisynthPreset` / `ApplyInternalPreset` (one undo step; the loaded name is saved in the device state and „ *“ follows a changed value) |
//! | ⋯ / right-click on the head | Öffnen, Bypass, Neu laden, Einklappen, Duplizieren, Entfernen |
//! | knob | `SetParam` (pisynth: also into the device state) |
//! | touching a parameter (press, drag, click) | `Ui(TouchParam)`: the inspector's „Parameter“ section and, while dragging, the status line show it |
//! | right-click on a parameter | „Auf Standard“ (the reset) and „Wert eingeben“ (text entry; not for choices and switches); no automation, MIDI learn or macro entries (prepared, not built) |
//! | click on the head | selects the device (Strg C / V / D, Entf, ← → act on it) |
//! | double-click on the head | collapses or expands; with Alt every device of the chain |
//! | click on a collapsed strip | expands it |
//! | drag a head | reorder: a 2 px accent line marks the slot, the drop is `MoveDevice`; with Alt a copy (`CopyDevice`), also onto another track in the arrangement (the instrument only with Alt, onto a track without one) |
//! | Strg C / Strg V | copy the selected devices / paste behind the selected device; both arrive as `Event::Copy` / `Event::Paste` from egui-winit (it swallows the key press), so the view reads them ([`clipboard_events`]); the keymap keeps the keys for kittest |
//! | drop a browser plugin | effect: `Add` at the marked slot; MIDI effect: `Add` into the MIDI part left of the instrument; instrument: replaces the instrument |
//! | click on a name in the chain head | scrolls to the device |
//! | wheel over the chain head, Shift-wheel in the body | scrolls the chain sideways |
//! | dock maximised | the frames stay 244 high at the top of the chain area; the space below stays empty and reserved for the automation of a later wave |
//! | wheel in the body | scrolls up and down while the dock is lower than the chain (< 292 px area) |
//! | Neu laden | `Reload` |

mod cells;
pub(crate) use cells::{select_pad, selected_pad};
pub mod frame;
pub mod layout;
pub mod metrics;
pub mod model;

use egui::{Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::{ChannelRef, DeviceId, DeviceRole, PluginRef, Track, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{TrackPalette, color, device, radius, size};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::{self as w, metrics as wm};
use komponist_ui::{fonts, px, state};

use self::frame::Placed;
use self::model::{Card, names};
use crate::actions::{
    Action, DropTarget, EditAction, PluginAction, SelectMode, SelectionChange, UiAction,
};
use crate::ui_state::{BrowserSegment, Reveal};
use crate::views::browser::model::selected_track;
use crate::views::late::LateTracker;
use crate::views::{View, ViewCtx, group_bounds};

/// The drag payload of a frame being reordered or copied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CardDrag {
    /// The device.
    pub device: DeviceId,
    /// Its track.
    pub track: TrackId,
    /// Its index in the chain.
    pub from: usize,
    /// Alt was held when the drag began: the drop copies.
    pub copy: bool,
}

/// What a drag over the chain would do.
#[derive(Debug, Clone, PartialEq)]
enum Drop {
    /// Insert line at this slot.
    Line(usize),
    /// Replace the instrument (ring around its card, at this index).
    Instrument(usize),
}

/// The device chain view. Its view state is [`crate::ui_state::DevicesView`] (scroll, collapsed,
/// pages).
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
            c.page = ctx.ui.devices.page(c.device);
            c.collapsed = ctx.ui.devices.collapsed.contains(&c.device);
            let fit = frame::head_width(ui, c);
            c.width = c.frame_width(fit);
        }
        let now = ui.input(|i| i.time);
        for c in &mut cards {
            let running = c.state == model::CardState::Ok;
            c.late = self.late.state(ctx.engine.host(), c.device, running, now);
            c.levels = ctx.meters.device(c.device);
        }
        let ids: Vec<DeviceId> = cards.iter().map(|c| c.device).collect();
        self.late.retain(&ids);
        let new_card = match &self.seen {
            Some((t, old)) if *t == track.id => ids.iter().find(|d| !old.contains(d)).copied(),
            _ => None,
        };
        self.seen = Some((track.id, ids));

        clipboard_events(ui, ctx);
        let rect = ui.max_rect();
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), metrics::HEADER_HEIGHT));
        let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);

        // Geometry and scroll.
        let widths: Vec<f32> = cards.iter().map(|c| c.width).collect();
        let spans = model::spans(&widths, metrics::BODY_PAD, metrics::CARD_GAP);
        let content = model::content_width(
            &spans,
            metrics::BODY_PAD,
            metrics::CARD_GAP,
            metrics::DROP_AREA_WIDTH,
        )
        .max(0.0);
        let max_x = (content - body.width()).max(0.0);
        let mut scroll_x = ctx.ui.devices.scroll_x.clamp(0.0, max_x);
        let request = ctx
            .ui
            .devices
            .reveal
            .take()
            .or_else(|| new_card.map(|d| (d, Reveal::Visible)));
        if let Some((device, mode)) = request
            && let Some(i) = cards.iter().position(|c| c.device == device)
        {
            scroll_x =
                model::reveal_scroll(spans[i], mode, body.width(), scroll_x, metrics::BODY_PAD)
                    .clamp(0.0, max_x);
        }
        let vertical = model::scrolls_vertically(rect.height());
        let content_h = 2.0 * metrics::BODY_PAD + device::frame::TOTAL_H;
        let max_y = if vertical {
            (content_h - body.height()).max(0.0)
        } else {
            0.0
        };
        let scroll_y = ctx.ui.devices.scroll_y.clamp(0.0, max_y);

        // The wheel: over the head sideways, in the body sideways with Shift (or a horizontal
        // wheel) and up and down while the chain scrolls vertically. Over a knob, fader or number
        // field the control takes it (Shift: fine adjust), so the body leaves it alone there.
        let (mut scroll_x, mut scroll_y) = (scroll_x, scroll_y);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if let Some(pos) = pointer {
            let delta = ui.input(|i| i.smooth_scroll_delta);
            if header.contains(pos) && max_x > 0.0 && delta != egui::Vec2::ZERO {
                scroll_x = (scroll_x - (delta.x + delta.y)).clamp(0.0, max_x);
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            } else if body.contains(pos)
                && !komponist_ui::widgets::number_field::wheel_over_control(ui.ctx())
            {
                let mut used = egui::Vec2::ZERO;
                if max_x > 0.0 && delta.x != 0.0 {
                    scroll_x = (scroll_x - delta.x).clamp(0.0, max_x);
                    used.x = delta.x;
                }
                if max_y > 0.0 && delta.y != 0.0 {
                    scroll_y = (scroll_y - delta.y).clamp(0.0, max_y);
                    used.y = delta.y;
                }
                if used != egui::Vec2::ZERO {
                    ui.input_mut(|i| i.smooth_scroll_delta -= used);
                }
            }
        }

        let visible = model::visible(&spans, scroll_x, body.width());
        // The live spectrum of the visible frames (WP-92); hidden ones cost nothing.
        for (c, on_screen) in cards.iter_mut().zip(&visible) {
            let show = *on_screen && model::shows_spectrum(c);
            c.analysis = ctx.meters.analysis(c.device, show, now);
            // The compressor's level point and reduction history likewise.
            let live = *on_screen && model::shows_dynamics(c);
            if model::has_dynamics_graph(c) {
                c.dynamics = ctx.meters.dynamics(c.device, live, now);
            }
        }
        header_row(ui, ctx, header, track, &cards, &visible);
        let view = ChainView {
            body,
            scroll_x,
            scroll_y,
            max_x,
            max_y,
            content,
            content_h,
            spans,
        };
        let (sx, sy) = chain(ui, ctx, &view, track, &cards);
        ctx.ui.devices.scroll_x = sx;
        ctx.ui.devices.scroll_y = sy;
    }
}

/// Strg C and Strg V as the desktop delivers them: egui-winit turns the key press into
/// `Event::Copy` / `Event::Paste(text)` and sends no key event, so the keymap never sees it (kittest
/// sends the keys, which the keymap handles). With the chain focused and no text field holding the
/// keyboard, a copy event copies the selected devices and a paste event pastes them. Paste only
/// arrives while the system clipboard holds text.
fn clipboard_events(ui: &Ui, ctx: &mut ViewCtx<'_>) {
    if !ctx.focused || ui.memory(|m| m.focused().is_some()) {
        return;
    }
    let (copy, paste) = ui.input(|i| {
        (
            i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
            i.events.iter().any(|e| matches!(e, egui::Event::Paste(_))),
        )
    });
    if copy {
        ctx.send(Action::Ui(UiAction::CopyDevices));
    }
    if paste {
        ctx.send(Action::Edit(EditAction::PasteDevices));
    }
}

/// The chain head: colour mark, track name, the devices as `a › b › c` (visible ones on
/// `surface.3`, a click scrolls there), „+ Gerät“ (opens the browser on Effekte).
fn header_row(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    rect: Rect,
    track: &Track,
    cards: &[Card],
    visible: &[bool],
) {
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
    let g = w::galley(
        ui,
        &track.name,
        w::font_at(&style, wm::SEMIBOLD),
        style.line_height,
    );
    let pos = w::text_pos(rect, &g, x, ppp);
    let r = ui.allocate_rect(Rect::from_min_size(pos, g.size()), Sense::hover());
    r.widget_info(|| WidgetInfo::labeled(Role::Label, true, &track.name));
    x = pos.x + g.size().x + metrics::HEADER_GAP;
    ui.painter().galley(pos, g, color32(color::TEXT_1));

    let button_width = {
        let g = w::galley(
            ui,
            names::ADD,
            fonts::emphasis_font_id(&style),
            style.line_height,
        );
        g.size().x.ceil() + 2.0 * wm::BUTTON_PADDING_X
    };
    let right_edge = rect.right() - metrics::HEADER_PAD_X - button_width;
    let right = Rect::from_min_max(
        pos2(right_edge, rect.top()),
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

    // The chain, cut at the button.
    let chain_area = Rect::from_min_max(
        pos2(x, rect.top()),
        pos2(right_edge - metrics::HEADER_GAP, rect.bottom()),
    );
    let mut cu = ui.new_child(UiBuilder::new().max_rect(chain_area));
    cu.set_clip_rect(chain_area.intersect(ui.clip_rect()));
    let small = typography::UI;
    let sep = w::galley(
        &cu,
        metrics::CHAIN_SEPARATOR,
        fonts::font_id(&small),
        small.line_height,
    );
    for (i, card) in cards.iter().enumerate() {
        if i > 0 {
            x += metrics::CHAIN_SEPARATOR_GAP;
            let pos = w::text_pos(rect, &sep, x, ppp);
            cu.painter()
                .galley(pos, sep.clone(), color32(color::TEXT_3));
            x += sep.size().x + metrics::CHAIN_SEPARATOR_GAP;
        }
        let g = w::galley(&cu, &card.name, fonts::font_id(&small), small.line_height);
        let chip = Rect::from_min_size(
            pos2(x, rect.center().y - size::CONTROL_COMPACT / 2.0),
            vec2(
                g.size().x + 2.0 * metrics::CHIP_PAD_X,
                size::CONTROL_COMPACT,
            ),
        );
        let r = cu.interact(
            chip.intersect(chain_area),
            cu.scope_id().with(("chain", card.device.0)),
            Sense::click(),
        );
        r.widget_info(|| WidgetInfo::labeled(Role::Button, true, names::reveal(&card.name)));
        let shown = visible.get(i).copied().unwrap_or(false);
        if shown {
            px::fill(cu.painter(), chip, radius::S, color32(color::SURFACE_3));
        } else if r.hovered() {
            px::fill(cu.painter(), chip, radius::S, color32(color::SURFACE_2));
        }
        let ink = if shown { color::TEXT_1 } else { color::TEXT_2 };
        let pos = w::text_pos(chip, &g, chip.left() + metrics::CHIP_PAD_X, ppp);
        cu.painter().galley(pos, g, color32(ink));
        if r.clicked() {
            ctx.send(Action::Ui(UiAction::ScrollToDevice(card.device)));
        }
        x = chip.right();
    }
}

/// The chain's geometry for one frame.
struct ChainView {
    /// The area under the chain head.
    body: Rect,
    scroll_x: f32,
    scroll_y: f32,
    max_x: f32,
    max_y: f32,
    /// Content size.
    content: f32,
    content_h: f32,
    /// `(left, right)` of every frame in the content.
    spans: Vec<(f32, f32)>,
}

/// The frames, the drop slot, drag and drop and the scrollbars. Returns the scroll offsets after
/// dragging a scrollbar.
fn chain(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    view: &ChainView,
    track: &Track,
    cards: &[Card],
) -> (f32, f32) {
    let body = view.body;
    let pad = metrics::BODY_PAD;
    let gap = metrics::CARD_GAP;
    let mut cu = ui.new_child(
        UiBuilder::new()
            .max_rect(body)
            .id_salt(("komponist_device_chain", track.id.0))
            .accessibility_role(Role::Group)
            .accessibility_label(names::CHAIN),
    );
    cu.set_clip_rect(body.intersect(ui.clip_rect()));
    group_bounds(&cu, body);
    let top = body.top() + pad - view.scroll_y;
    let bottom = top + device::frame::TOTAL_H;
    let origin = body.left() - view.scroll_x;
    let alt = ui.input(|i| i.modifiers.alt);
    let mut spans = Vec::with_capacity(cards.len());
    for (i, card) in cards.iter().enumerate() {
        let (l, r) = view.spans[i];
        let rect = Rect::from_min_max(pos2(origin + l, top), pos2(origin + r, bottom));
        let selected = ctx.ui.selection.devices.contains(&card.device);
        let at = Placed {
            track,
            index: i,
            selected,
        };
        let head = frame::show(&mut cu, ctx, card, rect, &at);
        if head.clicked() {
            ctx.send(Action::Ui(UiAction::Select(SelectionChange::Devices {
                devices: vec![card.device],
                mode: SelectMode::Replace,
            })));
            if card.collapsed {
                ctx.send(Action::Ui(UiAction::ToggleDeviceCollapsed {
                    device: card.device,
                    all: false,
                }));
            }
        }
        if head.double_clicked() && !card.collapsed {
            ctx.send(Action::Ui(UiAction::ToggleDeviceCollapsed {
                device: card.device,
                all: alt,
            }));
        }
        // The instrument stays where it is; with Alt it can still be copied (onto another track).
        if !card.instrument || alt {
            head.dnd_set_drag_payload(CardDrag {
                device: card.device,
                track: track.id,
                from: i,
                copy: alt,
            });
        }
        spans.push((rect.left(), rect.right()));
    }
    let x = spans.last().map_or(origin + pad, |(_, r)| r + gap);
    let area = Rect::from_min_max(pos2(x, top), pos2(x + metrics::DROP_AREA_WIDTH, bottom));
    drop_area(&mut cu, area);

    if let Some(drop) = drop_over(&mut cu, ctx, body, track, cards, &spans) {
        match drop {
            Drop::Line(slot) => {
                let x = match spans.get(slot) {
                    Some((left, _)) => left - gap / 2.0,
                    None => spans
                        .last()
                        .map_or(origin + pad - gap / 2.0, |(_, r)| r + gap / 2.0),
                };
                let line = Rect::from_min_max(
                    pos2(x - metrics::INSERT_LINE / 2.0, top),
                    pos2(x + metrics::INSERT_LINE / 2.0, bottom),
                );
                px::fill(cu.painter(), line, 0.0, color32(color::ACCENT));
            }
            Drop::Instrument(index) => {
                if let Some((l, r)) = spans.get(index) {
                    let rect = Rect::from_min_max(pos2(*l, top), pos2(*r, bottom));
                    state::paint_select_ring(cu.painter(), rect, radius::M);
                }
            }
        }
    }

    let sx = scrollbar(
        &mut cu,
        body,
        false,
        view.content,
        view.scroll_x,
        view.max_x,
    );
    let sy = scrollbar(
        &mut cu,
        body,
        true,
        view.content_h,
        view.scroll_y,
        view.max_y,
    );
    (sx, sy)
}

/// A 4 px scrollbar along the bottom (or right) edge of `body`, shown while there is something
/// to scroll: a `surface.4` thumb, `surface.5` while dragged. Returns the scroll after dragging.
fn scrollbar(ui: &mut Ui, body: Rect, vertical: bool, content: f32, scroll: f32, max: f32) -> f32 {
    if max <= 0.0 {
        return scroll;
    }
    let ppp = ui.pixels_per_point();
    let thickness = metrics::SCROLLBAR;
    let view = if vertical {
        body.height()
    } else {
        body.width()
    };
    let thumb_len = (view * view / content).clamp(thickness * 4.0, view);
    let travel = view - thumb_len;
    let at = if max > 0.0 {
        scroll / max * travel
    } else {
        0.0
    };
    let thumb = if vertical {
        Rect::from_min_size(
            pos2(body.right() - thickness, body.top() + at),
            vec2(thickness, thumb_len),
        )
    } else {
        Rect::from_min_size(
            pos2(body.left() + at, body.bottom() - thickness),
            vec2(thumb_len, thickness),
        )
    };
    let id = ui.scope_id().with(("komponist_device_scrollbar", vertical));
    let r = ui.interact(thumb, id, Sense::drag());
    r.widget_info(|| {
        WidgetInfo::labeled(
            Role::ScrollBar,
            true,
            if vertical {
                names::SCROLL_V
            } else {
                names::SCROLL_H
            },
        )
    });
    let mut scroll = scroll;
    if r.dragged() && travel > 0.0 {
        let d = if vertical {
            r.drag_delta().y
        } else {
            r.drag_delta().x
        };
        scroll = (scroll + d / travel * max).clamp(0.0, max);
    }
    let fill = if r.dragged() {
        color::SURFACE_5
    } else {
        color::SURFACE_4
    };
    let _ = ppp;
    px::fill(ui.painter(), thumb, thickness / 2.0, color32(fill));
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
    let midi = model::midi_slots(cards);
    if let Some(drag) = egui::DragAndDrop::payload::<CardDrag>(&dnd) {
        if drag.track != track.id || cards.get(drag.from).is_some_and(|c| c.instrument) {
            return None;
        }
        // A MIDI effect goes into the MIDI part, any other effect behind the instrument.
        let (lo, hi) = model::drop_bounds(cards, drag.from);
        let at = slot.clamp(lo, hi.max(lo));
        if drag.copy {
            if released {
                egui::DragAndDrop::clear_payload(&dnd);
                ctx.send(Action::Edit(EditAction::CopyDevice {
                    device: drag.device,
                    to: ChannelRef::Track(track.id),
                    index: at,
                }));
            }
            return Some(Drop::Line(at));
        }
        let to = model::move_target_in(drag.from, slot, lo, hi)?;
        if released {
            egui::DragAndDrop::clear_payload(&dnd);
            ctx.send(Action::Edit(EditAction::MoveDevice {
                device: drag.device,
                to_index: to,
            }));
        }
        return Some(Drop::Line(at));
    }
    let plugin = egui::DragAndDrop::payload::<PluginRef>(&dnd)?;
    let role = ctx.engine.host().role(&plugin)?;
    let (drop, index) = match role {
        DeviceRole::Instrument if first > midi => (Drop::Instrument(midi), midi),
        DeviceRole::Instrument => (Drop::Line(midi), midi),
        DeviceRole::MidiEffect => (Drop::Line(slot.min(midi)), slot.min(midi)),
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
