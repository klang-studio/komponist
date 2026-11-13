//! Design tokens of komponist as Rust constants, plus the bundled fonts and icons.
//!
//! `design/tokens.json` is the single source of truth. The build script turns it into the constants
//! of the modules below and fails on any token it does not understand. No other crate hard-codes a
//! design value (rule 2 in `CLAUDE.md`).
//!
//! - Colours ([`color`], track colours in [`track`]) are [`Srgba`]: `[r, g, b, a]`, 8-bit sRGB with
//!   straight (not premultiplied) alpha.
//! - Lengths ([`space`], [`radius`], [`line`](mod@line), [`size`], …) are `f32` design px, which
//!   are egui points.
//! - Track colours are computed with [`oklch_to_srgb`], a port of the prototype's `hex()`;
//!   [`TrackPalette`] holds all colours of one hue.
//! - [`font`] embeds IBM Plex Sans and Mono, [`Icon`] the Lucide icon subset ([`icon`]).
//!
//! The crate does not depend on egui. The `egui` feature adds [`egui_support`] with `Color32`
//! conversions.

pub mod icon;
mod oklch;
pub mod track;

pub mod font;
pub mod typography;

#[cfg(feature = "egui")]
pub mod egui_support;

pub use icon::Icon;
pub use oklch::oklch_to_srgb;
pub use track::TrackPalette;

/// A colour: `[r, g, b, a]`, 8-bit sRGB (gamma-encoded) with straight alpha, i.e. the colour
/// channels are not multiplied by alpha. This is what `#rrggbb` means in CSS, and what
/// `egui::Color32::from_rgba_unmultiplied` takes.
pub type Srgba = [u8; 4];

/// A shadow as in CSS `box-shadow` (outer shadows only): offset, blur radius, spread, colour.
///
/// Used for floating surfaces ([`shadow`]) and for rings drawn as a spread-only shadow
/// ([`select::RING`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BoxShadow {
    /// `[x, y]` offset in px.
    pub offset: [f32; 2],
    /// CSS blur radius in px; 0 is a sharp edge.
    pub blur: f32,
    /// How far the shadow grows beyond the object on every side, in px.
    pub spread: f32,
    /// Colour of the shadow.
    pub color: Srgba,
}

/// UI colours (`color.*`): surfaces, text, lines, accent, states, meters, canvas fills.
///
/// `color.<group>.<key>` becomes `<GROUP>_<KEY>`; `default` is dropped (`accent.default` →
/// [`ACCENT`](color::ACCENT)). The surface scale is also available as the array
/// [`SURFACE`](color::SURFACE) for the "+1 / +2 steps" hover and pressed rules, the line scale as
/// [`LINE`](color::LINE). `line.0`, `state.record-ink` and `canvas.*` are komponist additions
/// taken from the design data (`design/README.md`); `canvas.outside-clip` is translucent.
pub mod color {
    use crate::Srgba;
    include!(concat!(env!("OUT_DIR"), "/color.rs"));
}

/// Spacing scale (`space.*`) in px: `space.1.5` → [`SPACE_1_5`](space::SPACE_1_5).
pub mod space {
    include!(concat!(env!("OUT_DIR"), "/space.rs"));
}

/// Corner radii (`radius.*`) in px.
pub mod radius {
    include!(concat!(env!("OUT_DIR"), "/radius.rs"));
}

/// Line tokens (`line.*`).
pub mod line {
    include!(concat!(env!("OUT_DIR"), "/line.rs"));
}

/// Keyboard focus ring (`focus.*`): drawn only after keyboard input.
pub mod focus {
    use crate::Srgba;
    include!(concat!(env!("OUT_DIR"), "/focus.rs"));
}

/// Selection ring of timeline objects (`select.*`).
pub mod select {
    use crate::BoxShadow;
    include!(concat!(env!("OUT_DIR"), "/select.rs"));
}

/// Shadows (`shadow.*`), only for floating elements: menus, tooltips, plugin windows.
pub mod shadow {
    use crate::BoxShadow;
    include!(concat!(env!("OUT_DIR"), "/shadow.rs"));
}

/// Fixed sizes (`size.*`) in px. Ranges (`{default, min, max}`) become modules, pairs
/// (`[w, h]`) `[f32; 2]` arrays; `size.dock.max` is relative to the window height.
pub mod size {
    include!(concat!(env!("OUT_DIR"), "/size.rs"));
}
