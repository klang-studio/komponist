//! The step numbers and the rows: row headers (colour chip, name, pad or note, M S) and the step
//! cells (`Screens.dc.html` `stepSeq()` `nums`, `rows`, `cells`; `ks-view.js` `stepView`).
//!
//! A cell that is off is `surface.2`, in every other group of four `canvas.step-group-alt`; a
//! set cell has the note colour of its velocity (`TrackPalette::note`). Ratchet `n` splits it
//! into `n` parts on the off colour, a probability below 100 % stands bottom right in
//! `surface.0`. The step cursor has a 1 px accent ring; the column of the running light is
//! `surface.4`, its set steps `TrackPalette::play` with a `text.1` ring.
//!
//! The canvas hit-tests itself: every visible step and row header is an egui widget (so
//! AccessKit and kittest find them), and a press starts a [`Gesture`] that follows the pointer
//! over the row until the release.

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Color32, Event, FontId, Galley, Id, PointerButton, Pos2, Rangef, Rect, Role, Sense, Ui,
    UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_model::edits::StepRowProp;
use komponist_model::{ClipId, Step, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, line, radius, typography};
use komponist_ui::widgets::metrics as wm;
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::{px, widgets};
use std::sync::Arc;

use super::metrics as m;
use super::names;
use super::pattern::{self, Gesture, PressMods};
use super::{Areas, Open, row_pitch};
use crate::actions::{Action, EditAction, SelectionChange, TransportAction, UiAction};
use crate::views::ViewCtx;

/// What the grid keeps between frames.
#[derive(Debug, Default)]
pub struct GridState {
    /// The step gesture in progress and its clip.
    gesture: Option<(ClipId, Gesture)>,
    /// A press on a step or a row header is held; its release ends the gesture and the
    /// audition.
    pressed: bool,
    /// The row note sounding since a press: track and key.
    audition: Option<(TrackId, u8)>,
    /// A press on M or S is held and its changes form one gesture.
    toggle_gesture: bool,
    /// Vertical scroll of the rows in points (when they do not fit).
    pub scroll: f32,
}

impl GridState {
    /// Closes what is open (the clip went away): the gesture, the sounding note.
    pub fn abandon(&mut self, ctx: &mut ViewCtx<'_>) {
        if self.gesture.take().is_some() || self.toggle_gesture {
            ctx.send(Action::Edit(EditAction::EndGesture));
        }
        self.toggle_gesture = false;
        self.pressed = false;
        if let Some((track, key)) = self.audition.take() {
            ctx.send(live_note(track, key, 0));
        }
    }
}

/// The step columns right of the row headers: `count` cells of equal width, `gap` apart, inside
/// the padding (`flex:1; gap:4px; padding:… 8px`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Columns {
    /// Left edge of the first cell.
    pub left: f32,
    /// Width of a cell.
    pub width: f32,
    /// Number of cells.
    pub count: usize,
}

impl Columns {
    /// The columns of `count` steps in the full-width `rect` (headers on the left).
    pub fn new(rect: Rect, count: usize) -> Columns {
        let left = rect.left() + m::HEADER_WIDTH + m::CELLS_PAD_X;
        let right = rect.right() - m::CELLS_PAD_X;
        let n = count.max(1) as f32;
        let width = ((right - left - (n - 1.0) * m::CELL_GAP) / n).max(1.0);
        Columns {
            left,
            width,
            count: count.max(1),
        }
    }

    /// The x range of cell `i`.
    pub fn x(&self, i: usize) -> Rangef {
        let x = self.left + i as f32 * (self.width + m::CELL_GAP);
        Rangef::new(x, x + self.width)
    }

    /// The cell under `x`; each gap belongs half to its neighbours. `None` outside.
    pub fn at(&self, x: f32) -> Option<usize> {
        let rel = x - self.left + m::CELL_GAP / 2.0;
        if rel < 0.0 {
            return None;
        }
        let i = (rel / (self.width + m::CELL_GAP)).floor() as usize;
        (i < self.count).then_some(i)
    }
}

/// How a cell looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellLook {
    /// The cell's surface.
    pub fill: Color32,
    /// Ratchet parts: count and colour.
    pub parts: Option<(usize, Color32)>,
    /// Probability shown bottom right.
    pub probability: Option<u8>,
    /// The 1 px ring outside: cursor (accent) or running light (`text.1`).
    pub ring: Option<Color32>,
}

/// The look of `step` at pattern index `index`; `current` = in the running light's column,
/// `selected` = under the step cursor (`stepView` `cells`).
pub fn cell_look(
    step: &Step,
    index: usize,
    current: bool,
    selected: bool,
    palette: &TrackPalette,
) -> CellLook {
    let off = if current {
        color::SURFACE_4
    } else if (index / 4) % 2 == 1 {
        color::CANVAS_STEP_GROUP_ALT
    } else {
        color::SURFACE_2
    };
    let select_ring = selected.then(|| color32(color::ACCENT));
    if !step.on {
        return CellLook {
            fill: color32(off),
            parts: None,
            probability: None,
            ring: select_ring,
        };
    }
    let on = color32(if current {
        palette.play
    } else {
        palette.note(step.velocity)
    });
    let ratchet = usize::from(step.ratchet.clamp(1, 4));
    CellLook {
        fill: if ratchet > 1 { color32(off) } else { on },
        parts: (ratchet > 1).then_some((ratchet, on)),
        probability: (step.probability < 100).then_some(step.probability),
        ring: select_ring.or(current.then(|| color32(color::TEXT_1))),
    }
}

/// Paints a cell with `look` into `rect`.
pub fn paint_cell(ui: &Ui, rect: Rect, look: &CellLook) {
    let painter = ui.painter();
    let ppp = ui.pixels_per_point();
    px::fill(painter, rect, radius::S, look.fill);
    if let Some((n, c)) = look.parts {
        let w = (rect.width() - (n - 1) as f32 * m::RATCHET_GAP) / n as f32;
        for k in 0..n {
            let x = rect.left() + k as f32 * (w + m::RATCHET_GAP);
            let part = Rect::from_x_y_ranges(x..=x + w, rect.y_range());
            px::fill(painter, part, m::RATCHET_RADIUS, c);
        }
    }
    if let Some(p) = look.probability {
        let g = widgets::styled_galley(ui, &format!("{p}%"), &typography::VALUE_S);
        let pos = pos2(
            px::snap(rect.right() - m::PROBABILITY_RIGHT - g.size().x, ppp),
            px::snap(rect.bottom() - m::PROBABILITY_BOTTOM - g.size().y, ppp),
        );
        painter.galley(pos, g, color32(color::SURFACE_0));
    }
    if let Some(ring) = look.ring {
        px::outline(painter, rect, radius::S, 0.0, ring);
    }
}

fn live_note(track: TrackId, key: u8, velocity: u8) -> Action {
    Action::Transport(TransportAction::LiveNote {
        track: Some(track),
        key,
        velocity,
    })
}

/// One-line galley of `text` in `font`, cut with "…" at `max_width`.
fn fitted(ui: &Ui, text: &str, font: FontId, line_height: f32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(
        text.to_owned(),
        TextFormat {
            font_id: font,
            line_height: Some(line_height),
            color: Color32::PLACEHOLDER,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width: max_width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    ui.painter().layout_job(job)
}

/// The pointer this pass: position, primary pressed now, primary up (released or not held).
#[derive(Debug, Clone, Copy)]
struct Pointer {
    pos: Option<Pos2>,
    pressed: bool,
    up: bool,
    mods: PressMods,
}

impl Pointer {
    /// The pointer of this pass. The modifiers are those of the press event when there is one
    /// (a press and its modifier changes can arrive in one pass), else the held ones.
    fn of(ui: &Ui) -> Pointer {
        ui.input(|i| {
            let mods = i
                .events
                .iter()
                .find_map(|e| match e {
                    Event::PointerButton {
                        button: PointerButton::Primary,
                        pressed: true,
                        modifiers,
                        ..
                    } => Some(*modifiers),
                    _ => None,
                })
                .unwrap_or(i.modifiers);
            Pointer {
                pos: i.pointer.latest_pos(),
                pressed: i.pointer.primary_pressed(),
                up: i.pointer.primary_released() || !i.pointer.primary_down(),
                mods: PressMods {
                    shift: mods.shift,
                    alt: mods.alt,
                },
            }
        })
    }
}

/// Draws the step numbers and the rows, and handles their input.
pub fn show(
    ui: &mut Ui,
    areas: &Areas,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut GridState,
    palette: &TrackPalette,
) {
    let cols = Columns::new(areas.numbers, open.steps());
    numbers(ui, areas.numbers, &cols, open);

    let rows = open.pattern.rows.len();
    let content = rows as f32 * row_pitch();
    let max_scroll = (content - areas.rows.height()).max(0.0);
    if max_scroll > 0.0 && ui.rect_contains_pointer(areas.rows) {
        let dy = ui.input_mut(|i| std::mem::take(&mut i.smooth_scroll_delta.y));
        state.scroll -= dy;
    }
    state.scroll = state.scroll.clamp(0.0, max_scroll);

    if let Some((clip, _)) = state.gesture
        && clip != open.clip
    {
        state.abandon(ctx);
    }

    let pointer = Pointer::of(ui);
    let clip = ui.clip_rect().intersect(areas.rows);
    let mut rows_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(areas.rows)
            .id_salt("komponist_step_rows"),
    );
    rows_ui.set_clip_rect(clip);
    let any_solo = open.pattern.rows.iter().any(|r| r.solo);
    let mut just_pressed = false;
    for r in 0..rows {
        let top = areas.rows.top() + r as f32 * row_pitch() - state.scroll;
        let rect = Rect::from_min_max(
            pos2(areas.rows.left(), top),
            pos2(areas.rows.right(), top + m::ROW_HEIGHT),
        );
        if !rect.intersects(clip) {
            continue;
        }
        let row = Row {
            index: r,
            rect,
            rows_top: areas.rows.top() - state.scroll,
            any_solo,
        };
        just_pressed |= header(&mut rows_ui, &row, open, ctx, state, palette, pointer);
        just_pressed |= cells(
            &mut rows_ui,
            &row,
            &cols,
            open,
            ctx,
            state,
            palette,
            pointer,
        );
        px::hline(
            rows_ui.painter(),
            rect.x_range(),
            rect.bottom(),
            color32(color::SURFACE_0),
        );
    }

    // Esc during a drag: the document goes back to the press (`CancelGesture`, which the app's
    // keymap also sends); stop following the pointer. The press still waits for its release
    // (note off).
    if super::escape_pressed(ui) && !just_pressed {
        let steps = state.gesture.take().is_some();
        let toggles = std::mem::take(&mut state.toggle_gesture);
        if steps || toggles {
            ctx.send(Action::Edit(EditAction::CancelGesture));
        }
    }

    // The gesture follows the pointer along its row.
    if !just_pressed
        && let Some((clip, mut g)) = state.gesture
        && let Some(pos) = pointer.pos
        && let Some(row) = open.pattern.rows.get(g.row())
    {
        let steps = &row.steps[..open.steps()];
        for (i, prop) in g.moved(steps, cols.at(pos.x), pos.y) {
            super::send_step(ctx, clip, g.row(), i, prop);
        }
        state.gesture = Some((clip, g));
    }
    if pointer.up && (state.pressed || state.toggle_gesture) {
        release(state, ctx);
    }
}

/// The release of a press: the gesture's last changes, the end of the undo step, note off.
fn release(state: &mut GridState, ctx: &mut ViewCtx<'_>) {
    if let Some((clip, g)) = state.gesture.take() {
        for (i, prop) in g.released() {
            super::send_step(ctx, clip, g.row(), i, prop);
        }
        ctx.send(Action::Edit(EditAction::EndGesture));
    }
    if state.toggle_gesture {
        ctx.send(Action::Edit(EditAction::EndGesture));
        state.toggle_gesture = false;
    }
    if let Some((track, key)) = state.audition.take() {
        ctx.send(live_note(track, key, 0));
    }
    state.pressed = false;
}

/// A row being drawn.
struct Row {
    index: usize,
    rect: Rect,
    /// Top of row 0 on screen (scrolled).
    rows_top: f32,
    any_solo: bool,
}

/// The step numbers: `type.value.s` (the design's Mono 9 has no token), the running light's in
/// `text.1`, every fourth `text.2`, the rest `text.4`; on the left the header column.
fn numbers(ui: &Ui, rect: Rect, cols: &Columns, open: &Open<'_>) {
    let painter = ui.painter();
    let ppp = ui.pixels_per_point();
    let header = Rect::from_min_size(rect.min, vec2(m::HEADER_WIDTH, rect.height()));
    header_column(ui, header, color32(color::SURFACE_2));
    for i in 0..open.steps() {
        let color = if open.current == Some(i) {
            color::TEXT_1
        } else if i % 4 == 0 {
            color::TEXT_2
        } else {
            color::TEXT_4
        };
        let g = widgets::styled_galley(ui, &(i + 1).to_string(), &typography::VALUE_S);
        let pos = widgets::text_pos(rect, &g, cols.x(i).min, ppp);
        painter.galley(pos, g, color32(color));
    }
    px::hline(
        painter,
        rect.x_range(),
        rect.bottom(),
        color32(color::SURFACE_0),
    );
}

/// The header column part of a row: `fill` with the 1 px `surface.0` edge on the right
/// (`borderRight:'1px solid '+C.f0`, inside the 180).
fn header_column(ui: &Ui, rect: Rect, fill: Color32) {
    let painter = ui.painter();
    px::fill(painter, rect, 0.0, fill);
    px::vline(
        painter,
        rect.right() - line::WIDTH,
        rect.y_range(),
        color32(color::SURFACE_0),
    );
}

/// A row header: paints it, handles the press (sound and select the row, keeping the step
/// column) and M S. Returns `true` if a
/// press started here.
fn header(
    ui: &mut Ui,
    row: &Row,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut GridState,
    palette: &TrackPalette,
    pointer: Pointer,
) -> bool {
    let r = row.index;
    let model = &open.pattern.rows[r];
    let name = &open.names[r];
    let rect = Rect::from_min_size(row.rect.min, vec2(m::HEADER_WIDTH, m::ROW_HEIGHT));
    let selected = open.cursor.is_some_and(|c| c.row == r);
    let id = Id::unique(("komponist_step_row", open.clip, r));
    let response = ui.interact(rect, id, Sense::CLICK);
    let label = names::row(name);
    response.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, &label));

    // Paint.
    let ppp = ui.pixels_per_point();
    let fill = if selected {
        color::ACCENT_SURFACE
    } else {
        color::SURFACE_2
    };
    header_column(ui, rect, color32(fill));
    let [cw, ch] = m::CHIP;
    let chip_x = rect.left() + m::HEADER_PAD_LEFT;
    let chip = Rect::from_min_size(pos2(chip_x, rect.center().y - ch / 2.0), vec2(cw, ch));
    px::fill(ui.painter(), chip, 0.0, color32(palette.base));
    let [tw, th] = m::ROW_TOGGLE;
    let content_right = rect.right() - line::WIDTH - m::HEADER_PAD_RIGHT;
    let solo_rect = Rect::from_min_size(
        pos2(content_right - tw, rect.center().y - th / 2.0),
        vec2(tw, th),
    );
    let mute_rect = solo_rect.translate(vec2(-(tw + m::HEADER_GAP), 0.0));
    let note = widgets::styled_galley(ui, &open.labels[r], &typography::VALUE_S);
    let note_x = mute_rect.left() - m::HEADER_GAP - note.size().x;
    ui.painter().galley(
        widgets::text_pos(rect, &note, note_x, ppp),
        note,
        color32(color::TEXT_3),
    );
    let weight = if selected { wm::SEMIBOLD } else { wm::MEDIUM };
    let name_x = chip.right() + m::HEADER_GAP;
    let style = typography::UI;
    let galley = fitted(
        ui,
        name,
        widgets::font_at(&style, weight),
        style.line_height,
        note_x - m::HEADER_GAP - name_x,
    );
    ui.painter().galley(
        widgets::text_pos(rect, &galley, name_x, ppp),
        galley,
        color32(color::TEXT_1),
    );

    // M S.
    for (kind, on, toggle_rect) in [
        (ToggleKind::Mute, model.mute, mute_rect),
        (ToggleKind::Solo, model.solo, solo_rect),
    ] {
        let implied = kind == ToggleKind::Mute && row.any_solo && !model.solo;
        let toggle = TrackToggle::new(kind, on)
            .implied(implied)
            .track(name)
            .size(vec2(tw, th));
        let out = ui
            .scope_builder(UiBuilder::new().max_rect(toggle_rect), |ui| toggle.show(ui))
            .inner;
        if let Some(event) = out.event {
            toggle_event(kind, event, r, open, ctx, state, pointer, row.rows_top);
        }
    }

    // Press: sound the pad and select the row ("Klick spielt das Pad an und wählt die Zeile").
    let held = pointer.pressed && response.is_pointer_button_down_on();
    let instant = response.clicked() && !state.pressed;
    if !(held || instant) {
        return false;
    }
    if state.pressed {
        release(state, ctx);
    }
    // The step column stays (prototype `onHdr` sets only `sel.row`): the toolbar then shows the
    // same step of the new row, so rows can be compared step by step.
    let step = open
        .cursor
        .and_then(|c| c.step)
        .filter(|&i| i < open.steps());
    ctx.send(Action::Ui(UiAction::Select(SelectionChange::Step {
        clip: open.clip,
        row: r,
        step,
    })));
    ctx.send(live_note(open.track, model.note, m::AUDITION_VELOCITY));
    if instant {
        ctx.send(live_note(open.track, model.note, 0));
        false
    } else {
        state.audition = Some((open.track, model.note));
        state.pressed = true;
        true
    }
}

/// Applies an M or S event of row `r`: one row, "only this row" (Alt-click S), or the rows a
/// drag sweeps over. A press held with the pointer is one gesture until the release.
#[allow(clippy::too_many_arguments)]
fn toggle_event(
    kind: ToggleKind,
    event: ToggleEvent,
    r: usize,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut GridState,
    pointer: Pointer,
    rows_top: f32,
) {
    let prop = |value: bool| match kind {
        ToggleKind::Solo => StepRowProp::Solo(value),
        _ => StepRowProp::Mute(value),
    };
    let current = |row: usize| {
        let model = &open.pattern.rows[row];
        match kind {
            ToggleKind::Solo => model.solo,
            _ => model.mute,
        }
    };
    let set = |ctx: &mut ViewCtx<'_>, row: usize, value: bool| {
        if current(row) != value {
            ctx.send(Action::Edit(EditAction::SetStepRow {
                clip: open.clip,
                row,
                prop: prop(value),
            }));
        }
    };
    match event {
        ToggleEvent::Set { value, exclusive } => {
            let held = !pointer.up;
            if held && !state.toggle_gesture {
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: "Zeile ändern".to_owned(),
                }));
                state.toggle_gesture = true;
            }
            if exclusive {
                if !held {
                    ctx.send(Action::Edit(EditAction::BeginGesture {
                        label: "Zeile ändern".to_owned(),
                    }));
                }
                for row in 0..open.pattern.rows.len() {
                    set(ctx, row, row == r);
                }
                if !held {
                    ctx.send(Action::Edit(EditAction::EndGesture));
                }
            } else {
                set(ctx, r, value);
            }
        }
        ToggleEvent::Sweep { value, from, to } => {
            // Every row the segment crosses, also rows a fast move skipped.
            let (lo, hi) = (from.y.min(to.y), from.y.max(to.y));
            for row in 0..open.pattern.rows.len() {
                let top = rows_top + row as f32 * row_pitch();
                if top + m::ROW_HEIGHT >= lo && top <= hi {
                    set(ctx, row, value);
                }
            }
        }
    }
}

/// The cells of a row: paints them, exposes them to AccessKit and starts gestures. Returns
/// `true` if a press started here.
#[allow(clippy::too_many_arguments)]
fn cells(
    ui: &mut Ui,
    row: &Row,
    cols: &Columns,
    open: &Open<'_>,
    ctx: &mut ViewCtx<'_>,
    state: &mut GridState,
    palette: &TrackPalette,
    pointer: Pointer,
) -> bool {
    let r = row.index;
    let model = &open.pattern.rows[r];
    let name = &open.names[r];
    let selected = open.selected_step(r);
    let y = Rangef::new(
        row.rect.top() + m::CELLS_PAD_Y,
        row.rect.top() + m::ROW_HEIGHT - m::CELLS_PAD_Y,
    );
    let mut started = false;
    for i in 0..open.steps() {
        let step = &model.steps[i];
        let rect = Rect::from_x_y_ranges(cols.x(i), y);
        let id = Id::unique(("komponist_step", open.clip, r, i));
        let response = ui.interact(rect, id, Sense::CLICK | Sense::DRAG);
        let is_selected = selected == Some(i);
        response.widget_info(|| {
            WidgetInfo::selected(
                Role::CheckBox,
                true,
                step.on,
                pattern::step_name(i, name, step),
            )
        });
        if is_selected {
            widgets::accesskit(&response, |n| n.set_selected(true));
        }
        let look = cell_look(step, i, open.current == Some(i), is_selected, palette);
        paint_cell(ui, rect, &look);

        let held = pointer.pressed && response.is_pointer_button_down_on();
        let instant = response.clicked() && !state.pressed;
        if !(held || instant) {
            continue;
        }
        if state.pressed {
            release(state, ctx);
        }
        let mods = if instant && !pointer.pressed {
            // AccessKit or keyboard: a plain click.
            PressMods::default()
        } else {
            pointer.mods
        };
        let y0 = pointer.pos.map_or(rect.center().y, |p| p.y);
        let press = pattern::press(r, i, step, mods, y0);
        if open.cursor.is_none_or(|c| c.row != r || c.step != Some(i)) {
            ctx.send(Action::Ui(UiAction::Select(SelectionChange::Step {
                clip: open.clip,
                row: r,
                step: Some(i),
            })));
        }
        if let Some(g) = press.gesture {
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: g.label().to_owned(),
            }));
            state.gesture = Some((open.clip, g));
        }
        if let Some(prop) = press.edit {
            super::send_step(ctx, open.clip, r, i, prop);
        }
        if press.audition {
            ctx.send(live_note(open.track, model.note, step.velocity));
            state.audition = Some((open.track, model.note));
        }
        state.pressed = true;
        if instant {
            release(state, ctx);
        } else {
            started = true;
        }
    }
    started
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_split_the_width_like_flex() {
        // 16 cells of 70 with 4 px gaps, as in the S03 reference (1376 wide dock).
        let rect = Rect::from_min_max(
            pos2(0.0, 0.0),
            pos2(180.0 + 16.0 + 16.0 * 70.0 + 60.0, 18.0),
        );
        let c = Columns::new(rect, 16);
        assert_eq!(c.width, 70.0);
        assert_eq!(c.x(0).min, 188.0);
        assert_eq!(c.x(1).min, 262.0);
        assert_eq!(c.at(188.0), Some(0));
        assert_eq!(c.at(259.0), Some(0), "the left half of a gap");
        assert_eq!(c.at(261.0), Some(1));
        assert_eq!(c.at(150.0), None);
        assert_eq!(c.at(rect.right()), None);
    }

    #[test]
    fn cell_looks_follow_the_screen() {
        let p = TrackPalette::new(45);
        let off = Step::default();
        assert_eq!(
            cell_look(&off, 0, false, false, &p).fill,
            color32(color::SURFACE_2)
        );
        assert_eq!(
            cell_look(&off, 5, false, false, &p).fill,
            color32(color::CANVAS_STEP_GROUP_ALT)
        );
        assert_eq!(
            cell_look(&off, 5, true, false, &p).fill,
            color32(color::SURFACE_4)
        );
        let on = Step {
            on: true,
            velocity: 90,
            ..Step::default()
        };
        let l = cell_look(&on, 0, false, false, &p);
        assert_eq!((l.fill, l.ring), (color32(p.note(90)), None));
        // Running light: the play colour with a text.1 ring.
        let l = cell_look(&on, 4, true, false, &p);
        assert_eq!(l.fill, color32(p.play));
        assert_eq!(l.ring, Some(color32(color::TEXT_1)));
        // The cursor ring wins.
        assert_eq!(
            cell_look(&on, 4, true, true, &p).ring,
            Some(color32(color::ACCENT))
        );
        // Ratchet 3 on the off colour, probability shown.
        let r = Step {
            ratchet: 3,
            probability: 50,
            ..on
        };
        let l = cell_look(&r, 15, false, false, &p);
        assert_eq!(l.fill, color32(color::CANVAS_STEP_GROUP_ALT));
        assert_eq!(l.parts, Some((3, color32(p.note(90)))));
        assert_eq!(l.probability, Some(50));
        // An off step shows neither.
        let l = cell_look(&Step { on: false, ..r }, 15, false, false, &p);
        assert_eq!((l.parts, l.probability), (None, None));
    }
}
