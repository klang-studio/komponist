//! The lane below the rows (`Screens.dc.html` `stepSeq()` `lane` and the dock markup; `ks-view.js`
//! `stepView` `lane`; `Prototyp.dc.html` `stepLane`): on the left the tabs Vel / Wahr. / Ratch.
//! and "Zeile …", on the right one bar per step of that row, as high as its velocity (of 127),
//! probability (of 100 %) or ratchet (of 4). The bar of the cursor's step is `accent`, the others
//! have the note colour of their velocity; under each step a 2 px `surface.2` base line.
//!
//! A press or drag in the bars sets the value of the set step under the pointer from the height
//! (one undo step per drag) and moves the step cursor there. AccessKit: each bar of a set step is
//! a slider ("Vel Step 5") that takes increment, decrement and set-value.

use egui::accesskit::{Action as AkAction, ActionData};
use egui::epaint::CornerRadius;
use egui::{Id, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::edits::StepProp;
use komponist_model::{ClipId, Step};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, line, radius, typography};
use komponist_ui::widgets::dropdown::Item;
use komponist_ui::widgets::tabs::Segmented;
use komponist_ui::{px, widgets};

use super::Open;
use super::grid::Columns;
use super::metrics as m;
use super::names;
use super::pattern::{self, changes};
use crate::actions::{Action, EditAction, SelectionChange, UiAction};
use crate::ui_state::StepLane;
use crate::views::ViewCtx;

/// The lane tabs in order, with their labels (verbatim from the screen).
pub const LANES: [(StepLane, &str); 3] = [
    (StepLane::Velocity, "Vel"),
    (StepLane::Probability, "Wahr."),
    (StepLane::Ratchet, "Ratch."),
];

/// What the lane keeps between frames: the drag in progress (its clip and row).
#[derive(Debug, Default)]
pub struct LaneState {
    drag: Option<(ClipId, usize)>,
}

/// The value of `step` in `lane` as a number (velocity, percent, hits).
fn value(lane: StepLane, step: &Step) -> u8 {
    match lane {
        StepLane::Velocity => step.velocity,
        StepLane::Probability => step.probability,
        StepLane::Ratchet => step.ratchet,
    }
}

/// `value` as `lane`'s change, clamped to its range.
fn prop(lane: StepLane, value: i32) -> StepProp {
    match lane {
        StepLane::Velocity => StepProp::Velocity(value.clamp(1, 127) as u8),
        StepLane::Probability => StepProp::Probability(value.clamp(0, 100) as u8),
        StepLane::Ratchet => StepProp::Ratchet(value.clamp(1, 4) as u8),
    }
}

/// Draws the lane into `rect` and handles its input.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut LaneState,
    palette: &TrackPalette,
) {
    if rect.height() <= 0.0 {
        return;
    }
    let mut ui = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt("komponist_step_lane"),
    );
    ui.set_clip_rect(ui.clip_rect().intersect(rect));
    let ui = &mut ui;
    let lane = ctx.ui.step_seq.lane;
    let row = open.lane_row;
    label_column(ui, rect, open, ctx, lane, row);
    let Some(model) = open.pattern.rows.get(row) else {
        return;
    };

    let cols = Columns::new(rect, open.steps());
    let top = rect.top() + m::LANE_PAD_TOP;
    let base_bottom = rect.bottom() - m::LANE_PAD_BOTTOM;
    let base_top = base_bottom - m::LANE_BASE_LINE;
    let max_h = (base_top - top).max(0.0);
    let area = Rect::from_min_max(
        pos2(cols.left - m::CELL_GAP / 2.0, top),
        pos2(cols.x(cols.count - 1).max + m::CELL_GAP / 2.0, base_bottom),
    );
    let response = ui.interact(
        area,
        Id::unique(("komponist_step_lane_bars", open.clip)),
        Sense::click_and_drag(),
    );

    let selected = open.selected_step(row);
    let tab = LANES
        .iter()
        .find(|(l, _)| *l == lane)
        .map_or("Vel", |(_, t)| t);
    for i in 0..open.steps() {
        let step = &model.steps[i];
        let x = cols.x(i);
        let painter = ui.painter();
        px::fill(
            painter,
            Rect::from_x_y_ranges(x, base_top..=base_bottom),
            0.0,
            color32(color::SURFACE_2),
        );
        let column = Rect::from_x_y_ranges(x, top..=base_bottom);
        let node = ui.interact(
            column,
            Id::unique(("komponist_step_lane_bar", open.clip, row, i)),
            Sense::hover(),
        );
        if !step.on {
            continue;
        }
        let h = (pattern::lane_fraction(lane, step) * max_h).round();
        if h > 0.0 {
            let fill = if selected == Some(i) {
                color32(color::ACCENT)
            } else {
                color32(palette.note(step.velocity))
            };
            let rad = radius::S.round() as u8;
            let bar = px::snap_rect(
                Rect::from_x_y_ranges(x, base_top - h..=base_top),
                ui.pixels_per_point(),
            );
            let corners = CornerRadius {
                nw: rad,
                ne: rad,
                sw: 0,
                se: 0,
            };
            ui.painter().rect_filled(bar, corners, fill);
        }
        // AccessKit.
        let v = value(lane, step);
        let name = format!("{tab} Step {}", i + 1);
        node.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::Slider, true, &name);
            info.value = Some(f64::from(v));
            info
        });
        widgets::accesskit(&node, |n| {
            n.add_action(AkAction::Increment);
            n.add_action(AkAction::Decrement);
            n.add_action(AkAction::SetValue);
        });
        let (delta, set) = ui.input(|inp| {
            let d = inp.num_accesskit_action_requests(node.id, AkAction::Increment) as i32
                - inp.num_accesskit_action_requests(node.id, AkAction::Decrement) as i32;
            let mut set = None;
            for request in inp.accesskit_action_requests(node.id, AkAction::SetValue) {
                if let Some(ActionData::NumericValue(v)) = request.data {
                    set = Some(v.round() as i32);
                }
            }
            (d, set)
        });
        let target = set.or((delta != 0).then(|| i32::from(v) + delta));
        if let Some(t) = target {
            let p = prop(lane, t);
            if changes(step, p) {
                ctx.send(set_step(open.clip, row, i, p));
            }
        }
    }

    // Press and drag: the set step under the pointer takes the height.
    let pressed = ui.input(|i| i.pointer.primary_pressed()) && response.is_pointer_button_down_on();
    let instant = response.clicked() && state.drag.is_none();
    if pressed || instant {
        ctx.send(Action::Edit(EditAction::BeginGesture {
            label: "Lane".to_owned(),
        }));
        state.drag = Some((open.clip, row));
    }
    if let Some((clip, drag_row)) = state.drag
        && let Some(pos) = ui.input(|i| i.pointer.latest_pos())
        && clip == open.clip
        && drag_row == row
        && (pressed || instant || response.dragged())
        && let Some(i) = cols.at(pos.x)
        && max_h > 0.0
    {
        let step = &model.steps[i];
        if step.on {
            let p = pattern::lane_prop(lane, (base_top - pos.y) / max_h);
            if changes(step, p) {
                ctx.send(set_step(open.clip, row, i, p));
            }
            if selected != Some(i) {
                ctx.send(Action::Ui(UiAction::Select(SelectionChange::Step {
                    clip: open.clip,
                    row,
                    step: Some(i),
                })));
            }
        }
    }
    let up = ui.input(|i| i.pointer.primary_released() || !i.pointer.primary_down());
    if state.drag.is_some() && (instant || up) {
        ctx.send(Action::Edit(EditAction::EndGesture));
        state.drag = None;
    }
}

fn set_step(clip: ClipId, row: usize, step: usize, prop: StepProp) -> Action {
    Action::Edit(EditAction::SetStep {
        clip,
        row,
        step,
        prop,
    })
}

/// The left column: `surface.2` with its right edge, the lane tabs and "Zeile …" below them
/// (`padding:8px; gap:6px`). A lane lower than both centres the tabs and leaves the row name out.
fn label_column(
    ui: &mut Ui,
    rect: Rect,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    lane: StepLane,
    row: usize,
) {
    let ppp = ui.pixels_per_point();
    let column = Rect::from_min_size(rect.min, vec2(m::HEADER_WIDTH, rect.height()));
    px::fill(ui.painter(), column, 0.0, color32(color::SURFACE_2));
    px::vline(
        ui.painter(),
        column.right() - line::WIDTH,
        column.y_range(),
        color32(color::SURFACE_0),
    );
    let items: Vec<Item<'_>> = LANES.iter().map(|(_, l)| Item::new(l)).collect();
    let mut index = LANES.iter().position(|(l, _)| *l == lane).unwrap_or(0);
    let name = open
        .names
        .get(row)
        .map(|n| names::row(n))
        .unwrap_or_default();
    let label = widgets::styled_galley(ui, &name, &typography::SMALL);
    // The tabs' height: a segment of `size.control-compact` in a 2 px frame.
    let tabs_h = komponist_tokens::size::CONTROL_COMPACT + 2.0 * komponist_tokens::space::SPACE_0_5;
    let full = 2.0 * m::LANE_LABEL_PAD + tabs_h + m::LANE_LABEL_GAP + label.size().y;
    let tabs_top = if rect.height() >= full {
        rect.top() + m::LANE_LABEL_PAD
    } else {
        rect.center().y - tabs_h / 2.0
    };
    let tabs_rect = Rect::from_min_max(
        pos2(column.left() + m::LANE_LABEL_PAD, tabs_top),
        pos2(column.right() - m::LANE_LABEL_PAD, tabs_top + tabs_h),
    );
    let mut tabs_ui = ui.new_child(UiBuilder::new().max_rect(tabs_rect));
    if tabs_ui
        .add(Segmented::new(names::LANE, &mut index, &items))
        .changed()
        && let Some((l, _)) = LANES.get(index)
    {
        ctx.send(Action::Ui(UiAction::SetStepLane(*l)));
    }
    if rect.height() >= full && !name.is_empty() {
        let pos = pos2(
            px::snap(column.left() + m::LANE_LABEL_PAD, ppp),
            px::snap(tabs_rect.bottom() + m::LANE_LABEL_GAP, ppp),
        );
        let r = Rect::from_min_size(pos, label.size());
        let response = ui.allocate_rect(r, Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &name));
        ui.painter().galley(pos, label, color32(color::TEXT_3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lane_values_clamp_to_their_range() {
        assert_eq!(prop(StepLane::Velocity, 200), StepProp::Velocity(127));
        assert_eq!(prop(StepLane::Velocity, 0), StepProp::Velocity(1));
        assert_eq!(prop(StepLane::Probability, -3), StepProp::Probability(0));
        assert_eq!(prop(StepLane::Ratchet, 5), StepProp::Ratchet(4));
        let s = Step {
            on: true,
            velocity: 52,
            probability: 50,
            ratchet: 3,
        };
        assert_eq!(value(StepLane::Velocity, &s), 52);
        assert_eq!(value(StepLane::Probability, &s), 50);
        assert_eq!(value(StepLane::Ratchet, &s), 3);
    }
}
