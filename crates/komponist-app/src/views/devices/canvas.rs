use egui::{Align2, Painter, Pos2, Rect, Role, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo};
use komponist_plugin_api::device_canvas::{Alignment, FmFrame, Point, Primitive, REFRESH, Tone};
use komponist_plugin_api::native_layout::{Canvas, FmDiagram};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, radius, typography, widget};
use komponist_ui::widgets::device::fm_algorithm::{FmAlgorithm, Topology};
use komponist_ui::{fonts, px};

use super::cells::{native_selection, native_selection_id};
use super::layout::Cell;
use super::model::Card;
use super::viewport::Viewport;

// Keep at least one half-percent of the display's source range visible.
const MIN_CANVAS_WIDTH: f64 = 1.0 / 200.0;

pub(super) fn draw(
    ui: &mut Ui,
    card: &Card,
    cell: &Cell,
    palette: &TrackPalette,
    rect: Rect,
) -> bool {
    let Some(layout) = &card.native_layout else {
        return false;
    };
    let Some(canvas) = card.native_cell(cell).and_then(|cell| cell.canvas.as_ref()) else {
        return false;
    };
    let lang = komponist_ui::lang(ui.ctx()).code();
    let name = layout.label(&canvas.name, lang).unwrap_or_default();
    let description = layout.label(&canvas.description, lang);
    ui.ctx().request_repaint_after(REFRESH);
    if let Some(diagram) = &canvas.fm_topology
        && let Some(frame) = fm_frame(card, canvas, diagram)
    {
        let selection = native_selection_id(card, &diagram.selector);
        let selected = native_selection(ui.ctx(), selection, frame.modulates.len());
        let topology = Topology {
            modulates: &frame.modulates,
            outputs: Some(frame.outputs),
            feedback: frame.feedback.map(usize::from),
        };
        let mut widget =
            FmAlgorithm::new(frame.algorithm.map_or(1, usize::from), topology, palette);
        if frame.algorithm.is_none() {
            widget = widget.titled(name);
        }
        let response = ui
            .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                widget
                    .cells(cell.cols, cell.rows)
                    .selected(Some(selected))
                    .show(ui)
            })
            .inner;
        if let Some(description) = description {
            ui.ctx()
                .accesskit_node_builder(response.response.id, |node| {
                    node.set_description(description);
                });
        }
        if let Some(operator) = response.clicked {
            ui.ctx()
                .data_mut(|data| data.insert_temp(selection, operator));
            ui.ctx().request_repaint();
        }
        return true;
    }
    let id = ui.scope_id().with((&canvas.key, card.device));
    let sense = if canvas.horizontal_viewport {
        Sense::click()
    } else {
        Sense::hover()
    };
    let response = ui.interact(rect, id, sense);
    response.widget_info(|| WidgetInfo::labeled(Role::Group, true, name));
    if let Some(description) = description {
        ui.ctx()
            .accesskit_node_builder(response.id, |node| node.set_description(description));
    }
    px::fill(ui.painter(), rect, radius::M, color32(color::SURFACE_1));
    if canvas.fm_topology.is_none()
        && let Some(frame) = card
            .display
            .as_ref()
            .and_then(|display| display.frames.iter().find(|frame| frame.key == canvas.key))
    {
        let painter = ui.painter().with_clip_rect(rect);
        let content = if canvas.horizontal_viewport {
            let memory = id.with("viewport");
            let mut view = ui
                .data(|data| data.get_temp::<Viewport>(memory))
                .unwrap_or_else(|| Viewport::fit(1.0));
            if response.double_clicked() {
                view = Viewport::fit(1.0);
            } else if response.hovered()
                && let Some(next) = view.wheel(ui, rect, 1.0, MIN_CANVAS_WIDTH)
            {
                view = next;
                ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
            }
            let width = rect.width() / view.width() as f32;
            let left = rect.left() - width * view.start as f32;
            ui.data_mut(|data| data.insert_temp(memory, view));
            Rect::from_min_max(
                Pos2::new(left, rect.top()),
                Pos2::new(left + width, rect.bottom()),
            )
        } else {
            rect
        };
        for primitive in &frame.primitives {
            paint(&painter, content, palette, primitive);
        }
    }
    if canvas.show_title {
        let [x, y] = komponist_ui::widgets::metrics::device::graph::TITLE_POS;
        let title = color32(color::TEXT_2);
        let painter = ui.painter().with_clip_rect(rect);
        let galley = painter.layout_job(fonts::layout_job(name, &typography::GRAPH_TITLE, title));
        let ppp = ui.pixels_per_point();
        painter.galley(
            Pos2::new(
                px::snap(rect.left() + x, ppp),
                px::snap(rect.top() + y, ppp),
            ),
            galley,
            title,
        );
    }
    true
}

fn fm_frame<'a>(card: &'a Card, canvas: &Canvas, diagram: &FmDiagram) -> Option<&'a FmFrame> {
    let operators = card
        .native_page()?
        .cells
        .iter()
        .filter_map(|cell| cell.selector.as_ref())
        .find(|selector| selector.key == diagram.selector)?
        .options
        .len();
    card.display
        .as_ref()?
        .fm_topologies
        .iter()
        .find(|frame| frame.key == canvas.key && frame.modulates.len() == operators)
}

pub(super) fn fm_carrier(card: &Card, cell: &Cell) -> Option<bool> {
    card.native_page()?
        .cells
        .iter()
        .filter_map(|cell| cell.canvas.as_ref())
        .find_map(|canvas| {
            let diagram = canvas.fm_topology.as_ref()?;
            let binding = diagram
                .carriers
                .iter()
                .find(|binding| binding.col == cell.col && binding.row == cell.row)?;
            Some(
                fm_frame(card, canvas, diagram)
                    .is_some_and(|frame| frame.is_carrier(usize::from(binding.operator))),
            )
        })
}

fn paint(painter: &Painter, rect: Rect, palette: &TrackPalette, primitive: &Primitive) {
    let pos = |[x, y]: Point| {
        Pos2::new(
            rect.left() + rect.width() * x,
            rect.top() + rect.height() * y,
        )
    };
    let color = |tone| {
        color32(match tone {
            Tone::Text => color::TEXT_1,
            Tone::Muted => color::TEXT_3,
            Tone::Accent => palette.base,
            Tone::Grid => color::LINE_2,
        })
    };
    let stroke = |tone| Stroke::new(widget::graph::curve::STROKE, color(tone));
    match primitive {
        Primitive::Line { from, to, tone } => {
            painter.line_segment([pos(*from), pos(*to)], stroke(*tone));
        }
        Primitive::Polyline { points, tone } => {
            painter.add(Shape::line(
                points.iter().copied().map(pos).collect(),
                stroke(*tone),
            ));
        }
        Primitive::Rectangle {
            from,
            to,
            tone,
            filled,
        } => {
            let shape = Rect::from_two_pos(pos(*from), pos(*to));
            if *filled {
                painter.rect_filled(shape, 0, color(*tone));
            } else {
                painter.rect_stroke(shape, 0, stroke(*tone), StrokeKind::Inside);
            }
        }
        Primitive::Circle {
            center,
            radius,
            tone,
            filled,
        } => {
            let radius = radius * rect.width().min(rect.height());
            if *filled {
                painter.circle_filled(pos(*center), radius, color(*tone));
            } else {
                painter.circle_stroke(pos(*center), radius, stroke(*tone));
            }
        }
        Primitive::Area {
            points,
            baseline,
            tone,
        } => {
            let baseline = pos([0.0, *baseline]).y;
            let fill = color(*tone).gamma_multiply(widget::graph::curve::FILL_OPACITY);
            for pair in points.windows(2) {
                let (a, b) = (pos(pair[0]), pos(pair[1]));
                let (ay, by) = (a.y - baseline, b.y - baseline);
                let bottom_a = Pos2::new(a.x, baseline);
                let bottom_b = Pos2::new(b.x, baseline);
                if ay * by < 0.0 {
                    let cross = Pos2::new(a.x + (b.x - a.x) * ay / (ay - by), baseline);
                    painter.add(Shape::convex_polygon(
                        vec![a, cross, bottom_a],
                        fill,
                        Stroke::NONE,
                    ));
                    painter.add(Shape::convex_polygon(
                        vec![cross, b, bottom_b],
                        fill,
                        Stroke::NONE,
                    ));
                } else {
                    painter.add(Shape::convex_polygon(
                        vec![a, b, bottom_b, bottom_a],
                        fill,
                        Stroke::NONE,
                    ));
                }
            }
        }
        Primitive::Text {
            at,
            text,
            alignment,
            tone,
        } => {
            let anchor = match alignment {
                Alignment::Left => Align2::LEFT_CENTER,
                Alignment::Center => Align2::CENTER_CENTER,
                Alignment::Right => Align2::RIGHT_CENTER,
            };
            painter.text(
                pos(*at),
                anchor,
                text,
                fonts::font_id(&typography::LABEL),
                color(*tone),
            );
        }
    }
}
