//! The time ruler above a timeline (`size.ruler` 22 in the arrangement; the piano roll's ruler is
//! 18 high in the screens) and the playhead (`Screens.dc.html` `arrangement`, `pianoRoll`;
//! `ks-view.js` `arrView`, `prView`).
//!
//! - Every bar: a 1 px `line.2` line over the full height and its number (`value.s`, 4 px after
//!   the line), centred in the height above the bottom band. Zoomed out, numbers and lines thin
//!   out to every 2nd, 4th, … bar.
//! - Beat ticks: 4 px `line.1` at the bottom, from 40 px per bar on.
//! - Arrangement: the loop range as `accent.surface` with a 2 px accent bar at the bottom; bar
//!   numbers inside the loop in `text.1`, else `text.3`. The playhead cap (9 × 6, `text.1`, red
//!   while recording) at the top.
//! - Editor (piano roll): numbers inside the clip in `text.2`, after it `text.4`; a 3 px bar in
//!   the clip colour along the clip.
//!
//! The playhead line through the lanes is [`paint_playhead`], painted by the canvas after its
//! contents.

use egui::{Color32, Painter, Rangef, Rect, Role, pos2, vec2};
use komponist_model::format::fmt_pos;
use komponist_model::{Meter, Tick, TickRange};
use komponist_tokens::egui_support::color32;
use komponist_tokens::typography;
use komponist_tokens::{Srgba, color, space};

use super::mesh::Quads;
use super::{Access, TimeAxis, metrics};
use crate::{fonts, px};

/// Which ruler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RulerKind {
    /// The arrangement ruler: loop range (when the loop is on) and playhead cap.
    Arrangement {
        /// The loop range, `None` when the loop is off.
        loop_range: Option<TickRange>,
    },
    /// The editor ruler of the piano roll: the clip's extent.
    Editor {
        /// The clip on the timeline.
        clip: TickRange,
        /// Colour of the clip range bar: the track's `base`, `state.record` while recording.
        clip_color: Color32,
    },
}

/// What a ruler shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RulerModel {
    /// Tick ↔ x of the canvas below.
    pub axis: TimeAxis,
    /// Meter of the project.
    pub meter: Meter,
    /// Arrangement or editor.
    pub kind: RulerKind,
    /// Playhead position for the cap; `None`: no cap (the editor draws only the line).
    pub playhead: Option<Tick>,
    /// Recording: the cap is `state.record`.
    pub recording: bool,
}

/// The smallest power of two `n` with `n × px_per_bar ≥ min`.
fn thin_out(px_per_bar: f32, min: f32) -> i64 {
    let mut n = 1_i64;
    while (n as f32) * px_per_bar < min && n < 1 << 20 {
        n *= 2;
    }
    n
}

fn in_range(range: TickRange, t: Tick) -> bool {
    range.start <= t && t < range.end
}

/// Paints a ruler into `rect` (the full ruler area of the canvas).
pub fn paint_ruler(painter: &Painter, rect: Rect, model: &RulerModel) {
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(rect, ppp);
    let painter = painter.with_clip_rect(r.intersect(painter.clip_rect()));
    let axis = &model.axis;
    let w = px::line_width(ppp);
    let mut q = Quads::new(ppp);
    q.quad(r, color32(color::SURFACE_1));

    let x_of = |t: Tick| px::snap(axis.x(t), ppp);
    match model.kind {
        RulerKind::Arrangement {
            loop_range: Some(lr),
        } if !lr.is_empty() => {
            let (x0, x1) = (x_of(lr.start), x_of(lr.end));
            q.quad(
                Rect::from_min_max(pos2(x0, r.min.y), pos2(x1, r.max.y)),
                color32(color::ACCENT_SURFACE),
            );
            q.quad(
                Rect::from_min_max(
                    pos2(x0, r.max.y - metrics::RULER_LOOP_BAR),
                    pos2(x1, r.max.y),
                ),
                color32(color::ACCENT),
            );
        }
        RulerKind::Editor { clip, clip_color } if !clip.is_empty() => {
            q.quad(
                Rect::from_min_max(
                    pos2(x_of(clip.start), r.max.y - metrics::RULER_RANGE_BAR),
                    pos2(x_of(clip.end), r.max.y),
                ),
                clip_color,
            );
        }
        _ => {}
    }

    // Bars, beat ticks and numbers.
    let meter = model.meter;
    let per_bar = meter.ticks_per_bar();
    let bar_px = axis.px_per_bar(meter);
    let style = typography::VALUE_S;
    let (t0, t1) = axis.ticks_in(r.x_range());
    let first_bar = t0.0.div_euclid(per_bar).max(0);
    let last_bar = t1.0.div_euclid(per_bar);
    // Room for the widest number on screen, its padding and a gap before the next line.
    let widest = painter
        .layout_job(fonts::layout_job(
            &(last_bar + 1).to_string(),
            &style,
            Color32::PLACEHOLDER,
        ))
        .size()
        .x;
    let label_every = thin_out(bar_px, widest + 2.0 * space::SPACE_1 + w);
    let line_every = thin_out(bar_px, space::SPACE_1);
    let ticks = bar_px >= metrics::RULER_BEAT_TICKS_MIN_BAR;
    let beat_px = axis.width(Tick(meter.ticks_per_beat()));
    let tick_h = space::SPACE_1;
    let label_h = r.height() - metrics::RULER_LOOP_BAR;
    let mut labels = Vec::new();
    for bar in first_bar..=last_bar {
        let t = Tick(bar * per_bar);
        let x = x_of(t);
        if bar % line_every == 0 {
            q.quad(
                Rect::from_min_max(pos2(x, r.min.y), pos2(x + w, r.max.y)),
                color32(color::LINE_2),
            );
        }
        if ticks {
            for b in 1..meter.num {
                let tx = px::snap(axis.x(t) + f32::from(b) * beat_px, ppp);
                q.quad(
                    Rect::from_min_max(pos2(tx, r.max.y - tick_h), pos2(tx + w, r.max.y)),
                    color32(color::LINE_1),
                );
            }
        }
        if bar % label_every == 0 {
            labels.push((bar, x, label_color(&model.kind, t)));
        }
    }
    q.paint(&painter);

    for (bar, x, c) in labels {
        let galley = painter.layout_job(fonts::layout_job(
            &(bar + 1).to_string(),
            &style,
            color32(c),
        ));
        let y = r.min.y + (label_h - galley.size().y) / 2.0;
        painter.galley(
            pos2(x + w + space::SPACE_1, px::snap(y, ppp)),
            galley,
            Color32::PLACEHOLDER,
        );
    }

    if let (RulerKind::Arrangement { .. }, Some(ph)) = (model.kind, model.playhead) {
        let [cw, ch] = metrics::PLAYHEAD_CAP;
        let x = x_of(ph);
        // `left: ph − 4`: centred on the 1 px line.
        let cap = Rect::from_min_size(pos2(x - (cw - w) / 2.0, r.min.y), vec2(cw, ch));
        let c = if model.recording {
            color::STATE_RECORD
        } else {
            color::TEXT_1
        };
        let mut q = Quads::new(ppp);
        q.rect(cap, color32(c));
        q.paint(&painter);
    }
}

fn label_color(kind: &RulerKind, bar_start: Tick) -> Srgba {
    match *kind {
        RulerKind::Arrangement {
            loop_range: Some(lr),
        } if in_range(lr, bar_start) => color::TEXT_1,
        RulerKind::Arrangement { .. } => color::TEXT_3,
        RulerKind::Editor { clip, .. } if in_range(clip, bar_start) => color::TEXT_2,
        RulerKind::Editor { .. } => color::TEXT_4,
    }
}

/// Paints the playhead: a 1 px `text.1` line at `x` (left edge, snapped) over `y`.
pub fn paint_playhead(painter: &Painter, x: f32, y: Rangef) {
    px::vline(painter, x, y, color32(color::TEXT_1));
}

/// Accessible name and role of the ruler: `Lineal` (role slider) with the playhead position as
/// value (`10.2.120`).
pub fn ruler_access(model: &RulerModel) -> Access {
    let a = Access::new(Role::Slider, "Lineal");
    match model.playhead {
        Some(t) => a.with_value(fmt_pos(t, model.meter)),
        None => a,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinning() {
        assert_eq!(thin_out(72.0, 20.0), 1);
        assert_eq!(thin_out(12.0, 20.0), 2);
        assert_eq!(thin_out(3.0, 20.0), 8);
    }

    #[test]
    fn label_colours() {
        let lr = TickRange::new(Tick(8 * 3840), Tick(12 * 3840));
        let arr = RulerKind::Arrangement {
            loop_range: Some(lr),
        };
        assert_eq!(label_color(&arr, Tick(8 * 3840)), color::TEXT_1);
        assert_eq!(label_color(&arr, Tick(12 * 3840)), color::TEXT_3);
        let ed = RulerKind::Editor {
            clip: TickRange::new(Tick(8 * 3840), Tick(10 * 3840)),
            clip_color: Color32::RED,
        };
        assert_eq!(label_color(&ed, Tick(9 * 3840)), color::TEXT_2);
        assert_eq!(label_color(&ed, Tick(10 * 3840)), color::TEXT_4);
    }

    #[test]
    fn access() {
        let m = RulerModel {
            axis: TimeAxis::per_bar(0.0, 72.0, Meter::FOUR_FOUR),
            meter: Meter::FOUR_FOUR,
            kind: RulerKind::Arrangement { loop_range: None },
            playhead: Some(Tick(9 * 3840 + 960 + 480)),
            recording: false,
        };
        let a = ruler_access(&m);
        assert_eq!(a.name, "Lineal");
        assert_eq!(a.value.as_deref(), Some("10.2.120"));
    }
}
