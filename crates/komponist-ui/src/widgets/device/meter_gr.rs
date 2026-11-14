//! `meter_gr` (1 × 3, `W_gr`): three bars In · GR · Out on a `surface.1` display. In and Out grow
//! from the bottom in `meter.ok`, above 75 % in `meter.loud`; GR hangs from the top in
//! `meter.loud`. Under the bars the read-out (`type.value.s`): the largest reduction of the last
//! 1.5 s ([`GrHold`]); a click resets it.
//!
//! AccessKit: role `Meter` named „Pegel und Gain Reduction“ with the read-out as value text; a
//! click (or the AccessKit click action) resets.

use std::collections::VecDeque;

use egui::{Painter, Rect, Response, Role, Sense, Ui, Widget, pos2, vec2};
use komponist_model::format::MINUS;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, widget};

use super::{LABEL, VALUE, cell_height, cell_width, paint_centered};
use crate::px;
use crate::widgets::metrics::device::meter_gr as metrics;
use crate::widgets::{accesskit, sense};

/// The bar names, left to right.
pub const NAMES: [&str; 3] = ["In", "GR", "Out"];
/// The accessible name.
pub const ACCESSIBLE_NAME: &str = "Pegel und Gain Reduction";

/// The largest gain reduction of the last [`metrics::HOLD_SECONDS`] seconds.
#[derive(Clone, Debug, Default)]
pub struct GrHold {
    samples: VecDeque<(f64, f32)>,
}

impl GrHold {
    /// An empty hold.
    pub fn new() -> Self {
        Self::default()
    }

    /// Notes a reduction of `db` (positive = reduced by that many dB) at time `now` (seconds).
    pub fn push(&mut self, now: f64, db: f32) {
        self.samples.push_back((now, db.max(0.0)));
        self.expire(now);
    }

    fn expire(&mut self, now: f64) {
        while self
            .samples
            .front()
            .is_some_and(|&(t, _)| now - t > metrics::HOLD_SECONDS)
        {
            self.samples.pop_front();
        }
    }

    /// The largest reduction in the window ending at `now`, 0 when there is none.
    pub fn max(&mut self, now: f64) -> f32 {
        self.expire(now);
        self.samples.iter().map(|&(_, db)| db).fold(0.0, f32::max)
    }

    /// Forgets everything (the click).
    pub fn reset(&mut self) {
        self.samples.clear();
    }
}

/// The read-out text of a reduction: „−4.1“ (U+2212, one decimal); „0.0“ without reduction.
pub fn readout(reduction_db: f32) -> String {
    if reduction_db <= 0.0 {
        "0.0".to_owned()
    } else {
        format!("{MINUS}{reduction_db:.1}")
    }
}

/// The filled parts of a bar at `level` (0–1): `(ok, loud)` heights as fractions of the bar. In
/// and Out turn `meter.loud` above 75 %; GR is one `loud` block from the top.
pub fn fills(name: &str, level: f32) -> (f32, f32) {
    let level = level.clamp(0.0, 1.0);
    if name == NAMES[1] {
        (0.0, level)
    } else {
        (
            level.min(metrics::LOUD_FROM),
            (level - metrics::LOUD_FROM).max(0.0),
        )
    }
}

/// Paints the meter into `rect`: `levels` In, GR, Out (0–1), `readout` at the bottom.
pub fn paint(painter: &Painter, rect: Rect, levels: [f32; 3], readout: &str) {
    let ppp = painter.pixels_per_point();
    let rect = px::snap_rect(rect, ppp);
    px::fill(painter, rect, radius::M, color32(color::SURFACE_1));
    let bar_h = rect.height() - metrics::BELOW;
    let pitch = widget::meter_gr::BAR_W + widget::meter_gr::BAR_GAP;
    let track = color32(color::METER_TRACK);
    for (i, (name, level)) in NAMES.iter().zip(levels).enumerate() {
        let x = rect.left() + metrics::PAD + i as f32 * pitch;
        let bar = Rect::from_min_size(
            pos2(x, rect.top() + metrics::PAD),
            vec2(widget::meter_gr::BAR_W, bar_h),
        );
        let bar = px::snap_rect(bar, ppp);
        px::fill(painter, bar, metrics::BAR_RADIUS, track);
        let clip = painter.with_clip_rect(bar);
        let (ok, loud) = fills(name, level);
        if *name == NAMES[1] {
            let h = bar.height() * loud;
            let r = Rect::from_min_size(bar.min, vec2(bar.width(), h));
            clip.rect_filled(r, 0.0, color32(color::METER_LOUD));
        } else {
            let ok_h = bar.height() * ok;
            let r = Rect::from_min_max(pos2(bar.left(), bar.bottom() - ok_h), bar.right_bottom());
            clip.rect_filled(r, 0.0, color32(color::METER_OK));
            let top = bar.bottom() - bar.height() * metrics::LOUD_FROM;
            let loud_h = bar.height() * loud;
            let r = Rect::from_min_max(pos2(bar.left(), top - loud_h), pos2(bar.right(), top));
            clip.rect_filled(r, 0.0, color32(color::METER_LOUD));
        }
        paint_centered(
            painter,
            pos2(bar.center().x, rect.top() + bar_h + metrics::NAME_GAP),
            name,
            &LABEL,
            None,
            f32::INFINITY,
            color32(color::TEXT_2),
        );
    }
    let y = rect.bottom() - metrics::READOUT_BOTTOM - VALUE.line_height;
    paint_centered(
        painter,
        pos2(rect.center().x, y),
        readout,
        &VALUE,
        None,
        rect.width(),
        color32(color::TEXT_1),
    );
}

/// The gain-reduction meter: `ui.add(MeterGr::new([0.72, 0.18, 0.64], "−4.1"))`; `clicked()`
/// means reset the [`GrHold`].
pub struct MeterGr<'a> {
    levels: [f32; 3],
    readout: &'a str,
    rows: u8,
}

impl<'a> MeterGr<'a> {
    /// A meter with the bar levels In, GR, Out (0–1) and the read-out text.
    pub fn new(levels: [f32; 3], readout: &'a str) -> Self {
        Self {
            levels,
            readout,
            rows: 3,
        }
    }

    /// Cell rows (default 3).
    #[must_use]
    pub fn rows(mut self, rows: u8) -> Self {
        self.rows = rows;
        self
    }
}

impl Widget for MeterGr<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let size = vec2(cell_width(1), cell_height(self.rows));
        let (rect, response) = ui.allocate_exact_size(size, sense(ui, Sense::click()));
        let spoken = if self.readout.is_empty() {
            String::new()
        } else {
            format!("größte Reduktion {} dB", self.readout)
        };
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(Role::Meter, true, ACCESSIBLE_NAME);
            info.current_text_value = Some(spoken.clone());
            info
        });
        accesskit(&response, |n| {
            n.add_action(egui::accesskit::Action::Click);
            n.set_description("Klick setzt die Anzeige zurück");
        });
        paint(ui.painter(), rect, self.levels, self.readout);
        if response.hovered() && !crate::widgets::is_still(ui) {
            ui.set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_keeps_the_maximum_of_the_last_1_5_seconds() {
        let mut h = GrHold::new();
        h.push(0.0, 6.0);
        h.push(0.5, 2.0);
        assert_eq!(h.max(1.0), 6.0);
        assert_eq!(h.max(1.5), 6.0, "exactly 1.5 s old: still inside");
        assert_eq!(h.max(1.6), 2.0, "the 6 dB fell out of the window");
        assert_eq!(h.max(2.1), 0.0);
        h.push(3.0, 4.0);
        h.reset();
        assert_eq!(h.max(3.1), 0.0, "a click resets");
    }

    #[test]
    fn negative_reduction_counts_as_none() {
        let mut h = GrHold::new();
        h.push(0.0, -3.0);
        assert_eq!(h.max(0.0), 0.0);
    }

    #[test]
    fn readout_uses_the_typographic_minus() {
        assert_eq!(readout(4.1), "\u{2212}4.1");
        assert_eq!(readout(0.0), "0.0");
    }

    #[test]
    fn bars_turn_loud_above_75_percent() {
        assert_eq!(fills("In", 0.5), (0.5, 0.0));
        let (ok, loud) = fills("Out", 0.9);
        assert_eq!(ok, 0.75);
        assert!((loud - 0.15).abs() < 1e-6);
        assert_eq!(fills("GR", 0.2), (0.0, 0.2));
        assert_eq!(fills("In", 2.0).0 + fills("In", 2.0).1, 1.0);
    }
}
