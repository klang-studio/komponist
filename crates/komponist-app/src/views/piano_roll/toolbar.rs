//! The toolbar row of the piano roll (`Screens.dc.html` dock markup, `ks-view.js` `prView`
//! `tools`, `fields`, `selT`):
//!
//! Zeiger · Stift · Radierer (segmented) · Raster ▾ · Snap · Quantisieren │ "2 NOTEN" · Vel ·
//! Länge · Note … − +
//!
//! The fields show the selection (average velocity, length and key of the first selected note)
//! and change it: drag ↕ (1 px = 1 step, Shift fine, Ctrl coarse; one undo step per drag), mouse
//! wheel, ↑ ↓ when focused, AccessKit increment and decrement. Velocity moves all selected notes
//! by the same amount, Länge changes their lengths by whole grid steps, Note transposes them.

use egui::accesskit::Action as AkAction;
use egui::{Align, Key, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::edits::VelocityChange;
use komponist_model::{ClipId, NoteId, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, radius, size, space, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field;
use komponist_ui::widgets::tabs::Segmented;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{State, drag, focus, fonts, px, widgets};

use super::edit::{self, GRIDS};
use super::metrics as m;
use super::names;
use crate::actions::{Action, EditAction, UiAction};
use crate::ui_state::Tool;
use crate::views::ViewCtx;

/// What the toolbar shows about the open clip.
pub struct Info {
    /// The open clip.
    pub clip: ClipId,
    /// Selected notes in clip order.
    pub selected: Vec<NoteId>,
    /// Notes in the clip.
    pub total: usize,
    /// Average velocity of the selection.
    pub velocity: u8,
    /// Length of the first selected note.
    pub length: Tick,
    /// Key of the first selected note.
    pub pitch: u8,
    /// Grid ticks (one "Länge" step).
    pub grid: Tick,
}

/// A drag on a field in progress: the field and the steps sent last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDrag {
    field: Field,
    steps: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Velocity,
    Length,
    Note,
}

/// Draws the toolbar into `rect`; `drag` keeps a field drag between frames.
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    ctx: &mut ViewCtx<'_>,
    info: &Info,
    drag: &mut Option<FieldDrag>,
) {
    let ppp = ui.pixels_per_point();
    px::hline(
        ui.painter(),
        rect.x_range(),
        rect.bottom() - px::line_width(ppp),
        color32(color::SURFACE_0),
    );
    let inner = Rect::from_min_max(
        pos2(rect.left() + m::TOOLBAR_PAD_X, rect.top()),
        pos2(
            rect.right() - m::TOOLBAR_PAD_X,
            rect.bottom() - px::line_width(ppp),
        ),
    );
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .id_salt("komponist_piano_roll_toolbar")
            .layout(Layout::left_to_right(Align::Center)),
    );
    row.spacing_mut().item_spacing = vec2(m::TOOLBAR_GAP, 0.0);

    // Tools.
    let tools = [Tool::Pointer, Tool::Pencil, Tool::Eraser];
    let items = [
        Item::new(names::POINTER),
        Item::new(names::PENCIL),
        Item::new(names::ERASER),
    ];
    let pr = ctx.ui.piano_roll;
    let mut tool = tools.iter().position(|t| *t == pr.tool).unwrap_or(0);
    if row
        .add(Segmented::new(names::TOOLS, &mut tool, &items))
        .changed()
    {
        ctx.send(Action::Ui(UiAction::SetTool(tools[tool])));
    }

    // Grid, snap, quantise.
    let labels: Vec<Item<'_>> = GRIDS
        .iter()
        .map(|g| Item::new(edit::grid_label(*g)))
        .collect();
    let mut grid = GRIDS.iter().position(|g| *g == pr.grid).unwrap_or(4);
    if row
        .add(Dropdown::new(names::GRID, &mut grid, &labels))
        .changed()
    {
        ctx.send(Action::Ui(UiAction::SetGrid(GRIDS[grid])));
    }
    if row.add(Button::new(names::SNAP).on(pr.snap)).clicked() {
        ctx.send(Action::Ui(UiAction::ToggleSnap));
    }
    let quantize = Action::Edit(EditAction::QuantizeSelection);
    let shortcut = ctx.keymap.label_for(&quantize).unwrap_or_default();
    let mut button = Button::new(names::QUANTIZE);
    if !shortcut.is_empty() {
        button = button.shortcut(&shortcut);
    }
    if row.add(button).clicked() {
        ctx.send(quantize);
    }

    // Divider, selection info, fields.
    let (r, _) = row.allocate_exact_size(
        vec2(
            2.0 * m::DIVIDER_MARGIN + px::line_width(ppp),
            m::DIVIDER_HEIGHT,
        ),
        Sense::hover(),
    );
    px::vline(
        row.painter(),
        r.center().x,
        r.y_range(),
        color32(color::LINE_1),
    );
    let text = edit::selection_text(info.selected.len(), info.total);
    let galley = widgets::styled_galley(&row, &text, &typography::LABEL);
    let (r, response) = row.allocate_exact_size(galley.size(), Sense::hover());
    row.painter().galley(r.min, galley, color32(color::TEXT_3));
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));

    if !info.selected.is_empty() {
        fields(&mut row, ctx, info, drag);
    }

    // Zoom buttons on the right.
    row.with_layout(Layout::right_to_left(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = m::ZOOM_GAP;
        for (icon, name, steps) in [
            (Icon::Plus, names::ZOOM_IN, 1),
            (Icon::Minus, names::ZOOM_OUT, -1),
        ] {
            let action = Action::Ui(UiAction::Zoom {
                vertical: false,
                steps,
            });
            let shortcut = ctx.keymap.label_for(&action);
            let tip = match shortcut.as_deref() {
                Some(sc) => Tip::new(name).shortcut(sc),
                None => Tip::new(name),
            };
            let b = Button::icon(icon, name).compact().tip(tip);
            if ui.add(b).clicked() {
                ctx.send(action);
            }
        }
    });
}

fn fields(row: &mut Ui, ctx: &mut ViewCtx<'_>, info: &Info, drag: &mut Option<FieldDrag>) {
    let specs = [
        (
            Field::Velocity,
            names::VELOCITY_KEY,
            names::VELOCITY_FIELD,
            info.velocity.to_string(),
            f64::from(info.velocity),
            m::FIELD_VELOCITY_WIDTH,
        ),
        (
            Field::Length,
            names::LENGTH_FIELD,
            names::LENGTH_FIELD,
            edit::fmt_note_length(info.length, ctx.project.meter),
            info.length.0 as f64,
            m::FIELD_LENGTH_WIDTH,
        ),
        (
            Field::Note,
            names::NOTE_FIELD,
            names::NOTE_FIELD,
            edit::key_name(info.pitch),
            f64::from(info.pitch),
            m::FIELD_NOTE_WIDTH,
        ),
    ];
    for (field, key, name, text, value, width) in specs {
        let out = key_field(row, key, name, &text, value, width);
        let dragging = drag.is_some_and(|d| d.field == field);
        if out.drag_started && !dragging {
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: field.label().to_owned(),
            }));
            *drag = Some(FieldDrag { field, steps: 0 });
        }
        if let (Some(steps), Some(d)) = (out.drag_steps, drag.as_mut())
            && d.field == field
            && steps != d.steps
        {
            d.steps = steps;
            ctx.send(field.action(info, steps));
        }
        if out.drag_stopped && drag.is_some_and(|d| d.field == field) {
            let moved = drag.is_some_and(|d| d.steps != 0);
            ctx.send(Action::Edit(if moved {
                EditAction::EndGesture
            } else {
                EditAction::CancelGesture
            }));
            *drag = None;
        }
        if out.steps != 0 && drag.is_none() {
            ctx.send(field.action(info, out.steps));
        }
    }
}

impl Field {
    /// Undo label of a drag on the field.
    fn label(self) -> &'static str {
        match self {
            Field::Velocity => "Velocity",
            Field::Length => "Noten-Länge",
            Field::Note => "Noten transponieren",
        }
    }

    /// The edit for `steps` steps from the value at the drag start (or now, for single steps).
    fn action(self, info: &Info, steps: i64) -> Action {
        let clip = info.clip;
        let notes = info.selected.clone();
        Action::Edit(match self {
            Field::Velocity => EditAction::SetNotesVelocity {
                clip,
                notes,
                change: VelocityChange::Offset(steps.clamp(-127, 127) as i16),
            },
            Field::Length => EditAction::ResizeNotes {
                clip,
                notes,
                delta: Tick(info.grid.0 * steps),
            },
            Field::Note => EditAction::MoveNotes {
                clip,
                notes,
                delta: Tick::ZERO,
                pitch_delta: steps.clamp(-127, 127) as i32,
            },
        })
    }
}

/// What a [`key_field`] did this frame.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FieldOut {
    /// A drag started.
    drag_started: bool,
    /// Steps since the drag start (while dragging).
    drag_steps: Option<i64>,
    /// The drag ended.
    drag_stopped: bool,
    /// Single steps: wheel, ↑ ↓, AccessKit.
    steps: i64,
}

/// A value field as the screens' toolbar draws it (`fs()`): `surface.2`, 24 high, `radius.m`,
/// padding 0 8, the key left (`type.value.s`, `text.3`) and the value right (`type.value.m`).
/// Looks and states are the number field's ([`number_field::look`]); it has no text entry.
fn key_field(ui: &mut Ui, key: &str, name: &str, text: &str, value: f64, width: f32) -> FieldOut {
    let (rect, response) =
        ui.allocate_exact_size(vec2(width, size::CONTROL), Sense::click_and_drag());
    let id = response.id;
    let mut out = FieldOut::default();
    let speed = ui.input(|i| drag::speed(i.modifiers));
    let acc_id = id.with("komponist_piano_roll_field_px");
    if response.drag_started() {
        out.drag_started = true;
        // egui reports the drag after the pointer moved a few points: count those too.
        let moved = ui.input(
            |i| match (i.pointer.press_origin(), i.pointer.latest_pos()) {
                (Some(o), Some(p)) => f64::from(o.y - p.y),
                _ => 0.0,
            },
        );
        let acc = moved * speed / drag::NUMBER_PX_PER_STEP;
        ui.data_mut(|d| d.insert_temp(acc_id, acc));
        out.drag_steps = Some(acc.trunc() as i64);
    } else if response.dragged() {
        let mut acc: f64 = ui.data(|d| d.get_temp(acc_id)).unwrap_or(0.0);
        acc += -f64::from(response.drag_delta().y) * speed / drag::NUMBER_PX_PER_STEP;
        ui.data_mut(|d| d.insert_temp(acc_id, acc));
        out.drag_steps = Some(acc.trunc() as i64);
    }
    if response.drag_stopped() {
        out.drag_stopped = true;
    }
    if response.hovered() || response.dragged() {
        ui.set_cursor_icon(egui::CursorIcon::ResizeVertical);
        if response.hovered() {
            out.steps += i64::from(widgets::wheel_steps(ui, id));
        }
    }
    if response.has_focus() {
        ui.input(|i| {
            out.steps += i64::from(i.key_pressed(Key::ArrowUp));
            out.steps -= i64::from(i.key_pressed(Key::ArrowDown));
        });
    }
    out.steps += ui.input(|i| {
        i.num_accesskit_action_requests(id, AkAction::Increment) as i64
            - i.num_accesskit_action_requests(id, AkAction::Decrement) as i64
    });

    let st = State::of(ui, &response);
    let look = number_field::look(st, false);
    let painter = ui.painter();
    let ppp = ui.pixels_per_point();
    px::fill(painter, rect, radius::M, look.fill);
    let pad = space::SPACE_2;
    let k = painter.layout_job(fonts::layout_job(
        key,
        &typography::VALUE_S,
        egui::Color32::PLACEHOLDER,
    ));
    let pos = widgets::text_pos(rect, &k, rect.left() + pad, ppp);
    painter.galley(pos, k, look.unit);
    let v = painter.layout_job(fonts::layout_job(
        text,
        &typography::VALUE_M,
        egui::Color32::PLACEHOLDER,
    ));
    let pos = widgets::text_pos(rect, &v, rect.right() - pad - v.size().x, ppp);
    painter.galley(pos, v, look.value);
    if look.focus_ring {
        focus::paint_ring(painter, rect, radius::M);
    }
    response.widget_info(|| {
        let mut w = WidgetInfo::labeled(Role::SpinButton, true, name);
        w.value = Some(value);
        w.current_text_value = Some(text.to_owned());
        w
    });
    widgets::accesskit(&response, |n| {
        n.add_action(AkAction::Increment);
        n.add_action(AkAction::Decrement);
    });
    out
}
