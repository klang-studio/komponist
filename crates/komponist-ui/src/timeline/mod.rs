//! Timeline elements: pure paint functions for the arrangement, the piano roll and the step grid
//! (section 05 "Spur, Clip, Note" of the design system, ARCHITECTURE §9.2).
//!
//! Nothing here allocates widgets or reacts to input. The canvases of wave 4 (arrangement WP-40,
//! piano roll WP-41, step sequencer WP-42) allocate one area, hit-test it themselves, compute the
//! state of each visible object and call the painters below:
//!
//! - [`track_header`]: [`paint_track_header`] for a track header (colour bar, name, instrument,
//!   M S R I, mini meter) with its 12 states. The toggles are painted statically;
//!   [`track_header::HeaderLayout::toggles`] exposes their rects so the arrangement can overlay the
//!   interactive WP-20 toggles at exactly those positions.
//! - [`clip`]: [`paint_clip`] for a clip (head, body, preview, hover handles, recording head, loop
//!   joints, trim edge, selection) and [`paint_clip_ghost`] for the dashed original of an Alt copy.
//!   The note or step preview is a precomputed [`PreviewMesh`], so the caller can cache it per
//!   (clip `Arc` pointer, zoom); at small zoom it merges notes per pixel column.
//! - [`note`]: [`paint_note`] for one note and [`NoteBatch`] for many (one mesh, the fast path for
//!   10 000 notes), with the zoom levels of the design system.
//! - [`ruler`]: [`paint_ruler`] (bar numbers, beat ticks, loop range, playhead cap) and
//!   [`paint_playhead`].
//! - [`grid`]: [`grid_lines`] and [`paint_grid`] for bar, beat and grid lines.
//!
//! Every painter has an accessibility helper (`*_access`) returning the German name, role and
//! state of the painted object as [`Access`]. The canvases register one AccessKit node per
//! visible object with it (rule 12), e.g. `Clip Beat A, Takt 1–4` or `Note C4, 1.1.000`.
//!
//! All axis-aligned edges are snapped to physical pixels ([`crate::px`]); only rounded corners are
//! anti-aliased. Geometry the design data gives without a token is collected in [`metrics`].

mod mesh;
pub mod metrics;

use egui::load::{SizeHint, TexturePoll};
use egui::{Color32, Painter, Rangef, Rect, Role, TextureOptions, WidgetInfo, accesskit, pos2};
use komponist_model::{Meter, Tick};
use komponist_tokens::Icon;


/// Maps musical time to screen x: `x = x0 + tick × px_per_tick`.
///
/// The arrangement zooms in px per bar (`barW`, 24–160 in the prototype), the piano roll in px
/// per 1/16 step (`cw`); both are a [`TimeAxis`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeAxis {
    /// Screen x of tick 0 (may lie left of the visible area when scrolled).
    pub x0: f32,
    /// Horizontal zoom in points per tick.
    pub px_per_tick: f32,
}

impl TimeAxis {
    /// An axis whose bars of `meter` are `px_per_bar` wide, tick 0 at `x0`.
    pub fn per_bar(x0: f32, px_per_bar: f32, meter: Meter) -> Self {
        Self {
            x0,
            px_per_tick: px_per_bar / meter.ticks_per_bar() as f32,
        }
    }

    /// An axis whose 1/16 steps are `px_per_step` wide, tick 0 at `x0`.
    pub fn per_step(x0: f32, px_per_step: f32) -> Self {
        Self {
            x0,
            px_per_tick: px_per_step / komponist_model::TICKS_PER_STEP as f32,
        }
    }

    /// Screen x of `tick`.
    pub fn x(&self, tick: Tick) -> f32 {
        self.x0 + tick.0 as f32 * self.px_per_tick
    }

    /// Width in points of a length of `ticks`.
    pub fn width(&self, ticks: Tick) -> f32 {
        ticks.0 as f32 * self.px_per_tick
    }

    /// The (fractional) tick at screen x.
    pub fn tick_at(&self, x: f32) -> f64 {
        f64::from(x - self.x0) / f64::from(self.px_per_tick)
    }

    /// Points per bar of `meter`.
    pub fn px_per_bar(&self, meter: Meter) -> f32 {
        self.px_per_tick * meter.ticks_per_bar() as f32
    }

    /// The ticks visible in the screen range `x`, widened to whole ticks.
    pub fn ticks_in(&self, x: Rangef) -> (Tick, Tick) {
        (
            Tick(self.tick_at(x.min).floor() as i64),
            Tick(self.tick_at(x.max).ceil() as i64),
        )
    }
}

/// Accessible role, German name and state of a painted timeline object (rule 12).
///
/// A canvas registers one AccessKit node per **visible** object with it, either through egui
/// (`response.widget_info(|| access.widget_info())` on an `ui.interact` area) or directly on an
/// AccessKit node ([`Access::fill_node`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Access {
    /// AccessKit role.
    pub role: Role,
    /// German accessible name, e.g. `Clip Beat A, Takt 1–4`.
    pub name: String,
    /// Selection state for selectable objects (clips, notes, tracks), `None` otherwise.
    pub selected: Option<bool>,
    /// A value shown to assistive technology, e.g. the playhead position of the ruler.
    pub value: Option<String>,
}

impl Access {
    /// An object without selection state.
    pub fn new(role: Role, name: impl Into<String>) -> Self {
        Self {
            role,
            name: name.into(),
            selected: None,
            value: None,
        }
    }

    /// A selectable object.
    pub fn selectable(role: Role, name: impl Into<String>, selected: bool) -> Self {
        Self {
            selected: Some(selected),
            ..Self::new(role, name)
        }
    }

    /// With `value`.
    #[must_use]
    pub fn with_value(mut self, value: impl Into<String>) -> Self {
        self.value = Some(value.into());
        self
    }

    /// As egui [`WidgetInfo`] (egui reports `selected` as AccessKit `toggled`).
    pub fn widget_info(&self) -> WidgetInfo {
        let mut info = WidgetInfo::labeled(self.role, true, &self.name);
        info.selected = self.selected;
        info.current_text_value = self.value.clone();
        info
    }

    /// Writes role, name and selection into an AccessKit node (`selected`, not `toggled`).
    pub fn fill_node(&self, node: &mut accesskit::Node) {
        node.set_role(self.role);
        node.set_label(self.name.clone());
        if let Some(selected) = self.selected {
            node.set_selected(selected);
        }
        if let Some(value) = &self.value {
            node.set_value(value.clone());
        }
    }
}

/// Paints `icon` into `rect`, tinted with `tint`, through a [`Painter`] (the timeline painters
/// have no `Ui`). The icon bytes are registered by [`crate::icons::install`]; until the SVG is
/// rasterised (normally at once) nothing is drawn.
pub(crate) fn paint_icon(painter: &Painter, rect: Rect, icon: Icon, tint: Color32) {
    let ppp = painter.pixels_per_point();
    let hint = SizeHint::Size {
        width: (rect.width() * ppp).round().max(1.0) as u32,
        height: (rect.height() * ppp).round().max(1.0) as u32,
        maintain_aspect_ratio: true,
    };
    if let Ok(TexturePoll::Ready { texture }) =
        painter
            .ctx()
            .try_load_texture(icon.uri(), TextureOptions::LINEAR, hint)
    {
        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        painter.image(texture.id, rect, uv, tint);
    }
}

/// `text` without a leading symbol `symbol` and the spaces after it ("⚠ Plugin fehlt" →
/// "Plugin fehlt"): the timeline painters draw ⚠ and ● as shapes, IBM Plex has no glyphs for them.
pub(crate) fn strip_symbol(text: &str, symbol: char) -> &str {
    text.strip_prefix(symbol).map_or(text, str::trim_start)
}

/// `Takt 9` or `Takt 1–4` (en dash): the bars a tick range touches, 1-based.
pub fn bar_span(start: Tick, end: Tick, meter: Meter) -> String {
    let per_bar = meter.ticks_per_bar();
    let first = start.0.div_euclid(per_bar) + 1;
    let last = (end.0 - 1).max(start.0).div_euclid(per_bar) + 1;
    if last > first {
        format!("Takt {first}\u{2013}{last}")
    } else {
        format!("Takt {first}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_maps_ticks_and_back() {
        let a = TimeAxis::per_bar(100.0, 72.0, Meter::FOUR_FOUR);
        assert_eq!(a.x(Tick(0)), 100.0);
        assert_eq!(a.x(Tick(3840)), 172.0);
        assert_eq!(a.px_per_bar(Meter::FOUR_FOUR), 72.0);
        assert!((a.tick_at(136.0) - 1920.0).abs() < 1e-3);
        let s = TimeAxis::per_step(0.0, 20.0);
        assert_eq!(s.x(Tick(240)), 20.0);
        assert_eq!(s.width(Tick(960)), 80.0);
        assert_eq!(a.ticks_in(Rangef::new(100.0, 172.0)), (Tick(0), Tick(3840)));
    }

    #[test]
    fn symbols_are_stripped() {
        assert_eq!(
            strip_symbol("⚠ Plugin fehlt: Vital", '⚠'),
            "Plugin fehlt: Vital"
        );
        assert_eq!(strip_symbol("● Aufnahme", '●'), "Aufnahme");
        assert_eq!(strip_symbol("Lead", '●'), "Lead");
    }

    #[test]
    fn bar_spans() {
        let m = Meter::FOUR_FOUR;
        assert_eq!(bar_span(Tick(0), Tick(4 * 3840), m), "Takt 1\u{2013}4");
        assert_eq!(bar_span(Tick(8 * 3840), Tick(9 * 3840), m), "Takt 9");
        assert_eq!(bar_span(Tick(8 * 3840), Tick(8 * 3840 + 10), m), "Takt 9");
        assert_eq!(bar_span(Tick(3000), Tick(4000), m), "Takt 1\u{2013}2");
    }

    #[test]
    fn access_into_widget_info() {
        let a = Access::selectable(Role::GridCell, "Clip Beat A, Takt 1\u{2013}4", true);
        let info = a.widget_info();
        assert_eq!(info.role, Role::GridCell);
        assert_eq!(info.label.as_deref(), Some("Clip Beat A, Takt 1\u{2013}4"));
        assert_eq!(info.selected, Some(true));
        let mut node = accesskit::Node::new(Role::Unknown);
        a.fill_node(&mut node);
        assert_eq!(node.role(), Role::GridCell);
        assert_eq!(node.label(), Some("Clip Beat A, Takt 1\u{2013}4"));
        assert_eq!(node.is_selected(), Some(true));
    }
}
