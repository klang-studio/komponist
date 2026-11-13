//! Icons: the Lucide subset of `komponist-tokens` through the egui_extras SVG loader, and the
//! symbols the design draws as plain shapes (Play, Stop, Record, the carets ▾ ▸).
//!
//! Icons are white SVGs with a 16 px intrinsic size; [`image`] tints them. [`install`] registers
//! the loaders and all icon bytes once, so an icon is ready in the first frame that shows it.
//! The shape painters take the box to draw into, so the caller (transport bar, dropdown, panel
//! header) keeps the sizes of its screen.

use egui::{Color32, Context, Image, Painter, Pos2, Rect, Shape, Stroke, Vec2, pos2};
use komponist_tokens::{Icon, size};

/// Installs the egui_extras image loaders (SVG) and the bytes of every [`Icon`] on `ctx`.
pub fn install(ctx: &Context) {
    egui_extras::install_image_loaders(ctx);
    for &icon in Icon::ALL {
        ctx.include_bytes(icon.uri(), icon.svg_bytes());
    }
}

/// `icon` as an egui [`Image`], `size.icon` (16 px) square, tinted with `tint`.
pub fn image(icon: Icon, tint: Color32) -> Image<'static> {
    Image::from_bytes(icon.uri(), icon.svg_bytes())
        .tint(tint)
        .fit_to_exact_size(Vec2::splat(size::ICON))
}

/// Paints `icon` centred in `rect` at `size.icon`, tinted with `tint`.
pub fn paint(ui: &egui::Ui, rect: Rect, icon: Icon, tint: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::splat(size::ICON));
    image(icon, tint).paint_at(ui, r);
}

/// Direction of a caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Caret {
    /// ▾: open, expanded, dropdown.
    Down,
    /// ▸: closed, collapsed, submenu.
    Right,
}

fn triangle(painter: &Painter, points: [Pos2; 3], color: Color32) {
    painter.add(Shape::convex_polygon(points.to_vec(), color, Stroke::NONE));
}

/// Play: a filled triangle pointing right, filling `rect`.
pub fn paint_play(painter: &Painter, rect: Rect, color: Color32) {
    triangle(
        painter,
        [
            rect.left_top(),
            pos2(rect.right(), rect.center().y),
            rect.left_bottom(),
        ],
        color,
    );
}

/// Stop: a filled square, centred in `rect` with the side of its shorter edge, pixel-snapped.
pub fn paint_stop(painter: &Painter, rect: Rect, color: Color32) {
    let side = rect.width().min(rect.height());
    let r = Rect::from_center_size(rect.center(), Vec2::splat(side));
    crate::px::fill(painter, r, 0.0, color);
}

/// Record: a filled circle inscribed in `rect`.
pub fn paint_record(painter: &Painter, rect: Rect, color: Color32) {
    let radius = rect.width().min(rect.height()) / 2.0;
    painter.circle_filled(rect.center(), radius, color);
}

/// A caret (filled triangle) filling `rect`: ▾ is as wide as `rect` and points down, ▸ is as high
/// as `rect` and points right.
pub fn paint_caret(painter: &Painter, rect: Rect, caret: Caret, color: Color32) {
    let points = match caret {
        Caret::Down => [
            rect.left_top(),
            rect.right_top(),
            pos2(rect.center().x, rect.bottom()),
        ],
        Caret::Right => [
            rect.left_top(),
            pos2(rect.right(), rect.center().y),
            rect.left_bottom(),
        ],
    };
    triangle(painter, points, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_registered() {
        let ctx = Context::default();
        install(&ctx);
        for &icon in Icon::ALL {
            let poll = ctx.try_load_image(icon.uri(), egui::SizeHint::default());
            assert!(
                matches!(poll, Ok(egui::load::ImagePoll::Ready { .. })),
                "{icon:?} not ready"
            );
        }
        let img = image(Icon::Search, Color32::RED);
        assert_eq!(img.uri(), Some(Icon::Search.uri()));
    }
}
