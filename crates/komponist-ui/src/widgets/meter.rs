//! The level meter ("Pegelanzeige"; design system `meterV`, `meterH`, `meters`, `ix.fader`).
//!
//! Two bars (left, right), 4 px wide and 1 px apart, on `meter.track`; the clip LED above them.
//! The level fills each bar in zones: `meter.ok` up to −6 dBFS, `meter.loud` up to 0 dBFS,
//! `meter.clip` above. A 1 px line marks the held peak, in `meter.peak` below −6 dBFS and in the
//! zone colour above. The scale is the fader taper ([`super::fader::db_to_p`]), so a meter next
//! to a fader reads on the fader's scale (−6 dBFS at 70 % of the height, as in the design).
//!
//! Ballistics (`ix.fader`, "Peak-Hold: hält 1,5 s, fällt dann mit 20 dB/s"):
//! [`klang_dsp::meter::PeakMeter`] per channel with [`HOLD_S`] and [`FALL_DB_PER_S`]; the level
//! falls at the same rate. "Clip-Anzeige: bleibt rot bis Klick auf Anzeige oder Wert": the clip
//! latch holds until a click on the meter resets it ([`MeterState::reset`]).
//!
//! **State:** the caller owns a [`MeterState`] per meter (e.g. next to the track's UI state) and
//! passes it with the linear peaks since the last frame to [`Meter`]; the widget feeds the peaks
//! into the ballistics with the time since its last frame (egui's input time). Keeping the state
//! with the caller, not in egui memory, lets a track keep its peak hold and clip latch while its
//! meter is scrolled out of view, and lets two views of one track (header and mixer) share it.
//!
//! The header variant (`meterH`, "Pegel 2 × 3"): bars 3 px wide, no LED; the clip shows as a
//! 3 px block at the top of the bar ([`Meter::header`]). Horizontal meters fill left to right,
//! the LED at the right end.

use std::time::Duration;

use egui::{
    Color32, Painter, Rect, Response, Role, Sense, Ui, Vec2, Widget, WidgetInfo, pos2, vec2,
};
use klang_dsp::meter::PeakMeter;
use klang_dsp::units::{db_to_gain, gain_to_db};
use komponist_model::format::fmt_db;
use komponist_model::project::{VOLUME_DB_MAX, VOLUME_DB_MIN};
use komponist_tokens::color;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{space, typography};

use super::fader;
use super::metrics::meter as metrics;
use super::number_field::{caption, column, is_still, sense, text_width, value_label};
use crate::Lang;
use crate::px;

/// Peak hold time in seconds ("hält 1,5 s").
pub const HOLD_S: f32 = 1.5;
/// Fall rate of the held peak and the level after the hold ("fällt dann mit 20 dB/s").
pub const FALL_DB_PER_S: f32 = 20.0;
/// Upper end of the ok zone ("meter.ok: bis −6 dBFS").
pub const LOUD_FROM_DB: f32 = -6.0;
/// Upper end of the loud zone ("meter.loud: −6 bis 0 dBFS"); above is clip.
pub const CLIP_FROM_DB: f32 = 0.0;

/// While the meter falls, it asks for a repaint after this many seconds (60 Hz).
pub const FRAME_S: f32 = 1.0 / 60.0;

/// The colour zone of a level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
    /// ≤ −6 dBFS.
    Ok,
    /// −6 … 0 dBFS.
    Loud,
    /// Above 0 dBFS.
    Clip,
}

impl Zone {
    /// The zone of `db` (dBFS).
    pub fn of(db: f32) -> Self {
        if db > CLIP_FROM_DB {
            Self::Clip
        } else if db > LOUD_FROM_DB {
            Self::Loud
        } else {
            Self::Ok
        }
    }

    /// The bar colour of the zone.
    pub fn color(self) -> Color32 {
        color32(match self {
            Self::Ok => color::METER_OK,
            Self::Loud => color::METER_LOUD,
            Self::Clip => color::METER_CLIP,
        })
    }

    /// The colour of a peak-hold line in the zone: `meter.peak` in the ok zone ("Peak-Hold-Linie
    /// unter −6 dBFS"), the zone colour above.
    pub fn peak_color(self) -> Color32 {
        match self {
            Self::Ok => color32(color::METER_PEAK),
            z => z.color(),
        }
    }
}

/// Where `db` (dBFS) lies on the meter, 0 at the bottom (−70 dB and below) … 1 at the top
/// (+6 dB): the fader taper upside down.
pub fn fraction(db: f32) -> f32 {
    1.0 - fader::db_to_p(db)
}

/// The ballistics of one stereo meter: a [`PeakMeter`] per channel and the time of the last
/// update. Owned by the caller (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct MeterState {
    channels: [PeakMeter; 2],
    last_time: Option<f64>,
}

impl Default for MeterState {
    fn default() -> Self {
        Self::new()
    }
}

impl MeterState {
    /// A silent meter with the design's ballistics.
    pub fn new() -> Self {
        Self {
            channels: [PeakMeter::new(HOLD_S, FALL_DB_PER_S); 2],
            last_time: None,
        }
    }

    /// Lets `dt` seconds pass, then takes the linear `peaks` (left, right) since the last update.
    pub fn update(&mut self, peaks: [f32; 2], dt: f32) {
        for (m, p) in self.channels.iter_mut().zip(peaks) {
            m.update(p, dt);
        }
    }

    /// [`Self::update`] at time `now` (seconds, monotonic): the time since the last call passes
    /// first (none on the first call).
    pub fn tick(&mut self, peaks: [f32; 2], now: f64) {
        let dt = self.last_time.map_or(0.0, |t| (now - t).max(0.0) as f32);
        self.last_time = Some(now);
        self.update(peaks, dt);
    }

    /// Clears the clip latch and the held peaks (click on the meter).
    pub fn reset(&mut self) {
        for m in &mut self.channels {
            m.reset();
        }
    }

    /// The level of channel `ch` (0 left, 1 right) in dBFS; −120 for silence.
    pub fn level_db(&self, ch: usize) -> f32 {
        self.channels[ch].level_db()
    }

    /// The held peak of channel `ch` in dBFS.
    pub fn held_db(&self, ch: usize) -> f32 {
        self.channels[ch].held_db()
    }

    /// The higher held peak of both channels, for the peak value under the meter.
    pub fn peak_db(&self) -> f32 {
        self.held_db(0).max(self.held_db(1))
    }

    /// Whether channel `ch` clipped since the last reset.
    pub fn channel_clipped(&self, ch: usize) -> bool {
        self.channels[ch].clipped()
    }

    /// Whether any channel clipped since the last reset (the LED).
    pub fn clipped(&self) -> bool {
        self.channels.iter().any(PeakMeter::clipped)
    }

    /// `true` while something is visible and still falling.
    pub fn is_active(&self) -> bool {
        self.channels
            .iter()
            .any(|m| m.held_db() > VOLUME_DB_MIN || m.level_db() > VOLUME_DB_MIN)
    }

    /// A meter frozen at `levels` and held peaks `held` (dBFS, held ≥ level, at most 30 dB
    /// apart), for still pictures and tests: each channel takes the held peak, then falls to the
    /// level within the hold time. A held peak above 0 dBFS has clipped.
    pub fn frozen(levels: [f32; 2], held: [f32; 2]) -> Self {
        let mut s = Self::new();
        for ((m, l), h) in s.channels.iter_mut().zip(levels).zip(held) {
            if h <= gain_to_db(0.0) {
                continue;
            }
            m.update(db_to_gain(h), 0.0);
            if l < h {
                m.update(0.0, (h - l) / FALL_DB_PER_S);
            }
        }
        s
    }
}

/// Orientation of a meter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    /// Bars side by side, filling upwards, LED on top.
    #[default]
    Vertical,
    /// Bars stacked, filling to the right, LED at the right end.
    Horizontal,
}

/// How a meter is drawn: orientation, bar width, LED or clip blocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    /// Orientation.
    pub orientation: Orientation,
    /// Width of one bar across the meter.
    pub bar_width: f32,
    /// Clip LED at the end (mixer); without it, a clip block at the top of each bar (header).
    pub led: bool,
}

impl Style {
    /// The mixer meter: vertical, bars 4, LED.
    pub const MIXER: Self = Self {
        orientation: Orientation::Vertical,
        bar_width: metrics::BAR_WIDTH,
        led: true,
    };

    /// The track header meter: vertical, bars 3, clip blocks.
    pub const HEADER: Self = Self {
        orientation: Orientation::Vertical,
        bar_width: metrics::HEADER_BAR_WIDTH,
        led: false,
    };

    /// Size across the bars: two bars and the gap.
    pub fn thickness(&self) -> f32 {
        2.0 * self.bar_width + metrics::BAR_GAP
    }

    /// The size of a meter `length` long.
    pub fn size(&self, length: f32) -> Vec2 {
        match self.orientation {
            Orientation::Vertical => vec2(self.thickness(), length),
            Orientation::Horizontal => vec2(length, self.thickness()),
        }
    }
}

/// The part of `bar` from `f0` to `f1` (0 = bottom or left, 1 = top or right).
fn span(bar: Rect, orientation: Orientation, f0: f32, f1: f32) -> Rect {
    let (f0, f1) = (f0.clamp(0.0, 1.0), f1.clamp(0.0, 1.0));
    match orientation {
        Orientation::Vertical => Rect::from_x_y_ranges(
            bar.x_range(),
            bar.bottom() - f1 * bar.height()..=bar.bottom() - f0 * bar.height(),
        ),
        Orientation::Horizontal => Rect::from_x_y_ranges(
            bar.left() + f0 * bar.width()..=bar.left() + f1 * bar.width(),
            bar.y_range(),
        ),
    }
}

/// Paints the meter of `state` into `rect` in `style`.
pub fn paint(painter: &Painter, rect: Rect, state: &MeterState, style: &Style) {
    let ppp = painter.pixels_per_point();
    let o = style.orientation;
    let along = |r: Rect, from_end: f32, len: f32| match o {
        // `from_end`: distance from the LED end (top / right).
        Orientation::Vertical => {
            Rect::from_x_y_ranges(r.x_range(), r.top() + from_end..=r.top() + from_end + len)
        }
        Orientation::Horizontal => Rect::from_x_y_ranges(
            r.right() - from_end - len..=r.right() - from_end,
            r.y_range(),
        ),
    };
    let mut bars_rect = rect;
    if style.led {
        let led_len = metrics::LED_HEIGHT;
        let led = along(rect, 0.0, led_len);
        let c = if state.clipped() {
            color32(color::METER_CLIP)
        } else {
            color32(color::SURFACE_3)
        };
        px::fill(painter, led, metrics::BAR_RADIUS, c);
        let cut = led_len + metrics::LED_GAP;
        bars_rect = match o {
            Orientation::Vertical => {
                Rect::from_min_max(pos2(rect.left(), rect.top() + cut), rect.max)
            }
            Orientation::Horizontal => {
                Rect::from_min_max(rect.min, pos2(rect.right() - cut, rect.bottom()))
            }
        };
    }
    let (f_loud, f_clip) = (fraction(LOUD_FROM_DB), fraction(CLIP_FROM_DB));
    let line = px::line_width(ppp);
    for ch in 0..2 {
        let offset = ch as f32 * (style.bar_width + metrics::BAR_GAP);
        let bar = match o {
            Orientation::Vertical => Rect::from_min_size(
                pos2(bars_rect.left() + offset, bars_rect.top()),
                vec2(style.bar_width, bars_rect.height()),
            ),
            Orientation::Horizontal => Rect::from_min_size(
                pos2(bars_rect.left(), bars_rect.top() + offset),
                vec2(bars_rect.width(), style.bar_width),
            ),
        };
        px::fill(
            painter,
            bar,
            metrics::BAR_RADIUS,
            color32(color::METER_TRACK),
        );
        let fl = fraction(state.level_db(ch));
        let zones = [
            (0.0, f_loud, Zone::Ok),
            (f_loud, f_clip, Zone::Loud),
            (f_clip, 1.0, Zone::Clip),
        ];
        for (f0, f1, zone) in zones {
            if fl > f0 {
                px::fill(painter, span(bar, o, f0, fl.min(f1)), 0.0, zone.color());
            }
        }
        let held = state.held_db(ch);
        let fh = fraction(held);
        if fh > 0.0 {
            // The line's lower (left) edge sits at the held level (`bottom: pk*100%`), kept inside
            // the bar.
            let len = match o {
                Orientation::Vertical => bar.height(),
                Orientation::Horizontal => bar.width(),
            };
            let f1 = (fh + line / len).min(1.0);
            let peak = span(bar, o, f1 - line / len, f1);
            px::fill(painter, peak, 0.0, Zone::of(held).peak_color());
        }
        if !style.led && state.channel_clipped(ch) {
            px::fill(
                painter,
                along(bar, 0.0, metrics::CLIP_BLOCK),
                0.0,
                color32(color::METER_CLIP),
            );
        }
    }
}

/// A stereo level meter: `ui.add(Meter::new(&mut track_ui.meter, [peak_l, peak_r], "Pegel Lead"))`.
///
/// `peaks` are the linear peaks since the last frame (e.g. from the engine's track meter). A
/// click resets the clip latch and the held peaks and is reported (`clicked()`).
pub struct Meter<'a> {
    state: &'a mut MeterState,
    peaks: [f32; 2],
    name: String,
    style: Style,
    length: f32,
}

impl<'a> Meter<'a> {
    /// The mixer meter ([`Style::MIXER`]), [`metrics::LENGTH`] long, with the German accessible
    /// `name` ("Pegel Lead").
    pub fn new(state: &'a mut MeterState, peaks: [f32; 2], name: impl Into<String>) -> Self {
        Self {
            state,
            peaks,
            name: name.into(),
            style: Style::MIXER,
            length: metrics::LENGTH,
        }
    }

    /// Length along the bars, LED included.
    #[must_use]
    pub fn length(mut self, length: f32) -> Self {
        self.length = length;
        self
    }

    /// The style.
    #[must_use]
    pub fn style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// The track header meter ([`Style::HEADER`]).
    #[must_use]
    pub fn header(self) -> Self {
        self.style(Style::HEADER)
    }

    /// Horizontal.
    #[must_use]
    pub fn horizontal(mut self) -> Self {
        self.style.orientation = Orientation::Horizontal;
        self
    }
}

/// The value AccessKit reads for a meter: the held peak ("−14.2 dB"), with ", Clip" when clipped.
pub fn value_text(t: Lang, state: &MeterState) -> String {
    if state.clipped() {
        t.ui_meter_value_clipped(fmt_db(state.peak_db()))
    } else {
        format!("{} dB", fmt_db(state.peak_db()))
    }
}

impl Widget for Meter<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = self.style.size(self.length);
        let (rect, response) = ui.allocate_exact_size(size, sense(ui, Sense::CLICK));
        if !is_still(ui) {
            let now = ui.input(|i| i.time);
            self.state.tick(self.peaks, now);
            if response.clicked() {
                self.state.reset();
            }
            if self.state.is_active() {
                ui.ctx()
                    .request_repaint_after(Duration::from_secs_f32(FRAME_S));
            }
        }
        paint(ui.painter(), rect, self.state, &self.style);
        let peak = self.state.peak_db().clamp(VOLUME_DB_MIN, VOLUME_DB_MAX);
        let text = value_text(crate::lang(ui.ctx()), self.state);
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::Meter, true, &self.name);
            info.value = Some(f64::from(peak));
            info.current_text_value = Some(text.clone());
            info
        });
        response.ctx.accesskit_node_builder(response.id, |n| {
            n.set_min_numeric_value(f64::from(VOLUME_DB_MIN));
            n.set_max_numeric_value(f64::from(VOLUME_DB_MAX));
        });
        response
    }
}

// ---------------------------------------------------------------------------------------------
// Gallery

/// The samples of the design (`meters`): label, levels, held peaks (dBFS). The design gives
/// heights; these are levels that land close to them on the taper, with its peak values.
pub const SAMPLES: [(&str, [f32; 2], [f32; 2]); 4] = [
    ("stille", [-120.0, -120.0], [-120.0, -120.0]),
    ("normal", [-16.2, -17.4], [-14.2, -14.6]),
    ("laut", [-3.0, -3.6], [-2.1, -2.4]),
    ("clipping", [0.8, 0.5], [0.8, 0.6]),
];

/// One still meter (mixer style, [`metrics::LENGTH`]): not interactive; AccessKit sees an
/// image named `name`.
pub fn still(ui: &mut Ui, name: &str, state: &MeterState) -> Response {
    let (rect, response) =
        ui.allocate_exact_size(Style::MIXER.size(metrics::LENGTH), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Image, false, name));
    paint(ui.painter(), rect, state, &Style::MIXER);
    response
}

/// The width of one sample column: meter, peak value, label.
fn sample_width(ui: &Ui, label: &str, state: &MeterState) -> f32 {
    Style::MIXER
        .thickness()
        .max(text_width(
            ui,
            &fmt_db(state.peak_db()),
            &typography::VALUE_M,
        ))
        .max(text_width(ui, label, &typography::VALUE_S))
}

/// The width of the meter samples with `gap` between them.
pub(crate) fn gallery_width(ui: &Ui, gap: f32) -> f32 {
    let w: f32 = SAMPLES
        .iter()
        .map(|(label, levels, held)| sample_width(ui, label, &MeterState::frozen(*levels, *held)))
        .sum();
    w + gap * (SAMPLES.len() - 1) as f32
}

/// The meter samples of the "Fader und Pegelanzeige" card, `top` below the row's top: meter,
/// peak value (`meter.clip` when clipped), label.
pub fn gallery_samples(ui: &mut Ui, top: f32) {
    for (label, levels, held) in SAMPLES {
        let state = MeterState::frozen(levels, held);
        let width = sample_width(ui, label, &state);
        column(ui, width, egui::Align::Center, space::SPACE_2, |ui| {
            if top > 0.0 {
                ui.allocate_exact_size(vec2(width, top), Sense::hover());
            }
            still(ui, &format!("Pegel · {label}"), &state);
            let c = if state.clipped() {
                color::METER_CLIP
            } else {
                color::TEXT_1
            };
            value_label(ui, &fmt_db(state.peak_db()), c);
            caption(ui, label);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn zones() {
        assert_eq!(Zone::of(-20.0), Zone::Ok);
        assert_eq!(Zone::of(-6.0), Zone::Ok);
        assert_eq!(Zone::of(-5.9), Zone::Loud);
        assert_eq!(Zone::of(0.0), Zone::Loud);
        assert_eq!(Zone::of(0.1), Zone::Clip);
        assert_eq!(Zone::Ok.peak_color(), color32(color::METER_PEAK));
        assert_eq!(Zone::Loud.peak_color(), color32(color::METER_LOUD));
        assert_eq!(Zone::Clip.color(), color32(color::METER_CLIP));
    }

    #[test]
    fn scale_is_the_fader_taper() {
        // −6 dBFS at 70 % of the height, as the design's meters (`Math.min(l, .7)`).
        assert!(near(fraction(-6.0), 0.7));
        assert!(near(fraction(0.0), 0.86));
        assert_eq!(fraction(6.0), 1.0);
        assert_eq!(fraction(-120.0), 0.0);
    }

    #[test]
    fn ballistics_hold_fall_and_latch() {
        let mut s = MeterState::new();
        assert!(!s.is_active());
        s.tick([db_to_gain(-6.0), db_to_gain(-12.0)], 10.0);
        assert!(near(s.level_db(0), -6.0) && near(s.held_db(1), -12.0));
        // 1 s later: the level fell 20 dB, the held peak waits.
        s.tick([0.0, 0.0], 11.0);
        assert!(near(s.level_db(0), -26.0));
        assert!(near(s.held_db(0), -6.0));
        // 1 s more: 0.5 s past the hold, the held peak fell 10 dB.
        s.tick([0.0, 0.0], 12.0);
        assert!(near(s.held_db(0), -16.0));
        assert!(!s.clipped());
        s.tick([1.2, 0.5], 12.1);
        assert!(s.clipped() && s.channel_clipped(0) && !s.channel_clipped(1));
        // The clip latch holds until the reset.
        s.tick([0.0, 0.0], 30.0);
        assert!(s.clipped());
        s.reset();
        assert!(!s.clipped());
        assert!(!s.is_active());
        assert_eq!(s.peak_db(), -120.0);
    }

    #[test]
    fn frozen_states() {
        let s = MeterState::frozen([-16.2, -17.4], [-14.2, -14.6]);
        assert!(near(s.level_db(0), -16.2) && near(s.level_db(1), -17.4));
        assert!(near(s.held_db(0), -14.2) && near(s.peak_db(), -14.2));
        assert!(!s.clipped());
        assert!(MeterState::frozen([0.8, 0.5], [0.8, 0.6]).clipped());
        assert!(!MeterState::frozen([-120.0; 2], [-120.0; 2]).is_active());
        assert_eq!(
            value_text(Lang::De, &MeterState::frozen([0.8, 0.5], [0.8, 0.6])),
            "+0.8 dB, Clip"
        );
        assert_eq!(
            value_text(Lang::De, &MeterState::new()),
            "\u{2212}\u{221e} dB"
        );
    }

    #[test]
    fn sizes() {
        assert_eq!(Style::MIXER.size(124.0), vec2(9.0, 124.0));
        assert_eq!(Style::HEADER.thickness(), 7.0);
        let h = Style {
            orientation: Orientation::Horizontal,
            ..Style::MIXER
        };
        assert_eq!(h.size(100.0), vec2(100.0, 9.0));
        let bar = Rect::from_min_size(pos2(0.0, 0.0), vec2(4.0, 100.0));
        assert_eq!(
            span(bar, Orientation::Vertical, 0.0, 0.25),
            Rect::from_min_max(pos2(0.0, 75.0), pos2(4.0, 100.0))
        );
        let bar = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 4.0));
        assert_eq!(
            span(bar, Orientation::Horizontal, 0.5, 1.0),
            Rect::from_min_max(pos2(50.0, 0.0), pos2(100.0, 4.0))
        );
    }
}
