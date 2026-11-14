//! The toolbar row of the step sequencer (`Screens.dc.html` dock markup for `isStep`;
//! `ks-view.js` `stepView` `lenT`, `selT`, `fields`):
//!
//! Länge · Auflösung ▾ · Swing │ "STEP 11 · HAT ZU" · Vel · Wahr. · Ratch. … Als MIDI-Clip
//!
//! "Länge" (1–32 steps), "Swing" (0–75 %) and the step's "Vel" (1–127), "Wahr." (0–100 %) and
//! "Ratch." (1–4) are the design system's [`NumberField`] with the key inside; a drag on one is
//! one undo step. "Auflösung" is a [`Dropdown`] with a label. The step
//! info and its fields show the step under the cursor while it is set (`fields` is empty for an
//! off step); with only a row selected the info shows the row's name.

use egui::{Align, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::edits::{StepClipProp, StepProp};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, line, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{px, widgets};

use super::metrics as m;
use super::pattern::{self, RESOLUTIONS};
use super::{Open, names};
use crate::actions::{Action, EditAction};
use crate::views::ViewCtx;

/// One field: what it shows and its range.
#[derive(Debug, Clone, Copy)]
struct Spec<'a> {
    /// The key shown inside ("Länge").
    key: &'a str,
    /// The accessible name ("Länge", "Wahrscheinlichkeit").
    name: &'a str,
    /// The value.
    value: f64,
    /// Lowest value.
    min: f64,
    /// Highest value.
    max: f64,
    /// Alt-click value.
    default: f64,
    /// Unit after the value ("Steps", "%"), `""` for none.
    unit: &'a str,
    /// Width in points.
    width: f32,
}

/// What a field did in this pass. Values are whole numbers within the range.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Out {
    /// A drag started (the caller opens a gesture).
    drag_started: bool,
    /// The value while dragging.
    dragging: Option<f64>,
    /// The drag ended (the caller closes the gesture).
    drag_stopped: bool,
    /// A single change: wheel, ↑ ↓, Alt-click, text entry, AccessKit.
    set: Option<f64>,
}

/// Adds the field to `ui` and reports what the user did.
fn field(ui: &mut Ui, spec: Spec<'_>) -> Out {
    let mut value = spec.value;
    let response = ui.add(
        NumberField::new(&mut value, spec.name)
            .key(spec.key)
            .unit(spec.unit)
            .range(spec.min..=spec.max)
            .step(1.0)
            .decimals(0)
            .default_value(spec.default)
            .width(spec.width),
    );
    let mut out = Out {
        drag_started: response.drag_started(),
        drag_stopped: response.drag_stopped(),
        ..Out::default()
    };
    if response.changed() {
        let v = value.round().clamp(spec.min, spec.max);
        if response.dragged() || response.drag_started() {
            out.dragging = Some(v);
        } else {
            out.set = Some(v);
        }
    }
    out
}

/// A value field of the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Length,
    Swing,
    Velocity,
    Probability,
    Ratchet,
}

impl Field {
    /// Undo label of a drag on the field.
    fn label(self) -> &'static str {
        match self {
            Field::Length => "Pattern-Länge",
            Field::Swing => "Swing",
            Field::Velocity => "Velocity",
            Field::Probability => "Wahrscheinlichkeit",
            Field::Ratchet => "Ratchet",
        }
    }
}

/// What the toolbar keeps between frames: the field being dragged, and whether Esc cancelled
/// that drag (the app restores the state from its start; the field sends nothing more).
#[derive(Debug, Default)]
pub struct ToolbarState {
    drag: Option<Field>,
    cancelled: bool,
}

/// Draws the toolbar into `rect` (28 high; its 1 px `surface.0` line lies below it).
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut ToolbarState,
) {
    let ppp = ui.pixels_per_point();
    px::hline(
        ui.painter(),
        rect.x_range(),
        rect.bottom(),
        color32(color::SURFACE_0),
    );
    let inner = Rect::from_min_max(
        pos2(rect.left() + m::TOOLBAR_PAD_X, rect.top()),
        pos2(rect.right() - m::TOOLBAR_PAD_X, rect.bottom()),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .id_salt("komponist_step_toolbar")
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.set_clip_rect(ui.clip_rect().intersect(rect));
    row.spacing_mut().item_spacing = vec2(m::TOOLBAR_GAP, 0.0);
    let clip = open.clip;
    let pattern = open.pattern;
    if state.drag.is_some() && !state.cancelled && super::escape_pressed(ui) {
        ctx.send(Action::Edit(EditAction::CancelGesture));
        state.cancelled = true;
    }

    // Länge.
    let spec = Spec {
        key: names::LENGTH,
        name: names::LENGTH,
        value: f64::from(pattern.length_steps),
        min: 1.0,
        max: komponist_model::project::MAX_STEPS as f64,
        default: 16.0,
        unit: names::STEPS,
        width: m::FIELD_LENGTH_WIDTH,
    };
    let out = field(&mut row, spec);
    handle(Field::Length, out, spec.value, ctx, state, |v| {
        Action::Edit(EditAction::SetStepClipProp {
            clip,
            prop: StepClipProp::LengthSteps(v as u8),
        })
    });

    // Auflösung.
    resolution(&mut row, open, ctx);

    // Swing.
    let spec = Spec {
        key: names::SWING,
        name: names::SWING,
        value: f64::from(pattern::swing_percent(pattern.swing)),
        min: 0.0,
        max: 75.0,
        default: 0.0,
        unit: names::PERCENT,
        width: m::FIELD_SWING_WIDTH,
    };
    let out = field(&mut row, spec);
    handle(Field::Swing, out, spec.value, ctx, state, |v| {
        Action::Edit(EditAction::SetStepClipProp {
            clip,
            prop: StepClipProp::Swing(v as f32 / 100.0),
        })
    });

    // Divider.
    let (r, _) = row.allocate_exact_size(
        vec2(2.0 * m::DIVIDER_MARGIN + line::WIDTH, m::DIVIDER_HEIGHT),
        Sense::hover(),
    );
    px::vline(
        row.painter(),
        r.center().x - line::WIDTH / 2.0,
        r.y_range(),
        color32(color::LINE_1),
    );

    // Step info and the fields of the selected step.
    if let Some(cursor) = open.cursor {
        let name = &open.names[cursor.row];
        let text = match cursor.step {
            Some(step) => names::step_info(step, name),
            None => name.clone(),
        };
        let galley = widgets::styled_galley(&row, &text, &typography::LABEL);
        let (r, response) = row.allocate_exact_size(galley.size(), Sense::hover());
        row.painter().galley(
            pos2(px::snap(r.left(), ppp), px::snap(r.top(), ppp)),
            galley,
            color32(color::TEXT_3),
        );
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));
        if let Some(step) = cursor.step
            && let Some(s) = pattern.rows[cursor.row].steps.get(step).filter(|s| s.on)
        {
            step_fields(&mut row, open, ctx, state, cursor.row, step, s);
        }
    }

    // Als MIDI-Clip.
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let tip = Tip::new(names::TO_MIDI).detail(names::TO_MIDI_DETAIL);
        if ui.add(Button::new(names::TO_MIDI).tip(tip)).clicked() {
            ctx.send(Action::Edit(EditAction::StepClipToMidi(clip)));
        }
    });

    // A drag whose field went away (its step turned off) still ends.
    if state.drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
        end_drag(ctx, state);
    }
}

/// Vel, Wahr., Ratch. of step `step` in row `r`.
fn step_fields(
    row: &mut Ui,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut ToolbarState,
    r: usize,
    step: usize,
    s: &komponist_model::Step,
) {
    let clip = open.clip;
    let set = move |prop: StepProp| {
        Action::Edit(EditAction::SetStep {
            clip,
            row: r,
            step,
            prop,
        })
    };
    let specs = [
        (
            Field::Velocity,
            Spec {
                key: names::VELOCITY_KEY,
                name: names::VELOCITY,
                value: f64::from(s.velocity),
                min: 1.0,
                max: 127.0,
                default: 100.0,
                unit: "",
                width: m::FIELD_VELOCITY_WIDTH,
            },
        ),
        (
            Field::Probability,
            Spec {
                key: names::PROBABILITY_KEY,
                name: names::PROBABILITY,
                value: f64::from(s.probability),
                min: 0.0,
                max: 100.0,
                default: 100.0,
                unit: names::PERCENT,
                width: m::FIELD_PROBABILITY_WIDTH,
            },
        ),
        (
            Field::Ratchet,
            Spec {
                key: names::RATCHET_KEY,
                name: names::RATCHET,
                value: f64::from(s.ratchet),
                min: 1.0,
                max: 4.0,
                default: 1.0,
                unit: "",
                width: m::FIELD_RATCHET_WIDTH,
            },
        ),
    ];
    for (f, spec) in specs {
        let out = field(row, spec);
        handle(f, out, spec.value, ctx, state, |v| {
            let v = v as u8;
            set(match f {
                Field::Velocity => StepProp::Velocity(v),
                Field::Probability => StepProp::Probability(v),
                _ => StepProp::Ratchet(v),
            })
        });
    }
}

/// Turns a field's output into actions: a drag is one gesture, other changes one undo step
/// each. `current` is the value in the document; unchanged values send nothing.
fn handle(
    f: Field,
    out: Out,
    current: f64,
    ctx: &mut ViewCtx<'_>,
    state: &mut ToolbarState,
    action: impl Fn(f64) -> Action,
) {
    if out.drag_started && state.drag != Some(f) {
        if state.drag.is_some() {
            end_drag(ctx, state);
        }
        ctx.send(Action::Edit(EditAction::BeginGesture {
            label: f.label().to_owned(),
        }));
        state.drag = Some(f);
    }
    if let Some(v) = out.dragging
        && v != current
        && !state.cancelled
    {
        ctx.send(action(v));
    }
    if out.drag_stopped && state.drag == Some(f) {
        end_drag(ctx, state);
    }
    if let Some(v) = out.set
        && v != current
        && state.drag.is_none()
    {
        ctx.send(action(v));
    }
}

/// Ends the field drag: one undo step, unless Esc cancelled it.
fn end_drag(ctx: &mut ViewCtx<'_>, state: &mut ToolbarState) {
    if !state.cancelled {
        ctx.send(Action::Edit(EditAction::EndGesture));
    }
    state.drag = None;
    state.cancelled = false;
}

/// "Auflösung ▾": the dropdown with its key inside the box (`gap:10px`, S03).
fn resolution(row: &mut Ui, open: &Open<'_>, ctx: &mut ViewCtx<'_>) {
    let clip = open.clip;
    let items: Vec<Item<'_>> = RESOLUTIONS.iter().map(|(_, l)| Item::new(l)).collect();
    let mut selected = pattern::resolution_index(open.pattern.resolution);
    let response =
        row.add(Dropdown::new(names::RESOLUTION, &mut selected, &items).label(names::RESOLUTION));
    if response.changed()
        && let Some((res, _)) = RESOLUTIONS.get(selected)
    {
        ctx.send(Action::Edit(EditAction::SetStepClipProp {
            clip,
            prop: StepClipProp::Resolution(*res),
        }));
    }
}
