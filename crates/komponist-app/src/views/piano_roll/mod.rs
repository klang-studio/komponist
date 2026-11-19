//! The piano roll (WP-41): the dock's editor tab for a MIDI clip (`Screens.dc.html`
//! `pianoRoll()` and its markup, `ks-view.js` `prView`, `Designsystem.dc.html` `ix.note` and
//! `notes`, `Prototyp.dc.html` `prDown`, `prDbl`, `prVel`, `quantize`, `moveNotes`).
//!
//! From top to bottom inside the dock body:
//!
//! - toolbar ([`toolbar`]): tools Zeiger / Stift / Radierer, Raster, Snap, Quantisieren, the
//!   selection info and the fields Vel, Länge, Note, zoom − +;
//! - the editor ruler (18 high; bar numbers, the clip range in the track colour);
//! - keyboard (`size.keyboard` 52) and note grid (rows of `size.key`, 4 … 24, black-key rows
//!   darker, a `line.2` line under every C, bar / beat / grid lines, the veil after the clip end,
//!   notes, box, playhead);
//! - the velocity lane (`size.velocity-lane` 64).
//!
//! The canvas ([`canvas`]) handles pointer gestures and painting. The keys of `ix.note` (↑ ↓,
//! Shift ↑ ↓, ← →, Q, Strg A, Entf, 1 / 2 / 3, Alt + / Alt −) are app shortcuts
//! ([`crate::keymap`]); their actions act on the selection and the open clip. Playing notes
//! light up from the playhead (no engine feedback needed): a note under the clip-local playhead
//! while the transport runs, and its key. An empty clip opens with the pencil (the dispatcher's
//! `OpenClip`) and shows the grid with a hint. Step clips are the step sequencer's (WP-42).
//!
//! Level of detail follows the design system's note zoom levels through
//! [`komponist_ui::timeline::NoteBatch`] (names from 12 px note height, rings down to key height
//! 6, an accent fill below); key names are left out when a key is lower than their text.
//! Performance: notes are culled to the visible time window by binary search
//! ([`edit::time_window`]) and the visible keys, and painted as one mesh.

mod canvas;
pub mod edit;
pub mod geometry;
pub mod metrics;
mod toolbar;

use std::sync::Arc;

use egui::Ui;
use komponist_engine::TransportState;
use komponist_model::{ClipContent, ClipId, NoteId, TrackId};
use komponist_tokens::TrackPalette;

use self::canvas::{Frame, Gesture};
use self::geometry::Geometry;
use crate::views::{View, ViewCtx, clip_sections};

/// The top of the velocity lane's scale.
pub const VELOCITY_MAX: &str = "127";

/// The piano roll view. Its document changes go out as actions; its view state lives in
/// [`crate::ui_state::PianoRollView`]; here are only the gesture in progress and caches.
#[derive(Default)]
pub struct PianoRoll {
    /// The clip shown last; another one is scrolled into view when it opens.
    shown_clip: Option<ClipId>,
    /// The pointer gesture in progress.
    gesture: Option<Gesture>,
    /// A drag on a toolbar field in progress.
    field_drag: Option<toolbar::FieldDrag>,
    /// The key sounding from a click (keyboard, note press), on its track.
    audition: Option<(TrackId, u8)>,
    /// Colours of the open clip's track.
    palette: Option<TrackPalette>,
    /// Wheel movement not yet turned into whole keys.
    wheel_rows: f32,
    /// Notes of the live take in the last frame (a new one may scroll the keys).
    live_notes: usize,
    automation: crate::views::automation::ClipEditor,
}

/// Zooms horizontally by `factor` so that the absolute tick `at` stays at screen x `x`.
fn zoom_around(
    view: &mut crate::ui_state::PianoRollView,
    geo: &Geometry,
    meter: komponist_model::Meter,
    factor: f32,
    x: f32,
    at: f64,
) {
    use crate::metrics::arrangement::{PX_PER_BAR_MAX, PX_PER_BAR_MIN};
    view.px_per_bar = (view.px_per_bar * factor).clamp(PX_PER_BAR_MIN, PX_PER_BAR_MAX * 8.0);
    let ppt = f64::from(view.px_per_bar) / meter.ticks_per_bar() as f64;
    let left = at - f64::from(x - geo.grid.left()) / ppt;
    view.scroll_tick = (left - geo.clip_start.0 as f64).max(0.0);
}

impl PianoRoll {
    /// Z and + / −, which the dispatcher turns into requests (it does not know the grid): Z fits
    /// the clip's length across the grid and its notes into the visible keys; + / − zoom by
    /// 1.25 per step around the mouse pointer when it is over the grid, else around the playhead
    /// when that is visible, else around the left edge.
    fn apply_requests(
        &mut self,
        ui: &Ui,
        ctx: &mut ViewCtx<'_>,
        geo: &Geometry,
        clip: &komponist_model::Clip,
        notes: &[komponist_model::Note],
    ) {
        use crate::metrics::arrangement::{PX_PER_BAR_MAX, PX_PER_BAR_MIN, ZOOM_STEP};
        let meter = ctx.project.meter;
        let requests = ctx.requests;
        if requests.fit_piano_roll {
            let bars = (clip.length.0 as f64 / meter.ticks_per_bar() as f64).max(1.0 / 16.0);
            let pr = &mut ctx.ui.piano_roll;
            pr.px_per_bar = (f64::from(geo.grid.width()) / bars) as f32;
            pr.px_per_bar = pr
                .px_per_bar
                .clamp(PX_PER_BAR_MIN / 4.0, PX_PER_BAR_MAX * 8.0);
            pr.scroll_tick = 0.0;
            pr.top_key = edit::fit_top_key(notes, geo.full_rows());
        }
        if requests.piano_roll_zoom != 0 {
            let factor = ZOOM_STEP.powi(requests.piano_roll_zoom);
            let pointer = ui
                .input(|i| i.pointer.latest_pos())
                .filter(|p| geo.grid.contains(*p));
            let playhead = ctx.engine.playhead();
            let head_x = geo.axis.x(playhead);
            let (x, at) = match pointer {
                Some(p) => (p.x, geo.tick_at(p.x)),
                None if geo.grid.x_range().contains(head_x) => (head_x, playhead.0 as f64),
                None => (geo.grid.left(), geo.tick_at(geo.grid.left())),
            };
            zoom_around(&mut ctx.ui.piano_roll, geo, meter, factor, x, at);
        }
    }

    /// A piano roll with nothing open yet.
    pub fn new() -> PianoRoll {
        PianoRoll::default()
    }

    /// Wheel: keys up / down; Shift wheel (horizontal): time; Ctrl wheel: zoom around the
    /// pointer. Over the ruler, keyboard, grid and lane.
    fn scroll_and_zoom(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, geo: &Geometry) {
        let area = egui::Rect::from_min_max(geo.ruler.min.min(geo.keys.min), ui.max_rect().max);
        if !ui.rect_contains_pointer(area) {
            return;
        }
        let (scroll, zoom, pointer) = ui.input(|i| {
            (
                i.smooth_scroll_delta,
                i.zoom_delta(),
                i.pointer.latest_pos(),
            )
        });
        let pr = &mut ctx.ui.piano_roll;
        if zoom != 1.0
            && let Some(p) = pointer
        {
            let x = p.x.max(geo.grid.left());
            zoom_around(pr, geo, ctx.project.meter, zoom, x, geo.tick_at(x));
        }
        if scroll.y != 0.0 {
            self.wheel_rows += scroll.y / geo.key_height;
            let whole = self.wheel_rows.trunc();
            self.wheel_rows -= whole;
            if whole != 0.0 {
                let top = i32::from(pr.top_key) + whole as i32;
                pr.top_key = edit::clamp_top_key(top, geo.full_rows());
            }
        }
        if scroll.x != 0.0 {
            let ticks = f64::from(scroll.x) / f64::from(geo.axis.px_per_tick);
            pr.scroll_tick = (pr.scroll_tick - ticks).max(0.0);
        }
        if scroll != egui::Vec2::ZERO {
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
    }
}

impl View for PianoRoll {
    fn reset_document(&mut self) {
        self.automation = Default::default();
    }

    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        let project = Arc::clone(ctx.project);
        // While recording, the take being recorded instead of the open clip ("Piano Roll · live",
        // S02): read-only, red clip range, following the playhead.
        let live = ctx
            .recording
            .and_then(|r| r.first())
            .and_then(|(t, c)| Some((project.track(t)?, Arc::clone(c))));
        let (track, clip) = match &live {
            Some((track, clip)) => (*track, clip),
            None => {
                let Some(found) = ctx.ui.editor.clip.and_then(|id| project.find_clip(id)) else {
                    return;
                };
                found
            }
        };
        let clip_id = clip.id;
        let ClipContent::Midi(midi) = &clip.content else {
            return;
        };
        let notes = midi.active().notes.as_slice();
        let meter = project.meter;
        let body = ui.max_rect();

        let lane_budget = Geometry::lane_height_limit(body, ctx.ui.piano_roll.key_height);
        let velocity_min = metrics::VELOCITY_MIN_HEIGHT.min(lane_budget);
        let automation_min = if live.is_none() {
            self.automation.minimum_height(clip)
        } else {
            0.0
        };
        let velocity_max = (lane_budget - automation_min).max(velocity_min);
        let geometry = |view: crate::ui_state::PianoRollView| {
            let view = crate::ui_state::PianoRollView {
                velocity_height: view.velocity_height.min(velocity_max),
                ..view
            };
            Geometry::new(body, &view, clip.start, meter)
        };

        let geo = geometry(ctx.ui.piano_roll);
        let available = geo.grid.height() - geo.key_height;
        let automation_height = if live.is_none() {
            self.automation.height(clip, available)
        } else {
            0.0
        };
        let geo = geo.reserve_automation(automation_height);
        if self.shown_clip != Some(clip_id) {
            // A clip opens: its notes come into view (`openClip`), from its start.
            self.shown_clip = Some(clip_id);
            self.gesture = None;
            self.field_drag = None;
            self.live_notes = notes.len();
            let pr = &mut ctx.ui.piano_roll;
            pr.top_key = edit::fit_top_key(notes, geo.full_rows());
            pr.scroll_tick = 0.0;
        }
        let pr = &mut ctx.ui.piano_roll;
        pr.top_key = edit::clamp_top_key(i32::from(pr.top_key), geo.full_rows());
        pr.scroll_tick = pr.scroll_tick.max(0.0);
        let geo = geometry(ctx.ui.piano_roll).reserve_automation(automation_height);
        if live.is_none() {
            self.apply_requests(ui, ctx, &geo, clip, notes);
        }
        let geo = geometry(ctx.ui.piano_roll).reserve_automation(automation_height);
        self.scroll_and_zoom(ui, ctx, &geo);
        let geo = geometry(ctx.ui.piano_roll).reserve_automation(automation_height);
        if live.is_some() {
            // The head left the view (it ran on, or the loop wrapped): show its bar at the left.
            let (t0, t1) = geo.visible_local_ticks();
            let head = ctx.engine.playhead() - clip.start;
            if head < t0 || head >= t1 {
                let bar = meter.ticks_per_bar();
                ctx.ui.piano_roll.scroll_tick = (head.0.max(0) / bar * bar) as f64;
            }
            // A new note outside the visible keys scrolls them just enough to show it.
            if notes.len() != self.live_notes {
                self.live_notes = notes.len();
                let (top, bottom) = (ctx.ui.piano_roll.top_key, geo.bottom_key());
                if let Some(n) = notes.iter().max_by_key(|n| (n.start, n.id))
                    && (n.pitch > top || n.pitch < bottom)
                {
                    let rows = geo.full_rows() as i32;
                    let pitch = i32::from(n.pitch);
                    let new_top = if n.pitch > top {
                        pitch + metrics::FIT_ABOVE
                    } else {
                        pitch + rows - 1 - metrics::FIT_BELOW
                    };
                    ctx.ui.piano_roll.top_key = edit::clamp_top_key(new_top, geo.full_rows());
                }
            }
        }
        let geo = geometry(ctx.ui.piano_roll).reserve_automation(automation_height);

        if self.palette.as_ref().is_none_or(|p| p.hue != track.hue.0) {
            self.palette = Some(TrackPalette::new(track.hue.0));
        }
        let palette = self
            .palette
            .clone()
            .unwrap_or_else(|| TrackPalette::new(track.hue.0));

        // The live take's notes are not selectable.
        let selection = if live.is_some() {
            Default::default()
        } else {
            ctx.ui.selection.notes.clone()
        };
        let selected: Vec<NoteId> = if selection.is_empty() {
            Vec::new()
        } else {
            notes
                .iter()
                .filter(|n| selection.contains(&n.id))
                .map(|n| n.id)
                .collect()
        };
        let first = notes.iter().find(|n| selection.contains(&n.id));
        let velocity = if selected.is_empty() {
            0
        } else {
            let sum: u32 = notes
                .iter()
                .filter(|n| selection.contains(&n.id))
                .map(|n| u32::from(n.velocity))
                .sum();
            (f64::from(sum) / selected.len() as f64).round() as u8
        };
        let view = ctx.ui.piano_roll;
        let grid = view.grid.ticks(meter);
        let info = toolbar::Info {
            clip: clip_id,
            selected,
            total: notes.len(),
            velocity,
            length: first.map_or(grid, |n| n.length),
            pitch: first.map_or(60, |n| n.pitch),
            grid,
            live: live.is_some(),
        };
        if let Some(button) = toolbar::show(
            ui,
            geo.toolbar,
            ctx,
            &info,
            &mut self.field_drag,
            self.automation.shown(clip),
        ) {
            self.automation.toolbar(ui, ctx, clip, track, &button);
        }

        let playhead = ctx.engine.playhead();
        let running = matches!(
            ctx.engine.transport(),
            TransportState::Playing | TransportState::Recording
        );
        let (t0, t1) = geo.visible_local_ticks();
        let held = ctx.held_notes.clone();
        let frame = Frame {
            held: &held,
            geo,
            clip,
            track: track.id,
            palette: &palette,
            notes,
            window: edit::time_window(notes, t0, t1),
            meter,
            grid_kind: view.grid,
            grid,
            snap: view.snap,
            tool: view.tool,
            selection,
            playhead,
            playing: running
                .then(|| edit::local_playhead(clip, playhead))
                .flatten(),
            muted: clip.mute || track.channel.mute,
            live: live.is_some(),
        };
        self.canvas(ui, ctx, &frame);
        if live.is_none() {
            let geo = &frame.geo;
            let lanes = egui::Rect::from_min_max(
                egui::pos2(geo.keys.left(), geo.grid.bottom()),
                egui::pos2(geo.grid.right(), geo.grid.bottom() + automation_height),
            );
            let axis = komponist_ui::timeline::TimeAxis {
                x0: geo.axis.x(clip.start),
                ..geo.axis
            };
            self.automation.show(
                ui,
                ctx,
                lanes,
                geo.grid.left(),
                clip,
                track,
                axis,
                view.grid,
                view.snap,
                &palette,
            );
            if let (Some(label), Some(lane)) = (geo.lane_label, geo.lane)
                && lane.is_positive()
                && let Some(height) = clip_sections::resize_from_top(
                    ui,
                    label.union(lane),
                    ui.scope_id().with("velocity-height"),
                    &ctx.lang
                        .dock_section_height(ctx.lang.piano_roll_velocity_lane()),
                    lane.height(),
                    velocity_min..=velocity_max,
                )
            {
                ctx.ui.piano_roll.velocity_height = height;
            }
        }
        if self.gesture.is_some() || running {
            ui.ctx().request_repaint();
        }
    }
}
