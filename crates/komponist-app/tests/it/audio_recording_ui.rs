//! Recording controls use null capture and synthetic inventory; never enumerate real hardware.

use komponist_app::dialogs::new_track::first_unused_input;
use komponist_app::meters::Meters;
use komponist_app::ui_state::StatusKind;
use komponist_app::{Action, App, EditAction};
use komponist_model::edits::{self, ExclusiveFlag, NewAudioTrack, NewBus, TrackProp};
use komponist_model::{
    AudioFile, AudioInput, AudioTake, ClipContent, HardwareInput, MonitorMode, Project, Tick,
    TrackOutput,
};

fn audio(input: AudioInput) -> (Project, komponist_model::TrackId) {
    edits::add_audio_track(
        &Project::new("Recording"),
        NewAudioTrack {
            input,
            ..Default::default()
        },
    )
    .unwrap()
}

#[test]
fn arm_refuses_missing_and_unavailable_inputs_without_disarming_other_tracks() {
    for input in [
        AudioInput::None,
        AudioInput::Hardware(HardwareInput {
            first: 7,
            stereo: true,
        }),
    ] {
        let (p, armed) = audio(AudioInput::None);
        let p = edits::set_track_prop(&p, &[armed], TrackProp::Arm(true)).unwrap();
        let (p, target) = edits::add_audio_track(
            &p,
            NewAudioTrack {
                input,
                ..Default::default()
            },
        )
        .unwrap();
        let mut app = App::headless(p);
        app.dispatch(Action::Edit(EditAction::SetExclusive {
            track: target,
            flag: ExclusiveFlag::Arm,
        }));
        assert!(app.project().track(armed).unwrap().arm);
        assert!(!app.project().track(target).unwrap().arm);
        assert_eq!(app.ui().status.as_ref().unwrap().kind, StatusKind::Error);
        app.dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![target],
            prop: TrackProp::Arm(true),
        }));
        assert!(!app.project().track(target).unwrap().arm);
    }
}

#[test]
fn synthetic_input_arms_and_first_unused_mono_skips_both_stereo_channels() {
    let (p, track) = audio(AudioInput::Hardware(HardwareInput {
        first: 0,
        stereo: true,
    }));
    assert_eq!(
        first_unused_input(&p, 4),
        AudioInput::Hardware(HardwareInput {
            first: 2,
            stereo: false
        })
    );
    assert_eq!(first_unused_input(&p, 2), AudioInput::None);
    let mut app = App::headless(p);
    let _source = app.engine_mut().set_test_audio_input(2, 48_000).unwrap();
    app.dispatch(Action::Edit(EditAction::SetTrackProp {
        tracks: vec![track],
        prop: TrackProp::Arm(true),
    }));
    assert!(app.project().track(track).unwrap().arm);
}

#[test]
fn bus_record_creates_an_armed_track_outside_the_group_with_one_undo() {
    let (p, member) = audio(AudioInput::None);
    let (p, bus) = edits::add_bus(&p, NewBus::group(vec![member])).unwrap();
    let original = p.clone();
    let mut app = App::headless(p);
    app.dispatch(Action::Edit(EditAction::RecordBus(bus)));
    let created = app
        .project()
        .tracks
        .iter()
        .find(|t| t.id != member && t.id != bus)
        .unwrap();
    assert!(created.is_audio() && created.arm);
    assert_eq!(created.audio.input, AudioInput::Bus(bus));
    assert_eq!(created.audio.monitor, MonitorMode::Off);
    assert_eq!(created.output, TrackOutput::Master);
    assert!(komponist_model::routing::group_ancestors(app.project(), created.id).is_empty());
    let order: Vec<_> = app.project().tracks.iter().map(|t| t.id).collect();
    assert_eq!(order, [bus, member, created.id]);
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(**app.project(), original);
}

#[test]
fn take_actions_preserve_shared_properties_and_are_undoable() {
    let (p, track) = audio(AudioInput::None);
    let file = AudioFile::new(
        std::path::Path::new("/missing/recording.wav"),
        None,
        48_000,
        96_000,
        1,
    );
    let (p, clip) = edits::add_clip(
        &p,
        track,
        edits::NewClip {
            name: "Passes".into(),
            start: Tick::ZERO,
            length: Tick(960),
            content: edits::NewClipContent::Audio {
                file: file.clone(),
                offset: 0,
            },
        },
    )
    .unwrap();
    let (mut p, _) = edits::add_audio_take(
        &p,
        clip,
        AudioTake {
            name: "Second".into(),
            file,
            offset: 48,
        },
    )
    .unwrap();
    let track = std::sync::Arc::make_mut(&mut p.tracks[0]);
    let clip_data = std::sync::Arc::make_mut(&mut track.clips[0]);
    let ClipContent::Audio(a) = &mut clip_data.content else {
        panic!("audio")
    };
    a.gain_db = -4.0;
    a.fade_in = Tick(24);
    a.fade_out = Tick(48);
    let mut app = App::headless(p);
    app.dispatch(Action::Edit(EditAction::SelectAudioTake { clip, take: 0 }));
    app.dispatch(Action::Edit(EditAction::RenameAudioTake {
        clip,
        take: 1,
        name: "Final".into(),
    }));
    app.dispatch(Action::Edit(EditAction::RemoveAudioTake { clip, take: 1 }));
    let ClipContent::Audio(a) = &app.project().tracks[0].clips[0].content else {
        panic!("audio")
    };
    assert_eq!(a.takes.len(), 1);
    assert_eq!(a.gain_db, -4.0);
    assert_eq!((a.fade_in, a.fade_out), (Tick(24), Tick(48)));
    app.dispatch(Action::Edit(EditAction::Undo));
    let ClipContent::Audio(a) = &app.project().tracks[0].clips[0].content else {
        panic!("audio")
    };
    assert_eq!(a.takes[1].name, "Final");
}

#[test]
fn input_meter_is_drained_once_and_shared_by_all_views() {
    let (p, track) = audio(AudioInput::None);
    let mut app = App::headless(p);
    app.engine_mut().hold_engine().unwrap();
    app.step();
    let meter = app.engine().track_meter(track).unwrap();
    meter.record_input(0.5, 0.25);
    let mut meters = Meters::new();
    meters.update(app.engine(), 1.0);
    let first = meters.input(track).unwrap().level_db(0);
    assert!((first + 6.0206).abs() < 0.01);
    assert_eq!(meter.take_input(), (0.0, 0.0));
    assert_eq!(meters.input(track).unwrap().level_db(0), first);
    assert_eq!(meters.input(track).unwrap().level_db(0), first);
    assert!(meters.track(track).unwrap().level_db(0) < first);
}

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange, UiAction};
use komponist_app::dialogs::settings::{self, AudioDevices};
use komponist_i18n::Lang;

struct CaptureDevices;
impl AudioDevices for CaptureDevices {
    fn hosts(&self) -> Vec<komponist_audio::HostKind> {
        vec![komponist_audio::HostKind::Null]
    }
    fn devices(&self, _: komponist_audio::HostKind) -> Vec<komponist_audio::DeviceInfo> {
        vec![]
    }
    fn input_devices(&self, _: komponist_audio::HostKind) -> Vec<komponist_audio::InputDeviceInfo> {
        vec![komponist_audio::InputDeviceInfo {
            id: Some(komponist_audio::DeviceId("test:capture".into())),
            name: "Test Capture".into(),
            channels: 4,
            is_default: true,
            sample_rate: 48_000,
        }]
    }
}
fn harness(app: App) -> Harness<'static, App> {
    let snapshots = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots");
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1280.0, 800.0))
        .with_options(SnapshotOptions::new().output_path(snapshots))
        .with_max_steps(64)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}
fn snapshot(h: &mut Harness<'_, App>, results: &mut SnapshotResults, lang: Lang, name: &str) {
    let paths: Vec<_> = h
        .query_all_by_role(Role::Label)
        .filter(|node| {
            let access = node.accesskit_node();
            access
                .value()
                .is_some_and(|value| value.starts_with('/') || value.contains(":\\"))
                || access
                    .label()
                    .is_some_and(|label| label.starts_with('/') || label.contains(":\\"))
        })
        .map(|node| node.rect())
        .collect();
    for rect in paths {
        h.mask(rect);
    }
    if let Some(cpu) = h.query_by_role_and_label(Role::Status, lang.transport_cpu()) {
        h.mask(cpu.rect().expand(2.0));
    }
    h.snapshot(format!(
        "audio-recording-{name}{}-1x",
        if lang == Lang::En { "-en" } else { "" }
    ));
    results.extend_harness(h);
}

#[test]
fn input_chooser_monitoring_bus_record_and_capture_settings_in_both_languages() {
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let (p, track) = audio(AudioInput::Hardware(HardwareInput {
            first: 0,
            stereo: false,
        }));
        let p = edits::set_track_prop(&p, &[track], TrackProp::Name("Voice".into())).unwrap();
        let p =
            edits::set_track_prop(&p, &[track], TrackProp::MonitorMode(MonitorMode::Auto)).unwrap();
        let p = edits::set_track_prop(&p, &[track], TrackProp::Arm(true)).unwrap();
        let (p, bus) = edits::add_bus(
            &p,
            NewBus {
                name: Some("Loop Bus".into()),
                ..NewBus::group(vec![track])
            },
        )
        .unwrap();
        let mut app = App::headless(p)
            .with_lang(lang)
            .with_audio_devices(Box::new(CaptureDevices));
        let _source = app.engine_mut().set_test_audio_input(4, 48_000).unwrap();
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![track],
            mode: SelectMode::Replace,
        })));
        let mut h = harness(app);
        let input_name = lang.audio_input_tip(lang.audio_input_mono(1));
        let input = h
            .get_all_by_role_and_label(Role::Button, &input_name)
            .next()
            .unwrap();
        input.click();
        h.run_steps(3);
        for _ in 0..100 {
            if h.query_by_role_and_label(Role::MenuItem, "Test Capture")
                .is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
            h.run_steps(1);
        }
        h.get_by_role_and_label(Role::MenuItem, "Test Capture");
        h.run_steps(4);
        snapshot(&mut h, &mut results, lang, "input-channels");
        h.get_by_role_and_label(Role::MenuItem, lang.audio_input_section_bus())
            .click();
        h.run_steps(2);
        let blocked = format!("Loop Bus · {}", lang.audio_input_loop());
        assert!(
            h.get_by_role_and_label(Role::MenuItem, &blocked)
                .accesskit_node()
                .is_disabled()
        );
        snapshot(&mut h, &mut results, lang, "input-loop");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        let monitor = h
            .get_all_by_role(Role::Button)
            .find(|node| node.accesskit_node().value().as_deref() == Some(lang.ui_monitor_auto()))
            .expect("Auto monitoring control");
        assert!(
            monitor
                .accesskit_node()
                .description()
                .unwrap()
                .contains(lang.audio_monitor_auto_armed())
        );
        monitor.click();
        h.run_steps(2);
        assert_eq!(
            h.state().project().track(track).unwrap().audio.monitor,
            MonitorMode::Off
        );
        let control = h
            .get_all_by_role(Role::Button)
            .find(|node| node.accesskit_node().value().as_deref() == Some(lang.ui_monitor_off()))
            .unwrap();
        control.click();
        h.run_steps(2);
        assert_eq!(
            h.state().project().track(track).unwrap().audio.monitor,
            MonitorMode::On
        );
        h.get_all_by_role(Role::Button)
            .find(|node| node.accesskit_node().value().as_deref() == Some(lang.ui_monitor_on()))
            .unwrap()
            .click_secondary();
        h.run_steps(2);
        h.get_by_role_and_label(Role::MenuItemCheckBox, lang.ui_monitor_menu_auto())
            .click();
        h.run_steps(2);
        assert_eq!(
            h.state().project().track(track).unwrap().audio.monitor,
            MonitorMode::Auto
        );
        snapshot(&mut h, &mut results, lang, "header");
        h.state_mut().dispatch(Action::Ui(UiAction::ToggleMixer));
        h.run_steps(3);
        h.get_by_role_and_label(Role::Group, &lang.routing_bus_strip("Loop Bus"))
            .get_by_role_and_label(Role::Button, "Loop Bus")
            .click_secondary();
        h.run_steps(2);
        h.get_by_role_and_label(Role::MenuItem, lang.routing_record_bus());
        snapshot(&mut h, &mut results, lang, "mixer");
        h.key_press(egui::Key::Escape);
        h.run_steps(2);
        h.state_mut().dispatch(Action::Ui(UiAction::OpenSettings(
            komponist_app::ui_state::SettingsSection::Audio,
        )));
        h.run_steps(4);
        h.get_by_role_and_label(Role::ComboBox, lang.settings_input_device());
        h.get_by_role_and_label(Role::RadioButton, lang.settings_capture_pcm24());
        snapshot(&mut h, &mut results, lang, "settings");
        assert!(h.state().project().track(bus).unwrap().is_bus());
    }
    results.unwrap();
}

#[test]
fn capture_device_choices_distinguish_disabled_default_and_disconnected() {
    let (entries, index) = settings::input_entries(Lang::En, &[], None);
    assert_eq!(index, 0);
    assert!(entries[0].config.is_none());
    assert_eq!(entries[1].config, Some(Default::default()));
    let missing = komponist_audio::InputConfig {
        device: Some("Disconnected".into()),
        device_id: None,
    };
    let (entries, index) = settings::input_entries(Lang::En, &[], Some(&missing));
    assert_eq!(entries[index].config, Some(missing));
    assert!(entries[index].label.contains("Disconnected"));
}

#[test]
fn take_rows_expose_selection_duration_and_delete_in_both_languages() {
    let dir = crate::export::scratch("recording-ui-takes");
    let path = dir.join("take.wav");
    let mut writer = hound::WavWriter::create(
        &path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for n in 0..96_000 {
        writer
            .write_sample((12_000.0 * (n as f32 * 0.08).sin()) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
    let file = AudioFile::new(&path, None, 96_000, 48_000, 1);
    let (p, track) = audio(AudioInput::None);
    let (p, clip) = edits::add_clip(
        &p,
        track,
        edits::NewClip {
            name: "Recorded loop".into(),
            start: Tick::ZERO,
            length: Tick(960),
            content: edits::NewClipContent::Audio {
                file: file.clone(),
                offset: 0,
            },
        },
    )
    .unwrap();
    let (p, _) = edits::add_audio_take(
        &p,
        clip,
        AudioTake {
            name: "Second pass".into(),
            file,
            offset: 0,
        },
    )
    .unwrap();
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let mut app = App::headless(p.clone()).with_lang(lang);
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![clip],
            mode: SelectMode::Replace,
        })));
        let mut h = harness(app);
        let first = lang.inspector_take(1);
        let first_node = h.get_by_role_and_label(Role::RadioButton, &first);
        assert_eq!(
            first_node.accesskit_node().description().as_deref(),
            Some("00:02")
        );
        assert_eq!(
            h.get_by_role_and_label(Role::RadioButton, "Second pass")
                .accesskit_node()
                .toggled(),
            Some(egui::accesskit::Toggled::True)
        );
        first_node.click();
        h.run_steps(3);
        let ClipContent::Audio(audio) = &h.state().project().tracks[0].clips[0].content else {
            panic!("audio")
        };
        assert_eq!(audio.active_take, 0);
        for _ in 0..12 {
            h.run_steps(1);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // Scratch paths and CPU are nondeterministic; their labels retain their full accessible value.
        if let Some(path) = h.query_by_role_and_label(Role::Label, lang.audio_file_path()) {
            h.mask(path.rect());
        }
        snapshot(&mut h, &mut results, lang, "takes");
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
        assert_eq!(
            h.state().project().tracks[0].clips.len(),
            1,
            "Delete on a take must retain the clip"
        );
        let ClipContent::Audio(a) = &h.state().project().tracks[0].clips[0].content else {
            panic!("audio")
        };
        assert_eq!(a.takes.len(), 1);
        assert_eq!(a.takes[0].name, "Second pass");
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, &lang.audio_take_remove("Second pass"))
            .click();
        h.run_steps(3);
        let ClipContent::Audio(audio) = &h.state().project().tracks[0].clips[0].content else {
            panic!("audio")
        };
        assert_eq!(audio.takes.len(), 1);
    }
    results.unwrap();
}

#[test]
fn provisional_recordings_chip_is_persistent_and_fits_the_transport_in_both_languages() {
    use komponist_app::panels::transport::{self, TransportModel};
    use std::sync::Arc;
    struct Bar {
        project: Arc<Project>,
        meters: Meters,
        lang: Lang,
        keymap: komponist_app::keymap::Keymap,
    }
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let state = Bar {
            project: Arc::new(Project::new("Voice Session")),
            meters: Meters::new(),
            lang,
            keymap: komponist_app::keymap::Keymap::default_bindings(),
        };
        let mut h = komponist_ui::snapshot::builder::<Bar>(1.0)
            .with_size(vec2(1280.0, komponist_tokens::size::TRANSPORT))
            .with_options(SnapshotOptions::new().output_path(
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots"),
            ))
            .build_ui_state(
                |ui, state: &mut Bar| {
                    let model = TransportModel {
                        project: &state.project,
                        dirty: true,
                        state: komponist_engine::TransportState::Stopped,
                        playhead: Tick::ZERO,
                        cpu: 0.0,
                        audio: &komponist_audio::AudioState::Stopped,
                        keymap: &state.keymap,
                        lang: state.lang,
                        master_display: Default::default(),
                        plugin_windows: &[],
                        provisional_recordings: Some(std::path::Path::new(
                            "/tmp/komponist/Voice Session",
                        )),
                    };
                    transport::show(ui, ui.max_rect(), &model, &mut state.meters, &mut vec![]);
                },
                state,
            );
        h.run_steps(3);
        let chip = h
            .get_by_role_and_label(Role::Label, lang.audio_recordings_provisional())
            .rect();
        let mark = h
            .get_by_role_and_label(Role::Label, transport::WORDMARK)
            .rect();
        assert!(
            chip.right() <= mark.left() + komponist_app::metrics::transport::WORDMARK_WIDTH + 1.0
        );
        assert!(
            chip.right()
                < h.get_by_role_and_label(Role::Button, lang.transport_play())
                    .rect()
                    .left()
        );
        h.run_steps(3);
        h.get_by_role_and_label(Role::Label, lang.audio_recordings_provisional());
        h.snapshot(format!(
            "audio-recording-provisional{}-1x",
            if lang == Lang::En { "-en" } else { "" }
        ));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

#[test]
fn capture_device_changes_are_refused_while_recording() {
    let (mut p, track) = audio(AudioInput::Hardware(HardwareInput {
        first: 0,
        stereo: false,
    }));
    p.count_in_bars = 0;
    let p = edits::set_track_prop(&p, &[track], TrackProp::Arm(true)).unwrap();
    let mut app = App::headless(p)
        .with_recording_directory(crate::export::scratch("recording-ui-device-guard"));
    let _source = app.engine_mut().set_test_audio_input(2, 48_000).unwrap();
    app.step();
    app.dispatch(Action::Transport(
        komponist_app::actions::TransportAction::ToggleRecord,
    ));
    assert!(app.audio_recording().is_some());
    app.dispatch(Action::Settings(
        komponist_app::actions::SettingsAction::SetAudioInput(None),
    ));
    assert!(app.audio_recording().is_some());
    assert_eq!(app.engine().audio_input_channels(), 2);
    assert_eq!(
        app.ui().status.as_ref().unwrap().text,
        Lang::De.audio_input_change_recording()
    );
    app.dispatch(Action::Transport(
        komponist_app::actions::TransportAction::DiscardTake,
    ));
}

#[test]
fn live_audio_inspector_and_input_status_render_in_both_languages() {
    let mut results = SnapshotResults::new();
    for lang in [Lang::De, Lang::En] {
        let (p, bus) = edits::add_bus(
            &Project::new("Live Audio"),
            NewBus {
                name: Some("Source Bus".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let (mut p, track) = edits::add_audio_track(
            &p,
            NewAudioTrack {
                name: Some("Voice".into()),
                input: AudioInput::Bus(bus),
                ..Default::default()
            },
        )
        .unwrap();
        p.count_in_bars = 0;
        let p = edits::set_track_prop(&p, &[track], TrackProp::Arm(true)).unwrap();
        let mut app = App::headless(p)
            .with_lang(lang)
            .with_recording_directory(crate::export::scratch("recording-ui-live"));
        app.engine_mut().hold_engine().unwrap();
        app.step();
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![track],
            mode: SelectMode::Replace,
        })));
        app.dispatch(Action::Transport(
            komponist_app::actions::TransportAction::ToggleRecord,
        ));
        assert!(app.engine_mut().run_held(4_800, &[], 0));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            app.step();
            if app.audio_recording().and_then(|r| r.clip(track)).is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "live capture did not publish"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let expected = lang.status_recording_input("Voice", lang.audio_input_bus("Source Bus"));
        assert_eq!(
            komponist_app::panels::statusbar::model(&app, std::time::Instant::now()).left,
            expected
        );
        let mut h = harness(app);
        h.get_by_role_and_label(Role::Group, lang.inspector_section_clip())
            .get_by_role_and_label(
                Role::Label,
                &format!(
                    "{} {}",
                    lang.new_track_input(),
                    lang.audio_input_bus("Source Bus")
                ),
            );
        h.get_by_role_and_label(Role::Label, &format!("{} 1", lang.inspector_clip_takes()));
        assert!(
            h.get_all_by_role(Role::Label).any(
                |node| node.accesskit_node().label().as_deref() == Some(lang.audio_file_path())
            )
        );
        h.get_by_role_and_label(Role::Label, lang.audio_recordings_provisional());
        let path = h
            .get_all_by_role(Role::Label)
            .find(|node| node.accesskit_node().label().as_deref() == Some(lang.audio_file_path()))
            .unwrap()
            .rect();
        h.mask(path);
        snapshot(&mut h, &mut results, lang, "live-inspector");
        h.state_mut().dispatch(Action::Transport(
            komponist_app::actions::TransportAction::DiscardTake,
        ));
        h.run_steps(2);
    }
    results.unwrap();
}
