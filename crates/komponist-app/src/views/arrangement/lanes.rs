//! The lanes: one canvas for all tracks (ARCHITECTURE §9.2). It paints lane backgrounds, the bar
//! grid and the visible clips, hit-tests clips itself and turns clicks and drags into actions
//! (`ix.clip`), and registers an AccessKit node per visible clip.

use std::sync::Arc;

use egui::{CornerRadius, CursorIcon, Id, Painter, Pos2, Rangef, Rect, Sense, Ui, pos2};
use komponist_i18n::Lang;
use komponist_ipc::NoteName;
use komponist_model::edits::{NewClip, NewClipContent};
use komponist_model::format::fmt_length;
use komponist_model::{ClipContent, ClipId, Grid, Project, TICKS_PER_STEP, Tick, Track};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, radius};
use komponist_ui::px;
use komponist_ui::timeline::clip::body_height;
use komponist_ui::timeline::{
    AudioFileState, AudioLook, ClipEdge, ClipModel, ClipState, GridSpec, PreviewMesh, clip_access,
    grid_lines, paint_clip, paint_clip_ghost, paint_grid, paint_playhead,
};
use komponist_ui::tooltip::{self, Placement};

use super::geometry::{ClipHit, Geometry, Zone};
use super::{
    Arrangement, Drag, EdgeDrag, MoveDrag, click_mode, metrics, register, select_clips,
    select_tracks,
};
use crate::actions::{Action, EditAction, SelectMode, UiAction};
use crate::step_rows;
use crate::ui_state::ArrangementView;
use crate::views::ViewCtx;

/// The tick a dragged position lands on: on the grid when snapping (and Shift is not held), else
/// on the tick.
pub fn snap_tick(t: f64, view: &ArrangementView, free: bool, project: &Project) -> Tick {
    let tick = Tick(t.round() as i64);
    if view.snap && !free {
        view.grid.snap(tick, project.meter)
    } else {
        tick
    }
}

/// The shortest length a clip edge drag leaves: one grid step when snapping, else one 1/16.
fn min_length(view: &ArrangementView, free: bool, project: &Project) -> Tick {
    if view.snap && !free {
        view.grid.ticks(project.meter)
    } else {
        Tick(TICKS_PER_STEP)
    }
}

/// What a double-click on empty space of `track` at tick `at` creates: a one-bar clip starting on
/// the grid (prototype `newClip`): a step clip "Pattern n" on a sampler track, whose rows the sampler
/// decides ([`crate::step_rows::rows_for_in`] with the `note_names` it reports now: its eight pads,
/// notes 36–43, without note names; ARCHITECTURE §13.3), else an empty MIDI clip "MIDI-Clip".
pub fn new_clip(
    lang: Lang,
    project: &Project,
    track: &Track,
    at: Tick,
    view: &ArrangementView,
    note_names: &[NoteName],
) -> NewClip {
    let meter = project.meter;
    let start = if view.snap {
        view.grid.snap_floor(at, meter)
    } else {
        at
    }
    .max(Tick::ZERO);
    let length = Grid::Bar.ticks(meter);
    if step_rows::is_sampler_track(track) {
        let instrument = step_rows::instrument(track).map(|d| &d.plugin);
        let rows = step_rows::rows_for_in(lang, instrument, note_names);
        NewClip {
            start,
            length,
            name: lang.arrangement_new_pattern(track.clips.len() + 1),
            content: NewClipContent::Step { rows },
        }
    } else {
        NewClip {
            start,
            length,
            name: lang.arrangement_new_midi_clip().to_owned(),
            content: NewClipContent::Midi {
                notes: Vec::new(),
                take_name: String::new(),
            },
        }
    }
}

impl Arrangement {
    /// Lane backgrounds, grid, separators, the clips and the canvas interaction.
    pub(super) fn lanes(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Arc<Project>,
        g: &Geometry,
    ) {
        let lanes = g.lanes();
        let response = ui.interact(
            lanes,
            Id::unique("komponist_arrangement_lanes"),
            Sense::click_and_drag(),
        );
        self.canvas_input(ui, ctx, project, g, &response);

        let painter = ui.painter().clone();
        paint_backgrounds(&painter, ctx, project, g);
        for i in g.visible_rows() {
            super::buses::paint_bus_lane(ui, ctx.lang, project, g, i);
        }
        self.paint_clips(ui, &painter, ctx, project, g);
    }

    fn canvas_input(
        &mut self,
        ui: &Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
        response: &egui::Response,
    ) {
        let (mods, origin, pos) = ui.input(|i| {
            (
                i.modifiers,
                i.pointer.press_origin(),
                i.pointer.interact_pos(),
            )
        });

        if response.drag_started()
            && let Some(origin) = origin
        {
            match g.clip_at(project, origin) {
                Some(hit) => self.start_clip_drag(ctx, project, hit, origin, mods),
                None => {
                    let base = if mods.shift || mods.command || mods.ctrl {
                        ctx.ui.selection.clips.iter().copied().collect()
                    } else {
                        Vec::new()
                    };
                    self.drag = Some(Drag::Box {
                        origin,
                        base,
                        last: Vec::new(),
                    });
                }
            }
        }
        if response.dragged()
            && let Some(pos) = pos
        {
            self.update_clip_drag(ctx, project, g, pos, mods.shift);
        }
        if response.drag_stopped() {
            self.finish_drag(ctx);
        }

        if response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            match g.clip_at(project, pos) {
                Some(hit) if hit.zone == Zone::Gain => {
                    ctx.send(Action::Edit(EditAction::SetAudioClipProp {
                        clip: hit.clip,
                        prop: komponist_model::edits::AudioClipProp::GainDb(0.0),
                    }))
                }
                Some(hit) => ctx.send(Action::Ui(UiAction::OpenClip(hit.clip))),
                None => {
                    if g.row_at(pos.y).is_some_and(|i| {
                        project.tracks[i].kind == komponist_model::TrackKind::Audio
                    }) {
                        // An audio clip needs a linked file; import/drop creates it (#12 part B).
                        return;
                    }
                    if g.row_at(pos.y).is_some_and(|i| !g.rows[i].takes_clips()) {
                        // A bus holds no clips: say why instead of adding one (the model refuses
                        // it as well).
                        ctx.send(Action::Ui(UiAction::ShowStatus {
                            text: ctx.lang.routing_no_clips().to_owned(),
                            kind: crate::ui_state::StatusKind::Info,
                        }));
                    } else if let Some(i) = g.row_at(pos.y) {
                        let track = &project.tracks[i];
                        let at = Tick(g.tick_at(pos.x).max(0.0) as i64);
                        let host = ctx.engine.host();
                        let note_names =
                            step_rows::instrument(track).map_or(&[][..], |d| host.note_names(d.id));
                        let clip = new_clip(
                            ctx.lang,
                            project,
                            track,
                            at,
                            &ctx.ui.arrangement,
                            note_names,
                        );
                        ctx.send(Action::Edit(EditAction::AddClip {
                            track: track.id,
                            clip,
                        }));
                    }
                }
            }
        } else if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            let mode = click_mode(mods);
            match g.clip_at(project, pos) {
                Some(hit) if mods.ctrl && matches!(hit.zone, Zone::FadeIn | Zone::FadeOut) => {
                    let prop = if hit.zone == Zone::FadeIn {
                        komponist_model::edits::AudioClipProp::FadeIn(Tick::ZERO)
                    } else {
                        komponist_model::edits::AudioClipProp::FadeOut(Tick::ZERO)
                    };
                    ctx.send(Action::Edit(EditAction::SetAudioClipProp {
                        clip: hit.clip,
                        prop,
                    }));
                }
                Some(hit) => ctx.send(select_clips(vec![hit.clip], clip_mode(mode))),
                None if mode == SelectMode::Replace => {
                    // Prototype `laneDown`: the track is selected, the clip selection goes.
                    ctx.send(select_clips(Vec::new(), SelectMode::Replace));
                    if let Some(i) = g.row_at(pos.y) {
                        ctx.send(select_tracks(vec![project.tracks[i].id], mode));
                    }
                }
                None => {}
            }
        }

        // Cursor: what a press would do here.
        let cursor = match &self.drag {
            Some(Drag::Move(_)) => Some(CursorIcon::Grabbing),
            Some(Drag::Trim(_) | Drag::Loop(_) | Drag::TrimStart { .. }) => {
                Some(CursorIcon::ResizeHorizontal)
            }
            Some(Drag::AudioControl { zone, .. }) => Some(if *zone == Zone::Gain {
                CursorIcon::ResizeVertical
            } else {
                CursorIcon::ResizeHorizontal
            }),
            Some(_) => None,
            None => pointer_over(ui, g.lanes())
                .and_then(|p| g.clip_at(project, p))
                .map(|hit| match hit.zone {
                    Zone::Body => CursorIcon::Grab,
                    Zone::End | Zone::Loop | Zone::Start | Zone::FadeIn | Zone::FadeOut => {
                        CursorIcon::ResizeHorizontal
                    }
                    Zone::Gain => CursorIcon::ResizeVertical,
                }),
        };
        if let Some(c) = cursor {
            ui.ctx().set_cursor_icon(c);
        }
    }

    fn start_clip_drag(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        hit: ClipHit,
        origin: Pos2,
        mods: egui::Modifiers,
    ) {
        let Some((_, clip)) = project.find_clip(hit.clip) else {
            return;
        };
        let selected = ctx.ui.selection.clips.contains(&hit.clip);
        let adding = mods.shift || mods.command || mods.ctrl;
        if !selected {
            let mode = if adding {
                SelectMode::Add
            } else {
                SelectMode::Replace
            };
            ctx.send(select_clips(vec![hit.clip], mode));
        }
        match hit.zone {
            Zone::Body => {
                // The selection in display order (for one undo step over all of them).
                let mut clips: Vec<ClipId> = project
                    .tracks
                    .iter()
                    .flat_map(|t| t.clips.iter().map(|c| c.id))
                    .filter(|c| {
                        *c == hit.clip
                            || ((selected || adding) && ctx.ui.selection.clips.contains(c))
                    })
                    .collect();
                clips.dedup();
                let copy = mods.alt;
                let label = if copy {
                    "undo-copy-clips"
                } else {
                    "undo-move-clips"
                };
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: label.to_owned(),
                }));
                self.drag = Some(Drag::Move(MoveDrag {
                    source: Arc::clone(ctx.project),
                    clips,
                    grab_start: clip.start,
                    grab_row: hit.row,
                    origin,
                    copy,
                    last: None,
                }));
            }
            Zone::Start => {
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: "undo-clip-length".into(),
                }));
                self.drag = Some(Drag::TrimStart {
                    clip: hit.clip,
                    start: clip.start,
                    origin: origin.x,
                });
            }
            Zone::FadeIn | Zone::FadeOut | Zone::Gain => {
                if let ClipContent::Audio(a) = &clip.content {
                    let value = match hit.zone {
                        Zone::FadeIn => a.fade_in.0 as f32,
                        Zone::FadeOut => a.fade_out.0 as f32,
                        _ => a.gain_db,
                    };
                    ctx.send(Action::Edit(EditAction::BeginGesture {
                        label: if hit.zone == Zone::Gain {
                            "undo-clip-gain"
                        } else {
                            "undo-clip-fade"
                        }
                        .into(),
                    }));
                    self.drag = Some(Drag::AudioControl {
                        clip: hit.clip,
                        zone: hit.zone,
                        origin,
                        value,
                        length: clip.length,
                        fine: mods.shift,
                    });
                }
            }
            Zone::End | Zone::Loop => {
                let step = matches!(clip.content, ClipContent::Step(_));
                let edge = EdgeDrag {
                    clip: hit.clip,
                    start: clip.start,
                    end: clip.end(),
                    origin: origin.x,
                    loop_length: clip.loop_length,
                    period: (!step).then(|| clip.loop_length.unwrap_or(clip.length)),
                    length: clip.length,
                };
                let (label, drag) = if hit.zone == Zone::End {
                    ("undo-clip-length", Drag::Trim(edge))
                } else {
                    ("undo-clip-loop", Drag::Loop(edge))
                };
                ctx.send(Action::Edit(EditAction::BeginGesture {
                    label: label.to_owned(),
                }));
                self.drag = Some(drag);
            }
        }
    }

    fn update_clip_drag(
        &mut self,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
        pos: Pos2,
        free: bool,
    ) {
        let view = ctx.ui.arrangement;
        let ppt = f64::from(g.axis.px_per_tick);
        match &mut self.drag {
            Some(Drag::Move(m)) => {
                let raw = m.grab_start.0 as f64 + f64::from(pos.x - m.origin.x) / ppt;
                let audio = m.clips.iter().any(|id| {
                    m.source
                        .find_clip(*id)
                        .is_some_and(|(_, c)| matches!(c.content, ClipContent::Audio(_)))
                });
                let delta = if audio && view.snap && !free {
                    // Snap from an audio anchor even when the grabbed MIDI clip is off-grid.
                    let anchor = m
                        .clips
                        .iter()
                        .find_map(|id| {
                            m.source.find_clip(*id).and_then(|(_, c)| {
                                matches!(c.content, ClipContent::Audio(_)).then_some(c.start)
                            })
                        })
                        .unwrap_or(m.grab_start);
                    view.grid.snap(
                        Tick((anchor.0 as f64 + raw - m.grab_start.0 as f64).round() as i64),
                        project.meter,
                    ) - anchor
                } else {
                    snap_tick(raw, &view, free, project) - m.grab_start
                };
                let row = g.nearest_row(pos.y).unwrap_or(m.grab_row);
                let track_delta = row as i32 - m.grab_row as i32;
                if m.clips.iter().any(|id| {
                    m.source.find_clip(*id).is_some_and(|(track, _)| {
                        project
                            .tracks
                            .iter()
                            .position(|t| t.id == track.id)
                            .and_then(|i| usize::try_from(i as i32 + track_delta).ok())
                            .and_then(|i| m.source.tracks.get(i))
                            .is_none_or(|target| target.kind != track.kind)
                    })
                }) {
                    // Refusing a destination rolls back the preview; a release there must not
                    // commit an intermediate move to a track the pointer crossed on its way.
                    if m.last.take().is_some() {
                        ctx.send(Action::Edit(EditAction::CancelGesture));
                        ctx.send(Action::Edit(EditAction::BeginGesture {
                            label: if m.copy {
                                "undo-copy-clips"
                            } else {
                                "undo-move-clips"
                            }
                            .into(),
                        }));
                        ctx.send(select_clips(m.clips.clone(), SelectMode::Replace));
                    }
                    ctx.send(Action::Ui(UiAction::ShowStatus {
                        text: if audio {
                            ctx.lang.status_drop_refused_midi()
                        } else {
                            ctx.lang.status_drop_refused_audio()
                        }
                        .to_owned(),
                        kind: crate::ui_state::StatusKind::Info,
                    }));
                    return;
                }
                if m.last != Some((delta, track_delta)) {
                    m.last = Some((delta, track_delta));
                    let (clips, copy) = (m.clips.clone(), m.copy);
                    ctx.send(Action::Edit(if copy {
                        EditAction::CopyClips {
                            clips,
                            delta,
                            track_delta,
                        }
                    } else {
                        EditAction::MoveClips {
                            clips,
                            delta,
                            track_delta,
                        }
                    }));
                }
            }
            Some(Drag::TrimStart {
                clip,
                start,
                origin,
            }) => {
                let raw = start.0 as f64 + f64::from(pos.x - *origin) / ppt;
                let start = snap_tick(raw, &view, free, project);
                ctx.send(Action::Edit(EditAction::TrimAudioClipStart {
                    clip: *clip,
                    start,
                }));
            }
            Some(Drag::AudioControl {
                clip,
                zone,
                origin,
                value,
                length,
                fine,
            }) => {
                use komponist_model::edits::AudioClipProp;
                let prop = if *zone == Zone::Gain {
                    let height = project
                        .find_clip(*clip)
                        .and_then(|(t, _)| project.tracks.iter().position(|v| v.id == t.id))
                        .map_or(
                            komponist_tokens::size::track::DEFAULT
                                - komponist_tokens::size::CLIP_HEADER
                                - 2.0 * komponist_ui::widgets::metrics::timeline::CLIP_INSET_Y,
                            |row| {
                                body_height(
                                g.lane(row).height()
                                    - 2.0 * komponist_ui::widgets::metrics::timeline::CLIP_INSET_Y,
                            )
                            },
                        );
                    let [lo, hi] = komponist_ui::widgets::metrics::timeline::GAIN_RANGE_DB;
                    let db = (*value
                        - (pos.y - origin.y) * (hi - lo) / height.max(1.0)
                            * if *fine {
                                komponist_ui::widgets::metrics::timeline::GAIN_FINE_FACTOR
                            } else {
                                1.0
                            })
                    .clamp(lo, hi);
                    AudioClipProp::GainDb(db)
                } else {
                    let sign = if *zone == Zone::FadeIn { 1.0 } else { -1.0 };
                    let ticks = Tick(
                        (*value as f64 + sign * f64::from(pos.x - origin.x) / ppt).round() as i64,
                    )
                    .max(Tick::ZERO)
                    .min(*length);
                    if *zone == Zone::FadeIn {
                        AudioClipProp::FadeIn(ticks)
                    } else {
                        AudioClipProp::FadeOut(ticks)
                    }
                };
                ctx.send(Action::Edit(EditAction::SetAudioClipProp {
                    clip: *clip,
                    prop,
                }));
            }
            Some(Drag::Trim(e)) => {
                let audio = project.find_clip(e.clip).and_then(|(_, c)| {
                    if let ClipContent::Audio(a) = &c.content {
                        Some(a)
                    } else {
                        None
                    }
                });
                if let Some(mut length) = edge_length(e, pos.x, ppt, &view, free, project) {
                    if let Some(a) = audio {
                        length = length.min(a.max_length(project.tempo));
                        e.length = length;
                    }
                    ctx.send(Action::Edit(EditAction::SetClipLength {
                        clip: e.clip,
                        length,
                    }));
                }
            }
            Some(Drag::Loop(e)) => {
                if let Some(length) = edge_length(e, pos.x, ppt, &view, free, project) {
                    // Repetitions of the content period; a drag that stays within one pass keeps
                    // the clip's loop setting. Step clips repeat their pattern by themselves.
                    let loop_length = match e.period {
                        Some(p) if length > p => Some(p),
                        Some(_) => e.loop_length,
                        None => None,
                    };
                    ctx.send(Action::Edit(EditAction::SetClipLoop {
                        clip: e.clip,
                        loop_length,
                        length,
                    }));
                }
            }
            Some(Drag::Box { origin, base, last }) => {
                let area = Rect::from_two_pos(*origin, pos);
                let mut ids = base.clone();
                for c in g.clips_in(project, area) {
                    if !ids.contains(&c) {
                        ids.push(c);
                    }
                }
                if *last != ids {
                    *last = ids.clone();
                    ctx.send(select_clips(ids, SelectMode::Replace));
                }
            }
            _ => {}
        }
    }

    fn paint_clips(
        &mut self,
        ui: &mut Ui,
        painter: &Painter,
        ctx: &mut ViewCtx<'_>,
        project: &Arc<Project>,
        g: &Geometry,
    ) {
        let any_solo = project.any_solo();
        let gates = any_solo.then(|| komponist_model::routing::gates(project));
        let (t0, t1) = g.visible_ticks();
        let ppt = g.axis.px_per_tick;
        let hover = if self.drag.is_none() {
            pointer_over(ui, g.lanes()).and_then(|p| g.clip_at(project, p))
        } else {
            None
        };
        let (copying, trimming) = match &self.drag {
            Some(Drag::Move(m)) if m.copy => (Some(m.clips.as_slice()), None),
            Some(Drag::Trim(e) | Drag::Loop(e)) => (None, Some(e.clip)),
            _ => (None, None),
        };
        let lanes = g.lanes().intersect(g.tracks);
        let mut ghosts = Vec::new();
        for i in g.visible_rows() {
            let track = &project.tracks[i];
            if !g.rows[i].takes_clips() {
                continue;
            }
            let implied = gates
                .as_ref()
                .is_some_and(|gt| !track.channel.solo && gt.output(track.id).solo_muted);
            let track_muted = track.channel.mute || implied;
            let palette = self.palettes.get(track.hue.0);
            for clip in &track.clips {
                if clip.end() < t0 {
                    continue;
                }
                if clip.start > t1 {
                    break;
                }
                let rect = g.clip_rect(i, clip);
                if copying.is_some_and(|c| c.contains(&clip.id)) {
                    ghosts.push(rect);
                    continue;
                }
                let selected = ctx.ui.selection.clips.contains(&clip.id);
                let state = ClipState {
                    hovered: hover.is_some_and(|h| h.clip == clip.id),
                    selected,
                    trim: (trimming == Some(clip.id)).then_some(ClipEdge::End),
                };
                let preview = self.previews.get(clip, ppt, body_height(rect.height()));
                let audio = self.audio.get(
                    clip,
                    project.tempo,
                    ctx.project_dir,
                    ppt,
                    body_height(rect.height()),
                    ui.ctx(),
                );
                let message = audio
                    .as_ref()
                    .map_or_else(String::new, |(state, _)| match state {
                        AudioFileState::Missing => {
                            if let ClipContent::Audio(a) = &clip.content {
                                ctx.lang.audio_clip_missing(
                                    a.file.abs.file_name().unwrap_or_default().to_string_lossy(),
                                )
                            } else {
                                String::new()
                            }
                        }
                        AudioFileState::Pending => ctx.lang.audio_clip_peaks().to_owned(),
                        AudioFileState::Ready => String::new(),
                    });
                let audio_look = audio.and_then(|(file, wave)| {
                    let ClipContent::Audio(a) = &clip.content else {
                        return None;
                    };
                    Some(AudioLook {
                        takes: (a.takes.len() > 1).then_some((a.active_take + 1, a.takes.len())),
                        file,
                        wave,
                        repitch: (a.time_mode == komponist_model::ClipTimeMode::RePitch)
                            .then_some(a.original_tempo.bpm),
                        repitch_label: ctx.lang.audio_clip_repitch_badge(),
                        message: &message,
                        gain_db: a.gain_db,
                        fade_in: a.fade_in.0 as f32 * ppt,
                        fade_out: a.fade_out.0 as f32 * ppt,
                    })
                });
                let model = ClipModel {
                    name: &clip.name,
                    palette,
                    muted: clip.mute || track_muted,
                    recording: false,
                    loop_mark: clip.loop_length.is_some(),
                    audio: audio_look.as_ref(),
                    preview: Some(preview),
                };
                paint_clip(painter, rect, &model, state);
                if selected {
                    paint_selected_edge(painter, rect);
                }
                let access = clip_access(
                    komponist_ui::lang(ui.ctx()),
                    &model,
                    clip.start,
                    clip.end(),
                    project.meter,
                    state,
                );
                let node = ui.interact(
                    rect.intersect(lanes),
                    Id::unique(("komponist_arrangement_clip", clip.id.0)),
                    Sense::hover(),
                );
                register(&node, &access);
                if let ClipContent::Audio(audio) = &clip.content {
                    audio_clip_menu(ui, ctx, clip, audio, &node);
                }
                if let Some(audio) = audio_look.as_ref() {
                    let parts = komponist_ui::timeline::audio_parts(
                        rect,
                        audio.fade_in,
                        audio.fade_out,
                        audio.gain_db,
                    );
                    if state.selected || state.hovered {
                        let ms = |px: f32| {
                            format!(
                                "{:.0} ms",
                                f64::from(px / ppt) / komponist_model::PPQ as f64 * 60000.0
                                    / project.tempo.bpm
                            )
                        };
                        let trim_w = komponist_ui::widgets::metrics::timeline::CLIP_HANDLE_WIDTH
                            .min(rect.width() / 3.0);
                        for (key, r, name, value) in [
                            (
                                "fade-in",
                                parts.fade_in,
                                ctx.lang.audio_clip_fade_in(),
                                ms(audio.fade_in),
                            ),
                            (
                                "fade-out",
                                parts.fade_out,
                                ctx.lang.audio_clip_fade_out(),
                                ms(audio.fade_out),
                            ),
                            (
                                "gain",
                                parts.gain_band,
                                ctx.lang.audio_clip_gain_line(),
                                format!("{:.1} dB", audio.gain_db).replace('-', "−"),
                            ),
                            (
                                "start",
                                Rect::from_min_max(
                                    rect.min,
                                    pos2(rect.left() + trim_w, rect.bottom()),
                                ),
                                ctx.lang.audio_clip_start(),
                                komponist_model::format::fmt_pos(clip.start, project.meter),
                            ),
                            (
                                "end",
                                Rect::from_min_max(
                                    pos2(rect.right() - trim_w, rect.top()),
                                    rect.max,
                                ),
                                ctx.lang.audio_clip_end(),
                                komponist_model::format::fmt_pos(clip.end(), project.meter),
                            ),
                        ] {
                            let access =
                                komponist_ui::timeline::Access::new(egui::Role::Slider, name)
                                    .with_value(value);
                            let response = ui.interact(
                                r.intersect(lanes),
                                Id::unique(("audio-control", clip.id.0, key)),
                                Sense::hover(),
                            );
                            register(&response, &access);
                        }
                    }
                    if hover.is_some_and(|h| h.clip == clip.id)
                        && ui
                            .input(|i| i.pointer.hover_pos())
                            .is_some_and(|p| p.y < parts.body.top())
                        && let Some(orig) = audio.repitch
                    {
                        tooltip::show_value(
                            ui.ctx(),
                            Id::unique(("audio-repitch", clip.id.0)),
                            rect,
                            Placement::Above,
                            &ctx.lang.audio_clip_repitch_tip(orig, project.tempo.bpm),
                        );
                    }
                }
            }
            // Audio capture is drawn directly from bounded writer telemetry. Never request
            // peaks from an unfinished WAV; the current lap may contain only a partial bucket.
            if let Some(recording) = ctx.audio_recording
                && let Some(rec) = recording.clip(track.id)
                && let Some(progress) = recording.progress(track.id)
                && rec.end() >= t0
                && rec.start <= t1
            {
                let rect = g.clip_rect(i, rec);
                let data = crate::views::audio_editor::live::summaries(
                    &progress.waveform,
                    progress.channels,
                );
                let wave = komponist_ui::timeline::WaveMesh::from_summaries(
                    &data,
                    &komponist_ui::timeline::WaveSpec {
                        width: (rect.width()
                            - 2.0 * komponist_ui::widgets::metrics::timeline::CLIP_INSET_X)
                            .max(0.0),
                        px_per_tick: ppt,
                        body_height: body_height(rect.height()),
                        offset: 0,
                        frames: progress.frames,
                        gain: 1.0,
                        fade_in: 0.0,
                        fade_out: 0.0,
                    },
                );
                let audio = AudioLook {
                    file: AudioFileState::Ready,
                    wave: &wave,
                    repitch: None,
                    repitch_label: "",
                    message: "",
                    gain_db: 0.0,
                    fade_in: 0.0,
                    fade_out: 0.0,
                };
                let model = ClipModel {
                    name: rec.name.trim_start_matches("● "),
                    palette,
                    muted: false,
                    recording: true,
                    loop_mark: false,
                    audio: Some(&audio),
                    preview: None,
                };
                paint_clip(painter, rect, &model, ClipState::REST);
                let access = clip_access(
                    ctx.lang,
                    &model,
                    rec.start,
                    rec.end(),
                    project.meter,
                    ClipState::REST,
                );
                let node = ui.interact(
                    rect.intersect(lanes),
                    Id::unique(("arrangement-audio-recording", track.id.0)),
                    Sense::hover(),
                );
                register(&node, &access);
            }
            // The take being recorded on this track (S02): red head, growing with the playhead.
            if let Some(rec) = ctx.recording.and_then(|r| r.clip(track.id))
                && rec.end() >= t0
                && rec.start <= t1
            {
                let rect = g.clip_rect(i, rec);
                let preview = PreviewMesh::from_clip(rec, ppt, body_height(rect.height()));
                let model = ClipModel {
                    name: &rec.name,
                    palette,
                    muted: false,
                    recording: true,
                    loop_mark: false,
                    audio: None,
                    preview: Some(&preview),
                };
                paint_clip(painter, rect, &model, ClipState::REST);
                let access = clip_access(
                    komponist_ui::lang(ui.ctx()),
                    &model,
                    rec.start,
                    rec.end(),
                    project.meter,
                    ClipState::REST,
                );
                let node = ui.interact(
                    rect.intersect(lanes),
                    Id::unique(("komponist_arrangement_recording", track.id.0)),
                    Sense::hover(),
                );
                register(&node, &access);
            }
        }
        for r in ghosts {
            paint_clip_ghost(painter, r);
        }

        if let Some(Drag::Move(m)) = &self.drag
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && let Some(row) = g.row_at(p.y)
            && let Some(grabbed) = m.source.tracks.get(m.grab_row)
            && project.tracks[row].kind != grabbed.kind
        {
            ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
            let reason = if project.tracks[row].is_bus() {
                ctx.lang.routing_no_clips()
            } else if grabbed.kind == komponist_model::TrackKind::Audio {
                ctx.lang.status_drop_refused_midi()
            } else {
                ctx.lang.status_drop_refused_audio()
            };
            tooltip::show_value(
                ui.ctx(),
                Id::unique("audio-refused-kind"),
                g.lane(row),
                Placement::Above,
                reason,
            );
        }

        // A clip dragged over a bus row is refused: it stays on the nearest track, and the pointer
        // says why.
        if let Some(Drag::Move(_)) = &self.drag
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && let Some(i) = g.bus_row_at(p.y)
        {
            let r = Rect::from_x_y_ranges(Rangef::point(p.x), g.rows[i].y);
            tooltip::show_value(
                ui.ctx(),
                Id::unique("komponist_arrangement_no_clips"),
                r,
                Placement::Above,
                ctx.lang.routing_no_clips(),
            );
        }

        if let Some(Drag::Trim(e)) = &self.drag
            && let Some((_, clip)) = project.find_clip(e.clip)
            && let ClipContent::Audio(a) = &clip.content
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
            && e.end.0 as f64 + f64::from(p.x - e.origin) / f64::from(ppt)
                > (e.start + a.max_length(project.tempo)).0 as f64
        {
            ui.ctx().set_cursor_icon(CursorIcon::NotAllowed);
            if let Some(i) = project
                .tracks
                .iter()
                .position(|t| t.clips.iter().any(|c| c.id == e.clip))
            {
                let r = g.clip_rect(i, clip);
                px::vline(painter, r.right(), r.y_range(), color32(color::STATE_ERROR));
                tooltip::show_value(
                    ui.ctx(),
                    Id::unique("audio-eof"),
                    r,
                    Placement::Above,
                    ctx.lang.audio_clip_end_of_file(),
                );
            }
        }

        // Show the material the move/copy will cut, using the immutable gesture-start document.
        if let Some(Drag::Move(m)) = &self.drag
            && let Some((delta, rows)) = m.last
        {
            for id in &m.clips {
                if let Some((from, clip)) = m.source.find_clip(*id)
                    && matches!(clip.content, ClipContent::Audio(_))
                    && let Some(row) = m
                        .source
                        .tracks
                        .iter()
                        .position(|t| t.id == from.id)
                        .and_then(|i| usize::try_from(i as i32 + rows).ok())
                    && let Some(track) = m.source.tracks.get(row)
                {
                    let start = clip.start + delta;
                    let end = clip.end() + delta;
                    for below in &track.clips {
                        if m.clips.contains(&below.id) {
                            continue;
                        }
                        let left = start.max(below.start);
                        let right = end.min(below.end());
                        if left < right {
                            let r = g.clip_rect(row, below).intersect(Rect::from_x_y_ranges(
                                Rangef::new(g.axis.x(left), g.axis.x(right)),
                                g.lane(row).y_range(),
                            ));
                            px::fill(
                                painter,
                                r,
                                0.0,
                                color32(color::ACCENT).gamma_multiply(
                                    komponist_ui::widgets::metrics::timeline::CUT_SHADE_ALPHA,
                                ),
                            );
                            px::vline(painter, r.left(), r.y_range(), color32(color::ACCENT));
                            tooltip::show_value(
                                ui.ctx(),
                                Id::unique("audio-overlap"),
                                r,
                                Placement::Above,
                                ctx.lang.audio_clip_cuts_below(),
                            );
                        }
                    }
                }
            }
        }

        // The length while an edge is dragged ("Länge im Tooltip").
        if let Some(Drag::Trim(e) | Drag::Loop(e)) = &self.drag
            && let Some(i) = project
                .tracks
                .iter()
                .position(|t| t.clips.iter().any(|c| c.id == e.clip))
            && let Some((_, clip)) = project.find_clip(e.clip)
        {
            let r = g.clip_rect(i, clip);
            let edge = Rect::from_x_y_ranges(Rangef::point(r.right()), r.y_range());
            tooltip::show_value(
                ui.ctx(),
                Id::unique("komponist_arrangement_length"),
                edge,
                Placement::Above,
                &fmt_length(e.length, project.meter),
            );
        }

        // Box selection: `canvas.box-select` with a 1 px accent edge, as in the piano roll
        // (`ks-view.js` `prView` `box`).
        if let Some(Drag::Box { origin, .. }) = &self.drag
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            let area = Rect::from_two_pos(*origin, pos).intersect(lanes);
            if area.is_positive() {
                px::fill(painter, area, 0.0, color32(color::CANVAS_BOX_SELECT));
                px::inner_border(painter, area, color32(color::ACCENT));
            }
        }

        // The playhead over the lanes.
        let ph = ctx.engine.playhead();
        if ph >= Tick::ZERO {
            let x = g.axis.x(ph);
            if g.lane_x.contains(x) {
                paint_playhead(
                    painter,
                    x,
                    Rangef::new(g.tracks.top() - 1.0, g.tracks.bottom()),
                );
            }
        }
    }
}

/// The audio clip's approved context menu. The canvas retains primary drag handling;
/// a secondary click explicitly opens the stable clip node's menu.
fn audio_clip_menu(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    clip: &komponist_model::Clip,
    audio: &komponist_model::AudioClip,
    node: &egui::Response,
) {
    use crate::actions::ProjectAction;
    use komponist_ui::widgets::context_menu::{ContextMenu, Entry};
    if ui.input(|i| i.pointer.secondary_clicked())
        && ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| node.rect.contains(p))
    {
        let pos = ui.input(|i| i.pointer.interact_pos()).unwrap();
        ContextMenu::<Action>::open_at(ui.ctx(), node.id, pos);
        if !ctx.ui.selection.clips.contains(&clip.id) {
            ctx.send(select_clips(vec![clip.id], SelectMode::Replace));
        }
    }
    let path = audio
        .file
        .resolve(ctx.project_dir)
        .unwrap_or_else(|| audio.file.abs.clone());
    let folder = path.parent().map(ToOwned::to_owned);
    let show = Entry::action(
        ctx.lang.audio_file_show(),
        Action::Project(ProjectAction::ShowInFolder(
            folder.clone().unwrap_or_default(),
        )),
    );
    let show = if folder.is_some() {
        show
    } else {
        show.disabled()
    };
    let split = Entry::action(
        ctx.lang.audio_clip_context_split(),
        Action::Edit(EditAction::SplitAtPlayhead),
    );
    let split = if ctx.engine.playhead() > clip.start && ctx.engine.playhead() < clip.end() {
        split
    } else {
        split.disabled()
    };
    let entries = [
        Entry::action(
            ctx.lang.audio_clip_context_mute(),
            Action::Edit(EditAction::SetClipsMuted {
                clips: vec![clip.id],
                muted: !clip.mute,
            }),
        )
        .checked(clip.mute),
        Entry::action(
            ctx.lang.audio_clip_context_duplicate(),
            Action::Edit(EditAction::DuplicateSelection),
        ),
        split,
        Entry::separator(),
        Entry::action(
            ctx.lang.audio_file_replace(),
            Action::Project(ProjectAction::ReplaceAudio(clip.id)),
        ),
        show,
        Entry::separator(),
        Entry::action(
            ctx.lang.arrangement_delete(),
            Action::Edit(EditAction::DeleteSelection),
        ),
    ];
    if let Some(action) =
        ContextMenu::new(ctx.lang.audio_clip_context_menu(), &entries).show_on(node)
    {
        ctx.send(action);
    }
}

/// The new length of the clip of edge drag `e` with the pointer at `x`, if it changed (then also
/// stored in `e`).
fn edge_length(
    e: &mut EdgeDrag,
    x: f32,
    ppt: f64,
    view: &ArrangementView,
    free: bool,
    project: &Project,
) -> Option<Tick> {
    let raw = e.end.0 as f64 + f64::from(x - e.origin) / ppt;
    let end = snap_tick(raw, view, free, project);
    let length = (end - e.start).max(min_length(view, free, project));
    (length != e.length).then(|| {
        e.length = length;
        length
    })
}

/// The pointer, if it is over `rect` and no popup covers it. (The clip nodes on top of the
/// canvas sense hover only, so the canvas response's own hover cannot be trusted.)
fn pointer_over(ui: &Ui, rect: Rect) -> Option<Pos2> {
    ui.input(|i| i.pointer.hover_pos())
        .filter(|_| ui.rect_contains_pointer(rect))
}

/// `SelectMode` of a click on a clip: Shift adds (the clip has no range), Strg toggles.
fn clip_mode(mode: SelectMode) -> SelectMode {
    match mode {
        SelectMode::Extend => SelectMode::Add,
        m => m,
    }
}

/// The right edge of a selected clip, where its length is dragged (`clipView` `edge`: 6 px,
/// `text.1` at 16 %, inside the clip's rounded corners).
fn paint_selected_edge(painter: &Painter, clip: Rect) {
    let ppp = painter.pixels_per_point();
    let r = px::snap_rect(clip, ppp);
    let w = metrics::CLIP_EDGE_WIDTH.min(r.width() / 2.0);
    let edge = Rect::from_min_max(pos2(r.right() - w, r.top()), r.max);
    let k = radius::M as u8;
    painter.rect_filled(
        edge,
        CornerRadius {
            nw: 0,
            ne: k,
            sw: 0,
            se: k,
        },
        color32(color::CANVAS_CLIP_EDGE_SELECTED),
    );
}

/// Lane fills (selected tracks `canvas.lane-selected`, else the panel's `surface.1`), the bar
/// grid over lanes and "Neue Spur", and the 1 px `surface.0` line under each lane.
fn paint_backgrounds(painter: &Painter, ctx: &ViewCtx<'_>, project: &Project, g: &Geometry) {
    for i in g.visible_rows() {
        if ctx.ui.selection.tracks.contains(&project.tracks[i].id) {
            px::fill(
                painter,
                g.lane(i),
                0.0,
                color32(color::CANVAS_LANE_SELECTED),
            );
        }
    }
    let spec = GridSpec::new(ctx.ui.arrangement.grid, metrics::GRID_MIN_SPACING);
    let lines = grid_lines(&g.axis, project.meter, g.lane_x, &spec);
    paint_grid(painter, g.lanes(), &lines);
    for i in g.visible_rows() {
        let y = g.rows[i].y.max - px::line_width(painter.pixels_per_point());
        px::hline(painter, g.lane_x, y, color32(color::SURFACE_0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    #[test]
    fn snapping_follows_the_view() {
        let p = demo_project();
        let mut v = ArrangementView::default();
        assert_eq!(snap_tick(3000.0, &v, false, &p), Tick(3840));
        assert_eq!(snap_tick(3000.0, &v, true, &p), Tick(3000));
        v.grid = Grid::Quarter;
        assert_eq!(snap_tick(3000.0, &v, false, &p), Tick(2880));
        v.snap = false;
        assert_eq!(snap_tick(3000.0, &v, false, &p), Tick(3000));
        assert_eq!(min_length(&v, false, &p), Tick(240));
    }

    #[test]
    fn new_clips_follow_the_instrument() {
        let p = demo_project();
        let v = ArrangementView::default();
        // Drums has the Sampler: a step clip with the eight pads from note 36.
        let c = new_clip(Lang::De, &p, &p.tracks[0], Tick(5 * 3840 + 100), &v, &[]);
        assert_eq!(c.start, Tick(5 * 3840));
        assert_eq!(c.length, Tick(3840));
        assert_eq!(c.name, "Pattern 5");
        let NewClipContent::Step { rows } = &c.content else {
            panic!("step clip expected");
        };
        assert_eq!(rows.len(), 8);
        assert_eq!((rows[0].name.as_str(), rows[0].note), ("Kick", 36));
        assert_eq!((rows[7].name.as_str(), rows[7].note), ("Perc", 43));
        // The names of the pads that hold a sample (the device host reports them as note names)
        // replace the defaults; the other pads keep theirs.
        let names: Vec<komponist_ipc::NoteName> = [(36, "Bassdrum"), (38, "Clap")]
            .into_iter()
            .map(|(key, name)| komponist_ipc::NoteName {
                key,
                channel: None,
                name: name.into(),
            })
            .collect();
        let c = new_clip(Lang::De, &p, &p.tracks[0], Tick(0), &v, &names);
        let NewClipContent::Step { rows } = &c.content else {
            panic!("step clip expected");
        };
        let rows: Vec<(&str, u8)> = rows.iter().map(|r| (r.name.as_str(), r.note)).collect();
        assert_eq!(rows.len(), 8);
        assert_eq!(
            rows[..4],
            [
                ("Bassdrum", 36),
                ("Snare", 37),
                ("Clap", 38),
                ("Hat zu", 39)
            ]
        );
        // Lead has hexa: an empty MIDI clip.
        let c = new_clip(Lang::De, &p, &p.tracks[2], Tick(3840 * 2 + 10), &v, &[]);
        assert_eq!((c.start, c.name.as_str()), (Tick(2 * 3840), "MIDI-Clip"));
        assert!(matches!(c.content, NewClipContent::Midi { ref notes, .. } if notes.is_empty()));
    }

    #[test]
    fn new_step_clips_name_their_rows_in_the_language() {
        let p = demo_project();
        let v = ArrangementView::default();
        let c = new_clip(Lang::En, &p, &p.tracks[0], Tick(0), &v, &[]);
        assert_eq!(c.name, "Pattern 5", "after four clips");
        let NewClipContent::Step { rows } = &c.content else {
            panic!("step clip expected");
        };
        assert_eq!((rows[3].name.as_str(), rows[3].note), ("Closed Hat", 39));
        assert_eq!((rows[4].name.as_str(), rows[4].note), ("Open Hat", 40));
    }
}
