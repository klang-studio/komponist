//! Track header ("Spur-Header", design system `hdr`, `headers`): 224 × 56 (height 32–160), colour
//! bar, name and instrument, the toggles M S R I and a two-channel mini meter.
//!
//! [`paint_track_header`] paints everything as a still picture: the toggles with the colours of
//! the WP-20 track toggle ([`toggle::look`], [`paint_toggle`]) and the meter with the WP-21 header
//! meter ([`meter::paint`] in [`meter::Style::HEADER`], on the fader taper). The arrangement
//! (WP-40) puts the interactive widgets on top of the painted ones: a
//! [`toggle::TrackToggle`] at each of [`HeaderLayout::toggles`] and the clickable
//! [`meter::Meter`] (clip reset) at [`HeaderLayout::meter`]. Painted alone (gallery, snapshots,
//! thumbnails), the header looks the same.
//!
//! States ([`State`]): rest `surface.2`, hover `surface.3`, selected `accent.surface`, keyboard
//! focus an inset 1 px accent line. Muted (M or implied by solo elsewhere): colour bar `dim`,
//! name `text.3`. Implied mute shows M with a `text.3` contour on `surface.2`.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{Color32, Painter, Rect, Role, Stroke, StrokeKind, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{Icon, TrackPalette, color, line, radius, size, space};

use super::Access;
use crate::Lang;
use crate::state::{self, State};
use crate::widgets::meter::{self, MeterState};
use crate::widgets::metrics::timeline as metrics;
use crate::widgets::toggle::{self, ToggleKind};
use crate::{fonts, px, theme, widgets};

/// What a track header shows.
#[derive(Clone, Copy, Debug)]
pub struct HeaderModel<'a> {
    /// Track name (`type.track`).
    pub name: &'a str,
    /// Second line: instrument, "stumm durch Solo", "Instrument hierher ziehen",
    /// "Plugin fehlt: Vital".
    pub instrument: &'a str,
    /// The second line is an error: `state.error` instead of `text.3`, with ⚠ in front (drawn as
    /// icon; a leading "⚠ " in [`Self::instrument`] is dropped).
    pub instrument_error: bool,
    /// Colours of the track hue.
    pub palette: &'a TrackPalette,
    /// M.
    pub mute: bool,
    /// S.
    pub solo: bool,
    /// R.
    pub arm: bool,
    /// I.
    pub monitor: bool,
    /// Muted because another track is soloed (M shows the implied look unless on).
    pub implied_mute: bool,
    /// The track's meter (ballistics owned by the caller, WP-21); `None`: silent.
    pub meter: Option<&'a MeterState>,
}

impl<'a> HeaderModel<'a> {
    /// A header for `name` / `instrument` in `palette`, all toggles off, meter silent.
    pub fn new(name: &'a str, instrument: &'a str, palette: &'a TrackPalette) -> Self {
        Self {
            name,
            instrument,
            instrument_error: false,
            palette,
            mute: false,
            solo: false,
            arm: false,
            monitor: false,
            implied_mute: false,
            meter: None,
        }
    }

    /// Whether the track is silent (muted or implied muted): dim colour bar, name in `text.3`.
    pub fn dimmed(&self) -> bool {
        self.mute || self.implied_mute
    }

    /// Whether toggle `kind` is on.
    pub fn is_on(&self, kind: ToggleKind) -> bool {
        match kind {
            ToggleKind::Mute => self.mute,
            ToggleKind::Solo => self.solo,
            ToggleKind::Arm => self.arm,
            ToggleKind::Monitor => self.monitor,
        }
    }

    /// Whether toggle `kind` shows its implied look: M muted by a solo elsewhere, I off on an
    /// armed track (which monitors anyway).
    pub fn is_implied(&self, kind: ToggleKind) -> bool {
        match kind {
            ToggleKind::Mute => self.implied_mute,
            ToggleKind::Monitor => self.arm && !self.monitor,
            ToggleKind::Solo | ToggleKind::Arm => false,
        }
    }
}

/// Where the parts of a header lie (`hdr` flex layout: bar 3, gap 4, text with 6 padding, gap 4,
/// toggles 4 × 18 with gap 2, gap 4, meter with padding 8 8 8 6).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderLayout {
    /// Everything above the bottom separator.
    pub body: Rect,
    /// The 1 px `surface.0` line at the bottom (`borderBottom`).
    pub separator: Rect,
    /// The colour bar on the left.
    pub color_bar: Rect,
    /// Name and instrument line, each clipped with an ellipsis.
    pub text: Rect,
    /// The toggles M S R I (`size.toggle`), in [`ToggleKind::ALL`] order. WP-40 places the
    /// interactive WP-20 toggles exactly here.
    pub toggles: [Rect; 4],
    /// The meter: both channels ([`meter::Style::HEADER`]); WP-40 places the clickable meter
    /// here.
    pub meter: Rect,
    /// The meter with its padding.
    pub meter_area: Rect,
}

/// The layout of a header in `rect` (any height from `size.track.min` to `size.track.max`;
/// contents are centred vertically, the colour bar and the meter stretch).
pub fn header_layout(rect: Rect) -> HeaderLayout {
    let sep = line::WIDTH;
    let body = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - sep));
    let separator = Rect::from_min_max(pos2(rect.min.x, rect.max.y - sep), rect.max);
    let color_bar = Rect::from_min_max(
        body.min,
        pos2(body.min.x + metrics::HEADER_COLOR_BAR, body.max.y),
    );

    // Meter: padding 8 8 8 6 around the two channels.
    let meter_w = meter::Style::HEADER.thickness();
    let meter_area = Rect::from_min_max(
        pos2(
            body.max.x - space::SPACE_2 - meter_w - space::SPACE_1_5,
            body.min.y,
        ),
        body.max,
    );
    let meter = Rect::from_x_y_ranges(
        meter_area.min.x + space::SPACE_1_5..=meter_area.min.x + space::SPACE_1_5 + meter_w,
        body.min.y + space::SPACE_2..=body.max.y - space::SPACE_2,
    );

    // Toggles: size.toggle each, space.0.5 between, space.1 before the meter.
    let [tw, th] = size::TOGGLE;
    let n = ToggleKind::ALL.len() as f32;
    let toggles_w = n * tw + (n - 1.0) * space::SPACE_0_5 + widgets::metrics::TOGGLE_GROUP_GAP;
    let toggles_right = meter_area.min.x - space::SPACE_1;
    let toggles_left = toggles_right - toggles_w;
    let ty = body.center().y - th / 2.0;
    let toggles = std::array::from_fn(|i| {
        let x = toggles_left + ToggleKind::row_offset(i, tw, space::SPACE_0_5);
        Rect::from_min_size(pos2(x, ty), vec2(tw, th))
    });

    let text = Rect::from_min_max(
        pos2(
            color_bar.max.x + space::SPACE_1 + space::SPACE_1_5,
            body.min.y,
        ),
        pos2(toggles_left - space::SPACE_1, body.max.y),
    );
    HeaderLayout {
        body,
        separator,
        color_bar,
        text,
        toggles,
        meter,
        meter_area,
    }
}

/// Paints a track header in `rect` in `state` (hovered, selected, focused; pressed counts as
/// hovered + 1 step).
pub fn paint_track_header(painter: &Painter, rect: Rect, model: &HeaderModel<'_>, state: State) {
    let l = header_layout(rect);
    let dimmed = model.dimmed();

    px::fill(painter, l.body, 0.0, state::row_fill(2, state));
    px::fill(painter, l.separator, 0.0, color32(color::SURFACE_0));
    // Keyboard focus: an inset 1 px accent line. The colour bar and the rest of the content are
    // painted over it, as CSS paints children over an inset box shadow.
    if state.focused {
        px::inner_border(painter, l.body, color32(color::ACCENT));
    }
    let bar = if dimmed {
        model.palette.dim
    } else {
        model.palette.base
    };
    px::fill(painter, l.color_bar, 0.0, color32(bar));

    paint_texts(painter, &l, model);
    for (kind, r) in ToggleKind::ALL.into_iter().zip(l.toggles) {
        paint_toggle(painter, r, kind, model.is_on(kind), model.is_implied(kind));
    }
    let silent = MeterState::new();
    let levels = model.meter.unwrap_or(&silent);
    meter::paint(painter, l.meter, levels, &meter::Style::HEADER);
}

fn paint_texts(painter: &Painter, l: &HeaderLayout, model: &HeaderModel<'_>) {
    let name_style = typography::TRACK;
    let inst_style = typography::SMALL;
    let name_color = if model.dimmed() {
        color::TEXT_3
    } else {
        color::TEXT_1
    };
    let inst_color = if model.instrument_error {
        color::STATE_ERROR
    } else {
        color::TEXT_3
    };
    let block = name_style.line_height + metrics::HEADER_TEXT_GAP + inst_style.line_height;
    let ppp = painter.pixels_per_point();
    let top = px::snap(l.body.center().y - block / 2.0, ppp);
    let x = px::snap(l.text.min.x, ppp);
    let width = l.text.width().max(0.0);
    let clip = painter.with_clip_rect(l.text.intersect(painter.clip_rect()));
    let name = one_line(painter, model.name, &name_style, color32(name_color), width);
    clip.galley(pos2(x, top), name, Color32::PLACEHOLDER);
    let y = top + name_style.line_height + metrics::HEADER_TEXT_GAP;
    let mut text_x = x;
    if model.instrument_error {
        // "⚠ Plugin fehlt: Vital": the ⚠ as icon, centred on the line.
        let s = metrics::HEADER_WARNING_ICON;
        let icon = Rect::from_min_size(
            pos2(x, px::snap(y + (inst_style.line_height - s) / 2.0, ppp)),
            vec2(s, s),
        );
        super::paint_icon(
            &clip,
            icon,
            Icon::TriangleExclamationPoint,
            color32(inst_color),
        );
        text_x = px::snap(icon.max.x + space::SPACE_1, ppp);
    }
    let inst = one_line(
        painter,
        instrument_text(model),
        &inst_style,
        color32(inst_color),
        (l.text.max.x - text_x).max(0.0),
    );
    clip.galley(pos2(text_x, y), inst, Color32::PLACEHOLDER);
}

/// The instrument line without a leading ⚠ (drawn as icon when [`HeaderModel::instrument_error`]).
fn instrument_text<'a>(model: &HeaderModel<'a>) -> &'a str {
    super::strip_symbol(model.instrument, '⚠')
}

/// `text` laid out in `style` on one line of at most `width`, cut with "…".
pub(crate) fn one_line(
    painter: &Painter,
    text: &str,
    style: &TypeStyle,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = fonts::layout_job(text, style, color);
    job.wrap = TextWrapping {
        max_width: width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    painter.layout_job(job)
}

/// Paints a track toggle as a still picture in the look of the WP-20 widget
/// ([`toggle::look`] at rest): 18 × 16, `radius.s`, letter in Mono 10 semibold.
pub fn paint_toggle(painter: &Painter, rect: Rect, kind: ToggleKind, on: bool, implied: bool) {
    let look = toggle::look(kind, on, implied, State::REST);
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    px::fill(painter, r, radius::S, look.fill);
    if let Some(contour) = look.contour {
        painter.rect_stroke(
            r,
            theme::corner_radius(radius::S),
            Stroke::new(px::line_width(ppp), contour),
            StrokeKind::Inside,
        );
    }
    if toggle::paint_symbol(painter, r, kind, look.text) {
        return;
    }
    let style = typography::VALUE_S;
    let galley = painter.layout_job(LayoutJob::single_section(
        kind.letter().to_owned(),
        TextFormat {
            font_id: widgets::toggle_letter_font(),
            line_height: Some(style.line_height),
            color: look.text,
            ..Default::default()
        },
    ));
    painter.galley(
        widgets::text_pos_centered(r, &galley, ppp),
        galley,
        look.text,
    );
}

/// Accessible name and role of a header: `Spur Lead, pisynth`, `Spur Strings, Plugin fehlt:
/// Vital` (role row, selectable).
pub fn header_access(t: Lang, model: &HeaderModel<'_>, state: State) -> Access {
    let instrument = instrument_text(model);
    let name = if instrument.is_empty() {
        t.a11y_track(model.name)
    } else {
        t.a11y_track_instrument(model.name, instrument)
    };
    Access::selectable(Role::Row, name, state.selected)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect() -> Rect {
        Rect::from_min_size(
            pos2(10.0, 20.0),
            vec2(size::TRACK_HEADER, size::track::DEFAULT),
        )
    }

    #[test]
    fn layout_matches_the_design() {
        let l = header_layout(rect());
        assert_eq!(l.body.height(), 55.0);
        assert_eq!(l.separator.height(), 1.0);
        assert_eq!(l.color_bar.width(), 3.0);
        // Meter: padding 8 right, two channels 3 + 1 + 3, padding 6 left.
        assert_eq!(l.meter.max.x, 10.0 + 224.0 - 8.0);
        assert_eq!(l.meter.width(), 7.0);
        assert_eq!(l.meter_area.width(), 21.0);
        assert_eq!(l.meter.height(), 55.0 - 16.0);
        // Toggles: 18 × 16, gap 2, gap 4 before the meter.
        assert_eq!(l.toggles[3].max.x, l.meter_area.min.x - 4.0);
        assert_eq!(l.toggles[1].min.x - l.toggles[0].max.x, 2.0);
        // R and I stand apart from M and S by the extra group gap (ARCHITECTURE §13 item 8).
        assert_eq!(l.toggles[2].min.x - l.toggles[1].max.x, 2.0 + 4.0);
        assert_eq!(l.toggles[3].min.x - l.toggles[2].max.x, 2.0);
        for t in l.toggles {
            assert_eq!(t.size(), vec2(18.0, 16.0));
            assert_eq!(t.center().y, l.body.center().y);
        }
        // Text: bar 3 + gap 4 + padding 6; 104 − 4 (group gap) = 100 wide.
        assert_eq!(l.text.min.x, 10.0 + 13.0);
        assert_eq!(l.text.width(), 100.0);
    }

    #[test]
    fn toggle_states() {
        let p = TrackPalette::new(300);
        let mut m = HeaderModel::new("Pad", "stumm durch Solo", &p);
        m.implied_mute = true;
        assert!(m.is_implied(ToggleKind::Mute));
        assert!(!m.is_implied(ToggleKind::Solo));
        assert!(!m.is_on(ToggleKind::Mute));
        assert!(m.dimmed());
        m.mute = true;
        m.arm = true;
        assert!(m.is_on(ToggleKind::Mute) && m.is_on(ToggleKind::Arm));
        assert!(!m.is_on(ToggleKind::Monitor));
        // Armed with I off: I is implied (auto-monitoring); with I on, or unarmed, it is not.
        assert!(m.is_implied(ToggleKind::Monitor));
        m.monitor = true;
        assert!(!m.is_implied(ToggleKind::Monitor));
        m.monitor = false;
        m.arm = false;
        assert!(!m.is_implied(ToggleKind::Monitor));
    }

    #[test]
    fn access_names() {
        let p = TrackPalette::new(150);
        let m = HeaderModel::new("Lead", "pisynth", &p);
        let a = header_access(Lang::De, &m, State::SELECTED);
        assert_eq!(a.name, "Spur Lead, pisynth");
        assert_eq!(a.role, Role::Row);
        assert_eq!(a.selected, Some(true));
        let m = HeaderModel::new("MIDI 3", "", &p);
        assert_eq!(header_access(Lang::De, &m, State::REST).name, "Spur MIDI 3");
        let mut m = HeaderModel::new("Strings", "⚠ Plugin fehlt: Vital", &p);
        m.instrument_error = true;
        assert_eq!(
            header_access(Lang::De, &m, State::REST).name,
            "Spur Strings, Plugin fehlt: Vital"
        );
    }
}
