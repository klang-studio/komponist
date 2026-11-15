//! The ruler row: the track count over the headers, the ruler over the lanes (bar numbers, loop
//! range, playhead cap) and its interaction (S01 "Lineal ziehen: Oben: Playhead setzen. Unten
//! (Loop-Leiste): Loop-Bereich aufziehen."; prototype `rulerDown`).

use egui::{CursorIcon, Id, Sense, Ui};
use komponist_engine::TransportState;
use komponist_model::{Project, Tick, TickRange};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::timeline::{RulerKind, RulerModel, paint_ruler, ruler_access};
use komponist_ui::{px, widgets as w};

use super::geometry::Geometry;
use super::{Arrangement, Drag, metrics, names};
use crate::actions::{Action, TransportAction};
use crate::views::ViewCtx;

/// The playhead tick for a press at fractional tick `t`: on the nearest beat (prototype: quarter
/// of a bar), with `free` (Shift) on the tick; never before 0.
pub fn playhead_at(t: f64, beat: i64, free: bool) -> Tick {
    let t = t.max(0.0);
    if free {
        Tick(t.round() as i64)
    } else {
        Tick((t / beat as f64).round() as i64 * beat)
    }
}

/// The loop range of a drag that pressed in the bar starting at `bar` and is at fractional tick
/// `t` now: whole bars from the earlier to the later end, at least the pressed bar (prototype
/// `rulerDown`).
pub fn loop_range(bar: Tick, t: f64, bar_ticks: i64) -> TickRange {
    let cur = ((t.max(0.0) / bar_ticks as f64).round() as i64) * bar_ticks;
    let start = bar.0.min(cur);
    let end = (bar.0 + bar_ticks).max(cur);
    TickRange::new(Tick(start), Tick(end))
}

impl Arrangement {
    /// Paints the ruler row and handles presses and drags on the ruler.
    pub(super) fn ruler(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
    ) {
        let ppp = ui.pixels_per_point();
        let painter = ui.painter().clone();
        px::fill(&painter, g.count, 0.0, color32(color::SURFACE_2));
        let count = names::track_count(project.tracks.len());
        let galley = w::styled_galley(ui, &count, &typography::VALUE_S);
        let pos = w::text_pos(
            g.count,
            &galley,
            g.count.left() + metrics::COUNT_PADDING_X,
            ppp,
        );
        let count_rect = egui::Rect::from_min_size(pos, galley.size());
        painter.galley(pos, galley, color32(color::TEXT_3));
        let node = ui.interact(
            count_rect,
            Id::unique("komponist_arrangement_count"),
            Sense::hover(),
        );
        node.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Label, true, &count));

        let recording = matches!(
            ctx.engine.telemetry().transport(),
            TransportState::Recording | TransportState::CountIn
        );
        let model = RulerModel {
            axis: g.axis,
            meter: project.meter,
            kind: RulerKind::Arrangement {
                loop_range: project.loop_on.then_some(project.loop_range),
            },
            playhead: Some(ctx.engine.playhead()).filter(|t| *t >= Tick::ZERO),
            recording,
        };
        paint_ruler(&painter, g.ruler, &model);
        px::hline(
            &painter,
            g.body.x_range(),
            g.ruler.bottom(),
            color32(color::SURFACE_0),
        );

        let response = ui.interact(
            g.ruler,
            Id::unique("komponist_arrangement_ruler"),
            Sense::click_and_drag(),
        );
        let lang = komponist_ui::lang(ui.ctx());
        response.widget_info(|| ruler_access(lang, &model).widget_info());
        let half = g.ruler.center().y;
        if let Some(p) = response.hover_pos()
            && p.y >= half
        {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
        }

        let (pressed, down, pos, shift) = ui.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.interact_pos(),
                i.modifiers.shift,
            )
        });
        let Some(pos) = pos else {
            return;
        };
        let meter = project.meter;
        let t = g.tick_at(pos.x);
        if pressed && response.is_pointer_button_down_on() || pressed && response.clicked() {
            if pos.y < half {
                self.drag = Some(Drag::Playhead);
            } else {
                let bar_ticks = meter.ticks_per_bar();
                let bar = Tick((t.max(0.0) as i64).div_euclid(bar_ticks) * bar_ticks);
                self.drag = Some(Drag::LoopRange { bar, last: None });
            }
        }
        match &mut self.drag {
            Some(Drag::Playhead) => {
                let at = playhead_at(t, meter.ticks_per_beat(), shift);
                ctx.send(Action::Transport(TransportAction::Locate(at)));
                if !down {
                    self.drag = None;
                }
            }
            Some(Drag::LoopRange { bar, last }) => {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                let range = loop_range(*bar, t, meter.ticks_per_bar());
                if *last != Some((range.start, range.end)) {
                    *last = Some((range.start, range.end));
                    ctx.send(Action::Transport(TransportAction::SetLoopRange(range)));
                }
                if !down {
                    self.drag = None;
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playhead_snaps_to_beats() {
        assert_eq!(playhead_at(1000.0, 960, false), Tick(960));
        assert_eq!(playhead_at(1500.0, 960, false), Tick(1920));
        assert_eq!(playhead_at(1500.4, 960, true), Tick(1500));
        assert_eq!(playhead_at(-50.0, 960, false), Tick(0));
    }

    #[test]
    fn loop_ranges_cover_whole_bars() {
        let bar = 3840;
        // Press in bar 9 (tick 8 × 3840), no move: bar 9.
        let r = loop_range(Tick(8 * bar), 8.4 * bar as f64, bar);
        assert_eq!((r.start, r.end), (Tick(8 * bar), Tick(9 * bar)));
        // Dragged right to bar 12's end.
        let r = loop_range(Tick(8 * bar), 11.8 * bar as f64, bar);
        assert_eq!((r.start, r.end), (Tick(8 * bar), Tick(12 * bar)));
        // Dragged left to bar 5's start: the pressed bar stays in.
        let r = loop_range(Tick(8 * bar), 4.2 * bar as f64, bar);
        assert_eq!((r.start, r.end), (Tick(4 * bar), Tick(9 * bar)));
    }
}
