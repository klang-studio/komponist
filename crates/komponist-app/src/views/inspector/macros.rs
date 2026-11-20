//! Track macro details in the inspector, using the same slots as the device-chain card.

use egui::{Align, Rangef, Rect, Response, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_model::automation::{AutomationOwner, AutomationTarget, bend as bend_curve};
use komponist_model::macros::{MACRO_COUNT, Mapping};
use komponist_model::{Track, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, TrackPalette, color, radius, size, space, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::number_field::{
    NumberField, accesskit_requests, expose_value, format_number, parse_number,
};
use komponist_ui::{fonts, px, widgets};

use super::{Meta, frame, place};
use crate::actions::{Action, EditAction, UiAction};
use crate::views::ViewCtx;
use crate::views::automation::{Catalog, Target};
use crate::views::mixer::gesture_actions;
use crate::views::track_macros::{self as macros, Action as Macro};

/// Two rows of four small knob cells (Track Macros proposal §8.1).
const CELL_HEIGHT: f32 = 50.0;
/// Mapping range track and current-value marker (Track Macros proposal §8.1).
const RANGE_TRACK: f32 = 4.0;
const RANGE_MARK: [f32; 2] = [2.0, 8.0];
const RANGE_HANDLE: [f32; 2] = [6.0, 12.0];
const CURVE_SIZE: [f32; 2] = [28.0, 18.0];
// Leave room for the signed value and the standard NumberField text-entry padding.
const CURVE_ENTRY_WIDTH: f32 = 56.0;
const NORMALIZED_STEP: f64 = 0.01;

pub(super) fn section(ui: &mut Ui, ctx: &mut ViewCtx<'_>, x: Rangef, y: &mut f32, track: &Track) {
    let lang = ctx.lang;
    let index = ctx.ui.track_macros.selected(track.id);
    let slot = &track.macros.0[usize::from(index)];
    let (_, show) = frame(
        ui,
        x,
        y,
        lang.macros_title(),
        Meta::Action(lang.macros_show_in_devices()),
        |ui, col| {
            let grid = col.take(2.0 * CELL_HEIGHT);
            for index in 0..MACRO_COUNT {
                let width = grid.width() / 4.0;
                let min =
                    grid.min + vec2((index % 4) as f32 * width, (index / 4) as f32 * CELL_HEIGHT);
                let rect = Rect::from_min_size(min, vec2(width, CELL_HEIGHT));
                macros::dial(ui, ctx, track, index as u8, rect, true);
            }
            let view = &ctx.ui.track_macros;
            if !view.selected.contains_key(&track.id) && view.mapping != Some(track.id) {
                return;
            }
            let row = col.take(size::CONTROL);
            name_field(ui, ctx, row, track.id, index, &slot.name);
            let row = col.take(size::CONTROL);
            let mut value = macros::value(ctx, track, index);
            let response = place(ui, row, ("slot-value", index), |ui| {
                ui.add(
                    NumberField::new(&mut value, lang.macros_value())
                        .key(lang.macros_value())
                        .width(row.width())
                        .range(0.0..=1.0)
                        .step(NORMALIZED_STEP)
                        .format(|value| macros::percent(lang, value))
                        .parse(macros::parse_percent),
                )
            });
            let mut actions = Vec::new();
            gesture_actions(
                &response,
                response.changed(),
                || {
                    Action::Macro(Macro::BeginValue {
                        track: track.id,
                        index,
                    })
                },
                &mut actions,
                |out| {
                    out.push(Action::Macro(Macro::SetValue {
                        track: track.id,
                        index,
                        value,
                    }))
                },
            );
            for action in actions {
                ctx.send(action);
            }
            let row = col.take(size::CONTROL);
            let mut controls = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(row)
                    .id_salt("mapping-list")
                    .layout(egui::Layout::right_to_left(Align::Center)),
            );
            macros::map_button(&mut controls, ctx, track.id);
            macros::text(
                ui,
                Rect::from_x_y_ranges(row.left()..=controls.cursor().right(), row.y_range()),
                &lang.macros_mappings(slot.mappings.len()),
                &typography::LABEL,
                color32(color::TEXT_2),
            );
            if slot.mappings.is_empty() {
                let row = col.take(2.0 * typography::SMALL.line_height);
                wrapping_text(ui, row, lang.macros_no_mappings());
                return;
            }
            let value = macros::value(ctx, track, index);
            for mapping in &slot.mappings {
                let resolved = Catalog::new(ctx, AutomationOwner::Track(track.id))
                    .map(|catalog| catalog.resolve(&mapping.target));
                let resolved = resolved.map(|result| {
                    result.map_err(|warning| {
                        warning.map(|warning| {
                            let reason = warning.trim_start_matches('⚠').trim_start();
                            let hint = if matches!(mapping.target, AutomationTarget::Param { device, .. }
                                if ctx.project.find_device(device).is_some()
                                    && matches!(ctx.engine.device_status(device),
                                        komponist_plugins::DeviceStatus::Missing
                                            | komponist_plugins::DeviceStatus::Crashed(_)))
                            {
                                lang.macros_plugin_missing_hint()
                            } else {
                                lang.macros_mapping_kept()
                            };
                            format!("{reason} · {hint}")
                        })
                    })
                });
                let details_height = RANGE_HANDLE[1] + 2.0 * typography::SMALL.line_height;
                let details_height = match &resolved {
                    Some(Err(Some(warning))) => details_height.max(
                        ui.painter()
                            .layout(
                                warning.clone(),
                                fonts::font_id(&typography::SMALL),
                                color32(color::TEXT_3),
                                col.width(),
                            )
                            .size()
                            .y,
                    ),
                    _ => details_height,
                };
                let row = col.take(size::CONTROL + details_height);
                mapping_row(ui, ctx, row, track, index, mapping, resolved, value);
            }
        },
    );
    if show.is_some_and(|r| r.clicked()) {
        ctx.send(Action::Ui(UiAction::SetDockTab(
            crate::ui_state::DockTab::Devices,
        )));
    }
}

fn name_field(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    row: Rect,
    track: TrackId,
    index: u8,
    name: &str,
) {
    let lang = ctx.lang;
    let label = Rect::from_min_size(row.min, vec2(row.width() / 4.0, row.height()));
    macros::text(
        ui,
        label,
        lang.macros_name(),
        &typography::SMALL,
        color32(color::TEXT_3),
    );
    let field = Rect::from_min_max(pos2(label.right(), row.top()), row.max);
    let id = ui.scope_id().with(("track-macro-name", track, index));
    let mut buffer = ui
        .ctx()
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| name.to_owned());
    let response = ui.put(
        field,
        egui::TextEdit::singleline(&mut buffer)
            .id(id)
            .font(fonts::font_id(&typography::UI))
            .text_color(color32(color::TEXT_1))
            .desired_width(field.width()),
    );
    response.widget_info(|| WidgetInfo::labeled(Role::TextInput, true, lang.macros_name()));
    let (enter, escape) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::Enter),
            i.key_pressed(egui::Key::Escape),
        )
    });
    let focused = response.has_focus();
    if !escape && (response.lost_focus() || (focused && enter)) {
        let next = buffer.trim().to_owned();
        if next != name {
            ctx.send(Action::Macro(Macro::Rename {
                track,
                index,
                name: next,
            }));
        }
    }
    ui.ctx().data_mut(|d| {
        if focused && !enter && !escape {
            d.insert_temp(id, buffer);
        } else {
            d.remove::<String>(id);
        }
    });
    if focused && (enter || escape) {
        response.surrender_focus();
    }
}

fn wrapping_text(ui: &mut Ui, row: Rect, s: &str) {
    place(ui, row, "description", |ui| {
        ui.add(
            egui::Label::new(
                egui::RichText::new(s)
                    .font(fonts::font_id(&typography::SMALL))
                    .color(color32(color::TEXT_3)),
            )
            .wrap(),
        );
    });
}

#[allow(clippy::too_many_arguments)]
fn mapping_row(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    row: Rect,
    track: &Track,
    index: u8,
    mapping: &Mapping,
    resolved: Option<Result<Target, Option<String>>>,
    value: f32,
) {
    let lang = ctx.lang;
    let id = ui.scope_id().with((track.id, index, &mapping.target));
    let ink = color32(TrackPalette::new(track.hue.0).base);
    let (target, warning) = match resolved {
        Some(Ok(target)) => (Some(target), None),
        Some(Err(Some(warning))) => (None, Some(warning)),
        _ => (None, None),
    };
    let (first, last) = if mapping.invert {
        (mapping.max, mapping.min)
    } else {
        (mapping.min, mapping.max)
    };
    let range = if let Some(target) = &target {
        lang.devices_range(target.format(first), target.format(last))
    } else {
        lang.devices_range(
            macros::percent(lang, f64::from(first)),
            macros::percent(lang, f64::from(last)),
        )
    };
    let (device, param) = match mapping.label.split_once(" · ") {
        Some(parts) => parts,
        None => ("", mapping.label.as_str()),
    };
    let access = lang.macros_a11y_mapping(param, device, &range);
    let group = ui.interact(row, id, Sense::hover());
    group.widget_info(|| WidgetInfo::labeled(Role::Group, true, &access));
    widgets::accesskit(&group, |node| {
        if let Some(warning) = &warning {
            node.set_description(warning.as_str());
        }
    });
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .id_salt(id)
            .accessibility_parent(group.id),
    );
    child.set_clip_rect(row.intersect(ui.clip_rect()));
    let ui = &mut child;
    let line = Rect::from_min_size(row.min, vec2(row.width(), size::CONTROL));
    let mut controls = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(line)
            .id_salt("controls")
            .layout(egui::Layout::right_to_left(Align::Center)),
    );
    controls.spacing_mut().item_spacing.x = space::SPACE_0_5;
    if controls
        .add(Button::icon(Icon::X, lang.macros_remove()).small())
        .clicked()
    {
        ctx.send(Action::Macro(Macro::Remove {
            track: track.id,
            index,
            target: mapping.target.clone(),
        }));
    }
    if controls
        .add(
            Button::icon(Icon::Repeat, lang.macros_invert())
                .small()
                .on(mapping.invert),
        )
        .clicked()
    {
        ctx.send(Action::Macro(Macro::Invert {
            track: track.id,
            index,
            target: mapping.target.clone(),
            invert: !mapping.invert,
        }));
    }
    let mut bend = mapping.bend;
    let response = controls.add(
        NumberField::new(&mut bend, lang.macros_a11y_bend(param))
            .width(CURVE_ENTRY_WIDTH)
            .range(-1.0..=1.0)
            .step(NORMALIZED_STEP)
            .default_value(0.0)
            .format(|value| {
                if value == 0.0 {
                    lang.macros_bend_linear().to_owned()
                } else if value > 0.0 {
                    format!("+{}", format_number(value, 2))
                } else {
                    format_number(value, 2)
                }
            })
            .parse(|text| {
                if text.eq_ignore_ascii_case(lang.macros_bend_linear()) {
                    Some(0.0)
                } else {
                    parse_number(text, "")
                }
            }),
    );
    let tip = crate::platform::Platform::current()
        .pick(lang.macros_bend_tip(), lang.macros_bend_tip_mac());
    let curve_label = if bend == 0.0 {
        lang.macros_bend_linear().to_owned()
    } else if bend > 0.0 {
        format!("+{}", format_number(f64::from(bend), 2))
    } else {
        format_number(f64::from(bend), 2)
    };
    response
        .clone()
        .on_hover_text(format!("{tip} · {curve_label}"));
    widgets::accesskit(&response, |node| {
        node.set_role(Role::Slider);
        node.set_description(tip);
    });
    if !widgets::number_field::value_entry_open(ui.ctx(), response.id) {
        let fill = widgets::number_field::look(
            komponist_ui::state::State::of(&controls, &response),
            false,
        )
        .fill;
        let plot =
            Rect::from_center_size(response.rect.center(), vec2(CURVE_SIZE[0], CURVE_SIZE[1]));
        px::fill(ui.painter(), response.rect.shrink(1.5), radius::S, fill);
        let line = (0..=16)
            .map(|step| {
                let t = f32::from(step as u8) / 16.0;
                pos2(
                    egui::lerp(plot.x_range(), t),
                    egui::lerp(plot.y_range(), 1.0 - bend_curve(t, bend)),
                )
            })
            .collect::<Vec<_>>();
        ui.painter()
            .add(egui::Shape::line(line, egui::Stroke::new(1.0, ink)));
    }
    let mut actions = Vec::new();
    gesture_actions(
        &response,
        response.changed(),
        || begin("undo-macro-bend"),
        &mut actions,
        |out| {
            out.push(Action::Macro(Macro::Bend {
                track: track.id,
                index,
                target: mapping.target.clone(),
                bend,
            }));
        },
    );
    for action in actions {
        ctx.send(action);
    }

    let caption = Rect::from_x_y_ranges(row.left()..=controls.cursor().right(), line.y_range());
    let caption_response = ui.interact(caption, id.with("reveal"), Sense::click());
    let title = target
        .as_ref()
        .map_or(mapping.label.as_str(), |target| target.label.as_str());
    caption_response.widget_info(|| WidgetInfo::labeled(Role::Button, true, title));
    macros::text(
        ui,
        caption,
        title,
        &typography::UI,
        color32(if warning.is_some() {
            color::STATE_ERROR
        } else {
            color::TEXT_1
        }),
    );
    if caption_response.clicked() {
        ctx.send(Action::Macro(Macro::Reveal {
            track: track.id,
            target: mapping.target.clone(),
        }));
    }
    if let Some(warning) = warning {
        let hint_rect = Rect::from_min_max(pos2(row.left(), line.bottom()), row.max);
        wrapping_text(ui, hint_rect, &warning);
        caption_response.on_hover_text(warning);
        return;
    }
    let bar = Rect::from_x_y_ranges(
        row.left() + RANGE_HANDLE[0] / 2.0..=row.right() - RANGE_HANDLE[0] / 2.0,
        line.bottom()..=line.bottom() + RANGE_HANDLE[1],
    );
    let effective = mapping.value_at(value);
    range_bar(
        ui,
        ctx,
        bar,
        track.id,
        index,
        mapping,
        target.as_ref(),
        effective,
        ink,
    );
    let footer = Rect::from_min_max(pos2(row.left(), bar.bottom()), row.max);
    let value = target.as_ref().map_or_else(
        || macros::percent(lang, f64::from(effective)),
        |t| t.format(effective),
    );
    macros::text(
        ui,
        footer,
        &range,
        &typography::SMALL,
        color32(color::TEXT_2),
    );
    let value_galley = widgets::styled_galley(ui, &value, &typography::VALUE_S);
    let value_rect = Rect::from_x_y_ranges(
        footer.right() - value_galley.size().x..=footer.right(),
        footer.y_range(),
    );
    px::fill(ui.painter(), value_rect, 0.0, color32(color::SURFACE_2));
    macros::text(ui, value_rect, &value, &typography::VALUE_S, ink);
}

#[derive(Clone, Copy)]
struct EndpointDrag {
    epoch: u64,
    collapsed_at: Option<f32>,
}

#[allow(clippy::too_many_arguments)]
fn range_bar(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    rect: Rect,
    track: TrackId,
    index: u8,
    mapping: &Mapping,
    target: Option<&Target>,
    value: f32,
    ink: egui::Color32,
) {
    let lang = ctx.lang;
    let bar = Rect::from_center_size(rect.center(), vec2(rect.width(), RANGE_TRACK));
    let pos = |value| egui::lerp(bar.x_range(), value);
    let cs = |value| pos2(pos(value), bar.center().y);
    let low = mapping.min;
    let high = mapping.max;
    let fill = Rect::from_x_y_ranges(pos(low)..=pos(high), bar.y_range());
    px::fill(ui.painter(), bar, radius::S, color32(color::SURFACE_0));
    px::fill(ui.painter(), fill, radius::S, ink.gamma_multiply(0.5));
    px::fill(
        ui.painter(),
        Rect::from_center_size(cs(value), vec2(RANGE_MARK[0], RANGE_MARK[1])),
        0.0,
        ink,
    );
    let id = ui.scope_id().with("range");
    let mover = ui.interact(
        fill.expand2(vec2(0.0, (rect.height() - bar.height()) / 2.0)),
        id,
        Sense::drag(),
    );
    let alt = ui.input(|i| i.modifiers.alt);
    let epoch = ctx.ui.track_macros.range_drag_epoch;
    if mover.drag_started() && alt {
        ui.data_mut(|d| d.insert_temp(id, epoch));
        ctx.send(begin("undo-macro-range"));
    }
    let moving = ui.data(|d| d.get_temp::<u64>(id)) == Some(epoch);
    if mover.dragged() && moving && bar.width() > 0.0 {
        let delta = (mover.drag_delta().x / bar.width()).clamp(-low, 1.0 - high);
        if delta != 0.0 {
            ctx.send(Action::Macro(Macro::Range {
                track,
                index,
                target: mapping.target.clone(),
                min: low + delta,
                max: high + delta,
            }));
        }
    }
    if mover.drag_stopped() {
        ui.data_mut(|d| d.remove::<u64>(id));
        if moving {
            ctx.send(Action::Edit(EditAction::EndGesture));
        }
    }
    for (is_min, current) in [(true, low), (false, high)] {
        let handle = Rect::from_center_size(cs(current), vec2(RANGE_HANDLE[0], RANGE_HANDLE[1]));
        let name = if is_min {
            lang.macros_a11y_min(&mapping.label)
        } else {
            lang.macros_a11y_max(&mapping.label)
        };
        let response = ui.interact(handle, id.with(is_min), Sense::click_and_drag());
        let formatter = |value| {
            target.map_or_else(
                || macros::percent(lang, f64::from(value)),
                |t| t.format(value),
            )
        };
        let text = formatter(current);
        response.clone().on_hover_text(format!("{name} · {text}"));
        let limits = if is_min { 0.0..=high } else { low..=1.0 };
        expose_value(
            &response,
            Role::Slider,
            &name,
            f64::from(current),
            text,
            f64::from(*limits.start())..=f64::from(*limits.end()),
            NORMALIZED_STEP,
        );
        px::fill(ui.painter(), handle, radius::S, color32(color::TEXT_2));
        let (press, edit) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowRight) as i8
                    - i.key_pressed(egui::Key::ArrowLeft) as i8,
                i.key_pressed(egui::Key::Enter),
            )
        });
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        ..Default::default()
                    },
                );
            });
        }
        let drag_id = response.id.with("endpoint-drag");
        if response.drag_started() {
            ui.data_mut(|d| {
                d.insert_temp(
                    drag_id,
                    EndpointDrag {
                        epoch,
                        collapsed_at: (low == high).then_some(low),
                    },
                );
            });
        }
        let drag = ui.data(|d| d.get_temp::<EndpointDrag>(drag_id));
        if response.drag_stopped() {
            ui.data_mut(|d| d.remove::<EndpointDrag>(drag_id));
        }
        if drag.is_some_and(|drag| drag.epoch != epoch) {
            continue;
        }
        let (steps, set) = accesskit_requests(ui, response.id);
        let steps = if response.has_focus() {
            steps + i32::from(press)
        } else {
            steps
        };
        let mut candidate = if response.dragged() && bar.width() > 0.0 {
            response
                .interact_pointer_pos()
                .map(|p| ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0))
        } else if let Some(value) = set.filter(|value| value.is_finite()) {
            Some(value.clamp(0.0, 1.0) as f32)
        } else {
            (steps != 0).then_some(current + steps as f32 * NORMALIZED_STEP as f32)
        };
        if response.double_clicked() || (response.has_focus() && edit) {
            ui.ctx()
                .data_mut(|d| d.insert_temp(id.with(("range-entry", is_min)), true));
        }
        if let Some(typed) = range_entry(
            ui, &response, is_min, id, &name, current, &formatter, target,
        ) {
            candidate = Some(typed);
        }
        let mut actions = Vec::new();
        gesture_actions(
            &response,
            candidate.is_some(),
            || begin("undo-macro-range"),
            &mut actions,
            |out| {
                if let Some(value) = candidate {
                    let (min, max) = if let Some(anchor) = drag.and_then(|drag| drag.collapsed_at) {
                        (value.min(anchor), value.max(anchor))
                    } else if is_min {
                        (value.clamp(0.0, high), high)
                    } else {
                        (low, value.clamp(low, 1.0))
                    };
                    out.push(Action::Macro(Macro::Range {
                        track,
                        index,
                        target: mapping.target.clone(),
                        min,
                        max,
                    }));
                }
            },
        );
        for action in actions {
            ctx.send(action);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn range_entry(
    ui: &mut Ui,
    response: &Response,
    is_min: bool,
    id: egui::Id,
    name: &str,
    current: f32,
    formatter: &impl Fn(f32) -> String,
    target: Option<&Target>,
) -> Option<f32> {
    let state = id.with(("range-entry", is_min));
    if !ui
        .ctx()
        .data(|d| d.get_temp::<bool>(state))
        .unwrap_or_default()
    {
        return None;
    }
    let started = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(state.with("started")))
        .unwrap_or_default();
    let (cancel, commit) = ui.input(|i| {
        (
            i.key_pressed(egui::Key::Escape),
            i.key_pressed(egui::Key::Enter),
        )
    });
    let mut value = current;
    let result = egui::Area::new(state)
        .order(egui::Order::Foreground)
        .fixed_pos(response.rect.left_bottom())
        .show(ui.ctx(), |ui| {
            let edit = ui.add(
                NumberField::new(&mut value, name)
                    .width(size::KNOB * 3.0)
                    .range(0.0..=1.0)
                    .step(NORMALIZED_STEP)
                    .format(|value| formatter(value as f32))
                    .parse(|text| match target {
                        Some(t) => t.parse(text).map(f64::from),
                        None => parse_number(text, "%").map(|value| value / 100.0),
                    }),
            );
            if !started {
                widgets::number_field::open_value_entry(ui.ctx(), edit.id, formatter(current));
                ui.ctx().request_repaint();
                ui.ctx()
                    .data_mut(|d| d.insert_temp(state.with("started"), true));
            }
            (
                edit.changed(),
                widgets::number_field::value_entry_open(ui.ctx(), edit.id),
            )
        });
    let (changed, entry_open) = result.inner;
    let outside = started && result.response.clicked_elsewhere();
    if cancel || changed || started && (!entry_open || outside) {
        ui.ctx().data_mut(|d| {
            d.remove::<bool>(state);
            d.remove::<bool>(state.with("started"));
        });
        if started && (cancel || commit) && !outside {
            response.request_focus();
        }
    }
    (changed && !cancel).then_some(value)
}

fn begin(label: &str) -> Action {
    Action::Edit(EditAction::BeginGesture {
        label: label.to_owned(),
    })
}
