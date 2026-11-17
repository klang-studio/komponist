//! The arrangement (WP-40): ruler, track headers, lanes with clips, the "Neue Spur" area and the
//! empty-project hint (S01, S02, S10; design system `ix.header`, `ix.clip`; `Screens.dc.html`
//! `arrangement()`; `ks-view.js` `arrView`, `clipView`).
//!
//! One canvas (ARCHITECTURE §9.2): the lanes are one interaction area that hit-tests its clips
//! itself ([`geometry`]) and paints them with the `komponist_ui::timeline` painters; clip
//! previews are cached per clip and zoom ([`cache`]). Headers are one interaction area each, with
//! the WP-20 track toggles and the WP-21 header meter placed on the painted ones. Every visible
//! track and clip gets an AccessKit node ("Spur Lead, hexa", "Clip Lead A, Takt 9–10").
//!
//! The view never changes the document: it sends [`Action`]s. Drags are gestures
//! (`BeginGesture`, edits with the total delta, `EndGesture`; Esc cancels). It changes its own
//! scroll and zoom in [`crate::ui_state::ArrangementView`]: wheel scrolling, Ctrl + wheel zoom, and
//! the anchoring of `+` / `−` / `Alt +` / `Alt −` (applied by the dispatcher) around the mouse or the
//! playhead.
//!
//! | Where | Input | Action |
//! |---|---|---|
//! | ruler, top half | press, drag | `Transport(Locate)` on beats (Shift: free) |
//! | ruler, bottom half (loop bar) | press, drag | `Transport(SetLoopRange)` in whole bars |
//! | header | click / Shift / Strg | `Ui(Select(Tracks))` replace / range / toggle |
//! | header name | double-click, F2 (first selected track), context menu „Umbenennen“ | rename in place, Enter applies, Esc discards, an empty name keeps the old one |
//! | header | drag ↕ | `Edit(MoveTracks)` at the 2 px insert line |
//! | header bottom edge | drag ↕ (Alt: all) | `Edit(SetTrackProp(Height))`, 32–160 |
//! | M S R I | press, drag across, Alt | `Edit(SetTrackProp)` / `Edit(SetExclusive)` |
//! | clip | click / Shift / Strg | `Ui(Select(Clips))` |
//! | empty lane | click, drag | select the track; box selection |
//! | clip | drag (Shift: no snap, Alt: copy) | `Edit(MoveClips)` / `Edit(CopyClips)` |
//! | clip end | drag | `Edit(SetClipLength)`, length tooltip |
//! | clip corner top right | drag | `Edit(SetClipLoop)` |
//! | clip / empty lane | double-click | `Ui(OpenClip)` / `Edit(AddClip)` (1 bar) |
//! | group header (WP-103) | fold triangle (Alt: all groups) | `Edit(SetTrackProp(Folded))`; members are indented with a bracket, effect buses stand in the "Busse" section ([`buses`]) |
//! | header | right click | context menu: rename, colour, "In Gruppe zusammenfassen" (Strg G), "Send zu neuem Bus", "Send an …", "Ausgang", "Gruppe auflösen" / delete |
//! | lane of a bus | double-click, a clip dragged over it | refused with "Busse nehmen keine Clips auf." |
//! | header, lane, empty area | drop a `PluginRef` | `Plugin(Add)` on the track / a new track |
//! | header, lane | drop a device head dragged with Alt (`CardDrag`, copy) | `Edit(CopyDevice)` on that track |
//! | everywhere | wheel / Shift wheel / Strg wheel | scroll ↕ / ↔ / zoom around the mouse |
//!
//! Keys (Strg D, Strg E, 0, Entf, +, −, Z, Strg [ ]) are the keymap's.

mod audio_drop;
mod buses;
mod cache;
mod files;
pub mod geometry;
mod headers;
mod lanes;
pub mod metrics;
mod overlay;
mod ruler;
mod toolbar;

use std::sync::Arc;

use egui::{Key, Pos2, Rect, Ui, UiBuilder};
use komponist_model::{ChannelRef, ClipId, PluginRef, Project, Tick, TrackId};

use self::cache::{Palettes, PreviewCache};
use self::geometry::Geometry;
use super::devices::CardDrag;
use super::{View, ViewCtx};
use crate::actions::{Action, DropTarget, EditAction, PluginAction};
use crate::metrics::arrangement::{PX_PER_BAR_MAX, PX_PER_BAR_MIN, V_ZOOM};
use crate::ui_state::ArrangementView;
use komponist_ui::widgets::rename_field::RenameState;

/// The count box: "6 Spuren", with buses "8 Spuren · 1 Gruppe · 2 Busse" (`05-arrangement`).
/// Groups and effect buses are not counted as tracks.
pub fn count_line(lang: komponist_i18n::Lang, project: &komponist_model::Project) -> String {
    let groups = project.tracks.iter().filter(|t| t.is_group()).count();
    let buses = project
        .tracks
        .iter()
        .filter(|t| t.is_bus() && !t.folder)
        .count();
    let tracks = project.tracks.len() - groups - buses;
    let mut line = lang.arrangement_count_tracks(tracks);
    if groups > 0 {
        line.push_str(" · ");
        line.push_str(&lang.arrangement_count_groups(groups));
    }
    if buses > 0 {
        line.push_str(" · ");
        line.push_str(&lang.arrangement_count_buses(buses));
    }
    line
}

/// What the pointer is dragging.
#[derive(Clone, Debug, PartialEq)]
enum Drag {
    /// Ruler top: the playhead.
    Playhead,
    /// Ruler bottom: the loop range, from the bar pressed first.
    LoopRange {
        /// Start of the bar pressed first.
        bar: Tick,
        /// The range last sent.
        last: Option<(Tick, Tick)>,
    },
    /// Clips moved or copied.
    Move(MoveDrag),
    /// The end of a clip.
    Trim(EdgeDrag),
    /// Audio source-start trim.
    TrimStart {
        clip: ClipId,
        start: Tick,
        origin: f32,
    },
    /// Audio fade or gain control.
    AudioControl {
        clip: ClipId,
        zone: geometry::Zone,
        origin: Pos2,
        value: f32,
        length: Tick,
        fine: bool,
    },
    /// The loop corner of a clip.
    Loop(EdgeDrag),
    /// Box selection on empty lane space.
    Box {
        /// Press position.
        origin: Pos2,
        /// Selection kept (Shift / Strg at the start).
        base: Vec<ClipId>,
        /// Selection last sent.
        last: Vec<ClipId>,
    },
    /// Track headers dragged to a new position.
    Reorder {
        /// The tracks moved, in display order.
        tracks: Vec<TrackId>,
        /// Insert position under the pointer.
        slot: Option<usize>,
        /// The group or effect bus under the pointer's middle band: dropping sets the tracks'
        /// output to it instead of moving them (then `slot` is `None`).
        onto: Option<TrackId>,
    },
    /// The bottom edge of a header.
    Height {
        /// The track.
        track: TrackId,
        /// Alt: all tracks get the height.
        all: bool,
        /// Displayed height at the press.
        start: f32,
        /// Press y.
        origin: f32,
        /// Stored height last sent.
        last: u16,
    },
}

/// A clip move or copy.
#[derive(Clone, Debug, PartialEq)]
struct MoveDrag {
    /// Gesture-start document for overlap previews and destination validation.
    source: Arc<Project>,
    /// The clips (the originals, for a copy).
    clips: Vec<ClipId>,
    /// Start of the grabbed clip at the press: it snaps to the grid.
    grab_start: Tick,
    /// Row of the grabbed clip at the press.
    grab_row: usize,
    /// Press position.
    origin: Pos2,
    /// Alt at the start: copy, original dashed.
    copy: bool,
    /// Delta last sent (ticks, tracks).
    last: Option<(Tick, i32)>,
}

/// A drag of a clip's end or loop corner.
#[derive(Clone, Debug, PartialEq)]
struct EdgeDrag {
    /// The clip.
    clip: ClipId,
    /// Its start.
    start: Tick,
    /// Its end at the press.
    end: Tick,
    /// Press x.
    origin: f32,
    /// Its loop length at the press.
    loop_length: Option<Tick>,
    /// The content period a loop drag repeats (`None` for step clips, which repeat by
    /// themselves).
    period: Option<Tick>,
    /// Length last sent.
    length: Tick,
}

/// A track being renamed.
#[derive(Clone, Debug, PartialEq)]
struct Rename {
    track: TrackId,
    edit: RenameState,
}

/// Zoom and scroll at the end of the last frame (to anchor zoom changes made by the dispatcher).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Memo {
    px_per_bar: f32,
    scroll_tick: f64,
    v_zoom: f32,
    scroll_y: f32,
}

impl Memo {
    fn of(v: &ArrangementView) -> Memo {
        Memo {
            px_per_bar: v.px_per_bar,
            scroll_tick: v.scroll_tick,
            v_zoom: v.v_zoom,
            scroll_y: v.scroll_y,
        }
    }
}

/// The arrangement view.
#[derive(Debug, Default)]
pub struct Arrangement {
    audio_hover: audio_drop::HoverFile,
    palettes: Palettes,
    previews: PreviewCache,
    audio: files::AudioCache,
    drag: Option<Drag>,
    rename: Option<Rename>,
    /// A press on M S R I opened a gesture (ended on release).
    toggle_gesture: bool,
    memo: Option<Memo>,
}

impl Arrangement {
    /// A fresh view.
    pub fn new() -> Arrangement {
        Arrangement::default()
    }

    /// Number of cached clip previews (diagnostics, tests).
    pub fn cached_previews(&self) -> usize {
        self.previews.len()
    }
}

impl View for Arrangement {
    fn header(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        toolbar::show(ui, ctx);
    }

    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        let project = Arc::clone(ctx.project);
        let body = ui.max_rect();
        self.previews.begin_frame();
        self.audio.begin_frame();
        self.follow_zoom(ui, ctx, &project, body);
        self.wheel(ui, ctx, &project, body);
        clamp_scroll(&mut ctx.ui.arrangement, &project, body);
        let g = Geometry::new(body, &project, &ctx.ui.arrangement);

        if escape_pressed(ui) && self.drag.is_some() {
            self.cancel_drag(ctx);
        }
        if self
            .rename
            .as_ref()
            .is_some_and(|r| project.track(r.track).is_none())
        {
            self.rename = None;
        }
        // A bus made by Strg G or „Send zu neuem Bus“: its name goes into the edit mode.
        if let Some(id) = ctx.ui.arrangement.rename_request.take()
            && let Some(track) = project.track(id)
        {
            self.rename = Some(Rename {
                track: id,
                edit: RenameState::new(&track.name),
            });
        }

        self.ruler(ui, ctx, &project, &g);
        let mut tracks = ui.new_child(
            UiBuilder::new()
                .max_rect(g.tracks)
                .id_salt("komponist_arrangement_tracks"),
        );
        tracks.set_clip_rect(g.tracks.intersect(ui.clip_rect()));
        // Timeline objects can extend left of tick zero while horizontally scrolled. Clip
        // their paint and hit area to the lanes themselves, not the combined header/track
        // region: indented group headers deliberately leave their ancestor bracket gutter open.
        let mut lanes = tracks.new_child(
            UiBuilder::new()
                .max_rect(g.lanes())
                .id_salt("komponist_arrangement_lane_canvas"),
        );
        lanes.set_clip_rect(g.lanes().intersect(tracks.clip_rect()));
        self.lanes(&mut lanes, ctx, &project, &g);
        self.headers(&mut tracks, ctx, &project, &g);
        self.new_track_area(&mut tracks, ctx, &g);
        self.overlays(&mut tracks, ctx, &project, &g);
        if project.tracks.is_empty() {
            self.empty_hint(&mut tracks, ctx, &g);
        }
        self.drop_target(&mut tracks, ctx, &project, &g);
        self.drop_audio(&mut tracks, ctx, &project, &g);

        // Safety net: a drag whose release no response reported (released over a popup, …).
        if self.drag.is_some() && !ui.input(|i| i.pointer.primary_down()) {
            self.finish_drag(ctx);
        }
        if self.toggle_gesture && !ui.input(|i| i.pointer.primary_down()) {
            ctx.send(Action::Edit(EditAction::EndGesture));
            self.toggle_gesture = false;
        }
        self.previews.end_frame();
        self.audio.end_frame();
        self.memo = Some(Memo::of(&ctx.ui.arrangement));
    }
}

impl Arrangement {
    /// Anchors zoom changes made elsewhere (`+` / `−` / `Alt +` / `Alt −` through the dispatcher,
    /// the panel header's buttons) around the mouse over the lanes, else the playhead
    /// (horizontally) or the top (vertically). A change that also moved the scroll position
    /// ("Z: Auswahl einpassen", computed by the dispatcher) keeps it, and scrolls the tracks of the
    /// selected clips into view.
    fn follow_zoom(&self, ui: &Ui, ctx: &mut ViewCtx<'_>, project: &Project, body: Rect) {
        let Some(m) = self.memo else {
            return;
        };
        let playhead = ctx.engine.playhead();
        // Rows of the selected clips (for "Z").
        let selected_rows: Vec<usize> = project
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.clips
                    .iter()
                    .any(|c| ctx.ui.selection.clips.contains(&c.id))
            })
            .map(|(i, _)| i)
            .collect();
        let v = &mut ctx.ui.arrangement;
        let g = Geometry::new(body, project, v);
        let pointer = ui.input(|i| i.pointer.hover_pos());
        if v.scroll_tick != m.scroll_tick {
            // Only the dispatcher's "Z: Auswahl einpassen" scrolls between frames: the tracks of
            // the fitted clips come into view as well.
            if let (Some(&first), Some(&last)) = (selected_rows.first(), selected_rows.last()) {
                let (top, bottom) = (g.rows[first].y.min, g.rows[last].y.max);
                if top < g.tracks.top() || bottom > g.tracks.bottom() {
                    v.scroll_y += top - g.tracks.top();
                }
            }
        } else if v.px_per_bar != m.px_per_bar {
            let bar = project.meter.ticks_per_bar() as f64;
            let (old, new) = (f64::from(m.px_per_bar) / bar, f64::from(v.px_per_bar) / bar);
            let left = f64::from(g.lane_x.min);
            let x = match pointer.filter(|p| g.lanes().contains(*p)) {
                Some(p) => f64::from(p.x),
                None => left + (playhead.0 as f64 - m.scroll_tick) * old,
            };
            let tick = m.scroll_tick + (x - left) / old;
            v.scroll_tick = (tick - (x - left) / new).max(0.0);
        }
        if v.v_zoom != m.v_zoom && v.scroll_y == m.scroll_y {
            let anchor = pointer
                .filter(|p| g.tracks.contains(*p))
                .map_or(0.0, |p| p.y - g.tracks.top());
            let content = m.scroll_y + anchor;
            v.scroll_y = (content * v.v_zoom / m.v_zoom - anchor).max(0.0);
        }
    }

    /// Wheel over the view: scrolls (Shift: horizontally); Strg + wheel or a pinch zooms
    /// horizontally around the mouse.
    fn wheel(&self, ui: &Ui, ctx: &mut ViewCtx<'_>, project: &Project, body: Rect) {
        if !ui.rect_contains_pointer(body) {
            return;
        }
        let (scroll, zoom, pointer) =
            ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        let v = &mut ctx.ui.arrangement;
        let bar = project.meter.ticks_per_bar() as f64;
        if zoom != 1.0 {
            let g = Geometry::new(body, project, v);
            let old = f64::from(v.px_per_bar) / bar;
            let left = f64::from(g.lane_x.min);
            let x = pointer.map_or(left, |p| f64::from(p.x).max(left));
            let tick = v.scroll_tick + (x - left) / old;
            v.px_per_bar = (v.px_per_bar * zoom).clamp(PX_PER_BAR_MIN, PX_PER_BAR_MAX);
            let new = f64::from(v.px_per_bar) / bar;
            v.scroll_tick = (tick - (x - left) / new).max(0.0);
        } else if scroll != egui::Vec2::ZERO {
            v.scroll_y -= scroll.y;
            let ppt = f64::from(v.px_per_bar) / bar;
            v.scroll_tick = (v.scroll_tick - f64::from(scroll.x) / ppt).max(0.0);
        }
    }

    /// Esc during a drag: undo what it did so far.
    fn cancel_drag(&mut self, ctx: &mut ViewCtx<'_>) {
        use crate::actions::{SelectMode, SelectionChange};
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Move(m) => {
                ctx.send(Action::Edit(EditAction::CancelGesture));
                if m.copy {
                    ctx.send(select_clips(m.clips, SelectMode::Replace));
                }
            }
            Drag::Trim(_)
            | Drag::Loop(_)
            | Drag::TrimStart { .. }
            | Drag::AudioControl { .. }
            | Drag::Height { .. } => {
                ctx.send(Action::Edit(EditAction::CancelGesture));
            }
            Drag::Box { base, .. } => {
                ctx.send(Action::Ui(crate::actions::UiAction::Select(
                    SelectionChange::Clips {
                        clips: base,
                        mode: SelectMode::Replace,
                    },
                )));
            }
            Drag::Playhead | Drag::LoopRange { .. } | Drag::Reorder { .. } => {}
        }
    }

    /// The drag ends (release): close its gesture or apply it.
    fn finish_drag(&mut self, ctx: &mut ViewCtx<'_>) {
        use crate::actions::SelectMode;
        let Some(drag) = self.drag.take() else {
            return;
        };
        match drag {
            Drag::Move(m) => {
                let moved = m.last.is_some_and(|d| d != (Tick::ZERO, 0));
                if moved {
                    ctx.send(Action::Edit(EditAction::EndGesture));
                } else {
                    // Nothing moved: no undo step, and no copy on top of the original.
                    ctx.send(Action::Edit(EditAction::CancelGesture));
                    if m.copy {
                        ctx.send(select_clips(m.clips, SelectMode::Replace));
                    }
                }
            }
            Drag::Trim(_)
            | Drag::Loop(_)
            | Drag::TrimStart { .. }
            | Drag::AudioControl { .. }
            | Drag::Height { .. } => {
                ctx.send(Action::Edit(EditAction::EndGesture));
            }
            Drag::Reorder {
                tracks,
                onto: Some(bus),
                ..
            } => match super::routing::output_drop(ctx.lang, ctx.project, &tracks, bus) {
                super::routing::OutputDrop::Set => {
                    ctx.send(Action::Edit(EditAction::SetOutput {
                        tracks,
                        output: komponist_model::TrackOutput::Bus(bus),
                    }));
                }
                super::routing::OutputDrop::Refused(reason) => {
                    ctx.send(Action::Ui(crate::actions::UiAction::ShowStatus {
                        text: reason,
                        kind: crate::ui_state::StatusKind::Info,
                    }));
                }
                super::routing::OutputDrop::Unchanged => {}
            },
            Drag::Reorder { tracks, slot, .. } => {
                if let Some(slot) = slot
                    && let Some(to_index) = reorder_target(ctx.project, &tracks, slot)
                {
                    ctx.send(Action::Edit(EditAction::MoveTracks { tracks, to_index }));
                }
            }
            Drag::Playhead | Drag::LoopRange { .. } | Drag::Box { .. } => {}
        }
    }

    /// A plugin dragged from the browser (an egui drag-and-drop payload of type [`PluginRef`])
    /// and released over a header or lane goes onto that track; over the empty area it makes a
    /// new track (S10). Prototype `dropTrack`, `dropNew`.
    fn drop_target(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, project: &Project, g: &Geometry) {
        if egui::DragAndDrop::has_payload_of_type::<CardDrag>(ui.ctx()) {
            self.drop_device(ui, ctx, project, g);
            return;
        }
        if !egui::DragAndDrop::has_payload_of_type::<PluginRef>(ui.ctx()) {
            return;
        }
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let Some(target) = drop_target_at(project, g, pos) else {
            return;
        };
        // A bus takes audio effects only: an instrument or a MIDI effect dropped on it is refused,
        // with the reason at the pointer.
        if let DropTarget::Track(id) = &target
            && project.track(*id).is_some_and(|t| t.is_bus())
            && let Some(plugin) = egui::DragAndDrop::payload::<PluginRef>(ui.ctx())
            && ctx
                .engine
                .host()
                .role(&plugin)
                .is_some_and(|r| r != komponist_model::DeviceRole::Effect)
        {
            if let Some(i) = g.row_at(pos.y) {
                let r = Rect::from_x_y_ranges(egui::Rangef::point(pos.x), g.rows[i].y);
                komponist_ui::tooltip::show_value(
                    ui.ctx(),
                    egui::Id::unique("komponist_arrangement_effects_only"),
                    r,
                    komponist_ui::tooltip::Placement::Above,
                    ctx.lang.routing_effects_only(),
                );
            }
            return;
        }
        overlay::paint_drop_target(ui, g, &target);
        if ui.input(|i| i.pointer.any_released())
            && let Some(plugin) = egui::DragAndDrop::take_payload::<PluginRef>(ui.ctx())
        {
            ctx.send(Action::Plugin(PluginAction::Add {
                plugin: PluginRef::clone(&plugin),
                target,
            }));
        }
    }
}

impl Arrangement {
    /// A device head dragged out of the chain with Alt (a copy) and released over a track's header or
    /// lane puts a copy on that track (device handoff §9: "Alt-Ziehen kopiert, auch auf eine andere
    /// Spur im Arrangement"); the chain keeps its order and one instrument per track
    /// (`EditAction::CopyDevice`). Without Alt, and over the empty area, nothing happens.
    fn drop_device(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, project: &Project, g: &Geometry) {
        let Some(drag) = egui::DragAndDrop::payload::<CardDrag>(ui.ctx()) else {
            return;
        };
        if !drag.copy {
            return;
        }
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else {
            return;
        };
        let Some(target @ DropTarget::Track(track)) = drop_target_at(project, g, pos) else {
            return;
        };
        overlay::paint_drop_target(ui, g, &target);
        if ui.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(ui.ctx());
            ctx.send(Action::Edit(EditAction::CopyDevice {
                device: drag.device,
                to: ChannelRef::Track(track),
                index: usize::MAX,
            }));
        }
    }
}

/// Where a plugin dropped at `pos` goes: the track of the header or lane under it, else (below
/// the tracks, or in an empty project) a new track at the end.
pub fn drop_target_at(project: &Project, g: &Geometry, pos: Pos2) -> Option<DropTarget> {
    if !g.tracks.contains(pos) {
        return None;
    }
    match g.row_at(pos.y) {
        Some(i) => Some(DropTarget::Track(project.tracks.get(i)?.id)),
        None => Some(DropTarget::NewTrack { index: None }),
    }
}

/// The `to_index` of `MoveTracks` for `tracks` dropped at insert position `slot` (0 ..= track
/// count, counted with the moved tracks), or `None` when the order stays.
pub fn reorder_target(project: &Project, tracks: &[TrackId], slot: usize) -> Option<usize> {
    let order: Vec<TrackId> = project.tracks.iter().map(|t| t.id).collect();
    let to_index = order[..slot.min(order.len())]
        .iter()
        .filter(|t| !tracks.contains(t))
        .count();
    let mut rest: Vec<TrackId> = order
        .iter()
        .copied()
        .filter(|t| !tracks.contains(t))
        .collect();
    let moved: Vec<TrackId> = order
        .iter()
        .copied()
        .filter(|t| tracks.contains(t))
        .collect();
    rest.splice(to_index..to_index, moved);
    (rest != order).then_some(to_index)
}

/// Keeps the scroll inside the content: not before tick 0, not past the last row.
fn clamp_scroll(v: &mut ArrangementView, project: &Project, body: Rect) {
    let g = Geometry::new(body, project, v);
    let max = g.max_scroll_y(project, v.v_zoom);
    v.scroll_y = v.scroll_y.clamp(0.0, max);
    v.scroll_tick = v.scroll_tick.max(0.0);
    v.v_zoom = v.v_zoom.clamp(V_ZOOM[0], V_ZOOM[1]);
}

/// Whether Esc was pressed in this frame. Read from the raw input: the app's keymap consumes Esc
/// (as `ClearSelection`, or `CancelGesture` during a drag) before the views run.
pub(crate) fn escape_pressed(ui: &Ui) -> bool {
    ui.input(|i| {
        i.raw.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key {
                    key: Key::Escape,
                    pressed: true,
                    ..
                }
            )
        })
    })
}

/// Gives the node of a painted timeline object its role, name and selection (egui's
/// `WidgetInfo` reports selection as "toggled"; AccessKit's `selected` is set here as well).
fn register(response: &egui::Response, access: &komponist_ui::timeline::Access) {
    response.widget_info(|| access.widget_info());
    if let Some(selected) = access.selected {
        komponist_ui::widgets::accesskit(response, |n| n.set_selected(selected));
    }
}

/// `Ui(Select(Clips))`.
fn select_clips(clips: Vec<ClipId>, mode: crate::actions::SelectMode) -> Action {
    Action::Ui(crate::actions::UiAction::Select(
        crate::actions::SelectionChange::Clips { clips, mode },
    ))
}

/// `Ui(Select(Tracks))`.
fn select_tracks(tracks: Vec<TrackId>, mode: crate::actions::SelectMode) -> Action {
    Action::Ui(crate::actions::UiAction::Select(
        crate::actions::SelectionChange::Tracks { tracks, mode },
    ))
}

/// The selection mode of a click with `mods`: Strg toggles, Shift extends, else replace.
fn click_mode(mods: egui::Modifiers) -> crate::actions::SelectMode {
    use crate::actions::SelectMode;
    if mods.command || mods.ctrl {
        SelectMode::Toggle
    } else if mods.shift {
        SelectMode::Extend
    } else {
        SelectMode::Replace
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    #[test]
    fn reorder_targets() {
        let p = demo_project();
        let ids: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
        // Lead (2) above Drums (0).
        assert_eq!(reorder_target(&p, &[ids[2]], 0), Some(0));
        // Lead at its own place: no change.
        assert_eq!(reorder_target(&p, &[ids[2]], 2), None);
        assert_eq!(reorder_target(&p, &[ids[2]], 3), None);
        // Lead below Keys (4): insert position 5 → index 4 without Lead.
        assert_eq!(reorder_target(&p, &[ids[2]], 5), Some(4));
        // Drums and Bass to the end.
        assert_eq!(reorder_target(&p, &[ids[0], ids[1]], 6), Some(4));
    }

    #[test]
    fn names_and_grids() {
        use komponist_i18n::Lang;
        assert_eq!(Lang::De.arrangement_count_tracks(0), "0 Spuren");
        assert_eq!(Lang::De.arrangement_count_tracks(1), "1 Spur");
        assert_eq!(Lang::De.status_clips_selected(2), "2 Clips ausgewählt");
        assert_eq!(
            Lang::De.arrangement_plugin_missing("Vital"),
            "⚠ Plugin fehlt: Vital"
        );
        assert_eq!(
            count_line(Lang::De, &demo_project()),
            Lang::De.arrangement_count_tracks(demo_project().tracks.len())
        );
    }
}
