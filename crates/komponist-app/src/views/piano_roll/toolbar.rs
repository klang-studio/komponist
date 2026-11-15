//! The toolbar row of the piano roll (`Screens.dc.html` dock markup, `ks-view.js` `prView`
//! `tools`, `fields`, `selT`):
//!
//! Zeiger · Stift · Radierer (segmented) · Raster ▾ · Snap · Quantisieren │ "2 NOTEN" · Vel ·
//! Länge · Note … − +
//!
//! The fields are the design system's [`NumberField`] with the key inside ("Vel 66"). They show
//! the selection (average velocity, length and key of the first selected note) and change it:
//! drag ↕ (1 px = 1 step, Shift fine, Ctrl coarse; one undo step per drag), mouse wheel, ↑ ↓ when
//! focused, AccessKit increment and decrement; Vel also takes typed values. Velocity moves all
//! selected notes by the same amount, Länge changes their lengths by whole grid steps, Note
//! transposes them. The fields hold no value of their own: each change becomes an offset from
//! the value at the start of the drag.

use egui::{Align, Event, Key, Layout, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_model::edits::VelocityChange;
use komponist_model::{ClipId, NoteId, Tick};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Icon, color, typography};
use komponist_ui::widgets::button::Button;
use komponist_ui::widgets::dropdown::{Dropdown, Item};
use komponist_ui::widgets::number_field::NumberField;
use komponist_ui::widgets::tabs::Segmented;
use komponist_ui::widgets::tooltip::Tip;
use komponist_ui::{px, widgets};

use super::edit;
use super::metrics as m;
use super::names;
use crate::actions::{Action, EditAction, GridTarget, UiAction};
use crate::ui_state::Tool;
use crate::views::{ViewCtx, grid};

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
    /// The editor shows the take being recorded: "LIVE" instead of the note count (S02).
    pub live: bool,
}

/// A drag on a field in progress: the field and the steps sent last.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldDrag {
    field: Field,
    /// The value when the drag started.
    start: f64,
    steps: i64,
    /// Esc cancelled the gesture (the app restored the document): nothing more is sent.
    cancelled: bool,
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
    let tool_tips: Vec<(String, Option<String>)> = tools
        .iter()
        .zip(&items)
        .map(|(t, item)| {
            (
                item.label.to_owned(),
                ctx.keymap
                    .label_for(&Action::Ui(UiAction::SetTool(*t)), ctx.lang),
            )
        })
        .collect();
    let tips: Vec<Tip<'_>> = tool_tips
        .iter()
        .map(|(name, sc)| match sc {
            Some(sc) => Tip::new(name).shortcut(sc),
            None => Tip::new(name),
        })
        .collect();
    if row
        .add(Segmented::new(names::TOOLS, &mut tool, &items).tips(&tips))
        .changed()
    {
        ctx.send(Action::Ui(UiAction::SetTool(tools[tool])));
    }

    // Grid, snap, quantise.
    let labels = grid::items();
    let mut selected = grid::index(pr.grid);
    if row
        .add(Dropdown::new(names::GRID, &mut selected, &labels).label(names::GRID))
        .changed()
    {
        ctx.send(Action::Ui(UiAction::SetGrid {
            target: GridTarget::PianoRoll,
            grid: grid::GRIDS[selected].0,
        }));
    }
    if row
        .add(Button::new(names::SNAP).on(pr.snap).small())
        .clicked()
    {
        ctx.send(Action::Ui(UiAction::ToggleSnap(GridTarget::PianoRoll)));
    }
    let quantize = Action::Edit(EditAction::QuantizeSelection);
    let shortcut = ctx
        .keymap
        .label_for(&quantize, ctx.lang)
        .unwrap_or_default();
    let mut button = Button::new(names::QUANTIZE);
    if !shortcut.is_empty() {
        button = button.chip(&shortcut);
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
    let text = if info.live {
        crate::record::names::LIVE.to_owned()
    } else {
        edit::selection_text(info.selected.len(), info.total)
    };
    let galley = widgets::styled_galley(&row, &text, &typography::LABEL);
    let (r, response) = row.allocate_exact_size(galley.size(), Sense::hover());
    row.painter().galley(r.min, galley, color32(color::TEXT_3));
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, &text));

    if !info.selected.is_empty() {
        fields(&mut row, ctx, info, drag);
    } else if drag.take().is_some() {
        // The selection went away during a field drag (Esc, undo): close its undo step.
        ctx.send(Action::Edit(EditAction::EndGesture));
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
            let shortcut = ctx.keymap.label_for(&action, ctx.lang);
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
    let meter = ctx.project.meter;
    let grid = info.grid.0.max(1) as f64;
    let esc = escape_pressed(row);
    for field in [Field::Velocity, Field::Length, Field::Note] {
        // The value shown, its step in the field's own unit, and its range.
        let (value, step, range) = match field {
            Field::Velocity => (f64::from(info.velocity), 1.0, 0.0..=127.0),
            Field::Length => (info.length.0 as f64, grid, grid..=grid * 4096.0),
            Field::Note => (f64::from(info.pitch), 1.0, 0.0..=127.0),
        };
        let mut shown = value;
        let widget = NumberField::new(&mut shown, field.name())
            .key(field.key())
            .range(range)
            .step(step)
            .default_value(value)
            .width(field.width());
        let widget = match field {
            Field::Velocity => widget,
            Field::Length => widget
                .format(move |v| edit::fmt_note_length(Tick(v as i64), meter))
                .no_text_entry(),
            Field::Note => widget.format(|v| edit::key_name(v as u8)).no_text_entry(),
        };
        let response = row.add(widget);
        let dragging = drag.is_some_and(|d| d.field == field);
        if response.drag_started() && !dragging {
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: field.label().to_owned(),
            }));
            *drag = Some(FieldDrag {
                field,
                start: value,
                steps: 0,
                cancelled: false,
            });
        }
        if dragging
            && esc
            && let Some(d) = drag.as_mut()
            && !d.cancelled
        {
            // The focused field has the keyboard, so the app's Esc shortcut does not run:
            // cancel the gesture here; the field sends nothing more.
            d.cancelled = true;
            ctx.send(Action::Edit(EditAction::CancelGesture));
        }
        if response.changed() {
            let base = match drag {
                Some(d) if d.field == field => d.start,
                _ => value,
            };
            let steps = ((shown - base) / step).round() as i64;
            match drag {
                Some(d) if d.field == field => {
                    if !d.cancelled && steps != d.steps {
                        d.steps = steps;
                        ctx.send(field.action(info, steps));
                    }
                }
                _ if steps != 0 => ctx.send(field.action(info, steps)),
                _ => {}
            }
        }
        if response.drag_stopped()
            && let Some(d) = drag.filter(|d| d.field == field)
        {
            if !d.cancelled {
                ctx.send(Action::Edit(if d.steps != 0 {
                    EditAction::EndGesture
                } else {
                    EditAction::CancelGesture
                }));
            }
            *drag = None;
        }
    }
}

/// Esc pressed in this frame's raw input (the app maps it to `CancelGesture` before the views).
fn escape_pressed(ui: &Ui) -> bool {
    ui.input(|i| {
        i.raw.events.iter().any(|e| {
            matches!(
                e,
                Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                }
            )
        })
    })
}

impl Field {
    /// The key shown inside the field.
    fn key(self) -> &'static str {
        match self {
            Field::Velocity => names::VELOCITY_KEY,
            Field::Length => names::LENGTH_FIELD,
            Field::Note => names::NOTE_FIELD,
        }
    }

    /// The accessible name of the field.
    fn name(self) -> &'static str {
        match self {
            Field::Velocity => names::VELOCITY_FIELD,
            Field::Length => names::LENGTH_FIELD,
            Field::Note => names::NOTE_FIELD,
        }
    }

    /// The width of the field.
    fn width(self) -> f32 {
        match self {
            Field::Velocity => m::FIELD_VELOCITY_WIDTH,
            Field::Length => m::FIELD_LENGTH_WIDTH,
            Field::Note => m::FIELD_NOTE_WIDTH,
        }
    }

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
