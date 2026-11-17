//! Track headers (`ix.header`, `ix.toggle`): painted with `paint_track_header`, one interaction
//! area each (select, rename, reorder), the WP-20 track toggles and the WP-21 header meter on
//! their painted places, and a grip on the bottom edge for the height.

use std::sync::Arc;

use egui::{
    Align, CursorIcon, Id, Layout, Rangef, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2,
};
use komponist_i18n::Lang;
use komponist_model::edits::{ChannelProp, ExclusiveFlag, TrackProp};
use komponist_model::{Project, Track, TrackId, routing};
use komponist_plugins::{DeviceStatus, FailReason};
use komponist_tokens::typography;
use komponist_ui::State;
use komponist_ui::timeline::{HeaderModel, header_access, header_layout, paint_track_header};
use komponist_ui::widgets::meter::{Meter, MeterState};
use komponist_ui::widgets::metrics::timeline as tm;
use komponist_ui::widgets::rename_field::{RenameField, RenameOutcome, RenameState};
use komponist_ui::widgets::toggle::{ToggleEvent, ToggleKind, TrackToggle};
use komponist_ui::{fonts, px};

use super::buses;
use super::geometry::{Geometry, RowKind, row_height, stored_height};
use super::{
    Arrangement, Drag, Rename, click_mode, metrics, register, select_clips, select_tracks,
};
use crate::actions::{Action, EditAction, SelectMode, UiAction};
use crate::engine_sync::EngineSync;
use crate::meters::Meters;
use crate::views::ViewCtx;

/// The second line of a header and whether it is an error: "stumm durch Solo" while muted by a
/// solo elsewhere (`any_solo`: the track's output is solo-muted, `routing::gates`); else the instrument ("Hexa", "Surge XT · lädt …", "Dexed · aus",
/// "⚠ Plugin fehlt: Vital"), or "Instrument hierher ziehen" without one.
pub fn instrument_line(
    lang: Lang,
    track: &Track,
    any_solo: bool,
    engine: &EngineSync,
) -> (String, bool) {
    let ch = &track.channel;
    if any_solo && !ch.solo && !ch.mute {
        return (lang.arrangement_implied_mute().to_owned(), false);
    }
    let Some((_, device)) = ch.instrument() else {
        return (lang.arrangement_no_instrument().to_owned(), false);
    };
    let name = &crate::views::plugin_name(lang, &device.plugin);
    match engine.device_status(device.id) {
        DeviceStatus::Missing => (lang.arrangement_plugin_missing(name), true),
        DeviceStatus::Crashed(FailReason::Exited { .. }) => {
            (lang.arrangement_plugin_crashed(name), true)
        }
        DeviceStatus::Crashed(FailReason::LoadFailed(_)) => {
            (lang.arrangement_plugin_load_failed(name), true)
        }
        DeviceStatus::Crashed(FailReason::Unresponsive) => {
            (lang.arrangement_plugin_unresponsive(name), true)
        }
        DeviceStatus::Loading => (lang.arrangement_plugin_loading(name), false),
        DeviceStatus::Ok if device.bypass => (lang.arrangement_plugin_bypassed(name), false),
        DeviceStatus::Ok => (name.clone(), false),
    }
}

/// The track property a toggle switches.
pub fn toggle_prop(kind: ToggleKind, value: bool) -> TrackProp {
    match kind {
        ToggleKind::Mute => TrackProp::Channel(ChannelProp::Mute(value)),
        ToggleKind::Solo => TrackProp::Channel(ChannelProp::Solo(value)),
        ToggleKind::Arm => TrackProp::Arm(value),
        ToggleKind::Monitor => TrackProp::Monitor(value),
    }
}

/// Whether toggle `kind` of `track` is on.
pub fn toggle_on(track: &Track, kind: ToggleKind) -> bool {
    match kind {
        ToggleKind::Mute => track.channel.mute,
        ToggleKind::Solo => track.channel.solo,
        ToggleKind::Arm => track.arm,
        ToggleKind::Monitor => track.monitor,
    }
}

/// The name line of a header at `rect` (where the rename field goes): the text column, one
/// `type.track` line high, as `paint_track_header` centres name and instrument.
fn name_rect(rect: Rect, ppp: f32) -> Rect {
    let l = header_layout(rect);
    let block = typography::TRACK.line_height + tm::HEADER_TEXT_GAP + typography::SMALL.line_height;
    let top = px::snap(l.body.center().y - block / 2.0, ppp);
    Rect::from_min_max(
        pos2(l.text.left(), top),
        pos2(l.text.right(), top + typography::TRACK.line_height),
    )
}

impl Arrangement {
    /// The headers of the visible rows, then their height grips (on top of the next header's
    /// first pixels).
    pub(super) fn headers(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Arc<Project>,
        g: &Geometry,
    ) {
        let any_solo = project.any_solo();
        // Mute and solo follow the signal path: a group stays audible while one of its members
        // is soloed (`routing::gates`).
        let gates = any_solo.then(|| routing::gates(project));
        let mods = ui.input(|i| i.modifiers);
        let rows: Vec<usize> = g.visible_rows().collect();
        for &i in &rows {
            self.header(ui, ctx, project, g, i, gates.as_ref(), mods);
        }
        for &i in &rows {
            if g.rows[i].takes_clips() {
                self.height_grip(ui, ctx, project, g, i, mods.alt);
            }
        }
        self.section_label(ui, ctx.lang, g);
        if let Some(Drag::Reorder { .. }) = self.drag {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn header(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
        i: usize,
        gates: Option<&routing::Gates>,
        mods: egui::Modifiers,
    ) {
        let track = &project.tracks[i];
        let row = g.rows[i];
        let rect = g.header(i);
        let ppp = ui.pixels_per_point();
        let response = ui.interact(
            rect,
            Id::unique(("komponist_arrangement_header", track.id.0)),
            Sense::click_and_drag(),
        );
        let selected = ctx.ui.selection.tracks.contains(&track.id);
        let state = State {
            hovered: self.drag.is_none() && ui.rect_contains_pointer(rect),
            pressed: response.is_pointer_button_down_on() && self.drag.is_none(),
            focused: false,
            selected,
            disabled: false,
        };
        let implied_mute =
            gates.is_some_and(|g| !track.channel.solo && g.output(track.id).solo_muted);
        let l = header_layout(rect);
        let name = if track.is_bus() {
            self.bus_header(
                ui,
                ctx,
                project,
                track,
                row.kind,
                rect,
                state,
                implied_mute,
                &response,
            );
            buses::bus_name_rect(rect, ppp, row.kind)
        } else {
            let (instrument, error) = if track.is_audio() {
                (String::new(), false)
            } else {
                instrument_line(ctx.lang, track, implied_mute, ctx.engine)
            };
            if row.depth > 0 {
                buses::paint_brackets(ui.painter(), project, g, i, &mut self.palettes);
            }
            let model = HeaderModel {
                name: &track.name,
                instrument: &instrument,
                instrument_error: error,
                palette: self.palettes.get(track.hue.0),
                mute: track.channel.mute,
                solo: track.channel.solo,
                arm: track.arm,
                monitor: if track.is_audio() {
                    crate::views::audio_inputs::monitoring(track)
                } else {
                    track.monitor
                },
                implied_mute,
                meter: None,
            };
            paint_track_header(ui.painter(), rect, &model, state);
            let access = header_access(komponist_ui::lang(ui.ctx()), &model, state);
            register(&response, &access);
            if track.is_audio() {
                let name = name_rect(rect, ppp);
                let input = if rect.height() >= crate::views::audio_inputs::INPUT_LINE_MIN {
                    Rect::from_min_size(
                        pos2(l.text.left(), name.bottom() + tm::HEADER_TEXT_GAP),
                        egui::vec2(l.text.width(), typography::SMALL.line_height),
                    )
                } else {
                    Rect::from_center_size(
                        pos2(
                            l.toggles[0].left() - komponist_tokens::size::ICON / 2.0,
                            rect.center().y,
                        ),
                        egui::vec2(komponist_tokens::size::ICON, komponist_tokens::size::ICON),
                    )
                };
                crate::views::audio_inputs::input_button(ui, ctx, project, track, input);
                if rect.height() >= crate::views::audio_inputs::INPUT_LINE_MIN
                    && (track.arm || crate::views::audio_inputs::monitoring(track))
                {
                    let bar = Rect::from_min_size(
                        pos2(
                            input.left(),
                            input.bottom() + komponist_tokens::space::SPACE_0_5,
                        ),
                        egui::vec2(
                            crate::views::audio_inputs::HEADER_INPUT_WIDTH.min(input.width()),
                            crate::views::audio_inputs::INPUT_BAR,
                        ),
                    );
                    crate::views::audio_inputs::input_meter(ui, ctx, track, bar);
                }
            }
            name_rect(rect, ppp)
        };

        // Select, rename, reorder.
        if response.double_clicked()
            && response.interact_pointer_pos().is_some_and(|p| {
                name.contains(p) || (!track.is_audio() && header_layout(rect).text.contains(p))
            })
        {
            self.rename = Some(Rename {
                track: track.id,
                edit: RenameState::new(&track.name),
            });
        } else if response.clicked() {
            let mode = click_mode(mods);
            if mode == SelectMode::Replace {
                ctx.send(select_clips(Vec::new(), SelectMode::Replace));
            }
            ctx.send(select_tracks(vec![track.id], mode));
        }
        if response.drag_started() && row.kind != RowKind::EffectBus {
            let tracks: Vec<TrackId> = if selected {
                project
                    .tracks
                    .iter()
                    .map(|t| t.id)
                    .filter(|t| ctx.ui.selection.tracks.contains(t))
                    .collect()
            } else {
                ctx.send(select_tracks(vec![track.id], SelectMode::Replace));
                vec![track.id]
            };
            self.drag = Some(Drag::Reorder {
                tracks,
                slot: None,
                onto: None,
            });
        }
        if response.dragged()
            && let Some(Drag::Reorder { tracks, slot, onto }) = &mut self.drag
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            *onto = g.drop_onto_at(p.y).filter(|b| !tracks.contains(b));
            *slot = onto.is_none().then(|| g.insert_slot(p.y));
        }
        if response.drag_stopped() {
            self.finish_drag(ctx);
        }
        if let Some(action) = self.header_menu(ctx, project, track, &response) {
            action.apply(self, ctx, track);
        }

        if track.is_bus() {
            // Toggles and meter of a bus header: M and S only, right-aligned.
            let [_, _, m, s] = l.toggles;
            let rects = [(ToggleKind::Mute, m), (ToggleKind::Solo, s)];
            self.toggles(ui, ctx, project, g, track, &rects, implied_mute);
            let record = l.toggles[0].union(l.toggles[1]);
            crate::views::audio_inputs::record_bus_button(ui, ctx, track, record, true);
        } else {
            let rects: Vec<(ToggleKind, Rect)> =
                ToggleKind::ALL.into_iter().zip(l.toggles).collect();
            self.toggles(ui, ctx, project, g, track, &rects, implied_mute);
        }
        for action in header_meter(ui, ctx.lang, ctx.meters, track, l.meter, mods.alt) {
            ctx.send(action);
        }

        if self.rename.as_ref().is_some_and(|r| r.track == track.id) {
            self.rename_field(ui, ctx, track, name);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn toggles(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
        track: &Track,
        rects: &[(ToggleKind, Rect)],
        implied_mute: bool,
    ) {
        for (k, &(kind, rect)) in rects.iter().enumerate() {
            if track.is_audio() && kind == ToggleKind::Monitor {
                crate::views::audio_inputs::monitor_button(ui, ctx, track, rect);
                continue;
            }
            let mut child = ui.new_child(
                UiBuilder::new()
                    .max_rect(rect)
                    .id_salt(("komponist_arrangement_toggle", track.id.0, k))
                    .layout(Layout::left_to_right(Align::Min)),
            );
            let out = TrackToggle::new(kind, toggle_on(track, kind))
                .implied(match kind {
                    ToggleKind::Mute => implied_mute,
                    ToggleKind::Monitor => track.arm && !track.monitor,
                    ToggleKind::Solo | ToggleKind::Arm => false,
                })
                .track(&track.name)
                .show(&mut child);
            match out.event {
                Some(ToggleEvent::Set {
                    exclusive: true, ..
                }) => {
                    let flag = if kind == ToggleKind::Solo {
                        ExclusiveFlag::Solo
                    } else {
                        ExclusiveFlag::Arm
                    };
                    ctx.send(Action::Edit(EditAction::SetExclusive {
                        track: track.id,
                        flag,
                    }));
                }
                Some(ToggleEvent::Set { value, .. }) => {
                    if !self.toggle_gesture {
                        ctx.send(Action::Edit(EditAction::BeginGesture {
                            label: "undo-toggle-tracks".to_owned(),
                        }));
                        self.toggle_gesture = true;
                    }
                    ctx.send(Action::Edit(EditAction::SetTrackProp {
                        tracks: vec![track.id],
                        prop: toggle_prop(kind, value),
                    }));
                }
                Some(ToggleEvent::Sweep { value, from, to }) => {
                    // "Ziehen über Spuren": every track whose row the pointer crossed.
                    let span = Rangef::new(from.y.min(to.y), from.y.max(to.y));
                    let tracks: Vec<TrackId> = project
                        .tracks
                        .iter()
                        .zip(&g.rows)
                        .filter(|(t, r)| {
                            r.shown()
                                && r.y.min <= span.max
                                && span.min < r.y.max
                                && toggle_on(t, kind) != value
                                && !(t.is_bus()
                                    && matches!(kind, ToggleKind::Arm | ToggleKind::Monitor))
                        })
                        .map(|(t, _)| t.id)
                        .collect();
                    if !tracks.is_empty() {
                        ctx.send(Action::Edit(EditAction::SetTrackProp {
                            tracks,
                            prop: toggle_prop(kind, value),
                        }));
                    }
                }
                None => {}
            }
        }
    }

    /// The text field of a header being renamed: over the name, `surface.0` with an accent ring;
    /// Enter (or a click elsewhere) applies, Esc discards.
    fn rename_field(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, track: &Track, rect: Rect) {
        let Some(rename) = self.rename.as_mut() else {
            return;
        };
        let id = Id::unique(("komponist_arrangement_rename", track.id.0));
        let font = fonts::font_id(&typography::TRACK);
        let label = ctx.lang.arrangement_rename();
        match RenameField::new(&mut rename.edit, id, label, &track.name, font).show(ui, rect) {
            RenameOutcome::Editing => {}
            RenameOutcome::Commit(name) => {
                ctx.send(Action::Edit(EditAction::SetTrackProp {
                    tracks: vec![track.id],
                    prop: TrackProp::Name(name),
                }));
                self.rename = None;
            }
            RenameOutcome::Cancel => self.rename = None,
        }
    }

    /// The bottom edge of header `i`: drag ↕ sets the height (32–160), Alt for all tracks.
    fn height_grip(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        project: &Project,
        g: &Geometry,
        i: usize,
        alt: bool,
    ) {
        let track = &project.tracks[i];
        let row = g.rows[i];
        let grip = Rect::from_x_y_ranges(
            g.header_x,
            Rangef::new(
                row.y.max - metrics::HEIGHT_GRAB,
                row.y.max + metrics::HEIGHT_GRAB,
            ),
        );
        let response = ui.interact(
            grip,
            Id::unique(("komponist_arrangement_height", track.id.0)),
            Sense::drag(),
        );
        let height = row.y.span();
        let grip_name = ctx.lang.arrangement_height_grip(&track.name);
        response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::Splitter, true, &grip_name);
            info.value = Some(f64::from(height));
            info
        });
        if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
        }
        let v_zoom = ctx.ui.arrangement.v_zoom;
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
        {
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: "undo-track-height".to_owned(),
            }));
            self.drag = Some(Drag::Height {
                track: track.id,
                all: alt,
                start: row_height(track.height, v_zoom),
                origin: origin.y,
                last: track.height,
            });
        }
        if response.dragged()
            && let Some(Drag::Height {
                track: id,
                all,
                start,
                origin,
                last,
            }) = &mut self.drag
            && *id == track.id
            && let Some(p) = ui.input(|i| i.pointer.interact_pos())
        {
            let h = stored_height(*start + (p.y - *origin), v_zoom);
            if h != *last {
                *last = h;
                let tracks = if *all {
                    project.tracks.iter().map(|t| t.id).collect()
                } else {
                    vec![*id]
                };
                ctx.send(Action::Edit(EditAction::SetTrackProp {
                    tracks,
                    prop: TrackProp::Height(h),
                }));
            }
        }
        if response.drag_stopped() {
            self.finish_drag(ctx);
        }
    }
}

/// The header meter at `rect` (clickable: resets the track's peak and clip; Alt-click all
/// strips, S04). Returns what to dispatch.
fn header_meter(
    ui: &mut Ui,
    lang: Lang,
    meters: &mut Meters,
    track: &Track,
    rect: Rect,
    alt: bool,
) -> Vec<Action> {
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("komponist_arrangement_meter", track.id.0))
            .layout(Layout::left_to_right(Align::Min)),
    );
    let mut silent = MeterState::new();
    let state = meters.track_mut(track.id).unwrap_or(&mut silent);
    let response = child.add(
        Meter::new(state, Meters::NO_PEAKS, lang.arrangement_meter(&track.name))
            .header()
            .length(rect.height()),
    );
    if response.clicked() && alt {
        vec![Action::Ui(UiAction::ResetMeters {
            track: Some(track.id),
            all: true,
        })]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    #[test]
    fn toggles_map_to_track_props() {
        assert_eq!(
            toggle_prop(ToggleKind::Mute, true),
            TrackProp::Channel(ChannelProp::Mute(true))
        );
        assert_eq!(toggle_prop(ToggleKind::Arm, false), TrackProp::Arm(false));
        let p = demo_project();
        assert!(toggle_on(&p.tracks[5], ToggleKind::Mute), "Arp is muted");
        assert!(!toggle_on(&p.tracks[5], ToggleKind::Solo));
    }

    #[test]
    fn name_field_sits_on_the_name_line() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(224.0, 56.0));
        let n = name_rect(r, 1.0);
        assert_eq!(n.left(), 13.0);
        assert_eq!(n.height(), 16.0);
        // Block of 16 + 1 + 14 centred in the 55 px body: top at 12.
        assert_eq!(n.top(), 12.0);
    }

    #[test]
    fn scrolled_midi_and_audio_clips_cannot_paint_in_group_header_gutters() {
        use komponist_model::edits::{NewAudioTrack, NewBus, NewClip, NewClipContent, NewTrack};
        use komponist_model::{AudioFile, Tick, edits};
        let p = Project::new("Header clipping");
        let (p, bass) = edits::add_track(
            &p,
            NewTrack {
                name: Some("Bass".into()),
                hue: None,
                instrument: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let (p, shaker) = edits::add_audio_track(
            &p,
            NewAudioTrack {
                name: Some("Shaker".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let (p, bass_clip) = edits::add_clip(
            &p,
            bass,
            NewClip {
                start: Tick::ZERO,
                length: Tick(8 * 3840),
                name: "Bass phrase".into(),
                content: NewClipContent::Midi {
                    notes: Vec::new(),
                    take_name: String::new(),
                },
            },
        )
        .unwrap();
        let (p, shaker_clip) = edits::add_clip(
            &p,
            shaker,
            NewClip {
                start: Tick::ZERO,
                length: Tick(8 * 3840),
                name: "Shaker loop".into(),
                content: NewClipContent::Audio {
                    file: AudioFile {
                        abs: "/missing/header-clipping.wav".into(),
                        rel: Default::default(),
                        frames: 48_000 * 16,
                        sample_rate: 48_000,
                        channels: 1,
                    },
                    offset: 0,
                },
            },
        )
        .unwrap();
        let (p, _) = edits::add_bus(&p, NewBus::group(vec![bass, shaker])).unwrap();
        let mut app = crate::App::headless(p);
        app.ui.arrangement.scroll_tick = 4.0 * 3840.0;
        app.ui.selection.tracks.insert(shaker);
        app.ui.selection.clips.extend([bass_clip, shaker_clip]);
        let project = Arc::clone(app.project());
        let context = egui::Context::default();
        komponist_ui::install(&context);
        let mut geometry = None;
        let mut output = context.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    pos2(0.0, 0.0),
                    egui::vec2(900.0, 500.0),
                )),
                ..Default::default()
            },
            |ui| {
                geometry = Some(Geometry::new(ui.max_rect(), &project, &app.ui.arrangement));
                let mut ctx = ViewCtx::new(
                    &project,
                    &mut app.ui,
                    &app.engine,
                    &mut app.meters,
                    &app.keymap,
                    &mut app.queue,
                );
                app.views.arrangement.show(ui, &mut ctx);
            },
        );
        output.textures_delta.clear();
        let g = geometry.unwrap();
        for track in [bass, shaker] {
            let row = project.tracks.iter().position(|t| t.id == track).unwrap();
            assert!(
                g.header(row).left() > g.header_full(row).left(),
                "fixture must be indented"
            );
            let clip = &project.tracks[row].clips[0];
            let r = g.clip_rect(row, clip);
            assert!(
                r.left() < g.header_full(row).left(),
                "fixture must scroll beneath the gutter"
            );
            let mut clipped_meshes = 0;
            for shape in &output.shapes {
                let egui::Shape::Mesh(mesh) = &shape.shape else {
                    continue;
                };
                let in_row = |v: &egui::epaint::Vertex| v.pos.y >= r.top() && v.pos.y <= r.bottom();
                if mesh
                    .vertices
                    .iter()
                    .any(|v| in_row(v) && v.pos.x < g.header(row).left())
                    && mesh
                        .vertices
                        .iter()
                        .any(|v| in_row(v) && v.pos.x > g.lane_x.min)
                {
                    clipped_meshes += 1;
                    assert!(
                        shape.clip_rect.left() >= g.lane_x.min,
                        "{} clip mesh leaks into the ancestor gutter: {:?}",
                        project.tracks[row].name,
                        shape.clip_rect
                    );
                }
            }
            assert!(
                clipped_meshes > 0,
                "fixture must paint a crossing clip mesh"
            );
        }
    }
}
