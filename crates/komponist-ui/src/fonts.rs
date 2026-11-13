//! Fonts: the embedded IBM Plex faces as egui font families, one family per face.
//!
//! egui picks a face only by family, so every bundled face (`komponist_tokens::font::FACES`) gets
//! its own [`FontFamily::Name`], keyed by the face name (`"IBMPlexSans-SemiBold"`). A text style
//! of the design system ([`TypeStyle`]) maps to the family of its face with [`font_id`]; size,
//! line height, letter spacing, case and default colour come from the style as well
//! ([`text_format`], [`rich_text`], [`layout_job`]).
//!
//! `FontFamily::Proportional` is Plex Sans 400 and `FontFamily::Monospace` Plex Mono 400, so plain
//! egui text uses the design fonts too. egui's default fonts stay in the list as fallbacks for the
//! few symbols Plex does not have, so no glyph is drawn as tofu.

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId, RichText};
use komponist_tokens::color;
use komponist_tokens::egui_support::color32;
use komponist_tokens::font::{self, Face, Family};
use komponist_tokens::typography::TypeStyle;

/// The egui family of one bundled face.
pub fn face_family(face: &Face) -> FontFamily {
    FontFamily::Name(face.name.into())
}

/// The egui family for `family` at `weight`, falling back to the regular (400) face if that
/// weight is not bundled.
pub fn family(family: Family, weight: u16) -> FontFamily {
    let face = font::face(family, weight)
        .or_else(|| font::face(family, 400))
        .expect("the regular face of every family is bundled");
    face_family(face)
}

/// The font of a text style: its face and size.
pub fn font_id(style: &TypeStyle) -> FontId {
    FontId::new(style.size, face_family(style.face()))
}

/// The font of a text style in its emphasis weight (`type.ui` 500 for buttons); the regular font
/// if the style has no emphasis weight.
pub fn emphasis_font_id(style: &TypeStyle) -> FontId {
    let face = style.emphasis_face().unwrap_or_else(|| style.face());
    FontId::new(style.size, face_family(face))
}

/// The default colour of a text style: its token colour (`type.small` → `text.2`), else
/// `text.1`.
pub fn style_color(style: &TypeStyle) -> Color32 {
    color32(style.color.unwrap_or(color::TEXT_1))
}

/// Applies the case of `style` to `text` (`type.label` is set in capitals).
pub fn apply_case(text: &str, style: &TypeStyle) -> String {
    if style.uppercase {
        text.to_uppercase()
    } else {
        text.to_owned()
    }
}

/// A [`TextFormat`] for `style` in `color`: font, line height and letter spacing.
pub fn text_format(style: &TypeStyle, color: Color32) -> TextFormat {
    TextFormat {
        font_id: font_id(style),
        extra_letter_spacing: style.tracking_px(),
        line_height: Some(style.line_height),
        color,
        ..Default::default()
    }
}

/// A single-section [`LayoutJob`] of `text` in `style` and `color` (case applied), for painting
/// with [`egui::Painter::layout_job`].
pub fn layout_job(text: &str, style: &TypeStyle, color: Color32) -> LayoutJob {
    LayoutJob::single_section(apply_case(text, style), text_format(style, color))
}

/// `text` as [`RichText`] in `style` with its default colour ([`style_color`]), for labels.
pub fn rich_text(text: &str, style: &TypeStyle) -> RichText {
    RichText::new(apply_case(text, style))
        .font(font_id(style))
        .line_height(Some(style.line_height))
        .extra_letter_spacing(style.tracking_px())
        .color(style_color(style))
}

/// The spec line of a text style as the design system writes it: `"Sans 16/20 · 600"`,
/// `"Sans 12/16 · 400 / 500"`, `"Sans 10/12 · 600 · caps +0.08em"`, `"Mono 20/24 · 500 · tnum"`.
pub fn spec(style: &TypeStyle) -> String {
    let family = match style.family {
        Family::Sans => "Sans",
        Family::Mono => "Mono",
    };
    let mut parts = vec![format!("{family} {}/{}", style.size, style.line_height)];
    parts.push(match style.emphasis_weight {
        Some(e) => format!("{} / {e}", style.weight),
        None => style.weight.to_string(),
    });
    if style.uppercase {
        parts.push(format!("caps +{}em", style.tracking_em));
    }
    if style.family == Family::Mono {
        parts.push("tnum".to_owned());
    }
    parts.join(" · ")
}

/// Font definitions with all bundled faces, egui's default fonts as fallbacks.
pub fn definitions() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let fallback_sans = defs
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let fallback_mono = defs
        .families
        .get(&FontFamily::Monospace)
        .cloned()
        .unwrap_or_default();

    for face in &font::FACES {
        defs.font_data.insert(
            face.name.to_owned(),
            Arc::new(FontData::from_static(face.bytes)),
        );
    }
    for face in &font::FACES {
        let mut list = vec![face.name.to_owned()];
        if face.family == Family::Mono {
            // Symbols Plex Mono lacks: Plex Sans of the same weight before egui's fonts.
            let sans = font::face(Family::Sans, face.weight)
                .or_else(|| font::face(Family::Sans, 400))
                .expect("Plex Sans 400 is bundled");
            list.push(sans.name.to_owned());
        }
        list.extend(fallback_sans.iter().cloned());
        defs.families.insert(face_family(face), list);
    }

    let regular = |family| {
        font::face(family, 400)
            .expect("the regular face of every family is bundled")
            .name
            .to_owned()
    };
    let mut proportional = vec![regular(Family::Sans)];
    proportional.extend(fallback_sans);
    let mut monospace = vec![regular(Family::Mono), regular(Family::Sans)];
    monospace.extend(fallback_mono);
    defs.families.insert(FontFamily::Proportional, proportional);
    defs.families.insert(FontFamily::Monospace, monospace);
    defs
}

/// Installs [`definitions`] into `ctx`.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(definitions());
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_tokens::typography;

    #[test]
    fn every_face_has_its_own_family_first() {
        let defs = definitions();
        for face in &font::FACES {
            let list = &defs.families[&face_family(face)];
            assert_eq!(list[0], face.name);
            assert!(defs.font_data.contains_key(face.name));
        }
        assert_eq!(
            defs.families[&FontFamily::Proportional][0],
            "IBMPlexSans-Regular"
        );
        assert_eq!(
            defs.families[&FontFamily::Monospace][0],
            "IBMPlexMono-Regular"
        );
    }

    #[test]
    fn styles_map_to_their_faces() {
        assert_eq!(
            font_id(&typography::TITLE),
            FontId::new(16.0, FontFamily::Name("IBMPlexSans-SemiBold".into()))
        );
        assert_eq!(
            font_id(&typography::VALUE_L),
            FontId::new(20.0, FontFamily::Name("IBMPlexMono-Medium".into()))
        );
        assert_eq!(
            emphasis_font_id(&typography::UI).family,
            FontFamily::Name("IBMPlexSans-Medium".into())
        );
        assert_eq!(
            emphasis_font_id(&typography::SMALL),
            font_id(&typography::SMALL)
        );
        assert_eq!(
            family(Family::Mono, 600),
            FontFamily::Name("IBMPlexMono-SemiBold".into())
        );
        assert_eq!(
            family(Family::Mono, 700),
            FontFamily::Name("IBMPlexMono-Regular".into())
        );
    }

    #[test]
    fn spec_lines() {
        assert_eq!(spec(&typography::TITLE), "Sans 16/20 · 600");
        assert_eq!(spec(&typography::UI), "Sans 12/16 · 400 / 500");
        assert_eq!(spec(&typography::LABEL), "Sans 10/12 · 600 · caps +0.08em");
        assert_eq!(spec(&typography::VALUE_L), "Mono 20/24 · 500 · tnum");
    }

    #[test]
    fn label_format() {
        let f = text_format(&typography::LABEL, Color32::WHITE);
        assert_eq!(f.line_height, Some(12.0));
        assert!((f.extra_letter_spacing - 0.8).abs() < 1e-6);
        assert_eq!(
            apply_case("Raster · Snap", &typography::LABEL),
            "RASTER · SNAP"
        );
        assert_eq!(apply_case("Raster", &typography::UI), "Raster");
        assert_eq!(style_color(&typography::LABEL), color32(color::TEXT_3));
        assert_eq!(style_color(&typography::UI), color32(color::TEXT_1));
    }

    /// Every character the design texts use must come from a font, not as tofu.
    #[test]
    fn design_symbols_have_glyphs() {
        let ctx = egui::Context::default();
        install(&ctx);
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        // The carets ▾ ▸ are drawn as shapes (`icons::paint_caret`), ⋯ is `Icon::Ellipsis`.
        let symbols = "−×·„“…→←↑↓↔↕✓ØÄÖÜäöüß°½";
        for (_, style) in typography::ALL {
            let id = font_id(&style);
            let missing: String = symbols
                .chars()
                .filter(|&c| !ctx.fonts_mut(|f| f.has_glyph(&id, c)))
                .collect();
            assert!(missing.is_empty(), "{missing:?} missing in {:?}", id.family);
        }
    }
}
