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
pub mod device;
pub mod dropdown;
pub mod fader;
pub mod knob;
pub mod meter;
pub mod number_field;
pub mod panel_header;
pub mod switch;
pub mod tabs;
pub mod text_field;
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
    /// Horizontal padding of a 24 px button in `type.small` (the piano roll toolbar's "Snap" and
    /// "Falten": `height:24px; padding:0 9px; font-size:11px`, S01).
    pub const SMALL_BUTTON_PADDING_X: f32 = 9.0;
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
    /// Gap between a dropdown's label and its value (`gap:10px` in the toolbars' "Raster 1/16").
    pub const DROPDOWN_LABEL_GAP: f32 = space::SPACE_2 + space::SPACE_0_5;
    /// Height of dialog buttons (S06 `height:28px`).
    pub const DIALOG_BUTTON_HEIGHT: f32 = 28.0;
    /// Height of the start screen's buttons (S09 `height:32px`).
    pub const LARGE_BUTTON_HEIGHT: f32 = space::SPACE_8;
    /// Horizontal padding of dialog and start-screen buttons (`padding:0 12px`).
    pub const DIALOG_BUTTON_PADDING_X: f32 = space::SPACE_3;
    /// Extra gap before R in a row of track toggles, so M S and R I read as two groups (owner
    /// decision 2026-11-14, ARCHITECTURE §13 item 8; a step of the 4 px grid).
    pub const TOGGLE_GROUP_GAP: f32 = space::SPACE_1;
    /// Diameter of the filled record circle that replaces the letter R on the arm toggle.
    pub const TOGGLE_RECORD_DOT: f32 = 8.0;
    /// Box of the speaker icon that replaces the letter I on the monitoring toggle (the
    /// 18 × 16 toggle leaves no room for the 16 px icon).
    pub const TOGGLE_MONITOR_ICON: f32 = 12.0;
    /// CSS weight of IBM Plex regular.
    pub const REGULAR: u16 = 400;
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

    /// Device widgets (WP-83): what `device-tokens.json` does not give, from the generator
    /// `geraete_page_generator.py` (`W_*`) of the device handoff.
    pub mod device {
        use komponist_tokens::{radius, space};

        /// Height of a device label line (`lbl()`: `line-height:12px`; token `widget.number.label_h`).
        pub const LABEL_H: f32 = komponist_tokens::widget::number::LABEL_H;
        /// Corner radius of fields, segment tracks, toggle buttons, displays (`border-radius:3px`).
        pub const FIELD_RADIUS: f32 = radius::M;
        /// Corner radius of one segment (`W_seg` `border-radius:2px`).
        pub const SEGMENT_RADIUS: f32 = radius::S;
        /// Horizontal padding of a dropdown field (`W_drop` `padding:0 6px`).
        pub const DROPDOWN_PADDING_X: f32 = space::SPACE_1_5;
        /// The dropdown caret (`W_drop`: CSS borders 3 / 3 / 4): width and height.
        pub const DROPDOWN_CARET: [f32; 2] = [6.0, 4.0];
        /// Space above the knob in a knob cell (`W_knob` `padding-top:2px`).
        pub const KNOB_CELL_PAD_TOP: f32 = space::SPACE_0_5;
        /// Gap between knob, name and value in a knob cell (`gap:2px`).
        pub const KNOB_CELL_GAP: f32 = space::SPACE_0_5;
        /// Room kept left and right of a knob name (`max-width:52px` in 56).
        pub const KNOB_CELL_NAME_INSET: f32 = space::SPACE_0_5;
        /// Gap between the automation dot and the name (`gap:3px`).
        pub const KNOB_CELL_DOT_GAP: f32 = 3.0;
        /// Horizontal padding of a toggle button's text (`padding` of `W_tbtn` is 0; the text is
        /// only cut before it touches the edge).
        pub const BUTTON_PADDING_X: f32 = space::SPACE_1;
        /// Stroke of a wave icon (`wave_icon` `stroke-width="1.3"`).
        pub const GLYPH_STROKE: f32 = 1.3;
        /// Gap between knob and text of the large knob (`W_knobL` `gap:8px`, token layout).
        pub const KNOB_LARGE_GAP: f32 = space::SPACE_2;
        /// Left padding of the large knob (`W_knobL` `padding-left:2px`).
        pub const KNOB_LARGE_PAD_LEFT: f32 = space::SPACE_0_5;
        /// Gap between the lines of the large knob's text (`gap:2px`).
        pub const KNOB_LARGE_TEXT_GAP: f32 = space::SPACE_0_5;

        /// Mini fader (`W_fader`).
        pub mod mini_fader {
            /// Top of the thumb's travel (`trackT = 4`).
            pub const TRACK_TOP: f32 = 4.0;
            /// Gap between the name and the value under the fader (`gap:1px`).
            pub const TEXT_GAP: f32 = 1.0;
        }

        /// Gain-reduction meter (`W_gr`).
        pub mod meter_gr {
            /// Left of the first bar (`x = 6 + i * 16`) and top of all bars (`top:6px`).
            pub const PAD: f32 = 6.0;
            /// Height kept free below the bars for names and read-out (`bh = h - 34`).
            pub const BELOW: f32 = 34.0;
            /// Top of a bar name below the bar (`top: bh + 10`).
            pub const NAME_GAP: f32 = 10.0;
            /// Width of a bar name's box (`width:14px`).
            pub const NAME_W: f32 = 14.0;
            /// Distance of the read-out from the bottom (`bottom:2px`).
            pub const READOUT_BOTTOM: f32 = 2.0;
            /// Where the meter turns `meter.loud` (`okh = min(l, .75)`).
            pub const LOUD_FROM: f32 = 0.75;
            /// Corner radius of a bar (`border-radius:1px`).
            pub const BAR_RADIUS: f32 = 1.0;
            /// The window of the held maximum reduction (handoff §4: "letzten 1,5 s").
            pub const HOLD_SECONDS: f64 = 1.5;
        }

        /// Pad grid (`W_pads`).
        pub mod pad_grid {
            use komponist_tokens::space;
            /// Padding of a pad (`padding:5px 6px`): vertical and horizontal.
            pub const PADDING: [f32; 2] = [5.0, space::SPACE_1_5];
            /// Height of the track-colour bar at the top (`height:3px`).
            pub const BAR_H: f32 = 3.0;
            /// Corner radius of the bar (`border-radius:1px`).
            pub const BAR_RADIUS: f32 = 1.0;
            /// Gap between name and note (`gap:1px`).
            pub const TEXT_GAP: f32 = 1.0;
            /// Opacity of the note (`opacity:.8`).
            pub const NOTE_OPACITY: f32 = 0.8;
            /// Lightness and chroma of a playing pad (`okl(.92, .06, hue)`, `widget.pad_grid.playing`).
            pub const PLAYING_LC: (f64, f64) = (0.92, 0.06);
        }

        /// FM algorithm display (`W_alg`).
        pub mod fm {
            /// Operator box (`bw, bh = 22, 18`).
            pub const NODE: [f32; 2] = [22.0, 18.0];
            /// Corner radius of a box (`rx="2"`).
            pub const NODE_RADIUS: f32 = 2.0;
            /// Distance of the carriers' baseline from the bottom (`h - 6`).
            pub const BASELINE: f32 = 6.0;
            /// Top of the carrier row above the bottom (`yc = h - 28`).
            pub const CARRIER_Y: f32 = 28.0;
            /// Distance between two rows (`yc - ym = 42`).
            pub const ROW_STEP: f32 = 42.0;
            /// The feedback loop (`h6 v-16 h-(bw/2+6) v7`): reach right of the box, rise, drop.
            pub const FEEDBACK: [f32; 3] = [6.0, 16.0, 7.0];
            /// Room kept free at the top for the title.
            pub const TITLE_ROOM: f32 = 18.0;
        }

        /// Graph (`W_env`, `W_eq`, `W_wave`, `W_lfo`, `W_xy`, `W_transfer`, `handle()`, `display()`
        /// of `geraete_page_generator.py`): the numbers the device tokens do not carry.
        pub mod graph {
            /// Position of the title from the top left (`left:6px;top:4px`; token `widget.graph.title`).
            pub const TITLE_POS: [f32; 2] = [6.0, 4.0];
            /// Extra reach of a handle's hit area around its circle (interaction, not in the handoff).
            pub const HIT_PAD: f32 = 3.0;
            /// Half width of the hit strip of an edge handle (interaction, not in the handoff).
            pub const EDGE_HIT: f32 = 6.0;
            /// Stroke of a handle (token `widget.graph.handle.stroke` = `1.5 text.1`).
            pub const HANDLE_STROKE: f32 = 1.5;
            /// Stroke of the selected handle (token `widget.graph.handle.selected` = `2 accent`).
            pub const HANDLE_STROKE_SELECTED: f32 = 2.0;
            /// Radius of the level point marker (token `widget.graph.marker.point` = `r 3 text.1`).
            pub const POINT_R: f32 = 3.0;
            /// Dash and gap of the playhead (token `widget.graph.marker.playhead`: `dash 2 2`).
            pub const PLAYHEAD_DASH: [f32; 2] = [2.0, 2.0];
            /// Top of the playhead (`W_lfo` `y1="14"`, `W_env` `top - 4`).
            pub const PLAYHEAD_TOP: f32 = 14.0;
            /// Dash and gap of the transfer diagonal (`W_transfer` `stroke-dasharray="3 3"`).
            pub const DIAGONAL_DASH: [f32; 2] = [3.0, 3.0];
            /// Dash and gap of a threshold line (`W_transfer` `stroke-dasharray="2 2"`).
            pub const THRESHOLD_DASH: [f32; 2] = [2.0, 2.0];
            /// Left of the x axis labels from their tick and their distance from the bottom
            /// (`W_eq` `x + 3`, `y = h - 4`: the baseline).
            pub const LABEL_DX: f32 = 3.0;
            /// Baseline of the x axis labels above the bottom edge (`y = h - 4`).
            pub const LABEL_BASELINE: f32 = 4.0;
            /// Baseline of a number inside a handle below its centre (`y + 3`).
            pub const NUMBER_BASELINE: f32 = 3.0;
            /// The XY puck (`W_xy`): fill radius, ring radius (1 px accent when selected).
            pub const PUCK_R: [f32; 2] = [7.0, 9.0];
            /// Opacity of the region outside the sample's start–end (token `widget.graph.region_outside`
            /// = `surface.1 @ 75 %`).
            pub const REGION_OPACITY: f32 = 0.75;
            /// Gap between a readout's name and its value (the space between the spans of `W_xy`).
            pub const READOUT_GAP: f32 = 3.0;
            /// Distance of the XY readouts from the corners (`right:5px;bottom:3px`, `left:5px;top:3px`).
            pub const READOUT_INSET: [f32; 2] = [5.0, 3.0];
            /// Opacity of the XY crosshair (`stroke-opacity="0.5"`).
            pub const CROSSHAIR_OPACITY: f32 = 0.5;
            /// The flag of an edge handle (`W_wave`: `width 6, height 8` at `y = 14`).
            pub const EDGE_FLAG: [f32; 2] = [6.0, 8.0];
            /// Top of an edge handle's line and flag (`W_wave` `y1="14"`).
            pub const EDGE_TOP: f32 = 14.0;

            /// Axis padding in px at the low and the high end of the axis, per configuration.
            pub mod pad {
                /// `W_env`: `pad = 8`, `top = 18`, `bot = h - 8` (x, y).
                pub const ENVELOPE: ([f32; 2], [f32; 2]) = ([8.0, 8.0], [8.0, 18.0]);
                /// `W_eq`: x over the full width, y `h/2 ∓ (h/2 - 10)`.
                pub const EQ: ([f32; 2], [f32; 2]) = ([0.0, 0.0], [10.0, 10.0]);
                /// `W_wave`: amplitude `h/2 - 12` around `h/2 + 4` → 8 below, 16 above.
                pub const WAVEFORM: ([f32; 2], [f32; 2]) = ([0.0, 0.0], [8.0, 16.0]);
                /// `W_lfo`: `X = 8 + t(w - 16)`, `Y = h/2 + 4 - v(h/2 - 16)` → 12 below, 20 above.
                pub const LFO: ([f32; 2], [f32; 2]) = ([8.0, 8.0], [12.0, 20.0]);
                /// `W_xy`: `8 + x(w - 16)`, `8 + (1 - y)(h - 16)`.
                pub const XY: ([f32; 2], [f32; 2]) = ([8.0, 8.0], [8.0, 8.0]);
                /// `W_transfer`: the full area.
                pub const TRANSFER: ([f32; 2], [f32; 2]) = ([0.0, 0.0], [0.0, 0.0]);
            }

            /// The envelope (`W_env`): the sustain hold between decay and release is `.22` of the
            /// width; a segment's share of the width per unit of its normalised time.
            pub const ENVELOPE_HOLD: f64 = 0.22;
            /// Share of the width one unit of normalised attack, decay or release time takes
            /// (komponist: `W_env` takes fractions of the width directly; times of 0–1 would
            /// overlap, so the sum of the three plus the hold stays below 1).
            pub const ENVELOPE_SEGMENT: f64 = 0.26;
        }
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

/// The style of captions in the gallery (`type.value.s`).
pub fn knob_caption_style() -> TypeStyle {
    typography::VALUE_S
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
