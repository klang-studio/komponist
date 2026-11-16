//! Tooltips: timing, frames and the value tooltip.
//!
//! Design system (`ix.tooltip`, `ix.button`): a tooltip appears after 600 ms of hover, below the
//! element with 6 px gap; while dragging a value, the value tooltip appears at once and follows
//! the value; hovering on to a neighbour shows its tooltip without delay. [`crate::theme`] puts
//! [`DELAY`] and [`GRACE`] into egui's interaction style, so `Response::on_hover_text` follows the
//! rule; [`frame`] gives the look (h 22 · px 8 · surface.5 · radius.m · shadow.float), and
//! [`show_value`] paints the inverted value tooltip immediately.
//!
//! The text layout of a tooltip with shortcut (`Quantisieren  Q`, two lines) is WP-20's tooltip
//! widget.

use std::time::Duration;

use egui::{Align2, Area, Context, Frame, Id, Margin, Order, Pos2, Rect, Stroke, pos2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, radius, shadow, size, space};

use crate::fonts;
use crate::theme::{corner_radius, egui_shadow};

/// Hover time before a tooltip appears ("Nach 600 ms Hover").
pub const DELAY: Duration = Duration::from_millis(600);

/// After a tooltip was shown, hovering another widget within this time shows its tooltip at once
/// ("Nachbar-Tooltips erscheinen ohne Verzögerung"). The design gives no number; 500 ms covers
/// the move across a gap between neighbouring controls.
pub const GRACE: Duration = Duration::from_millis(500);

/// Gap between the element and its tooltip ("unter dem Element, 6 px Abstand").
pub const GAP: f32 = space::SPACE_1_5;

/// Text style of tooltip text.
pub const TEXT: TypeStyle = typography::SMALL;

/// Text style of the value tooltip.
pub const VALUE_TEXT: TypeStyle = typography::VALUE_M;

/// Where hover tooltips ([`crate::widgets::tooltip::Tip::attach`]) go in a context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HoverSide {
    /// Below the element, as the design system says (the default).
    #[default]
    Below,
    /// Beside the element (left of it, or right where there is no room), vertically centred on
    /// it: for a window whose area below a strip of controls belongs to a native view that egui
    /// cannot draw over, such as the bridge's plugin frame on macOS (#52), where the plugin's
    /// `NSView` covers everything below the header.
    Beside,
}

/// Where the hover side of a context lives in its memory.
const HOVER_SIDE_ID: &str = "komponist_tooltip_hover_side";

/// Sets where hover tooltips go in `ctx`.
pub fn set_hover_side(ctx: &Context, side: HoverSide) {
    ctx.data_mut(|d| d.insert_temp(Id::unique(HOVER_SIDE_ID), side));
}

/// Where hover tooltips go in `ctx` ([`HoverSide::Below`] unless [`set_hover_side`] said else).
pub fn hover_side(ctx: &Context) -> HoverSide {
    ctx.data(|d| d.get_temp(Id::unique(HOVER_SIDE_ID)))
        .unwrap_or_default()
}

/// Where a tooltip sits relative to its element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    /// Below, left-aligned (hover tooltips).
    Below,
    /// Above, centred (the knob's value tooltip, "über dem Regler").
    Above,
}

impl Placement {
    /// The anchor point and alignment of a tooltip for an element at `rect`.
    pub fn anchor(self, rect: Rect) -> (Pos2, Align2) {
        match self {
            Self::Below => (pos2(rect.left(), rect.bottom() + GAP), Align2::LEFT_TOP),
            Self::Above => (
                pos2(rect.center().x, rect.top() - GAP),
                Align2::CENTER_BOTTOM,
            ),
        }
    }
}

fn vertical_margin(height: f32, style: &TypeStyle) -> i8 {
    ((height - style.line_height) / 2.0).max(0.0).round() as i8
}

/// The frame of a hover tooltip: surface.5, radius.m, shadow.float with the 1 px edge, px 8,
/// one line of [`TEXT`] gives the height `size.tooltip` (22).
pub fn frame() -> Frame {
    Frame::new()
        .fill(color32(color::SURFACE_5))
        .corner_radius(corner_radius(radius::M))
        .shadow(egui_shadow(&shadow::FLOAT))
        .stroke(Stroke::new(
            shadow::EDGE.spread,
            color32(shadow::EDGE.color),
        ))
        .inner_margin(Margin::symmetric(
            space::SPACE_2 as i8,
            vertical_margin(size::TOOLTIP, &TEXT),
        ))
}

/// The frame of the value tooltip while dragging: inverted (text.1 surface, surface.0 text),
/// radius.s, px 6, `size.control-compact` (20) high.
pub fn value_frame() -> Frame {
    Frame::new()
        .fill(color32(color::TEXT_1))
        .corner_radius(corner_radius(radius::S))
        .inner_margin(Margin::symmetric(
            space::SPACE_1_5 as i8,
            vertical_margin(size::CONTROL_COMPACT, &VALUE_TEXT),
        ))
}

/// Shows the value tooltip for an element at `rect` in this pass, without delay: call it every
/// pass while the drag lasts. `id` must be unique per element.
pub fn show_value(ctx: &Context, id: Id, rect: Rect, placement: Placement, text: &str) {
    let (pos, align) = placement.anchor(rect);
    Area::new(id.with("komponist_value_tooltip"))
        .order(Order::Tooltip)
        .fixed_pos(pos)
        .pivot(align)
        .interactable(false)
        .show(ctx, |ui| {
            value_frame().show(ui, |ui| {
                ui.label(fonts::rich_text(text, &VALUE_TEXT).color(color32(color::SURFACE_0)));
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    #[test]
    fn placement() {
        let r = Rect::from_min_size(pos2(10.0, 20.0), vec2(40.0, 24.0));
        assert_eq!(
            Placement::Below.anchor(r),
            (pos2(10.0, 50.0), Align2::LEFT_TOP)
        );
        assert_eq!(
            Placement::Above.anchor(r),
            (pos2(30.0, 14.0), Align2::CENTER_BOTTOM)
        );
    }

    #[test]
    fn frames_have_the_token_heights() {
        let m = frame().inner_margin;
        assert_eq!(
            f32::from(m.top + m.bottom) + TEXT.line_height,
            size::TOOLTIP
        );
        assert_eq!(f32::from(m.left), space::SPACE_2);
        let m = value_frame().inner_margin;
        assert_eq!(
            f32::from(m.top + m.bottom) + VALUE_TEXT.line_height,
            size::CONTROL_COMPACT
        );
        assert_eq!(DELAY.as_millis(), 600);
    }
}
