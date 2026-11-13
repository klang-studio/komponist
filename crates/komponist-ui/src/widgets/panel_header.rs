//! Panel header (`Panel-Kopf`: `h 28 · fläche-2 · px 8 · Titel label`).
//!
//! Caret ▾ (open) or ▸ (collapsed), 8 px gap, the title in `type.label` (caps); on the right
//! the actions (open) or the shortcut hint in Mono 10 `text.4` (collapsed, "Strg 2"). Padding
//! 0 8, or 0 4 0 8 with actions; actions are frameless icon buttons 24 × 20.
//!
//! | | caret and title |
//! |---|---|
//! | open | text.2 |
//! | collapsed | text.3 |
//! | panel focused | text.1, plus a 1 px accent line at the top |
//!
//! The header follows the state rules: hover lifts the surface (surface.2 → surface.3) and turns
//! text.2 into text.1. A click on the header (not on an action), or Enter / Space when it has
//! keyboard focus, collapses or expands the panel. The panel focus ("fokussiertes Panel") is the
//! caller's: the line shows whenever `focused` is set, the keyboard ring only after keyboard
//! input.

use egui::{
    Align, Color32, CornerRadius, Layout, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo,
    vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Icon, color, radius, size, space};

use super::button::Button;
use super::metrics;
use crate::icons::{self, Caret};
use crate::state::{self, State};
use crate::{focus, fonts, px};

/// Colours of a panel header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderLook {
    /// Surface.
    pub fill: Color32,
    /// Caret and title.
    pub text: Color32,
    /// The shortcut hint.
    pub hint: Color32,
    /// The 1 px accent line at the top (focused panel).
    pub accent_line: bool,
    /// Draw the keyboard focus ring.
    pub focus_ring: bool,
}

/// The [`HeaderLook`] of a header, `open` or collapsed, of a `focused` panel or not, in `state`.
pub fn look(open: bool, focused: bool, state: State) -> HeaderLook {
    let base = if focused {
        color::TEXT_1
    } else if open {
        color::TEXT_2
    } else {
        color::TEXT_3
    };
    HeaderLook {
        fill: state::surface_for(2, state),
        text: state::text_for(base, state),
        hint: color32(color::TEXT_4),
        accent_line: focused,
        focus_ring: state.focused && !state.disabled,
    }
}

/// A panel header bound to the panel's `open` flag.
#[must_use = "call `.show(ui, actions)`"]
pub struct PanelHeader<'a> {
    title: &'a str,
    open: &'a mut bool,
    focused: bool,
    shortcut: Option<&'a str>,
    corner_radius: CornerRadius,
}

impl<'a> PanelHeader<'a> {
    /// The header of the panel `title` ("Browser"), collapsing and expanding `open`.
    pub fn new(title: &'a str, open: &'a mut bool) -> Self {
        Self {
            title,
            open,
            focused: false,
            shortcut: None,
            corner_radius: CornerRadius::ZERO,
        }
    }

    /// Marks the panel as the focused one (accent line, title text.1).
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// The shortcut that toggles the panel, shown when collapsed ("Strg 2").
    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// Rounded corners (the gallery's free-standing headers); panels in the app are square.
    pub fn corner_radius(mut self, corner_radius: CornerRadius) -> Self {
        self.corner_radius = corner_radius;
        self
    }

    /// Adds the header over the available width; `actions` adds the action buttons (right to
    /// left, shown only while open), e.g. with [`action`]. The response is the header's click
    /// target; it is `changed()` when the panel was collapsed or expanded.
    pub fn show(self, ui: &mut Ui, actions: impl FnOnce(&mut Ui)) -> Response {
        let width = ui.available_width();
        let (rect, mut response) = ui.allocate_exact_size(
            vec2(width, size::PANEL_HEADER),
            super::sense(ui, Sense::click()),
        );
        if response.clicked() {
            *self.open = !*self.open;
            response.mark_changed();
        }
        let open = *self.open;
        let st = State::of(ui, &response);
        let enabled = ui.is_enabled() && !st.disabled;
        let title = self.title;
        response.widget_info(|| WidgetInfo::labeled(Role::Button, enabled, title));
        super::accesskit(&response, |n| n.set_expanded(open));

        let l = look(open, self.focused, st);
        let ppp = ui.pixels_per_point();
        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            painter.rect_filled(px::snap_rect(rect, ppp), self.corner_radius, l.fill);
            if l.accent_line {
                px::hline(painter, rect.x_range(), rect.top(), color32(color::ACCENT));
            }
            // Caret and title.
            let [cw, ch] = metrics::CARET;
            let (caret, size) = if open {
                (Caret::Down, vec2(cw, ch))
            } else {
                (Caret::Right, vec2(ch, cw))
            };
            let left = rect.left() + space::SPACE_2;
            let caret_rect = px::snap_rect(
                Rect::from_min_size(egui::pos2(left, rect.center().y - size.y / 2.0), size),
                ppp,
            );
            icons::paint_caret(painter, caret_rect, caret, l.text);
            let galley = super::styled_galley(ui, title, &typography::LABEL);
            let pos = super::text_pos(rect, &galley, left + cw + space::SPACE_2, ppp);
            painter.galley(pos, galley, l.text);
            if !open && let Some(sc) = self.shortcut {
                let hint = super::galley(
                    ui,
                    sc,
                    fonts::font_id(&typography::VALUE_S),
                    typography::VALUE_S.line_height,
                );
                let x = rect.right() - space::SPACE_2 - hint.size().x;
                painter.galley(super::text_pos(rect, &hint, x, ppp), hint, l.hint);
            }
            if l.focus_ring {
                focus::paint_ring(painter, rect, self.corner_radius.nw.into());
            }
        }
        if open {
            let inner = Rect::from_min_max(
                egui::pos2(rect.left(), rect.top()),
                egui::pos2(rect.right() - space::SPACE_1, rect.bottom()),
            );
            // A child that takes no room in the parent layout: the actions sit on the header.
            let mut row = ui.new_child(
                UiBuilder::new()
                    .max_rect(inner)
                    .layout(Layout::right_to_left(Align::Center)),
            );
            row.spacing_mut().item_spacing.x = space::SPACE_2;
            actions(&mut row);
        }
        response
    }
}

/// An action button of a panel header: frameless icon button, 24 × 20, on surface.2; `name` is
/// its accessible name and tooltip.
pub fn action(ui: &mut Ui, icon: Icon, name: &str) -> Response {
    ui.add(Button::icon(icon, name).frameless(2).compact())
}

/// The "Panel-Kopf" card of the gallery: open with an action hovered, collapsed, focused panel;
/// then a live header.
pub fn gallery(ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = space::SPACE_3 + space::SPACE_0_5;
    let rounded = crate::theme::corner_radius(radius::M);
    let top_rounded = CornerRadius {
        se: 0,
        sw: 0,
        ..rounded
    };
    let body = |ui: &mut Ui, height: f32| {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let ppp = ui.pixels_per_point();
        let edge = px::line_width(ppp);
        px::fill(ui.painter(), r, 0.0, color32(color::SURFACE_0));
        let inner = Rect::from_min_max(
            egui::pos2(r.left() + edge, r.top()),
            egui::pos2(r.right() - edge, r.bottom() - edge),
        );
        px::fill(ui.painter(), inner, 0.0, color32(color::SURFACE_1));
    };
    super::sample(ui, "aufgeklappt · Aktion hover", |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let mut open = true;
        state::force(ui, State::REST, |ui| {
            PanelHeader::new("Browser", &mut open)
                .corner_radius(top_rounded)
                .show(ui, |ui| {
                    state::force(ui, State::HOVERED, |ui| action(ui, Icon::Ellipsis, "Mehr"));
                    action(ui, Icon::Plus, "Hinzufügen");
                });
        });
        body(ui, space::SPACE_6 + space::SPACE_3);
    });
    super::sample(ui, "eingeklappt", |ui| {
        let mut open = false;
        state::force(ui, State::REST, |ui| {
            PanelHeader::new("Inspector", &mut open)
                .shortcut("Strg 2")
                .corner_radius(rounded)
                .show(ui, |_| {});
        });
    });
    super::sample(
        ui,
        "fokussiertes Panel · 1 px akzent oben, Titel text-1",
        |ui| {
            let mut open = true;
            state::force(ui, State::REST, |ui| {
                PanelHeader::new("Piano Roll · Lead A", &mut open)
                    .focused(true)
                    .corner_radius(top_rounded)
                    .show(ui, |_| {});
            });
        },
    );

    let id = egui::Id::unique("komponist_gallery_panel_live");
    let mut open: bool = ui.data(|d| d.get_temp(id)).unwrap_or(true);
    super::sample(ui, "live", |ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let corners = if open { top_rounded } else { rounded };
        PanelHeader::new("Geräte", &mut open)
            .shortcut("Strg 3")
            .corner_radius(corners)
            .show(ui, |ui| {
                action(ui, Icon::Plus, "Gerät hinzufügen");
            });
        if open {
            body(ui, space::SPACE_6 + space::SPACE_3);
        }
    });
    ui.data_mut(|d| d.insert_temp(id, open));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        let open = look(true, false, State::REST);
        assert_eq!(
            (open.fill, open.text),
            (color32(color::SURFACE_2), color32(color::TEXT_2))
        );
        assert!(!open.accent_line);
        assert_eq!(look(false, false, State::REST).text, color32(color::TEXT_3));
        let focused = look(true, true, State::REST);
        assert_eq!(focused.text, color32(color::TEXT_1));
        assert!(focused.accent_line);
        let hover = look(true, false, State::HOVERED);
        assert_eq!(
            (hover.fill, hover.text),
            (color32(color::SURFACE_3), color32(color::TEXT_1))
        );
        assert_eq!(open.hint, color32(color::TEXT_4));
    }
}
