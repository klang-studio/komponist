//! Full-source waveform dock with per-channel envelopes, editable clip window and file recovery.
pub mod fields;
pub mod live;
pub mod metrics;
pub mod source;
pub mod toolbar;

use self::{metrics as m, source::Source};
use crate::views::{View, ViewCtx};
use crate::{Action, EditAction, TransportAction};
use egui::{
    Align2, CursorIcon, Id, Pos2, Rect, Role, Sense, Stroke, Ui, UiBuilder, WidgetInfo, pos2, vec2,
};
use komponist_model::edits::AudioClipProp;
use komponist_model::{AudioClip, Clip, ClipContent, ClipId, PPQ, Project, Tick, TickRange};
use komponist_tokens::egui_support::color32;
use komponist_tokens::{TrackPalette, color, size, space, typography};
use komponist_ui::timeline::{
    Access, GridSpec, RulerKind, RulerModel, TimeAxis, WaveMesh, WaveSpec, audio_parts, grid_lines,
    paint_grid, paint_playhead, paint_ruler,
};
use komponist_ui::{fonts, px};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Control {
    Start,
    End,
    FadeIn,
    FadeOut,
    Gain,
}
#[derive(Clone, Debug)]
struct Gesture {
    control: Control,
    clip: ClipId,
    origin: Pos2,
    start: Tick,
    length: Tick,
    gain: f32,
    fade_in: Tick,
    fade_out: Tick,
    lane_height: f32,
}
struct Meshes {
    clip: Arc<Clip>,
    peaks: Arc<komponist_audiofile::PeakData>,
    full_spec: WaveSpec,
    window_spec: WaveSpec,
    full: Vec<WaveMesh>,
    window: Vec<WaveMesh>,
}
/// Immutable frame data shared by painting and hit-testing.
struct Frame<'a> {
    project: &'a Project,
    clip: &'a Clip,
    audio: &'a AudioClip,
    axis: TimeAxis,
    source_start: Tick,
    selected: bool,
}
/// Cached waveform source and pointer gesture; document edits go through the normal action dispatcher.
#[derive(Default)]
pub struct AudioEditor {
    shown: Option<ClipId>,
    source: Source,
    meshes: Option<Meshes>,
    gesture: Option<Gesture>,
    last_ppb: Option<f32>,
}
impl View for AudioEditor {
    fn show(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) {
        if ctx.audio_recording.is_some() && self.gesture.take().is_some() {
            ctx.send(Action::Edit(EditAction::CancelGesture));
        }
        if self.live(ui, ctx) {
            return;
        }
        let project = Arc::clone(ctx.project);
        let Some((track, clip)) = ctx.ui.editor.clip.and_then(|id| project.find_clip(id)) else {
            return;
        };
        let ClipContent::Audio(a) = &clip.content else {
            return;
        };
        if self.shown != Some(clip.id) {
            if self.gesture.take().is_some() {
                ctx.send(Action::Edit(EditAction::CancelGesture));
            }
            self.shown = Some(clip.id);
            ctx.ui.waveform.scroll_tick = a.frames_to_ticks(a.offset, project.tempo).0 as f64;
        }
        self.source.refresh(a, ctx, ui.ctx());
        let body = ui.max_rect();
        let toolbar_rect = Rect::from_min_size(body.min, vec2(body.width(), size::PANEL_HEADER));
        toolbar::show(ui, toolbar_rect, ctx, &project, clip, a, &self.source);
        let rest = Rect::from_min_max(pos2(body.left(), toolbar_rect.bottom()), body.max);
        if self.source.file.is_none() {
            self.missing(ui, ctx, clip, a, rest);
            return;
        }
        let gutter = Rect::from_min_max(rest.min, pos2(rest.left() + m::GUTTER, rest.bottom()));
        let ruler = Rect::from_min_max(
            pos2(gutter.right(), rest.top()),
            pos2(rest.right(), rest.top() + m::RULER_H),
        );
        let canvas = Rect::from_min_max(pos2(gutter.right(), ruler.bottom()), rest.max);
        if let Some(previous) = self.last_ppb
            && previous != ctx.ui.waveform.px_per_bar
        {
            let old = previous / project.meter.ticks_per_bar() as f32;
            let new = ctx.ui.waveform.px_per_bar / project.meter.ticks_per_bar() as f32;
            let anchor = ctx.ui.waveform.scroll_tick + f64::from(canvas.width() / 2.0 / old);
            ctx.ui.waveform.scroll_tick = (anchor - f64::from(canvas.width() / 2.0 / new)).max(0.0);
        }
        self.wheel(ui, ctx, canvas, a, &project);
        self.last_ppb = Some(ctx.ui.waveform.px_per_bar);
        let ppt = ctx.ui.waveform.px_per_bar / project.meter.ticks_per_bar() as f32;
        let full_ticks = a.frames_to_ticks(a.file.frames, project.tempo);
        ctx.ui.waveform.scroll_tick = ctx.ui.waveform.scroll_tick.clamp(
            0.0,
            (full_ticks.0 as f64 - f64::from(canvas.width() / ppt)).max(0.0),
        );
        let axis = TimeAxis {
            x0: canvas.left() - ctx.ui.waveform.scroll_tick as f32 * ppt,
            px_per_tick: ppt,
        };
        let source_start = a.frames_to_ticks(a.offset, project.tempo);
        let source_end = source_start + clip.length;
        let palette = TrackPalette::new(track.hue.0);
        let ruler_model = RulerModel {
            axis,
            meter: project.meter,
            kind: RulerKind::Editor {
                clip: TickRange::new(source_start, source_end),
                clip_color: color32(palette.base),
            },
            playhead: None,
            recording: false,
        };
        paint_ruler(ui.painter(), ruler, &ruler_model);
        let channels = usize::from(a.file.channels.min(2));
        let lane_h = canvas.height() / channels as f32;
        let lanes: Vec<Rect> = (0..channels)
            .map(|ch| {
                Rect::from_min_max(
                    pos2(canvas.left(), canvas.top() + ch as f32 * lane_h),
                    pos2(canvas.right(), canvas.top() + (ch + 1) as f32 * lane_h),
                )
            })
            .collect();
        let name = ctx.lang.audio_waveform_title(&clip.name);
        let mut canvas_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(canvas)
                .id_salt(("waveform-group", clip.id.0))
                .accessibility_role(Role::Group)
                .accessibility_label(&name),
        );
        crate::views::group_bounds(&canvas_ui, rest);
        let ui = &mut canvas_ui;
        let response = ui.interact(
            canvas,
            Id::unique(("audio-editor-canvas", clip.id.0)),
            Sense::click_and_drag(),
        );
        let selected = ctx.ui.selection.clips.contains(&clip.id);
        let frame = Frame {
            project: &project,
            clip,
            audio: a,
            axis,
            source_start,
            selected,
        };
        self.input(ui, ctx, &response, &lanes, &frame);
        let peaks = self.source.peaks(ui.ctx());
        if let Some(peaks) = peaks {
            let full_spec = WaveSpec {
                width: full_ticks.0 as f32 * ppt,
                px_per_tick: ppt,
                body_height: (lane_h - 2.0 * m::WAVE_PAD).max(1.0),
                offset: 0,
                frames: a.file.frames,
                gain: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
            };
            let window_spec = WaveSpec {
                width: clip.length.0 as f32 * ppt,
                offset: a.offset,
                frames: a.source_frames(clip.length, project.tempo),
                gain: 10.0f32.powf(a.gain_db / 20.0),
                fade_in: a.fade_in.0 as f32 * ppt,
                fade_out: a.fade_out.0 as f32 * ppt,
                ..full_spec
            };
            if self.meshes.as_ref().is_none_or(|m| {
                !Arc::ptr_eq(&m.clip, clip)
                    || !Arc::ptr_eq(&m.peaks, &peaks)
                    || m.full_spec != full_spec
                    || m.window_spec != window_spec
            }) {
                let full = self
                    .meshes
                    .take()
                    .filter(|m| {
                        m.full_spec == full_spec
                            && Arc::ptr_eq(&m.peaks, &peaks)
                            && m.full.len() == channels
                    })
                    .map_or_else(
                        || {
                            (0..channels)
                                .map(|ch| WaveMesh::from_channel(&peaks, &full_spec, ch))
                                .collect()
                        },
                        |m| m.full,
                    );
                self.meshes = Some(Meshes {
                    clip: clip.clone(),
                    peaks: peaks.clone(),
                    full_spec,
                    window_spec,
                    full,
                    window: (0..channels)
                        .map(|ch| WaveMesh::from_channel(&peaks, &window_spec, ch))
                        .collect(),
                });
            }
        } else {
            self.meshes = None;
        }
        let painter = ui
            .painter()
            .with_clip_rect(canvas.intersect(ui.clip_rect()));
        px::fill(&painter, canvas, 0.0, color32(color::SURFACE_1));
        let lines = grid_lines(
            &axis,
            project.meter,
            canvas.x_range(),
            &GridSpec::new(ctx.ui.arrangement.grid, space::SPACE_3),
        );
        paint_grid(&painter, canvas, &lines);
        for (ch, lane) in lanes.iter().copied().enumerate() {
            let inner = lane.shrink2(vec2(0.0, m::WAVE_PAD));
            let full = Rect::from_min_max(
                pos2(axis.x0, inner.top()),
                pos2(axis.x(full_ticks), inner.bottom()),
            );
            let window = Rect::from_min_max(
                pos2(axis.x(source_start), inner.top()),
                pos2(axis.x(source_end), inner.bottom()),
            );
            if let Some(meshes) = &self.meshes {
                meshes.full[ch].paint(
                    &painter,
                    full,
                    color32(palette.wave_dim),
                    color32(palette.wave_dim),
                    color32(color::METER_CLIP),
                );
            }
            px::fill(&painter, window, 0.0, color32(palette.body));
            if let Some(meshes) = &self.meshes {
                meshes.window[ch].paint(
                    &painter,
                    window,
                    color32(palette.wave_peak),
                    color32(palette.wave_rms),
                    color32(color::METER_CLIP),
                );
            } else {
                px::hline(
                    &painter,
                    window.x_range(),
                    window.center().y,
                    color32(color::TEXT_3),
                );
            }
            for edge in [window.left(), window.right()] {
                px::vline(&painter, edge, lane.y_range(), color32(palette.base));
            }
            if selected {
                self.controls(ui, &painter, ctx, window, ch, &frame);
            }
            if ch > 0 {
                px::hline(&painter, lane.x_range(), lane.top(), color32(color::LINE_2));
            }
            self.scale(ui, gutter, lane, ch, channels, ctx.lang);
        }
        for (tick, text, align) in [
            (Tick::ZERO, ctx.lang.audio_source_start(), Align2::LEFT_TOP),
            (full_ticks, ctx.lang.audio_source_end(), Align2::RIGHT_TOP),
        ] {
            let x = axis.x(tick);
            px::vline(&painter, x, canvas.y_range(), color32(color::TEXT_3));
            painter.text(
                pos2(x, canvas.top() + space::SPACE_1),
                align,
                text,
                fonts::font_id(&typography::MONO_XS),
                color32(color::TEXT_3),
            );
        }
        if self.meshes.is_none() {
            let r = painter.text(
                canvas.center(),
                Align2::CENTER_CENTER,
                ctx.lang.audio_waveform_pending(),
                fonts::font_id(&typography::UI),
                color32(color::TEXT_3),
            );
            let name = ctx.lang.audio_waveform_pending();
            ui.interact(
                r,
                Id::unique(("audio-wave-pending", clip.id.0)),
                Sense::hover(),
            )
            .widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
        }
        let playhead = ctx.engine.playhead();
        if playhead >= clip.start && playhead <= clip.end() {
            paint_playhead(
                &painter,
                axis.x(source_start + playhead - clip.start),
                canvas.y_range(),
            );
        }
        let ruler_response = ui.interact(
            ruler,
            Id::unique(("audio-editor-ruler", clip.id.0)),
            Sense::click_and_drag(),
        );
        let ruler_access = komponist_ui::timeline::ruler_access(ctx.lang, &ruler_model);
        ruler_response.widget_info(|| ruler_access.widget_info());
        if (ruler_response.clicked() || ruler_response.dragged())
            && let Some(pos) = ruler_response.interact_pointer_pos()
        {
            let source = Tick(axis.tick_at(pos.x).round() as i64);
            let at = (clip.start + source - source_start)
                .max(clip.start)
                .min(clip.end());
            ctx.send(Action::Transport(TransportAction::Locate(at)));
        }
    }
}
impl AudioEditor {
    fn missing(&self, ui: &mut Ui, ctx: &mut ViewCtx<'_>, clip: &Clip, a: &AudioClip, rect: Rect) {
        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        px::fill(&painter, rect, 0.0, color32(color::STATE_ERROR_SURFACE));
        let step = komponist_ui::widgets::metrics::timeline::HATCH_ON
            + komponist_ui::widgets::metrics::timeline::HATCH_OFF;
        let mut x = rect.left() - rect.height();
        while x < rect.right() {
            painter.line_segment(
                [pos2(x, rect.bottom()), pos2(x + rect.height(), rect.top())],
                Stroke::new(
                    komponist_ui::widgets::metrics::timeline::HATCH_ON,
                    color32(color::CANVAS_HATCH),
                ),
            );
            x += step;
        }
        let inner = Rect::from_center_size(
            rect.center(),
            vec2(
                (rect.width() - 2.0 * m::BANNER_PAD).clamp(0.0, m::BANNER_CONTENT_W),
                typography::UI.line_height
                    + typography::MONO_XS.line_height
                    + size::CONTROL
                    + 3.0 * m::BANNER_GAP,
            ),
        );
        let mut content = ui.new_child(
            UiBuilder::new()
                .max_rect(inner)
                .id_salt("audio-missing-banner")
                .accessibility_role(Role::Group)
                .accessibility_label(ctx.lang.audio_waveform_title(&clip.name)),
        );
        crate::views::group_bounds(&content, rect);
        let message = ctx
            .lang
            .audio_clip_missing(a.file.abs.file_name().unwrap_or_default().to_string_lossy());
        let message = message.trim_start_matches('⚠').trim();
        let galley = painter.layout_no_wrap(
            message.to_owned(),
            fonts::font_id(&typography::UI),
            color32(color::STATE_ERROR),
        );
        let icon_w = komponist_tokens::size::ICON;
        let title_w = galley.size().x + icon_w + space::SPACE_1;
        let x = inner.center().x - title_w / 2.0;
        let title = Rect::from_min_size(
            pos2(x, inner.top()),
            vec2(title_w, typography::UI.line_height),
        );
        komponist_ui::icons::paint_in(
            &content,
            Rect::from_min_size(title.min, vec2(icon_w, icon_w)),
            komponist_tokens::Icon::TriangleExclamationPoint,
            color32(color::STATE_ERROR),
        );
        painter.galley(
            title.min + vec2(icon_w + space::SPACE_1, 0.0),
            galley,
            color32(color::STATE_ERROR),
        );
        content
            .interact(
                title,
                Id::unique(("audio-missing-message", clip.id.0)),
                Sense::hover(),
            )
            .widget_info(|| WidgetInfo::labeled(Role::Label, true, message));
        let path = self.source.path(a).display().to_string();
        let path_rect = Rect::from_min_size(
            pos2(inner.left(), title.bottom() + m::BANNER_GAP),
            vec2(inner.width(), typography::MONO_XS.line_height),
        );
        let mut path_ui = content.new_child(
            UiBuilder::new()
                .max_rect(path_rect)
                .layout(egui::Layout::top_down(egui::Align::Center)),
        );
        path_ui
            .add(
                egui::Label::new(
                    egui::RichText::new(&path)
                        .font(fonts::font_id(&typography::MONO_XS))
                        .color(color32(color::TEXT_3)),
                )
                .truncate(),
            )
            .on_hover_text(&path);
        let actions = Rect::from_center_size(
            pos2(
                inner.center().x,
                path_rect.bottom() + m::BANNER_GAP + size::CONTROL / 2.0,
            ),
            vec2(
                m::BANNER_REPLACE_W + m::BANNER_COPY_W + m::BANNER_GAP,
                size::CONTROL,
            ),
        );
        let mut row = content.new_child(
            UiBuilder::new()
                .max_rect(actions)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = m::BANNER_GAP;
        toolbar::recovery(&mut row, ctx, clip, a, &self.source);
    }

    fn wheel(
        &self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        canvas: Rect,
        a: &AudioClip,
        project: &Project,
    ) {
        if !ui.rect_contains_pointer(canvas) {
            return;
        }
        let (delta, zoom, pointer) =
            ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
        if delta == egui::Vec2::ZERO && zoom == 1.0 {
            return;
        }
        let view = &mut ctx.ui.waveform;
        if zoom != 1.0 {
            let x = pointer.map_or(canvas.center().x, |p| p.x) - canvas.left();
            let old = view.px_per_bar / project.meter.ticks_per_bar() as f32;
            let anchor = view.scroll_tick + f64::from(x / old);
            view.px_per_bar = (view.px_per_bar * zoom).clamp(m::PPB_MIN, m::PPB_MAX);
            let next = view.px_per_bar / project.meter.ticks_per_bar() as f32;
            view.scroll_tick = (anchor - f64::from(x / next)).max(0.0);
        } else {
            let ppt = view.px_per_bar / project.meter.ticks_per_bar() as f32;
            view.scroll_tick = (view.scroll_tick - f64::from((delta.x + delta.y) / ppt)).clamp(
                0.0,
                a.frames_to_ticks(a.file.frames, project.tempo).0 as f64,
            );
        }
        ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
    }
    fn input(
        &mut self,
        ui: &mut Ui,
        ctx: &mut ViewCtx<'_>,
        response: &egui::Response,
        lanes: &[Rect],
        f: &Frame<'_>,
    ) {
        let (clip, a, project, axis, source_start, selected) = (
            f.clip,
            f.audio,
            f.project,
            f.axis,
            f.source_start,
            f.selected,
        );
        if ui.input(|i| {
            i.raw.events.iter().any(|e| {
                matches!(
                    e,
                    egui::Event::Key {
                        key: egui::Key::Escape,
                        pressed: true,
                        ..
                    }
                )
            })
        }) && self.gesture.take().is_some()
        {
            ctx.send(Action::Edit(EditAction::CancelGesture));
        }
        let hit = |pos: Pos2| -> Option<(Control, f32)> {
            let lane = lanes.iter().copied().find(|r| r.contains(pos))?;
            let inner = lane.shrink2(vec2(0.0, m::WAVE_PAD));
            let window = Rect::from_min_max(
                pos2(axis.x(source_start), inner.top()),
                pos2(axis.x(source_start + clip.length), inner.bottom()),
            );
            let pseudo = Rect::from_min_max(window.min - vec2(0.0, size::CLIP_HEADER), window.max);
            let parts = audio_parts(
                pseudo,
                a.fade_in.0 as f32 * axis.px_per_tick,
                a.fade_out.0 as f32 * axis.px_per_tick,
                a.gain_db,
            );
            let c = if parts.gain.contains(pos) {
                Control::Gain
            } else if parts.fade_in.contains(pos) {
                Control::FadeIn
            } else if parts.fade_out.contains(pos) {
                Control::FadeOut
            } else if parts.gain_band.contains(pos) && window.contains(pos) {
                Control::Gain
            } else if (pos.x - window.left()).abs() <= m::TRIM_HIT {
                Control::Start
            } else if (pos.x - window.right()).abs() <= m::TRIM_HIT {
                Control::End
            } else {
                return None;
            };
            Some((c, inner.height()))
        };
        if selected
            && response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin())
            && let Some((control, lane_height)) = hit(origin)
        {
            let label = match control {
                Control::Gain => "undo-clip-gain",
                Control::FadeIn | Control::FadeOut => "undo-clip-fade",
                _ => "undo-clip-length",
            };
            ctx.send(Action::Edit(EditAction::BeginGesture {
                label: label.into(),
            }));
            self.gesture = Some(Gesture {
                control,
                clip: clip.id,
                origin,
                start: clip.start,
                length: clip.length,
                gain: a.gain_db,
                fade_in: a.fade_in,
                fade_out: a.fade_out,
                lane_height,
            });
        }
        if let Some(g) = &self.gesture
            && let Some(pos) = ui.input(|i| i.pointer.interact_pos())
        {
            let delta =
                Tick((f64::from(pos.x - g.origin.x) / f64::from(axis.px_per_tick)).round() as i64);
            let snap = |t: Tick| {
                if ctx.ui.arrangement.snap && !ui.input(|i| i.modifiers.shift) {
                    ctx.ui.arrangement.grid.snap(t, project.meter)
                } else {
                    t
                }
            };
            let action = match g.control {
                Control::Start => EditAction::TrimAudioClipStart {
                    clip: g.clip,
                    start: snap(g.start + delta),
                },
                Control::End => EditAction::SetClipLength {
                    clip: g.clip,
                    length: (snap(g.start + g.length + delta) - g.start)
                        .max(Tick(1))
                        .min(a.max_length(project.tempo)),
                },
                Control::FadeIn => EditAction::SetAudioClipProp {
                    clip: g.clip,
                    prop: AudioClipProp::FadeIn((g.fade_in + delta).max(Tick::ZERO)),
                },
                Control::FadeOut => EditAction::SetAudioClipProp {
                    clip: g.clip,
                    prop: AudioClipProp::FadeOut((g.fade_out - delta).max(Tick::ZERO)),
                },
                Control::Gain => {
                    let [lo, hi] = komponist_ui::widgets::metrics::timeline::GAIN_RANGE_DB;
                    let fine = if ui.input(|i| i.modifiers.shift) {
                        komponist_ui::widgets::metrics::timeline::GAIN_FINE_FACTOR
                    } else {
                        1.0
                    };
                    EditAction::SetAudioClipProp {
                        clip: g.clip,
                        prop: AudioClipProp::GainDb(
                            (g.gain
                                - (pos.y - g.origin.y) * (hi - lo) / g.lane_height.max(1.0) * fine)
                                .clamp(lo, hi),
                        ),
                    }
                }
            };
            ctx.send(Action::Edit(action));
        }
        if self.gesture.is_some()
            && (!ui.input(|i| i.pointer.primary_down()) || response.drag_stopped())
        {
            self.gesture = None;
            ctx.send(Action::Edit(EditAction::EndGesture));
        }
        if selected
            && response.double_clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some((control, _)) = hit(pos)
            && control == Control::Gain
        {
            ctx.send(Action::Edit(EditAction::SetAudioClipProp {
                clip: clip.id,
                prop: AudioClipProp::GainDb(0.0),
            }));
        }
        if selected
            && response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
            && let Some((control, _)) = hit(pos)
            && ui.input(|i| i.modifiers.ctrl)
            && matches!(control, Control::FadeIn | Control::FadeOut)
        {
            ctx.send(Action::Edit(EditAction::SetAudioClipProp {
                clip: clip.id,
                prop: if control == Control::FadeIn {
                    AudioClipProp::FadeIn(Tick::ZERO)
                } else {
                    AudioClipProp::FadeOut(Tick::ZERO)
                },
            }));
        }
        if selected
            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
            && let Some((control, _)) = hit(pos)
        {
            ui.ctx().set_cursor_icon(if control == Control::Gain {
                CursorIcon::ResizeVertical
            } else {
                CursorIcon::ResizeHorizontal
            });
        }
    }
    fn controls(
        &self,
        ui: &mut Ui,
        painter: &egui::Painter,
        ctx: &ViewCtx<'_>,
        window: Rect,
        ch: usize,
        f: &Frame<'_>,
    ) {
        let (clip, a, axis) = (f.clip, f.audio, f.axis);
        let parts = audio_parts(
            Rect::from_min_max(window.min - vec2(0.0, size::CLIP_HEADER), window.max),
            a.fade_in.0 as f32 * axis.px_per_tick,
            a.fade_out.0 as f32 * axis.px_per_tick,
            a.gain_db,
        );
        let ink = color32(color::TEXT_1);
        let wm = komponist_ui::widgets::metrics::timeline::GAIN_LINE_WIDTH;
        px::hline(painter, window.x_range(), parts.gain.center().y, ink);
        painter.circle_filled(
            parts.gain.center(),
            komponist_ui::widgets::metrics::timeline::GAIN_GRIP / 2.0,
            ink,
        );
        for (handle, left) in [(parts.fade_in, true), (parts.fade_out, false)] {
            let bottom = pos2(
                if left { window.left() } else { window.right() },
                window.bottom(),
            );
            let end = pos2(handle.center().x, window.top());
            painter.add(egui::Shape::convex_polygon(
                vec![bottom, pos2(bottom.x, window.top()), end],
                color32(color::CANVAS_OUTSIDE_CLIP),
                Stroke::NONE,
            ));
            painter.line_segment([bottom, end], Stroke::new(wm, ink));
            px::fill(painter, handle, 0.0, ink);
        }
        for rect in [
            Rect::from_min_max(
                window.min,
                pos2(window.left() + m::TRIM_HIT, window.bottom()),
            ),
            Rect::from_min_max(pos2(window.right() - m::TRIM_HIT, window.top()), window.max),
        ] {
            px::fill(
                painter,
                rect,
                0.0,
                color32(color::CANVAS_CLIP_EDGE_SELECTED),
            );
        }
        let ms = |ticks: Tick| {
            format!(
                "{:.0} ms",
                ticks.0 as f64 / PPQ as f64 * 60000.0 / ctx.project.tempo.bpm
            )
        };
        for (kind, rect, name, value) in [
            (
                Control::FadeIn,
                parts.fade_in,
                ctx.lang.audio_clip_fade_in(),
                ms(a.fade_in),
            ),
            (
                Control::FadeOut,
                parts.fade_out,
                ctx.lang.audio_clip_fade_out(),
                ms(a.fade_out),
            ),
            (
                Control::Gain,
                parts.gain_band,
                ctx.lang.audio_clip_gain_line(),
                format!("{} dB", komponist_model::format::fmt_db(a.gain_db)),
            ),
            (
                Control::Start,
                Rect::from_min_max(
                    window.min,
                    pos2(window.left() + m::TRIM_HIT, window.bottom()),
                ),
                ctx.lang.audio_clip_start(),
                komponist_model::format::fmt_time(a.offset as f64 / f64::from(a.file.sample_rate)),
            ),
            (
                Control::End,
                Rect::from_min_max(pos2(window.right() - m::TRIM_HIT, window.top()), window.max),
                ctx.lang.audio_clip_end(),
                komponist_model::format::fmt_length(clip.length, ctx.project.meter),
            ),
        ] {
            let visible = rect.intersect(painter.clip_rect());
            if !visible.is_positive() {
                continue;
            }
            let r = ui.interact(
                visible,
                Id::unique(("audio-editor-handle", clip.id.0, ch, kind as u8)),
                Sense::hover(),
            );
            let access = Access::new(Role::Slider, name).with_value(value);
            r.widget_info(|| access.widget_info());
        }
    }
    fn scale(
        &self,
        ui: &Ui,
        gutter: Rect,
        lane: Rect,
        ch: usize,
        channels: usize,
        t: komponist_i18n::Lang,
    ) {
        let r = Rect::from_x_y_ranges(gutter.x_range(), lane.y_range());
        px::fill(ui.painter(), r, 0.0, color32(color::SURFACE_2));
        let channel = if channels == 1 {
            t.audio_channel_mono()
        } else if ch == 0 {
            t.audio_channel_left()
        } else {
            t.audio_channel_right()
        };
        for (pos, align, text) in [
            (r.center(), Align2::CENTER_CENTER, channel),
            (
                pos2(r.center().x, r.top() + m::WAVE_PAD),
                Align2::CENTER_TOP,
                t.audio_amplitude_zero(),
            ),
            (
                pos2(r.center().x, r.bottom() - m::WAVE_PAD),
                Align2::CENTER_BOTTOM,
                t.audio_amplitude_silence(),
            ),
        ] {
            ui.painter().text(
                pos,
                align,
                text,
                fonts::font_id(&typography::MONO_XS),
                color32(color::TEXT_3),
            );
        }
    }
}
