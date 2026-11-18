//! The ruler row shows the track count over the headers and the bar numbers and playhead over
//! the lanes. Clicking or dragging the ruler sets the playhead.

use egui::{Id, Sense, Ui};
use komponist_engine::TransportState;
use komponist_model::{Project, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, typography};
use komponist_ui::timeline::{RulerKind, RulerModel, paint_ruler, ruler_access};
use komponist_ui::{px, widgets as w};

use super::geometry::Geometry;
use super::{Arrangement, Drag, count_line, metrics};
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
        let count = count_line(ctx.lang, project);
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
            kind: RulerKind::Arrangement { loop_range: None },
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
        if pressed && (response.is_pointer_button_down_on() || response.clicked()) {
            self.drag = Some(Drag::Playhead { last: None });
        }
        if let Some(Drag::Playhead { last }) = &mut self.drag {
            let at = playhead_at(g.tick_at(pos.x), project.meter.ticks_per_beat(), shift);
            // Seeking every frame would keep restarting playback while the pointer is held.
            if *last != Some(at) {
                *last = Some(at);
                ctx.send(Action::Transport(TransportAction::Locate(at)));
            }
            if !down {
                self.drag = None;
            }
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
    fn playhead_drag_seeks_once_per_target_tick() {
        use egui::{Event, Modifiers, PointerButton, RawInput, Rect, pos2, vec2};
        use std::sync::Arc;

        let mut app = crate::App::headless(Project::new("Ruler"));
        let mut arrangement = Arrangement::new();
        let project = Arc::clone(app.project());
        let context = egui::Context::default();
        komponist_ui::install(&context);
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 500.0));
        let geometry = Geometry::new(screen, &project, &app.ui.arrangement);
        let beat = project.meter.ticks_per_beat();
        let position = |tick| pos2(geometry.axis.x(tick), geometry.ruler.top() + 4.0);
        let from = position(Tick(4 * beat));
        let to = position(Tick(5 * beat));
        let button = |pos, pressed| Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let locate = |tick| Action::Transport(TransportAction::Locate(tick));
        let mut frame = |events| {
            let mut actions = Vec::new();
            let mut output = context.run_ui(
                RawInput {
                    screen_rect: Some(screen),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let mut ctx = ViewCtx::new(
                        &project,
                        &mut app.ui,
                        &app.engine,
                        &mut app.meters,
                        &app.keymap,
                        &mut actions,
                    );
                    arrangement.ruler(ui, &mut ctx, &project, &geometry);
                },
            );
            output.textures_delta.clear();
            actions
        };

        assert!(frame(vec![Event::PointerMoved(from)]).is_empty());
        assert_eq!(frame(vec![button(from, true)]), [locate(Tick(4 * beat))]);
        assert!(frame(vec![]).is_empty(), "holding must not seek again");
        assert!(frame(vec![Event::PointerMoved(from + vec2(2.0, 0.0))]).is_empty());
        assert_eq!(
            frame(vec![Event::PointerMoved(to)]),
            [locate(Tick(5 * beat))]
        );
        assert!(frame(vec![button(to, false)]).is_empty());
        assert_eq!(frame(vec![button(to, true)]), [locate(Tick(5 * beat))]);
    }
}
