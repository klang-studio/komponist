//! The shared normalized curve canvas. The arrangement supplies song ticks and the clip editors
//! supply content ticks. Edits are sent through the dispatcher as one undo step per gesture.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;

use egui::accesskit::Action as AccessAction;
use egui::{
    Color32, Id, Key, Modifiers, Pos2, Rect, Response, Role, Sense, Stroke, Ui, WidgetInfo, pos2,
    vec2,
};
use komponist_model::automation::{self, Point, Points, Shape};
use komponist_model::format::fmt_pos;
use komponist_model::{Grid, Meter, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, line, space};
use komponist_ui::timeline::{GridSpec, TimeAxis, grid_lines, paint_grid, paint_playhead};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{px, widgets};

use super::chooser::percentage;
use super::{Action as Automation, Source, Target, metrics as m};
use crate::actions::{Action, EditAction};
use crate::views::ViewCtx;

pub(crate) struct Override {
    pub start: Tick,
    pub end: Tick,
}

pub(crate) struct Paint<'a> {
    pub curve: Color32,
    pub fill: Color32,
    pub effective: &'a [Point],
    pub overrides: &'a [Override],
    pub beyond: Option<Tick>,
}

pub(crate) struct LaneFrame<'a> {
    pub source: Source,
    pub label: &'a str,
    pub access: &'a str,
    pub warning: Option<&'a str>,
    pub points: &'a [Point],
    pub target: Option<&'a Target>,
    pub rect: Rect,
    pub axis: TimeAxis,
    pub grid: Grid,
    pub snap: bool,
    pub end: Option<Tick>,
    pub head: Tick,
}

impl LaneFrame<'_> {
    fn pos(&self, point: Point) -> Pos2 {
        pos2(
            self.axis.x(point.tick),
            egui::lerp(self.rect.bottom()..=self.rect.top(), point.value),
        )
    }

    fn value(&self, y: f32) -> f32 {
        ((self.rect.bottom() - y) / self.rect.height()).clamp(0.0, 1.0)
    }

    fn tick(&self, x: f32, fine: bool, meter: Meter) -> Tick {
        let tick = Tick(self.axis.tick_at(x).round() as i64);
        let tick = if self.snap && !fine {
            self.grid.snap(tick, meter)
        } else {
            tick
        };
        tick.max(Tick::ZERO).min(self.end.unwrap_or(Tick(i64::MAX)))
    }

    fn format(&self, ctx: &ViewCtx<'_>, value: f32) -> String {
        self.target.map_or_else(
            || percentage(ctx.lang, value),
            |target| target.format(value),
        )
    }

    fn parse(&self, text: &str) -> Option<f32> {
        if let Some(target) = self.target {
            return target.parse(text);
        }
        widgets::number_field::parse_number(text, "%").map(|n| (n / 100.0).clamp(0.0, 1.0) as f32)
    }
}

#[derive(Debug, Default)]
pub(crate) struct Editor {
    selected: HashMap<Source, BTreeSet<usize>>,
    active: Option<Source>,
    drag: Option<Drag>,
    menu_point: Option<(Source, usize)>,
    entry: Option<ValueEntry>,
}

#[derive(Debug)]
struct ValueEntry {
    source: Source,
    index: usize,
    text: String,
}

#[derive(Debug)]
enum Drag {
    Move {
        source: Source,
        index: usize,
        origin: Pos2,
        points: Vec<Point>,
        indices: BTreeSet<usize>,
        copy: bool,
    },
    Bend {
        source: Source,
        index: usize,
        origin: Pos2,
        initial: f32,
    },
    Draw {
        source: Source,
        raw: Vec<Point>,
    },
    Box {
        source: Source,
        origin: Pos2,
        end: Pos2,
        append: bool,
    },
}

impl Drag {
    fn source(&self) -> &Source {
        match self {
            Self::Move { source, .. }
            | Self::Bend { source, .. }
            | Self::Draw { source, .. }
            | Self::Box { source, .. } => source,
        }
    }
}

#[derive(Clone, Copy)]
enum PointCommand {
    Shape(Shape),
    Value,
    Delete,
}

impl Editor {
    pub fn selected(&self, source: &Source) -> bool {
        self.active.as_ref() == Some(source)
    }

    pub fn finish_frame(&mut self, ui: &Ui, ctx: &mut ViewCtx<'_>) {
        if self.drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            if matches!(self.drag, Some(Drag::Move { .. } | Drag::Bend { .. })) {
                ctx.send(Action::Edit(EditAction::EndGesture));
            }
            self.drag = None;
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        paint: &Paint<'_>,
    ) {
        let visible = lane.rect.intersect(ui.clip_rect());
        if !visible.is_positive() {
            return;
        }
        let id = ui.scope_id().with(("automation-curve", &lane.source));
        let response = ui.interact(visible, id, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(Role::Group, true, lane.access));
        widgets::accesskit(&response, |node| {
            if let Some(warning) = lane.warning {
                node.set_description(warning);
            }
        });
        let painter = ui.painter().with_clip_rect(visible);
        paint_background(&painter, ctx.project.meter, lane);
        paint_curve(
            &painter,
            lane,
            lane.points,
            visible,
            paint.curve,
            Some(paint.fill),
            false,
        );
        for span in paint.overrides {
            let rect = span_rect(lane, span).intersect(visible);
            if !rect.is_positive() {
                continue;
            }
            let over = painter.with_clip_rect(rect);
            hatch(&over, rect);
            paint_curve(&over, lane, paint.effective, rect, paint.curve, None, true);
            over.text(
                rect.left_top() + vec2(space::SPACE_1, space::SPACE_0_5),
                egui::Align2::LEFT_TOP,
                ctx.lang.automation_overridden(),
                komponist_ui::fonts::font_id(&komponist_tokens::typography::LABEL),
                color32(color::TEXT_2),
            );
        }
        if let Some(end) = paint.beyond {
            let outside =
                Rect::from_min_max(pos2(lane.axis.x(end), lane.rect.top()), lane.rect.max)
                    .intersect(visible);
            if outside.is_positive() {
                px::fill(&painter, outside, 0.0, color32(color::CANVAS_OUTSIDE_CLIP));
            }
        }
        if lane.points.is_empty() && paint.overrides.is_empty() {
            painter.text(
                visible.center(),
                egui::Align2::CENTER_CENTER,
                ctx.lang.automation_lane_empty(),
                komponist_ui::fonts::font_id(&komponist_tokens::typography::SMALL),
                color32(color::TEXT_3),
            );
        }

        let indices = visible_range(lane, visible.expand(m::POINT_HIT));
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let handle = pointer.and_then(|p| closest_handle(lane, p));
        let dense = indices.len() as f32 * m::POINT_DENSITY > visible.width();
        let hovered = pointer
            .filter(|p| visible.contains(*p))
            .and_then(|p| nearest_point(lane, p));
        let mut point_action = None;
        let mut tooltip = None;
        for i in indices {
            let point = lane.points[i];
            let pos = lane.pos(point);
            let selected = self
                .selected
                .get(&lane.source)
                .is_some_and(|set| set.contains(&i));
            if (!dense || selected || hovered == Some(i))
                && visible.expand(m::POINT_HIT).contains(pos)
            {
                let (fill, ring) = if selected {
                    (color32(color::TEXT_1), color32(color::ACCENT))
                } else {
                    (paint.curve, color32(color::SURFACE_0))
                };
                painter.circle(pos, m::POINT / 2.0, fill, Stroke::new(line::WIDTH, ring));
                let node = ui.interact(
                    Rect::from_center_size(pos, vec2(m::POINT_HIT, m::POINT_HIT))
                        .intersect(visible),
                    id.with(("point", i)),
                    Sense::focusable_noninteractive(),
                );
                let value = lane.format(ctx, point.value);
                let position = fmt_pos(point.tick, ctx.project.meter);
                let label = ctx.lang.a11y_automation_point(&position, &value);
                node.widget_info(|| {
                    let mut info = WidgetInfo::labeled(Role::Slider, true, &label);
                    info.value = Some(f64::from(point.value));
                    info
                });
                widgets::accesskit(&node, |node| {
                    node.set_selected(selected);
                    node.set_value(value.clone());
                    node.add_action(AccessAction::Click);
                    node.add_action(AccessAction::Increment);
                    node.add_action(AccessAction::Decrement);
                });
                let (click, steps) = ui.input(|input| {
                    (
                        input.num_accesskit_action_requests(node.id, AccessAction::Click) > 0,
                        input.num_accesskit_action_requests(node.id, AccessAction::Increment)
                            as i32
                            - input.num_accesskit_action_requests(node.id, AccessAction::Decrement)
                                as i32,
                    )
                });
                if click {
                    self.select(lane, Some(i), false);
                    response.request_focus();
                }
                if steps != 0 {
                    point_action = Some((i, steps));
                }
                if hovered == Some(i) {
                    tooltip = Some(ctx.lang.automation_tooltip(lane.label, &value, &position));
                }
            }
            if (selected || handle == Some(i))
                && let Some(center) = handle_pos(lane, i)
            {
                paint_diamond(&painter, center, paint.curve);
            }
        }
        if let Some(i) = handle
            && let Some(center) = handle_pos(lane, i)
        {
            paint_diamond(&painter, center, paint.curve);
        }
        if let Some((point, steps)) = point_action {
            let mut points = lane.points.to_vec();
            points[point].value = (points[point].value + steps as f32 * 0.01).clamp(0.0, 1.0);
            send_points(ctx, lane, points, "undo-automation-value");
        }

        if response.hovered() {
            ui.ctx()
                .set_cursor_icon(if hovered.is_some() || handle.is_some() {
                    egui::CursorIcon::Grab
                } else {
                    egui::CursorIcon::Crosshair
                });
        }
        if let Some(tooltip) = tooltip {
            Tip::new(&tooltip).attach(&response);
        }
        self.pointer(ui, ctx, lane, &response);
        self.keyboard(ui, ctx, lane, &response);
        self.point_menu(ui, ctx, lane, &response);
        self.value_entry(ui, ctx, lane, id);
        self.draw_drag(ui, ctx, lane, &painter);
        let x = lane.axis.x(lane.head);
        if visible.x_range().contains(x) {
            paint_playhead(&painter, x, lane.rect.y_range());
        }
    }

    fn select(&mut self, lane: &LaneFrame<'_>, index: Option<usize>, append: bool) {
        if !append {
            self.selected.clear();
        }
        self.active = Some(lane.source.clone());
        let points = self.selected.entry(lane.source.clone()).or_default();
        if let Some(i) = index
            && (!append || !points.remove(&i))
        {
            points.insert(i);
        }
    }

    fn pointer(
        &mut self,
        ui: &Ui,
        ctx: &mut ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        response: &Response,
    ) {
        let (mods, origin, pointer) = ui.input(|i| {
            (
                i.modifiers,
                i.pointer.press_origin(),
                i.pointer.interact_pos(),
            )
        });
        let meter = ctx.project.meter;
        if response.double_clicked()
            && let Some(p) = pointer
        {
            let mut points = lane.points.to_vec();
            if let Some(i) = nearest_point(lane, p) {
                points.remove(i);
                send_points(ctx, lane, points, "undo-automation-delete-points");
            } else if lane.end.is_none_or(|end| p.x <= lane.axis.x(end)) {
                let point = Point::linear(lane.tick(p.x, mods.shift, meter), lane.value(p.y));
                points.push(point);
                send_points(ctx, lane, points, "undo-automation-add-point");
            }
            self.select(lane, None, false);
            response.request_focus();
        } else if response.clicked()
            && let Some(p) = pointer
        {
            if mods.alt
                && let Some(index) = closest_handle(lane, p)
            {
                let mut points = lane.points.to_vec();
                points[index].shape = Shape::Linear;
                send_points(ctx, lane, points, "undo-automation-shape");
            } else {
                self.select(lane, nearest_point(lane, p), mods.shift);
            }
            response.request_focus();
        }
        if response.drag_started()
            && let Some(origin) = origin
        {
            response.request_focus();
            let source = lane.source.clone();
            if let Some(index) = nearest_point(lane, origin) {
                if !self
                    .selected
                    .get(&source)
                    .is_some_and(|set| set.contains(&index))
                {
                    self.select(lane, Some(index), false);
                }
                let copy = mods.alt;
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: if copy {
                        "undo-automation-copy-points"
                    } else {
                        "undo-automation-move-points"
                    }
                    .to_owned(),
                }));
                self.drag = Some(Drag::Move {
                    source: source.clone(),
                    index,
                    origin,
                    points: lane.points.to_vec(),
                    indices: self.selected[&source].clone(),
                    copy,
                });
            } else if let Some(index) = closest_handle(lane, origin) {
                let initial = match lane.points[index].shape {
                    Shape::Curve { tension } => tension,
                    _ => 0.0,
                };
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: "undo-automation-bend".to_owned(),
                }));
                self.drag = Some(Drag::Bend {
                    source,
                    index,
                    origin,
                    initial,
                });
            } else if mods.command {
                self.drag = Some(Drag::Draw {
                    source,
                    raw: vec![Point::linear(
                        lane.tick(origin.x, true, meter),
                        lane.value(origin.y),
                    )],
                });
            } else {
                self.select(lane, None, mods.shift);
                self.drag = Some(Drag::Box {
                    source,
                    origin,
                    end: origin,
                    append: mods.shift,
                });
            }
        }
        if let Some(drag) = self.drag.as_mut().filter(|d| d.source() == &lane.source) {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                if matches!(drag, Drag::Move { .. } | Drag::Bend { .. }) {
                    ctx.send(Action::Edit(EditAction::CancelGesture));
                }
                self.drag = None;
                return;
            }
            if let Some(pointer) = pointer.filter(|_| response.dragged() || response.drag_stopped())
            {
                match drag {
                    Drag::Move {
                        index,
                        origin,
                        points,
                        indices,
                        copy,
                        ..
                    } => {
                        let raw_tick = lane.axis.x(points[*index].tick) + pointer.x - origin.x;
                        let dx = lane.tick(raw_tick, mods.shift, meter) - points[*index].tick;
                        let dy = (origin.y - pointer.y) / lane.rect.height()
                            * if mods.shift { 0.1 } else { 1.0 };
                        let (points, selected) =
                            move_points(points, indices, dx, dy, lane.end, *copy);
                        let label = if *copy {
                            "undo-automation-copy-points"
                        } else {
                            "undo-automation-move-points"
                        };
                        self.selected.insert(lane.source.clone(), selected);
                        send_points(ctx, lane, points, label);
                    }
                    Drag::Bend {
                        index,
                        origin,
                        initial,
                        ..
                    } if *index < lane.points.len() => {
                        let mut points = lane.points.to_vec();
                        let tension = (*initial
                            + (origin.y - pointer.y) / (lane.rect.height() / 2.0))
                            .clamp(-1.0, 1.0);
                        points[*index].shape = if tension == 0.0 {
                            Shape::Linear
                        } else {
                            Shape::Curve { tension }
                        };
                        send_points(ctx, lane, points, "undo-automation-bend");
                    }
                    Drag::Draw { raw, .. } => {
                        let tick = lane.tick(pointer.x, true, meter);
                        let point = Point::linear(tick, lane.value(pointer.y));
                        if raw.last().is_none_or(|old| *old != point) {
                            raw.push(point);
                        }
                    }
                    Drag::Box { end, .. } => *end = pointer,
                    _ => {}
                }
            }
        }
        if response.drag_stopped()
            && let Some(Drag::Draw { raw, .. }) = self.drag.as_ref()
        {
            let normalized = Points::new(raw.clone());
            let thinned = automation::thin(
                normalized.as_slice(),
                lane.target.is_some_and(|t| t.info.steps() > 0),
            );
            let mut points = lane.points.to_vec();
            if let (Some(first), Some(last)) =
                (thinned.as_slice().first(), thinned.as_slice().last())
            {
                points.retain(|p| p.tick < first.tick || p.tick > last.tick);
                points.extend_from_slice(thinned.as_slice());
                send_points(ctx, lane, points, "undo-automation-draw");
            }
        }
    }

    fn keyboard(
        &mut self,
        ui: &Ui,
        ctx: &mut ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        response: &Response,
    ) {
        if !response.has_focus()
            || self.entry.is_some()
            || self.drag.is_some()
            || ContextMenu::<PointCommand>::is_open(ui.ctx(), response.id)
        {
            return;
        }
        let indices = self.selected.get(&lane.source).cloned().unwrap_or_default();
        let meter = ctx.project.meter;
        let consume = |mods, key| ui.input_mut(|i| i.consume_key(mods, key));
        if consume(Modifiers::COMMAND, Key::C) {
            let copied: Vec<Point> = indices
                .iter()
                .filter_map(|i| lane.points.get(*i).copied())
                .collect();
            if !copied.is_empty() {
                ui.ctx().data_mut(|data| {
                    data.insert_temp(Id::unique("komponist-automation-clipboard"), copied)
                });
            }
        } else if consume(Modifiers::COMMAND, Key::V) {
            if let Some(copied) = ui.ctx().data(|data| {
                data.get_temp::<Vec<Point>>(Id::unique("komponist-automation-clipboard"))
            }) && let Some(first) = copied.first()
            {
                let start = lane.head.max(Tick::ZERO);
                let mut points = lane.points.to_vec();
                points.extend(copied.iter().filter_map(|p| {
                    let tick = start + (p.tick - first.tick);
                    lane.end
                        .is_none_or(|end| tick <= end)
                        .then_some(Point { tick, ..*p })
                }));
                send_points(ctx, lane, points, "undo-automation-paste");
            }
        } else if consume(Modifiers::NONE, Key::Delete) || consume(Modifiers::NONE, Key::Backspace)
        {
            self.delete_points(ctx, lane, &indices);
        } else if consume(Modifiers::NONE, Key::Enter) {
            if let Some(&index) = indices.first() {
                self.open_entry(ctx, lane, index);
            }
        } else {
            let dx = if consume(Modifiers::NONE, Key::ArrowLeft) {
                -lane.grid.ticks(meter)
            } else if consume(Modifiers::NONE, Key::ArrowRight) {
                lane.grid.ticks(meter)
            } else {
                Tick::ZERO
            };
            let dy = if consume(Modifiers::SHIFT, Key::ArrowUp) {
                0.001
            } else if consume(Modifiers::SHIFT, Key::ArrowDown) {
                -0.001
            } else if consume(Modifiers::NONE, Key::ArrowUp) {
                0.01
            } else if consume(Modifiers::NONE, Key::ArrowDown) {
                -0.01
            } else {
                0.0
            };
            if dx != Tick::ZERO || dy != 0.0 {
                let (points, _) = move_points(lane.points, &indices, dx, dy, lane.end, false);
                send_points(ctx, lane, points, "undo-automation-move-points");
            }
        }
    }

    fn point_menu(
        &mut self,
        ui: &Ui,
        ctx: &mut ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        response: &Response,
    ) {
        if response.secondary_clicked() {
            let index = response.interact_pointer_pos().and_then(|p| {
                nearest_point(lane, p).or_else(|| {
                    lane.points
                        .partition_point(|point| lane.axis.x(point.tick) <= p.x)
                        .checked_sub(1)
                })
            });
            self.menu_point = index.map(|i| (lane.source.clone(), i));
            if let Some(index) = index
                && !self
                    .selected
                    .get(&lane.source)
                    .is_some_and(|s| s.contains(&index))
            {
                self.select(lane, Some(index), false);
            }
        }
        let Some((_, index)) = self
            .menu_point
            .as_ref()
            .filter(|(source, index)| source == &lane.source && *index < lane.points.len())
        else {
            return;
        };
        let index = *index;
        let lang = ctx.lang;
        let shapes = [
            (lang.automation_shape_linear(), Shape::Linear),
            (lang.automation_shape_curve(), Shape::Curve { tension: 0.5 }),
            (lang.automation_shape_hold(), Shape::Hold),
        ]
        .map(|(label, shape)| {
            Entry::action(label, PointCommand::Shape(shape)).checked(
                std::mem::discriminant(&lane.points[index].shape) == std::mem::discriminant(&shape),
            )
        });
        let entries = [
            Entry::submenu(lang.automation_shape(), &shapes),
            Entry::action(lang.devices_enter_value(), PointCommand::Value),
            Entry::action(lang.automation_delete_points(), PointCommand::Delete),
        ];
        match ContextMenu::new(lang.automation_title(), &entries).show_on(response) {
            Some(PointCommand::Shape(shape)) => {
                let mut points = lane.points.to_vec();
                points[index].shape = shape;
                send_points(ctx, lane, points, "undo-automation-shape");
            }
            Some(PointCommand::Value) => self.open_entry(ctx, lane, index),
            Some(PointCommand::Delete) => {
                let indices = self
                    .selected
                    .get(&lane.source)
                    .cloned()
                    .unwrap_or_else(|| BTreeSet::from([index]));
                self.delete_points(ctx, lane, &indices);
            }
            None => {}
        }
        if !ContextMenu::<PointCommand>::is_open(ui.ctx(), response.id) {
            self.menu_point = None;
        }
    }

    fn delete_points(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        indices: &BTreeSet<usize>,
    ) {
        let points = lane
            .points
            .iter()
            .enumerate()
            .filter_map(|(i, p)| (!indices.contains(&i)).then_some(*p))
            .collect();
        send_points(ctx, lane, points, "undo-automation-delete-points");
        self.selected.remove(&lane.source);
    }

    fn open_entry(&mut self, ctx: &ViewCtx<'_>, lane: &LaneFrame<'_>, index: usize) {
        if let Some(point) = lane.points.get(index) {
            self.entry = Some(ValueEntry {
                source: lane.source.clone(),
                index,
                text: lane.format(ctx, point.value),
            });
        }
    }

    fn value_entry(&mut self, ui: &Ui, ctx: &mut ViewCtx<'_>, lane: &LaneFrame<'_>, id: Id) {
        let Some(entry) = self
            .entry
            .as_mut()
            .filter(|entry| entry.source == lane.source)
        else {
            return;
        };
        let Some(point) = lane.points.get(entry.index).copied() else {
            self.entry = None;
            return;
        };
        let mut commit = false;
        let mut cancel = false;
        let popup = egui::Area::new(id.with("automation-value"))
            .order(egui::Order::Foreground)
            .fixed_pos(lane.pos(point))
            .constrain(true)
            .show(ui.ctx(), |ui| {
                widgets::menu_frame().show(ui, |ui| {
                    let text = ui.add(
                        egui::TextEdit::singleline(&mut entry.text)
                            .id(id.with("automation-value-text"))
                            .desired_width(space::SPACE_8 * 2.0),
                    );
                    text.widget_info(|| {
                        WidgetInfo::labeled(Role::TextInput, true, ctx.lang.devices_enter_value())
                    });
                    commit = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
                        || (text.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
                    cancel = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    if !commit && !cancel {
                        text.request_focus();
                    }
                });
            });
        let outside = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|pos| !popup.response.rect.contains(pos))
        });
        if commit && let Some(value) = lane.parse(&entry.text) {
            let mut points = lane.points.to_vec();
            points[entry.index].value = value;
            send_points(ctx, lane, points, "undo-automation-value");
        }
        if commit || cancel || outside {
            self.entry = None;
            ui.memory_mut(|mem| mem.request_focus(id));
        }
    }

    fn draw_drag(
        &mut self,
        ui: &Ui,
        _ctx: &ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        painter: &egui::Painter,
    ) {
        match &self.drag {
            Some(Drag::Draw { source, raw }) if source == &lane.source => {
                let path = raw.iter().copied().map(|p| lane.pos(p)).collect();
                painter.add(egui::Shape::line(
                    path,
                    Stroke::new(m::CURVE_WIDTH, color32(color::ACCENT)),
                ));
            }
            Some(Drag::Box {
                source,
                origin,
                end,
                append,
            }) if in_same_group(source, &lane.source) => {
                let pointer = ui.input(|i| i.pointer.interact_pos()).unwrap_or(*end);
                let rect = Rect::from_two_pos(*origin, pointer);
                let hits = visible_range(lane, rect.intersect(lane.rect))
                    .filter(|i| rect.contains(lane.pos(lane.points[*i])));
                let selected = self.selected.entry(lane.source.clone()).or_default();
                if !append {
                    selected.clear();
                }
                selected.extend(hits);
                let paint = rect.intersect(lane.rect);
                if paint.is_positive() {
                    px::fill(
                        painter,
                        paint,
                        0.0,
                        color32(color::ACCENT).gamma_multiply(0.15),
                    );
                    px::inner_border(painter, paint, color32(color::ACCENT));
                }
            }
            _ => {}
        }
    }
}

fn in_same_group(a: &Source, b: &Source) -> bool {
    match (a, b) {
        (Source::Lane(a, _), Source::Lane(b, _)) => a == b,
        (Source::Clip(a, _), Source::Clip(b, _)) => a == b,
        _ => false,
    }
}

fn send_points(
    ctx: &mut ViewCtx<'_>,
    lane: &LaneFrame<'_>,
    points: Vec<Point>,
    label: &'static str,
) {
    let points = Points::new(points);
    if points.as_slice() != lane.points {
        ctx.send(Action::Automation(Automation::Points(
            lane.source.clone(),
            points,
            label,
        )));
    }
}

fn move_points(
    points: &[Point],
    indices: &BTreeSet<usize>,
    dx: Tick,
    dy: f32,
    end: Option<Tick>,
    copy: bool,
) -> (Vec<Point>, BTreeSet<usize>) {
    let mut min = i64::MIN;
    let mut max = i64::MAX;
    let mut low: f32 = 0.0;
    let mut high: f32 = 1.0;
    for &i in indices {
        let Some(point) = points.get(i) else {
            continue;
        };
        min = min.max(-point.tick.0);
        if let Some(end) = end {
            max = max.min(end.0.saturating_sub(point.tick.0));
        }
        low = low.max(point.value);
        high = high.min(point.value);
        if !copy {
            if i > 0 && !indices.contains(&(i - 1)) {
                min = min.max((points[i - 1].tick.0 - point.tick.0 + 1).min(0));
            }
            if i + 1 < points.len() && !indices.contains(&(i + 1)) {
                max = max.min((points[i + 1].tick.0 - point.tick.0 - 1).max(0));
            }
        }
    }
    let dx = dx.0.clamp(min.min(0), max.max(0));
    let dy = dy.clamp(-high, 1.0 - low);
    let mut next = Vec::with_capacity(points.len() + if copy { indices.len() } else { 0 });
    for (i, p) in points.iter().copied().enumerate() {
        let chosen = indices.contains(&i);
        if !chosen || copy {
            next.push((p, false));
        }
        if chosen && (!copy || dx != 0 || dy != 0.0) {
            next.push((
                Point {
                    tick: Tick(p.tick.0.saturating_add(dx)),
                    value: p.value + dy,
                    ..p
                },
                true,
            ));
        }
    }
    next.sort_by_key(|(point, _)| point.tick);
    if copy && next.windows(3).any(|w| w[0].0.tick == w[2].0.tick) {
        return (points.to_vec(), indices.clone());
    }
    let selected = next
        .iter()
        .enumerate()
        .filter_map(|(i, (_, chosen))| chosen.then_some(i))
        .collect();
    (next.into_iter().map(|(point, _)| point).collect(), selected)
}

fn visible_range(lane: &LaneFrame<'_>, rect: Rect) -> Range<usize> {
    if rect.width() < 0.0 {
        return 0..0;
    }
    let (start, end) = lane.axis.ticks_in(rect.x_range());
    let lo = lane.points.partition_point(|p| p.tick < start);
    let hi = lane.points.partition_point(|p| p.tick <= end);
    lo..hi
}

fn nearest_point(lane: &LaneFrame<'_>, pos: Pos2) -> Option<usize> {
    let window = Rect::from_center_size(pos, vec2(m::POINT_HIT, m::POINT_HIT));
    visible_range(lane, window)
        .map(|i| (i, lane.pos(lane.points[i]).distance_sq(pos)))
        .filter(|(_, d)| *d <= (m::POINT_HIT / 2.0).powi(2))
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i)
}

fn handle_pos(lane: &LaneFrame<'_>, i: usize) -> Option<Pos2> {
    let a = lane.points[i];
    let b = *lane.points.get(i + 1)?;
    if lane.axis.width(b.tick - a.tick) < m::POINT_HIT * 2.0 {
        return None;
    }
    let tick = Tick(a.tick.0 + (b.tick.0 - a.tick.0) / 2);
    let value = automation::value_at(lane.points, tick)?;
    Some(lane.pos(Point::linear(tick, value)))
}

fn closest_handle(lane: &LaneFrame<'_>, pos: Pos2) -> Option<usize> {
    let tick = Tick(lane.axis.tick_at(pos.x).floor() as i64);
    let i = lane
        .points
        .partition_point(|p| p.tick <= tick)
        .checked_sub(1)?;
    (handle_pos(lane, i)?.distance(pos) < m::POINT_HIT / 2.0).then_some(i)
}

fn paint_diamond(painter: &egui::Painter, center: Pos2, ink: Color32) {
    let r = m::HANDLE / 2.0;
    let points = vec![
        center + vec2(0.0, -r),
        center + vec2(r, 0.0),
        center + vec2(0.0, r),
        center + vec2(-r, 0.0),
    ];
    painter.add(egui::Shape::convex_polygon(
        points,
        color32(color::SURFACE_1),
        Stroke::new(line::WIDTH, ink),
    ));
}

fn paint_background(painter: &egui::Painter, meter: Meter, lane: &LaneFrame<'_>) {
    px::fill(painter, lane.rect, 0.0, color32(color::SURFACE_1));
    let spec = GridSpec::new(lane.grid, space::SPACE_2);
    let grid = grid_lines(&lane.axis, meter, lane.rect.x_range(), &spec);
    paint_grid(painter, lane.rect, &grid);
    let center = lane.rect.center().y;
    let guide = [
        pos2(lane.rect.left(), center),
        pos2(lane.rect.right(), center),
    ];
    painter.extend(egui::Shape::dashed_line(
        &guide,
        Stroke::new(line::WIDTH, color32(color::LINE_0)),
        space::SPACE_1,
        space::SPACE_1,
    ));
}

fn span_rect(lane: &LaneFrame<'_>, span: &Override) -> Rect {
    Rect::from_min_max(
        pos2(lane.axis.x(span.start), lane.rect.top()),
        pos2(lane.axis.x(span.end), lane.rect.bottom()),
    )
}

fn hatch(painter: &egui::Painter, rect: Rect) {
    px::fill(painter, rect, 0.0, color32(color::CANVAS_OUTSIDE_CLIP));
    let spacing = space::SPACE_2;
    let slope = rect.height();
    let mut x = rect.left() - slope;
    while x < rect.right() {
        painter.line_segment(
            [pos2(x, rect.bottom()), pos2(x + slope, rect.top())],
            Stroke::new(line::WIDTH, color32(color::LINE_0)),
        );
        x += spacing;
    }
}

fn paint_curve(
    painter: &egui::Painter,
    lane: &LaneFrame<'_>,
    points: &[Point],
    rect: Rect,
    ink: Color32,
    fill: Option<Color32>,
    dashed: bool,
) {
    if points.is_empty() {
        return;
    }
    let (start, end) = lane.axis.ticks_in(rect.x_range());
    let lo = points.partition_point(|p| p.tick < start).saturating_sub(1);
    let hi = (points.partition_point(|p| p.tick <= end) + 1).min(points.len());
    let mut path = Vec::with_capacity((hi - lo).min(rect.width() as usize * 2 + 2));
    let boundary = |x: f32| {
        let tick = Tick(lane.axis.tick_at(x).round() as i64);
        let value = automation::value_at(points, tick).unwrap_or_default();
        pos2(x, lane.pos(Point::linear(tick, value)).y)
    };
    path.push(boundary(rect.left()));
    for pair in points[lo..hi].windows(2) {
        let a = pair[0];
        let b = pair[1];
        let from = lane.pos(a);
        let to = lane.pos(b);
        if from.x >= rect.left() {
            path.push(from);
        }
        match a.shape {
            Shape::Hold if to.x <= rect.right() => path.push(pos2(to.x, from.y)),
            Shape::Curve { tension } if to.x > from.x => {
                let from_x = from.x.max(rect.left());
                let to_x = to.x.min(rect.right());
                let mut x = from_x + m::POINT_DENSITY;
                while x < to_x {
                    let fraction = (x - from.x) / (to.x - from.x);
                    let value = egui::lerp(a.value..=b.value, automation::bend(fraction, tension));
                    path.push(pos2(x, lane.pos(Point::linear(a.tick, value)).y));
                    x += m::POINT_DENSITY;
                }
            }
            _ => {}
        }
    }
    if let Some(last) = points.get(hi - 1).copied() {
        let p = lane.pos(last);
        if rect.x_range().contains(p.x) {
            path.push(p);
        }
    }
    path.push(boundary(rect.right()));

    // Keep the extremes of each pixel column so dense recordings do not create a draw item for
    // every point, while fast transients stay visible.
    let mut compact = Vec::with_capacity(path.len());
    for column in path.chunk_by(|a, b| a.x.floor() == b.x.floor()) {
        let min = column
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| a.y.total_cmp(&b.y))
            .map(|(i, _)| i)
            .unwrap();
        let max = column
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.y.total_cmp(&b.y))
            .map(|(i, _)| i)
            .unwrap();
        compact.push(column[min.min(max)]);
        if min != max {
            compact.push(column[min.max(max)]);
        }
    }
    if let Some(fill) = fill {
        let mut mesh = egui::Mesh::default();
        for pair in compact.windows(2) {
            let a = pair[0];
            let b = pair[1];
            let index = mesh.vertices.len() as u32;
            for pos in [a, b, pos2(b.x, rect.bottom()), pos2(a.x, rect.bottom())] {
                mesh.colored_vertex(pos, fill);
            }
            mesh.add_triangle(index, index + 1, index + 2);
            mesh.add_triangle(index, index + 2, index + 3);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
    if dashed {
        painter.extend(egui::Shape::dashed_line(
            &compact,
            Stroke::new(m::CURVE_WIDTH, ink),
            space::SPACE_1_5,
            space::SPACE_1,
        ));
    } else {
        painter.add(egui::Shape::line(compact, Stroke::new(m::CURVE_WIDTH, ink)));
    }
}
