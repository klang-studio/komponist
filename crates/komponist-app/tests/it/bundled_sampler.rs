//! Packaged sampler state test, launched by `scripts/measure-bundled-clap.py --historical-projects`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::{NodeT as _, Queryable as _};

use komponist_app::actions::PluginAction;
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_app::{Action, App, ProjectAction};
use komponist_builtin::Internal;
use komponist_device_core::pinned_clap_state;
use komponist_engine::Command;
use komponist_ipc::device_assets::AssetStatus;
use komponist_model::edits::{self, NewTrack};
use komponist_model::{DeviceId, Project, TrackId};
use komponist_plugins::bundled_clap::BundledClap;
use komponist_plugins::legacy_bundled::{
    migrate_legacy_bundled_project, plan_legacy_bundled_device,
};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent};
use komponist_sampler::links::{self, SampleLink};

use crate::acceptance_support::harness;
use crate::common::{NULL_BUFFER, PATIENCE, app_with, config, step_for, step_until};

const SAMPLE_FRAMES: u32 = 8192;

fn asset_status(app: &App, device: DeviceId, id: u32) -> Option<&AssetStatus> {
    let slots = app.engine().host().asset_slots(device)?;
    slots
        .slots
        .iter()
        .find(|slot| slot.id == id)
        .map(|slot| &slot.status)
}

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
    assert!(
        migrated.tracks.iter().all(|track| {
            track
                .channel
                .instrument()
                .unwrap()
                .1
                .asset_links
                .contains_key(&0)
        }),
        "both verified sampler bundles moved their file into the project link inventory"
    );
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
    if device.asset_links.contains_key(&0) {
        // Parameter-state changes leave a host-managed asset in place.
        assert_audible(&mut app, track.id, 60);
    } else {
        app.engine().track_meter(track.id).unwrap().take();
        assert_eq!(note_peak(&mut app, track.id, 60), 0.0);
    }
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
    let missing_assets = |app: &App| {
        komponist_app::dialogs::missing_plugins::missing_assets(
            komponist_i18n::Lang::De,
            app.project(),
            Some(&destination),
            app.engine().host().catalog(),
        )
    };
    let external = missing_assets(app);
    assert_eq!(missing.len() + external.len(), 2);
    for sample in missing {
        app.dispatch(Action::Plugin(PluginAction::LoadSample {
            device: sample.device,
            pad: sample.pad,
            path: replacement.clone(),
        }));
    }
    for row in external {
        let (device, slot) = row.asset.unwrap();
        app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device,
            slot,
            path: Some(replacement.clone()),
        }));
    }
    assert!(komponist_app::dialogs::missing_plugins::missing_samples(
        app.project(),
        Some(&destination),
    ).is_empty());
    assert!(missing_assets(app).is_empty());
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

fn scanned_packaged_sampler_host(scratch: &Path, launcher: std::path::PathBuf) -> PluginHost {
    let mut config = config(scratch);
    config.bridge_exe = launcher;
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

    host
}

#[test]
#[ignore = "requires verified sampler bundles; run with scripts/measure-bundled-clap.py --current-projects"]
fn bundled_sampler_restores_saved_parameter_bases() {
    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").expect("packaged launcher");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let host = scanned_packaged_sampler_host(scratch.path(), launcher.clone().into());
    let instrument = host
        .catalog()
        .entries()
        .iter()
        .find(|entry| entry.plugin.id == "org.klang-studio.komponist.general-sampler")
        .expect("verified General Sampler")
        .plugin
        .clone();
    let (project, track) = edits::add_track(
        &Project::new("Sampler parameter bases"),
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(instrument),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let device = project
        .track(track)
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1
        .id;
    let wait_for_sampler = |app: &mut App| {
        step_until(app, "sampler ready", |app| {
            app.engine().host().status(device) == DeviceStatus::Ok
        });
        assert_eq!(
            app.engine().host().bridge_pid(device).is_some(),
            force_bridge
        );
    };
    let snapshot = |app: &mut App| {
        let result = app.engine_mut().host_mut().snapshot_for_save(PATIENCE);
        assert!(result.blocked.is_empty());
        result
            .captured
            .into_iter()
            .find_map(|(id, state)| (id == device).then_some(state))
            .expect("fresh sampler snapshot")
    };
    let mut app = app_with(project.clone(), host);
    wait_for_sampler(&mut app);
    let host = app.engine().host();
    let gain = host.native_layout(device).unwrap().author_ids().unwrap()["gain"];
    let default = host.param_base(device, gain).unwrap();
    let gain_info = host
        .params(device)
        .unwrap()
        .iter()
        .find(|info| info.id == gain)
        .unwrap();
    let restored_gain = (gain_info.min + gain_info.max) / 2.0;
    assert_ne!(restored_gain, default);
    let baseline = snapshot(&mut app);
    assert!(baseline.parameter_bases.is_empty());
    let state = komponist_model::project::SavedPluginState {
        bytes: baseline.bytes.clone(),
        parameter_bases: [(gain, restored_gain)].into(),
    };

    // The direct path can apply a queued active restore as part of taking the snapshot.
    assert!(
        app.engine_mut()
            .host_mut()
            .restore_saved_state(device, &state)
    );
    if force_bridge {
        // The child acknowledges the asynchronous restore before offering a new snapshot.
        step_until(&mut app, "bridged restore applied", |app| {
            app.engine()
                .host()
                .param_base(device, gain)
                .is_some_and(|value| (value - restored_gain).abs() < 1e-6)
        });
        // An empty instrument needs a processing block to consume its queued bases.
        note_peak(&mut app, track, 60);
    }
    let captured = snapshot(&mut app);
    assert_ne!(captured.bytes, baseline.bytes);
    assert!(captured.parameter_bases.is_empty());
    assert!((app.engine().host().param_base(device, gain).unwrap() - restored_gain).abs() < 1e-6);
    app.shutdown();

    // Start a new instance from the unchanged blob plus the sidecar stored in a project.
    let project = edits::set_saved_plugin_state(&project, device, &state).unwrap();
    let path = scratch.path().join("parameter-bases.komponist");
    save_project(&path, &project).unwrap();
    let host = scanned_packaged_sampler_host(scratch.path(), launcher.into());
    let mut app = app_with(load_project(&path).unwrap(), host);
    wait_for_sampler(&mut app);
    assert!((app.engine().host().param_base(device, gain).unwrap() - restored_gain).abs() < 1e-6);
    assert_eq!(snapshot(&mut app), captured);
    app.shutdown();
}

#[test]
#[ignore = "requires a verified all-device package; run with scripts/measure-bundled-clap.py --current-projects"]
fn bundled_sampler_panels_edit_and_restore_current_state() {
    use egui::accesskit::Role;
    use komponist_app::actions::{
        EditAction, SelectMode, SelectionChange, SettingsAction, UiAction,
    };
    use komponist_app::views::automation::{Action as Automation, Source};
    use komponist_app::{settings::Language, ui_state::DockTab};
    use komponist_builtin::BuiltinKind;
    use komponist_ipc::waveform_markers::{Edit as MarkerEdit, Request as MarkerRequest};
    use komponist_model::Tick;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};

    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").expect("packaged launcher");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let sample = scratch.path().join("tone.wav");
    write_wav_with_region(&sample, |frame| {
        // Only the 40–100 ms slice contains sound. Leave silence at both cuts so
        // interpolation near a slice boundary cannot read into the tone.
        if (2_400..4_320).contains(&frame) {
            11_000
        } else {
            0
        }
    });
    let host = scanned_packaged_sampler_host(scratch.path(), launcher.into());

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
            let Some(layout) = app.engine().host().native_layout(*device) else {
                return false;
            };
            assert!(layout.has_audio_asset_editor() && layout.schema_version == 2);
            asset_status(app, *device, pad as u32) == Some(&AssetStatus::Empty)
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
        app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: *device,
            slot: pad as u32,
            path: Some(sample.clone()),
        }));
        step_until(&mut app, "loaded sampler waveform", |app| {
            if let Some((wire, value)) = edited {
                assert!(
                    (app.engine().host().param_base(*device, wire).unwrap() - value).abs() < 1e-6
                );
            }
            asset_status(app, *device, pad as u32) == Some(&AssetStatus::Ready)
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
    let edit_marker = |app: &mut App, prior: &[f64], operation, expected| {
        let deadline = Instant::now() + PATIENCE;
        let mut request_at = Instant::now();
        let revision = loop {
            let now = Instant::now();
            if now >= request_at {
                app.dispatch(Action::Plugin(PluginAction::RequestWaveformMarkers {
                    device: general,
                    slot: 0,
                }));
                request_at = now + Duration::from_millis(100);
            }
            app.step();
            let snapshot = app.engine().host().waveform_markers(general, 0);
            if let Some(markers) =
                snapshot.filter(|markers| markers.editable && markers.positions == prior)
            {
                break markers.revision;
            }
            assert!(
                Instant::now() < deadline,
                "no editable waveform for {operation:?}; expected {prior:?}, latest {snapshot:?}, asset {:?}, device {:?}",
                asset_status(app, general, 0),
                app.engine().device_status(general),
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        let request = MarkerRequest {
            revision,
            operation,
        };
        app.dispatch(Action::Plugin(PluginAction::EditWaveformMarkers {
            device: general,
            slot: 0,
            request,
        }));
        step_until(
            app,
            &format!("the waveform marker edit {operation:?} was captured"),
            |app| slice_state(app) == expected,
        );
    };
    let first = komponist_general_sampler::Settings::from_markers([0.04]);
    edit_marker(
        &mut app,
        &[],
        MarkerEdit::Insert { seconds: 0.04 },
        first.clone(),
    );
    edit_marker(
        &mut app,
        &[0.04],
        MarkerEdit::Insert { seconds: 0.10 },
        settings.clone(),
    );
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(slice_state(&app), first);
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(slice_state(&app), settings);
    step_until(&mut app, "restored slice map", |app| {
        asset_status(app, general, 0) == Some(&AssetStatus::Ready)
    });
    // Count real callbacks so a delayed bridge block cannot spill into the next measurement.
    let drain_render = |app: &mut App| {
        let blocks = u64::from(SAMPLE_FRAMES.div_ceil(NULL_BUFFER) + 1);
        let complete = app.engine().stats().callbacks() + blocks;
        step_until(app, "sampler preview render window completed", |app| {
            app.engine().stats().callbacks() >= complete
        });
    };
    drain_render(&mut app); // Let the earlier whole-sample audition finish.
    let meter = app.engine().track_meter(general_track).unwrap().clone();
    let preview_peak = |app: &mut App, time| {
        meter.take();
        app.dispatch(Action::Plugin(PluginAction::PreviewWaveform {
            device: general,
            slot: 0,
            key: 60,
            slice_time: Some(time),
        }));
        // Allow a control-plane bridge round trip before draining the complete voice.
        drain_render(app);
        drain_render(app);
        let (left, right) = meter.take();
        left.max(right)
    };
    let quiet_peak = preview_peak(&mut app, 0.02);
    assert!(quiet_peak < 1e-4, "first slice peak: {quiet_peak}");
    let tone_peak = preview_peak(&mut app, 0.08);
    assert!(tone_peak > 1e-3, "second slice peak: {tone_peak}");

    let (drum_kind, drum_track, drum) = tracks[0];
    let select = |app: &mut App, track| {
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![track],
            mode: SelectMode::Replace,
        })));
        app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    };
    select(&mut app, drum_track);
    let mut h = harness(app);
    let save_screenshot = |h: &mut egui_kittest::Harness<'_, App>,
                           device: &str,
                           lang: &str,
                           waveform: Option<&str>| {
        if let Some(folder) = std::env::var_os("KOMPONIST_SAMPLER_UI_SCREENSHOTS") {
            if let Some(label) = waveform {
                let deadline = Instant::now() + PATIENCE;
                while !has_waveform_peaks(h, label) {
                    h.run_steps(1);
                    assert!(Instant::now() < deadline, "{device} waveform did not load");
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            let path = std::path::PathBuf::from(folder);
            std::fs::create_dir_all(&path).unwrap();
            let route = if force_bridge { "bridge" } else { "direct" };
            h.render()
                .unwrap()
                .save(path.join(format!("{device}-{route}-{lang}.png")))
                .unwrap();
        }
    };
    for (language, lang) in [
        (Language::De, komponist_i18n::Lang::De),
        (Language::En, komponist_i18n::Lang::En),
    ] {
        let code = lang.code();
        h.state_mut()
            .dispatch(Action::Settings(SettingsAction::SetLanguage(language)));
        select(h.state_mut(), drum_track);
        h.run_steps(3);
        let pad = h
            .get_by_role_and_label(Role::Button, "Pad tone")
            .rect()
            .center();
        crate::acceptance_support::click_at(&mut h, pad);
        h.get_by_role_and_label(Role::Group, "Pad 3 · tone");
        h.get_by_role_and_label(Role::Slider, "Tune 3");
        save_screenshot(&mut h, "drum-sampler", code, Some("Pad 3 · tone"));
        select(h.state_mut(), general_track);
        h.run_steps(3);
        h.get_by_role_and_label(Role::Button, "tone");
        h.get_by_role_and_label(Role::Group, lang.ui_graph_waveform());
        let marker = format!(
            "{} {}",
            komponist_model::format::note_name(61),
            lang.ui_graph_position()
        );
        let deadline = Instant::now() + PATIENCE;
        while h
            .query_by_role_and_label(Role::Slider, marker.as_str())
            .is_none()
        {
            h.run_steps(1);
            assert!(
                Instant::now() < deadline,
                "Sampler marker was not accessible"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        save_screenshot(
            &mut h,
            "general-sampler",
            code,
            Some(lang.ui_graph_waveform()),
        );
        if lang == komponist_i18n::Lang::En {
            let label = format!(
                "{} {}",
                komponist_model::format::note_name(62),
                lang.ui_graph_position()
            );
            let handle = h.get_by_role_and_label(Role::Slider, &label).rect();
            let just_left = handle.center() - egui::vec2(2.0, 0.0);
            h.hover_at(just_left);
            h.step();
            crate::acceptance_support::press(&mut h, just_left, egui::PointerButton::Primary, true);
            h.step();
            let assignment =
                h.get_by_role_and_label(Role::ComboBox, lang.devices_sampler_midi_assignment());
            assert!(
                assignment
                    .accesskit_node()
                    .value()
                    .is_some_and(|value| value.contains("100")),
                "pressing the left edge of the 100 ms marker must select that marker immediately"
            );
            crate::acceptance_support::press(
                &mut h,
                just_left,
                egui::PointerButton::Primary,
                false,
            );
            h.step();
        }
        let modulation = h
            .get_by_role_and_label(Role::RadioButton, lang.devices_sampler_modulation())
            .rect()
            .center();
        crate::acceptance_support::click_at(&mut h, modulation);
        h.get_by_role_and_label(Role::Group, lang.devices_sampler_amp_envelope());
        h.get_by_role_and_label(Role::Group, lang.devices_sampler_filter_envelope());
        h.get_by_role_and_label(Role::Label, lang.devices_sampler_lfo());
        h.get_by_role_and_label(Role::Label, lang.devices_sampler_velocity());
        let deadline = Instant::now() + PATIENCE;
        while !h
            .state()
            .engine()
            .host()
            .device_display(general)
            .is_some_and(|display| {
                display
                    .frames
                    .iter()
                    .any(|frame| frame.key == "filter-envelope")
            })
        {
            h.run_steps(1);
            assert!(
                Instant::now() < deadline,
                "Sampler filter envelope display did not load"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        save_screenshot(&mut h, "general-sampler-modulation", code, None);
        let sample = h
            .get_by_role_and_label(Role::RadioButton, lang.devices_sampler_sample())
            .rect()
            .center();
        crate::acceptance_support::click_at(&mut h, sample);
    }
    select(h.state_mut(), drum_track);
    let drum_layout = h.state().engine().host().native_layout(drum).unwrap();
    let bank = drum_layout.pad_bank.as_ref().unwrap();
    let follow = bank.follow_param.unwrap();
    let note = bank.pads[3].note;
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device: drum,
            id: follow,
            value: 1.0,
        }));
    step_until(h.state_mut(), "pad follow enabled", |app| {
        app.engine().host().param_base(drum, follow) == Some(1.0)
    });
    h.run_steps(3);
    h.state_mut().engine_mut().send(Command::LiveNote {
        track: drum_track,
        key: note,
        velocity: 85,
    });
    let lang = komponist_i18n::Lang::En;
    let followed = lang.devices_waveform_title(4, lang.devices_no_sample());
    let deadline = Instant::now() + PATIENCE;
    while h.query_by_role_and_label(Role::Group, &followed).is_none() {
        h.run_steps(1);
        assert!(
            Instant::now() < deadline,
            "pad did not follow the incoming note"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    h.state_mut().engine_mut().send(Command::LiveNote {
        track: drum_track,
        key: note,
        velocity: 0,
    });
    let start = h
        .state()
        .engine()
        .host()
        .native_layout(drum)
        .unwrap()
        .author_ids()
        .unwrap()["pad3_start"];
    let start_info = drum_kind
        .params()
        .iter()
        .find(|param| param.id.as_str() == "pad3_start")
        .unwrap();
    let trim = pinned_clap_state::to_clap(start_info, 10.0);
    let default_trim = pinned_clap_state::to_clap(start_info, start_info.default);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device: drum,
            id: start,
            value: trim,
        }));
    let has_trim = |app: &App, value: f64| {
        app.engine()
            .host()
            .param_base(drum, start)
            .is_some_and(|current| (current - value).abs() < 1e-6)
    };
    step_until(h.state_mut(), "edited pad trim", |app| has_trim(app, trim));
    let alternate = scratch.path().join("alternate.wav");
    write_sample(&alternate);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: drum,
            slot: 2,
            path: Some(alternate.clone()),
        }));
    assert_eq!(
        h.state()
            .project()
            .find_device(drum)
            .unwrap()
            .2
            .parameter_bases
            .get(&start),
        Some(&default_trim)
    );
    step_until(
        h.state_mut(),
        "replacement reset the selected pad trim",
        |app| {
            has_trim(app, default_trim)
                && app.project().find_device(drum).unwrap().2.asset_links[&2].abs == alternate
        },
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    step_until(
        h.state_mut(),
        "undo restored the original sample and trim",
        |app| {
            has_trim(app, trim)
                && app.project().find_device(drum).unwrap().2.asset_links[&2].abs == sample
        },
    );
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: drum,
            slot: 2,
            path: None,
        }));
    step_until(h.state_mut(), "cleared pad", |app| {
        asset_status(app, drum, 2) == Some(&AssetStatus::Empty) && has_trim(app, trim)
    });
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    step_until(h.state_mut(), "restored pad", |app| {
        asset_status(app, drum, 2) == Some(&AssetStatus::Ready)
    });
    let host = h.state().engine().host();
    let gain = host.native_layout(general).unwrap().author_ids().unwrap()["gain"];
    let info = Internal::GeneralSampler
        .params()
        .iter()
        .find(|p| p.id.as_str() == "gain")
        .unwrap();
    let raw = |db| pinned_clap_state::to_clap(info, db);
    let hosted_state = |app: &App| {
        let (_, _, d) = app.project().find_device(general).unwrap();
        pinned_clap_state::decode(d.state.as_deref().unwrap()).unwrap()
    };
    let gain_saved = |app: &App, db| {
        hosted_state(app)["params"]["gain"]["f32"]
            .as_f64()
            .is_some_and(|gain| (gain - raw(db)).abs() < 1e-6)
    };
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
    // The marker edit must use the previous gain even if the cancel is still reaching audio.
    let changed_slices = komponist_general_sampler::Settings::from_markers([0.02, 0.10]);
    edit_marker(
        h.state_mut(),
        &[0.04, 0.10],
        MarkerEdit::Move {
            index: 0,
            seconds: 0.02,
        },
        changed_slices,
    );
    assert!(gain_saved(h.state(), -18.0));
    step_until(
        h.state_mut(),
        "the cancelled turn stays cancelled after a slice edit",
        |app| {
            let host = app.engine().host();
            asset_status(app, general, 0) == Some(&AssetStatus::Ready)
                && host
                    .param_base(general, gain)
                    .is_some_and(|value| (value - raw(-18.0)).abs() < 1e-6)
        },
    );

    let replacement = scratch.path().join("replacement.wav");
    write_sample(&replacement);
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::PickPluginAsset {
            device: general,
            slot: 0,
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
    let sample_name = |app: &App| {
        app.project().find_device(general).unwrap().2.asset_links[&0]
            .name()
            .unwrap()
            .into_owned()
    };
    assert_eq!(sample_name(h.state()), "replacement");
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(sample_name(h.state()), "tone");
    let stored_gain = hosted_state(h.state())["params"]["gain"]["f32"]
        .as_f64()
        .unwrap();
    assert!((stored_gain - raw(-22.0)).abs() < 1e-6);
    step_until(h.state_mut(), "sample undo completed", |app| {
        asset_status(app, general, 0) == Some(&AssetStatus::Ready)
    });

    set_gain(h.state_mut(), -28.0);
    h.state_mut().engine_mut().host_mut().request_state(general);
    step_until(h.state_mut(), "the card's gain was saved", |app| {
        gain_saved(app, -28.0)
    });

    // Simulate a newer change from the host, bypassing the application's card actions.
    let host = h.state_mut().engine_mut().host_mut();
    assert!(host.set_param(general, gain, raw(-9.0)));
    host.request_state(general);
    step_until(h.state_mut(), "the newer hosted gain was saved", |app| {
        gain_saved(app, -9.0)
    });
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: general,
            slot: 0,
            path: Some(replacement),
        }));
    assert_eq!(sample_name(h.state()), "replacement");
    assert!(gain_saved(h.state(), -9.0));

    step_until(h.state_mut(), "replacement is ready for playback", |app| {
        asset_status(app, general, 0) == Some(&AssetStatus::Ready)
    });
    let sample_peak = |app: &mut App| {
        app.engine().track_meter(general_track).unwrap().take();
        note_peak(app, general_track, 60)
    };
    let deadline = Instant::now() + PATIENCE;
    let base_peak = loop {
        let peak = sample_peak(h.state_mut());
        if peak > 1e-3 {
            break peak;
        }
        // The previous file's first slice is silent. A Ready inventory may still describe it.
        assert!(
            Instant::now() < deadline,
            "the unautomated replacement sample is silent: peak {peak}"
        );
    };
    let assert_live_automation = |app: &mut App, phase: &str| {
        let ready_after = app.engine().stats().callbacks() + 2;
        step_until(app, &format!("{phase}: audio callbacks advanced"), |app| {
            app.engine().stats().callbacks() >= ready_after
        });

        // Ready and StateApplied can precede the first sounding block after a bridge
        // restore. A silent probe cannot measure gain. Retry only silence, and check
        // the first audible probe so an incorrect automation value still fails at once.
        let deadline = Instant::now() + PATIENCE;
        let late_at_start = app.engine().host().late_blocks(general);
        let mut silent_notes = 0;
        let peak = loop {
            let peak = sample_peak(app);
            if peak != 0.0 {
                break peak;
            }
            silent_notes += 1;
            assert!(
                Instant::now() < deadline,
                "{phase}: sampler stayed silent after {silent_notes} notes; \
                 asset status {:?}, bridge late blocks {} (started at {late_at_start})",
                asset_status(app, general, 0),
                app.engine().host().late_blocks(general),
            );
        };
        if silent_notes > 0 {
            eprintln!("{phase}: sampler sounded after {silent_notes} silent notes");
        }
        let ratio = peak / base_peak;
        assert!(
            (0.01..0.05).contains(&ratio),
            "{phase}: the -42 dB lane did not attenuate the -9 dB base: peak {peak}, base {base_peak}"
        );
    };
    let owner = AutomationOwner::Track(general_track);
    let target = AutomationTarget::Param {
        device: general,
        param: komponist_model::params::ParamId::plugin(gain),
    };
    h.state_mut()
        .dispatch(Action::Automation(Automation::ShowTarget {
            owner,
            replace: None,
            target: target.clone(),
            label: "Gain".into(),
        }));
    let lane = h
        .state()
        .project()
        .track(general_track)
        .unwrap()
        .automation
        .lane(&target)
        .unwrap()
        .id;
    h.state_mut()
        .dispatch(Action::Automation(Automation::Points(
            Source::Lane(owner, lane),
            Points::new(vec![Point::linear(Tick::ZERO, raw(-42.0) as f32)]),
            "undo-automation-add-point",
        )));
    h.state_mut().engine_mut().send(Command::Play);
    step_until(
        h.state_mut(),
        "sampler gain automation is installed",
        |app| {
            asset_status(app, general, 0) == Some(&AssetStatus::Ready)
                && app.engine().transport() == komponist_engine::TransportState::Playing
                && app
                    .engine()
                    .program()
                    .is_some_and(|program| program.automation_reads(owner, &target))
        },
    );
    assert_live_automation(h.state_mut(), "after installing the gain lane");
    let before = slice_state(h.state());
    let prior: Vec<_> = before.slices.iter().map(|slice| slice.time).collect();
    let seconds = 0.08;
    let mut after = before.clone();
    after.slices.push(Default::default());
    after.slices[prior.len()].time = seconds;
    after.normalize();
    edit_marker(
        h.state_mut(),
        &prior,
        MarkerEdit::Insert { seconds },
        after.clone(),
    );
    assert!(gain_saved(h.state(), -9.0));
    let wait_for_restore = |app: &mut App| {
        step_until(app, "sampler marker state restored", |app| {
            app.device_events().iter().any(|event| {
                matches!(event, PluginManagerEvent::StateApplied { device } if *device == general)
            })
        });
        step_until(app, "restored sampler asset ready", |app| {
            asset_status(app, general, 0) == Some(&AssetStatus::Ready)
                && app.engine().transport() == komponist_engine::TransportState::Playing
        });
    };
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(slice_state(h.state()), before);
    assert!(gain_saved(h.state(), -9.0));
    wait_for_restore(h.state_mut());
    assert_live_automation(h.state_mut(), "after undoing the marker edit");
    h.state_mut().dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(slice_state(h.state()), after);
    assert!(gain_saved(h.state(), -9.0));
    wait_for_restore(h.state_mut());
    assert_live_automation(h.state_mut(), "after redoing the marker edit");
    assert_eq!(sample_name(h.state()), "replacement");
    h.state_mut().shutdown();
}

fn has_waveform_peaks(harness: &egui_kittest::Harness<'_, App>, label: &str) -> bool {
    use egui::{Shape, accesskit::Role};

    let Some(graph) = harness.query_by_role_and_label(Role::Group, label) else {
        return false;
    };
    let rect = graph.rect();
    // The waveform data is painted as a strip spanning the plot, with two vertices per column.
    harness.output().shapes.iter().any(|clipped| {
        let Shape::Mesh(mesh) = &clipped.shape else {
            return false;
        };
        let bounds = mesh.calc_bounds();
        mesh.vertices.len() == 2 * rect.width().ceil() as usize
            && bounds.height() > 1.0
            && rect.expand(1.0).contains_rect(bounds)
    })
}

fn assert_sample_sources(project: &Project, project_dir: &Path, source: &Path) {
    for track in &project.tracks {
        let device = track.channel.instrument().unwrap().1;
        if let Some(link) = device.asset_links.get(&0) {
            assert_eq!(link.resolve(Some(project_dir)).as_deref(), Some(source));
            continue;
        }
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
    for frame in 0..SAMPLE_FRAMES as usize {
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
