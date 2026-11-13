//! Conversions to egui types (feature `egui`).

use egui::Color32;

use crate::Srgba;

/// Converts a token colour (sRGB, straight alpha) to egui's premultiplied [`Color32`].
pub const fn color32(c: Srgba) -> Color32 {
    Color32::from_rgba_unmultiplied_const(c[0], c[1], c[2], c[3])
}

/// `.to_color32()` on token colours: `color::ACCENT.to_color32()`.
pub trait ToColor32 {
    /// See [`color32`].
    fn to_color32(self) -> Color32;
}

impl ToColor32 for Srgba {
    fn to_color32(self) -> Color32 {
        color32(self)
    }
}
