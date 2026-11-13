//! The komponist widgets.
//!
//! Controls and navigation (WP-20): [`button`], [`toggle`] (track toggles M S R I), [`switch`],
//! [`dropdown`], [`tabs`] (tab bar and segmented control), [`panel_header`], [`context_menu`] and
//! [`tooltip`] (hover tooltip with shortcut).
//!
//! Value controls and meters (WP-21): [`number_field`] (draggable number field), [`knob`],
//! [`fader`] (with the DBT taper and the value under it) and [`meter`] (level meter with peak
//! hold and clip latch). [`number_field`] also holds what the value controls share: drag
//! gesture, text entry, keyboard and wheel steps, AccessKit values.
//!
//! Every widget
//!
//! - computes its [`State`](crate::State) with [`State::of`](crate::State::of) and its colours
//!   in one `look(…, State)` function of its module, which the snapshot tests cover state by
//!   state;
//! - gives AccessKit a role and a German accessible name (`CLAUDE.md` rule 12);
//! - is a still picture when a state is forced around it ([`crate::state::force`]): it senses
//!   hover only, so it takes no clicks and no keyboard focus ([`sense`]). The gallery shows its
//!   samples that way, so Tab skips them; every gallery card also has a live row.
//!
//! Floating parts (dropdown list, context menu) are egui [`Area`](egui::Area)s on the
//! foreground layer; at most one is open at a time ([`floating`]).

pub mod button;
pub mod context_menu;
pub mod dropdown;
pub mod fader;
pub mod knob;
pub mod meter;
pub mod number_field;
pub mod panel_header;
pub mod switch;
pub mod tabs;
pub mod toggle;
pub mod tooltip;

use std::sync::Arc;

use egui::text::{LayoutJob, TextFormat};
use egui::{
    Color32, Event, FontId, Galley, Id, MouseWheelUnit, Pos2, Rect, Response, Sense, Stroke, Ui,
    Vec2, pos2,
};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{color, radius, shadow, space};

use crate::theme::{corner_radius, egui_shadow};
use crate::{fonts, px, state};

/// Sizes and weights the design system uses (`Designsystem.dc.html`) for which `tokens.json` has
/// no token. Where a value is an exact sum of spacing tokens, it is written that way; the rest are
/// the design's numbers. All of them are contract change requests of WP-20.
pub mod metrics {
    use komponist_tokens::{size, space};

    /// Caret ▾ (CSS borders 4 / 4 / 5): width and height. ▸ is the same triangle turned.
    pub const CARET: [f32; 2] = [8.0, 5.0];
    /// Horizontal padding of buttons and segments ("px 10").
    pub const BUTTON_PADDING_X: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Horizontal padding of a tab (`padding: 0 14px`).
    pub const TAB_PADDING_X: f32 = space::SPACE_3 + space::SPACE_0_5;
    /// Width of the ✓ column in dropdown lists and menus.
    pub const CHECK_COLUMN: f32 = space::SPACE_3 + space::SPACE_0_5;
    /// Size of the ✓ icon: the design sets ✓ as an 11 px glyph, about 12 px of icon box.
    pub const CHECK_ICON: f32 = 12.0;
    /// Gap between a tooltip's text and its shortcut.
    pub const SHORTCUT_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// A colour swatch of the "Farbe" submenu: width and height.
    pub const SWATCH: [f32; 2] = [20.0, 16.0];
    /// Columns of the colour grid (14 hues in two rows).
    pub const SWATCH_COLUMNS: usize = 7;
    /// Action button in a panel header: 24 × 20.
    pub const PANEL_ACTION: [f32; 2] = [size::CONTROL, size::CONTROL_COMPACT];
    /// CSS weight of IBM Plex medium (bundled for Sans and Mono).
    pub const MEDIUM: u16 = 500;
    /// CSS weight of IBM Plex semibold (bundled for Sans and Mono).
    pub const SEMIBOLD: u16 = 600;

    /// Number field (WP-21): `nf` in `Designsystem.dc.html`.
    pub mod number_field {
        /// Default width of a number field (`nf`: `width: 96`).
        pub const WIDTH: f32 = 96.0;
    }

    /// Flat knob (WP-21): `pt`, `arc`, `knob()`, `knobs`; card spec "Ø 28 (klein 20) · Bogen
    /// 270° · Strich 2". The diameters are the tokens `size.knob` and `size.knob-small`.
    pub mod knob {
        /// The arc spans 270°, from −135° to +135°, 0° up, clockwise (`a = -135 + 270 * v`).
        pub const ARC_DEGREES: f32 = 270.0;
        /// Stroke of track and value arc (`strokeWidth: 2`).
        pub const ARC_STROKE: f32 = 2.0;
        /// Arc radius = half the size − this (`r = c - 2`).
        pub const ARC_INSET: f32 = 2.0;
        /// Cap radius = half the size − this (`capR: c - 5`).
        pub const CAP_INSET: f32 = 5.0;
        /// Indicator length = half the size − this (`pt(c, c - 6, a)`).
        pub const INDICATOR_INSET: f32 = 6.0;
        /// Stroke of the indicator, round caps (`strokeWidth: 1.5`).
        pub const INDICATOR_STROKE: f32 = 1.5;
        /// Minimum width of a sample column in the gallery (`min-width: 56px`).
        pub const SAMPLE_MIN_WIDTH: f32 = 56.0;
    }

    /// Fader (WP-21): `scale`, `fader()`, `faders`; card spec "Bahn 4 · Knopf 32 × 44,
    /// Doppeltrapez (Taille 22) · kompakt 24 × 28 (Taille 15, 1 Rille je Hälfte)". Track width
    /// and thumb sizes are the tokens `size.fader-track`, `size.fader-thumb(-compact)`.
    pub mod fader {
        use komponist_tokens::space;

        /// Track length of the design's samples (`height: 200`); the default.
        pub const TRACK_LENGTH: f32 = 200.0;
        /// Width of the scale column, labels right-aligned in it (`width: 20`).
        pub const SCALE_WIDTH: f32 = space::SPACE_4 + space::SPACE_1;
        /// Gap between scale column and thumb (thumb `left: 22`).
        pub const SCALE_GAP: f32 = space::SPACE_0_5;
        /// Thickness of the value line ("Wertlinie", `midS` `strokeWidth: 2`).
        pub const VALUE_LINE: f32 = 2.0;
        /// Inset of the value line from the thumb's sides: normal (`x1=4 … x2=28`), compact.
        pub const VALUE_LINE_INSET: [f32; 2] = [4.0, 3.0];
        /// Waist of the normal thumb ("Taille 22"; polygon `5.5 … 26.5`).
        pub const WAIST: f32 = 22.0;
        /// Waist of the compact thumb ("Taille 15").
        pub const WAIST_COMPACT: f32 = 15.0;
        /// Grooves of the normal thumb per half: (distance from the top or bottom edge, inset
        /// from the side), from the SVG lines at y 6 / 11 / 33 / 38.
        pub const GROOVES: [(f32, f32); 2] = [(6.0, 5.0), (11.0, 6.0)];
        /// The one groove per half of the compact thumb ("1 Rille je Hälfte").
        pub const GROOVES_COMPACT: [(f32, f32); 1] = [(7.0, 6.0)];
        /// Gap between the sample columns of the card (`gap: 28px`).
        pub const SAMPLE_GAP: f32 = space::SPACE_6 + space::SPACE_1;
    }

    /// Timeline elements: track header, clip, note, ruler (WP-22).
    pub mod timeline;

    /// Level meter (WP-21): `meterV`, `meterH`, `meters`; card spec "Pegel 2 × 4, Abstand 1".
    pub mod meter {
        /// Bar width of the mixer meter (`meterV`: `width: 4`).
        pub const BAR_WIDTH: f32 = 4.0;
        /// Bar width of the track header meter (`meterH`: `width: 3`).
        pub const HEADER_BAR_WIDTH: f32 = 3.0;
        /// Gap between the bars (`gap: 1px`).
        pub const BAR_GAP: f32 = 1.0;
        /// Corner radius of bars and LED (`borderRadius: 1`).
        pub const BAR_RADIUS: f32 = 1.0;
        /// Height of the clip LED (`led`: `height: 4`).
        pub const LED_HEIGHT: f32 = 4.0;
        /// Gap between LED and bars (`gap: 2px`).
        pub const LED_GAP: f32 = 2.0;
        /// The clip block at the top of a header bar (`meterH` `clip`: `height: 3`).
        pub const CLIP_BLOCK: f32 = 3.0;
        /// Length of the design's samples (`height: 124px`); the default.
        pub const LENGTH: f32 = 124.0;
    }
}

/// The sense of a widget that wants `sense`: hover only while a state is forced around it
/// ([`state::force`]), so gallery samples and snapshot scenes take no clicks and no focus.
pub fn sense(ui: &Ui, sense: Sense) -> Sense {
    if state::forced(ui).is_some() {
        Sense::hover()
    } else {
        sense
    }
}

/// `true` if the widgets here are still pictures ([`sense`]).
pub fn is_still(ui: &Ui) -> bool {
    state::forced(ui).is_some()
}

/// `style` set in another bundled `weight` of its family (the design varies the weight of a type
/// token, e.g. `type.ui` at 600 for the primary button); falls back to the style's own face.
pub fn font_at(style: &TypeStyle, weight: u16) -> FontId {
    match komponist_tokens::font::face(style.family, weight) {
        Some(face) => FontId::new(style.size, fonts::face_family(face)),
        None => fonts::font_id(style),
    }
}

/// The font of the letters M S R I of track toggles: `type.value.s` (Mono 10) at semibold, as
/// `tgl` sets them (`fontWeight: 600`).
pub fn toggle_letter_font() -> FontId {
    font_at(&typography::VALUE_S, metrics::SEMIBOLD)
}

/// Lays out `text` on one line in `font` with line height `line_height` (no wrapping).
pub fn galley(ui: &Ui, text: &str, font: FontId, line_height: f32) -> Arc<Galley> {
    ui.painter().layout_job(LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            line_height: Some(line_height),
            color: Color32::PLACEHOLDER,
            ..Default::default()
        },
    ))
}

/// Lays out `text` in a type style (case and letter spacing applied).
pub fn styled_galley(ui: &Ui, text: &str, style: &TypeStyle) -> Arc<Galley> {
    ui.painter()
        .layout_job(fonts::layout_job(text, style, Color32::PLACEHOLDER))
}

/// The pixel-snapped top-left position that centres `galley` vertically in `rect`, its left
/// edge at `x`.
pub fn text_pos(rect: Rect, galley: &Galley, x: f32, pixels_per_point: f32) -> Pos2 {
    let y = rect.center().y - galley.size().y / 2.0;
    pos2(px::snap(x, pixels_per_point), px::snap(y, pixels_per_point))
}

/// The pixel-snapped top-left position that centres `galley` in `rect`.
pub fn text_pos_centered(rect: Rect, galley: &Galley, pixels_per_point: f32) -> Pos2 {
    text_pos(
        rect,
        galley,
        rect.center().x - galley.size().x / 2.0,
        pixels_per_point,
    )
}

/// Paints ✓ ([`Icon::Check`](komponist_tokens::Icon::Check) at [`metrics::CHECK_ICON`])
/// centred in the 14 px check column that starts at `left` in the row `row`.
pub fn paint_check(ui: &Ui, row: Rect, left: f32, color: Color32) {
    let center = pos2(left + metrics::CHECK_COLUMN / 2.0, row.center().y);
    let rect = Rect::from_center_size(center, Vec2::splat(metrics::CHECK_ICON));
    let rect = px::snap_rect(rect, ui.pixels_per_point());
    crate::icons::image(komponist_tokens::Icon::Check, color)
        .fit_to_exact_size(rect.size())
        .paint_at(ui, rect);
}

/// The frame of floating lists and menus: `surface.3`, `radius.l`, `shadow.float` plus the 1 px
/// `shadow.edge`, padding `space.1` (`menuBox`, the context menu). The edge lies outside the
/// content box, as the CSS `box-shadow` does.
pub fn menu_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(color32(color::SURFACE_3))
        .corner_radius(corner_radius(radius::L))
        .shadow(egui_shadow(&shadow::FLOAT))
        .stroke(Stroke::new(
            shadow::EDGE.spread,
            color32(shadow::EDGE.color),
        ))
        .inner_margin(egui::Margin::same(space::SPACE_1 as i8))
}

/// Mouse-wheel notches over a widget in this pass: + up, − down. Line-based wheels count their
/// lines; pixel-based ones (touchpads) accumulate in `ui` memory under `id` until a full line
/// (`InputOptions::line_scroll_speed`) is reached. Swallows the scroll, so a surrounding scroll
/// area stays put.
pub fn wheel_steps(ui: &mut Ui, id: Id) -> i32 {
    let line = ui.ctx().options(|o| o.input_options.line_scroll_speed);
    let mut acc: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
    let mut steps = 0.0_f32;
    ui.input_mut(|i| {
        for event in &i.events {
            if let Event::MouseWheel { unit, delta, .. } = event {
                match unit {
                    MouseWheelUnit::Line | MouseWheelUnit::Page => steps += delta.y.signum(),
                    MouseWheelUnit::Point => acc += delta.y / line,
                }
            }
        }
        i.smooth_scroll_delta = Vec2::ZERO;
    });
    let whole = acc.trunc();
    acc -= whole;
    ui.data_mut(|d| d.insert_temp(id, acc));
    (steps + whole) as i32
}

/// Index of the next enabled entry from `from` in direction `dir` (+1 / −1), without wrapping;
/// `None` if there is none. `from = None` starts before the first (or after the last) entry.
pub fn step_enabled(enabled: &[bool], from: Option<usize>, dir: i32) -> Option<usize> {
    let n = enabled.len() as i64;
    let mut i = match from {
        Some(i) => i as i64,
        None if dir > 0 => -1,
        None => n,
    };
    loop {
        i += i64::from(dir.signum());
        if i < 0 || i >= n {
            return None;
        }
        if enabled[i as usize] {
            return Some(i as usize);
        }
    }
}

/// The first enabled entry whose label starts with `prefix` (case-insensitive), searching from
/// `start` and wrapping around.
pub fn type_ahead<'a>(
    labels: impl ExactSizeIterator<Item = (&'a str, bool)> + Clone,
    prefix: &str,
    start: usize,
) -> Option<usize> {
    let prefix = prefix.to_lowercase();
    if prefix.is_empty() {
        return None;
    }
    let n = labels.len();
    let all: Vec<(&str, bool)> = labels.collect();
    (0..n)
        .map(|k| (start + k) % n)
        .find(|&i| all[i].1 && all[i].0.to_lowercase().starts_with(&prefix))
}

/// Typed text of this pass (`Event::Text`), for type-ahead.
pub fn typed_text(ui: &Ui) -> String {
    ui.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                Event::Text(t) => Some(t.as_str()),
                _ => None,
            })
            .collect()
    })
}

/// Pause after which type-ahead starts a new prefix (interaction timing, not a design token).
pub const TYPE_AHEAD_RESET_S: f64 = 1.0;

/// A type-ahead buffer: characters typed within [`TYPE_AHEAD_RESET_S`] of each other form one
/// prefix.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypeAhead {
    prefix: String,
    last: f64,
}

impl TypeAhead {
    /// Adds `typed` at time `now` (seconds); returns the prefix to search for.
    pub fn push(&mut self, typed: &str, now: f64) -> &str {
        if now - self.last > TYPE_AHEAD_RESET_S {
            self.prefix.clear();
        }
        self.prefix.push_str(typed);
        self.last = now;
        &self.prefix
    }
}

/// The one floating element (dropdown list, context menu) that is open, per context.
pub mod floating {
    use egui::{Context, Id};

    fn key() -> Id {
        Id::unique("komponist_ui::widgets::floating")
    }

    /// Opens `id` and closes whatever else was open.
    pub fn open(ctx: &Context, id: Id) {
        ctx.data_mut(|d| d.insert_temp(key(), Some(id)));
    }

    /// Closes `id` if it is open.
    pub fn close(ctx: &Context, id: Id) {
        if is_open(ctx, id) {
            close_all(ctx);
        }
    }

    /// Closes any floating element.
    pub fn close_all(ctx: &Context) {
        ctx.data_mut(|d| d.insert_temp::<Option<Id>>(key(), None));
    }

    /// `true` if `id` is the open floating element.
    pub fn is_open(ctx: &Context, id: Id) -> bool {
        current(ctx) == Some(id)
    }

    /// The open floating element, if any.
    pub fn current(ctx: &Context) -> Option<Id> {
        ctx.data(|d| d.get_temp::<Option<Id>>(key())).flatten()
    }
}

/// `true` if a pointer button went down this pass outside all of `rects`.
pub fn pressed_outside(ctx: &egui::Context, rects: &[Rect]) -> bool {
    ctx.input(|i| {
        i.events.iter().any(|e| match e {
            Event::PointerButton {
                pos, pressed: true, ..
            } => !rects.iter().any(|r| r.contains(*pos)),
            _ => false,
        })
    })
}

/// Sets AccessKit properties on the node of `response` that [`egui::WidgetInfo`] cannot express
/// (expanded, selected, description).
pub fn accesskit(response: &Response, f: impl FnOnce(&mut egui::accesskit::Node)) {
    response.ctx.accesskit_node_builder(response.id, f);
}

// ---------------------------------------------------------------------------------------------
// Gallery helpers

/// A gallery sample: the widget(s) drawn by `add`, the caption below in `type.value.s`, gap
/// `space.2` (the design's sample columns).
pub fn sample<R>(ui: &mut Ui, caption: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::SPACE_2;
        let r = add(ui);
        caption_text(ui, caption);
        r
    })
    .inner
}

/// A caption in `type.value.s` (Mono 10, `text.3`).
pub fn caption_text(ui: &mut Ui, text: &str) -> Response {
    ui.label(fonts::rich_text(text, &typography::VALUE_S))
}

/// A row of samples, `gap` apart, aligned at their bottom edge like the design's
/// `align-items: flex-end` rows.
pub fn sample_row(ui: &mut Ui, gap: f32, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(gap, gap);
        add(ui);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_skips_disabled() {
        let e = [true, false, true, true, false];
        assert_eq!(step_enabled(&e, Some(0), 1), Some(2));
        assert_eq!(step_enabled(&e, Some(3), 1), None);
        assert_eq!(step_enabled(&e, Some(2), -1), Some(0));
        assert_eq!(step_enabled(&e, None, 1), Some(0));
        assert_eq!(step_enabled(&e, None, -1), Some(3));
        assert_eq!(step_enabled(&[false, false], None, 1), None);
    }

    #[test]
    fn type_ahead_finds_prefixes() {
        let items = [
            ("1/4", true),
            ("1/8", true),
            ("1/16", true),
            ("Adaptiv", false),
        ];
        let it = || items.iter().map(|&(l, e)| (l, e));
        assert_eq!(type_ahead(it(), "1/1", 0), Some(2));
        assert_eq!(type_ahead(it(), "1", 1), Some(1));
        assert_eq!(type_ahead(it(), "1", 3), Some(0));
        assert_eq!(
            type_ahead(it(), "a", 0),
            None,
            "disabled entries are skipped"
        );
        assert_eq!(type_ahead(it(), "", 0), None);

        let mut t = TypeAhead::default();
        assert_eq!(t.push("1", 10.0), "1");
        assert_eq!(t.push("/", 10.5), "1/");
        assert_eq!(t.push("a", 12.0), "a");
    }

    #[test]
    fn floating_is_exclusive() {
        let ctx = egui::Context::default();
        let (a, b) = (Id::unique("a"), Id::unique("b"));
        floating::open(&ctx, a);
        assert!(floating::is_open(&ctx, a));
        floating::open(&ctx, b);
        assert!(!floating::is_open(&ctx, a));
        floating::close(&ctx, a);
        assert!(floating::is_open(&ctx, b));
        floating::close(&ctx, b);
        assert_eq!(floating::current(&ctx), None);
    }
}
