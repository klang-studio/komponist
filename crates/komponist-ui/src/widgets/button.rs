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
//! Sizes ([`Height`]): `size.control` 24 (default), `size.control-compact` 20 (panel headers;
//! [`Button::small`] sets its text in `type.small`), and the taller buttons of dialogs and the
//! start screen: 28 and 32 high, padding 0 12 ([`Button::dialog`], [`Button::large`]). A
//! [`Button::chip`] adds a shortcut chip after the label ("Quantisieren Q": `gap 8`, Mono 10,
//! `text.3`); [`Button::hint`] a hint in `type.ui` ("Beat-Skizze"). [`Button::ground`] makes a
//! secondary button rest one step above the surface it sits on (S06: on `surface.3` it is
//! `surface.4`).
//!
//! Focus: the 1 px ring at 1 px distance, after keyboard input only. Enter or Space clicks a
//! focused button. The tooltip appears after 600 ms with the shortcut ([`Tip`]).

use egui::{Color32, FontId, Response, Role, Sense, Ui, Widget, WidgetInfo, vec2};
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
    /// A text button that rests in text.2 and regular weight like a hint ("Maximieren" in the
    /// dock header, `maxS`); hover and press are as for any button.
    pub quiet: bool,
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
    let base_text = if appearance.icon || appearance.quiet {
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

/// How high a button is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Height {
    /// `size.control`, 24.
    #[default]
    Control,
    /// `size.control-compact`, 20 (panel-header actions).
    Compact,
    /// 28, padding 0 12: the buttons of dialogs (S06 `height:28px`).
    Dialog,
    /// 32, padding 0 12: the buttons of the start screen (S09), text in `type.track`.
    Large,
}

impl Height {
    /// The height in points.
    pub fn points(self) -> f32 {
        match self {
            Self::Control => size::CONTROL,
            Self::Compact => size::CONTROL_COMPACT,
            Self::Dialog => metrics::DIALOG_BUTTON_HEIGHT,
            Self::Large => metrics::LARGE_BUTTON_HEIGHT,
        }
    }

    /// Gap between the label and a chip or hint: 8 in toolbars (`Quantisieren`'s `gap:8px`), 10
    /// in dialogs and on the start screen (S06 "Exportieren", `gap:10px`).
    pub fn chip_gap(self) -> f32 {
        match self {
            Self::Control | Self::Compact => space::SPACE_2,
            Self::Dialog | Self::Large => metrics::SHORTCUT_GAP,
        }
    }

    /// Horizontal padding.
    pub fn padding_x(self) -> f32 {
        match self {
            Self::Control | Self::Compact => metrics::BUTTON_PADDING_X,
            Self::Dialog | Self::Large => metrics::DIALOG_BUTTON_PADDING_X,
        }
    }
}

/// Text after a button's label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trailing<'a> {
    /// A shortcut chip ("Q", "Strg N"): Mono 10, `text.3` (on a primary button the label's
    /// colour, medium).
    Chip(&'a str),
    /// A hint ("Beat-Skizze"): `type.ui`, `text.3`.
    Hint(&'a str),
}

impl Trailing<'_> {
    /// The text.
    pub fn text(&self) -> &str {
        match self {
            Self::Chip(t) | Self::Hint(t) => t,
        }
    }

    fn font(&self, primary: bool) -> FontId {
        match self {
            Self::Chip(_) if primary => super::font_at(&typography::VALUE_S, metrics::MEDIUM),
            Self::Chip(_) => fonts::font_id(&typography::VALUE_S),
            Self::Hint(_) => fonts::font_id(&typography::UI),
        }
    }

    fn line_height(&self) -> f32 {
        match self {
            Self::Chip(_) => typography::VALUE_S.line_height,
            Self::Hint(_) => typography::UI.line_height,
        }
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
    height: Height,
    small: bool,
    ground: Option<usize>,
    trailing: Option<Trailing<'a>>,
    fixed_width: Option<f32>,
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
            height: Height::Control,
            small: false,
            ground: None,
            trailing: None,
            fixed_width: None,
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

    /// Text.2 and regular weight at rest (see [`Appearance::quiet`]).
    pub fn quiet(mut self) -> Self {
        self.appearance.quiet = true;
        self
    }

    /// `size.control-compact` (20) high instead of `size.control` (24); icon buttons stay 24 wide
    /// (panel-header actions).
    pub fn compact(mut self) -> Self {
        self.height = Height::Compact;
        self
    }

    /// The text in `type.small` (11 px; the toolbars' "Snap"). With [`Self::compact`] the
    /// padding is 8, as in the arrangement's panel header.
    pub fn small(mut self) -> Self {
        self.small = true;
        self
    }

    /// A dialog button: 28 high, padding 0 12.
    pub fn dialog(mut self) -> Self {
        self.height = Height::Dialog;
        self
    }

    /// A start-screen button: 32 high, padding 0 12, text in `type.track`.
    pub fn large(mut self) -> Self {
        self.height = Height::Large;
        self
    }

    /// The surface level the button rests on: a secondary button is one step lighter than it
    /// (S06 on `surface.3` is `surface.4`). Without it the button is `surface.3`.
    pub fn ground(mut self, ground: usize) -> Self {
        self.ground = Some(ground);
        self
    }

    /// A shortcut chip after the label ("Quantisieren Q").
    pub fn chip(mut self, chip: &'a str) -> Self {
        self.trailing = Some(Trailing::Chip(chip));
        self
    }

    /// A hint after the label, in `type.ui` `text.3`.
    pub fn hint(mut self, hint: &'a str) -> Self {
        self.trailing = Some(Trailing::Hint(hint));
        self
    }

    /// A fixed width; the chip or hint then sits at the right edge.
    pub fn width(mut self, width: f32) -> Self {
        self.fixed_width = Some(width);
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

impl Button<'_> {
    /// The text style of the label: `type.ui`; `type.small` for [`Self::small`], `type.track` for
    /// [`Self::large`].
    fn style(&self) -> typography::TypeStyle {
        match (self.height, self.small) {
            (_, true) => typography::SMALL,
            (Height::Large, _) => typography::TRACK,
            _ => typography::UI,
        }
    }

    /// The fill and text colours: [`look`], except that a secondary button with a
    /// [`Self::ground`] rests one step above it.
    fn colours(&self, st: State) -> ButtonLook {
        let l = look(self.appearance, st);
        let plain = self.appearance.kind == Kind::Secondary
            && !self.appearance.on
            && !self.appearance.icon
            && self.appearance.frameless.is_none();
        match self.ground {
            Some(ground) if plain && !st.disabled => ButtonLook {
                fill: Some(state::surface_for(ground + 1, st)),
                ..l
            },
            _ => l,
        }
    }
}

impl Widget for Button<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let height = self.height.points();
        let padding = if self.small && self.height == Height::Compact {
            space::SPACE_2
        } else if self.small && self.height == Height::Control {
            metrics::SMALL_BUTTON_PADDING_X
        } else {
            self.height.padding_x()
        };
        let style = self.style();
        let primary = self.appearance.kind == Kind::Primary;
        let galley = match self.content {
            Content::Text(text) => {
                let weight = if primary {
                    metrics::SEMIBOLD
                } else if self.appearance.quiet {
                    metrics::REGULAR
                } else {
                    metrics::MEDIUM
                };
                let font = super::font_at(&style, weight);
                Some(super::galley(ui, text, font, style.line_height))
            }
            Content::Icon(_) => None,
        };
        let trailing = self.trailing.filter(|_| galley.is_some()).map(|t| {
            (
                super::galley(ui, t.text(), t.font(primary), t.line_height()),
                t,
            )
        });
        let gap = self.height.chip_gap();
        let width = match (&galley, self.fixed_width) {
            (_, Some(w)) => w,
            (Some(g), None) => {
                g.size().x.ceil()
                    + trailing
                        .as_ref()
                        .map_or(0.0, |(t, _)| gap + t.size().x.ceil())
                    + 2.0 * padding
            }
            (None, None) => size::CONTROL,
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
            let l = self.colours(st);
            let painter = ui.painter();
            if let Some(fill) = l.fill {
                px::fill(painter, rect, radius::M, fill);
            }
            let ppp = ui.pixels_per_point();
            match (self.content, galley) {
                (Content::Text(_), Some(galley)) => {
                    let left_aligned = trailing.is_some()
                        || self.fixed_width.is_some()
                        || matches!(self.height, Height::Dialog | Height::Large);
                    if left_aligned {
                        let pos = super::text_pos(rect, &galley, rect.left() + padding, ppp);
                        let text_right = pos.x + galley.size().x;
                        painter.galley(pos, galley, l.text);
                        if let Some((t, _)) = trailing {
                            let x = if self.fixed_width.is_some() {
                                rect.right() - padding - t.size().x
                            } else {
                                text_right + gap
                            };
                            let colour = if primary || !enabled {
                                l.text
                            } else {
                                color32(color::TEXT_3)
                            };
                            painter.galley(super::text_pos(rect, &t, x, ppp), t, colour);
                        }
                    } else {
                        let pos = super::text_pos_centered(rect, &galley, ppp);
                        painter.galley(pos, galley, l.text);
                    }
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
    let row3: [(&str, Button<'_>, State); 6] = [
        ("Kürzel", Button::new("Quantisieren").chip("Q"), S::REST),
        (
            "klein 20",
            Button::new("Snap").compact().small().on(true),
            S::REST,
        ),
        (
            "Dialog 28",
            Button::new("Abbrechen").dialog().ground(3),
            S::REST,
        ),
        (
            "Dialog primär",
            Button::primary("Exportieren").dialog().chip("Enter"),
            S::REST,
        ),
        (
            "Start 32",
            Button::new("Öffnen")
                .large()
                .ground(2)
                .width(180.0)
                .chip("Strg O"),
            S::REST,
        ),
        (
            "Hinweis",
            Button::new("Vorlage")
                .large()
                .ground(2)
                .width(180.0)
                .hint("Beat"),
            S::HOVERED,
        ),
    ];
    ui.spacing_mut().item_spacing.y = space::SPACE_4;
    for row in [&row1[..], &row2[..], &row3[..]] {
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
