//! The MIDI and step clip editors' automation lanes, sharing the arrangement curve editor.

use std::collections::HashMap;

use egui::{Color32, Rect, Response, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::automation::{
    self, AutomationMode, AutomationOwner, AutomationTarget, ClipAutomation, Point,
};
use komponist_model::{Clip, ClipId, Grid, Tick, Track};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, TrackPalette, color, line, size, space, typography};
use komponist_ui::timeline::TimeAxis;
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{fonts, icons, px, widgets};

use super::{
    Action as Automation, Catalog, Choice, Chooser, Editor, LaneFrame, Paint, Source,
    chooser_button, metrics as m, mode_chip, mode_chip_width, percentage,
};
use crate::actions::Action;
use crate::views::{ViewCtx, clip_sections};

#[derive(Default)]
pub(crate) struct ClipEditor {
    shown: HashMap<ClipId, bool>,
    heights: HashMap<(ClipId, AutomationTarget), f32>,
    chooser: Chooser,
    editor: Editor,
}

impl ClipEditor {
    pub fn shown(&self, clip: &Clip) -> bool {
        !clip.automation.is_empty() && self.shown.get(&clip.id).copied().unwrap_or(true)
    }

    pub fn height(&self, clip: &Clip, available: f32) -> f32 {
        if !self.shown(clip) {
            return 0.0;
        }
        clip.automation
            .iter()
            .map(|lane| self.lane_height(clip.id, &lane.target))
            .sum::<f32>()
            .min(available.max(0.0))
    }

    pub fn minimum_height(&self, clip: &Clip) -> f32 {
        self.height(clip, m::CLIP_HEIGHT_MIN)
    }

    fn lane_height(&self, clip: ClipId, target: &AutomationTarget) -> f32 {
        self.heights
            .get(&(clip, target.clone()))
            .copied()
            .unwrap_or(m::CLIP_HEIGHT_DEFAULT)
    }

    pub fn toolbar(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        clip: &Clip,
        track: &Track,
        button: &Response,
    ) {
        let id = button.id;
        if button.clicked() {
            if clip.automation.is_empty() {
                self.chooser.open(id);
            } else {
                let shown = self.shown(clip);
                self.shown.insert(clip.id, !shown);
            }
        }
        self.choose(ui, ctx, clip, track, button);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn show(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        rect: Rect,
        left: f32,
        clip: &Clip,
        track: &Track,
        axis: TimeAxis,
        grid: Grid,
        snap: bool,
        palette: &TrackPalette,
    ) {
        if !self.shown(clip) || !rect.is_positive() {
            self.editor.finish_frame(ui, ctx);
            return;
        }
        let mode_width = mode_chip_width(ui, ctx.lang);
        let height_limit = if clip.automation.len() == 1 {
            rect.height().min(m::CLIP_HEIGHT_MAX)
        } else {
            m::CLIP_HEIGHT_MAX
        };
        let mut body = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .id_salt(("clip-automation", clip.id)),
        );
        body.set_clip_rect(rect.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical()
            .scroll_source(egui::scroll_area::ScrollSource {
                drag: egui::scroll_area::DragScroll::Never,
                ..Default::default()
            })
            .auto_shrink([false, false])
            .show(&mut body, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for envelope in &clip.automation {
                    self.lane(
                        ui,
                        ctx,
                        clip,
                        track,
                        envelope,
                        left,
                        axis,
                        grid,
                        snap,
                        palette,
                        mode_width,
                        height_limit,
                    );
                }
            });
        self.editor.finish_frame(ui, ctx);
    }

    #[allow(clippy::too_many_arguments)]
    fn lane(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        clip: &Clip,
        track: &Track,
        envelope: &ClipAutomation,
        left: f32,
        axis: TimeAxis,
        grid: Grid,
        snap: bool,
        palette: &TrackPalette,
        mode_width: f32,
        height_limit: f32,
    ) {
        let key = (clip.id, envelope.target.clone());
        let height = self
            .lane_height(clip.id, &envelope.target)
            .min(height_limit);
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        if !row.intersects(ui.clip_rect()) {
            return;
        }
        let lang = ctx.lang;
        let paint = ui.painter();
        let gutter = Rect::from_min_max(row.min, pos2(left, row.bottom()));
        px::fill(paint, gutter, 0.0, color32(color::SURFACE_2));
        let icon = Rect::from_center_size(
            pos2(gutter.center().x, gutter.top() + m::CHIP_HEIGHT / 2.0),
            vec2(size::ICON, size::ICON),
        );
        icons::paint_in(ui, icon, Icon::Spline, color32(color::TEXT_2));
        let content = Rect::from_min_max(pos2(left, row.top()), row.max);
        let bar = Rect::from_min_size(
            content.min,
            vec2(content.width(), m::CHIP_HEIGHT + space::SPACE_1),
        );
        px::fill(paint, bar, 0.0, color32(color::SURFACE_2));
        let owner = AutomationOwner::Track(track.id);
        let catalog = Catalog::new(ctx, owner);
        let driven = catalog
            .as_ref()
            .and_then(|catalog| catalog.driven(&envelope.target));
        let result = catalog.map(|catalog| catalog.resolve(&envelope.target));
        let (target, warning) = match result {
            Some(Ok(target)) => (Some(target), None),
            Some(Err(warning)) => (None, warning),
            None => (None, None),
        };
        let label = target
            .as_ref()
            .map_or(envelope.label.as_str(), |t| t.label.as_str());
        let mode = track
            .automation
            .lane(&envelope.target)
            .map_or(AutomationMode::Read, |lane| lane.mode);
        let active = mode != AutomationMode::Off && warning.is_none() && driven.is_none();
        let shape = if active {
            color32(palette.ink)
        } else {
            color32(color::TEXT_3)
        };
        let id = ui.scope_id().with((&key, "envelope"));
        let access = lang.a11y_automation_clip_lane(label);
        let group = ui.interact(row.intersect(ui.clip_rect()), id, Sense::hover());
        group.widget_info(|| WidgetInfo::labeled(Role::Group, true, &access));
        widgets::accesskit(&group, |node| {
            if let Some(description) = warning.as_deref().or(driven.as_deref()) {
                node.set_description(description);
            }
        });
        let mut row_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(content)
                .id_salt(id)
                .accessibility_parent(id),
        );
        row_ui.set_clip_rect(ui.clip_rect().intersect(row));
        let close = Rect::from_center_size(
            pos2(bar.right() - m::CHIP_HEIGHT / 2.0, bar.center().y),
            vec2(m::CHIP_HEIGHT, m::CHIP_HEIGHT),
        );
        let mode_rect = Rect::from_min_size(
            pos2(close.left() - mode_width - space::SPACE_0_5, close.top()),
            vec2(mode_width, m::CHIP_HEIGHT),
        );
        let chooser_width =
            (mode_rect.left() - bar.left() - space::SPACE_2).clamp(0.0, m::CLIP_CHOOSER_WIDTH);
        let chooser = Rect::from_min_size(
            pos2(bar.left() + space::SPACE_1, close.top()),
            vec2(chooser_width, m::CHIP_HEIGHT),
        );
        let trigger = chooser
            .is_positive()
            .then(|| chooser_button(&mut row_ui, chooser, id.with("parameter"), lang, label));
        let source = Source::Clip(clip.id, envelope.target.clone());
        let close_response = row_ui.interact(close, id.with("remove"), Sense::click());
        close_response
            .widget_info(|| WidgetInfo::labeled(Role::Button, true, lang.automation_clip_remove()));
        icons::paint_in(
            &row_ui,
            close.shrink(space::SPACE_1),
            Icon::X,
            color32(color::TEXT_2),
        );
        Tip::new(lang.automation_clip_remove()).attach(&close_response);
        if close_response.clicked() {
            ctx.send(Action::Automation(Automation::Remove(source.clone())));
        }
        if let Some(lane) = track.automation.lane(&envelope.target)
            && let Some(mode) =
                mode_chip(&mut row_ui, mode_rect, id.with("mode"), lang, label, mode)
        {
            ctx.send(Action::Automation(Automation::Mode(owner, lane.id, mode)));
        }
        if let Some(trigger) = trigger {
            if let Some(reason) = &driven {
                trigger.clone().on_hover_text(reason);
            }
            if trigger.clicked() {
                self.chooser.open(trigger.id);
            }
            self.choose(ui, ctx, clip, track, &trigger);
        }

        let end = automation::clip_content_length(clip);
        let project_head = ctx.engine.playhead();
        let head = if project_head >= clip.start && project_head < clip.end() {
            Tick((project_head - clip.start).0 % end.0)
        } else {
            Tick(-1)
        };
        let base = target.as_ref().map_or(0.0, |target| target.base);
        let current = active
            .then(|| {
                ctx.automation_capture
                    .and_then(|capture| capture.current_clip_value(clip.id, &envelope.target))
                    .or_else(|| {
                        automation::value_at(
                            automation::clip_points(clip, envelope),
                            head.max(Tick::ZERO),
                        )
                    })
            })
            .flatten()
            .unwrap_or(base);
        let value = warning.clone().or(driven).unwrap_or_else(|| {
            target.as_ref().map_or_else(
                || percentage(lang, current),
                |target| target.format(current),
            )
        });
        let available = Rect::from_min_max(
            pos2(chooser.right() + space::SPACE_1, bar.top()),
            pos2(mode_rect.left(), bar.bottom()),
        );
        if available.is_positive() {
            let style = typography::SMALL;
            let galley = crate::views::elided_galley(
                &row_ui,
                &value,
                fonts::font_id(&style),
                style.line_height,
                available.width(),
            );
            let pos =
                widgets::text_pos(available, &galley, available.left(), ui.pixels_per_point());
            row_ui.painter().galley(
                pos,
                galley,
                color32(if warning.is_some() {
                    color::STATE_ERROR
                } else {
                    color::TEXT_2
                }),
            );
        }
        let body = Rect::from_min_max(pos2(content.left(), bar.bottom()), content.max);
        let frame = LaneFrame {
            source,
            label,
            access: &access,
            warning: warning.as_deref(),
            points: &envelope.points,
            target: target.as_ref(),
            rect: body,
            axis,
            grid,
            snap,
            end: Some(end),
            head,
        };
        let paint = Paint {
            curve: shape,
            fill: color32(palette.body).gamma_multiply(0.5),
            effective: &[],
            overrides: &[],
            beyond: Some(end),
        };
        self.editor.show(&mut row_ui, ctx, &frame, &paint);
        if clip.length > end {
            repetitions(
                row_ui.painter(),
                &frame,
                automation::clip_points(clip, envelope),
                end,
                clip.length,
                if active {
                    color32(palette.base)
                } else {
                    color32(color::TEXT_4)
                },
            );
        }
        if let Some(height) = clip_sections::resize_from_top(
            ui,
            row,
            id.with("resize"),
            &lang.dock_section_height(label),
            height,
            m::CLIP_HEIGHT_MIN..=m::CLIP_HEIGHT_MAX,
        ) {
            self.heights.insert(key, height);
        }
        ui.painter().hline(
            row.x_range(),
            row.bottom() - line::WIDTH / 2.0,
            egui::Stroke::new(line::WIDTH, color32(color::SURFACE_0)),
        );
    }

    fn choose(
        &mut self,
        ui: &Ui,
        ctx: &mut ViewCtx<'_>,
        clip: &Clip,
        track: &Track,
        trigger: &Response,
    ) {
        let Some(catalog) = Catalog::new(ctx, AutomationOwner::Track(track.id)) else {
            return;
        };
        match self
            .chooser
            .show(ui, trigger.id, trigger.rect, &catalog, false)
        {
            Some(Choice::Target { key, label })
                if clip.automation.iter().all(|a| a.target != key) =>
            {
                self.shown.insert(clip.id, true);
                ctx.send(Action::Automation(Automation::AddClip(clip.id, key, label)));
            }
            _ => {}
        }
    }
}

pub(crate) fn toolbar_button(ui: &mut Ui, lang: komponist_i18n::Lang, shown: bool) -> Response {
    ui.add(
        Button::icon(Icon::Spline, lang.automation_clip())
            .badge(lang.automation_clip())
            .on(shown),
    )
}

fn repetitions(
    painter: &egui::Painter,
    lane: &LaneFrame<'_>,
    points: &[Point],
    period: Tick,
    end: Tick,
    ink: Color32,
) {
    if points.is_empty() {
        return;
    }
    let (left, right) = lane
        .axis
        .ticks_in(lane.rect.intersect(painter.clip_rect()).x_range());
    let from = (left.0 / period.0).max(1);
    let to = (right.0.min(end.0) / period.0).max(0);
    let minimum = (space::SPACE_2 / lane.axis.width(period)).ceil().max(1.0) as i64;
    for index in (from..=to).step_by(minimum as usize) {
        let offset = Tick(period.0 * index);
        let interval = Rect::from_x_y_ranges(
            lane.axis.x(offset)..=lane.axis.x((offset + period).min(end)),
            lane.rect.y_range(),
        )
        .intersect(lane.rect)
        .intersect(painter.clip_rect());
        if !interval.is_positive() {
            continue;
        }
        let repeated = LaneFrame {
            axis: TimeAxis {
                x0: lane.axis.x(offset),
                ..lane.axis
            },
            ..lane.clone()
        };
        super::curve::paint_curve(
            &painter.with_clip_rect(interval),
            &repeated,
            points,
            interval,
            ink,
            None,
            false,
        );
    }
}
