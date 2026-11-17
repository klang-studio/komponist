//! Audio waveform dock, inspector and replacement through the headless app and AccessKit.
use egui::{Event, Key, Modifiers, PointerButton, Pos2, Role, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::ProjectAction;
use komponist_app::ui_state::FocusArea;
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_i18n::Lang;
use komponist_model::{AudioClip, AudioFile, ClipContent, ClipId, Project, Tempo, Tick, edits};
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
const BAR: i64 = 3840;
fn wav(path: &Path, frames: usize, channels: u16) {
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
            let phase = i as f32 * (if ch == 0 { 0.04 } else { 0.065 });
            let envelope = ((i % 24000) as f32 / 24000.0 * std::f32::consts::PI)
                .sin()
                .powi(2);
            w.write_sample((phase.sin() * envelope * 0.65 * 8388607.0) as i32)
                .unwrap();
        }
    }
    w.finalize().unwrap();
}
fn project(path: &Path, channels: u16) -> (Project, ClipId) {
    let mut p = Project::new("Source edit");
    p.tempo = Tempo::new(124.0);
    let (p, t) = edits::add_audio_track(
        &p,
        edits::NewAudioTrack {
            name: Some("Vocal".into()),
            ..Default::default()
        },
    )
    .unwrap();
    edits::add_clip(
        &p,
        t,
        edits::NewClip {
            start: Tick(BAR),
            length: Tick(2 * BAR),
            name: "Verse".into(),
            content: edits::NewClipContent::Audio {
                file: AudioFile::new(path, None, 384000, 48000, channels),
                offset: 24000,
            },
        },
    )
    .unwrap()
}
fn app(path: &Path, channels: u16, lang: Lang) -> (App, ClipId) {
    let (p, id) = project(path, channels);
    let mut app = App::headless(p).with_lang(lang);
    app.dispatch(Action::Ui(UiAction::OpenClip(id)));
    (app, id)
}
fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_max_steps(96)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}
fn data(h: &Harness<'_, App>, id: ClipId) -> AudioClip {
    let ClipContent::Audio(a) = &h.state().project().find_clip(id).unwrap().1.content else {
        panic!("audio")
    };
    a.clone()
}
fn wait_peaks(h: &mut Harness<'_, App>, lang: Lang) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while h
        .query_by_role_and_label(Role::Label, lang.audio_waveform_pending())
        .is_some()
        && Instant::now() < deadline
    {
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.run_steps(3);
}
fn press(h: &mut Harness<'_, App>, p: Pos2, down: bool) {
    h.event(Event::PointerButton {
        pos: p,
        button: PointerButton::Primary,
        pressed: down,
        modifiers: Modifiers::NONE,
    });
}
fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.run_steps(30);
    h.hover_at(from);
    h.step();
    press(h, from, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, false);
    h.run_steps(3);
}
fn enter(h: &mut Harness<'_, App>, lang: Lang, name: &str, text: &str) {
    let p = h
        .get_by_role_and_label(Role::Toolbar, lang.audio_waveform_controls())
        .get_by_role_and_label(Role::SpinButton, name)
        .rect()
        .center();
    h.run_steps(30);
    h.hover_at(p);
    h.step();
    for _ in 0..2 {
        press(h, p, true);
        press(h, p, false);
        h.step();
    }
    h.run_steps(2);
    let label = lang.ui_number_entry(name);
    h.get_by_role_and_label(Role::TextInput, &label)
        .type_text(text);
    h.key_press(Key::Enter);
    h.run_steps(3);
}
#[test]
fn audio_opens_waveform_and_localized_controls_in_both_languages() {
    let dir = crate::common::scratch("audio-editor-open");
    let path = dir.join("verse.wav");
    wav(&path, 384000, 2);
    for lang in [Lang::De, Lang::En] {
        let (app, id) = app(&path, 2, lang);
        let mut h = harness(app);
        wait_peaks(&mut h, lang);
        assert_eq!(h.state().ui().focus, FocusArea::Waveform);
        let title = lang.audio_waveform_title("Verse");
        let canvas = h.get_by_role_and_label(Role::Group, &title);
        assert!(
            canvas
                .get_all_by_role_and_label(Role::Slider, lang.audio_clip_gain_line())
                .count()
                >= 2
        );
        let controls = h.get_by_role_and_label(Role::Toolbar, lang.audio_waveform_controls());
        assert!(
            controls
                .query_by_role_and_label(Role::SpinButton, lang.audio_clip_source_start())
                .is_some()
        );
        assert!(
            h.get_by_role_and_label(Role::Group, lang.inspector_section_clip())
                .query_by_role_and_label(Role::SpinButton, lang.audio_clip_gain())
                .is_some()
        );
        assert_eq!(data(&h, id).file.channels, 2);
    }
}
#[test]
fn toolbar_gain_source_start_repitch_tempo_and_bars_edit_the_document() {
    let dir = crate::common::scratch("audio-editor-fields");
    let path = dir.join("verse.wav");
    wav(&path, 384000, 1);
    let (app, id) = app(&path, 1, Lang::De);
    let mut h = harness(app);
    enter(&mut h, Lang::De, Lang::De.audio_clip_gain(), "−3.5");
    assert_eq!(data(&h, id).gain_db, -3.5);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
    assert_eq!(data(&h, id).gain_db, 0.0);
    enter(
        &mut h,
        Lang::De,
        Lang::De.audio_clip_source_start(),
        "00:01.250",
    );
    assert_eq!(data(&h, id).offset, 60000);
    assert_eq!(
        h.state().project().find_clip(id).unwrap().1.start,
        Tick(BAR)
    );
    h.get_by_role_and_label(Role::Toolbar, Lang::De.audio_waveform_controls())
        .get_by_role_and_label(Role::Button, Lang::De.audio_clip_repitch())
        .click();
    h.run_steps(3);
    enter(
        &mut h,
        Lang::De,
        Lang::De.audio_clip_original_tempo(),
        "120",
    );
    assert_eq!(data(&h, id).original_tempo.bpm, 120.0);
    enter(&mut h, Lang::De, Lang::De.audio_clip_bar_count(), "8");
    assert_eq!(data(&h, id).original_tempo.bpm, 240.0);
}
#[test]
fn waveform_envelope_drag_is_one_undo_step_and_snap_off_allows_free_trim() {
    let dir = crate::common::scratch("audio-editor-drags");
    let path = dir.join("verse.wav");
    wav(&path, 384000, 1);
    let (mut app, id) = app(&path, 1, Lang::De);
    app.ui_mut().arrangement.snap = false;
    let mut h = harness(app);
    let title = Lang::De.audio_waveform_title("Verse");
    let gain = h
        .get_by_role_and_label(Role::Group, &title)
        .get_by_role_and_label(Role::Slider, Lang::De.audio_clip_gain_line())
        .rect();
    drag(&mut h, gain.center(), gain.center() + vec2(0.0, 20.0));
    assert!(data(&h, id).gain_db < 0.0);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
    assert_eq!(data(&h, id).gain_db, 0.0);
    let end = h
        .get_by_role_and_label(Role::Group, &title)
        .get_by_role_and_label(Role::Slider, Lang::De.audio_clip_end())
        .rect();
    let before = h.state().project().find_clip(id).unwrap().1.length;
    drag(&mut h, end.center(), end.center() - vec2(17.0, 0.0));
    let after = h.state().project().find_clip(id).unwrap().1.length;
    assert!(after < before);
    assert_ne!(after.0 % BAR, 0);
}
#[test]
fn replace_audio_preserves_settings_undo_and_rejects_invalid_files() {
    let dir = crate::common::scratch("audio-editor-replace");
    let path = dir.join("old.wav");
    wav(&path, 384000, 1);
    let (mut app, id) = app(&path, 1, Lang::De);
    app.dispatch(Action::Edit(EditAction::SetAudioClipProp {
        clip: id,
        prop: edits::AudioClipProp::GainDb(-4.0),
    }));
    app.dispatch(Action::Edit(EditAction::SetAudioClipProp {
        clip: id,
        prop: edits::AudioClipProp::FadeIn(Tick(120)),
    }));
    let before = app.project().clone();
    let replacement = dir.join("new.wav");
    wav(&replacement, 96000, 2);
    app.dispatch(Action::Project(ProjectAction::ReplaceAudioPath {
        clip: id,
        path: replacement,
    }));
    let (_, clip) = app.project().find_clip(id).unwrap();
    let ClipContent::Audio(a) = &clip.content else {
        unreachable!()
    };
    assert_eq!(a.gain_db, -4.0);
    assert_eq!(a.offset, 24000);
    assert_eq!(a.fade_in, Tick(120));
    assert_eq!(a.file.channels, 2);
    assert!(clip.length < before.find_clip(id).unwrap().1.length);
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(app.project(), &before);
    let bad = dir.join("bad.wav");
    std::fs::write(&bad, b"not audio").unwrap();
    app.dispatch(Action::Project(ProjectAction::ReplaceAudioPath {
        clip: id,
        path: bad,
    }));
    assert_eq!(app.project(), &before);
}
#[test]
fn missing_source_disables_toolbar_and_offers_localized_replacement() {
    let dir = crate::common::scratch("audio-editor-missing");
    for lang in [Lang::De, Lang::En] {
        let (app, _) = app(&dir.join("missing.wav"), 1, lang);
        let h = harness(app);
        assert!(
            h.get_by_role_and_label(Role::Toolbar, lang.audio_waveform_controls())
                .get_by_role_and_label(Role::SpinButton, lang.audio_clip_gain())
                .accesskit_node()
                .is_disabled()
        );
        assert!(
            h.query_by_role_and_label(Role::Button, lang.audio_file_copy_path())
                .is_some()
        );
    }
}
#[test]
fn arrangement_audio_context_menu_opens_file_replacement() {
    let dir = crate::common::scratch("audio-editor-context-menu");
    let path = dir.join("verse.wav");
    wav(&path, 384000, 1);
    let (app, id) = app(&path, 1, Lang::De);
    let mut h = harness(app);
    let (_, clip) = h.state().project().find_clip(id).unwrap();
    let span = komponist_ui::timeline::bar_span(
        Lang::De,
        clip.start,
        clip.end(),
        h.state().project().meter,
    );
    let name = Lang::De.a11y_clip(&clip.name, span);
    let at = h
        .get_by_role_and_label(Role::GridCell, &name)
        .rect()
        .center();
    h.hover_at(at);
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(2);
    h.get_by_role_and_label(Role::MenuItem, Lang::De.audio_file_replace())
        .click();
    h.run_steps(5);
    assert!(
        h.state()
            .dispatched()
            .any(|a| matches!(a, Action::Project(ProjectAction::ReplaceAudio(_)))),
        "menu dispatch log: {:?}",
        h.state().dispatched().collect::<Vec<_>>()
    );
    assert!(
        h.query_by_role_and_label(Role::Dialog, Lang::De.audio_file_replace())
            .is_some()
    );
}

#[test]
fn audio_editor_snapshots() {
    let dir = crate::common::scratch("audio-editor-shots");
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        for (slug, channels, missing) in [
            ("mono", 1, false),
            ("stereo", 2, false),
            ("missing", 1, true),
        ] {
            let path = dir.join(format!("{slug}.wav"));
            if !missing {
                wav(&path, 384000, channels);
            }
            let (mut app, id) = app(&path, channels, lang);
            app.dispatch(Action::Edit(EditAction::SetAudioClipProp {
                clip: id,
                prop: edits::AudioClipProp::FadeIn(Tick(480)),
            }));
            app.dispatch(Action::Edit(EditAction::SetAudioClipProp {
                clip: id,
                prop: edits::AudioClipProp::FadeOut(Tick(960)),
            }));
            app.dispatch(Action::Edit(EditAction::SetAudioClipProp {
                clip: id,
                prop: edits::AudioClipProp::TimeMode(komponist_model::ClipTimeMode::RePitch),
            }));
            let base = Project::clone(app.project());
            for ppp in [1.0, 2.0] {
                let mut app = App::headless(base.clone()).with_lang(lang);
                app.dispatch(Action::Ui(UiAction::OpenClip(id)));
                let mut h = komponist_ui::snapshot::builder(ppp)
                    .with_size(vec2(1920.0, 1080.0))
                    .with_options(SnapshotOptions::new().output_path(
                        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots"),
                    ))
                    .wgpu()
                    .build_ui_state(|ui, app: &mut App| app.show(ui), app);
                h.run_steps(3);
                wait_peaks(&mut h, lang);
                let suffix = if lang == Lang::En { "-en" } else { "" };
                let name = format!("audio-editor-{slug}{suffix}-{ppp}x");
                if std::env::var_os("KOMPONIST_AUDIO_EDITOR_PREVIEW").is_some() {
                    h.render()
                        .unwrap()
                        .save(std::env::temp_dir().join(format!("komponist-{name}.png")))
                        .unwrap();
                }
                // CPU telemetry and resolved scratch paths vary by run; retain their layout
                // while masking data-dependent pixels with fixed-width rectangles.
                let cpu = h
                    .get_by_role_and_label(Role::Status, lang.transport_cpu())
                    .rect();
                h.mask(cpu.expand(2.0));
                let waveform_name = lang.audio_waveform_title("Verse");
                let waveform_x = h
                    .get_by_role_and_label(Role::Group, &waveform_name)
                    .rect()
                    .x_range();
                let resolved = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                let path_label = resolved.display().to_string();
                let mut masks: Vec<_> = h
                    .get_all_by_role_and_label(Role::Label, &path_label)
                    .map(|n| egui::Rect::from_x_y_ranges(waveform_x, n.rect().y_range()))
                    .collect();
                masks.extend(
                    h.get_all_by_role_and_label(Role::Label, lang.audio_file_path())
                        .map(|n| n.rect()),
                );
                for rect in masks {
                    h.mask(rect);
                }
                h.snapshot(name);
                results.extend_harness(&mut h);
            }
        }
    }
    results.unwrap();
}
