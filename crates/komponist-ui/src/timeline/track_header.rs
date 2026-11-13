//! Track header ("Spur-Header", design system `hdr`, `headers`): 224 × 56 (height 32–160), colour
//! bar, name and instrument, the toggles M S R I and a two-channel mini meter.
//!
//! [`paint_track_header`] paints everything, the toggles statically ([`paint_toggle`], the look of
//! `tgl()` in the design data). The interactive toggle widget comes from WP-20; the arrangement
//! (WP-40) puts those widgets on top of the painted ones at [`HeaderLayout::toggles`], so the
//! static ones are only seen where no widget is placed (gallery, snapshots, thumbnails).
//!
//! States ([`State`]): rest `surface.2`, hover `surface.3`, selected `accent.surface`, keyboard
//! focus an inset 1 px accent line. Muted (M or implied by solo elsewhere): colour bar `dim`,
//! name `text.3`. Implied mute shows M with a `text.3` contour on `surface.2`.

use egui::text::TextWrapping;
use egui::{Align2, Color32, Painter, Rect, Role, Stroke, StrokeKind, pos2, vec2};
use komponist_tokens::egui_support::color32;
use komponist_tokens::font::Family;
use komponist_tokens::typography::{self, TypeStyle};
use komponist_tokens::{Icon, TrackPalette, color, line, radius, size, space};

use super::{Access, metrics};
use crate::state::{self, State};
use crate::{fonts, px, theme};

/// One of the four track toggles, in header order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TrackToggle {
    /// M: mute.
    Mute,
    /// S: solo.
    Solo,
    /// R: record arm.
    Arm,
    /// I: input monitoring.
    Monitor,
}

impl TrackToggle {
    /// All toggles in header order: M S R I.
    pub const ALL: [Self; 4] = [Self::Mute, Self::Solo, Self::Arm, Self::Monitor];

    /// The letter on the toggle.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Mute => "M",
            Self::Solo => "S",
            Self::Arm => "R",
            Self::Monitor => "I",
        }
    }

    /// The tooltip text of the prototype (`arrView` `btns.tip`), also the accessible name of the
    /// interactive toggle.
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Mute => "Mute · M",
            Self::Solo => "Solo · S · Alt-Klick exklusiv",
            Self::Arm => "Record-Arm · Alt-Klick exklusiv",
            Self::Monitor => "Monitoring",
        }
    }

    /// Fill and letter colour when on (`tgl().onC`).
    fn on_colors(self) -> (Color32, Color32) {
        let (fill, text) = match self {
            Self::Mute => (color::STATE_MUTE, color::SURFACE_0),
            Self::Solo => (color::STATE_SOLO, color::SURFACE_0),
            Self::Arm => (color::STATE_RECORD, color::TEXT_1),
            Self::Monitor => (color::ACCENT, color::ACCENT_INK),
        };
        (color32(fill), color32(text))
    }
}

/// How a statically painted toggle looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleLook {
    /// Off: `surface.4`, letter `text.2`.
    Off,
    /// On: the toggle's state colour.
    On,
    /// Muted implicitly by solo on another track (M only): `surface.2` with a 1 px `text.3`
    /// contour. Not a click target.
    Implied,
}

/// One meter channel of the header as fractions of the meter height (0 = bottom, 1 = top), already
/// mapped from dB by the caller.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterLevel {
    /// Current level.
    pub level: f32,
    /// Peak hold; no line at 0.
    pub peak: f32,
    /// Clip latch: red mark at the top.
    pub clip: bool,
}

impl MeterLevel {
    /// A channel at `level` with peak hold `peak`, not clipping.
    pub const fn new(level: f32, peak: f32) -> Self {
        Self {
            level,
            peak,
            clip: false,
        }
    }
}

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
    /// Muted because another track is soloed (M shows [`ToggleLook::Implied`] unless on).
    pub implied_mute: bool,
    /// The two meter channels.
    pub meter: [MeterLevel; 2],
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
            meter: [MeterLevel::default(); 2],
        }
    }

    /// Whether the track is silent (muted or implied muted): dim colour bar, name in `text.3`.
    pub fn dimmed(&self) -> bool {
        self.mute || self.implied_mute
    }

    /// Whether `toggle` is on.
    pub fn is_on(&self, toggle: TrackToggle) -> bool {
        match toggle {
            TrackToggle::Mute => self.mute,
            TrackToggle::Solo => self.solo,
            TrackToggle::Arm => self.arm,
            TrackToggle::Monitor => self.monitor,
        }
    }

    /// The look of `toggle` in this header.
    pub fn toggle_look(&self, toggle: TrackToggle) -> ToggleLook {
        if self.is_on(toggle) {
            ToggleLook::On
        } else if toggle == TrackToggle::Mute && self.implied_mute {
            ToggleLook::Implied
        } else {
            ToggleLook::Off
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
    /// The toggles M S R I (`size.toggle`), in [`TrackToggle::ALL`] order. WP-40 places the
    /// interactive WP-20 toggles exactly here.
    pub toggles: [Rect; 4],
    /// The two meter channels (each [`metrics::HEADER_METER_BAR`] wide).
    pub meter: [Rect; 2],
    /// The meter with its padding: the click target that resets the clip latch.
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

    // Meter: padding 8 8 8 6 around two channels with a 1 px gap.
    let meters_w = 2.0 * metrics::HEADER_METER_BAR + metrics::HEADER_METER_GAP;
    let meter_area = Rect::from_min_max(
        pos2(
            body.max.x - space::SPACE_2 - meters_w - space::SPACE_1_5,
            body.min.y,
        ),
        body.max,
    );
    let meter_y = egui::Rangef::new(body.min.y + space::SPACE_2, body.max.y - space::SPACE_2);
    let m0 = meter_area.min.x + space::SPACE_1_5;
    let meter = [
        Rect::from_x_y_ranges(m0..=m0 + metrics::HEADER_METER_BAR, meter_y),
        Rect::from_x_y_ranges(
            m0 + metrics::HEADER_METER_BAR + metrics::HEADER_METER_GAP..=m0 + meters_w,
            meter_y,
        ),
    ];

    // Toggles: 4 × size.toggle with space.0.5 between, space.1 before the meter.
    let [tw, th] = size::TOGGLE;
    let toggles_w = 4.0 * tw + 3.0 * space::SPACE_0_5;
    let toggles_right = meter_area.min.x - space::SPACE_1;
    let toggles_left = toggles_right - toggles_w;
    let ty = body.center().y - th / 2.0;
    let toggles = std::array::from_fn(|i| {
        let x = toggles_left + i as f32 * (tw + space::SPACE_0_5);
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
    let bar = if dimmed {
        model.palette.dim
    } else {
        model.palette.base
    };
    px::fill(painter, l.color_bar, 0.0, color32(bar));

    paint_texts(painter, &l, model);
    for (toggle, r) in TrackToggle::ALL.into_iter().zip(l.toggles) {
        paint_toggle(painter, r, toggle, model.toggle_look(toggle));
    }
    for (channel, r) in model.meter.iter().zip(l.meter) {
        paint_meter_channel(painter, r, *channel);
    }
    if state.focused {
        px::inner_border(painter, l.body, color32(color::ACCENT));
    }
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

/// Paints a track toggle statically (`tgl()`): 18 × 16, `radius.s`, letter in Mono 10.
///
/// The design sets the letter in Plex Mono 600, which komponist does not bundle; the medium (500)
/// face is the closest.
pub fn paint_toggle(painter: &Painter, rect: Rect, toggle: TrackToggle, look: ToggleLook) {
    let (fill, text) = match look {
        ToggleLook::Off => (color32(color::SURFACE_4), color32(color::TEXT_2)),
        ToggleLook::On => toggle.on_colors(),
        ToggleLook::Implied => (color32(color::SURFACE_2), color32(color::TEXT_2)),
    };
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    painter.rect_filled(r, theme::corner_radius(radius::S), fill);
    if look == ToggleLook::Implied {
        painter.rect_stroke(
            r,
            theme::corner_radius(radius::S),
            Stroke::new(px::line_width(ppp), color32(color::TEXT_3)),
            StrokeKind::Inside,
        );
    }
    let font = egui::FontId::new(typography::VALUE_S.size, fonts::family(Family::Mono, 500));
    let galley = painter.layout_no_wrap(toggle.letter().to_owned(), font, text);
    let pos = Align2::CENTER_CENTER
        .anchor_size(r.center(), galley.size())
        .min;
    painter.galley(
        pos2(px::snap(pos.x, ppp), px::snap(pos.y, ppp)),
        galley,
        text,
    );
}

/// Paints one meter channel (`meterH`): `meter.track`, level in `meter.ok` up to 80 % and
/// `meter.loud` above, a 1 px peak line (`meter.loud` above 80 %), the clip mark at the top.
pub fn paint_meter_channel(painter: &Painter, rect: Rect, level: MeterLevel) {
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    painter.rect_filled(r, 0.0, color32(color::METER_TRACK));
    let h = r.height();
    let y_at = |fraction: f32| px::snap(r.max.y - fraction.clamp(0.0, 1.0) * h, ppp);
    let loud_from = metrics::HEADER_METER_LOUD_FROM;
    let l = level.level.clamp(0.0, 1.0);
    if l > 0.0 {
        let ok_top = y_at(l.min(loud_from));
        painter.rect_filled(
            Rect::from_min_max(pos2(r.min.x, ok_top), r.max),
            0.0,
            color32(color::METER_OK),
        );
        if l > loud_from {
            painter.rect_filled(
                Rect::from_min_max(pos2(r.min.x, y_at(l)), pos2(r.max.x, y_at(loud_from))),
                0.0,
                color32(color::METER_LOUD),
            );
        }
    }
    if level.peak > 0.0 {
        let w = px::line_width(ppp);
        // CSS `bottom: peak·100 %`: the line's bottom edge sits at the peak, kept inside the bar.
        let bottom = y_at(level.peak).max(r.min.y + w);
        let c = if level.peak > loud_from {
            color::METER_LOUD
        } else {
            color::METER_OK
        };
        painter.rect_filled(
            Rect::from_min_max(pos2(r.min.x, bottom - w), pos2(r.max.x, bottom)),
            0.0,
            color32(c),
        );
    }
    if level.clip {
        let mark = px::snap(r.min.y + metrics::HEADER_METER_CLIP_MARK, ppp);
        painter.rect_filled(
            Rect::from_min_max(r.min, pos2(r.max.x, mark)),
            0.0,
            color32(color::METER_CLIP),
        );
    }
}

/// Accessible name and role of a header: `Spur Lead, pisynth`, `Spur Strings, Plugin fehlt:
/// Vital` (role row, selectable).
pub fn header_access(model: &HeaderModel<'_>, state: State) -> Access {
    let instrument = instrument_text(model);
    let name = if instrument.is_empty() {
        format!("Spur {}", model.name)
    } else {
        format!("Spur {}, {instrument}", model.name)
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
        assert_eq!(l.meter[1].max.x, 10.0 + 224.0 - 8.0);
        assert_eq!(l.meter[0].min.x, 10.0 + 224.0 - 8.0 - 7.0);
        assert_eq!(l.meter_area.width(), 21.0);
        assert_eq!(l.meter[0].height(), 55.0 - 16.0);
        // Toggles: 18 × 16, gap 2, gap 4 before the meter.
        assert_eq!(l.toggles[3].max.x, l.meter_area.min.x - 4.0);
        assert_eq!(l.toggles[1].min.x - l.toggles[0].max.x, 2.0);
        for t in l.toggles {
            assert_eq!(t.size(), vec2(18.0, 16.0));
            assert_eq!(t.center().y, l.body.center().y);
        }
        // Text: bar 3 + gap 4 + padding 6; 110 − 6 = 104 wide.
        assert_eq!(l.text.min.x, 10.0 + 13.0);
        assert_eq!(
            l.text.width(),
            224.0 - 3.0 - 4.0 - 6.0 - 4.0 - 78.0 - 4.0 - 21.0
        );
    }

    #[test]
    fn toggle_looks() {
        let p = TrackPalette::new(300);
        let mut m = HeaderModel::new("Pad", "stumm durch Solo", &p);
        m.implied_mute = true;
        assert_eq!(m.toggle_look(TrackToggle::Mute), ToggleLook::Implied);
        assert_eq!(m.toggle_look(TrackToggle::Solo), ToggleLook::Off);
        assert!(m.dimmed());
        m.mute = true;
        assert_eq!(m.toggle_look(TrackToggle::Mute), ToggleLook::On);
        let letters: String = TrackToggle::ALL.iter().map(|t| t.letter()).collect();
        assert_eq!(letters, "MSRI");
    }

    #[test]
    fn access_names() {
        let p = TrackPalette::new(150);
        let m = HeaderModel::new("Lead", "pisynth", &p);
        let a = header_access(&m, State::SELECTED);
        assert_eq!(a.name, "Spur Lead, pisynth");
        assert_eq!(a.role, Role::Row);
        assert_eq!(a.selected, Some(true));
        let m = HeaderModel::new("MIDI 3", "", &p);
        assert_eq!(header_access(&m, State::REST).name, "Spur MIDI 3");
        let mut m = HeaderModel::new("Strings", "⚠ Plugin fehlt: Vital", &p);
        m.instrument_error = true;
        assert_eq!(
            header_access(&m, State::REST).name,
            "Spur Strings, Plugin fehlt: Vital"
        );
    }
}
