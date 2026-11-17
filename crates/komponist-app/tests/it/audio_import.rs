//! Linked import, atomic undo, multi-file placement, refusal and keyboard/UI contracts.

use std::path::{Path, PathBuf};

use egui::accesskit::Role;
use egui::{Modifiers, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::dialogs::file::{Entry, FileBrowser, Output, Purpose};
use komponist_app::ui_state::{BrowserSegment, StatusKind};
use komponist_app::{Action, App, EditAction, ProjectAction};
use komponist_i18n::Lang;
use komponist_model::edits::{self, NewAudioTrack};
use komponist_model::{ClipContent, Project, Tempo, Tick};

fn wav(dir: &Path, name: &str, rate: u32, seconds: u32) -> PathBuf {
    let path = dir.join(name);
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(&path, spec).unwrap();
    for frame in 0..rate * seconds {
        writer
            .write_sample((10_000.0 * (frame as f32 * 0.1).sin()) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
    path
}

fn project() -> Project {
    let mut p = Project::new("Import");
    p.tempo = Tempo::new(120.0);
    p
}
fn import(app: &mut App, paths: Vec<PathBuf>, target: Option<komponist_model::TrackId>, at: i64) {
    app.dispatch(Action::Project(ProjectAction::ImportAudioPaths {
        paths,
        target,
        at: Tick(at),
    }));
}

#[test]
fn multiple_files_make_separate_linked_tracks_and_undo_as_one_step() {
    let dir = crate::export::scratch("audio-import-tracks");
    let paths = vec![
        wav(&dir, "one.wav", 44_100, 1),
        wav(&dir, "two.wav", 48_000, 2),
    ];
    let original = project();
    let mut app = App::headless(original.clone());
    import(&mut app, paths.clone(), None, 8000);
    assert_eq!(app.project().tracks.len(), 2);
    for (track, path) in app.project().tracks.iter().zip(&paths) {
        assert!(track.is_audio());
        assert_eq!(track.clips[0].start, Tick::ZERO);
        let ClipContent::Audio(audio) = &track.clips[0].content else {
            panic!("audio");
        };
        assert_eq!(&audio.file.abs, path);
        assert_eq!(
            audio.file.sample_rate,
            if path.ends_with("one.wav") {
                44_100
            } else {
                48_000
            }
        );
        assert!(!track.arm);
    }
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        2,
        "import never copies sources"
    );
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(**app.project(), original);
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(app.project().tracks.len(), 2);
}

#[test]
fn existing_track_imports_in_sequence_with_grid_or_free_placement() {
    let dir = crate::export::scratch("audio-import-sequence");
    let paths = vec![
        wav(&dir, "one.wav", 48_000, 1),
        wav(&dir, "two.wav", 48_000, 1),
    ];
    for snapping in [true, false] {
        let (p, track) = edits::add_audio_track(&project(), NewAudioTrack::default()).unwrap();
        let mut app = App::headless(p);
        app.ui_mut().arrangement.snap = snapping;
        import(&mut app, paths.clone(), Some(track), 17);
        let clips = &app.project().track(track).unwrap().clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[0].start, Tick(if snapping { 0 } else { 17 }));
        assert_eq!(clips[1].start, Tick(if snapping { 3840 } else { 1937 }));
    }
}

#[test]
fn import_cuts_overlap_but_refuses_midi_and_invalid_files_without_partial_edits() {
    let dir = crate::export::scratch("audio-import-refusals");
    let long = wav(&dir, "long.wav", 48_000, 4);
    let short = wav(&dir, "short.wav", 48_000, 2);
    let mut app = App::headless(project());
    import(&mut app, vec![long], None, 0);
    let track = app.project().tracks[0].id;
    import(&mut app, vec![short.clone()], Some(track), 3840);
    assert_eq!(app.project().tracks[0].clips.len(), 2);
    assert_eq!(app.project().tracks[0].clips[0].length, Tick(3840));
    let before = app.project().clone();
    import(
        &mut app,
        vec![short.clone(), dir.join("missing.wav")],
        None,
        0,
    );
    assert_eq!(app.project(), &before);
    assert_eq!(app.ui().status.as_ref().unwrap().kind, StatusKind::Error);
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    let midi = app.project().tracks.last().unwrap().id;
    let before = app.project().clone();
    import(&mut app, vec![short], Some(midi), 0);
    assert_eq!(app.project(), &before);
    assert_eq!(
        app.ui().status.as_ref().unwrap().text,
        Lang::De.status_drop_refused_midi()
    );
}

#[test]
fn import_dialog_multiselection_filters_directories_and_preserves_list_order() {
    let mut b = FileBrowser::new(
        Lang::De,
        Purpose::ImportAudio,
        PathBuf::from("/missing"),
        String::new(),
    );
    b.entries = ["a.wav", "b.flac", "c.aiff"]
        .iter()
        .map(|name| Entry {
            name: (*name).into(),
            path: PathBuf::from(name),
            is_dir: false,
        })
        .collect();
    b.select(0);
    b.select_with(2, Modifiers::CTRL);
    assert_eq!(
        b.confirm(),
        Some(Output::ChosenMany(vec![
            PathBuf::from("a.wav"),
            PathBuf::from("c.aiff")
        ]))
    );
    b.select_with(2, Modifiers::SHIFT);
    assert_eq!(b.multi_selected.len(), 1);
    b.select(0);
    b.select_with(2, Modifiers::SHIFT);
    assert_eq!(b.multi_selected.len(), 3);
}

#[test]
fn new_audio_tracks_offer_bus_inputs_and_skip_occupied_hardware_channels() {
    use komponist_app::dialogs::new_track::{first_unused_input, input_choices};
    use komponist_model::{AudioInput, HardwareInput};
    let (p, _) = edits::add_audio_track(
        &project(),
        NewAudioTrack {
            input: AudioInput::Hardware(HardwareInput {
                first: 0,
                stereo: true,
            }),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(first_unused_input(&p, 2), AudioInput::None);
    assert_eq!(
        first_unused_input(&p, 3),
        AudioInput::Hardware(HardwareInput {
            first: 2,
            stereo: false
        })
    );
    let (p, bus) = edits::add_bus(&p, Default::default()).unwrap();
    let choices = input_choices(&p, Lang::De, 3);
    assert_eq!(choices[0].0, AudioInput::None);
    assert!(
        choices
            .iter()
            .any(|(input, _)| *input == AudioInput::Bus(bus))
    );
}

#[test]
fn opening_a_project_keeps_missing_audio_links_and_lists_the_clips() {
    let dir = crate::export::scratch("missing-audio-open");
    let source = wav(&dir, "gone.wav", 48_000, 2);
    let mut app = App::headless(project());
    import(&mut app, vec![source.clone()], None, 0);
    let expected = app.project().clone();
    let file = dir.join("Missing.komponist");
    komponist_app::project_io::save_project(&file, app.project()).unwrap();
    std::fs::remove_file(source).unwrap();
    let mut opened = App::headless(project());
    opened.dispatch(Action::Project(ProjectAction::OpenPath(file)));
    assert_eq!(opened.project(), &expected);
    assert!(matches!(
        opened.ui().dialog,
        Some(komponist_app::ui_state::Dialog::MissingPlugins(_))
    ));
    assert_eq!(
        komponist_app::dialogs::missing_plugins::missing_audio(opened.project(), Some(&dir)).len(),
        1
    );
}

#[test]
fn collect_reports_skipped_non_audio_sources_in_both_languages() {
    let dir = crate::export::scratch("audio-collect-skipped");
    let source = dir.join("private.txt");
    std::fs::write(&source, "This is not audio").unwrap();
    for lang in [Lang::De, Lang::En] {
        let (p, track) = edits::add_audio_track(&project(), NewAudioTrack::default()).unwrap();
        let (p, _) = edits::add_clip(
            &p,
            track,
            edits::NewClip {
                name: "Skipped".into(),
                start: Tick::ZERO,
                length: Tick(960),
                content: edits::NewClipContent::Audio {
                    file: komponist_model::AudioFile::new(&source, None, 48_000, 48_000, 1),
                    offset: 0,
                },
            },
        )
        .unwrap();
        let file = dir.join(format!("Skipped-{lang:?}.komponist"));
        komponist_app::project_io::save_project(&file, &p).unwrap();
        let mut app = App::headless(project()).with_lang(lang);
        app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
        app.dispatch(Action::Project(ProjectAction::CollectAndSave));
        let status = app.ui().status.as_ref().unwrap();
        assert_eq!(status.kind, StatusKind::Error);
        assert!(
            status
                .text
                .contains(&lang.status_collect_skipped(1, "private.txt"))
        );
        assert!(
            !dir.join(format!("Skipped-{lang:?} Samples/private.txt"))
                .exists()
        );
    }
}

fn harness(app: App) -> Harness<'static, App> {
    let snapshots = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1280.0, 800.0))
        .with_options(SnapshotOptions::new().output_path(snapshots))
        .with_max_steps(64)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

fn mask_cpu<S>(h: &mut Harness<'_, S>, lang: Lang) {
    if let Some(cpu) = h.query_by_role_and_label(Role::Status, lang.transport_cpu()) {
        let rect = cpu.rect();
        h.mask(rect.expand(2.0));
    }
}

fn begin_drag(h: &mut Harness<'_, App>, from: egui::Pos2, to: egui::Pos2) {
    h.hover_at(from);
    h.step();
    h.event(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
}

fn release(h: &mut Harness<'_, App>, at: egui::Pos2) {
    h.event(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(3);
}

#[test]
fn audio_track_dialog_and_file_browser_render_in_both_languages() {
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let app = App::headless(project()).with_lang(lang);
        let mut h = harness(app);
        h.key_press_modifiers(
            Modifiers {
                command: true,
                shift: true,
                ..Default::default()
            },
            egui::Key::T,
        );
        h.run_steps(3);
        h.get_by_role_and_label(Role::Dialog, lang.new_track_title());
        h.get_by_role_and_label(Role::RadioButton, lang.new_track_type_audio());
        assert!(
            h.query_by_role_and_label(Role::TextInput, lang.new_track_search())
                .is_none()
        );
        mask_cpu(&mut h, lang);
        h.snapshot(if lang == Lang::De {
            "audio-new-track-1x"
        } else {
            "audio-new-track-en-1x"
        });
        h.get_by_role_and_label(Role::Button, lang.new_track_create())
            .click();
        h.run_steps(3);
        assert!(h.state().project().tracks[0].is_audio());
        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::I);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Dialog, lang.menu_import_audio());
        h.get_by_role_and_label(Role::Button, lang.dialog_cancel())
            .click();
        h.run_steps(3);
        results.extend_harness(&mut h);
        let dir = crate::export::scratch(if lang == Lang::De {
            "audio-browser-de"
        } else {
            "audio-browser-en"
        });
        wav(&dir, "Shaker Loop.wav", 48_000, 1);
        let mut app = App::headless(project()).with_lang(lang);
        app.ui_mut().browser.segment = BrowserSegment::Files;
        app.ui_mut().browser.directory = Some(dir.clone());
        let mut h = harness(app);
        for _ in 0..100 {
            if h.query_by_role_and_label(Role::ListItem, "Shaker Loop.wav")
                .is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            h.run_steps(2);
        }
        let row = h.get_by_role_and_label(Role::ListItem, "Shaker Loop.wav");
        assert!(
            row.accesskit_node()
                .description()
                .is_some_and(|s| s.contains("00:01"))
        );
        let path_label = komponist_app::dialogs::format::tilde(&dir);
        let path_rect = h.get_by_role_and_label(Role::Label, &path_label).rect();
        h.mask(path_rect);
        mask_cpu(&mut h, lang);
        h.snapshot(if lang == Lang::De {
            "audio-browser-files-1x"
        } else {
            "audio-browser-files-en-1x"
        });
        let from = h
            .get_by_role_and_label(Role::ListItem, "Shaker Loop.wav")
            .rect()
            .center();
        let target = egui::pos2(650.0, 150.0);
        begin_drag(&mut h, from, target);
        h.mask(path_rect);
        mask_cpu(&mut h, lang);
        h.snapshot(if lang == Lang::De {
            "audio-import-drop-1x"
        } else {
            "audio-import-drop-en-1x"
        });
        release(&mut h, target);
        assert_eq!(h.state().project().tracks.len(), 1);
        assert!(h.state().project().tracks[0].is_audio());
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.state_mut()
            .dispatch(Action::Edit(EditAction::NewMidiTrack));
        h.run_steps(3);
        let from = h
            .get_by_role_and_label(Role::ListItem, "Shaker Loop.wav")
            .rect()
            .center();
        let target = egui::pos2(650.0, 124.0);
        begin_drag(&mut h, from, target);
        h.mask(path_rect);
        mask_cpu(&mut h, lang);
        h.snapshot(if lang == Lang::De {
            "audio-import-refused-1x"
        } else {
            "audio-import-refused-en-1x"
        });
        release(&mut h, target);
        assert!(h.state().project().tracks.iter().all(|t| !t.is_audio()));
        assert_eq!(
            h.state().ui().status.as_ref().unwrap().text,
            lang.status_drop_refused_midi()
        );
        results.extend_harness(&mut h);

        let mut app = App::headless(project()).with_lang(lang);
        let source = wav(&dir, "Missing.wav", 48_000, 1);
        import(&mut app, vec![source.clone()], None, 0);
        let clip = app.project().tracks[0].clips[0].id;
        std::fs::remove_file(source).unwrap();
        app.ui_mut().dialog = Some(komponist_app::ui_state::Dialog::MissingPlugins(Vec::new()));
        let mut h = harness(app);
        h.get_by_role_and_label(Role::Button, lang.missing_audio_open_clip());
        // The dimmed inspector still paints the scratch filename behind the modal. Its
        // process-specific directory is not part of the missing-files UI contract.
        let source_path = h
            .get_by_role_and_label(Role::Label, lang.audio_file_path())
            .rect();
        h.mask(source_path);
        mask_cpu(&mut h, lang);
        h.snapshot(if lang == Lang::De {
            "audio-missing-files-1x"
        } else {
            "audio-missing-files-en-1x"
        });
        h.get_by_role_and_label(Role::Button, lang.missing_audio_open_clip())
            .click();
        h.run_steps(3);
        assert!(h.state().ui().dialog.is_none());
        assert_eq!(h.state().ui().editor.clip, Some(clip));
        assert!(h.state().ui().selection.clips.contains(&clip));
        results.extend_harness(&mut h);

        let mut browser = FileBrowser::new(lang, Purpose::ImportAudio, dir.clone(), String::new());
        browser.location = "/audio".into();
        browser.entries = ["Shaker Loop.wav", "Stereo Sweep.aiff"]
            .iter()
            .map(|name| Entry {
                name: (*name).into(),
                path: dir.join(name),
                is_dir: false,
            })
            .collect();
        browser.select(0);
        browser.select_with(1, Modifiers::CTRL);
        let snapshots = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
        let mut h = komponist_ui::snapshot::builder::<FileBrowser>(1.0)
            .with_size(vec2(1280.0, 800.0))
            .with_options(SnapshotOptions::new().output_path(snapshots))
            .build_ui_state(
                |ui, b: &mut FileBrowser| {
                    komponist_ui::set_lang(ui.ctx(), b.lang);
                    let _ = komponist_app::dialogs::file::show(ui.ctx(), b);
                },
                browser,
            );
        h.run_steps(3);
        mask_cpu(&mut h, lang);
        h.snapshot(if lang == Lang::De {
            "audio-import-dialog-1x"
        } else {
            "audio-import-dialog-en-1x"
        });
        results.extend_harness(&mut h);
    }
    results.unwrap();
}
