//! The clip section: MIDI clips (S01 `sec({title:'Clip', meta:'MIDI', … fields:[Start, Länge,
//! Loop, Loop-Länge, Transponieren, Velocity]})` and the takes after loop recording), step clips
//! (S03: Start, Länge, Pattern, Auflösung, Swing, Loop) and the take being recorded (S02:
//! "nimmt auf", Start, Länge, Noten, Quantisieren).

use egui::{Rangef, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::edits::{ClipProp, StepClipProp};
use komponist_model::format::{fmt_bar_pos, fmt_length};
use komponist_model::{Clip, ClipContent, ClipId, Project, StepClip, Tick, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius, typography};
use komponist_ui::state::{self, State};
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field::NumberField;

use super::model::{self, names};
use super::{
    Col, Inspector, Meta, RenameTarget, field_grid, field_key, frame, label, metrics as m, place,
    read_only_field, switch_field,
};
use crate::actions::{Action, EditAction};
use crate::views::mixer::send_gesture;
use crate::views::step_seq::pattern::RESOLUTIONS;
use crate::views::{ViewCtx, group_bounds};

/// Longest position or length the fields accept, in 1/16 steps (999 bars).
const MAX_STEPS: f64 = 999.0 * 16.0;

/// The clip section of clip `id`.
pub(super) fn section(
    insp: &mut Inspector,
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
    id: ClipId,
) {
    let Some((track, clip)) = project.find_clip(id) else {
        return;
    };
    let kind = match clip.content {
        ClipContent::Midi(_) => names::MIDI,
        ClipContent::Step(_) => names::STEP,
    };
    let chip = crate::views::mixer::model::base_color(track.hue.0);
    frame(ui, x, y, names::CLIP, Meta::Text(kind), |ui, col| {
        let target = RenameTarget::Clip(id);
        let (name_rect, rename) =
            super::name_row(ui, col, &clip.name, chip, false, !insp.renaming(&target));
        if rename {
            insp.start_rename(target.clone(), &clip.name);
        }
        insp.rename_field(ui, &target, &clip.name, name_rect, |name| {
            ctx.send(Action::Edit(EditAction::SetClipProp {
                clip: id,
                prop: ClipProp::Name(name),
            }));
        });
        match &clip.content {
            ClipContent::Midi(_) => midi_fields(ui, ctx, project, col, clip),
            ClipContent::Step(step) => step_fields(ui, ctx, project, col, clip, step),
        }
        takes(ui, ctx, col, clip);
    });
}

/// Start and Länge, the first two fields of every clip.
fn position_fields(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    cells: &[(Rect, Rect)],
    clip: &Clip,
) {
    let meter = project.meter;
    let id = clip.id;
    let (key, field) = cells[0];
    field_key(ui, key, names::START);
    let mut start = model::to_steps(clip.start);
    let r = place(ui, field, "start", |ui| {
        ui.add(
            NumberField::new(&mut start, names::START)
                .range(0.0..=MAX_STEPS)
                .step(1.0)
                .default_value(model::to_steps(clip.start))
                .width(field.width())
                .format(move |v| fmt_bar_pos(model::from_steps(v), meter))
                .parse(move |t| model::parse_bar_pos(t, meter).map(model::to_steps)),
        )
    });
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetClipProp {
            clip: id,
            prop: ClipProp::Start(model::from_steps(start)),
        })
    });

    let (key, field) = cells[1];
    field_key(ui, key, names::LENGTH);
    let mut length = model::to_steps(clip.length);
    let r = place(ui, field, "length", |ui| {
        ui.add(
            NumberField::new(&mut length, names::LENGTH)
                .range(1.0..=MAX_STEPS)
                .step(1.0)
                .default_value(model::to_steps(clip.length))
                .width(field.width())
                .format(move |v| fmt_length(model::from_steps(v), meter))
                .parse(move |t| model::parse_length(t, meter).map(model::to_steps)),
        )
    });
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetClipLength {
            clip: id,
            length: model::from_steps(length).max(komponist_model::Tick(1)),
        })
    });
}

/// MIDI clip: Start, Länge, Loop, Loop-Länge, Transponieren, Velocity.
fn midi_fields(ui: &mut Ui, ctx: &mut ViewCtx<'_>, project: &Project, col: &mut Col, clip: &Clip) {
    let cells = field_grid(col, 6);
    position_fields(ui, ctx, project, &cells, clip);
    let id = clip.id;
    let meter = project.meter;

    let (key, field) = cells[2];
    field_key(ui, key, names::LOOP);
    let looped = clip.loop_length.is_some();
    if switch_field(ui, field, names::LOOP, looped, false) {
        ctx.send(Action::Edit(EditAction::SetClipLoop {
            clip: id,
            loop_length: (!looped).then_some(clip.length),
            length: clip.length,
        }));
    }

    let (key, field) = cells[3];
    field_key(ui, key, names::LOOP_LENGTH);
    let loop_default = model::to_steps(clip.loop_length.unwrap_or(clip.length));
    let mut loop_len = loop_default;
    let r = place(ui, field, "loop_length", |ui| {
        ui.add_enabled(
            looped,
            NumberField::new(&mut loop_len, names::LOOP_LENGTH)
                .range(1.0..=MAX_STEPS)
                .step(1.0)
                .default_value(loop_default)
                .width(field.width())
                .format(move |v| fmt_length(model::from_steps(v), meter))
                .parse(move |t| model::parse_length(t, meter).map(model::to_steps)),
        )
    });
    let length = clip.length;
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetClipLoop {
            clip: id,
            loop_length: Some(model::from_steps(loop_len).max(Tick(1))),
            length,
        })
    });

    let (key, field) = cells[4];
    field_key(ui, key, names::TRANSPOSE);
    let mut semis = f64::from(clip.transpose);
    let r = place(ui, field, "transpose", |ui| {
        ui.add(
            NumberField::new(&mut semis, names::TRANSPOSE)
                .unit(names::SEMITONES)
                .range(-48.0..=48.0)
                .step(1.0)
                .default_value(0.0)
                .width(field.width())
                .format(|v| model::signed(v.round() as i32, false))
                .parse(model::parse_signed),
        )
    });
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetClipProp {
            clip: id,
            prop: ClipProp::Transpose(semis.round() as i8),
        })
    });

    let (key, field) = cells[5];
    field_key(ui, key, names::VELOCITY);
    let mut pct = f64::from(clip.velocity_pct);
    let r = place(ui, field, "velocity", |ui| {
        ui.add(
            NumberField::new(&mut pct, names::VELOCITY)
                .unit(names::PERCENT)
                .range(-100.0..=100.0)
                .step(1.0)
                .default_value(0.0)
                .width(field.width())
                .format(|v| model::signed(v.round() as i32, true))
                .parse(model::parse_signed),
        )
    });
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetClipProp {
            clip: id,
            prop: ClipProp::VelocityPct(pct.round() as i8),
        })
    });
}

/// Step clip: Start, Länge, Pattern, Auflösung, Swing, Loop (always on: the pattern repeats).
fn step_fields(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    col: &mut Col,
    clip: &Clip,
    step: &StepClip,
) {
    let cells = field_grid(col, 6);
    position_fields(ui, ctx, project, &cells, clip);
    let id = clip.id;

    let (key, field) = cells[2];
    field_key(ui, key, names::PATTERN);
    let mut steps = f64::from(step.length_steps);
    let r = place(ui, field, "pattern", |ui| {
        ui.add(
            NumberField::new(&mut steps, names::PATTERN)
                .unit(names::STEPS)
                .range(1.0..=32.0)
                .step(1.0)
                .default_value(16.0)
                .width(field.width()),
        )
    });
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetStepClipProp {
            clip: id,
            prop: StepClipProp::LengthSteps(steps.round() as u8),
        })
    });

    let (key, field) = cells[3];
    field_key(ui, key, names::RESOLUTION);
    let items: Vec<Item<'_>> = RESOLUTIONS.iter().map(|(_, l)| Item::new(l)).collect();
    let mut selected = RESOLUTIONS
        .iter()
        .position(|(r, _)| *r == step.resolution)
        .unwrap_or(1);
    let r = place(ui, field, "resolution", |ui| {
        ui.add(Dropdown::new(names::RESOLUTION, &mut selected, &items).width(field.width()))
    });
    if r.changed()
        && let Some((res, _)) = RESOLUTIONS.get(selected)
    {
        ctx.send(Action::Edit(EditAction::SetStepClipProp {
            clip: id,
            prop: StepClipProp::Resolution(*res),
        }));
    }

    let (key, field) = cells[4];
    field_key(ui, key, names::SWING);
    let mut swing = f64::from((step.swing * 100.0).round());
    let r = place(ui, field, "swing", |ui| {
        ui.add(
            NumberField::new(&mut swing, names::SWING)
                .unit(names::PERCENT)
                .range(0.0..=75.0)
                .step(1.0)
                .default_value(0.0)
                .width(field.width()),
        )
    });
    send_gesture(ctx, &r, names::UNDO_CLIP, || {
        Action::Edit(EditAction::SetStepClipProp {
            clip: id,
            prop: StepClipProp::Swing(swing as f32 / 100.0),
        })
    });

    let (key, field) = cells[5];
    field_key(ui, key, names::LOOP);
    switch_field(ui, field, names::LOOP, true, true);
}

/// The takes of a loop-recorded clip ("wählbar im Inspector"): one row each, the active one in
/// `accent.surface`; a click makes a take the active one (`SelectTake`).
fn takes(ui: &mut Ui, ctx: &mut ViewCtx<'_>, col: &mut Col, clip: &Clip) {
    let rows = model::take_rows(&clip.content);
    if rows.is_empty() {
        return;
    }
    let key = col.take(typography::SMALL.line_height);
    super::field_key(ui, key, names::TAKES);
    let n = rows.len();
    let h = n as f32 * m::TAKE_ROW + (n - 1) as f32 * m::TAKE_GAP;
    let area = col.take(h);
    let list = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .id_salt(("komponist_inspector_takes", clip.id.0))
            .accessibility_role(Role::ListBox)
            .accessibility_label(names::TAKES),
    );
    group_bounds(&list, area);
    for (i, take) in rows.iter().enumerate() {
        let top = area.top() + i as f32 * (m::TAKE_ROW + m::TAKE_GAP);
        let row = Rect::from_min_size(pos2(area.left(), top), vec2(area.width(), m::TAKE_ROW));
        let response = list.interact(row, list.scope_id().with(("take", i)), Sense::click());
        let st = State::of(&list, &response).with_selected(take.active);
        komponist_ui::px::fill(list.painter(), row, radius::M, state::row_fill(2, st));
        let inner = row.shrink2(vec2(m::TAKE_PAD_X, 0.0));
        let notes = names::notes(take.notes);
        let style = typography::VALUE_S;
        let g = komponist_ui::widgets::galley(
            &list,
            &notes,
            komponist_ui::fonts::font_id(&style),
            style.line_height,
        );
        let pos = komponist_ui::widgets::text_pos(
            inner,
            &g,
            inner.right() - g.size().x,
            list.pixels_per_point(),
        );
        let notes_left = pos.x;
        list.painter().galley(pos, g, color32(color::TEXT_3));
        let name_rect =
            Rect::from_x_y_ranges(inner.left()..=notes_left - m::TAKE_PAD_X, inner.y_range());
        let weight = take.active.then_some(super::SEMIBOLD);
        label(
            &list,
            name_rect,
            &take.name,
            &typography::UI,
            weight,
            color::TEXT_1,
        );
        if st.focused {
            komponist_ui::focus::paint_ring(list.painter(), row, radius::M);
        }
        let name = take.name.clone();
        let active = take.active;
        response.widget_info(|| {
            let mut info = WidgetInfo::selected(Role::ListBoxOption, true, active, &name);
            info.current_text_value = Some(notes.clone());
            info
        });
        list.ctx()
            .accesskit_node_builder(response.id, |n| n.set_selected(active));
        if response.clicked() && !take.active {
            ctx.send(Action::Edit(EditAction::SelectTake {
                clip: clip.id,
                take: i,
            }));
        }
    }
}

/// The take being recorded on `track` (S02): "nimmt auf", the take's name with a
/// `state.record` chip, Start, Länge, Noten, Quantisieren aus.
pub(super) fn recording_section(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    project: &Project,
    x: Rangef,
    y: &mut f32,
    track: TrackId,
) {
    let Some(rec) = ctx.recording else {
        return;
    };
    let Some(clip) = rec.clip(track) else {
        return;
    };
    let name = rec.take_name(track).unwrap_or_else(|| clip.name.clone());
    let notes = match &clip.content {
        ClipContent::Midi(m) => m.active().notes.len(),
        ClipContent::Step(_) => 0,
    };
    let meter = project.meter;
    let (start, length) = (clip.start, clip.length);
    frame(
        ui,
        x,
        y,
        names::CLIP,
        Meta::Record(names::RECORDING),
        |ui, col| {
            super::name_row(ui, col, &name, color::STATE_RECORD, false, false);
            let cells = field_grid(col, 4);
            let values = [
                (names::START, fmt_bar_pos(start, meter)),
                (names::LENGTH, fmt_length(length, meter)),
                (names::NOTES, notes.to_string()),
                (names::QUANTIZE, names::OFF.to_owned()),
            ];
            for ((key, field), (k, v)) in cells.into_iter().zip(values) {
                field_key(ui, key, k);
                read_only_field(ui, field, k, &v, "");
            }
        },
    );
}
