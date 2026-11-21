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
    write_wav(&destination.join("tone.wav"), 0);
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
    let saved = load_project(&copy).unwrap();
    let source = moved.join("tone.wav");
    for track in &saved.tracks {
        let device = track.channel.instrument().unwrap().1;
        let state = device.state.as_deref().unwrap();
        let wrapper: serde_json::Value = serde_json::from_slice(&state[8..]).unwrap();
        let assets = wrapper["fields"][pinned_clap_state::SAMPLE_ASSETS_FIELD]
            .as_str()
            .unwrap();
        assert_eq!(
            links::read(Some(assets.as_bytes()))[0]
                .1
                .resolve(Some(&destination)),
            Some(source.clone())
        );
    }

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
    app.shutdown();
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
        let sample = if frame / 12 % 2 == 0 {
            11_000_i16
        } else {
            -amplitude
        };
        writer.write_sample(sample).unwrap();
    }
    writer.finalize().unwrap();
}
