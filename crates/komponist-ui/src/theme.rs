//! The egui [`Style`] and [`Visuals`] of komponist, built from `komponist-tokens`.
//!
//! Only a dark theme exists in v0.1 (HANDOFF §5); [`apply`] installs it for both egui themes, so a
//! system light preference changes nothing. Rules from the design system:
//!
//! - Surfaces instead of borders: widgets have no stroke, hover lifts the surface one step
//!   (`surface.3 → 4`), pressed two (`surface.5`), text goes `text.2 → text.1`.
//! - No animation: [`Style::animation_time`] is 0.
//! - Tooltips after 600 ms, neighbours without delay ([`crate::tooltip`]).
//! - Shadows only on floating surfaces: `shadow.float` plus the 1 px `shadow.edge`.
//!
//! The komponist widgets (WP-20/21/22) paint themselves from [`crate::state`]; these visuals make
//! plain egui widgets (scroll bars, text edits, the gallery's own chrome) fit in.

use std::collections::BTreeMap;

use egui::style::{Selection, TextCursorStyle, WidgetVisuals, Widgets};
use egui::{
    CornerRadius, Margin, Shadow, Stroke, Style, TextStyle, Theme, ThemePreference, Visuals, vec2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{BoxShadow, color, line, radius, shadow, size, space};

use crate::{fonts, tooltip};

/// A token radius as egui corner radius (radii are whole px).
pub fn corner_radius(r: f32) -> CornerRadius {
    CornerRadius::same(r.round() as u8)
}

/// A token shadow as egui [`Shadow`] (offsets, blur and spread are whole px).
pub fn egui_shadow(s: &BoxShadow) -> Shadow {
    Shadow {
        offset: [s.offset[0].round() as i8, s.offset[1].round() as i8],
        blur: s.blur.round() as u8,
        spread: s.spread.round() as u8,
        color: color32(s.color),
    }
}

/// The text style registered under the name of a type token (`"title"`, `"value.m"`, …), for
/// `ui.style().text_styles` lookups. The names are those of `typography::ALL`.
pub fn text_style(token: &str) -> TextStyle {
    TextStyle::Name(token.into())
}

fn text_styles() -> BTreeMap<TextStyle, egui::FontId> {
    let mut styles: BTreeMap<TextStyle, egui::FontId> = typography::ALL
        .iter()
        .map(|(name, style)| (text_style(name), fonts::font_id(style)))
        .collect();
    styles.insert(TextStyle::Heading, fonts::font_id(&typography::TITLE));
    styles.insert(TextStyle::Body, fonts::font_id(&typography::UI));
    styles.insert(TextStyle::Button, fonts::emphasis_font_id(&typography::UI));
    styles.insert(TextStyle::Small, fonts::font_id(&typography::SMALL));
    styles.insert(TextStyle::Monospace, fonts::font_id(&typography::VALUE_M));
    styles
}

fn widget(fill: [u8; 4], text: [u8; 4]) -> WidgetVisuals {
    WidgetVisuals {
        bg_fill: color32(fill),
        weak_bg_fill: color32(fill),
        bg_stroke: Stroke::NONE,
        corner_radius: corner_radius(radius::M),
        fg_stroke: Stroke::new(line::WIDTH, color32(text)),
        expansion: 0.0,
    }
}

/// The komponist [`Visuals`] (dark).
pub fn visuals() -> Visuals {
    let base = Visuals::dark();
    let float = egui_shadow(&shadow::FLOAT);
    let edge = Stroke::new(shadow::EDGE.spread, color32(shadow::EDGE.color));
    Visuals {
        dark_mode: true,
        override_text_color: None,
        weak_text_color: Some(color32(color::TEXT_3)),
        widgets: Widgets {
            noninteractive: WidgetVisuals {
                bg_fill: color32(color::SURFACE_1),
                weak_bg_fill: color32(color::SURFACE_1),
                bg_stroke: Stroke::new(line::WIDTH, color32(color::LINE_1)),
                ..widget(color::SURFACE_1, color::TEXT_1)
            },
            inactive: widget(color::SURFACE_3, color::TEXT_2),
            hovered: widget(color::SURFACE_4, color::TEXT_1),
            active: widget(color::SURFACE_5, color::TEXT_1),
            open: widget(color::SURFACE_4, color::TEXT_1),
        },
        selection: Selection {
            bg_fill: color32(color::ACCENT_SURFACE),
            stroke: Stroke::new(line::WIDTH, color32(color::TEXT_1)),
        },
        hyperlink_color: color32(color::ACCENT),
        faint_bg_color: color32(color::SURFACE_2),
        extreme_bg_color: color32(color::SURFACE_0),
        text_edit_bg_color: Some(color32(color::SURFACE_2)),
        code_bg_color: color32(color::SURFACE_2),
        warn_fg_color: color32(color::METER_LOUD),
        error_fg_color: color32(color::STATE_ERROR),
        window_corner_radius: corner_radius(radius::L),
        window_shadow: float,
        window_fill: color32(color::SURFACE_3),
        window_stroke: edge,
        window_highlight_topmost: false,
        menu_corner_radius: corner_radius(radius::L),
        panel_fill: color32(color::SURFACE_1),
        popup_shadow: float,
        text_cursor: TextCursorStyle {
            stroke: Stroke::new(line::WIDTH, color32(color::TEXT_1)),
            blink: false,
            ..base.text_cursor.clone()
        },
        button_frame: true,
        collapsing_header_frame: false,
        indent_has_left_vline: false,
        striped: false,
        slider_trailing_fill: false,
        interact_cursor: None,
        image_loading_spinners: false,
        ..base
    }
}

/// The komponist [`Style`]: [`visuals`], type tokens as text styles, spacing from `space.*` and
/// `size.*`, no animation, tooltip timing.
pub fn style() -> Style {
    let mut style = Style {
        text_styles: text_styles(),
        visuals: visuals(),
        animation_time: 0.0,
        explanation_tooltips: false,
        url_in_tooltip: false,
        ..Style::default()
    };
    style.scroll_animation = egui::style::ScrollAnimation::none();

    let s = &mut style.spacing;
    s.item_spacing = vec2(space::SPACE_2, space::SPACE_1);
    s.button_padding = vec2(space::SPACE_2, space::SPACE_1);
    s.interact_size.y = size::CONTROL;
    s.menu_margin = Margin::same(space::SPACE_1 as i8);
    s.window_margin = Margin::same(space::SPACE_3 as i8);
    s.menu_width = size::MENU_WIDTH;
    s.icon_width = size::ICON;
    s.icon_spacing = space::SPACE_1_5;

    let i = &mut style.interaction;
    i.tooltip_delay = tooltip::DELAY.as_secs_f32();
    i.tooltip_grace_time = tooltip::GRACE.as_secs_f32();
    i.show_tooltips_only_when_still = true;
    i.selectable_labels = false;
    i.multi_widget_text_select = false;
    style
}

/// Installs the komponist style for both egui themes and selects the dark one.
pub fn apply(ctx: &egui::Context) {
    let style = std::sync::Arc::new(style());
    ctx.set_style_of(Theme::Dark, style.clone());
    ctx.set_style_of(Theme::Light, style);
    ctx.set_theme(ThemePreference::Dark);
}

/// The default text style of a [`TypeStyle`] for widgets that take a [`TextStyle`]: the name
/// registered for it in [`style`], if it is one of `typography::ALL`.
pub fn text_style_of(style: &TypeStyle) -> Option<TextStyle> {
    typography::ALL
        .iter()
        .find(|(_, s)| s == style)
        .map(|(name, _)| text_style(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::FontFamily;

    /// `true` if `family` is a bundled Plex family or one of egui's two defaults (which
    /// `fonts::definitions` maps to Plex).
    fn is_plex(family: &FontFamily) -> bool {
        match family {
            FontFamily::Name(name) => name.starts_with("IBMPlex"),
            FontFamily::Proportional | FontFamily::Monospace => true,
        }
    }

    #[test]
    fn style_follows_the_rules() {
        let s = style();
        assert_eq!(s.animation_time, 0.0);
        assert_eq!(s.interaction.tooltip_delay, 0.6);
        assert!(s.interaction.tooltip_grace_time > 0.0);
        let w = &s.visuals.widgets;
        // Surfaces instead of borders, hover +1, pressed +2, text 2 → 1.
        assert_eq!(w.inactive.weak_bg_fill, color32(color::SURFACE_3));
        assert_eq!(w.hovered.weak_bg_fill, color32(color::SURFACE_4));
        assert_eq!(w.active.weak_bg_fill, color32(color::SURFACE_5));
        assert_eq!(w.inactive.fg_stroke.color, color32(color::TEXT_2));
        assert_eq!(w.hovered.fg_stroke.color, color32(color::TEXT_1));
        for v in [&w.inactive, &w.hovered, &w.active, &w.open] {
            assert_eq!(v.bg_stroke, Stroke::NONE);
            assert_eq!(v.expansion, 0.0);
        }
        assert_eq!(s.visuals.window_shadow.offset, [0, 6]);
        assert_eq!(s.visuals.window_shadow.blur, 16);
        assert_eq!(s.visuals.window_shadow.color.a(), 0x73);
        assert_eq!(s.visuals.window_corner_radius, CornerRadius::same(6));
        assert_eq!(s.spacing.interact_size.y, 24.0);
        assert_eq!(s.spacing.menu_width, 220.0);
    }

    #[test]
    fn every_type_token_is_a_text_style() {
        let s = style();
        for (name, ts) in typography::ALL {
            let id = &s.text_styles[&text_style(name)];
            assert_eq!(id.size, ts.size, "{name}");
            assert!(is_plex(&id.family), "{name}");
            assert_eq!(text_style_of(&ts), Some(text_style(name)));
        }
        for t in [
            TextStyle::Heading,
            TextStyle::Body,
            TextStyle::Button,
            TextStyle::Small,
            TextStyle::Monospace,
        ] {
            assert!(is_plex(&s.text_styles[&t].family), "{t:?}");
        }
    }
}
