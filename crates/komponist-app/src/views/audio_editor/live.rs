//! Live recording views use bounded telemetry, never the unfinished WAV or its file cache.
use super::{AudioEditor, metrics as m};
use crate::recording::audio_record::AudioRecording;
use crate::ui_state::{FocusArea, UiState};
use crate::views::ViewCtx;
use egui::{Id, Rect, Role, Sense, Ui, UiBuilder, WidgetInfo, pos2, vec2};
use komponist_engine::recording::RecordingPeak;
use komponist_model::{Project, Tick, Track, TrackKind};
use komponist_session::recording::midi::Recording;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{color, size, space, typography};
use komponist_ui::timeline::{
    GridSpec, RulerKind, RulerModel, TimeAxis, WaveMesh, WaveSpec, WaveSummary, grid_lines,
    paint_grid, paint_ruler,
};
use komponist_ui::{fonts, px};

/// Selects the live editor from the selected recorded track, including audio count-in before
/// the writer has published its first frames. A mixed capture keeps MIDI and audio distinct.
pub fn recording_area(
    project: &Project,
    ui: &UiState,
    midi: Option<&Recording>,
    audio: Option<&AudioRecording>,
) -> Option<FocusArea> {
    let selected = ui
        .selection
        .anchor_track
        .filter(|id| ui.selection.tracks.contains(id));
    for id in selected.iter().chain(ui.selection.tracks.iter()) {
        if audio.is_some()
            && project
                .track(*id)
                .is_some_and(|t| t.arm && t.kind == TrackKind::Audio)
        {
            return Some(FocusArea::Waveform);
        }
        if midi.is_some_and(|r| r.clip(*id).is_some()) {
            return Some(FocusArea::PianoRoll);
        }
    }
    if audio.is_some() {
        Some(FocusArea::Waveform)
    } else if midi.is_some() {
        Some(FocusArea::PianoRoll)
    } else {
        None
    }
}

fn track<'a>(project: &'a Project, ui: &UiState, rec: &AudioRecording) -> Option<&'a Track> {
    ui.selection
        .anchor_track
        .iter()
        .filter(|id| ui.selection.tracks.contains(*id))
        .chain(ui.selection.tracks.iter())
        .filter_map(|id| project.track(*id))
        .find(|t| t.kind == TrackKind::Audio && (t.arm || rec.progress(t.id).is_some()))
        .or_else(|| rec.first().and_then(|(id, _)| project.track(id)))
        .or_else(|| {
            project
                .tracks
                .iter()
                .find(|t| t.arm && t.kind == TrackKind::Audio)
        })
        .map(|t| t.as_ref())
}

/// Converts engine telemetry at the app boundary. Combined buckets contain one sample per
/// channel per frame; native channel buckets contain exactly one sample per frame.
pub fn summaries(peaks: &[RecordingPeak], channels: u16) -> Vec<WaveSummary> {
    let channels = u64::from(channels.max(1));
    peaks
        .iter()
        .map(|p| WaveSummary {
            min: p.min,
            max: p.max,
            rms: p.rms,
            frames: p.samples / channels,
        })
        .collect()
}

impl AudioEditor {
    /// Returns true while this dock belongs to a live audio capture. Source edits and file
    /// actions are absent during capture; monitoring and viewport zoom remain available.
    pub(super) fn live(&mut self, ui: &mut Ui, ctx: &mut ViewCtx<'_>) -> bool {
        let Some(rec) = ctx.audio_recording else {
            return false;
        };
        let Some(track) = track(ctx.project, ctx.ui, rec) else {
            return true;
        };
        let project = std::sync::Arc::clone(ctx.project);
        let track = project.track(track.id).unwrap();
        let progress = rec.progress(track.id);
        let clip = rec.clip(track.id);
        let start = progress.map_or(rec.start, |p| p.start);
        let frames = progress.map_or(0, |p| p.frames);
        let rate = ctx.engine.sample_rate();
        let ticks = Tick(
            (frames as f64 / f64::from(rate) * project.tempo.bpm / 60.0
                * komponist_model::PPQ as f64)
                .round() as i64,
        );
        let rect = ui.max_rect();
        let toolbar = Rect::from_min_size(rect.min, vec2(rect.width(), size::PANEL_HEADER));
        live_toolbar(ui, ctx, toolbar, track, progress, rate);
        let gutter = Rect::from_min_max(
            pos2(rect.left(), toolbar.bottom() + m::RULER_H),
            pos2(rect.left() + m::GUTTER, rect.bottom()),
        );
        let ruler = Rect::from_min_max(
            pos2(gutter.right(), toolbar.bottom()),
            pos2(rect.right(), gutter.top()),
        );
        let canvas = Rect::from_min_max(pos2(gutter.right(), gutter.top()), rect.max);
        let ppt = ctx.ui.waveform.px_per_bar / project.meter.ticks_per_bar() as f32;
        // Follow the actual captured prefix, not the entire loop window or the callback head.
        let visible_ticks = canvas.width() / ppt;
        let scroll = (ticks.0 as f32 - visible_ticks).max(0.0);
        let axis = TimeAxis {
            x0: canvas.left() - (start.0 as f32 + scroll) * ppt,
            px_per_tick: ppt,
        };
        let model = RulerModel {
            axis,
            meter: project.meter,
            kind: RulerKind::Editor {
                clip: komponist_model::TickRange::new(start, start + ticks),
                clip_color: color32(color::STATE_RECORD),
            },
            playhead: None,
            recording: true,
        };
        paint_ruler(ui.painter(), ruler, &model);
        let name = ctx.lang.audio_live_waveform();
        let canvas_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(canvas)
                .id_salt(("audio-live-waveform", track.id.0))
                .accessibility_role(Role::Group)
                .accessibility_label(name),
        );
        crate::views::group_bounds(&canvas_ui, canvas);
        let painter = canvas_ui
            .painter()
            .with_clip_rect(canvas.intersect(ui.clip_rect()));
        px::fill(&painter, canvas, 0.0, color32(color::SURFACE_1));
        paint_grid(
            &painter,
            canvas,
            &grid_lines(
                &axis,
                project.meter,
                canvas.x_range(),
                &GridSpec::new(ctx.ui.arrangement.grid, space::SPACE_3),
            ),
        );
        let channels = progress.map_or(1, |p| usize::from(p.channels.clamp(1, 2)));
        for channel in 0..channels {
            let height = canvas.height() / channels as f32;
            let lane = Rect::from_min_max(
                pos2(canvas.left(), canvas.top() + channel as f32 * height),
                pos2(canvas.right(), canvas.top() + (channel + 1) as f32 * height),
            );
            let body = Rect::from_min_max(
                pos2(axis.x(start), lane.top() + m::WAVE_PAD),
                pos2(axis.x(start + ticks), lane.bottom() - m::WAVE_PAD),
            );
            px::fill(&painter, body, 0.0, color32(color::STATE_RECORD_SURFACE));
            let data = progress
                .and_then(|p| p.channel_waveforms.get(channel))
                .map_or_else(Vec::new, |p| summaries(p, 1));
            let spec = WaveSpec {
                width: body.width(),
                px_per_tick: ppt,
                body_height: body.height(),
                offset: 0,
                frames,
                gain: 1.0,
                fade_in: 0.0,
                fade_out: 0.0,
            };
            WaveMesh::from_summaries(&data, &spec).paint(
                &painter,
                body,
                color32(color::STATE_RECORD),
                color32(color::STATE_RECORD_INK),
                color32(color::METER_CLIP),
            );
            self.scale(ui, gutter, lane, channel, channels, ctx.lang);
            if channel > 0 {
                px::hline(&painter, lane.x_range(), lane.top(), color32(color::LINE_2));
            }
        }
        px::vline(
            &painter,
            axis.x(start + ticks),
            canvas.y_range(),
            color32(color::STATE_RECORD),
        );
        let name = clip.map_or(name, |c| c.name.trim_start_matches("● "));
        ui.interact(
            canvas,
            Id::unique(("audio-live-status", track.id.0)),
            Sense::hover(),
        )
        .widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
        true
    }
}

fn live_toolbar(
    ui: &mut Ui,
    ctx: &mut ViewCtx<'_>,
    rect: Rect,
    track: &Track,
    progress: Option<&komponist_engine::recording::RecordingProgress>,
    rate: u32,
) {
    px::fill(ui.painter(), rect, 0.0, color32(color::SURFACE_2));
    let mut row = ui.new_child(
        UiBuilder::new()
            .max_rect(rect)
            .id_salt(("audio-live-toolbar", track.id.0)),
    );
    egui::ScrollArea::horizontal()
        .id_salt("audio-live-toolbar-scroll")
        .max_height(size::PANEL_HEADER)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut row, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = space::SPACE_3;
                let t = ctx.lang;
                let input =
                    crate::views::audio_inputs::input_label(t, ctx.project, track.audio.input);
                let peak = progress.map_or(0.0, |p| p.peak);
                let db = if peak > 0.0 {
                    format!("{:.1}", 20.0 * peak.log10()).replace('-', "−")
                } else {
                    "−∞".to_owned()
                };
                let ms = progress.map_or(0, |p| p.frames * 1000 / u64::from(rate.max(1)));
                let duration = format!("{:02}:{:02}.{:03}", ms / 60000, ms / 1000 % 60, ms % 1000);
                for text in [
                    t.audio_live_input(input),
                    t.audio_live_peak(db),
                    t.audio_live_duration(duration),
                ] {
                    ui.label(
                        egui::RichText::new(text)
                            .font(fonts::font_id(&typography::MONO_XS))
                            .color(color32(color::TEXT_2)),
                    );
                }
                ui.label(t.audio_monitoring());
                let (_, monitor_rect) = ui.allocate_space(vec2(size::CONTROL, size::CONTROL));
                crate::views::audio_inputs::monitor_button(ui, ctx, track, monitor_rect);
                ui.label(match track.audio.monitor {
                    komponist_model::MonitorMode::Off => t.ui_monitor_off(),
                    komponist_model::MonitorMode::Auto => t.ui_monitor_auto(),
                    komponist_model::MonitorMode::On => t.ui_monitor_on(),
                });
                if let Some(p) = progress {
                    ui.label(
                        egui::RichText::new(
                            p.path.file_name().unwrap_or_default().to_string_lossy(),
                        )
                        .font(fonts::font_id(&typography::MONO_XS))
                        .color(color32(color::TEXT_3)),
                    );
                }
                for (icon, name, factor) in [
                    (
                        komponist_tokens::Icon::Minus,
                        t.audio_zoom_out(),
                        1.0 / m::ZOOM_STEP,
                    ),
                    (
                        komponist_tokens::Icon::Plus,
                        t.audio_zoom_in(),
                        m::ZOOM_STEP,
                    ),
                ] {
                    if ui
                        .add(komponist_ui::widgets::button::Button::icon(icon, name).compact())
                        .clicked()
                    {
                        ctx.ui.waveform.px_per_bar =
                            (ctx.ui.waveform.px_per_bar * factor).clamp(m::PPB_MIN, m::PPB_MAX);
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::App;
    use egui_kittest::kittest::Queryable as _;
    use komponist_i18n::Lang;
    use komponist_model::{AudioFile, ClipContent, TrackId, edits};
    use std::sync::Arc;

    fn fixture(lang: Lang, stereo: bool) -> (App, AudioRecording, TrackId, TrackId) {
        let p = Project::new("Live capture");
        let (p, bus) = edits::add_bus(&p, edits::NewBus::default()).unwrap();
        let (p, midi) = edits::add_track(
            &p,
            edits::NewTrack {
                name: Some("Keys".into()),
                hue: None,
                instrument: None,
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let (mut p, audio) = edits::add_audio_track(
            &p,
            edits::NewAudioTrack {
                name: Some("Vocal".into()),
                input: komponist_model::AudioInput::Bus(bus),
                ..Default::default()
            },
        )
        .unwrap();
        for track in &mut p.tracks {
            let track = Arc::make_mut(track);
            track.arm = !track.is_bus();
            if track.kind == TrackKind::Audio {
                track.audio.monitor = komponist_model::MonitorMode::Auto;
            }
        }
        let start = Tick(12 * 3840);
        let frames = 133920;
        let channels = if stereo { 2 } else { 1 };
        // Unequal interval lengths represent a compacted prefix and a partial final bucket.
        let left: Vec<_> = (0..129)
            .map(|i| {
                let frames = if i == 128 { 800 } else { 1040 };
                let level = 0.1 + (i as f32 * 0.17).sin().abs() * 0.7;
                RecordingPeak {
                    min: -level * 0.8,
                    max: level,
                    rms: level * 0.45,
                    samples: frames,
                }
            })
            .collect();
        let right: Vec<_> = left
            .iter()
            .map(|p| RecordingPeak {
                min: p.min * 0.3,
                max: p.max * 0.3,
                rms: p.rms * 0.3,
                samples: p.samples,
            })
            .collect();
        let combined = left
            .iter()
            .map(|p| RecordingPeak {
                samples: p.samples * u64::from(channels),
                rms: p.rms * if stereo { 0.545f32.sqrt() } else { 1.0 },
                ..*p
            })
            .collect();
        let progress = komponist_engine::recording::RecordingProgress {
            track: audio,
            path: "/preview/Take1.wav".into(),
            start,
            channels,
            frames,
            lap: 1,
            peak: 0.62,
            waveform: combined,
            channel_waveforms: if stereo {
                vec![left, right]
            } else {
                vec![left]
            },
        };
        let length = Tick(
            (frames as f64 / 48000.0 * p.tempo.bpm / 60.0 * komponist_model::PPQ as f64).round()
                as i64,
        );
        let (with_clip, id) = edits::add_clip(
            &p,
            audio,
            edits::NewClip {
                start,
                length,
                name: lang.record_clip(lang.record_take(1)),
                content: edits::NewClipContent::Audio {
                    file: AudioFile::new(
                        std::path::Path::new("/preview/Take1.wav"),
                        None,
                        frames,
                        48000,
                        channels,
                    ),
                    offset: 0,
                },
            },
        )
        .unwrap();
        let clip = with_clip.find_clip(id).unwrap().1.clone();
        let rec =
            AudioRecording::preview(start, start + length, vec![progress], vec![(audio, clip)]);
        let mut app = App::headless(p).with_lang(lang);
        app.ui.selection.tracks.clear();
        app.ui.selection.tracks.insert(audio);
        app.ui.arrangement.scroll_tick = start.0 as f64;
        app.ui.waveform.px_per_bar = m::PPB_DEFAULT;
        (app, rec, midi, audio)
    }

    #[test]
    fn mixed_live_editor_follows_selected_recorded_track_and_audio_count_in() {
        let (mut app, audio, midi_id, audio_id) = fixture(Lang::En, true);
        let midi = Recording::new(
            app.project(),
            Tick::ZERO,
            crate::recording::MidiTakeNames(Lang::En),
        );
        assert_eq!(
            recording_area(app.project(), &app.ui, Some(&midi), Some(&audio)),
            Some(FocusArea::Waveform)
        );
        app.ui.selection.tracks.insert(midi_id);
        app.ui.selection.anchor_track = Some(midi_id);
        assert_eq!(
            recording_area(app.project(), &app.ui, Some(&midi), Some(&audio)),
            Some(FocusArea::PianoRoll)
        );
        app.ui.selection.tracks.clear();
        app.ui.selection.anchor_track = Some(audio_id);
        app.ui.selection.tracks.insert(audio_id);
        assert_eq!(
            recording_area(
                app.project(),
                &app.ui,
                Some(&midi),
                Some(&AudioRecording::default())
            ),
            Some(FocusArea::Waveform)
        );
    }

    fn show(ui: &mut Ui, app: &mut App, rec: &AudioRecording) {
        let project = Arc::clone(app.project());
        let rect = ui.max_rect();
        let top = Rect::from_min_max(
            rect.min,
            pos2(rect.right(), rect.top() + rect.height() * 0.45),
        );
        let strip = Rect::from_min_max(
            pos2(rect.left(), top.bottom()),
            pos2(rect.right(), top.bottom() + size::TAB),
        );
        crate::panels::dock::tab_strip(
            ui,
            strip,
            &project,
            &app.ui,
            Some(FocusArea::Waveform),
            &app.keymap,
            app.lang,
            &mut app.queue,
        );
        let bottom = Rect::from_min_max(pos2(rect.left(), strip.bottom()), rect.max);
        let mut ctx = ViewCtx::new(
            &project,
            &mut app.ui,
            app.session.engine(),
            &mut app.meters,
            &app.keymap,
            &mut app.queue,
        );
        ctx.audio_recording = Some(rec);
        ctx.lang = app.lang;
        app.views
            .arrangement
            .show(&mut ui.new_child(UiBuilder::new().max_rect(top)), &mut ctx);
        app.views.waveform.show(
            &mut ui.new_child(UiBuilder::new().max_rect(bottom)),
            &mut ctx,
        );
    }

    #[test]
    fn live_audio_accessibility_has_no_edit_or_missing_source_controls() {
        for lang in [Lang::De, Lang::En] {
            let (app, rec, _, _) = fixture(lang, true);
            let mut h = komponist_ui::snapshot::builder(1.0)
                .with_size(vec2(1600.0, 700.0))
                .build_ui_state(
                    |ui, state: &mut (App, AudioRecording)| show(ui, &mut state.0, &state.1),
                    (app, rec),
                );
            h.run_steps(3);
            assert!(
                h.query_by_role_and_label(Role::Group, lang.audio_live_waveform())
                    .is_some()
            );
            assert!(
                h.query_by_role_and_label(Role::Button, lang.audio_file_replace())
                    .is_none()
            );
            assert!(
                h.query_by_role_and_label(Role::Label, lang.audio_waveform_pending())
                    .is_none()
            );
            assert!(
                h.query_by_role_and_label(Role::Slider, lang.audio_clip_gain_line())
                    .is_none()
            );
            let name = lang.record_clip(lang.record_take(1));
            let status = h.get_by_role_and_label(Role::Label, name.trim_start_matches("● "));
            assert!(status.rect().width() > 100.0);
            let ClipContent::Audio(a) = &h.state().1.first().unwrap().1.content else {
                panic!("audio");
            };
            assert!(!a.file.abs.exists(), "fixture must never open a WAV");
        }
    }

    #[test]
    fn live_waveform_snapshots() {
        let mut results = egui_kittest::SnapshotResults::new();
        for lang in [Lang::De, Lang::En] {
            for ppp in [1.0, 2.0] {
                let (app, rec, _, _) = fixture(lang, true);
                let mut h = komponist_ui::snapshot::builder(ppp)
                    .with_size(vec2(1600.0, 700.0))
                    .with_options(
                        egui_kittest::SnapshotOptions::new().output_path(
                            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                                .join("tests/snapshots"),
                        ),
                    )
                    .with_render_every_step(true)
                    .wgpu()
                    .build_ui_state(
                        |ui, state: &mut (App, AudioRecording)| show(ui, &mut state.0, &state.1),
                        (app, rec),
                    );
                h.run_steps(3);
                let suffix = if lang == Lang::En { "-en" } else { "" };
                let name = format!("audio-recording-live{suffix}-{ppp}x");
                if std::env::var_os("KOMPONIST_AUDIO_LIVE_PREVIEW").is_some() {
                    h.render()
                        .unwrap()
                        .save(std::env::temp_dir().join(format!("komponist-{name}.png")))
                        .unwrap();
                }
                h.snapshot(name);
                results.extend_harness(&mut h);
            }
        }
        results.unwrap();
    }
}
