//! The step sequencer in the dock's editor tab (WP-42; S03, `Screens.dc.html` `stepSeq()`,
//! `PADS`, the dock markup for `isStep`; `ks-view.js` `stepView`; `Prototyp.dc.html` `stepDown`,
//! `stepEnter`, `stepLane`).
//!
//! From top to bottom:
//!
//! - toolbar ([`toolbar`]): "Länge", "Auflösung", "Swing" │ "STEP n · Zeile", "Vel", "Wahr.",
//!   "Ratch." of the selected step … "Als MIDI-Clip";
//! - step numbers, the current step brighter while playing ([`grid`]);
//! - one row per pad: header (colour chip, name, note, M S; a press sounds the pad and selects
//!   the row) and up to 32 steps whose brightness follows the velocity ([`grid`], gestures in
//!   [`pattern`]);
//! - the lane of the selected row with the tabs Vel / Wahr. / Ratch. ([`lane`]).
//!
//! The running light: while the transport plays the open clip, the column of the current step
//! brightens (`surface.4`) and set steps in it glow (`TrackPalette::play`) with a `text.1`
//! contour. Row names come from the instrument's note names when it reports them
//! (`DeviceHost::note_names`), else from the model; the small label right of the name is the
//! sampler pad ("P1", S03) on a sampler track, else the note name ([`crate::step_rows`]).
//!
//! The view never changes the document: steps, rows and pattern properties change through
//! [`EditAction::SetStep`](crate::actions::EditAction::SetStep), `SetStepRow`,
//! `SetStepClipProp` and `StepClipToMidi`; a drag is one gesture (one undo step), Esc during
//! a drag cancels it. Rows sound through `TransportAction::LiveNote` on the clip's track.
//!
//! AccessKit: every visible step is a check box named like "Step 5, Kick, an, Velocity 100"
//! (selected = the step cursor); row headers are buttons "Zeile Kick"; the toolbar fields are
//! spin buttons; lane bars are sliders ("Vel Step 5").

pub mod grid;
pub mod lane;
pub mod metrics;
pub mod pattern;
pub mod toolbar;

use std::sync::Arc;

use egui::{Event, Key, Rect, Ui, pos2};
use komponist_model::{ClipContent, ClipId, Project, StepClip, TrackId};
use komponist_tokens::{TrackPalette, line};

use crate::actions::{Action, EditAction, UiAction};
use crate::step_rows;
use crate::ui_state::StepCursor;
use crate::views::{View, ViewCtx, centered_text};
use metrics as m;

/// Unit of swing and probability: the same in every language (`docs/I18N.md` §5).
pub const PERCENT: &str = "%";

/// The open step clip as the parts of the view need it.
pub struct Open<'p> {
    /// The clip.
    pub clip: ClipId,
    /// Its track (live notes).
    pub track: TrackId,
    /// The pattern.
    pub pattern: &'p StepClip,
    /// Row names shown: the instrument's note names where it has one, else the model's.
    pub names: Vec<String>,
    /// The label right of each row's name: sampler pad ("P1") or note name ("C2").
    pub labels: Vec<String>,
    /// The step cursor, if it is in this clip.
    pub cursor: Option<StepCursor>,
    /// The row the lane shows: the cursor's, else the first.
    pub lane_row: usize,
    /// The pattern index of the running light, while playing.
    pub current: Option<usize>,
}

impl<'p> Open<'p> {
    /// The open step clip of `ctx` in `project`, if the editor shows one.
    pub fn of(project: &'p Project, ctx: &ViewCtx<'_>) -> Option<Open<'p>> {
        let id = ctx.ui.editor.clip?;
        let (track, clip) = project.find_clip(id)?;
        let ClipContent::Step(pattern) = &clip.content else {
            return None;
        };
        let host = ctx.engine.host();
        let instrument = step_rows::instrument(track);
        let note_names = step_rows::device_note_names(instrument, host);
        let names = step_rows::row_names(pattern, &note_names);
        let labels = pattern
            .rows
            .iter()
            .map(|r| step_rows::row_label(instrument.map(|d| &d.plugin), r.note))
            .collect();
        let cursor = ctx
            .ui
            .selection
            .step
            .filter(|c| c.clip == id && c.row < pattern.rows.len());
        let lane_row = cursor.map_or(0, |c| c.row);
        let telemetry = ctx.engine.telemetry();
        let current = pattern::current_step(
            pattern,
            clip.start,
            clip.length,
            ctx.engine.playhead(),
            telemetry.transport(),
        );
        Some(Open {
            clip: id,
            track: track.id,
            pattern,
            names,
            labels,
            cursor,
            lane_row,
            current,
        })
    }

    /// Number of steps shown (the pattern length, 1 ..= 32).
    pub fn steps(&self) -> usize {
        usize::from(self.pattern.length_steps).clamp(1, komponist_model::project::MAX_STEPS)
    }

    /// The selected step of row `row`, if the cursor is on one.
    pub fn selected_step(&self, row: usize) -> Option<usize> {
        self.cursor.filter(|c| c.row == row).and_then(|c| c.step)
    }
}

/// The vertical split of the view: toolbar, step numbers, rows, lane (all with their lines).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Areas {
    /// Toolbar (28, its line below).
    pub toolbar: Rect,
    /// Step numbers (18, line below).
    pub numbers: Rect,
    /// Visible part of the rows (each row 26 + line).
    pub rows: Rect,
    /// The lane: the rest, at least [`metrics::LANE_MIN_HEIGHT`].
    pub lane: Rect,
}

impl Areas {
    /// The split of `rect` for `rows` rows.
    pub fn of(rect: Rect, rows: usize) -> Areas {
        let lw = line::WIDTH;
        let toolbar =
            Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + m::TOOLBAR_HEIGHT));
        let numbers_top = toolbar.bottom() + lw;
        let numbers = Rect::from_min_max(
            pos2(rect.left(), numbers_top),
            pos2(rect.right(), numbers_top + m::NUMBERS_HEIGHT),
        );
        let rows_top = numbers.bottom() + lw;
        let content = rows as f32 * row_pitch();
        let room = (rect.bottom() - rows_top - m::LANE_MIN_HEIGHT).max(0.0);
        let rows_rect = Rect::from_min_max(
            pos2(rect.left(), rows_top),
            pos2(rect.right(), rows_top + content.min(room)),
        );
        let lane = Rect::from_min_max(
            pos2(rect.left(), rows_rect.bottom()),
            pos2(rect.right(), rect.bottom().max(rows_rect.bottom())),
        );
        Areas {
            toolbar,
            numbers,
            rows: rows_rect,
            lane,
        }
    }
}

/// Whether Esc went down in this pass. The app's keymap consumes Esc (`ClearSelection`, or
/// `CancelGesture` during a drag) before the views run, so this looks at the raw events.
pub fn escape_pressed(ui: &Ui) -> bool {
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

/// Height of a row with its line.
pub fn row_pitch() -> f32 {
    m::ROW_HEIGHT + line::WIDTH
}

/// The step sequencer view.
#[derive(Default)]
pub struct StepSequencer {
    palette: Option<TrackPalette>,
    toolbar: toolbar::ToolbarState,
    grid: grid::GridState,
    lane: lane::LaneState,
    automation: crate::views::automation::ClipEditor,
}

impl StepSequencer {
    /// A step sequencer without gestures, scrolled to the top.
    pub fn new() -> StepSequencer {
        StepSequencer::default()
    }
}

impl View for StepSequencer {
    fn reset_document(&mut self) {
        self.automation = Default::default();
    }

    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        let rect = ui.max_rect();
        let project: Arc<Project> = Arc::clone(ctx.project);
        let Some(open) = Open::of(&project, ctx) else {
            self.grid.abandon(ctx);
            centered_text(ui, rect, ctx.lang.step_seq_no_clip(), None);
            return;
        };
        let Some((track, clip)) = project.find_clip(open.clip) else {
            return;
        };
        let hue = track.hue.0;
        if self.palette.as_ref().is_none_or(|p| p.hue != hue) {
            self.palette = None;
        }
        let palette = self.palette.get_or_insert_with(|| TrackPalette::new(hue));
        let base = Areas::of(rect, open.pattern.rows.len());
        let available = rect.bottom() - base.rows.top() - row_pitch() - m::LANE_MIN_HEIGHT;
        let automation_height = self.automation.height(clip, available);
        let areas = Areas::of(
            Rect::from_min_max(
                rect.min,
                pos2(rect.right(), rect.bottom() - automation_height),
            ),
            open.pattern.rows.len(),
        );
        let button = toolbar::show(
            ui,
            areas.toolbar,
            &open,
            ctx,
            &mut self.toolbar,
            self.automation.shown(clip),
        );
        self.automation.toolbar(ui, ctx, clip, track, &button);
        grid::show(ui, &areas, &open, ctx, &mut self.grid, palette);
        lane::show(ui, areas.lane, &open, ctx, &mut self.lane, palette);
        let columns = grid::Columns::new(areas.rows, open.steps());
        let resolution = open.pattern.resolution;
        let axis = komponist_ui::timeline::TimeAxis {
            x0: columns.left,
            px_per_tick: (columns.width + m::CELL_GAP) / resolution.ticks().0 as f32,
        };
        let grid = match resolution {
            komponist_model::StepResolution::Eighth => komponist_model::Grid::Eighth,
            komponist_model::StepResolution::Sixteenth => komponist_model::Grid::Sixteenth,
            komponist_model::StepResolution::ThirtySecond => komponist_model::Grid::ThirtySecond,
            komponist_model::StepResolution::EighthTriplet => komponist_model::Grid::EighthTriplet,
            komponist_model::StepResolution::SixteenthTriplet => {
                komponist_model::Grid::SixteenthTriplet
            }
        };
        let lanes = Rect::from_min_max(pos2(rect.left(), areas.lane.bottom()), rect.max);
        self.automation.show(
            ui,
            ctx,
            lanes,
            columns.left,
            clip,
            track,
            axis,
            grid,
            true,
            palette,
        );
    }
}

/// Sends `SetStep` and announces a velocity, probability or ratchet change in the status bar
/// (prototype: "Velocity 52", "Ratchet ×3").
pub(super) fn send_step(
    ctx: &mut ViewCtx<'_>,
    clip: komponist_model::ClipId,
    row: usize,
    step: usize,
    prop: komponist_model::edits::StepProp,
) {
    ctx.send(Action::Edit(EditAction::SetStep {
        clip,
        row,
        step,
        prop,
    }));
    if let Some(text) = pattern::status_text(ctx.lang, prop) {
        ctx.send(Action::Ui(UiAction::ShowStatus {
            text,
            kind: crate::ui_state::StatusKind::Info,
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_take_what_the_lane_leaves() {
        // The dock body at the default height: 320 − 28 tab strip.
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(1000.0, 292.0));
        let a = Areas::of(rect, 8);
        assert_eq!(a.toolbar.height(), 28.0);
        assert_eq!(a.numbers.top(), 29.0);
        assert_eq!(a.rows.top(), 48.0);
        assert_eq!(a.rows.height(), 8.0 * 27.0);
        assert_eq!(a.lane.height(), 292.0 - 48.0 - 216.0);
        // 16 rows do not fit: the lane keeps its minimum, the rows scroll.
        let a = Areas::of(rect, 16);
        assert_eq!(a.lane.height(), m::LANE_MIN_HEIGHT);
        assert_eq!(a.rows.bottom(), a.lane.top());
    }
}
