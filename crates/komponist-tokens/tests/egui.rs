//! The tokens through egui: `Color32` conversion (feature `egui`), the icons through the
//! egui_extras SVG loader, the fonts through egui's font code.

use std::sync::Arc;

use egui::load::{ImagePoll, SizeHint};
use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use komponist_tokens::egui_support::{ToColor32, color32};
use komponist_tokens::{Icon, color, font, shadow, size};

#[test]
fn color32_conversion() {
    assert_eq!(color32(color::ACCENT), Color32::from_rgb(0x4e, 0xa7, 0xe6));
    assert_eq!(
        color::SURFACE_3.to_color32(),
        Color32::from_rgb(0x26, 0x28, 0x2b)
    );
    // Straight alpha in, premultiplied out.
    assert_eq!(
        shadow::FLOAT.color.to_color32(),
        Color32::from_rgba_unmultiplied(0, 0, 0, 115)
    );
    assert_eq!(
        color32([255, 255, 255, 128]).to_srgba_unmultiplied(),
        [255, 255, 255, 128]
    );
    const IN_CONST: Color32 = color32(color::TEXT_1);
    assert_eq!(IN_CONST, Color32::from_rgb(0xe8, 0xe9, 0xeb));
}

/// Rasterises `icon` with the loader the UI uses, at its intrinsic size.
fn rasterise(ctx: &egui::Context, icon: Icon) -> Arc<egui::ColorImage> {
    ctx.include_bytes(icon.uri(), icon.svg_bytes());
    match ctx.try_load_image(icon.uri(), SizeHint::default()) {
        Ok(ImagePoll::Ready { image }) => image,
        Ok(ImagePoll::Pending { .. }) => panic!("{icon:?}: still loading"),
        Err(e) => panic!("{icon:?}: {e}"),
    }
}

#[test]
fn icons_load_white_at_icon_size() {
    let ctx = egui::Context::default();
    egui_extras::install_image_loaders(&ctx);
    let side = size::ICON as usize;
    let coverage =
        |image: &egui::ColorImage| -> u32 { image.pixels.iter().map(|p| u32::from(p.a())).sum() };
    for &icon in Icon::ALL {
        let image = rasterise(&ctx, icon);
        assert_eq!(image.size, [side, side], "{icon:?}");
        // White, premultiplied: every pixel is grey with r = g = b = a, so the tint decides the colour.
        assert!(
            image
                .pixels
                .iter()
                .all(|p| p.r() == p.a() && p.g() == p.a() && p.b() == p.a()),
            "{icon:?} is not white"
        );
        // A 1.5 px stroke covers at least one pixel by more than half, even when it straddles two.
        assert!(
            image.pixels.iter().any(|p| p.a() > 160),
            "{icon:?} draws nothing solid"
        );
        // Stroke icons stay sparse on the 16 px grid.
        assert!(
            coverage(&image) < 255 * (side * side) as u32 / 2,
            "{icon:?} too heavy"
        );
    }
    let star = coverage(&rasterise(&ctx, Icon::Star));
    let filled = coverage(&rasterise(&ctx, Icon::StarFilled));
    assert!(
        filled > star * 3 / 2,
        "filled star {filled} vs outline {star}"
    );
}

#[test]
fn font_faces_have_tabular_digits() {
    let mut defs = FontDefinitions::empty();
    for face in &font::FACES {
        defs.font_data.insert(
            face.name.to_owned(),
            Arc::new(FontData::from_static(face.bytes)),
        );
        defs.families.insert(
            FontFamily::Name(face.name.into()),
            vec![face.name.to_owned()],
        );
    }
    let ctx = egui::Context::default();
    ctx.set_fonts(defs);
    ctx.run_ui(egui::RawInput::default(), |_| {})
        .textures_delta
        .clear();

    for face in &font::FACES {
        let id = FontId::new(100.0, FontFamily::Name(face.name.into()));
        let widths: Vec<f32> = ctx.fonts_mut(|f| {
            "0123456789+−"
                .chars()
                .map(|c| f.glyph_width(&id, c))
                .collect()
        });
        // 600 units per em at 100 px = 60 px, for every digit and for + and − (U+2212).
        for (c, w) in "0123456789+−".chars().zip(&widths) {
            assert!(
                (w - 60.0).abs() < 0.01,
                "{}: '{c}' is {w} px wide",
                face.name
            );
        }
    }
}
