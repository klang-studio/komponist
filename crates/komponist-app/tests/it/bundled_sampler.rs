//! Packaged sampler state test, launched by `scripts/measure-bundled-clap.py --historical-projects`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable as _;

use komponist_app::actions::PluginAction;
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_app::{Action, App, ProjectAction};
use komponist_builtin::Internal;
use komponist_device_core::pinned_clap_state;
use komponist_engine::Command;
use komponist_model::edits::{self, NewTrack};
use komponist_model::{Project, TrackId};
use komponist_plugins::bundled_clap::BundledClap;
use komponist_plugins::legacy_bundled::{
    migrate_legacy_bundled_project, plan_legacy_bundled_device,
};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent};
use komponist_sampler::links::{self, SampleLink};

use crate::acceptance_support::harness;
use crate::common::{PATIENCE, app_with, config, step_for, step_until};

#[test]
#[ignore = "requires a verified all-device package; run with scripts/measure-bundled-clap.py --historical-projects"]
fn both_samplers_play_assets_after_the_project_moves() {
    let launcher = std::env::var_os("KOMPONIST_HISTORICAL_BUNDLED_APP")
        .expect("set the unpacked app with scripts/measure-bundled-clap.py --historical-projects");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let inventory = BundledClap::discover(scratch.path()).unwrap().unwrap();
    let before_dir = scratch.path().join("before");
    std::fs::create_dir(&before_dir).unwrap();
    let asset = before_dir.join("tone.wav");
    write_sample(&asset);
    let link = SampleLink::new(&asset, Some(&before_dir));

    let mut source = Project::new("Moved sampler project");
    for kind in [Internal::Sampler, Internal::GeneralSampler] {
        let (next, track) = edits::add_track(
            &source,
            NewTrack {
                name: None,
                hue: None,
                instrument: Some(kind.plugin_ref()),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = next.track(track).unwrap().channel.instrument().unwrap().1;
        let state = kind.encode_state(&kind.defaults());
        let linked = links::write(Some(&state), 0, Some(&link));
        source = edits::set_device_state(&next, device.id, Some(linked.into())).unwrap();
    }
    let plans = source.tracks.iter().map(|track| {
        let device = track.channel.instrument().unwrap().1;
        plan_legacy_bundled_device(&inventory, device, Some(&before_dir)).unwrap()
    });
    let migrated = migrate_legacy_bundled_project(&source, plans)
        .unwrap()
        .project;
    let filename = "samples.komponist";
    save_project(&before_dir.join(filename), &migrated).unwrap();
    let moved = scratch.path().join("moved");
    std::fs::rename(&before_dir, &moved).unwrap();
    assert!(!asset.exists());
    let reopened_path = moved.join(filename);
    assert_eq!(load_project(&reopened_path).unwrap(), migrated);

    let mut config = config(scratch.path());
    config.bridge_exe = launcher.into();
    let mut host = PluginHost::new(config);
    host.start_scan(&PluginDirs {
        clap: Vec::new(),
        vst3: Vec::new(),
    });
    let deadline = Instant::now() + PATIENCE;
    while !host
        .poll()
        .iter()
        .any(|event| matches!(event, PluginManagerEvent::ScanFinished(_)))
    {
        assert!(Instant::now() < deadline, "bundled CLAP scan timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut app = app_with(Project::new("Empty"), host);
    app.dispatch(Action::Project(ProjectAction::OpenPath(reopened_path)));
    for (index, key) in [komponist_sampler::note_of_pad(0), 60]
        .into_iter()
        .enumerate()
    {
        let track = &migrated.tracks[index];
        let device = track.channel.instrument().unwrap().1;
        step_until(&mut app, "relocated sampler ready", |app| {
            app.engine().device_status(device.id) == DeviceStatus::Ok
        });
        assert_eq!(
            app.engine().host().bridge_pid(device.id).is_some(),
            force_bridge
        );
        assert_audible(&mut app, track.id, key);
    }
    assert_eq!(app.project().as_ref(), &migrated);
    assert!(
        !app.doc().is_dirty(),
        "runtime path refresh must not edit the project"
    );

    // Exercise state restore with the original stale absolute fallback too.
    let track = &migrated.tracks[1];
    let device = track.channel.instrument().unwrap().1;
    let linked = device.state.as_ref().unwrap();
    let empty = Arc::from(pinned_clap_state::encode(
        &device.plugin.version,
        Default::default(),
        Default::default(),
    ));
    assert!(app.engine_mut().host_mut().set_state(device.id, &empty));
    let restored = |app: &App| {
        app.device_events().iter().any(|event| {
        matches!(event, PluginManagerEvent::StateApplied { device: id } if *id == device.id)
    })
    };
    step_until(&mut app, "empty sampler state applied", restored);
    step_for(&mut app, Duration::from_millis(300));
    app.engine().track_meter(track.id).unwrap().take();
    assert_eq!(note_peak(&mut app, track.id, 60), 0.0);
    assert!(app.engine_mut().host_mut().set_state(device.id, linked));
    step_until(&mut app, "original sampler state restored", restored);
    assert_audible(&mut app, track.id, 60);

    // Saving into another folder must keep the moved source, even when a silent
    // sample with the same file name exists next to the new project.
    let destination = scratch.path().join("destination");
    std::fs::create_dir(&destination).unwrap();
    let silent = destination.join("tone.wav");
    write_wav(&silent, 0);
    assert!(
        hound::WavReader::open(silent)
            .unwrap()
            .samples::<i16>()
            .all(|sample| sample.unwrap() == 0)
    );
    let copy = destination.join("copy.komponist");
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Project(ProjectAction::SaveAs));
    h.run_steps(3);
    h.get_by_label("Dateiname").focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Dateiname")
        .type_text(copy.to_str().unwrap());
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    let app = h.state_mut();
    assert_eq!(app.project_path(), Some(copy.as_path()));
    // Capture the live sampler state a second time, after adopting the new folder.
    app.dispatch(Action::Project(ProjectAction::Save));
    let source = moved.join("tone.wav");
    assert_sample_sources(&load_project(&copy).unwrap(), &destination, &source);

    // Both bundled samplers still advertise missing assets and accept a relink.
    let replacement = scratch.path().join("found.wav");
    std::fs::rename(&source, &replacement).unwrap();
    let missing =
        komponist_app::dialogs::missing_plugins::missing_samples(app.project(), Some(&destination));
    assert_eq!(missing.len(), 2);
    for sample in missing {
        app.dispatch(Action::Plugin(PluginAction::LoadSample {
            device: sample.device,
            pad: sample.pad,
            path: replacement.clone(),
        }));
    }
    assert!(komponist_app::dialogs::missing_plugins::missing_samples(
        app.project(),
        Some(&destination),
    ).is_empty());
    assert_audible(app, track.id, 60);

    // Both CLAP instances still have the original source in their live state when
    // collection begins. Their next snapshots must adopt the collected file too.
    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    let collected = destination.join("copy Samples/found.wav");
    assert!(collected.is_file());
    assert_eq!(
        std::fs::read_dir(collected.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    assert_sample_sources(&load_project(&copy).unwrap(), &destination, &collected);
    std::fs::remove_file(&replacement).unwrap();
    app.dispatch(Action::Project(ProjectAction::Save));
    assert_sample_sources(&load_project(&copy).unwrap(), &destination, &collected);
    for (track, key) in migrated
        .tracks
        .iter()
        .zip([komponist_sampler::note_of_pad(0), 60])
    {
        assert_audible(app, track.id, key);
    }
    app.shutdown();
}

#[test]
#[ignore = "requires a verified all-device package; run with scripts/measure-bundled-clap.py --current-projects"]
fn bundled_sampler_panels_edit_and_restore_current_state() {
    use egui::accesskit::Role;
    use komponist_app::actions::{
        EditAction, SelectMode, SelectionChange, SettingsAction, UiAction,
    };
    use komponist_app::{settings::Language, ui_state::DockTab};
    use komponist_builtin::BuiltinKind;
    use komponist_ipc::sampler_ui::Status;

    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").expect("packaged launcher");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let sample = scratch.path().join("tone.wav");
    write_wav_with_region(&sample, |frame| {
        // Only the slice that spans 40–100 ms contains sound.
        if (1_920..4_800).contains(&frame) {
            11_000
        } else {
            0
        }
    });
    let mut config = config(scratch.path());
    config.bridge_exe = launcher.into();
    let mut host = PluginHost::new(config);
    host.start_scan(&PluginDirs {
        clap: Vec::new(),
        vst3: Vec::new(),
    });
    let deadline = Instant::now() + PATIENCE;
    while !host
        .poll()
        .iter()
        .any(|event| matches!(event, PluginManagerEvent::ScanFinished(_)))
    {
        assert!(Instant::now() < deadline, "bundled CLAP scan timed out");
        std::thread::sleep(Duration::from_millis(5));
    }

    let mut project = Project::new("Packaged samplers");
    let mut tracks = Vec::new();
    for kind in [Internal::Sampler, Internal::GeneralSampler] {
        let instrument = host
            .catalog()
            .packaged(BuiltinKind::Internal(kind))
            .unwrap()
            .plugin
            .clone();
        let (next, track) = edits::add_track(
            &project,
            NewTrack {
                name: None,
                hue: None,
                instrument: Some(instrument),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = next.track(track).unwrap().channel.instrument().unwrap().1;
        assert!(device.state.is_none());
        tracks.push((kind, track, device.id));
        project = next;
    }
    let project_path = scratch.path().join("samplers.komponist");
    save_project(&project_path, &project).unwrap();
    let mut app = app_with(Project::new("Empty"), host).with_lang(komponist_i18n::Lang::De);
    app.dispatch(Action::Project(ProjectAction::OpenPath(project_path)));
    for (kind, track, device) in &tracks {
        let pad = usize::from(*kind == Internal::Sampler) * 2;
        step_until(&mut app, "initial sampler UI", |app| {
            app.engine()
                .host()
                .sampler_ui(*device)
                .is_some_and(|view| view.idle)
        });
        assert_eq!(
            app.engine().host().bridge_pid(*device).is_some(),
            force_bridge
        );
        // Turn a knob and replace the sample before the host has captured the knob turn.
        let edited = if *kind == Internal::GeneralSampler {
            let wire = app
                .engine()
                .host()
                .native_layout(*device)
                .unwrap()
                .author_ids()
                .unwrap()["gain"];
            let info = kind
                .params()
                .iter()
                .find(|param| param.id.as_str() == "gain")
                .unwrap();
            let value = komponist_device_core::pinned_clap_state::to_clap(info, -12.0);
            app.dispatch(Action::Plugin(PluginAction::SetParam {
                device: *device,
                id: wire,
                value,
            }));
            Some((wire, value))
        } else {
            None
        };
        app.dispatch(Action::Plugin(PluginAction::LoadSample {
            device: *device,
            pad,
            path: sample.clone(),
        }));
        step_until(&mut app, "loaded sampler waveform", |app| {
            if let Some((wire, value)) = edited {
                assert!(
                    (app.engine().host().param_base(*device, wire).unwrap() - value).abs() < 1e-6
                );
            }
            app.engine().host().sampler_ui(*device).is_some_and(|view| {
                view.idle
                    && view.pads[pad].status == Status::Ready
                    && view.pads[pad].peaks.iter().any(|peak| *peak > 0.1)
            })
        });
        let key = if *kind == Internal::Sampler {
            komponist_sampler::note_of_pad(pad)
        } else {
            60
        };
        assert_audible(&mut app, *track, key);
    }
    let (_, general_track, general) = tracks[1];
    let mode = app
        .engine()
        .host()
        .native_layout(general)
        .unwrap()
        .author_ids()
        .unwrap()["mode"];
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: general,
        id: mode,
        value: 2.0,
    }));
    step_until(&mut app, "saved slice mode", |app| {
        let device = app.project().find_device(general).unwrap().2;
        device
            .state
            .as_deref()
            .and_then(pinned_clap_state::decode)
            .is_some_and(|state| state["params"]["mode"]["string"] == "slices")
    });
    let settings = komponist_general_sampler::Settings::from_markers([0.04, 0.10]);
    let slice_state = |app: &App| {
        let device = app.project().find_device(general).unwrap().2;
        let assets = device
            .state
            .as_deref()
            .and_then(komponist_plugins::bundled_sampler::assets);
        komponist_general_sampler::read_settings(assets.as_deref().map(str::as_bytes))
    };
    app.dispatch(Action::Plugin(PluginAction::SetSamplerSlices {
        device: general,
        settings: settings.clone(),
    }));
    assert_eq!(slice_state(&app), settings);
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(
        slice_state(&app),
        komponist_general_sampler::Settings::default()
    );
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(slice_state(&app), settings);
    step_until(&mut app, "restored slice map", |app| {
        !app.engine().host().sampler_is_loading(general)
    });
    step_for(&mut app, Duration::from_millis(200));
    app.engine().track_meter(general_track).unwrap().take();
    app.dispatch(Action::Plugin(PluginAction::PreviewSampler {
        device: general,
        slice_time: Some(0.08),
    }));
    step_for(&mut app, Duration::from_millis(160));
    assert!(app.engine().track_meter(general_track).unwrap().take().0 > 1e-3);
    // The first slice is silent. Confirm that its preview cannot produce the same peak.
    app.dispatch(Action::Plugin(PluginAction::PreviewSampler {
        device: general,
        slice_time: Some(0.02),
    }));
    step_for(&mut app, Duration::from_millis(160));
    assert!(app.engine().track_meter(general_track).unwrap().take().0 < 1e-4);

    let (_, drum_track, drum) = tracks[0];
    let select = |app: &mut App, track| {
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![track],
            mode: SelectMode::Replace,
        })));
        app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    };
    select(&mut app, drum_track);
    let mut h = harness(app);
    let save_screenshot = |h: &mut egui_kittest::Harness<'_, App>, device: &str, lang: &str| {
        if let Some(folder) = std::env::var_os("KOMPONIST_SAMPLER_UI_SCREENSHOTS") {
            let path = std::path::PathBuf::from(folder);
            std::fs::create_dir_all(&path).unwrap();
            let route = if force_bridge { "bridge" } else { "direct" };
            h.render()
                .unwrap()
                .save(path.join(format!("{device}-{route}-{lang}.png")))
                .unwrap();
        }
    };
    for (lang, code) in [(Language::De, "de"), (Language::En, "en")] {
        h.state_mut()
            .dispatch(Action::Settings(SettingsAction::SetLanguage(lang)));
        select(h.state_mut(), drum_track);
        h.run_steps(3);
        let pad = h
            .get_by_role_and_label(Role::Button, "Pad tone")
            .rect()
            .center();
        crate::acceptance_support::click_at(&mut h, pad);
        h.get_by_role_and_label(Role::Group, "Pad 3 · tone");
        h.get_by_role_and_label(Role::Slider, "Tune 3");
        save_screenshot(&mut h, "drum-sampler", code);
        select(h.state_mut(), general_track);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, "tone");
        save_screenshot(&mut h, "general-sampler", code);
    }
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::ClearSample {
            device: drum,
            pad: 2,
        }));
    step_until(h.state_mut(), "cleared pad", |app| {
        app.engine()
            .host()
            .sampler_ui(drum)
            .is_some_and(|snapshot| snapshot.pads[2].status == Status::Empty)
    });
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    step_until(h.state_mut(), "restored pad", |app| {
        app.engine()
            .host()
            .sampler_ui(drum)
            .is_some_and(|snapshot| snapshot.pads[2].status == Status::Ready)
    });
    let host = h.state().engine().host();
    let gain = host.native_layout(general).unwrap().author_ids().unwrap()["gain"];
    let info = Internal::GeneralSampler
        .params()
        .iter()
        .find(|p| p.id.as_str() == "gain")
        .unwrap();
    let raw = |db| pinned_clap_state::to_clap(info, db);
    let set_gain = |app: &mut App, db| {
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device: general,
            id: gain,
            value: raw(db),
        }))
    };
    set_gain(h.state_mut(), -18.0);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::BeginParamGesture {
            device: general,
            id: gain,
        }));
    set_gain(h.state_mut(), -36.0);
    h.state_mut()
        .dispatch(Action::Edit(EditAction::CancelGesture));
    // Force a full state restore before the cancelled knob can be captured.
    let changed_slices = komponist_general_sampler::Settings::from_markers([0.02, 0.10]);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetSamplerSlices {
            device: general,
            settings: changed_slices,
        }));
    step_until(
        h.state_mut(),
        "the cancelled turn stays cancelled after a slice edit",
        |app| {
            let host = app.engine().host();
            host.sampler_ui(general).is_some_and(|view| view.idle)
                && host
                    .param_base(general, gain)
                    .is_some_and(|value| (value - raw(-18.0)).abs() < 1e-6)
        },
    );

    let replacement = scratch.path().join("replacement.wav");
    write_sample(&replacement);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::PickSample {
            device: general,
            pad: 0,
        }));
    h.run_steps(3);
    let row = h
        .get_by_role_and_label(Role::ListItem, "replacement.wav")
        .rect()
        .center();
    crate::acceptance_support::click_at(&mut h, row);
    set_gain(h.state_mut(), -22.0);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    let hosted_state = |app: &App| {
        let (_, _, d) = app.project().find_device(general).unwrap();
        pinned_clap_state::decode(d.state.as_deref().unwrap()).unwrap()
    };
    let sample_name = |app: &App| {
        let state = hosted_state(app);
        let assets = state["fields"][pinned_clap_state::SAMPLE_ASSETS_FIELD]
            .as_str()
            .unwrap();
        links::read(Some(assets.as_bytes()))[0].1.name.clone()
    };
    assert_eq!(sample_name(h.state()), "replacement");
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(sample_name(h.state()), "tone");
    let stored_gain = hosted_state(h.state())["params"]["gain"]["f32"]
        .as_f64()
        .unwrap();
    assert!((stored_gain - raw(-22.0)).abs() < 1e-6);
    h.state_mut().shutdown();
}

fn assert_sample_sources(project: &Project, project_dir: &Path, source: &Path) {
    for track in &project.tracks {
        let device = track.channel.instrument().unwrap().1;
        let state = device.state.as_deref().unwrap();
        let wrapper: serde_json::Value = serde_json::from_slice(&state[8..]).unwrap();
        let assets = wrapper["fields"][pinned_clap_state::SAMPLE_ASSETS_FIELD]
            .as_str()
            .unwrap();
        assert_eq!(
            links::read(Some(assets.as_bytes()))[0]
                .1
                .resolve(Some(project_dir))
                .as_deref(),
            Some(source)
        );
    }
}

fn assert_audible(app: &mut App, track: TrackId, key: u8) {
    let deadline = Instant::now() + PATIENCE;
    while note_peak(app, track, key) <= 1e-4 {
        assert!(
            Instant::now() < deadline,
            "relocated sampler track {track:?} is silent"
        );
    }
}

fn note_peak(app: &mut App, track: TrackId, key: u8) -> f32 {
    app.engine_mut().send(Command::LiveNote {
        track,
        key,
        velocity: 100,
    });
    step_for(app, Duration::from_millis(160));
    app.engine_mut().send(Command::LiveNote {
        track,
        key,
        velocity: 0,
    });
    let (left, right) = app.engine().track_meter(track).unwrap().take();
    left.max(right)
}

fn write_sample(path: &Path) {
    write_wav(path, 11_000);
}

fn write_wav(path: &Path, amplitude: i16) {
    write_wav_with_region(path, |_| amplitude);
}

fn write_wav_with_region(path: &Path, amplitude: impl Fn(usize) -> i16) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..8192 {
        let amplitude = amplitude(frame);
        let sample = if frame / 12 % 2 == 0 {
            amplitude
        } else {
            -amplitude
        };
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}
