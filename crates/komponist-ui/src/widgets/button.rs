//! Button: secondary (text), primary, icon; with or without surface; as a toggle ("Snap an").
//!
//! `h 24 · px 10 · radius.m · ui-medium` (`btn`, `iconB`, `buttonRows` of the design system):
//!
//! | | rest | hover | pressed | disabled |
//! |---|---|---|---|---|
//! | secondary | surface.3, text.1 | surface.4 | surface.5 | surface.2, text.4 |
//! | on (toggle) | accent, accent.ink | accent.hover | accent.hover | surface.2, text.4 |
//! | primary (600) | accent, accent.ink | accent.hover | accent.hover | surface.2, text.4 |
//! | icon 24 × 24 | surface.3, text.2 | surface.4, text.1 | surface.5, text.1 | surface.2, text.4 |
//! | without surface | –, text.2 | ground + 2, text.1 | ground + 3, text.1 | –, text.4 |
//!
//! Focus: the 1 px ring at 1 px distance, after keyboard input only. Enter or Space clicks a
//! focused button. The tooltip appears after 600 ms with the shortcut ([`Tip`]).

use egui::{Color32, Response, Role, Sense, Ui, Widget, WidgetInfo, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Icon, color, radius, size, space};

use super::metrics;
use super::tooltip::Tip;
use crate::state::{self, State};
use crate::{focus, fonts, icons, px};

/// Primary or secondary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    /// The default button ("Quantisieren").
    #[default]
    Secondary,
    /// The one main action of a dialog or screen ("Exportieren"): accent surface, semibold.
    Primary,
}

/// The variant of a button, which with the [`State`] decides its colours ([`look`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appearance {
    /// Primary or secondary.
    pub kind: Kind,
    /// A toggle button that is on ("Snap an"): looks like a primary button.
    pub on: bool,
    /// Without surface, resting on surface `ground` (`ohne Fläche`); `None` = with surface.
    pub frameless: Option<usize>,
    /// Shows an icon (text.2 at rest) instead of text (text.1).
    pub icon: bool,
}

/// Colours of a button in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ButtonLook {
    /// Surface; `None` = none (a button without surface at rest).
    pub fill: Option<Color32>,
    /// Text or icon colour.
    pub text: Color32,
    /// Draw the focus ring.
    pub focus_ring: bool,
}

/// The [`ButtonLook`] of a button of `appearance` in `state` (table in the module docs).
pub fn look(appearance: Appearance, state: State) -> ButtonLook {
    let base_text = if appearance.icon {
        color::TEXT_2
    } else {
        color::TEXT_1
    };
    let focus_ring = state.focused && !state.disabled;
    let active = state.hovered || state.pressed;
    let (fill, text) = if state.disabled {
        let fill = appearance
            .frameless
            .is_none()
            .then(|| color32(color::SURFACE_2));
        (fill, color32(color::TEXT_4))
    } else if appearance.kind == Kind::Primary || appearance.on {
        let fill = if active {
            color::ACCENT_HOVER
        } else {
            color::ACCENT
        };
        (Some(color32(fill)), color32(color::ACCENT_INK))
    } else if let Some(ground) = appearance.frameless {
        let fill = if state.pressed {
            Some(state::surface(ground + 3))
        } else if state.hovered {
            Some(state::surface(ground + 2))
        } else {
            None
        };
        (fill, state::text_for(base_text, state))
    } else {
        (
            Some(state::surface_for(3, state)),
            state::text_for(base_text, state),
        )
    };
    ButtonLook {
        fill,
        text,
        focus_ring,
    }
}

#[derive(Clone, Copy, Debug)]
enum Content<'a> {
    Text(&'a str),
    Icon(Icon),
}

/// A button. Add it with `ui.add(Button::new("Quantisieren"))`; `clicked()` on the response
/// says it was pressed (pointer, Enter or Space when focused, AccessKit).
#[must_use = "add it with `ui.add(…)`"]
#[derive(Clone, Copy, Debug)]
pub struct Button<'a> {
    content: Content<'a>,
    appearance: Appearance,
    toggle: bool,
    compact: bool,
    name: Option<&'a str>,
    shortcut: Option<&'a str>,
    tip: Option<Tip<'a>>,
}

impl<'a> Button<'a> {
    fn with(content: Content<'a>, appearance: Appearance) -> Self {
        Self {
            content,
            appearance,
            toggle: false,
            compact: false,
            name: None,
            shortcut: None,
            tip: None,
        }
    }

    /// A secondary text button.
    pub fn new(text: &'a str) -> Self {
        Self::with(Content::Text(text), Appearance::default())
    }

    /// A primary text button.
    pub fn primary(text: &'a str) -> Self {
        Self::with(
            Content::Text(text),
            Appearance {
                kind: Kind::Primary,
                ..Appearance::default()
            },
        )
    }

    /// A square icon button; `name` is its accessible name and tooltip ("Hinzufügen").
    pub fn icon(icon: Icon, name: &'a str) -> Self {
        Self {
            name: Some(name),
            ..Self::with(
                Content::Icon(icon),
                Appearance {
                    icon: true,
                    ..Appearance::default()
                },
            )
        }
    }

    /// Makes it a toggle button, shown as on (accent) when `on` ("Snap"). AccessKit reports it
    /// as a toggle button with that state.
    pub fn on(mut self, on: bool) -> Self {
        self.toggle = true;
        self.appearance.on = on;
        self
    }

    /// Without surface at rest, on a ground of surface level `ground` (1 for panels, 2 for panel
    /// headers): hover shows surface `ground + 2`.
    pub fn frameless(mut self, ground: usize) -> Self {
        self.appearance.frameless = Some(ground);
        self
    }

    /// `size.control-compact` (20) high instead of `size.control` (24); icon buttons stay 24 wide
    /// (panel-header actions).
    pub fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    /// The shortcut shown in the tooltip ("Q").
    pub fn shortcut(mut self, shortcut: &'a str) -> Self {
        self.shortcut = Some(shortcut);
        self
    }

    /// A tooltip of its own (instead of name and shortcut).
    pub fn tip(mut self, tip: Tip<'a>) -> Self {
        self.tip = Some(tip);
        self
    }

    /// The accessible name, if it differs from the text.
    pub fn name(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }

    /// The tooltip this button shows, if any: its own, else for icon buttons and buttons with a
    /// shortcut the name with the shortcut.
    pub fn effective_tip(&self) -> Option<Tip<'a>> {
        if self.tip.is_some() {
            return self.tip;
        }
        let name = self.accessible_name();
        match (self.content, self.shortcut) {
            (_, Some(sc)) => Some(Tip::new(name).shortcut(sc)),
            (Content::Icon(_), None) => Some(Tip::new(name)),
            (Content::Text(_), None) => None,
        }
    }

    /// The accessible name: [`Self::name`], else the text, else the icon's name.
    pub fn accessible_name(&self) -> &'a str {
        match (self.name, self.content) {
            (Some(name), _) => name,
            (None, Content::Text(text)) => text,
            (None, Content::Icon(icon)) => icon.name(),
        }
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let height = if self.compact {
            size::CONTROL_COMPACT
        } else {
            size::CONTROL
        };
        let style = typography::UI;
        let galley = match self.content {
            Content::Text(text) => {
                let font = match self.appearance.kind {
                    Kind::Primary => super::font_at(&style, metrics::SEMIBOLD),
                    Kind::Secondary => fonts::emphasis_font_id(&style),
                };
                Some(super::galley(ui, text, font, style.line_height))
            }
            Content::Icon(_) => None,
        };
        let width = match &galley {
            Some(g) => g.size().x.ceil() + 2.0 * metrics::BUTTON_PADDING_X,
            None => size::CONTROL,
        };
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, height), super::sense(ui, Sense::click()));
        let st = State::of(ui, &response);
        let enabled = ui.is_enabled() && !st.disabled;
        let name = self.accessible_name();
        let (toggle, on) = (self.toggle, self.appearance.on);
        response.widget_info(|| {
            if toggle {
                WidgetInfo::selected(Role::Button, enabled, on, name)
            } else {
                WidgetInfo::labeled(Role::Button, enabled, name)
            }
        });

        if ui.is_rect_visible(rect) {
            let l = look(self.appearance, st);
            let painter = ui.painter();
            if let Some(fill) = l.fill {
                px::fill(painter, rect, radius::M, fill);
            }
            match (self.content, galley) {
                (Content::Text(_), Some(galley)) => {
                    let pos = super::text_pos_centered(rect, &galley, ui.pixels_per_point());
                    painter.galley(pos, galley, l.text);
                }
                (Content::Icon(icon), _) => icons::paint(ui, rect, icon, l.text),
                (Content::Text(_), None) => {}
            }
            if l.focus_ring {
                focus::paint_ring(painter, rect, radius::M);
            }
        }
        if !super::is_still(ui)
            && let Some(tip) = self.effective_tip()
        {
            tip.attach(&response);
        }
        response
    }
}

/// The "Button" card of the gallery: the two rows of `buttonRows`, then a live row.
pub fn gallery(ui: &mut Ui) {
    use State as S;
    let row1: [(&str, Button<'_>, State); 6] = [
        ("standard", Button::new("Quantisieren"), S::REST),
        ("hover", Button::new("Quantisieren"), S::HOVERED),
        ("gedrückt", Button::new("Quantisieren"), S::PRESSED),
        ("an", Button::new("Snap").on(true), S::REST),
        ("fokus", Button::new("Quantisieren"), S::FOCUSED),
        ("deaktiviert", Button::new("Quantisieren"), S::DISABLED),
    ];
    let plus = || Button::icon(Icon::Plus, "Hinzufügen");
    let row2: [(&str, Button<'_>, State); 7] = [
        ("icon", plus(), S::REST),
        ("hover", plus(), S::HOVERED),
        ("ohne Fläche", plus().frameless(1), S::REST),
        ("ohne Fl. hover", plus().frameless(1), S::HOVERED),
        ("primär", Button::primary("Exportieren"), S::REST),
        ("primär hover", Button::primary("Exportieren"), S::HOVERED),
        ("primär deakt.", Button::primary("Exportieren"), S::DISABLED),
    ];
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    for row in [&row1[..], &row2[..]] {
        super::sample_row(ui, space::SPACE_4, |ui| {
            for (caption, button, st) in row {
                super::sample(ui, caption, |ui| {
                    // Distinct names, so the live buttons below are found by their own.
                    let name = format!("{} · {caption}", button.accessible_name());
                    state::force(ui, *st, |ui| ui.add(button.name(&name)));
                });
            }
        });
    }
    live_row(ui);
}

fn live_row(ui: &mut Ui) {
    let id = egui::Id::unique("komponist_gallery_button_live");
    let (mut clicks, mut snap): (u32, bool) = ui.data(|d| d.get_temp(id)).unwrap_or((0, true));
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SPACE_4;
        if ui
            .add(
                Button::new("Quantisieren")
                    .shortcut("Q")
                    .name("Quantisieren · live"),
            )
            .clicked()
        {
            clicks += 1;
        }
        if ui.add(Button::new("Snap").on(snap)).clicked() {
            snap = !snap;
        }
        if ui.add(Button::icon(Icon::Plus, "Hinzufügen")).clicked() {
            clicks += 1;
        }
        if ui
            .add(Button::icon(Icon::Ellipsis, "Mehr").frameless(1))
            .clicked()
        {
            clicks += 1;
        }
        if ui.add(Button::primary("Exportieren")).clicked() {
            clicks += 1;
        }
        super::caption_text(ui, &format!("live · {clicks} Klicks"));
    });
    ui.data_mut(|d| d.insert_temp(id, (clicks, snap)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(c: [u8; 4]) -> Option<Color32> {
        Some(color32(c))
    }

    #[test]
    fn secondary_follows_the_state_rules() {
        let a = Appearance::default();
        assert_eq!(look(a, State::REST).fill, c(color::SURFACE_3));
        assert_eq!(look(a, State::REST).text, color32(color::TEXT_1));
        assert_eq!(look(a, State::HOVERED).fill, c(color::SURFACE_4));
        assert_eq!(look(a, State::PRESSED).fill, c(color::SURFACE_5));
        assert_eq!(look(a, State::DISABLED).fill, c(color::SURFACE_2));
        assert_eq!(look(a, State::DISABLED).text, color32(color::TEXT_4));
        assert!(look(a, State::FOCUSED).focus_ring);
        assert!(!look(a, State::FOCUSED.with_disabled(true)).focus_ring);
    }

    #[test]
    fn primary_and_on() {
        let p = Appearance {
            kind: Kind::Primary,
            ..Appearance::default()
        };
        assert_eq!(look(p, State::REST).fill, c(color::ACCENT));
        assert_eq!(look(p, State::HOVERED).fill, c(color::ACCENT_HOVER));
        assert_eq!(look(p, State::REST).text, color32(color::ACCENT_INK));
        assert_eq!(look(p, State::DISABLED).fill, c(color::SURFACE_2));
        let on = Appearance {
            on: true,
            ..Appearance::default()
        };
        assert_eq!(look(on, State::REST), look(p, State::REST));
    }

    #[test]
    fn icon_and_frameless() {
        let i = Appearance {
            icon: true,
            ..Appearance::default()
        };
        assert_eq!(look(i, State::REST).text, color32(color::TEXT_2));
        assert_eq!(look(i, State::HOVERED).text, color32(color::TEXT_1));
        assert_eq!(look(i, State::HOVERED).fill, c(color::SURFACE_4));
        let f = Appearance {
            frameless: Some(1),
            ..i
        };
        assert_eq!(look(f, State::REST).fill, None);
        assert_eq!(look(f, State::HOVERED).fill, c(color::SURFACE_3));
        assert_eq!(look(f, State::PRESSED).fill, c(color::SURFACE_4));
        assert_eq!(look(f, State::DISABLED).fill, None);
        // In a panel header (surface.2), hover is surface.4 like "⋯" in the design.
        let h = Appearance {
            frameless: Some(2),
            ..i
        };
        assert_eq!(look(h, State::HOVERED).fill, c(color::SURFACE_4));
    }

    #[test]
    fn tips() {
        assert_eq!(Button::new("Quantisieren").effective_tip(), None);
        assert_eq!(
            Button::new("Quantisieren").shortcut("Q").effective_tip(),
            Some(Tip::new("Quantisieren").shortcut("Q"))
        );
        assert_eq!(
            Button::icon(Icon::Plus, "Hinzufügen").effective_tip(),
            Some(Tip::new("Hinzufügen"))
        );
    }
}
