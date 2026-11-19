//! Approved Warp UI: real view snapshots and localized model/AccessKit interactions.
use super::audio_editor::{data, enter, harness};
use egui::{Key, Role, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::views::{
    View, ViewCtx,
    audio_editor::{AudioEditor, mapping::Mapping},
};
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_i18n::Lang;
use komponist_model::{
    AudioFile, AudioTake, ClipContent, ClipId, ClipTimeMode, Project, Tempo, Tick, edits,
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
const BAR: i64 = 3840;
fn source(path: &Path, frames: usize, channels: u16) {
    let mut w = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels,
            sample_rate: 48000,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for i in 0..frames {
        for ch in 0..channels {
            let sec = i as f64 / 48000.0;
            let age = (i % 32000) as f64 / 48000.0;
            let pulse = 0.45
                * (-age / 0.035).exp()
                * (age / 0.002).min(1.0)
                * (std::f64::consts::TAU * 95.0 * age).sin();
            let value = 0.2 * (std::f64::consts::TAU * 440.0 * sec).sin()
                + pulse
                + 0.015 * (std::f64::consts::TAU * (660.0 + 220.0 * f64::from(ch)) * sec).sin();
            w.write_sample((value * 8388607.0) as i32).unwrap();
        }
    }
    w.finalize().unwrap();
}
fn project(path: &Path, frames: u64, channels: u16, trim: bool) -> (Project, ClipId) {
    let mut p = Project::new("Warp");
    p.tempo = Tempo::new(120.0);
    let (p, t) = edits::add_audio_track(&p, edits::NewAudioTrack::default()).unwrap();
    let (p, c) = edits::add_clip(
        &p,
        t,
        edits::NewClip {
            start: Tick(4 * BAR),
            length: Tick(2 * BAR),
            name: "Pulse 90".into(),
            content: edits::NewClipContent::Audio {
                file: AudioFile::new(path, None, frames, 48000, channels),
                offset: if trim { 64000 } else { 0 },
            },
        },
    )
    .unwrap();
    let p =
        edits::set_audio_clip_prop(&p, c, edits::AudioClipProp::OriginalTempo(Tempo::new(90.0)))
            .unwrap();
    (p, c)
}
fn app(p: Project, c: ClipId, t: Lang) -> App {
    let mut a = App::headless(p).with_lang(t);
    a.dispatch(Action::Ui(UiAction::OpenClip(c)));
    a
}
fn mode(h: &mut Harness<'_, App>, t: Lang, name: &str, inspector: bool) {
    let scope = if inspector {
        Role::Group
    } else {
        Role::Toolbar
    };
    let label = if inspector {
        t.inspector_section_clip()
    } else {
        t.audio_clip_mode()
    };
    h.get_by_role_and_label(scope, label)
        .get_by_role_and_label(Role::ComboBox, t.audio_clip_mode())
        .click();
    h.run_steps(2);
    h.get_by_role_and_label(Role::ListBoxOption, name).click();
    h.run_steps(3);
}
#[test]
fn exclusive_modes_are_opt_in_shared_and_undoable_in_both_languages() {
    for t in [Lang::De, Lang::En] {
        let dir = crate::common::scratch(&format!("warp-mode-{t:?}"));
        let path = dir.join("Pulse 90.wav");
        source(&path, 384000, 2);
        let (p, c) = project(&path, 384000, 2, false);
        let mut h = harness(app(p, c, t));
        assert!(!data(&h, c).warp_enabled());
        assert_eq!(data(&h, c).time_mode, ClipTimeMode::Fixed);
        h.get_by_role_and_label(Role::Toolbar, t.audio_clip_mode())
            .get_by_role_and_label(Role::ComboBox, t.audio_clip_mode())
            .click();
        h.run_steps(2);
        h.key_press(Key::Escape);
        h.run_steps(2);
        assert!(!data(&h, c).warp_enabled());
        assert!(
            h.get_by_role_and_label(Role::Toolbar, t.audio_clip_mode())
                .get_by_role_and_label(Role::ComboBox, t.audio_clip_mode())
                .is_focused(),
            "mode retains focus after Escape"
        );
        h.key_press(Key::Space);
        h.run_steps(2);
        h.key_press(Key::ArrowDown);
        h.run_steps(2);
        h.key_press(Key::ArrowDown);
        h.run_steps(2);
        h.key_press(Key::Enter);
        h.run_steps(3);
        assert!(data(&h, c).warp_enabled());
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(2);
        mode(&mut h, t, t.audio_clip_warp(), false);
        assert!(data(&h, c).warp_enabled());
        assert_eq!(
            h.state().undo_text().as_deref(),
            Some(t.undo_clip_audio_mode())
        );
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(2);
        assert!(!data(&h, c).warp_enabled());
        h.state_mut().dispatch(Action::Edit(EditAction::Redo));
        h.run_steps(2);
        assert!(data(&h, c).warp_enabled());
        mode(&mut h, t, t.audio_clip_repitch(), true);
        assert!(!data(&h, c).warp_enabled());
        assert_eq!(data(&h, c).time_mode, ClipTimeMode::RePitch);
        mode(&mut h, t, t.audio_clip_fixed(), false);
        assert!(!data(&h, c).warp_enabled());
        assert_eq!(data(&h, c).time_mode, ClipTimeMode::Fixed);
        let before = h.state().undo_text();
        mode(&mut h, t, t.audio_clip_fixed(), true);
        assert_eq!(h.state().undo_text(), before);
    }
}
#[test]
fn full_source_bars_do_not_fit_the_trimmed_clip_and_tempo_undo_restores_phase() {
    for t in [Lang::De, Lang::En] {
        let dir = crate::common::scratch(&format!("warp-bars-{t:?}"));
        let path = dir.join("Pulse 90.wav");
        source(&path, 384000, 2);
        let (p, c) = project(&path, 384000, 2, true);
        let p = edits::set_audio_clip_warp(&p, c, true).unwrap();
        let p = edits::trim_audio_clip_start(&p, c, Tick(4 * BAR + 137)).unwrap();
        let mut h = harness(app(p, c, t));
        assert_eq!(
            komponist_app::views::audio_editor::fields::source_bars(
                &data(&h, c),
                h.state().project()
            ),
            3.0
        );
        let old = data(&h, c);
        let length = h.state().project().find_clip(c).unwrap().1.length;
        enter(&mut h, t, t.audio_clip_bar_count(), "2");
        assert_eq!(data(&h, c).original_tempo.bpm, 60.0);
        assert!(h.state().project().find_clip(c).unwrap().1.length <= length);
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(2);
        assert_eq!(data(&h, c), old);
        enter(&mut h, t, t.audio_clip_original_tempo(), "abc");
        assert_eq!(data(&h, c), old);
        enter(&mut h, t, t.audio_clip_original_tempo(), "999");
        assert_eq!(data(&h, c).original_tempo.bpm, 240.0);
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(2);
        assert_eq!(data(&h, c), old);
    }
}
#[test]
fn take_changes_keep_shared_warp_and_refresh_full_source_bars() {
    let dir = crate::common::scratch("warp-takes");
    let path = dir.join("Pulse 90.wav");
    source(&path, 384000, 2);
    let other = dir.join("Short.wav");
    source(&other, 48000, 1);
    let (p, c) = project(&path, 384000, 2, false);
    let p = edits::set_audio_clip_warp(&p, c, true).unwrap();
    let (p, _) = edits::add_audio_take(
        &p,
        c,
        AudioTake {
            name: "Short".into(),
            file: AudioFile::new(&other, None, 48000, 48000, 1),
            offset: 0,
        },
    )
    .unwrap();
    let mut h = harness(app(p, c, Lang::En));
    assert!(data(&h, c).warp_enabled());
    assert_eq!(data(&h, c).original_tempo.bpm, 90.0);
    let short = data(&h, c);
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SelectAudioTake {
            clip: c,
            take: 0,
        }));
    h.run_steps(2);
    assert_eq!(
        komponist_app::views::audio_editor::fields::source_bars(&data(&h, c), h.state().project()),
        3.0
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
    assert_eq!(data(&h, c), short);
}

#[test]
fn tempo_detection_uses_the_active_take_and_only_confirmed_tempo_is_undoable() {
    use komponist_app::project_io::fmt_bpm;
    use komponist_audiofile::{FilePool, TempoState};

    for t in [Lang::De, Lang::En] {
        let dir = crate::common::scratch(&format!("warp-tempo-{t:?}"));
        let base = dir.join("Base.wav");
        let path = dir.join("Pulse 90.wav");
        source(&base, 48000, 1);
        komponist_app::samples::demo_with_kit(komponist_app::showcase::showcase_project(), &dir)
            .unwrap();
        let (p, c) = project(&base, 48000, 1, false);
        let p = edits::set_audio_clip_prop(
            &p,
            c,
            edits::AudioClipProp::OriginalTempo(Tempo::new(120.0)),
        )
        .unwrap();
        let (p, _) = edits::add_audio_take(
            &p,
            c,
            AudioTake {
                name: "Pulse 90".into(),
                file: AudioFile::new(&path, None, 256000, 48000, 2),
                offset: 32000,
            },
        )
        .unwrap();
        let active = FilePool::global().open(&path).unwrap();
        let other = FilePool::global().open(&base).unwrap();
        let mut h = harness(app(p, c, t));
        let open = |h: &mut Harness<'_, App>| {
            if t == Lang::De {
                h.get_by_role_and_label(Role::Group, t.audio_file())
                    .get_by_role_and_label(Role::Button, t.audio_file_actions())
                    .click();
            } else {
                let inspector = h
                    .get_by_role_and_label(Role::Group, t.audio_file())
                    .get_by_role_and_label(Role::Button, t.audio_file_actions())
                    .rect();
                h.get_all_by_role_and_label(Role::Button, t.audio_file_actions())
                    .find(|button| button.rect() != inspector)
                    .expect("file actions in the dock")
                    .click();
            }
            h.run_steps(3);
        };
        let before = data(&h, c);
        assert_eq!(before.file.abs, path);
        open(&mut h);
        h.get_by_role_and_label(Role::MenuItem, t.audio_tempo_detect())
            .click();
        let deadline = Instant::now() + Duration::from_secs(10);
        let result = loop {
            h.run_steps(1);
            match active.tempo_state() {
                TempoState::Idle | TempoState::Pending if Instant::now() < deadline => {
                    std::thread::yield_now();
                }
                TempoState::Ready(Some(result)) => break result,
                state => panic!("no tempo estimate for Pulse 90: {state:?}"),
            }
        };
        assert!((result.bpm - 90.0).abs() <= 1.0, "estimate: {result:?}");
        assert!(matches!(other.tempo_state(), TempoState::Idle));
        assert_eq!(data(&h, c), before);
        assert_eq!(h.state().undo_text(), None);
        h.run_steps(3);
        h.get_by_role_and_label(
            Role::MenuItem,
            &t.audio_tempo_estimate(
                fmt_bpm(result.bpm),
                format!("{:.0}", result.confidence * 100.0),
            ),
        );
        h.get_by_role_and_label(
            Role::MenuItem,
            &t.audio_tempo_use_half(fmt_bpm((result.bpm * 50.0).round() / 100.0)),
        );
        h.render()
            .unwrap()
            .save(dir.join("tempo-result.png"))
            .unwrap();
        h.key_press(Key::Escape);
        h.run_steps(3);
        assert_eq!(data(&h, c), before);
        assert_eq!(h.state().undo_text(), None);

        open(&mut h);
        h.get_by_role_and_label(
            Role::MenuItem,
            &t.audio_tempo_use_double(fmt_bpm(result.bpm * 2.0)),
        )
        .click();
        h.run_steps(3);
        let after = data(&h, c);
        assert_eq!(after.original_tempo.bpm, result.bpm * 2.0);
        assert_eq!(after.time_mode, before.time_mode);
        assert_eq!(after.warp_enabled(), before.warp_enabled());
        assert_eq!(after.file, before.file);
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(3);
        assert_eq!(data(&h, c), before);
    }
}

#[test]
fn failed_source_tempo_can_be_retried_for_the_same_open_file() {
    use komponist_audiofile::{FilePool, TempoState};
    use std::fs::{FileTimes, OpenOptions};

    for t in [Lang::De, Lang::En] {
        let dir = crate::common::scratch(&format!("warp-retry-tempo-{t:?}"));
        let path = dir.join("Retry.wav");
        let write = |first_sample| {
            let mut wav = hound::WavWriter::create(
                &path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 48000,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            wav.write_sample(first_sample).unwrap();
            for _ in 1..48000 {
                wav.write_sample(0.0_f32).unwrap();
            }
            wav.finalize().unwrap();
        };
        write(f32::NAN);
        let file = FilePool::global().open(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let (p, c) = project(&path, 48000, 1, false);
        let mut h = harness(app(p, c, t));
        let unchanged = data(&h, c);
        h.get_by_role_and_label(Role::Group, t.audio_file())
            .get_by_role_and_label(Role::Button, t.audio_file_actions())
            .click();
        h.run_steps(3);
        h.get_by_role_and_label(Role::MenuItem, t.audio_tempo_detect())
            .click();
        let wait_for_state = |h: &mut Harness<'_, App>, expected: fn(&TempoState) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                h.run_steps(1);
                let state = file.tempo_state();
                if expected(&state) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "unexpected tempo state {state:?}"
                );
                std::thread::yield_now();
            }
            h.run_steps(3);
        };
        wait_for_state(&mut h, |state| matches!(state, TempoState::Failed(_)));
        assert!(
            h.get_by_role_and_label(Role::MenuItem, t.audio_tempo_failed())
                .accesskit_node()
                .is_disabled()
        );
        h.render()
            .unwrap()
            .save(dir.join("tempo-retry.png"))
            .unwrap();

        // Repair the samples without changing the file identity so the old handle stays loaded.
        write(0.0_f32);
        OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(Arc::ptr_eq(&file, &FilePool::global().open(&path).unwrap()));
        h.get_by_role_and_label(Role::MenuItem, t.audio_tempo_retry())
            .click();
        wait_for_state(&mut h, |state| matches!(state, TempoState::Ready(None)));
        h.get_by_role_and_label(Role::MenuItem, t.audio_tempo_unknown());
        assert_eq!(data(&h, c), unchanged);
        assert_eq!(h.state().undo_text(), None);
    }
}

struct Scene {
    app: App,
    ui: komponist_app::ui_state::UiState,
    view: AudioEditor,
    meters: komponist_app::meters::Meters,
    keymap: komponist_app::keymap::Keymap,
    lang: Lang,
    last_seek: Option<Tick>,
}
impl Scene {
    fn new(p: Project, c: ClipId, t: Lang) -> Self {
        let app = app(p, c, t);
        let ui = app.ui().clone();
        Self {
            app,
            ui,
            view: AudioEditor::default(),
            meters: komponist_app::meters::Meters::new(),
            keymap: komponist_app::keymap::Keymap::default_bindings(),
            lang: t,
            last_seek: None,
        }
    }
    fn show(&mut self, ui: &mut egui::Ui) {
        let mut actions = Vec::new();
        let p = Arc::clone(self.app.project());
        let mut ctx = ViewCtx::new(
            &p,
            &mut self.ui,
            self.app.engine(),
            &mut self.meters,
            &self.keymap,
            &mut actions,
        );
        ctx.lang = self.lang;
        self.view.show(ui, &mut ctx);
        for action in actions {
            if let Action::Transport(TransportAction::Locate(t)) = &action {
                self.last_seek = Some(*t);
            }
            self.app.dispatch(action);
        }
    }
}
fn scene(p: Project, c: ClipId, t: Lang, width: f32, ppp: f32) -> Harness<'static, Scene> {
    komponist_ui::snapshot::builder(ppp)
        .with_size(vec2(width, 400.0))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .wgpu()
        .build_ui_state(|ui, s: &mut Scene| s.show(ui), Scene::new(p, c, t))
}
fn peaks(h: &mut Harness<'_, Scene>) {
    let start = Instant::now();
    loop {
        h.run_steps(2);
        if h.query_by_role_and_label(Role::Label, h.state().lang.audio_waveform_pending())
            .is_none()
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[test]
fn source_peaks_seek_in_song_time_and_zoom_keeps_the_same_native_source() {
    let dir = crate::common::scratch("warp-seek");
    let path = dir.join("Pulse 90.wav");
    source(&path, 384000, 2);
    let (p, c) = project(&path, 384000, 2, true);
    let p = edits::set_audio_clip_warp(&p, c, true).unwrap();
    let mut h = scene(p, c, Lang::En, 1280.0, 1.0);
    h.run_steps(3);
    peaks(&mut h);
    let ruler = h
        .get_by_role_and_label(Role::Slider, Lang::En.a11y_ruler())
        .rect();
    let ppt = h.state().ui.waveform.px_per_bar / BAR as f32;
    let scroll = h.state().ui.waveform.scroll_tick;
    let expected = Tick(5 * BAR);
    let x = ruler.left() + (expected.0 as f64 - scroll) as f32 * ppt;
    h.hover_at(egui::pos2(x, ruler.center().y));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: egui::pos2(x, ruler.center().y),
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    assert_eq!(h.state().last_seek, Some(expected));
    let clip = h.state().app.project().find_clip(c).unwrap().1;
    let ClipContent::Audio(a) = &clip.content else {
        unreachable!()
    };
    assert_eq!(
        Mapping::new(clip, a, h.state().app.project().tempo).source_frame(expected),
        192000
    );
    let canvas = h
        .get_by_role_and_label(Role::Group, &Lang::En.audio_waveform_title("Pulse 90"))
        .rect();
    let pointer = egui::pos2(x, canvas.center().y);
    let anchor = scroll + f64::from((pointer.x - ruler.left()) / ppt);
    h.hover_at(pointer);
    h.step();
    h.event_modifiers(
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 48.0),
            modifiers: egui::Modifiers::CTRL,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        },
        egui::Modifiers::CTRL,
    );
    h.run_steps(20);
    let new = h.state().ui.waveform.scroll_tick
        + f64::from((pointer.x - ruler.left()) / (h.state().ui.waveform.px_per_bar / BAR as f32));
    assert!((anchor - new).abs() < 0.01);
}
#[test]
fn snapshots_of_warp_modes_full_and_trimmed_sources_in_both_languages() {
    let dir = crate::common::scratch("warp-snapshots");
    let mut results = SnapshotResults::new();
    for t in [Lang::De, Lang::En] {
        for (slug, width, channels, trim, mode, missing) in [
            ("stereo", 1280.0, 2, false, 2, false),
            ("narrow", 640.0, 2, false, 2, false),
            ("trimmed", 1280.0, 2, true, 2, false),
            ("mono", 640.0, 1, false, 2, false),
            ("fixed", 1280.0, 2, false, 0, false),
            ("repitch", 1280.0, 2, false, 1, false),
            ("missing", 640.0, 2, false, 2, true),
        ] {
            let case = dir.join(format!("{slug}-{t:?}"));
            std::fs::create_dir_all(&case).unwrap();
            let path = case.join("Pulse 90.wav");
            let frames = if trim { 384000 } else { 256000 };
            if !missing {
                source(&path, frames, channels);
            }
            let (mut p, c) = project(&path, frames as u64, channels, trim);
            if mode == 2 {
                p = edits::set_audio_clip_warp(&p, c, true).unwrap();
            } else if mode == 1 {
                p = edits::set_audio_clip_prop(
                    &p,
                    c,
                    edits::AudioClipProp::TimeMode(ClipTimeMode::RePitch),
                )
                .unwrap();
            }
            for ppp in [1.0, 2.0] {
                let mut h = scene(p.clone(), c, t, width, ppp);
                h.run_steps(3);
                if !missing {
                    peaks(&mut h);
                }
                let suffix = if t == Lang::En { "-en" } else { "" };
                let name = format!("audio-warp-{slug}{suffix}-{ppp}x");
                for label in [
                    t.audio_clip_mode(),
                    t.audio_clip_original_tempo(),
                    t.audio_clip_bar_count(),
                ] {
                    for role in [Role::ComboBox, Role::SpinButton] {
                        for node in h.query_all_by_role_and_label(role, label) {
                            assert!(
                                node.rect().right() <= width && node.rect().left() >= 0.0,
                                "essential control outside dock: {label}"
                            );
                        }
                    }
                }
                for label in [
                    t.audio_file_actions(),
                    t.audio_zoom_out(),
                    t.audio_zoom_in(),
                ] {
                    let rect = h.get_by_role_and_label(Role::Button, label).rect();
                    assert!(
                        rect.right() <= width - komponist_tokens::space::SPACE_2 * 2.0,
                        "icon extends past dock padding: {label}"
                    );
                }
                if missing {
                    let masks: Vec<_> = h
                        .query_all_by_role_and_label(Role::Label, &path.display().to_string())
                        .map(|n| egui::Rect::from_x_y_ranges(0.0..=width, n.rect().y_range()))
                        .collect();
                    for rect in masks {
                        h.mask(rect);
                    }
                }
                results.add(h.try_snapshot(name));
            }
        }
    }
    results.unwrap();
}

#[test]
fn source_readout_never_pairs_clamped_source_zero_with_an_unrelated_song_position() {
    for t in [Lang::De, Lang::En] {
        let dir = crate::common::scratch(&format!("warp-readout-{t:?}"));
        let path = dir.join("Pulse 90.wav");
        source(&path, 384000, 2);
        let (p, c) = project(&path, 384000, 2, true);
        let p = edits::set_audio_clip_warp(&p, c, true).unwrap();
        let expected = t.audio_source_time(
            "00:01.333",
            komponist_model::format::fmt_pos(Tick(4 * BAR), p.meter),
        );
        let mut h = scene(p, c, t, 1600.0, 1.0);
        h.run_steps(3);
        peaks(&mut h);
        assert!(
            h.query_by_role_and_label(Role::Label, &expected).is_some(),
            "out-of-clip playhead uses valid clip anchor"
        );
        // Opening fits the clip. Zoom out explicitly to expose empty space beyond the source.
        h.state_mut().ui.waveform.px_per_bar = 400.0;
        h.run_steps(2);
        h.state_mut().ui.waveform.scroll_tick = (4 * BAR) as f64;
        h.run_steps(2);
        let canvas = h
            .get_by_role_and_label(Role::Group, &t.audio_waveform_title("Pulse 90"))
            .rect();
        h.hover_at(egui::pos2(canvas.right() - 2.0, canvas.center().y));
        h.run_steps(2);
        assert!(
            h.query_by_role_and_label(Role::Label, &expected).is_some(),
            "empty canvas uses valid clip anchor"
        );
    }
}

#[test]
fn ui_opt_in_exports_the_same_pitch_preserving_audio_as_the_offline_playback_path() {
    use komponist_app::actions::{BitDepth, ExportRange};
    use komponist_engine::compile::{self, CompileEnv};
    use komponist_engine::{Command, Engine, EngineConfig};
    struct Env;
    impl CompileEnv for Env {
        fn midi_port(&self, _: &str) -> Option<komponist_engine::program::MidiPort> {
            None
        }
        fn device_latency(&self, _: komponist_model::DeviceId) -> u32 {
            0
        }
        fn sample_rate(&self) -> f64 {
            48000.0
        }
    }
    let dir = crate::common::scratch("warp-ui-export");
    let path = dir.join("Pulse 90.wav");
    source(&path, 256000, 2);
    let (p, c) = project(&path, 256000, 2, false);
    let mut h = harness(app(p, c, Lang::En));
    mode(&mut h, Lang::En, Lang::En.audio_clip_warp(), false);
    assert!(h.state().ui().selection.clips.contains(&c));
    let p = Project::clone(h.state().project());
    let mut settings = super::export::export_settings(&dir, ExportRange::Selection);
    settings.tail_seconds = 0.0;
    settings.sample_rate = 48000;
    settings.normalize = false;
    settings.dither = false;
    settings.bit_depth = BitDepth::Float32;
    let output = settings.folder.join(&settings.file_name);
    super::export::export(h.state_mut(), settings);
    let wav = super::export::read(&output);
    assert_eq!(wav.frames, 192000);
    std::fs::write(
        dir.join("Warp.komponist"),
        komponist_model::persist::to_json(&p).unwrap(),
    )
    .unwrap();
    let program = compile::build(&p, None, &Env);
    let (mut engine, mut handle) = Engine::new(EngineConfig::default());
    for command in [
        Command::SetProgram(Box::new(program)),
        Command::Locate(Tick(4 * BAR)),
        Command::Play,
    ] {
        handle.send(command).unwrap();
    }
    let mut playback = Vec::new();
    while playback.len() < wav.samples.len() {
        let n = ((wav.samples.len() - playback.len()) / 2).min(64);
        playback.extend_from_slice(engine.render(n));
        while handle.poll().is_some() {}
    }
    assert_eq!(
        wav.samples, playback,
        "UI export and offline playback must share DSP chunks"
    );
    let left: Vec<_> = wav
        .samples
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| f[0])
        .collect();
    let x = &left[60000..69600];
    let crossings: Vec<_> = x
        .windows(2)
        .enumerate()
        .filter(|(_, p)| p[0] <= 0.0 && p[1] > 0.0)
        .map(|(i, p)| i as f64 - f64::from(p[0]) / f64::from(p[1] - p[0]))
        .collect();
    let hz = (crossings.len() - 1) as f64 * 48000.0 / (crossings.last().unwrap() - crossings[0]);
    assert!((hz - 440.0).abs() < 2.0, "Warp carrier frequency {hz}");
}
