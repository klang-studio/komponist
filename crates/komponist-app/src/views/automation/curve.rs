//! The shared normalized curve canvas. The arrangement supplies song ticks and the clip editors
//! supply content ticks. Edits are sent through the dispatcher as one undo step per gesture.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use egui::accesskit::Action as AccessAction;
use egui::{
    Color32, Id, Key, Modifiers, Pos2, Rect, Response, Role, Sense, Stroke, Ui, WidgetInfo, pos2,
    vec2,
};
use komponist_model::automation::{self, Point, Points, Shape};
use komponist_model::format::fmt_pos;
use komponist_model::{Grid, Meter, Project, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, line, space};
use komponist_ui::timeline::{GridSpec, TimeAxis, grid_lines, paint_grid, paint_playhead};
use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{px, widgets};

use super::chooser::percentage;
use super::{Action as Automation, Source, Target, lanes, metrics as m};
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

#[derive(Clone)]
pub(crate) struct LaneFrame<'a> {
    pub source: Source,
    pub label: &'a str,
    pub access: &'a str,
    pub warning: Option<&'a str>,
    pub points: &'a Points,
    pub target: Option<&'a Target>,
    pub rect: Rect,
    pub axis: TimeAxis,
    pub grid: Grid,
    pub snap: bool,
    pub end: Option<Tick>,
    pub head: Tick,
}

impl LaneFrame<'_> {
    fn points(&self) -> &[Point] {
        self.points.as_slice()
    }

    fn editable_rect(&self) -> Rect {
        self.end.map_or(self.rect, |end| {
            self.rect.intersect(Rect::from_x_y_ranges(
                self.axis.x(Tick::ZERO)..=self.axis.x(end),
                self.rect.y_range(),
            ))
        })
    }

    fn value_rect(&self) -> Rect {
        if matches!(self.source, Source::Clip(..)) {
            self.rect.shrink2(vec2(
                0.0,
                m::CLIP_VALUE_INSET.min(self.rect.height() * 0.25),
            ))
        } else {
            self.rect
        }
    }

    fn curve_width(&self) -> f32 {
        if matches!(self.source, Source::Clip(..)) {
            m::CLIP_CURVE_WIDTH
        } else {
            m::CURVE_WIDTH
        }
    }

    fn pos(&self, point: Point) -> Pos2 {
        let values = self.value_rect();
        pos2(
            self.axis.x(point.tick),
            egui::lerp(values.bottom()..=values.top(), point.value),
        )
    }

    fn value(&self, y: f32) -> f32 {
        let values = self.value_rect();
        ((values.bottom() - y) / values.height()).clamp(0.0, 1.0)
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
    /// The points each selection's indices refer to. A selection is dropped as soon as its lane
    /// holds other points than these (undo, redo, automation write, macro learn, another editor).
    basis: HashMap<Source, Points>,
    active: Option<Source>,
    focus_request: Option<Source>,
    drag: Option<Drag>,
    menu_point: Option<(Source, usize)>,
    entry: Option<ValueEntry>,
    dense_curves: HashMap<Source, DenseCurve>,
    paint_frame: u64,
    pruned_frame: Option<u64>,
}

/// A dense trace's triangles are reused until its points, zoom, viewport or colours change.
#[derive(Debug)]
struct DenseCurve {
    points: Points,
    settings: MeshSettings,
    mesh: Arc<egui::Mesh>,
    frame: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MeshSettings {
    axis: TimeAxis,
    lane: Rect,
    visible: Rect,
    trace: Rect,
    width: f32,
    ink: Color32,
    fill: Color32,
    pixels_per_point: f32,
    meter: Meter,
    grid: Grid,
    end: Option<Tick>,
}

#[derive(Debug)]
struct ValueEntry {
    source: Source,
    index: usize,
    text: String,
    text_id: Id,
    seen: bool,
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
        base: HashMap<Source, BTreeSet<usize>>,
        frame: Option<u64>,
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
    pub fn focus(&mut self, source: Source) {
        self.selected.clear();
        self.active = Some(source.clone());
        self.focus_request = Some(source);
    }

    /// Drops every selection whose lane no longer holds the `Points` it was made on: its indices
    /// would name other points after an undo, a redo or any edit from outside this editor. Edits
    /// of this editor keep their selection because `send_points` records the points it sends.
    /// Runs once per frame, before the first lane is shown: edits sent earlier in the same frame
    /// reach the project only after it, so a later lane would see them as foreign.
    fn prune_stale(&mut self, project: &Project) {
        if self.pruned_frame.replace(self.paint_frame) == Some(self.paint_frame) {
            return;
        }
        let basis = &self.basis;
        self.selected.retain(|source, _| {
            source_points(project, source)
                .zip(basis.get(source))
                .is_some_and(|(now, then)| std::ptr::eq(now.as_slice(), then.as_slice()))
        });
        self.basis
            .retain(|source, _| self.selected.contains_key(source));
        // A Shift-marquee restores its base selection every frame: a lane dropped here must leave
        // that base too, or the marquee would bring back its stale indices on the new points.
        if let Some(Drag::Box { base, .. }) = self.drag.as_mut() {
            base.retain(|source, _| self.selected.contains_key(source));
        }
    }

    pub fn selected(&self, source: &Source) -> bool {
        self.active.as_ref() == Some(source)
    }

    pub fn finish_frame(&mut self, ui: &Ui, ctx: &mut ViewCtx<'_>) {
        self.dense_curves
            .retain(|_, entry| entry.frame == self.paint_frame);
        self.paint_frame = self.paint_frame.wrapping_add(1);
        if let Some(entry) = self.entry.as_mut()
            && !std::mem::take(&mut entry.seen)
        {
            ui.memory_mut(|mem| mem.surrender_focus(entry.text_id));
            self.entry = None;
        }
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
        self.prune_stale(ctx.project);
        let visible = lane.rect.intersect(ui.clip_rect());
        if !visible.is_positive() {
            return;
        }
        let id = ui.scope_id().with(("automation-curve", &lane.source));
        let editable = lane.editable_rect().intersect(visible);
        let response = if editable.is_positive() {
            ui.interact(editable, id, Sense::click_and_drag())
        } else {
            ui.interact(visible, id, Sense::hover())
        };
        if self.focus_request.as_ref() == Some(&lane.source) {
            response.request_focus();
            self.focus_request = None;
        }
        response.widget_info(|| WidgetInfo::labeled(Role::Group, true, lane.access));
        widgets::accesskit(&response, |node| {
            if let Some(warning) = lane.warning {
                node.set_description(warning);
            }
        });
        let painter = ui.painter().with_clip_rect(visible);
        let hit_rect = editable.expand(m::POINT_HIT);
        let indices = visible_range(lane, hit_rect);
        let dense = indices.len() as f32 * m::POINT_DENSITY > editable.width();
        let written_ranges =
            ctx.automation_capture
                .map_or_else(Vec::new, |capture| match &lane.source {
                    Source::Lane(owner, id) => capture.preview_lane_ranges(*owner, *id),
                    Source::Clip(clip, target) => capture.preview_clip_ranges(*clip, target),
                });
        let preview = ctx
            .automation_capture
            .filter(|_| !written_ranges.is_empty())
            .and_then(|capture| match &lane.source {
                Source::Lane(owner, id) => capture.preview_lane(*owner, *id, lane.points),
                Source::Clip(clip, target) => capture.preview_clip(*clip, target, lane.points),
            });
        let contour = preview.as_ref().map_or(lane.points(), Points::as_slice);
        let contour = &contour[editable_range(contour, lane.end)];
        let trace = editable;
        if dense && preview.is_none() && trace.is_positive() {
            let settings = MeshSettings {
                axis: lane.axis,
                lane: lane.rect,
                visible,
                trace,
                width: lane.curve_width(),
                ink: paint.curve,
                fill: paint.fill,
                pixels_per_point: painter.pixels_per_point(),
                meter: ctx.project.meter,
                grid: lane.grid,
                end: lane.end,
            };
            self.paint_dense(&painter, lane, contour, settings);
        } else {
            paint_background(&painter, ctx.project.meter, lane);
            if trace.is_positive() {
                paint_curve(
                    &painter.with_clip_rect(trace),
                    lane,
                    contour,
                    trace,
                    paint.curve,
                    Some(paint.fill),
                    false,
                );
            }
        }
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
        if preview.is_some() {
            let record = color32(color::STATE_RECORD);
            for (start, end) in written_ranges {
                let bounds = span_rect(lane, &Override { start, end })
                    .expand(line::WIDTH)
                    .intersect(trace);
                if bounds.is_positive() {
                    paint_curve(
                        &painter.with_clip_rect(bounds),
                        lane,
                        contour,
                        bounds,
                        record,
                        Some(record.gamma_multiply(m::RECORD_FILL_ALPHA)),
                        false,
                    );
                }
            }
        }
        if let Some(end) = paint.beyond {
            let outside =
                Rect::from_min_max(pos2(lane.axis.x(end), lane.rect.top()), lane.rect.max)
                    .intersect(visible);
            if outside.is_positive() {
                px::fill(&painter, outside, 0.0, color32(color::CANVAS_OUTSIDE_CLIP));
            }
        }
        if contour.is_empty() && paint.overrides.is_empty() && editable.is_positive() {
            painter.text(
                editable.center(),
                egui::Align2::CENTER_CENTER,
                ctx.lang.automation_lane_empty(),
                komponist_ui::fonts::font_id(&komponist_tokens::typography::SMALL),
                color32(color::TEXT_3),
            );
        }

        if !editable.is_positive() {
            return;
        }
        if let Some(selected) = self.selected.get_mut(&lane.source)
            && lane.end.is_some()
        {
            let range = editable_range(lane.points(), lane.end);
            selected.retain(|i| range.contains(i));
        }
        let pointer = ui
            .input(|i| i.pointer.hover_pos())
            .filter(|p| editable.contains(*p));
        let handle = pointer.and_then(|p| closest_handle(lane, p));
        let hovered = pointer.and_then(|p| nearest_point(lane, p));
        // Only an active accessibility tree needs individual nodes for unmarked points.
        let expose_dense = dense
            && ui
                .ctx()
                .accesskit_node_builder(response.id, |_| ())
                .is_some();
        let mut point_action = None;
        let mut tooltip = None;
        for i in indices {
            let point = lane.points()[i];
            let pos = lane.pos(point);
            let selected = self
                .selected
                .get(&lane.source)
                .is_some_and(|set| set.contains(&i));
            let marker = !dense || selected || hovered == Some(i);
            if marker && hit_rect.contains(pos) {
                let (fill, ring) = if selected {
                    (color32(color::TEXT_1), color32(color::ACCENT))
                } else {
                    (paint.curve, color32(color::SURFACE_0))
                };
                painter.circle(pos, m::POINT / 2.0, fill, Stroke::new(line::WIDTH, ring));
            }
            if (marker || expose_dense)
                && let bounds = Rect::from_center_size(pos, vec2(m::POINT_HIT, m::POINT_HIT))
                    .intersect(editable)
                && bounds.is_positive()
            {
                let node = ui.interact(
                    bounds,
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
            let mut points = lane.points().to_vec();
            points[point].value =
                (points[point].value + steps as f32 * m::VALUE_STEP).clamp(0.0, 1.0);
            send_points(&mut self.basis, ctx, lane, points, "undo-automation-value");
        }

        if response.hovered() {
            ctx.ui.automation_hint = true;
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
        self.basis.insert(lane.source.clone(), lane.points.clone());
        let points = self.selected.entry(lane.source.clone()).or_default();
        if let Some(i) = index
            && (!append || !points.remove(&i))
        {
            points.insert(i);
        }
    }

    fn paint_dense(
        &mut self,
        painter: &egui::Painter,
        lane: &LaneFrame<'_>,
        contour: &[Point],
        settings: MeshSettings,
    ) {
        let cached = self.dense_curves.get_mut(&lane.source).filter(|curve| {
            curve.settings == settings && std::ptr::eq(curve.points.as_slice(), lane.points())
        });
        let mesh = if let Some(curve) = cached {
            curve.frame = self.paint_frame;
            Arc::clone(&curve.mesh)
        } else {
            let path = curve_path(lane, contour, settings.trace);
            let mut mesh = dense_background(&settings);
            let has_jump = contour.windows(2).any(|pair| {
                pair[0].value != pair[1].value
                    && (pair[0].shape == Shape::Hold || pair[0].tick == pair[1].tick)
            });
            mesh.append(if has_jump {
                dense_jump_mesh(&path, &settings)
            } else {
                dense_band(&path, &settings)
            });
            let mesh = Arc::new(mesh);
            self.dense_curves.insert(
                lane.source.clone(),
                DenseCurve {
                    points: lane.points.clone(),
                    settings,
                    mesh: Arc::clone(&mesh),
                    frame: self.paint_frame,
                },
            );
            mesh
        };
        painter.add(egui::Shape::Mesh(mesh));
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
            let mut points = lane.points().to_vec();
            if let Some(i) = nearest_point(lane, p) {
                points.remove(i);
                send_points(
                    &mut self.basis,
                    ctx,
                    lane,
                    points,
                    "undo-automation-delete-points",
                );
            } else if lane.end.is_none_or(|end| p.x <= lane.axis.x(end)) {
                let point = Point::linear(lane.tick(p.x, mods.shift, meter), lane.value(p.y));
                points.push(point);
                send_points(
                    &mut self.basis,
                    ctx,
                    lane,
                    points,
                    "undo-automation-add-point",
                );
            }
            self.select(lane, None, false);
            response.request_focus();
        } else if response.clicked()
            && let Some(p) = pointer
        {
            if mods.alt
                && let Some(index) = closest_handle(lane, p)
            {
                let mut points = lane.points().to_vec();
                points[index].shape = Shape::Linear;
                send_points(&mut self.basis, ctx, lane, points, "undo-automation-shape");
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
                    points: lane.points().to_vec(),
                    indices: self.selected[&source].clone(),
                    copy,
                });
            } else if let Some(index) = closest_handle(lane, origin) {
                let initial = match lane.points()[index].shape {
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
                    base: self.selected.clone(),
                    frame: None,
                });
            }
        }
        if let Some(drag) = self.drag.as_mut().filter(|d| d.source() == &lane.source) {
            // The app's keymap consumes a plain Esc first and cancels the gesture itself; this
            // editor then only has to stop the drag, or it would keep sending points after the
            // cancel. An Esc the keymap left (with modifiers, or one it skipped) is taken here,
            // whatever its modifiers, and cancels the gesture, so a stopped drag never leaves
            // its gesture open.
            let consumed = ui.input_mut(|i| {
                let before = i.events.len();
                i.events.retain(|e| {
                    !matches!(
                        e,
                        egui::Event::Key {
                            key: Key::Escape,
                            pressed: true,
                            ..
                        }
                    )
                });
                i.events.len() != before
            });
            if consumed || crate::views::arrangement::escape_pressed(ui) {
                if consumed && matches!(drag, Drag::Move { .. } | Drag::Bend { .. }) {
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
                        let dy = (origin.y - pointer.y) / lane.value_rect().height().max(1.0)
                            * if mods.shift { m::FINE_DRAG_FACTOR } else { 1.0 };
                        let rank = indices.iter().position(|i| i == index);
                        let original = points[*index];
                        let (points, selected) =
                            move_points(points, indices, dx, dy, lane.end, *copy);
                        let point = rank
                            .and_then(|rank| selected.iter().nth(rank))
                            .map_or(original, |i| points[*i]);
                        show_drag_value(ui, ctx, lane, response.id, point);
                        let label = if *copy {
                            "undo-automation-copy-points"
                        } else {
                            "undo-automation-move-points"
                        };
                        self.selected.insert(lane.source.clone(), selected);
                        send_points(&mut self.basis, ctx, lane, points, label);
                    }
                    Drag::Bend {
                        index,
                        origin,
                        initial,
                        ..
                    } if *index < lane.points().len() => {
                        let mut points = lane.points().to_vec();
                        let tension = (*initial
                            + (origin.y - pointer.y) / (lane.rect.height() / 2.0))
                            .clamp(-1.0, 1.0);
                        points[*index].shape = if tension == 0.0 {
                            Shape::Linear
                        } else {
                            Shape::Curve { tension }
                        };
                        send_points(&mut self.basis, ctx, lane, points, "undo-automation-bend");
                    }
                    Drag::Draw { raw, .. } => {
                        let tick = lane.tick(pointer.x, true, meter);
                        let point = Point::linear(tick, lane.value(pointer.y));
                        show_drag_value(ui, ctx, lane, response.id, point);
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
            let mut points = lane.points().to_vec();
            if let (Some(first), Some(last)) =
                (thinned.as_slice().first(), thinned.as_slice().last())
            {
                points.retain(|p| p.tick < first.tick || p.tick > last.tick);
                points.extend_from_slice(thinned.as_slice());
                send_points(&mut self.basis, ctx, lane, points, "undo-automation-draw");
                self.selected.remove(&lane.source);
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
            || self
                .entry
                .as_ref()
                .is_some_and(|entry| entry.source == lane.source)
            || self.drag.is_some()
            || (self.menu_point.as_ref().is_some_and(|(source, index)| {
                source == &lane.source && editable_range(lane.points(), lane.end).contains(index)
            }) && ContextMenu::<PointCommand>::is_open(ui.ctx(), response.id))
        {
            return;
        }
        let indices = self.selected.get(&lane.source).cloned().unwrap_or_default();
        let meter = ctx.project.meter;
        let consume = |mods, key| ui.input_mut(|i| i.consume_key(mods, key));
        if consume(Modifiers::COMMAND, Key::C) {
            let copied: Vec<Point> = indices
                .iter()
                .filter_map(|i| lane.points().get(*i).copied())
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
                let mut points = lane.points().to_vec();
                points.extend(copied.iter().filter_map(|p| {
                    let tick = start + (p.tick - first.tick);
                    lane.end
                        .is_none_or(|end| tick <= end)
                        .then_some(Point { tick, ..*p })
                }));
                if send_points(&mut self.basis, ctx, lane, points, "undo-automation-paste") {
                    self.selected.clear();
                }
            }
        } else if consume(Modifiers::NONE, Key::Delete) || consume(Modifiers::NONE, Key::Backspace)
        {
            self.delete_points(ctx, lane);
        } else if consume(Modifiers::NONE, Key::Enter) {
            if let Some(&index) = indices.first() {
                self.open_entry(ctx, lane, index, response.id);
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
                m::VALUE_STEP_FINE
            } else if consume(Modifiers::SHIFT, Key::ArrowDown) {
                -m::VALUE_STEP_FINE
            } else if consume(Modifiers::NONE, Key::ArrowUp) {
                m::VALUE_STEP
            } else if consume(Modifiers::NONE, Key::ArrowDown) {
                -m::VALUE_STEP
            } else {
                0.0
            };
            if dx != Tick::ZERO || dy != 0.0 {
                let (points, _) = move_points(lane.points(), &indices, dx, dy, lane.end, false);
                send_points(
                    &mut self.basis,
                    ctx,
                    lane,
                    points,
                    "undo-automation-move-points",
                );
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
        let range = editable_range(lane.points(), lane.end);
        if response.secondary_clicked() {
            let index = response.interact_pointer_pos().and_then(|p| {
                nearest_point(lane, p).or_else(|| {
                    lane.points()
                        .partition_point(|point| lane.axis.x(point.tick) <= p.x)
                        .checked_sub(1)
                        .filter(|i| range.contains(i))
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
            .filter(|(source, index)| source == &lane.source && range.contains(index))
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
                std::mem::discriminant(&lane.points()[index].shape)
                    == std::mem::discriminant(&shape),
            )
        });
        let entries = [
            Entry::submenu(lang.automation_shape(), &shapes),
            Entry::action(lang.devices_enter_value(), PointCommand::Value),
            Entry::action(lang.automation_delete_points(), PointCommand::Delete),
        ];
        match ContextMenu::new(lang.automation_title(), &entries).show_on(response) {
            Some(PointCommand::Shape(shape)) => {
                let mut points = lane.points().to_vec();
                points[index].shape = shape;
                send_points(&mut self.basis, ctx, lane, points, "undo-automation-shape");
            }
            Some(PointCommand::Value) => self.open_entry(ctx, lane, index, response.id),
            Some(PointCommand::Delete) => self.delete_points(ctx, lane),
            None => {}
        }
        if !ContextMenu::<PointCommand>::is_open(ui.ctx(), response.id) {
            self.menu_point = None;
        }
    }

    fn delete_points(&mut self, ctx: &mut ViewCtx<'_>, lane: &LaneFrame<'_>) {
        let mut replacements = Vec::new();
        self.selected.retain(|source, indices| {
            if !in_same_group(source, &lane.source) {
                return true;
            }
            if let Some(points) = source_points(ctx.project, source) {
                let points = points.as_slice();
                let range = editable_range(points, lane.end);
                let remaining: Vec<_> = points
                    .iter()
                    .enumerate()
                    .filter_map(|(i, point)| {
                        (!range.contains(&i) || !indices.contains(&i)).then_some(*point)
                    })
                    .collect();
                if remaining.len() != points.len() {
                    replacements.push(Automation::Points(
                        source.clone(),
                        Points::new(remaining),
                        "undo-automation-delete-points",
                    ));
                }
            }
            false
        });
        let grouped = replacements.len() > 1;
        if grouped {
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: "undo-automation-delete-points".to_owned(),
            }));
        }
        for edit in replacements {
            ctx.send(Action::Automation(edit));
        }
        if grouped {
            ctx.send(Action::Edit(EditAction::EndGesture));
        }
    }

    fn open_entry(&mut self, ctx: &ViewCtx<'_>, lane: &LaneFrame<'_>, index: usize, id: Id) {
        if let Some(point) = lane.points().get(index) {
            self.entry = Some(ValueEntry {
                source: lane.source.clone(),
                index,
                text: lane.format(ctx, point.value),
                text_id: id.with("automation-value-text"),
                seen: false,
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
        let Some(&point) = lane
            .points()
            .get(entry.index)
            .filter(|_| editable_range(lane.points(), lane.end).contains(&entry.index))
        else {
            ui.memory_mut(|mem| mem.surrender_focus(entry.text_id));
            self.entry = None;
            return;
        };
        entry.seen = true;
        let mut value = None;
        let mut cancel = false;
        let popup = egui::Area::new(id.with("automation-value"))
            .order(egui::Order::Foreground)
            .fixed_pos(lane.pos(point))
            .constrain(true)
            .show(ui.ctx(), |ui| {
                widgets::menu_frame().show(ui, |ui| {
                    let text = ui.add(
                        egui::TextEdit::singleline(&mut entry.text)
                            .id(entry.text_id)
                            .desired_width(space::SPACE_8 * 2.0),
                    );
                    text.widget_info(|| {
                        WidgetInfo::labeled(Role::TextInput, true, ctx.lang.devices_enter_value())
                    });
                    let commit = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter))
                        || (text.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
                    cancel = ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    if commit && !cancel {
                        value = lane.parse(&entry.text);
                    }
                    if value.is_none() && !cancel {
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
        if !outside && let Some(value) = value {
            let mut points = lane.points().to_vec();
            points[entry.index].value = value;
            send_points(&mut self.basis, ctx, lane, points, "undo-automation-value");
        }
        if value.is_some() || cancel || outside {
            ui.memory_mut(|mem| {
                mem.surrender_focus(entry.text_id);
                if !outside {
                    mem.request_focus(id);
                }
            });
            self.entry = None;
        }
    }

    fn draw_drag(
        &mut self,
        ui: &Ui,
        _ctx: &ViewCtx<'_>,
        lane: &LaneFrame<'_>,
        painter: &egui::Painter,
    ) {
        match &mut self.drag {
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
                base,
                frame,
            }) if in_same_group(source, &lane.source) => {
                let pointer = ui.input(|i| i.pointer.interact_pos()).unwrap_or(*end);
                let rect = Rect::from_two_pos(*origin, pointer);
                let paint = rect.intersect(lane.editable_rect());
                let hits = visible_range(lane, paint)
                    .filter(|i| paint.contains(lane.pos(lane.points()[*i])));
                let current = ui.ctx().cumulative_pass_nr();
                if *frame != Some(current) {
                    self.selected.clone_from(base);
                    *frame = Some(current);
                }
                self.basis.insert(lane.source.clone(), lane.points.clone());
                self.selected
                    .entry(lane.source.clone())
                    .or_default()
                    .extend(hits);
                if paint.is_positive() {
                    px::fill(
                        painter,
                        paint,
                        0.0,
                        color32(color::ACCENT).gamma_multiply(m::MARQUEE_FILL_ALPHA),
                    );
                    px::inner_border(painter, paint, color32(color::ACCENT));
                }
            }
            _ => {}
        }
    }
}

fn show_drag_value(ui: &Ui, ctx: &ViewCtx<'_>, lane: &LaneFrame<'_>, id: Id, point: Point) {
    let value = lane.format(ctx, point.value);
    let text =
        ctx.lang
            .automation_tooltip(lane.label, &value, fmt_pos(point.tick, ctx.project.meter));
    komponist_ui::tooltip::show_value(
        ui.ctx(),
        id.with("drag-value"),
        Rect::from_center_size(lane.pos(point), vec2(m::POINT, m::POINT)),
        komponist_ui::tooltip::Placement::Above,
        &text,
    );
}

fn in_same_group(a: &Source, b: &Source) -> bool {
    match (a, b) {
        (Source::Lane(a, _), Source::Lane(b, _)) => a == b,
        (Source::Clip(a, _), Source::Clip(b, _)) => a == b,
        _ => false,
    }
}

fn source_points<'a>(project: &'a Project, source: &Source) -> Option<&'a Points> {
    match source {
        Source::Lane(owner, id) => lanes(project, *owner)?
            .lanes
            .iter()
            .find(|lane| lane.id == *id)
            .map(|lane| &lane.points),
        Source::Clip(id, target) => project
            .find_clip(*id)?
            .1
            .automation
            .iter()
            .find(|envelope| &envelope.target == target)
            .map(|envelope| &envelope.points),
    }
}

/// Sends the new points of a lane and remembers them as the ones the selection refers to,
/// so the edit itself does not invalidate the selection (see [`Editor::prune_stale`]).
fn send_points(
    basis: &mut HashMap<Source, Points>,
    ctx: &mut ViewCtx<'_>,
    lane: &LaneFrame<'_>,
    points: Vec<Point>,
    label: &'static str,
) -> bool {
    let points = Points::new(points);
    if points.as_slice() == lane.points() {
        return false;
    }
    basis.insert(lane.source.clone(), points.clone());
    ctx.send(Action::Automation(Automation::Points(
        lane.source.clone(),
        points,
        label,
    )));
    true
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

// The document retains points outside the clip's current content. Keep full-array indices for
// edits and limit only the displayed, editable range.
fn editable_range(points: &[Point], end: Option<Tick>) -> Range<usize> {
    end.map_or(0..points.len(), |end| {
        points.partition_point(|p| p.tick < Tick::ZERO)..points.partition_point(|p| p.tick <= end)
    })
}

fn visible_range(lane: &LaneFrame<'_>, rect: Rect) -> Range<usize> {
    if rect.width() < 0.0 {
        return 0..0;
    }
    let (start, end) = lane.axis.ticks_in(rect.x_range());
    let range = editable_range(lane.points(), lane.end);
    let lo = lane
        .points()
        .partition_point(|p| p.tick < start)
        .max(range.start);
    let hi = lane
        .points()
        .partition_point(|p| p.tick <= end)
        .min(range.end);
    lo.min(hi)..hi
}

fn nearest_point(lane: &LaneFrame<'_>, pos: Pos2) -> Option<usize> {
    let window = Rect::from_center_size(pos, vec2(m::POINT_HIT, m::POINT_HIT));
    visible_range(lane, window)
        .map(|i| (i, lane.pos(lane.points()[i]).distance_sq(pos)))
        .filter(|(_, d)| *d <= (m::POINT_HIT / 2.0).powi(2))
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(i, _)| i)
}

fn handle_pos(lane: &LaneFrame<'_>, i: usize) -> Option<Pos2> {
    let range = editable_range(lane.points(), lane.end);
    if i < range.start || i + 1 >= range.end {
        return None;
    }
    let a = lane.points()[i];
    let b = lane.points()[i + 1];
    if lane.axis.width(b.tick - a.tick) < m::POINT_HIT * 2.0 {
        return None;
    }
    let tick = Tick(a.tick.0 + (b.tick.0 - a.tick.0) / 2);
    let value = automation::value_at(&lane.points()[range], tick)?;
    Some(lane.pos(Point::linear(tick, value)))
}

fn closest_handle(lane: &LaneFrame<'_>, pos: Pos2) -> Option<usize> {
    let tick = Tick(lane.axis.tick_at(pos.x).floor() as i64);
    let i = lane
        .points()
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

/// Keep each pixel column's endpoints and extremes in traversal order.
struct CurvePath {
    points: Vec<Pos2>,
    column: Option<CurveColumn>,
}

struct CurveColumn {
    first: Pos2,
    last: Pos2,
    min: Pos2,
    max: Pos2,
    min_first: bool,
}

impl CurvePath {
    fn new(capacity: usize) -> Self {
        Self {
            points: Vec::with_capacity(capacity),
            column: None,
        }
    }

    fn push(&mut self, point: Pos2) {
        if let Some(column) = self
            .column
            .as_mut()
            .filter(|column| column.min.x.floor() == point.x.floor())
        {
            column.last = point;
            if point.y.total_cmp(&column.min.y).is_lt() {
                column.min = point;
                column.min_first = false;
            } else if point.y.total_cmp(&column.max.y).is_ge() {
                column.max = point;
                column.min_first = true;
            }
        } else {
            self.flush_column();
            self.column = Some(CurveColumn {
                first: point,
                last: point,
                min: point,
                max: point,
                min_first: true,
            });
        }
    }

    fn flush_column(&mut self) {
        if let Some(column) = self.column.take() {
            let extrema = if column.min_first {
                [column.min, column.max]
            } else {
                [column.max, column.min]
            };
            for point in [column.first, extrema[0], extrema[1], column.last] {
                if self.points.last() != Some(&point) {
                    self.points.push(point);
                }
            }
        }
    }

    fn into_points(mut self) -> Vec<Pos2> {
        self.flush_column();
        self.points
    }
}

pub(super) fn paint_curve(
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
    let compact = curve_path(lane, points, rect);
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
            Stroke::new(lane.curve_width(), ink),
            space::SPACE_1_5,
            space::SPACE_1,
        ));
    } else {
        painter.add(egui::Shape::line(
            compact,
            Stroke::new(lane.curve_width(), ink),
        ));
    }
}

fn curve_path(lane: &LaneFrame<'_>, points: &[Point], rect: Rect) -> Vec<Pos2> {
    let (start, end) = lane.axis.ticks_in(rect.x_range());
    let lo = points.partition_point(|p| p.tick < start).saturating_sub(1);
    let hi = (points.partition_point(|p| p.tick <= end) + 1).min(points.len());
    let mut path = CurvePath::new((hi - lo).min(rect.width() as usize * 4 + 4));
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

    path.into_points()
}

fn dense_background(settings: &MeshSettings) -> egui::Mesh {
    let rect = settings.lane;
    let ppp = settings.pixels_per_point;
    let width = px::line_width(ppp);
    let mut mesh = egui::Mesh::default();
    mesh.add_colored_rect(px::snap_rect(rect, ppp), color32(color::SURFACE_1));

    let spec = GridSpec::new(settings.grid, space::SPACE_2);
    for (x, weight) in grid_lines(&settings.axis, settings.meter, rect.x_range(), &spec) {
        let x = px::snap(x, ppp);
        mesh.add_colored_rect(
            Rect::from_min_max(
                pos2(x, px::snap(rect.top(), ppp)),
                pos2(x + width, px::snap(rect.bottom(), ppp)),
            ),
            weight.color(),
        );
    }

    let y = px::snap(rect.center().y, ppp);
    let mut x = rect.left();
    while x < rect.right() {
        mesh.add_colored_rect(
            Rect::from_min_max(
                pos2(x, y),
                pos2((x + space::SPACE_1).min(rect.right()), y + width),
            ),
            color32(color::LINE_0),
        );
        x += 2.0 * space::SPACE_1;
    }
    mesh
}

/// A dense trace is a single band of each column's minimum and maximum values. Connecting the
/// bands keeps continuous curves compact.
fn dense_band(path: &[Pos2], settings: &MeshSettings) -> egui::Mesh {
    let mut mesh = egui::Mesh::default();
    let mut band = egui::Mesh::default();
    let half_width = settings.width / 2.0;
    for (index, column) in path.chunk_by(|a, b| a.x.floor() == b.x.floor()).enumerate() {
        let first = column[0];
        let (top, bottom) = column
            .iter()
            .fold((first.y, first.y), |(top, bottom), point| {
                (top.min(point.y), bottom.max(point.y))
            });
        let x = first.x;
        mesh.colored_vertex(pos2(x, (top + bottom) / 2.0), settings.fill);
        mesh.colored_vertex(pos2(x, settings.trace.bottom()), settings.fill);
        band.colored_vertex(pos2(x, top - half_width), settings.ink);
        band.colored_vertex(pos2(x, bottom + half_width), settings.ink);
        if index != 0 {
            let fill = (index as u32 - 1) * 2;
            mesh.add_triangle(fill, fill + 2, fill + 3);
            mesh.add_triangle(fill, fill + 3, fill + 1);
            band.add_triangle(fill, fill + 2, fill + 3);
            band.add_triangle(fill, fill + 3, fill + 1);
        }
    }
    mesh.append(band);
    mesh
}

/// Keep each column's extremes as a vertical mark. Join the outgoing value of the previous
/// column to the incoming value of the next, so a hold stays flat up to its jump.
fn dense_jump_mesh(path: &[Pos2], settings: &MeshSettings) -> egui::Mesh {
    let mut fill = egui::Mesh::default();
    let mut ink = egui::Mesh::default();
    let half_width = settings.width / 2.0;
    let mut previous: Option<Pos2> = None;
    let strip = |mesh: &mut egui::Mesh, a: Pos2, b: Pos2, c: Pos2, d: Pos2, color| {
        let index = mesh.vertices.len() as u32;
        for vertex in [a, b, c, d] {
            mesh.colored_vertex(vertex, color);
        }
        mesh.add_triangle(index, index + 1, index + 2);
        mesh.add_triangle(index, index + 2, index + 3);
    };
    for column in path.chunk_by(|a, b| a.x.floor() == b.x.floor()) {
        let first = column[0];
        let last = *column.last().unwrap();
        let (top, bottom) = column
            .iter()
            .fold((first.y, first.y), |(top, bottom), point| {
                (top.min(point.y), bottom.max(point.y))
            });
        if let Some(before) = previous {
            let bottom = settings.trace.bottom();
            strip(
                &mut fill,
                before,
                first,
                pos2(first.x, bottom),
                pos2(before.x, bottom),
                settings.fill,
            );
            let dy = vec2(0.0, half_width);
            strip(
                &mut ink,
                before - dy,
                first - dy,
                first + dy,
                before + dy,
                settings.ink,
            );
        }
        if top < bottom {
            ink.add_colored_rect(
                Rect::from_min_max(
                    pos2(first.x - half_width, top - half_width),
                    pos2(first.x + half_width, bottom + half_width),
                ),
                settings.ink,
            );
        }
        previous = Some(last);
    }
    fill.append(ink);
    fill
}
#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use egui::{Event, PointerButton};
    use egui_kittest::{Harness, kittest::Queryable};
    use komponist_model::automation::{AutomationOwner, AutomationTarget};
    use komponist_model::edits::automation::{add_lane, set_lane_points};

    use super::*;
    use crate::{App, meters::Meters, ui_state::UiState};

    #[test]
    fn dense_hold_jumps_keep_the_approach_and_exit_flat() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(42.0, 30.0));
        let settings = MeshSettings {
            axis: TimeAxis {
                x0: 0.0,
                px_per_tick: 1.0,
            },
            lane: rect,
            visible: rect,
            trace: rect,
            width: 2.0,
            ink: Color32::GREEN,
            fill: Color32::BLACK,
            pixels_per_point: 1.0,
            meter: Meter::FOUR_FOUR,
            grid: Grid::Bar,
            end: None,
        };
        let mut path = CurvePath::new(5);
        for point in [
            pos2(0.0, 24.0),
            pos2(21.0, 24.0),
            pos2(21.0, 7.0),
            pos2(21.5, 10.0),
            pos2(42.0, 10.0),
        ] {
            path.push(point);
        }
        let mesh = dense_jump_mesh(&path.into_points(), &settings);
        let (triangles, _) = mesh.indices.as_chunks::<3>();
        let fills = triangles.iter().filter(|triangle| {
            triangle
                .iter()
                .all(|&i| mesh.vertices[i as usize].color == settings.fill)
        });
        for triangle in fills {
            let edge: Vec<_> = triangle
                .iter()
                .map(|&i| mesh.vertices[i as usize].pos)
                .filter(|p| p.y < rect.bottom())
                .collect();
            if edge.len() == 2 {
                assert_eq!(edge[0].y, edge[1].y);
            }
        }
        assert!(
            mesh.vertices
                .iter()
                .any(|v| v.color == settings.ink && v.pos == pos2(20.0, 6.0))
        );
        assert!(
            mesh.vertices
                .iter()
                .any(|v| v.color == settings.ink && v.pos == pos2(22.0, 25.0))
        );
    }

    struct Fixture {
        app: App,
        editor: Editor,
        ui_state: UiState,
        meters: Meters,
        sources: Vec<Source>,
        hide_first_lane: bool,
    }

    impl Fixture {
        fn new(lanes: &[Vec<Point>]) -> Self {
            let mut project = Project::new("Automation curve");
            let owner = AutomationOwner::Master;
            let mut sources = Vec::new();
            for (points, target) in lanes
                .iter()
                .zip([AutomationTarget::Volume, AutomationTarget::Pan])
            {
                let (next, id) = add_lane(&project, owner, target, "Test".to_owned()).unwrap();
                project = set_lane_points(&next, owner, id, Points::new(points.clone())).unwrap();
                sources.push(Source::Lane(owner, id));
            }
            Self {
                app: App::headless(project),
                editor: Editor::default(),
                ui_state: UiState::new(),
                meters: Meters::new(),
                sources,
                hide_first_lane: false,
            }
        }

        fn points(&self, lane: usize) -> &[Point] {
            source_points(self.app.project(), &self.sources[lane])
                .unwrap()
                .as_slice()
        }

        fn show(&mut self, ui: &mut Ui) {
            let project = Arc::clone(self.app.project());
            let mut actions = Vec::new();
            let mut ctx = ViewCtx::new(
                &project,
                &mut self.ui_state,
                self.app.engine(),
                &mut self.meters,
                self.app.keymap(),
                &mut actions,
            );
            for (index, source) in self.sources.iter().enumerate() {
                if index == 0 && self.hide_first_lane {
                    continue;
                }
                self.editor.show(
                    ui,
                    &mut ctx,
                    &LaneFrame {
                        source: source.clone(),
                        label: "Test",
                        access: "Test",
                        warning: None,
                        points: source_points(&project, source).unwrap(),
                        target: None,
                        rect: Rect::from_min_size(
                            pos2(10.0, 10.0 + index as f32 * 50.0),
                            vec2(200.0, 50.0),
                        ),
                        axis: TimeAxis {
                            x0: 10.0,
                            px_per_tick: 1.0,
                        },
                        grid: Grid::Bar,
                        snap: false,
                        end: None,
                        head: Tick::ZERO,
                    },
                    &Paint {
                        curve: Color32::GREEN,
                        fill: Color32::BLACK,
                        effective: &[],
                        overrides: &[],
                        beyond: None,
                    },
                );
            }
            self.editor.finish_frame(ui, &mut ctx);
            for action in actions {
                self.app.dispatch(action);
            }
        }
    }

    fn harness(lanes: &[Vec<Point>]) -> Harness<'static, Fixture> {
        Harness::builder()
            .with_context_setup(komponist_ui::install)
            .with_size(vec2(220.0, 130.0))
            .build_ui_state(|ui, fixture| fixture.show(ui), Fixture::new(lanes))
    }

    #[test]
    fn dense_points_have_accesskit_actions_without_hovering() {
        let points = (1..=100)
            .map(|tick| Point::linear(Tick(tick), 0.25))
            .collect();
        let mut h = harness(&[points]);
        assert_eq!(h.get_all_by_role(Role::Slider).count(), 100);
        h.get_all_by_role(Role::Slider)
            .next()
            .unwrap()
            .increment_accesskit();
        h.run_steps(2);
        assert_eq!(h.state().points(0)[0].value, 0.26);
    }

    #[test]
    fn paste_before_selected_point_clears_stale_selection() {
        let mut h = harness(&[vec![
            Point::linear(Tick(25), 0.25),
            Point::linear(Tick(80), 0.75),
        ]]);
        h.get_all_by_role(Role::Slider)
            .last()
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        h.key_press_modifiers(Modifiers::COMMAND, Key::C);
        h.run_steps(2);
        h.key_press_modifiers(Modifiers::COMMAND, Key::V);
        h.run_steps(2);
        assert_eq!(
            h.state().points(0),
            [
                Point::linear(Tick::ZERO, 0.75),
                Point::linear(Tick(25), 0.25),
                Point::linear(Tick(80), 0.75),
            ]
        );
        h.key_press(Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().points(0).len(), 3);
    }

    #[test]
    fn undo_drops_the_selection_instead_of_retargeting_its_indices() {
        let original = vec![
            Point::linear(Tick(20), 0.2),
            Point::linear(Tick(60), 0.4),
            Point::linear(Tick(100), 0.6),
            Point::linear(Tick(140), 0.8),
        ];
        let mut h = harness(std::slice::from_ref(&original));
        let mut added = original.clone();
        added.insert(0, Point::linear(Tick(5), 0.9));
        let source = h.state().sources[0].clone();
        h.state_mut()
            .app
            .dispatch(Action::Automation(Automation::Points(
                source,
                Points::new(added),
                "undo-automation-add-point",
            )));
        h.run_steps(2);
        // Index 3 of 5 is the old point 2 (tick 100).
        h.get_all_by_role(Role::Slider)
            .nth(3)
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        assert_eq!(h.state().editor.selected.values().next().unwrap().len(), 1);

        h.state_mut().app.dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(2);
        assert_eq!(h.state().points(0), original);
        assert!(h.state().editor.selected.is_empty());
        h.key_press(Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().points(0), original);
        // Redo is an outside edit as well.
        h.get_all_by_role(Role::Slider)
            .nth(1)
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        h.state_mut().app.dispatch(Action::Edit(EditAction::Redo));
        h.run_steps(2);
        h.key_press(Key::ArrowUp);
        h.run_steps(2);
        assert_eq!(h.state().points(0).len(), 5);
        assert!(h.state().points(0).iter().all(|p| p.value != 0.41));
    }

    #[test]
    fn own_edits_keep_the_selection() {
        let mut h = harness(&[vec![
            Point::linear(Tick(25), 0.25),
            Point::linear(Tick(80), 0.5),
        ]]);
        h.get_all_by_role(Role::Slider)
            .last()
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        for _ in 0..3 {
            h.key_press(Key::ArrowUp);
            h.run_steps(2);
        }
        assert_eq!(h.state().points(0)[1].value, 0.53);
        assert_eq!(h.state().points(0)[0].value, 0.25);
    }

    #[test]
    fn shift_marquee_does_not_restore_a_selection_an_outside_edit_made_stale() {
        let original = vec![
            Point::linear(Tick(20), 0.2),
            Point::linear(Tick(60), 0.4),
            Point::linear(Tick(100), 0.6),
        ];
        let mut h = harness(std::slice::from_ref(&original));
        h.get_all_by_role(Role::Slider)
            .nth(1)
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        // A Shift-marquee over empty space, right of the last point, keeps the button down.
        let (from, to) = (pos2(180.0, 15.0), pos2(200.0, 25.0));
        h.event(Event::ModifiersChanged(Modifiers::SHIFT));
        h.event(Event::PointerMoved(from));
        h.step();
        h.event(Event::PointerButton {
            pos: from,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::SHIFT,
        });
        h.step();
        h.event(Event::PointerMoved(to));
        h.step();
        assert!(matches!(h.state().editor.drag, Some(Drag::Box { .. })));
        // An outside edit inserts a point before the selected one while the marquee is open.
        let mut added = original.clone();
        added.insert(0, Point::linear(Tick(5), 0.9));
        let source = h.state().sources[0].clone();
        h.state_mut()
            .app
            .dispatch(Action::Automation(Automation::Points(
                source,
                Points::new(added.clone()),
                "undo-automation-add-point",
            )));
        h.run_steps(2);
        h.event(Event::PointerButton {
            pos: to,
            button: PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::SHIFT,
        });
        h.event(Event::ModifiersChanged(Modifiers::NONE));
        h.run_steps(2);
        // Index 1 now names tick 20; the old selection must not come back and delete it.
        assert!(h.state().editor.selected.values().all(BTreeSet::is_empty));
        h.key_press(Key::Delete);
        h.run_steps(2);
        assert_eq!(h.state().points(0), added);
    }

    #[test]
    fn invalid_value_keeps_focus_and_hiding_lane_closes_entry() {
        let mut h = harness(&[
            vec![Point::linear(Tick(25), 0.25)],
            vec![Point::linear(Tick(50), 0.5)],
        ]);
        h.get_all_by_role(Role::Slider)
            .next()
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        h.key_press(Key::Enter);
        h.run_steps(2);
        h.key_press_modifiers(Modifiers::COMMAND, Key::A);
        h.event(Event::Text("invalid".to_owned()));
        h.run_steps(2);
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().editor.entry.as_ref().unwrap().text, "invalid");
        assert_eq!(h.state().points(0)[0].value, 0.25);
        h.key_press_modifiers(Modifiers::COMMAND, Key::A);
        h.event(Event::Text("60 %".to_owned()));
        h.run_steps(2);
        h.key_press(Key::Enter);
        h.run_steps(2);
        assert_eq!(h.state().points(0)[0].value, 0.6);
        assert!(h.state().editor.entry.is_none());

        h.key_press(Key::Enter);
        h.run_steps(2);
        assert!(h.state().editor.entry.is_some());
        h.state_mut().hide_first_lane = true;
        h.run_steps(2);
        assert!(h.state().editor.entry.is_none());
        h.get_by_role(Role::Slider).click_accesskit();
        h.run_steps(2);
        h.key_press(Key::ArrowUp);
        h.run_steps(2);
        assert_eq!(h.state().points(1)[0].value, 0.51);
    }

    #[test]
    fn shift_marquee_can_shrink_and_cross_lane_delete_is_one_undo() {
        let mut h = harness(&[
            vec![Point::linear(Tick(25), 0.25), Point::linear(Tick(80), 0.4)],
            vec![Point::linear(Tick(50), 0.45)],
        ]);
        h.get_all_by_role(Role::Slider)
            .next()
            .unwrap()
            .click_accesskit();
        h.run_steps(2);
        let origin = pos2(45.0, 20.0);
        h.hover_at(origin);
        h.event(Event::ModifiersChanged(Modifiers::SHIFT));
        h.event(Event::PointerButton {
            pos: origin,
            button: PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::SHIFT,
        });
        h.run_steps(2);
        h.hover_at(pos2(110.0, 107.0));
        h.run_steps(2);
        let selected = |fixture: &Fixture, lane| {
            fixture
                .editor
                .selected
                .get(&fixture.sources[lane])
                .cloned()
                .unwrap_or_default()
        };
        assert_eq!(selected(h.state(), 0), BTreeSet::from([0, 1]));
        assert_eq!(selected(h.state(), 1), BTreeSet::from([0]));
        h.hover_at(pos2(70.0, 58.0));
        h.run_steps(2);
        assert_eq!(selected(h.state(), 0), BTreeSet::from([0]));
        assert!(selected(h.state(), 1).is_empty());
        let end = pos2(110.0, 107.0);
        h.hover_at(end);
        h.run_steps(2);
        h.drop_at(end);
        h.event(Event::ModifiersChanged(Modifiers::NONE));
        h.run_steps(2);
        let previous = Arc::clone(h.state().app.project());
        h.key_press(Key::Delete);
        h.run_steps(2);
        assert!(h.state().points(0).is_empty());
        assert!(h.state().points(1).is_empty());
        h.state_mut().app.dispatch(Action::Edit(EditAction::Undo));
        assert_eq!(h.state().app.project(), &previous);
    }

    #[test]
    fn column_compaction_preserves_endpoints_and_extrema_in_traversal_order() {
        let mut path = CurvePath::new(5);
        let samples = [
            pos2(0.1, 4.0),
            pos2(0.2, 8.0),
            pos2(0.3, 2.0),
            pos2(0.4, 4.0),
            pos2(1.1, 3.0),
            pos2(1.2, 3.0),
            pos2(1.3, 3.0),
            pos2(2.1, 5.0),
        ];
        for point in samples {
            path.push(point);
        }
        assert_eq!(
            path.into_points(),
            [
                samples[0], samples[1], samples[2], samples[3], samples[4], samples[6], samples[7]
            ]
        );
    }
}
