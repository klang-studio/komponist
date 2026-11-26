//! Status bar (`size.statusbar` 22; `Screens.dc.html` `stt`, `ks-view.js` `statusView`).
//!
//! Left: what is selected or happening (`type.small`, `text.2`; red while recording or on an
//! error); next to it the hint for the current context (`text.3`); right: the audio output
//! ("PipeWire · 48 kHz · 256 Samples · 5.3 ms", `type.value.s`; "· ohne Echtzeit-Priorität" in the
//! warning colour when the audio thread got none). A status message
//! ([`crate::ui_state::StatusMessage`]) replaces the left text for 7 s; a finished export's
//! message carries the link „Im Ordner zeigen“ right after it, in `accent` (S08 "fertig ·
//! Statuszeile", prototype `linkS`). Texts come from `locales/*/status.ftl` (`docs/I18N.md`), verbatim from the screens in German;
//! a
//! leading "● " (recording) is painted as a dot in the text colour, the fonts have no ●
//! (`ks-view.js` `statusView` `dotS`).

use std::time::Instant;

use egui::{Rect, Role, Sense, Ui, WidgetInfo, pos2, vec2};
use komponist_audio::{AudioState, HostKind, StreamConfig};
use komponist_engine::TransportState;
use komponist_i18n::Lang;
use komponist_model::ClipContent;
use komponist_tokens::egui_support::color32;
use komponist_tokens::{Srgba, color, space, typography};
use komponist_ui::{px, widgets as w};

use crate::app::{App, STATUS_DURATION};
use crate::metrics::transport::ERROR_DOT;
use crate::platform::Platform;
use crate::ui_state::{FocusArea, StatusKind};

/// The three texts of the status bar and their colours.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusModel {
    /// Left text.
    pub left: String,
    /// Colour of the left text.
    pub left_color: Srgba,
    /// A green dot before the left text (a success message).
    pub ok_dot: bool,
    /// A link after the left text ("Im Ordner zeigen"); [`show`] reports a click.
    pub link: Option<String>,
    /// The hint next to it.
    pub mid: String,
    /// The audio output, right-aligned.
    pub right: String,
    /// Colour of the right text.
    pub right_color: Srgba,
}

/// "48 kHz", "44.1 kHz".
pub fn fmt_rate(rate: u32) -> String {
    let khz = f64::from(rate) / 1000.0;
    if (khz - khz.round()).abs() < 1e-9 {
        format!("{khz:.0} kHz")
    } else {
        format!("{khz:.1} kHz")
    }
}

/// The audio output as the status bar shows it: driver, rate, buffer and its latency
/// (`buffer / rate`): "PipeWire · 48 kHz · 256 Samples · 5.3 ms".
pub fn fmt_output(config: &StreamConfig, t: Lang) -> String {
    let host = match config.host {
        HostKind::CoreAudio => "CoreAudio",
        HostKind::Wasapi => "WASAPI",
        HostKind::Asio => "ASIO",
        HostKind::PipeWire => "PipeWire",
        HostKind::Jack => "JACK",
        HostKind::Alsa => "ALSA",
        HostKind::Null => t.transport_no_device(),
    };
    let ms = f64::from(config.buffer_frames) * 1000.0 / f64::from(config.sample_rate.max(1));
    t.status_output(
        host,
        fmt_rate(config.sample_rate),
        config.buffer_frames,
        format!("{ms:.1}"),
    )
}

/// What the status bar shows for `app` at `now`.
pub fn model(app: &App, now: Instant) -> StatusModel {
    let t = app.lang();
    let mut s = StatusModel {
        left: String::new(),
        left_color: color::TEXT_2,
        ok_dot: false,
        link: None,
        mid: String::new(),
        right: String::new(),
        right_color: color::TEXT_3,
    };
    let mapping_hint = app
        .ui()
        .track_macros
        .mapping
        .and_then(|id| app.project().track(id))
        .map(|track| {
            let index = app.ui().track_macros.selected(track.id);
            let source = crate::views::track_macros::reference(
                t,
                index,
                &track.macros.0[usize::from(index)],
            );
            t.status_macros_mapping(source)
        });
    match app.engine().audio_state() {
        // Without real-time priority dropouts are likely: said at the right, in the warning
        // colour (S07 explains it).
        AudioState::Running { config, .. } if app.engine().stats().realtime_denied() => {
            s.right = format!("{} · {}", fmt_output(config, t), t.status_no_realtime());
            s.right_color = color::METER_LOUD;
        }
        AudioState::Running { config, .. } => s.right = fmt_output(config, t),
        AudioState::NoDevice { .. } => {
            s.left = t.transport_no_device().to_owned();
            s.left_color = color::STATE_ERROR;
            s.right = t.status_audio_stopped().to_owned();
            s.right_color = color::STATE_ERROR;
            s.mid = mapping_hint.unwrap_or_default();
            return s;
        }
        AudioState::Stopped => s.right = t.status_audio_stopped().to_owned(),
    }
    context(app, &mut s);
    if app.ui().automation_hint
        && matches!(
            app.transport(),
            TransportState::Stopped | TransportState::Playing
        )
        && !app
            .ui()
            .devices
            .touched
            .as_ref()
            .is_some_and(|touch| touch.dragging)
    {
        s.mid = Platform::current()
            .pick(t.automation_lane_hint(), t.automation_lane_hint_mac())
            .to_owned();
    }
    if let Some(hint) = mapping_hint {
        s.mid = hint;
    }
    if let Some(msg) = &app.ui().status
        && now.saturating_duration_since(msg.since) < STATUS_DURATION
    {
        s.left = msg.text.clone();
        s.ok_dot = msg.kind == StatusKind::Success;
        s.link = msg.link.as_ref().map(|(label, _)| label.clone());
        s.left_color = match msg.kind {
            StatusKind::Error => color::STATE_ERROR,
            StatusKind::Success | StatusKind::Info => color::TEXT_2,
        };
    } else if matches!(
        app.transport(),
        TransportState::Stopped | TransportState::Playing
    ) {
        if let Some(error) = app.engine().audio_input_error() {
            s.left = t.audio_input_open_failed(error);
            s.left_color = color::STATE_ERROR;
        } else if let Some(program) = app.engine().program()
            && program.routing.pdc_shortfall > 0
            && !app
                .ui()
                .devices
                .touched
                .as_ref()
                .is_some_and(|touch| touch.dragging)
        {
            s.left = t.status_pdc_shortfall(program.routing.pdc_shortfall);
            s.left_color = color::METER_LOUD;
        }
    }
    s
}

/// Left and middle text from the transport, focus and selection.
fn context(app: &App, s: &mut StatusModel) {
    let t = app.lang();
    let platform = Platform::current();
    let p = app.project();
    let ui = app.ui();
    let sel = &ui.selection;
    match app.transport() {
        TransportState::Recording => {
            let armed: Vec<&str> = p
                .tracks
                .iter()
                .filter(|t| t.arm)
                .map(|t| t.name.as_str())
                .collect();
            s.left = if let Some(recording) = app.audio_recording() {
                let inputs = recording
                    .tracks()
                    .filter_map(|id| p.track(id))
                    .map(|track| crate::views::audio_inputs::input_label(t, p, track.audio.input))
                    .collect::<Vec<_>>()
                    .join(", ");
                t.status_recording_input(armed.join(", "), inputs)
            } else {
                t.status_recording(armed.join(", "))
            };
            // "· Take 1": the take the editor shows (S02).
            if let Some(take) = app
                .recording()
                .and_then(|r| r.first().and_then(|(track, _)| r.take_name(track)))
            {
                s.left.push_str(" · ");
                s.left.push_str(&take);
            }
            s.left_color = color::STATE_RECORD;
            s.mid = t.status_recording_hint().to_owned();
            return;
        }
        TransportState::CountIn => {
            s.left = t.status_count_in().to_owned();
            s.left_color = color::STATE_RECORD;
            return;
        }
        TransportState::Playing | TransportState::Stopped => {}
    }
    // A device parameter held and moved: its name and value (WP-89).
    if let Some(touch) = ui.devices.touched.as_ref().filter(|t| t.dragging)
        && let Some(param) = crate::views::devices::model::touched(
            t,
            p,
            app.engine().host(),
            &ui.plugin_windows,
            touch,
        )
    {
        s.left = format!("{} · {}", param.name, param.value);
        s.mid = Platform::current()
            .pick(t.status_param_hint(), t.status_param_hint_mac())
            .to_owned();
        return;
    }
    let open = ui.editor.clip.and_then(|c| p.find_clip(c));
    let track = sel.tracks.iter().next().and_then(|t| p.track(*t));
    match (ui.focus, open) {
        (FocusArea::PianoRoll, Some((_, clip))) => {
            if let ClipContent::Midi(midi) = &clip.content {
                let total = midi.active().notes.len();
                s.left = t.status_notes_selected(&clip.name, sel.notes.len(), total);
                s.mid = platform
                    .pick(t.status_notes_hint(), t.status_notes_hint_mac())
                    .to_owned();
                return;
            }
        }
        (FocusArea::StepSequencer, Some((_, clip))) => {
            if let ClipContent::Step(steps) = &clip.content {
                let len = usize::from(steps.length_steps);
                match sel.step.and_then(|c| c.step) {
                    Some(step) => {
                        s.left = t.status_step_selected(&clip.name, step + 1, len);
                        s.mid = Platform::current()
                            .pick(t.status_step_hint(), t.status_step_hint_mac())
                            .to_owned();
                    }
                    None => {
                        let set: usize = steps
                            .rows
                            .iter()
                            .map(|r| r.steps.iter().take(len).filter(|st| st.on).count())
                            .sum();
                        s.left = t.status_steps_set(&clip.name, set);
                        s.mid = t.status_steps_hint().to_owned();
                    }
                }
                return;
            }
        }
        (FocusArea::Mixer, _) if ui.layout.dock_maximized => {
            if let Some(tr) = track {
                s.left = t.status_track_selected(&tr.name);
            }
            s.mid = t.status_mixer_hint().to_owned();
            return;
        }
        (FocusArea::Devices, _) => {
            if let Some(tr) = track {
                s.left = t.status_devices(&tr.name, tr.channel.devices.len());
                s.mid = Platform::current()
                    .pick(t.status_devices_hint(), t.status_devices_hint_mac())
                    .to_owned();
                return;
            }
        }
        _ => {}
    }
    if !sel.clips.is_empty() {
        s.left = t.status_clips_selected(sel.clips.len());
        let audio = sel.clips.iter().any(|id| {
            p.find_clip(*id)
                .is_some_and(|(_, c)| matches!(c.content, komponist_model::ClipContent::Audio(_)))
        });
        s.mid = if audio {
            platform.pick(t.status_audio_clips_hint(), t.status_audio_clips_hint_mac())
        } else {
            platform.pick(t.status_clips_hint(), t.status_clips_hint_mac())
        }
        .to_owned();
    } else if let Some(tr) = track {
        s.left = t.status_track_selected(&tr.name);
        s.mid = t.status_track_hint().to_owned();
    } else {
        s.left = if app.project_path().is_none() || app.doc().is_dirty() {
            t.status_project_unsaved(&p.name)
        } else {
            p.name.clone()
        };
        if p.tracks.is_empty() {
            s.mid = platform
                .pick(t.status_empty_hint(), t.status_empty_hint_mac())
                .to_owned();
        }
    }
}

/// Draws the status bar into `rect`; returns whether the link was clicked.
pub fn show(ui: &mut Ui, rect: Rect, model: &StatusModel) -> bool {
    let painter = ui.painter().clone();
    let ppp = ui.pixels_per_point();
    px::fill(&painter, rect, 0.0, color32(color::SURFACE_1));
    let inner = rect.shrink2(vec2(space::SPACE_3, 0.0));
    let clip = painter.with_clip_rect(inner);

    let right = w::styled_galley(ui, &model.right, &typography::VALUE_S);
    let right_x = inner.right() - right.size().x;
    let right_rect = Rect::from_min_size(w::text_pos(inner, &right, right_x, ppp), right.size());
    let mut x = inner.left();
    let rec_text = model.left.strip_prefix("● ");
    let dot = if model.ok_dot {
        Some(color::STATE_PLAY)
    } else {
        rec_text.map(|_| model.left_color)
    };
    if let Some(dot) = dot {
        let c = pos2(x + ERROR_DOT / 2.0, inner.center().y);
        clip.circle_filled(c, ERROR_DOT / 2.0, color32(dot));
        x += ERROR_DOT + space::SPACE_2;
    }
    let left_limit = (right_rect.left() - space::SPACE_4).max(inner.left());
    let left_bounds = Rect::from_x_y_ranges(inner.left()..=left_limit, inner.y_range());
    let text_clip = clip.with_clip_rect(left_bounds);
    let mut parts = Vec::new();
    let mut link_rect = None;
    let link = model.link.as_deref().unwrap_or("");
    for (i, (text, col)) in [
        (model.left.as_str(), model.left_color),
        (link, color::ACCENT),
        (model.mid.as_str(), color::TEXT_3),
    ]
    .into_iter()
    .enumerate()
    {
        if text.is_empty() {
            continue;
        }
        let shown = if i == 0 {
            rec_text.unwrap_or(text)
        } else {
            text
        };
        let g = w::styled_galley(ui, shown, &typography::SMALL);
        let pos = w::text_pos(inner, &g, x, ppp);
        let r = Rect::from_min_size(pos, g.size());
        x += g.size().x + space::SPACE_4;
        text_clip.galley(pos, g, color32(col));
        if i == 1 {
            link_rect = Some(r.intersect(inner));
        } else if i == 0 {
            parts.push((r.intersect(left_bounds), text, r.right() > left_limit));
        } else {
            parts.push((r.intersect(inner), text, false));
        }
    }
    clip.galley(right_rect.min, right, color32(model.right_color));
    parts.push((right_rect, model.right.as_str(), false));
    for (i, (r, text, clipped)) in parts.into_iter().enumerate() {
        if text.is_empty() || r.width() <= 0.0 {
            continue;
        }
        let id = ui.scope_id().with(("komponist_status", i));
        let response = ui.interact(r, id, Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
        if clipped {
            response.on_hover_ui(|ui| {
                let screen_width = ui.ctx().content_rect().width() - 2.0 * space::SPACE_4;
                ui.set_max_width(ui.spacing().tooltip_width.min(screen_width.max(1.0)));
                ui.add(egui::Label::new(text).wrap());
            });
        }
    }
    let Some(r) = link_rect.filter(|r| r.width() > 0.0) else {
        return false;
    };
    let id = ui.scope_id().with("komponist_status_link");
    let response = ui
        .interact(r, id, Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand);
    response.widget_info(|| WidgetInfo::labeled(Role::Link, true, link));
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_text_matches_s01() {
        // The default host depends on the platform; S01 shows PipeWire.
        let config = StreamConfig {
            host: HostKind::PipeWire,
            ..StreamConfig::default()
        };
        assert_eq!(
            fmt_output(&config, Lang::De),
            "PipeWire · 48 kHz · 256 Samples · 5.3 ms"
        );
        assert_eq!(
            fmt_output(&config, Lang::En),
            "PipeWire · 48 kHz · 256 samples · 5.3 ms"
        );
        assert_eq!(fmt_rate(44_100), "44.1 kHz");
        assert_eq!(fmt_rate(96_000), "96 kHz");
    }

    #[test]
    fn hovering_the_clipped_left_status_shows_the_full_text() {
        use egui_kittest::Harness;
        use egui_kittest::kittest::Queryable as _;

        let message = "Export failed · Warp in audio clip “A long audio clip name that does not fit” · The audio file cannot be read";
        for (width, expected_labels) in [(420.0, 2), (1400.0, 1)] {
            let mut h = Harness::builder()
                .with_size(vec2(width, 200.0))
                .with_step_dt(0.2)
                .with_context_setup(komponist_ui::install)
                .build_ui(|ui| {
                    let rect = Rect::from_min_size(
                        ui.min_rect().min,
                        vec2(ui.available_width(), komponist_tokens::size::STATUSBAR),
                    );
                    let model = StatusModel {
                        left: message.into(),
                        left_color: color::STATE_ERROR,
                        ok_dot: false,
                        link: None,
                        mid: String::new(),
                        right: "PipeWire · 48 kHz · 256 samples · 5.3 ms".into(),
                        right_color: color::TEXT_3,
                    };
                    show(ui, rect, &model);
                });
            let left = h.get_by_label(message).rect().center();
            h.event(egui::Event::PointerMoved(left));
            h.run_steps(6);
            let labels: Vec<_> = h.query_all_by_label(message).collect();
            assert_eq!(labels.len(), expected_labels);
            assert!(labels.iter().all(|node| node.rect().right() <= width));
        }
    }

    #[test]
    fn compensation_warning_shows_in_both_languages_and_clears_after_a_recompile() {
        use crate::actions::{Action, EditAction};
        use crate::device_host::DeviceHost as _;
        use egui_kittest::kittest::Queryable as _;
        use komponist_engine::MAX_PDC_FRAMES;
        use komponist_model::edits::{self, NewDevice, NewTrack};
        use komponist_model::{ChannelRef, DeviceRole, Project};
        use std::time::Duration;

        let wait_for_shortfall = |app: &mut App, expected| {
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                app.step();
                let actual = app
                    .engine()
                    .program()
                    .map(|program| program.routing.pdc_shortfall);
                if actual == Some(expected) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "expected a PDC shortfall of {expected} frames, got {actual:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        let track = NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        };
        let (project, slow) = edits::add_track(&Project::new("Latency"), track.clone()).unwrap();
        let (mut project, _) = edits::add_track(&project, track).unwrap();
        let host = App::headless_plugin_host();
        let limiter = host
            .catalog()
            .entries()
            .iter()
            .find(|entry| entry.plugin.id == "org.klang-studio.komponist.limiter")
            .expect("build the Limiter fixture with scripts/build-test-plugins.sh")
            .plugin
            .clone();
        let limiter = NewDevice {
            role: DeviceRole::Effect,
            plugin: limiter,
            state: None,
            sidechain: None,
        };
        // Each limiter contributes 96 frames at the default 48 kHz.
        let limiters = MAX_PDC_FRAMES / 96 + 1;
        let frames = (limiters * 96 - MAX_PDC_FRAMES) as u32;
        for index in 0..limiters {
            (project, _) =
                edits::insert_device(&project, ChannelRef::Track(slow), index, limiter.clone())
                    .unwrap();
        }
        let device = project.track(slow).unwrap().channel.devices[0].id;
        let preview = std::env::var_os("KOMPONIST_PDC_PREVIEW").map(std::path::PathBuf::from);
        if let Some(dir) = &preview {
            crate::project_io::save_project(&dir.join("pdc.komponist"), &project).unwrap();
        }

        for lang in [Lang::De, Lang::En] {
            let mut app = App::headless(project.clone()).with_lang(lang);
            wait_for_shortfall(&mut app, frames);
            let warning = lang.status_pdc_shortfall(frames);
            let status = model(&app, Instant::now());
            assert_eq!(status.left, warning);
            assert_eq!(status.left_color, color::METER_LOUD);
            let mut h = komponist_ui::snapshot::builder(1.0)
                .with_size(vec2(1000.0, 36.0))
                .build_ui_state(
                    |ui, status| {
                        let rect = Rect::from_min_size(
                            ui.min_rect().min,
                            vec2(ui.available_width(), komponist_tokens::size::STATUSBAR),
                        );
                        show(ui, rect, status);
                    },
                    status,
                );
            h.run_steps(2);
            let left = h.get_by_label(&warning).rect();
            let right = h.get_by_label(&h.state().right).rect();
            assert!(left.right() + space::SPACE_4 < right.left());
            if let Some(dir) = &preview {
                h.render()
                    .unwrap()
                    .save(dir.join(format!("pdc-{}.png", lang.code())))
                    .unwrap();
            }

            app.dispatch(Action::Edit(EditAction::SetDeviceBypass {
                device,
                bypass: true,
            }));
            wait_for_shortfall(&mut app, 0);
            assert_ne!(model(&app, Instant::now()).left, warning);
        }
    }

    #[test]
    fn plurals() {
        assert_eq!(Lang::De.status_clips_selected(1), "1 Clip ausgewählt");
        assert_eq!(Lang::De.status_clips_selected(3), "3 Clips ausgewählt");
        assert_eq!(Lang::En.status_clips_selected(1), "1 clip selected");
        assert_eq!(Lang::En.status_clips_selected(3), "3 clips selected");
    }
}
