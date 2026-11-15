//! The canvas part of the piano roll: ruler, keyboard, note grid and velocity lane. One egui
//! area each (not one widget per note, ARCHITECTURE §9.2): the view hit-tests itself
//! ([`edit::hit_note`]), keeps the pointer gesture in [`Gesture`] and paints with the
//! `komponist_ui::timeline` painters ([`NoteBatch`] for the notes).
//!
//! | Where | Input | Result (`ix.note`, prototype) |
//! |---|---|---|
//! | grid, pencil | click / drag | note one grid step long / length pulled in grid steps |
//! | grid, pointer | double-click | note set (empty spot) or deleted (on a note) |
//! | grid, pointer | press on empty spot, drag | box selection (Shift adds) |
//! | note | click, Shift-click | select, add / remove |
//! | note | drag | time and pitch (Shift: free, Alt: copy, original dashed) |
//! | note edge | drag | length of all selected notes, length in the tooltip |
//! | note | Ctrl-drag ↕ | velocity of the selected notes, value in the tooltip |
//! | grid, eraser | click / drag | deletes the notes under the pointer |
//! | keyboard | press / slide | plays the key (`LiveNote`) |
//! | velocity lane | press / drag | sets the velocity of the nearest note start |
//! | ruler | press / drag | playhead (to the beat) |
//! | anywhere | wheel / Shift wheel / Ctrl wheel | keys / time / zoom around the pointer |
//!
//! A drag is one undo step: the first change sends `Edit(BeginGesture)`, then the total delta
//! since the press, the release `Edit(EndGesture)` (or `CancelGesture` when it ends where it
//! started, or on Esc).

use std::collections::BTreeSet;
use std::ops::Range;

use egui::accesskit::Role;
use egui::{
    Color32, CursorIcon, Event, Id, Key, Mesh, Modifiers, PointerButton, Pos2, Rect, Response,
    Sense, Shape, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_model::edits::{NewNote, VelocityChange};
use komponist_model::{Clip, Grid, Meter, Note, NoteId, Tick, TrackId};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, typography};
use komponist_ui::timeline::{
    GridSpec, NoteBatch, NoteState, RulerKind, RulerModel, grid_lines, note_access, paint_grid,
    paint_playhead, paint_ruler, ruler_access,
};
use komponist_ui::tooltip::{self, Placement};
use komponist_ui::{fonts, px};

use super::edit::{self, Snap};
use super::geometry::Geometry;
use super::{PianoRoll, metrics as m, names};
use crate::actions::{Action, EditAction, SelectMode, SelectionChange, TransportAction, UiAction};
use crate::ui_state::Tool;
use crate::views::ViewCtx;

/// What one frame of the canvas works with.
pub struct Frame<'a> {
    /// Rects and mappings.
    pub geo: Geometry,
    /// The open clip.
    pub clip: &'a Clip,
    /// Its track (live notes go there).
    pub track: TrackId,
    /// Colours of the track.
    pub palette: &'a TrackPalette,
    /// Notes of the active take, sorted by start.
    pub notes: &'a [Note],
    /// Indices of the notes that may be visible in time.
    pub window: Range<usize>,
    /// Project meter.
    pub meter: Meter,
    /// The grid ("Raster").
    pub grid_kind: Grid,
    /// Its spacing in ticks.
    pub grid: Tick,
    /// Snap on.
    pub snap: bool,
    /// Active tool.
    pub tool: Tool,
    /// Selected notes.
    pub selection: BTreeSet<NoteId>,
    /// The playhead, absolute.
    pub playhead: Tick,
    /// The clip-local playhead while the transport runs and it is inside the clip.
    pub playing: Option<Tick>,
    /// Keys held live (computer keyboard): lit like the playing notes.
    pub held: &'a [u8],
    /// The clip or its track is muted: notes are contours.
    pub muted: bool,
    /// The take being recorded (WP-51): read-only, the clip range in `state.record`, no hint.
    pub live: bool,
}

/// The pointer gesture in progress.
#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    /// Notes dragged in time and pitch (or copied with Alt).
    Move(MoveDrag),
    /// The edge of notes dragged.
    Resize(ResizeDrag),
    /// Ctrl-drag ↕ on a note.
    Velocity(VelocityDrag),
    /// A new note drawn with the pencil (sent on release).
    Pencil(PencilDrag),
    /// The eraser down: notes under the pointer are deleted.
    Erase {
        /// Notes deleted in this gesture.
        erased: BTreeSet<NoteId>,
        /// `BeginGesture` sent.
        begun: bool,
    },
    /// Box selection.
    Box(BoxDrag),
    /// Velocity lane.
    Lane {
        /// `BeginGesture` sent.
        begun: bool,
        /// Notes and value set last.
        last: Option<(Vec<NoteId>, u8)>,
    },
    /// A key of the keyboard held.
    Key,
    /// The ruler: the playhead follows.
    Ruler {
        /// Tick located last.
        last: Option<Tick>,
    },
    /// A press that needs nothing more (Shift-click on a selected note, a press outside the
    /// keys); waits for the release, so its click is not taken as a new press.
    Held,
    /// Esc ended the gesture; waits for the release.
    Cancelled,
}

/// A move or copy drag.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveDrag {
    notes: Vec<NoteId>,
    /// Start, length and key of each dragged note at the press (the dashed originals).
    originals: Vec<(Tick, Tick, u8)>,
    anchor: Pos2,
    copy: bool,
    pitch: u8,
    last: (Tick, i32),
    begun: bool,
}

/// An edge drag.
#[derive(Debug, Clone, PartialEq)]
pub struct ResizeDrag {
    notes: Vec<NoteId>,
    /// Start and length of each note at the press.
    originals: Vec<(Tick, Tick)>,
    /// Length of the grabbed note at the press.
    length: Tick,
    anchor_x: f32,
    last: Tick,
    begun: bool,
    /// Rect of the grabbed note at the press (tooltip anchor).
    rect: Rect,
}

/// A Ctrl-drag ↕ on a note.
#[derive(Debug, Clone, PartialEq)]
pub struct VelocityDrag {
    notes: Vec<NoteId>,
    anchor_y: f32,
    /// Velocity of the grabbed note at the press.
    velocity: u8,
    last: i16,
    begun: bool,
    rect: Rect,
}

/// A note being drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PencilDrag {
    start: Tick,
    pitch: u8,
    length: Tick,
}

/// A box selection.
#[derive(Debug, Clone, PartialEq)]
pub struct BoxDrag {
    origin: Pos2,
    current: Pos2,
    base: BTreeSet<NoteId>,
    last: Vec<NoteId>,
}

/// The canvas areas that take the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    Grid,
    Keys,
    Lane,
    Ruler,
}

/// Pointer state of one frame.
#[derive(Debug, Clone, Copy)]
struct Pointer {
    primary_down: bool,
    latest: Option<Pos2>,
    origin: Option<Pos2>,
    /// Modifiers now (Shift turns snap off while dragging).
    now: Modifiers,
    /// Modifiers of this frame's primary press, else `now`.
    press: Modifiers,
    escape: bool,
}

impl Pointer {
    fn read(ui: &Ui) -> Pointer {
        ui.input(|i| {
            let press = i
                .events
                .iter()
                .rev()
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
                primary_down: i.pointer.primary_down(),
                latest: i.pointer.latest_pos(),
                origin: i.pointer.press_origin(),
                now: i.modifiers,
                press,
                // The app's keymap consumes Esc (`ClearSelection`) before the views run, so
                // look at the raw events of the frame.
                escape: i.keys_down.contains(&Key::Escape)
                    || i.raw.events.iter().any(|e| {
                        matches!(
                            e,
                            Event::Key {
                                key: Key::Escape,
                                pressed: true,
                                ..
                            }
                        )
                    }),
            }
        })
    }
}

fn ctrl(m: Modifiers) -> bool {
    m.ctrl || m.command
}

fn base_id(part: &str) -> Id {
    Id::unique(("komponist_piano_roll", part))
}

impl PianoRoll {
    /// Ruler, keyboard, grid and lane for one frame.
    pub(super) fn canvas(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, f: &Frame<'_>) {
        let sense = Sense::CLICK | Sense::DRAG;
        let ruler = ui.interact(f.geo.ruler, base_id("ruler"), sense);
        let keys = ui.interact(f.geo.keys, base_id("keys"), sense);
        let lane = f.geo.lane.map(|r| ui.interact(r, base_id("lane"), sense));
        let grid = ui.interact(f.geo.grid, base_id("grid"), sense);

        let mut areas = vec![
            (Area::Grid, &grid),
            (Area::Keys, &keys),
            (Area::Ruler, &ruler),
        ];
        if let Some(l) = &lane {
            areas.push((Area::Lane, l));
        }
        let p = Pointer::read(ui);
        if f.live {
            if self
                .gesture
                .as_ref()
                .is_some_and(|g| *g != Gesture::Cancelled)
            {
                self.cancel(ctx);
            }
        } else {
            self.pointer(ctx, f, &areas, p);
            self.cursor(ui, f, &grid, &lane, p);
        }

        let painter = ui.painter().clone();
        self.paint_ruler(&painter, f);
        self.paint_keys(&painter, f);
        let hovered = self.hovered_note(f, &grid, p);
        let visible = self.paint_grid(&painter, f, hovered);
        self.paint_lane(&painter, f);
        self.tooltips(ui, f);

        let model = ruler_model(f);
        let lang = komponist_ui::lang(ui.ctx());
        ruler.widget_info(|| ruler_access(lang, &model).widget_info());
        self.access(ui, f, &visible);
    }

    // --------------------------------------------------------------------------------------------
    // Pointer

    fn pointer(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        f: &Frame<'_>,
        areas: &[(Area, &Response)],
        p: Pointer,
    ) {
        if p.escape && self.gesture.is_some() {
            self.cancel(ctx);
        }
        if self.gesture.is_none() {
            let pressed = areas
                .iter()
                .find(|(_, r)| (r.is_pointer_button_down_on() && p.primary_down) || r.clicked());
            if let Some((area, response)) = pressed
                && let Some(at) = p.origin.or(response.interact_pointer_pos())
            {
                self.press(ctx, f, *area, at, p.press);
            }
        }
        if self.gesture.is_some() {
            if let Some(at) = p.latest {
                self.update(ctx, f, at, p.now);
            }
            if !p.primary_down {
                self.release(ctx, f);
            }
        }
        let grid = areas.iter().find(|(a, _)| *a == Area::Grid).map(|(_, r)| r);
        if f.tool == Tool::Pointer
            && let Some(grid) = grid
            && grid.double_clicked()
            && let Some(at) = grid.interact_pointer_pos()
        {
            self.double_click(ctx, f, at);
        }
    }

    fn press(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        f: &Frame<'_>,
        area: Area,
        at: Pos2,
        mods: Modifiers,
    ) {
        match area {
            Area::Keys => {
                self.gesture = Some(Gesture::Key);
                self.audition(ctx, f.track, Some(f.geo.pitch_at_clamped(at.y)));
            }
            Area::Lane => {
                self.gesture = Some(Gesture::Lane {
                    begun: false,
                    last: None,
                })
            }
            Area::Ruler => self.gesture = Some(Gesture::Ruler { last: None }),
            Area::Grid => self.press_grid(ctx, f, at, mods),
        }
    }

    fn press_grid(&mut self, ctx: &mut ViewCtx<'_>, f: &Frame<'_>, at: Pos2, mods: Modifiers) {
        if f.tool == Tool::Eraser {
            self.gesture = Some(Gesture::Erase {
                erased: BTreeSet::new(),
                begun: false,
            });
            return;
        }
        if let Some(hit) = edit::hit_note(f.notes, f.window.clone(), &f.geo, at) {
            let n = f.notes[hit.index];
            let selected = f.selection.contains(&n.id);
            let rect = f.geo.note_rect(n.start, n.length, n.pitch);
            if ctrl(mods) {
                let notes = if selected {
                    selected_in_order(f)
                } else {
                    vec![n.id]
                };
                self.gesture = Some(Gesture::Velocity(VelocityDrag {
                    notes,
                    anchor_y: at.y,
                    velocity: n.velocity,
                    last: 0,
                    begun: false,
                    rect,
                }));
                return;
            }
            let mut selection = f.selection.clone();
            if !selected {
                let mode = if mods.shift {
                    SelectMode::Add
                } else {
                    selection.clear();
                    SelectMode::Replace
                };
                selection.insert(n.id);
                ctx.send(select(vec![n.id], mode));
            } else if mods.shift && !hit.edge {
                ctx.send(select(vec![n.id], SelectMode::Toggle));
                self.gesture = Some(Gesture::Held);
                return;
            }
            let targets: Vec<&Note> = f
                .notes
                .iter()
                .filter(|q| selection.contains(&q.id))
                .collect();
            self.audition(ctx, f.track, Some(n.pitch));
            self.gesture = Some(if hit.edge {
                Gesture::Resize(ResizeDrag {
                    notes: targets.iter().map(|q| q.id).collect(),
                    originals: targets.iter().map(|q| (q.start, q.length)).collect(),
                    length: n.length,
                    anchor_x: at.x,
                    last: Tick::ZERO,
                    begun: false,
                    rect,
                })
            } else {
                Gesture::Move(MoveDrag {
                    notes: targets.iter().map(|q| q.id).collect(),
                    originals: targets
                        .iter()
                        .map(|q| (q.start, q.length, q.pitch))
                        .collect(),
                    anchor: at,
                    copy: mods.alt,
                    pitch: n.pitch,
                    last: (Tick::ZERO, 0),
                    begun: false,
                })
            });
            return;
        }
        let Some(pitch) = f.geo.pitch_at(at.y) else {
            self.gesture = Some(Gesture::Held);
            return;
        };
        if f.tool == Tool::Pencil {
            let snap = f.snap && !mods.shift;
            let start = edit::new_note_start(f.geo.tick_at(at.x), f.clip.start, f.grid, snap);
            self.gesture = Some(Gesture::Pencil(PencilDrag {
                start,
                pitch,
                length: f.grid,
            }));
            self.audition(ctx, f.track, Some(pitch));
            return;
        }
        let base = if mods.shift {
            f.selection.clone()
        } else {
            if !f.selection.is_empty() {
                ctx.send(select(Vec::new(), SelectMode::Replace));
            }
            BTreeSet::new()
        };
        self.gesture = Some(Gesture::Box(BoxDrag {
            origin: at,
            current: at,
            last: base.iter().copied().collect(),
            base,
        }));
    }

    fn update(&mut self, ctx: &mut ViewCtx<'_>, f: &Frame<'_>, at: Pos2, mods: Modifiers) {
        let snap: Snap = (f.snap && !mods.shift).then_some(f.grid);
        let clip = f.clip.id;
        let mut audition = None;
        match &mut self.gesture {
            Some(Gesture::Move(d)) => {
                let dx = f64::from(at.x - d.anchor.x) / f64::from(f.geo.axis.px_per_tick);
                let delta = edit::move_delta(dx, snap);
                let pitch_delta = edit::pitch_delta(at.y - d.anchor.y, f.geo.key_height);
                if (delta, pitch_delta) != d.last {
                    if d.last.1 != pitch_delta {
                        audition = Some((i32::from(d.pitch) + pitch_delta).clamp(0, 127) as u8);
                    }
                    d.last = (delta, pitch_delta);
                    let label = if d.copy {
                        "Noten kopieren"
                    } else {
                        "Noten verschieben"
                    };
                    begin(ctx, &mut d.begun, label);
                    let notes = d.notes.clone();
                    ctx.send(Action::Edit(if d.copy {
                        EditAction::CopyNotes {
                            clip,
                            notes,
                            delta,
                            pitch_delta,
                        }
                    } else {
                        EditAction::MoveNotes {
                            clip,
                            notes,
                            delta,
                            pitch_delta,
                        }
                    }));
                }
            }
            Some(Gesture::Resize(d)) => {
                let dx = f64::from(at.x - d.anchor_x) / f64::from(f.geo.axis.px_per_tick);
                let delta = edit::resized_length(d.length, dx, snap) - d.length;
                if delta != d.last {
                    d.last = delta;
                    begin(ctx, &mut d.begun, "Noten-Länge");
                    ctx.send(Action::Edit(EditAction::ResizeNotes {
                        clip,
                        notes: d.notes.clone(),
                        delta,
                    }));
                }
            }
            Some(Gesture::Velocity(d)) => {
                let offset = edit::velocity_offset(at.y - d.anchor_y);
                if offset != d.last {
                    d.last = offset;
                    begin(ctx, &mut d.begun, "Velocity");
                    ctx.send(Action::Edit(EditAction::SetNotesVelocity {
                        clip,
                        notes: d.notes.clone(),
                        change: VelocityChange::Offset(offset),
                    }));
                    if let Some(n) = f.notes.iter().find(|n| d.notes.contains(&n.id)) {
                        let v = (i16::from(n.velocity) + offset).clamp(1, 127);
                        status(ctx, format!("Velocity {v}"));
                    }
                }
            }
            Some(Gesture::Pencil(d)) => {
                let snap = f.snap && !mods.shift;
                d.length = edit::pencil_length(d.start, f.geo.local_tick_at(at.x), f.grid, snap);
            }
            Some(Gesture::Erase { erased, begun }) => {
                if let Some(hit) = edit::hit_note(f.notes, f.window.clone(), &f.geo, at) {
                    let id = f.notes[hit.index].id;
                    if erased.insert(id) {
                        begin(ctx, begun, "Noten löschen");
                        ctx.send(Action::Edit(EditAction::RemoveNotes {
                            clip,
                            notes: vec![id],
                        }));
                    }
                }
            }
            Some(Gesture::Box(d)) => {
                d.current = at;
                let r = Rect::from_two_pos(d.origin, d.current);
                let (t0, t1) = (
                    f.geo.local_tick_at(r.left()),
                    f.geo.local_tick_at(r.right()),
                );
                let hi = f.geo.pitch_at_clamped(r.top());
                let lo = f.geo.pitch_at_clamped(r.bottom());
                let mut ids = d.base.clone();
                ids.extend(edit::box_notes(f.notes, t0, t1, lo, hi));
                let ids: Vec<NoteId> = ids.into_iter().collect();
                if ids != d.last {
                    d.last = ids.clone();
                    ctx.send(select(ids, SelectMode::Replace));
                }
            }
            Some(Gesture::Lane { begun, last }) => {
                if let Some(lane) = f.geo.lane {
                    let at_tick = f.geo.local_tick_at(at.x);
                    let max = f64::from(m::VELOCITY_PICK_DISTANCE / f.geo.axis.px_per_tick);
                    let targets = edit::lane_targets(f.notes, &f.selection, at_tick, max);
                    let v = edit::lane_velocity(lane.bottom() - m::VELOCITY_BOTTOM - at.y);
                    let next = (targets, v);
                    if !next.0.is_empty() && last.as_ref() != Some(&next) {
                        begin(ctx, begun, "Velocity");
                        ctx.send(Action::Edit(EditAction::SetNotesVelocity {
                            clip,
                            notes: next.0.clone(),
                            change: VelocityChange::Set(v),
                        }));
                        status(ctx, format!("Velocity {v}"));
                        *last = Some(next);
                    }
                }
            }
            Some(Gesture::Key) => audition = Some(f.geo.pitch_at_clamped(at.y)),
            Some(Gesture::Ruler { last }) => {
                let beat = f.meter.ticks_per_beat() as f64;
                let tick = Tick(((f.geo.tick_at(at.x) / beat).round() * beat).max(0.0) as i64);
                if *last != Some(tick) {
                    *last = Some(tick);
                    ctx.send(Action::Transport(TransportAction::Locate(tick)));
                }
            }
            Some(Gesture::Held | Gesture::Cancelled) | None => {}
        }
        if audition.is_some() {
            self.audition(ctx, f.track, audition);
        }
    }

    fn release(&mut self, ctx: &mut ViewCtx<'_>, f: &Frame<'_>) {
        match self.gesture.take() {
            Some(Gesture::Move(d)) if d.begun => {
                if d.last == (Tick::ZERO, 0) {
                    ctx.send(Action::Edit(EditAction::CancelGesture));
                } else {
                    // `move_notes` keeps every note at ≥ 0.
                    let min = d.originals.iter().map(|o| o.0).min().unwrap_or(Tick::ZERO);
                    let delta = d.last.0.max(-min);
                    let end = d.originals.iter().map(|o| o.0 + delta + o.1).max();
                    self.extend_clip(ctx, f, end);
                    ctx.send(Action::Edit(EditAction::EndGesture));
                }
            }
            Some(Gesture::Resize(d)) if d.begun => {
                if d.last == Tick::ZERO {
                    ctx.send(Action::Edit(EditAction::CancelGesture));
                } else {
                    let end = d
                        .originals
                        .iter()
                        .map(|(s, l)| *s + (*l + d.last).max(Tick(1)))
                        .max();
                    self.extend_clip(ctx, f, end);
                    ctx.send(Action::Edit(EditAction::EndGesture));
                }
            }
            Some(Gesture::Velocity(d)) if d.begun => {
                ctx.send(Action::Edit(if d.last == 0 {
                    EditAction::CancelGesture
                } else {
                    EditAction::EndGesture
                }));
            }
            Some(Gesture::Erase { begun: true, .. }) | Some(Gesture::Lane { begun: true, .. }) => {
                ctx.send(Action::Edit(EditAction::EndGesture));
            }
            Some(Gesture::Pencil(d)) => {
                let note = NewNote {
                    start: d.start,
                    length: d.length,
                    pitch: d.pitch,
                    velocity: m::NEW_NOTE_VELOCITY,
                };
                self.add_note(ctx, f, note);
            }
            _ => {}
        }
        self.audition(ctx, f.track, None);
    }

    /// Esc during a drag: the document goes back to the press, the drag waits for the release.
    fn cancel(&mut self, ctx: &mut ViewCtx<'_>) {
        let begun = match &self.gesture {
            Some(Gesture::Move(d)) => d.begun,
            Some(Gesture::Resize(d)) => d.begun,
            Some(Gesture::Velocity(d)) => d.begun,
            Some(Gesture::Erase { begun, .. }) | Some(Gesture::Lane { begun, .. }) => *begun,
            _ => false,
        };
        if begun {
            ctx.send(Action::Edit(EditAction::CancelGesture));
        }
        self.gesture = Some(Gesture::Cancelled);
    }

    fn double_click(&mut self, ctx: &mut ViewCtx<'_>, f: &Frame<'_>, at: Pos2) {
        if let Some(hit) = edit::hit_note(f.notes, f.window.clone(), &f.geo, at) {
            ctx.send(Action::Edit(EditAction::RemoveNotes {
                clip: f.clip.id,
                notes: vec![f.notes[hit.index].id],
            }));
        } else if let Some(pitch) = f.geo.pitch_at(at.y) {
            let start = edit::new_note_start(f.geo.tick_at(at.x), f.clip.start, f.grid, f.snap);
            let note = NewNote {
                start,
                length: f.grid,
                pitch,
                velocity: m::NEW_NOTE_VELOCITY,
            };
            self.add_note(ctx, f, note);
        }
    }

    /// Adds `note`, lengthening the clip in the same undo step if it ends after it.
    fn add_note(&mut self, ctx: &mut ViewCtx<'_>, f: &Frame<'_>, note: NewNote) {
        let add = Action::Edit(EditAction::AddNotes {
            clip: f.clip.id,
            notes: vec![note],
        });
        match edit::extended_length(f.clip, note.start + note.length, f.meter) {
            Some(length) => {
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: "Note setzen".to_owned(),
                }));
                ctx.send(add);
                ctx.send(Action::Edit(EditAction::SetClipLength {
                    clip: f.clip.id,
                    length,
                }));
                ctx.send(Action::Edit(EditAction::EndGesture));
            }
            None => ctx.send(add),
        }
    }

    /// Inside an open gesture, before it ends: lengthens the clip if notes now end at `end`.
    fn extend_clip(&self, ctx: &mut ViewCtx<'_>, f: &Frame<'_>, end: Option<Tick>) {
        if let Some(length) = end.and_then(|e| edit::extended_length(f.clip, e, f.meter)) {
            ctx.send(Action::Edit(EditAction::SetClipLength {
                clip: f.clip.id,
                length,
            }));
        }
    }

    /// Plays `pitch` on `track` (and stops the key played before), or stops with `None`.
    pub(super) fn audition(&mut self, ctx: &mut ViewCtx<'_>, track: TrackId, pitch: Option<u8>) {
        let next = pitch.map(|p| (track, p));
        if self.audition == next {
            return;
        }
        if let Some((t, key)) = self.audition.take() {
            ctx.send(live(t, key, 0));
        }
        if let Some((t, key)) = next {
            ctx.send(live(t, key, m::AUDITION_VELOCITY));
        }
        self.audition = next;
    }

    fn hovered_note(&self, f: &Frame<'_>, grid: &Response, p: Pointer) -> Option<NoteId> {
        if self.gesture.is_some() || !grid.hovered() {
            return None;
        }
        let at = p.latest?;
        edit::hit_note(f.notes, f.window.clone(), &f.geo, at).map(|h| f.notes[h.index].id)
    }

    fn cursor(&self, ui: &Ui, f: &Frame<'_>, grid: &Response, lane: &Option<Response>, p: Pointer) {
        let icon = match &self.gesture {
            Some(Gesture::Resize(_)) => Some(CursorIcon::ResizeHorizontal),
            Some(Gesture::Lane { .. }) => Some(CursorIcon::ResizeVertical),
            Some(_) => None,
            None if grid.hovered() => match f.tool {
                Tool::Pencil => Some(CursorIcon::Crosshair),
                Tool::Eraser => Some(CursorIcon::Cell),
                Tool::Pointer => p
                    .latest
                    .and_then(|at| edit::hit_note(f.notes, f.window.clone(), &f.geo, at))
                    .filter(|h| h.edge)
                    .map(|_| CursorIcon::ResizeHorizontal),
            },
            None if lane.as_ref().is_some_and(Response::hovered) => {
                Some(CursorIcon::ResizeVertical)
            }
            None => None,
        };
        if let Some(icon) = icon {
            ui.set_cursor_icon(icon);
        }
    }

    // --------------------------------------------------------------------------------------------
    // Painting

    fn paint_ruler(&self, painter: &egui::Painter, f: &Frame<'_>) {
        let ppp = painter.pixels_per_point();
        let w = px::line_width(ppp);
        let g = &f.geo;
        px::fill(painter, g.corner, 0.0, color32(color::SURFACE_2));
        px::vline(
            painter,
            g.corner.right() - w,
            g.corner.y_range(),
            color32(color::SURFACE_0),
        );
        let r = Rect::from_min_max(g.ruler.min, pos2(g.ruler.right(), g.ruler.bottom() - w));
        paint_ruler(painter, r, &ruler_model(f));
        px::hline(
            painter,
            g.corner.left()..=g.ruler.right(),
            g.ruler.bottom() - w,
            color32(color::SURFACE_0),
        );
    }

    fn paint_keys(&self, painter: &egui::Painter, f: &Frame<'_>) {
        let ppp = painter.pixels_per_point();
        let w = px::line_width(ppp);
        let g = &f.geo;
        let painter = painter.with_clip_rect(g.keys.intersect(painter.clip_rect()));
        let playing = self.playing_keys(f);
        let mut mesh = Mesh::default();
        let mut labels = Vec::new();
        let right = g.keys.right() - w;
        for pitch in g.bottom_key()..=g.top_key {
            let top = g.row_top(pitch);
            let black = is_black(pitch);
            let key_right = if black {
                right - m::BLACK_KEY_INSET
            } else {
                right
            };
            let key = px::snap_rect(
                Rect::from_min_max(
                    pos2(g.keys.left(), top),
                    pos2(key_right, top + g.key_height),
                ),
                ppp,
            );
            let on = playing.contains(&pitch);
            let fill = if on {
                f.palette.key_playing
            } else if black {
                color::SURFACE_0
            } else {
                color::SURFACE_4
            };
            mesh.add_colored_rect(key, color32(fill));
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(key.left(), key.bottom() - w), key.max),
                color32(color::SURFACE_1),
            );
            if pitch % 12 == 0 && g.key_height >= typography::VALUE_S.size {
                let ink = if on { color::SURFACE_0 } else { color::TEXT_3 };
                labels.push((key, pitch, ink));
            }
        }
        painter.add(Shape::mesh(mesh));
        for (key, pitch, ink) in labels {
            let galley = painter.layout_job(fonts::layout_job(
                &edit::key_name(pitch),
                &typography::VALUE_S,
                color32(ink),
            ));
            // `lineHeight: KH-1`: centred above the key's bottom line.
            let y = key.top() + (key.height() - w - galley.size().y) / 2.0;
            let x = key.right() - m::KEY_LABEL_PAD - galley.size().x;
            painter.galley(
                pos2(px::snap(x, ppp), px::snap(y, ppp)),
                galley,
                Color32::PLACEHOLDER,
            );
        }
        px::vline(&painter, right, g.keys.y_range(), color32(color::SURFACE_0));
    }

    /// The keys that light up: notes under the playhead while playing, the key played by
    /// clicking and the keys held on the computer keyboard.
    fn playing_keys(&self, f: &Frame<'_>) -> BTreeSet<u8> {
        let mut keys: BTreeSet<u8> = self.audition.map(|(_, k)| k).into_iter().collect();
        keys.extend(f.held.iter().copied());
        if let Some(at) = f.playing {
            keys.extend(
                f.notes
                    .iter()
                    .take_while(|n| n.start <= at)
                    .filter(|n| edit::sounds_at(n, at))
                    .map(|n| n.pitch),
            );
        }
        keys
    }

    /// Paints the grid and its notes; returns the visible notes (index, rect, state) for the
    /// AccessKit nodes.
    fn paint_grid(
        &self,
        painter: &egui::Painter,
        f: &Frame<'_>,
        hovered: Option<NoteId>,
    ) -> Vec<(usize, Rect, NoteState)> {
        let ppp = painter.pixels_per_point();
        let w = px::line_width(ppp);
        let g = &f.geo;
        let painter = painter.with_clip_rect(g.grid.intersect(painter.clip_rect()));

        // Rows: black keys darker, a line.2 line under every C.
        let mut mesh = Mesh::default();
        for pitch in g.bottom_key()..=g.top_key {
            let row = px::snap_rect(g.row(pitch), ppp);
            let fill = if is_black(pitch) {
                color::CANVAS_BLACK_KEY_ROW
            } else {
                color::SURFACE_2
            };
            mesh.add_colored_rect(row, color32(fill));
            if pitch % 12 == 0 {
                mesh.add_colored_rect(
                    Rect::from_min_max(pos2(row.left(), row.bottom() - w), row.max),
                    color32(color::LINE_2),
                );
            }
        }
        painter.add(Shape::mesh(mesh));
        self.paint_lines(&painter, f, g.grid);

        // Notes.
        let bottom = g.bottom_key();
        let ghosts: BTreeSet<NoteId> = match &self.gesture {
            Some(Gesture::Move(d)) if d.copy && d.begun => d.notes.iter().copied().collect(),
            _ => BTreeSet::new(),
        };
        let mut batch = NoteBatch::with_capacity(ppp, f.window.len());
        let mut visible = Vec::new();
        if let Some(Gesture::Move(d)) = &self.gesture
            && d.begun
            && !d.copy
        {
            let ghost = NoteState {
                ghost: true,
                ..NoteState::REST
            };
            for (start, length, pitch) in &d.originals {
                if (bottom..=g.top_key).contains(pitch) {
                    batch.add(
                        g.note_rect(*start, *length, *pitch),
                        f.palette,
                        *pitch,
                        0,
                        ghost,
                    );
                }
            }
        }
        for i in f.window.clone() {
            let n = &f.notes[i];
            if n.pitch < bottom || n.pitch > g.top_key {
                continue;
            }
            let rect = g.note_rect(n.start, n.length, n.pitch);
            if rect.right() < g.grid.left() || rect.left() > g.grid.right() {
                continue;
            }
            let state = NoteState {
                hovered: hovered == Some(n.id),
                selected: f.selection.contains(&n.id),
                playing: f.playing.is_some_and(|at| edit::sounds_at(n, at)),
                muted: f.muted,
                ghost: ghosts.contains(&n.id),
            };
            batch.add(rect, f.palette, n.pitch, n.velocity, state);
            visible.push((i, rect, state));
        }
        if let Some(Gesture::Pencil(d)) = &self.gesture {
            let state = NoteState {
                selected: true,
                muted: f.muted,
                ..NoteState::REST
            };
            batch.add(
                g.note_rect(d.start, d.length, d.pitch),
                f.palette,
                d.pitch,
                m::NEW_NOTE_VELOCITY,
                state,
            );
        }
        batch.paint(&painter);

        if let Some(Gesture::Box(d)) = &self.gesture {
            let r = Rect::from_two_pos(d.origin, d.current).intersect(g.grid);
            px::fill(&painter, r, 0.0, color32(color::CANVAS_BOX_SELECT));
            px::inner_border(&painter, r, color32(color::ACCENT));
        }
        self.paint_playhead(&painter, f, g.grid);
        if f.notes.is_empty() && !f.live && !matches!(self.gesture, Some(Gesture::Pencil(_))) {
            let galley = painter.layout_job(fonts::layout_job(
                names::EMPTY_HINT,
                &typography::UI,
                color32(color::TEXT_3),
            ));
            let pos = empty_hint_pos(g.grid, galley.size());
            painter.galley(
                pos2(px::snap(pos.x, ppp), px::snap(pos.y, ppp)),
                galley,
                Color32::PLACEHOLDER,
            );
        }
        visible
    }

    /// Grid lines and the outside-clip veil over `rect` (grid or lane).
    fn paint_lines(&self, painter: &egui::Painter, f: &Frame<'_>, rect: Rect) {
        let g = &f.geo;
        let spec = GridSpec::new(f.grid_kind, m::GRID_MIN_SPACING);
        let lines = grid_lines(&g.axis, f.meter, rect.x_range(), &spec);
        paint_grid(painter, rect, &lines);
        let end = g.x_of_local(edit::content_length(f.clip)).max(rect.left());
        if end < rect.right() {
            px::fill(
                painter,
                Rect::from_min_max(pos2(end, rect.top()), rect.max),
                0.0,
                color32(color::CANVAS_OUTSIDE_CLIP),
            );
        }
    }

    fn paint_playhead(&self, painter: &egui::Painter, f: &Frame<'_>, rect: Rect) {
        let x = f.geo.axis.x(f.playhead);
        if (rect.left()..rect.right()).contains(&x) {
            paint_playhead(painter, x, rect.y_range());
        }
    }

    fn paint_lane(&self, painter: &egui::Painter, f: &Frame<'_>) {
        let (Some(label), Some(lane)) = (f.geo.lane_label, f.geo.lane) else {
            return;
        };
        let ppp = painter.pixels_per_point();
        let w = px::line_width(ppp);
        // Label column: "VEL" at the top, "127" at the bottom.
        px::fill(painter, label, 0.0, color32(color::SURFACE_2));
        px::vline(
            painter,
            label.right() - w,
            label.y_range(),
            color32(color::SURFACE_0),
        );
        let pad = m::VELOCITY_LABEL_PAD;
        let top = painter.layout_job(fonts::layout_job(
            names::VELOCITY_KEY,
            &typography::LABEL,
            color32(color::TEXT_3),
        ));
        painter.galley(
            pos2(label.left() + pad, px::snap(label.top() + w + pad, ppp)),
            top,
            Color32::PLACEHOLDER,
        );
        let max = painter.layout_job(fonts::layout_job(
            names::VELOCITY_MAX,
            &typography::VALUE_S,
            color32(color::TEXT_4),
        ));
        let y = label.bottom() - pad - max.size().y;
        painter.galley(
            pos2(label.left() + pad, px::snap(y, ppp)),
            max,
            Color32::PLACEHOLDER,
        );

        let area = Rect::from_min_max(pos2(lane.left(), lane.top() + w), lane.max);
        let clipped = painter.with_clip_rect(area.intersect(painter.clip_rect()));
        self.paint_lines(&clipped, f, area);
        let base = lane.bottom() - m::VELOCITY_BOTTOM;
        let mut mesh = Mesh::default();
        for i in f.window.clone() {
            let n = &f.notes[i];
            let x = px::snap(
                f.geo.x_of_local(n.start) + komponist_ui::widgets::metrics::timeline::NOTE_INSET,
                ppp,
            );
            if x > area.right() || x + m::VELOCITY_HEAD_MAX_WIDTH < area.left() {
                continue;
            }
            let h = px::snap(f32::from(n.velocity) / 127.0 * m::VELOCITY_MAX_HEIGHT, ppp);
            let c = if f.selection.contains(&n.id) {
                color32(color::ACCENT)
            } else {
                color32(f.palette.note(n.velocity))
            };
            let top = base - h;
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, top), pos2(x + m::VELOCITY_STEM_WIDTH, base)),
                c,
            );
            let head = (f.geo.axis.width(n.length) - m::VELOCITY_HEAD_SHORTEN)
                .clamp(m::VELOCITY_HEAD_MIN_WIDTH, m::VELOCITY_HEAD_MAX_WIDTH);
            let head = px::snap(head, ppp);
            mesh.add_colored_rect(
                Rect::from_min_max(pos2(x, top), pos2(x + head, top + m::VELOCITY_HEAD_HEIGHT)),
                c,
            );
        }
        clipped.add(Shape::mesh(mesh));
        self.paint_playhead(&clipped, f, area);
        px::hline(
            painter,
            label.left()..=lane.right(),
            lane.top(),
            color32(color::SURFACE_0),
        );
    }

    /// Value tooltips while dragging (`ix.note`: "Wert im Tooltip"; length like the clip edge).
    fn tooltips(&self, ui: &Ui, f: &Frame<'_>) {
        let (rect, text) = match &self.gesture {
            Some(Gesture::Velocity(d)) => {
                let v = (i16::from(d.velocity) + d.last).clamp(1, 127);
                (d.rect, v.to_string())
            }
            Some(Gesture::Resize(d)) => (
                d.rect,
                edit::fmt_note_length((d.length + d.last).max(Tick(1)), f.meter),
            ),
            Some(Gesture::Lane {
                last: Some((_, v)), ..
            }) => {
                let at = ui.input(|i| i.pointer.latest_pos()).unwrap_or_default();
                (Rect::from_center_size(at, vec2(0.0, 0.0)), v.to_string())
            }
            _ => return,
        };
        tooltip::show_value(ui.ctx(), base_id("value"), rect, Placement::Above, &text);
    }

    // --------------------------------------------------------------------------------------------
    // Accessibility

    /// AccessKit nodes: the keyboard with its visible keys, the grid with its visible notes
    /// ("Note C4, 9.1.000"), the velocity lane, the empty-clip hint.
    fn access(&self, ui: &mut Ui, f: &Frame<'_>, visible: &[(usize, Rect, NoteState)]) {
        let g = &f.geo;
        let mut keys = ui.new_child(
            UiBuilder::new()
                .max_rect(g.keys)
                .id_salt("komponist_piano_roll_keys")
                .accessibility_role(Role::Group)
                .accessibility_label(names::KEYBOARD),
        );
        if !ui.ctx().has_accesskit_node(keys.unique_id()) {
            return;
        }
        group_bounds(&keys, g.keys);
        for pitch in g.bottom_key()..=g.top_key {
            let top = g.row_top(pitch);
            let r = Rect::from_min_max(
                pos2(g.keys.left(), top),
                pos2(g.keys.right(), top + g.key_height),
            )
            .intersect(g.keys);
            node(
                &mut keys,
                base_id("key").with(pitch),
                r,
                Role::Button,
                &names::key(pitch),
                None,
            );
        }

        let mut notes = ui.new_child(
            UiBuilder::new()
                .max_rect(g.grid)
                .id_salt("komponist_piano_roll_notes")
                .accessibility_role(Role::Grid)
                .accessibility_label(names::NOTES),
        );
        group_bounds(&notes, g.grid);
        for (i, rect, state) in visible.iter().take(m::MAX_NOTE_NODES) {
            let n = &f.notes[*i];
            let access = note_access(
                komponist_ui::lang(ui.ctx()),
                n.pitch,
                f.clip.start + n.start,
                f.meter,
                *state,
            );
            node(
                &mut notes,
                base_id("note").with(n.id.0),
                rect.intersect(g.grid),
                access.role,
                &access.name,
                access.selected,
            );
        }
        if f.notes.is_empty() && !f.live {
            let galley = notes.painter().layout_job(fonts::layout_job(
                names::EMPTY_HINT,
                &typography::UI,
                Color32::PLACEHOLDER,
            ));
            let r = Rect::from_min_size(empty_hint_pos(g.grid, galley.size()), galley.size());
            node(
                &mut notes,
                base_id("empty"),
                r,
                Role::Label,
                names::EMPTY_HINT,
                None,
            );
        }
        if let Some(lane) = g.lane {
            // The group's node is created with the child `Ui`; the lane has no child nodes.
            let group = ui.new_child(
                UiBuilder::new()
                    .max_rect(lane)
                    .id_salt("komponist_piano_roll_lane")
                    .accessibility_role(Role::Group)
                    .accessibility_label(names::VELOCITY_LANE),
            );
            group_bounds(&group, lane);
        }
    }
}

/// Gives the AccessKit node of the group `ui` the bounds `rect` (egui leaves a `Ui`'s node
/// without bounds), so the egui MCP server and screen readers can locate the area.
fn group_bounds(ui: &Ui, rect: Rect) {
    ui.ctx().accesskit_node_builder(ui.unique_id(), |n| {
        n.set_bounds(egui::accesskit::Rect {
            x0: rect.min.x.into(),
            y0: rect.min.y.into(),
            x1: rect.max.x.into(),
            y1: rect.max.y.into(),
        });
    });
}

/// A hover-only AccessKit node `id` at `rect` (in the parent `ui`'s group).
fn node(ui: &mut Ui, id: Id, rect: Rect, role: Role, name: &str, selected: Option<bool>) {
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(role, true, name));
    if let Some(selected) = selected {
        ui.ctx()
            .accesskit_node_builder(id, |n| n.set_selected(selected));
    }
}

/// Where the empty-clip hint sits: centred, its top at 45 % of the grid (`emptyS`: `top:'45%'`).
fn empty_hint_pos(grid: Rect, size: egui::Vec2) -> Pos2 {
    pos2(
        grid.center().x - size.x / 2.0,
        grid.top() + grid.height() * EMPTY_HINT_TOP,
    )
}

/// `emptyS`: `top:'45%'`.
const EMPTY_HINT_TOP: f32 = 0.45;

fn ruler_model(f: &Frame<'_>) -> RulerModel {
    RulerModel {
        axis: f.geo.axis,
        meter: f.meter,
        kind: RulerKind::Editor {
            clip: komponist_model::TickRange::new(
                f.clip.start,
                f.clip.start + edit::content_length(f.clip),
            ),
            clip_color: color32(if f.live {
                color::STATE_RECORD
            } else {
                f.palette.base
            }),
        },
        playhead: Some(f.playhead),
        recording: f.live,
    }
}

fn is_black(pitch: u8) -> bool {
    matches!(pitch % 12, 1 | 3 | 6 | 8 | 10)
}

fn selected_in_order(f: &Frame<'_>) -> Vec<NoteId> {
    f.notes
        .iter()
        .filter(|n| f.selection.contains(&n.id))
        .map(|n| n.id)
        .collect()
}

fn select(notes: Vec<NoteId>, mode: SelectMode) -> Action {
    Action::Ui(UiAction::Select(SelectionChange::Notes { notes, mode }))
}

/// A transient message in the status bar ("Velocity 52", the prototype's `app.status`).
fn status(ctx: &mut ViewCtx<'_>, text: String) {
    ctx.send(Action::Ui(UiAction::ShowStatus {
        text,
        kind: crate::ui_state::StatusKind::Info,
    }));
}

fn live(track: TrackId, key: u8, velocity: u8) -> Action {
    Action::Transport(TransportAction::LiveNote {
        track: Some(track),
        key,
        velocity,
    })
}

/// Sends `BeginGesture(label)` once per drag.
fn begin(ctx: &mut ViewCtx<'_>, begun: &mut bool, label: &str) {
    if !*begun {
        *begun = true;
        ctx.send(Action::Edit(EditAction::BeginGesture {
            label: label.to_owned(),
        }));
    }
}
