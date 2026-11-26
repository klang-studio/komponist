//! Packaged sampler state tests, launched by `scripts/measure-bundled-clap.py --current-projects`.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui_kittest::kittest::{NodeT as _, Queryable as _};

use komponist_app::actions::PluginAction;
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_app::{Action, App, ProjectAction};
use komponist_engine::Command;
use komponist_general_sampler as general_sampler;
use komponist_ipc::device_assets::AssetStatus;
use komponist_model::edits::{self, NewTrack};
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{DeviceId, Project, TrackId};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent};

use crate::acceptance_support::harness;
use crate::common::{NULL_BUFFER, PATIENCE, app_with, config, step_for, step_until};

const SAMPLE_FRAMES: u32 = 8192;

/// The first of the Sampler bundle's eight pad notes (the pad bank's own `note` fields).
const FIRST_PAD_NOTE: u8 = 36;

fn note_of_pad(pad: usize) -> u8 {
    FIRST_PAD_NOTE + pad as u8
}

/// The pinned wrapper's state document: an 8-byte little-endian length, then JSON.
fn decode_state(state: &[u8]) -> Option<serde_json::Value> {
    let (length, payload) = state.split_first_chunk::<8>()?;
    (u64::from_le_bytes(*length) == payload.len() as u64)
        .then(|| serde_json::from_slice(payload).ok())?
}

/// The pinned wrapper's state document without parameters and fields.
fn encode_empty_state(version: &str) -> Vec<u8> {
    let payload =
        serde_json::to_vec(&serde_json::json!({"version": version, "params": {}, "fields": {}}))
            .unwrap();
    let mut state = (payload.len() as u64).to_le_bytes().to_vec();
    state.extend_from_slice(&payload);
    state
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sampler {
    Pads,
    General,
}

impl Sampler {
    fn id(self) -> &'static str {
        match self {
            Self::Pads => "org.klang-studio.komponist.sampler",
            Self::General => "org.klang-studio.komponist.general-sampler",
        }
    }
}

fn asset_status(app: &App, device: DeviceId, id: u32) -> Option<&AssetStatus> {
    let slots = app.engine().host().asset_slots(device)?;
    slots
        .slots
        .iter()
        .find(|slot| slot.id == id)
        .map(|slot| &slot.status)
}

#[test]
#[ignore = "requires a verified all-device package; run with scripts/measure-bundled-clap.py --current-projects"]
fn both_samplers_play_assets_after_the_project_moves() {
    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP")
        .expect("set the unpacked app with scripts/measure-bundled-clap.py --current-projects");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let before_dir = scratch.path().join("before");
    std::fs::create_dir(&before_dir).unwrap();
    let asset = before_dir.join("tone.wav");
    write_sample(&asset);
    let link = AssetLink::new(&asset, Some(&before_dir));

    // The current format: bundled sampler references with a host-managed link in slot 0.
    let host = scanned_packaged_sampler_host(scratch.path(), launcher.clone().into());
    let mut source = Project::new("Moved sampler project");
    for id in [
        "org.klang-studio.komponist.sampler",
        "org.klang-studio.komponist.general-sampler",
    ] {
        let prepared = host
            .manager()
            .prepare_bundled_device(id, None, &[], &[])
            .unwrap();
        let state = prepared.state.clone();
        let (next, track) = edits::add_track(
            &source,
            NewTrack {
                name: None,
                hue: None,
                instrument: Some(prepared.plugin),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = next
            .track(track)
            .unwrap()
            .channel
            .instrument()
            .unwrap()
            .1
            .id;
        let next = edits::set_device_state(&next, device, state).unwrap();
        source = edits::set_device_asset_links(&next, device, [(0, link.clone())].into()).unwrap();
    }
    drop(host);
    let project = source;
    let filename = "samples.komponist";
    save_project(&before_dir.join(filename), &project).unwrap();
    let moved = scratch.path().join("moved");
    std::fs::rename(&before_dir, &moved).unwrap();
    assert!(!asset.exists());
    let reopened_path = moved.join(filename);
    assert_eq!(load_project(&reopened_path).unwrap(), project);

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
    for (index, key) in [note_of_pad(0), 60].into_iter().enumerate() {
        let track = &project.tracks[index];
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
    assert_eq!(app.project().as_ref(), &project);
    assert!(
        !app.doc().is_dirty(),
        "runtime path refresh must not edit the project"
    );

    // Exercise state restore with the original stale absolute fallback too.
    let track = &project.tracks[1];
    let device = track.channel.instrument().unwrap().1;
    let linked = device.state.as_ref().unwrap();
    let empty = Arc::from(encode_empty_state(&device.plugin.version));
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
        app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: sample.device,
            slot: sample.pad as u32,
            path: Some(replacement.clone()),
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
    step_until(app, "both repaired sample links", |app| {
        komponist_app::dialogs::missing_plugins::missing_samples(app.project(), Some(&destination))
            .is_empty()
            && missing_assets(app).is_empty()
    });
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
    for (track, key) in project.tracks.iter().zip([note_of_pad(0), 60]) {
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
fn general_sampler_file_edits_preserve_slice_history() {
    use komponist_app::actions::EditAction;
    use komponist_ipc::waveform_markers::SliceOptions;
    use komponist_model::plugin_assets::AssetLink;

    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP").expect("packaged launcher");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let original = scratch.path().join("original.wav");
    let replacement = scratch.path().join("replacement.wav");
    let missing = scratch.path().join("missing.wav");
    write_sample(&original);
    write_wav(&replacement, 9_000);
    let original = Some(original.as_path());
    let replacement = Some(replacement.as_path());
    let cases = [
        ("assign", None, replacement, true, true),
        ("replace", original, replacement, true, true),
        ("clear", original, None, true, true),
        ("reload", original, original, true, true),
        ("relink", Some(missing.as_path()), replacement, true, false),
        ("hidden", original, replacement, false, true),
    ];

    let mut slices = general_sampler::Settings::from_markers([0.04, 0.10]);
    slices.first.reverse = Some(true);
    slices.first.fade_out_ms = Some(3.0);
    slices.slices[0].options.gate = true;
    slices.slices[1].options.fade_in_ms = Some(7.0);
    let empty_slices = general_sampler::Settings::default();

    for (case, before_path, after_path, slice_mode, reset) in cases {
        let mode = if slice_mode { 2.0 } else { 0.0 };
        let host = scanned_packaged_sampler_host(scratch.path(), launcher.clone().into());
        let prepared = host
            .manager()
            .prepare_bundled_device(
                "org.klang-studio.komponist.general-sampler",
                None,
                &[("mode", mode), ("start", 0.1)],
                &[],
            )
            .unwrap();
        let (project, track) = edits::add_track(
            &Project::new(case),
            NewTrack {
                name: None,
                hue: None,
                instrument: Some(prepared.plugin),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let sampler = project
            .track(track)
            .unwrap()
            .channel
            .instrument()
            .unwrap()
            .1
            .id;
        let seeded = komponist_plugins::bundled_sampler::edit_slices(
            prepared.state.as_deref().unwrap(),
            &slices,
        )
        .unwrap();
        let project = edits::set_device_state(&project, sampler, Some(seeded.into())).unwrap();
        let initial_links = before_path.map(|path| (0, AssetLink::new(path, None)));
        let project =
            edits::set_device_asset_links(&project, sampler, initial_links.into_iter().collect())
                .unwrap();
        let file = scratch.path().join(format!("{case}.komponist"));
        save_project(&file, &project).unwrap();
        let mut app =
            app_with(load_project(&file).unwrap(), host).with_lang(komponist_i18n::Lang::En);

        let wait_for = |app: &mut App,
                        phase,
                        path: Option<&Path>,
                        settings: &general_sampler::Settings,
                        previous_revision: Option<u64>| {
            let status = match path {
                Some(path) if path.is_file() => AssetStatus::Ready,
                Some(_) => AssetStatus::Missing,
                None => AssetStatus::Empty,
            };
            let positions: Vec<_> = settings.slices.iter().map(|slice| slice.time).collect();
            let options: Option<Vec<_>> = slice_mode.then(|| {
                std::iter::once(settings.first)
                    .chain(settings.slices.iter().map(|slice| slice.options))
                    .map(|options| SliceOptions {
                        gate: options.gate,
                        reverse: options.reverse,
                        fade_in_ms: options.fade_in_ms,
                        fade_out_ms: options.fade_out_ms,
                    })
                    .collect()
            });
            step_until(
                app,
                &format!("{case} {phase}: committed file link and slices"),
                |app| {
                    let device = app.project().find_device(sampler).unwrap().2;
                    device.asset_links.get(&0).map(|link| link.abs.as_path()) == path
                        && sampler_slice_settings(app, sampler) == *settings
                        && asset_status(app, sampler, 0) == Some(&status)
                },
            );

            let deadline = Instant::now() + PATIENCE;
            loop {
                app.dispatch(Action::Plugin(PluginAction::RequestWaveformMarkers {
                    device: sampler,
                    slot: 0,
                }));
                app.step();
                let live = app.engine().host().waveform_markers(sampler, 0);
                if let Some(markers) = live
                    && Some(markers.revision) != previous_revision
                    && markers.positions == positions
                    && markers.slice_options == options
                    && markers.editable == (slice_mode && status == AssetStatus::Ready)
                {
                    return markers.revision;
                }
                assert!(
                    Instant::now() < deadline,
                    "{case} {phase}: live marker state {live:?}"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        let mut revision = wait_for(&mut app, "reopen", before_path, &slices, None);
        let host = app.engine().host();
        assert!(host.verified_bundled_kind(sampler).is_some(), "{case}");
        assert!(
            host.supports_asset_edit_state(sampler),
            "{case}: expected authored file edits"
        );
        assert_eq!(host.bridge_pid(sampler).is_some(), force_bridge, "{case}");
        let ids = host.native_layout(sampler).unwrap().author_ids().unwrap();
        let start = ids["start"];
        let mode_id = ids["mode"];
        let gain = ids["gain"];
        let default_gain = host.param_base(sampler, gain).unwrap();
        let edit_gain = matches!(case, "replace" | "clear");
        let pending_gain = if edit_gain { 0.25 } else { default_gain };
        if edit_gain {
            assert_ne!(pending_gain, default_gain);
            // Submit the file edit before another frame can capture this knob turn.
            app.dispatch(Action::Plugin(PluginAction::SetParam {
                device: sampler,
                id: gain,
                value: pending_gain,
            }));
        }
        let before_reload = (case == "reload").then(|| {
            let peak = audible_note_peak(&mut app, track, 60);
            step_for(&mut app, Duration::from_millis(250));
            write_wav(before_path.unwrap(), 2_000);
            peak
        });
        assert_eq!(app.doc().undo_label(), None, "{case}");
        app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
            device: sampler,
            slot: 0,
            path: after_path.map(Path::to_owned),
        }));
        let after_slices = if reset { &empty_slices } else { &slices };
        let after_start = if reset && after_path.is_some() {
            0.0
        } else {
            0.1
        };
        let check_bases = |app: &mut App, phase, expected_start| {
            step_until(
                app,
                &format!("{case} {phase}: parameter bases restored"),
                |app| {
                    let host = app.engine().host();
                    let device = app.project().find_device(sampler).unwrap().2;
                    let saved_gain = device.parameter_bases.get(&gain).copied().or_else(|| {
                        let blob = decode_state(device.state.as_deref()?)?;
                        blob["params"]["gain"]["f32"].as_f64()
                    });
                    let near = |value: Option<f64>, expected: f64| {
                        value.is_some_and(|value| (value - expected).abs() < 1e-6)
                    };
                    near(saved_gain, pending_gain)
                        && near(host.param_base(sampler, start), expected_start)
                        && near(host.param_base(sampler, mode_id), mode)
                        && near(host.param_base(sampler, gain), pending_gain)
                },
            );
        };

        revision = wait_for(&mut app, "edit", after_path, after_slices, Some(revision));
        check_bases(&mut app, "edit", after_start);
        if let Some(before) = before_reload {
            let after = audible_note_peak(&mut app, track, 60);
            assert!(
                (0.08..0.35).contains(&(after / before)),
                "same-path reload did not play the quieter replacement: {before} -> {after}"
            );
        }
        assert_eq!(
            app.undo_text().as_deref(),
            Some("Change Plug-in File Link"),
            "{case}"
        );

        app.dispatch(Action::Edit(EditAction::Undo));
        revision = wait_for(&mut app, "undo", before_path, &slices, Some(revision));
        check_bases(&mut app, "undo", 0.1);
        assert_eq!(app.doc().undo_label().is_some(), edit_gain, "{case}");

        app.dispatch(Action::Edit(EditAction::Redo));
        wait_for(&mut app, "redo", after_path, after_slices, Some(revision));
        check_bases(&mut app, "redo", after_start);
        assert_eq!(app.doc().redo_label(), None, "{case}");
        app.shutdown();
    }
}

fn sampler_slice_settings(app: &App, sampler: DeviceId) -> general_sampler::Settings {
    let device = app.project().find_device(sampler).unwrap().2;
    let assets = device
        .state
        .as_deref()
        .and_then(komponist_plugins::bundled_sampler::assets);
    general_sampler::read_settings(assets.as_deref().map(str::as_bytes))
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
    for kind in [Sampler::Pads, Sampler::General] {
        let instrument =
            crate::common::shipped_plugin(&host, kind.id()).expect("verified sampler bundle");
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
        let pad = usize::from(*kind == Sampler::Pads) * 2;
        step_until(&mut app, "initial sampler UI", |app| {
            let Some(layout) = app.engine().host().native_layout(*device) else {
                return false;
            };
            assert!(layout.has_audio_asset_editor());
            asset_status(app, *device, pad as u32) == Some(&AssetStatus::Empty)
        });
        assert_eq!(
            app.engine().host().bridge_pid(*device).is_some(),
            force_bridge
        );
        // Turn a knob and replace the sample before the host has captured the knob turn.
        let edited = if *kind == Sampler::General {
            let wire = app
                .engine()
                .host()
                .native_layout(*device)
                .unwrap()
                .author_ids()
                .unwrap()["gain"];
            let value = crate::common::author_wire_value(&app, *device, wire, -12.0);
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
        let key = if *kind == Sampler::Pads {
            note_of_pad(pad)
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
            .and_then(decode_state)
            .is_some_and(|state| state["params"]["mode"]["string"] == "slices")
    });
    let settings = komponist_general_sampler::Settings::from_markers([0.04, 0.10]);
    let slice_state = |app: &App| sampler_slice_settings(app, general);
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
    let host = h.state().engine().host();
    let drum_layout = host.native_layout(drum).unwrap().clone();
    let start = drum_layout.author_ids().unwrap()["pad3_start"];
    let trim = crate::common::wire_value(&drum_layout, start, 10.0);
    let default_trim = host
        .params(drum)
        .unwrap()
        .iter()
        .find(|info| info.id == start)
        .unwrap()
        .default;
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
    step_until(
        h.state_mut(),
        "replacement reset the selected pad trim",
        |app| {
            has_trim(app, default_trim)
                && app.project().find_device(drum).unwrap().2.asset_links[&2].abs == alternate
        },
    );
    let replaced_pad = h.state().project().find_device(drum).unwrap().2;
    assert!(!replaced_pad.parameter_bases.contains_key(&start));
    let device_state = decode_state(replaced_pad.state.as_deref().unwrap()).unwrap();
    assert!(
        device_state["params"]["pad3_start"]["f32"]
            .as_f64()
            .is_some_and(|saved| (saved - default_trim).abs() < 1e-6),
        "the replaced pad's saved trim is the default"
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
    let layout = host.native_layout(general).unwrap().clone();
    let gain = layout.author_ids().unwrap()["gain"];
    let raw = |db| crate::common::wire_value(&layout, gain, db);
    let hosted_state = |app: &App| {
        let (_, _, d) = app.project().find_device(general).unwrap();
        decode_state(d.state.as_deref().unwrap()).unwrap()
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
    let effective_gain = |app: &App| {
        app.project()
            .find_device(general)
            .unwrap()
            .2
            .parameter_bases
            .get(&gain)
            .copied()
            .unwrap_or_else(|| hosted_state(app)["params"]["gain"]["f32"].as_f64().unwrap())
    };
    let gain_applied = |app: &App, expected: f64| {
        asset_status(app, general, 0) == Some(&AssetStatus::Ready)
            && app
                .engine()
                .host()
                .param_base(general, gain)
                .is_some_and(|value| (value - expected).abs() < 1e-6)
    };
    let previous_gain = effective_gain(h.state());
    let previous_slices = slice_state(h.state());
    assert!((previous_gain - raw(-18.0)).abs() >= 1e-6);
    step_until(h.state_mut(), "the gain before the knob turn", |app| {
        gain_applied(app, previous_gain)
    });
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
        changed_slices.clone(),
    );
    assert!(gain_saved(h.state(), -18.0));
    step_until(
        h.state_mut(),
        "the cancelled turn stays cancelled after a slice edit",
        |app| gain_applied(app, raw(-18.0)),
    );
    for (action, expected_slices, expected_gain, description) in [
        (
            EditAction::Undo,
            &previous_slices,
            raw(-18.0),
            "undoing the slice edit preserves the gain",
        ),
        (
            EditAction::Undo,
            &previous_slices,
            previous_gain,
            "the next undo reverts the preceding knob turn",
        ),
        (
            EditAction::Redo,
            &previous_slices,
            raw(-18.0),
            "redoing the knob turn does not change the slices",
        ),
        (
            EditAction::Redo,
            &changed_slices,
            raw(-18.0),
            "redoing the slice edit preserves the gain",
        ),
    ] {
        let app = h.state_mut();
        app.dispatch(Action::Edit(action));
        assert_eq!(&slice_state(app), expected_slices, "{description}");
        let saved_gain = effective_gain(app);
        assert!(
            (saved_gain - expected_gain).abs() < 1e-6,
            "{description}: expected saved gain {expected_gain}, got {saved_gain}"
        );
        step_until(app, description, |app| gain_applied(app, expected_gain));
    }

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
    step_until(h.state_mut(), "file picker replacement committed", |app| {
        sample_name(app) == "replacement"
    });
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(sample_name(h.state()), "tone");
    let model = h.state().project().find_device(general).unwrap().2;
    let stored_gain = model
        .parameter_bases
        .get(&gain)
        .copied()
        .unwrap_or_else(|| {
            hosted_state(h.state())["params"]["gain"]["f32"]
                .as_f64()
                .unwrap()
        });
    assert!((stored_gain - raw(-22.0)).abs() < 1e-6);
    step_until(h.state_mut(), "sample undo completed", |app| {
        asset_status(app, general, 0) == Some(&AssetStatus::Ready)
            && app
                .engine()
                .host()
                .param_base(general, gain)
                .is_some_and(|value| (value - raw(-22.0)).abs() < 1e-6)
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
    step_until(h.state_mut(), "replacement is ready for playback", |app| {
        sample_name(app) == "replacement"
            && asset_status(app, general, 0) == Some(&AssetStatus::Ready)
    });
    assert!(gain_saved(h.state(), -9.0));
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
        // StateApplied queues automation reassertion to the audio thread. A host
        // callback that fell back on a busy bridge did not confirm its delivery.
        let callback_window = |app: &App| {
            (
                app.engine().stats().callbacks(),
                app.engine().host().late_blocks(general),
            )
        };
        let (mut start, mut late) = callback_window(app);
        step_until(
            app,
            &format!("{phase}: audio callbacks reached the sampler"),
            |app| {
                let (callbacks, current_late) = callback_window(app);
                if current_late != late {
                    (start, late) = (callbacks, current_late);
                }
                callbacks >= start + 2
            },
        );

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
                && !app.engine().host().render_asset_bindings_pending(general)
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
        let link = device.asset_links.get(&0).expect("a host-managed link");
        assert_eq!(link.resolve(Some(project_dir)).as_deref(), Some(source));
    }
}

fn assert_audible(app: &mut App, track: TrackId, key: u8) {
    audible_note_peak(app, track, key);
}

fn audible_note_peak(app: &mut App, track: TrackId, key: u8) -> f32 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        app.engine().track_meter(track).unwrap().take();
        let peak = note_peak(app, track, key);
        if peak > 1e-4 {
            return peak;
        }
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
