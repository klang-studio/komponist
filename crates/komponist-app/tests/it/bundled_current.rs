//! Current-format projects exercised against the eleven verified release bundles.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use komponist_app::actions::{EditAction, ExportRange, PluginAction, ProjectAction};
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_app::{Action, App};
use komponist_builtin::{BuiltinKind, Internal};
use komponist_device_core::pinned_clap_state;
use komponist_engine::Command;
use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
use komponist_model::edits::{self, NewClip, NewClipContent, NewDevice, NewNote, NewTrack};
use komponist_model::macros::Mapping;
use komponist_model::params::ParamId;
use komponist_model::{
    ChannelRef, DeviceId, DeviceRole, PPQ, PluginFormat, PluginRef, Project, Sidechain, Tick,
    TrackId,
};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent};
use serde_json::{Value, json};

use crate::common::{PATIENCE, app_with, config, devices, step_for, step_until};
use crate::export::{export, export_settings, read};

const DEVICE_PREFIX: &str = "org.klang-studio.komponist.";

fn package_env() -> (PathBuf, PathBuf) {
    let required = |key| {
        PathBuf::from(std::env::var_os(key).expect("run the current bundled project test script"))
    };
    (
        required("KOMPONIST_CURRENT_BUNDLED_APP"),
        required("KOMPONIST_CURRENT_BUNDLED_RESULTS"),
    )
}

fn scan(launcher: &Path, work: &Path) -> PluginHost {
    let mut config = config(work);
    config.bridge_exe = launcher.to_owned();
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
        assert!(Instant::now() < deadline, "packaged catalog scan timed out");
        thread::sleep(Duration::from_millis(5));
    }
    host
}

fn shipped(host: &PluginHost, slug: &str) -> PluginRef {
    let id = format!("{DEVICE_PREFIX}{slug}");
    host.catalog()
        .entries()
        .iter()
        .find(|entry| entry.plugin.id == id && entry.plugin.format == PluginFormat::Clap)
        .unwrap_or_else(|| panic!("missing packaged catalog entry {id}"))
        .plugin
        .clone()
}

struct Fixture {
    project: Project,
    hexa: TrackId,
    sampler_tracks: [(TrackId, u8); 2],
    eq: DeviceId,
    scale: DeviceId,
    delay: DeviceId,
}

fn project(host: &PluginHost, sample: &Path) -> Fixture {
    let mut project = Project::new("Bundled CLAP project");
    let mut tracks = Vec::new();
    let sampler_key = komponist_sampler::note_of_pad(0);
    for (index, (name, note)) in [
        ("hexa", 60),
        ("volt", 64),
        ("sampler", sampler_key),
        ("general-sampler", 67),
    ]
    .into_iter()
    .enumerate()
    {
        let (next, track) = edits::add_track(
            &project,
            NewTrack {
                name: Some(name.into()),
                hue: None,
                instrument: Some(shipped(host, name)),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let (next, _) = edits::add_clip(
            &next,
            track,
            NewClip {
                start: Tick::ZERO,
                length: Tick(PPQ * 2),
                name: name.into(),
                content: NewClipContent::Midi {
                    notes: vec![NewNote {
                        start: Tick(index as i64 * PPQ / 8),
                        length: Tick(PPQ),
                        pitch: note,
                        velocity: 76,
                    }],
                    take_name: String::new(),
                },
            },
        )
        .unwrap();
        project = next;
        tracks.push(track);
    }

    let [hexa, volt, sampler, general] = tracks[..] else {
        unreachable!("fixture always creates four tracks");
    };
    let mut insert = |slug, track, role, index, state| {
        let (next, device) = edits::insert_device(
            &project,
            ChannelRef::Track(track),
            index,
            NewDevice {
                role,
                plugin: shipped(host, slug),
                sidechain: None,
                state,
            },
        )
        .unwrap();
        project = next;
        device
    };
    insert("arpeggiator", hexa, DeviceRole::MidiEffect, 0, None);
    let scale = insert("scale", hexa, DeviceRole::MidiEffect, 1, None);
    let eq = insert("eq", hexa, DeviceRole::Effect, 3, None);
    insert("hall", hexa, DeviceRole::Effect, 4, None);
    let delay = insert(
        "delay",
        general,
        DeviceRole::Effect,
        1,
        Some(factory_delay(&shipped(host, "delay").version).into()),
    );
    let presse = insert("presse", volt, DeviceRole::Effect, 1, None);
    insert("limiter", volt, DeviceRole::Effect, 2, None);
    project = edits::set_sidechain(&project, presse, Some(Sidechain::new(sampler))).unwrap();
    for (_, device) in devices(&project) {
        if device.state.is_none() {
            let state = pinned_clap_state::encode(
                &device.plugin.version,
                Default::default(),
                Default::default(),
            );
            project = edits::set_device_state(&project, device.id, Some(state.into())).unwrap();
        }
    }
    let link = komponist_sampler::links::SampleLink::new(sample, sample.parent());
    let assets = komponist_sampler::links::write(None, 0, Some(&link));
    for track in [sampler, general] {
        let device = project
            .track(track)
            .unwrap()
            .channel
            .instrument()
            .unwrap()
            .1;
        let fields = [(
            pinned_clap_state::SAMPLE_ASSETS_FIELD.to_owned(),
            Value::String(String::from_utf8(assets.clone()).unwrap()),
        )]
        .into_iter()
        .collect();
        let state = pinned_clap_state::encode(&device.plugin.version, Default::default(), fields);
        project = edits::set_device_state(&project, device.id, Some(state.into())).unwrap();
    }

    Fixture {
        project,
        hexa,
        sampler_tracks: [(sampler, sampler_key), (general, 67)],
        eq,
        scale,
        delay,
    }
}

fn factory_delay(version: &str) -> Vec<u8> {
    let device = Internal::Delay;
    let params = device
        .params()
        .iter()
        .zip(device.preset_values(0).unwrap())
        .map(|(info, value)| {
            (
                info.id.as_str().to_owned(),
                pinned_clap_state::parameter(info, value).0,
            )
        })
        .collect();
    let name = device.presets()[0].name;
    let fields = [(
        pinned_clap_state::FACTORY_PRESET_FIELD.to_owned(),
        Value::String(serde_json::to_string(name).unwrap()),
    )]
    .into_iter()
    .collect();
    pinned_clap_state::encode(version, params, fields)
}

fn open(app: &mut App, path: &Path) {
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.to_owned())));
    assert_eq!(app.project_path(), Some(path));
}

fn assert_ready(app: &mut App, expected: &[(TrackId, komponist_model::Device)], bridge: bool) {
    step_until(app, "all eleven authenticated CLAP instances", |app| {
        expected.iter().all(|(_, device)| {
            let host = app.engine().host();
            host.verified_bundled_kind(device.id).is_some()
                && host.native_layout(device.id).is_some()
        })
    });
    for (_, device) in expected {
        let host = app.engine().host();
        assert_eq!(
            host.bridge_pid(device.id).is_some(),
            bridge,
            "{}",
            device.plugin.id
        );
        assert!(
            host.state_warning(device.id).is_none(),
            "{}",
            device.plugin.id
        );
    }
}

fn assert_audible(app: &mut App, track: TrackId, key: u8) {
    let deadline = Instant::now() + PATIENCE;
    loop {
        app.engine_mut().send(Command::LiveNote {
            track,
            key,
            velocity: 84,
        });
        step_for(app, Duration::from_millis(100));
        app.engine_mut().send(Command::LiveNote {
            track,
            key,
            velocity: 0,
        });
        let (left, right) = app.engine().track_meter(track).unwrap().take();
        if left.max(right) > 1e-4 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "sampler track {track:?} is silent"
        );
    }
}

fn write_sample(path: &Path) {
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
    for frame in 0..12_000 {
        let phase = std::f64::consts::TAU * f64::from(frame) / 148.0;
        writer.write_sample((8_000.0 * phase.sin()) as i16).unwrap();
    }
    writer.finalize().unwrap();
}

fn assert_export_parity(direct: &Path, bridged: &Path) {
    let direct = read(direct);
    let bridged = read(bridged);
    assert_eq!(direct.frames, bridged.frames);
    assert_eq!(direct.spec, bridged.spec);
    for window in direct
        .samples
        .chunks(4_800)
        .zip(bridged.samples.chunks(4_800))
    {
        let (direct, bridged) = window;
        let rms = |samples: &[f32]| {
            (samples.iter().map(|value| value * value).sum::<f32>() / samples.len() as f32).sqrt()
        };
        let floor = 0.002;
        let left = rms(direct);
        let right = rms(bridged);
        assert!(
            (left - right).abs() <= floor + left.max(right) * 0.07,
            "{left} vs {right}"
        );
    }
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn current_project_round_trip_and_offline_render() {
    let (launcher, results) = package_env();
    let bridge = std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some();
    let mode = if bridge { "bridge" } else { "direct" };
    let work = results.join(mode);
    std::fs::create_dir(&work).unwrap();
    let project_folder = work.join("authored");
    std::fs::create_dir(&project_folder).unwrap();
    let sample = project_folder.join("fixture.wav");
    write_sample(&sample);

    let cache = tempfile::tempdir().unwrap();
    let host = scan(&launcher, cache.path());
    let fixture = project(&host, &sample);
    let mut authored = fixture.project;
    let path = project_folder.join("bundled.komponist");
    let expected = devices(&authored);
    assert_eq!(expected.len(), 11);
    save_project(&path, &authored).unwrap();
    let mut app = app_with(Project::new("Empty"), host);
    open(&mut app, &path);
    assert_ready(&mut app, &expected, bridge);
    let host = app.engine().host();
    assert_eq!(
        host.verified_bundled_kind(fixture.eq),
        Some(BuiltinKind::Internal(Internal::Eq))
    );
    let ids = host
        .native_layout(fixture.eq)
        .unwrap()
        .author_ids()
        .unwrap();
    let frequency = ids["b1_freq"];
    let gain = ids["b1_gain"];
    let scale_mode = host
        .native_layout(fixture.scale)
        .unwrap()
        .author_ids()
        .unwrap()["scale"];
    app.dispatch(Action::Edit(EditAction::SetDeviceBypass {
        device: fixture.eq,
        bypass: true,
    }));
    assert!(app.project().find_device(fixture.eq).unwrap().2.bypass);
    app.dispatch(Action::Edit(EditAction::Undo));
    assert!(!app.project().find_device(fixture.eq).unwrap().2.bypass);
    app.dispatch(Action::Edit(EditAction::Redo));
    assert!(app.project().find_device(fixture.eq).unwrap().2.bypass);
    app.dispatch(Action::Edit(EditAction::Undo));

    let owner = AutomationOwner::Track(fixture.hexa);
    let (next, lane) = edits::automation::add_lane(
        &authored,
        owner,
        AutomationTarget::Param {
            device: fixture.eq,
            param: ParamId::plugin(frequency),
        },
        "EQ frequency".into(),
    )
    .unwrap();
    authored = edits::automation::set_lane_points(
        &next,
        owner,
        lane,
        Points::new(vec![
            Point::linear(Tick::ZERO, 0.29),
            Point::linear(Tick(PPQ), 0.71),
        ]),
    )
    .unwrap();
    authored = edits::macros::add_mapping(
        &authored,
        fixture.hexa,
        0,
        Mapping {
            target: AutomationTarget::Param {
                device: fixture.eq,
                param: ParamId::plugin(gain),
            },
            label: "EQ gain".into(),
            min: 0.4,
            max: 0.65,
            invert: false,
            bend: 0.35,
        },
    )
    .unwrap();
    authored = edits::macros::set_macro_value(&authored, fixture.hexa, 0, 0.38).unwrap();
    save_project(&path, &authored).unwrap();
    open(&mut app, &path);
    assert_ready(&mut app, &expected, bridge);

    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: fixture.scale,
        id: scale_mode,
        value: 3.0,
    }));
    for (track, key) in fixture.sampler_tracks {
        assert_audible(&mut app, track, key);
    }
    step_until(&mut app, "new scale enum applied", |app| {
        app.engine().host().param_base(fixture.scale, scale_mode) == Some(3.0)
    });
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved = load_project(&path).unwrap();
    let (_, _, scale) = saved.find_device(fixture.scale).unwrap();
    assert_ne!(
        scale.state,
        authored.find_device(fixture.scale).unwrap().2.state
    );
    let mut with_current_state = authored;
    for (_, device) in devices(&saved) {
        assert_eq!(device.plugin.format, PluginFormat::Clap);
        assert!(
            device.state.is_some(),
            "{} has no saved CLAP state",
            device.plugin.id
        );
        with_current_state =
            edits::set_device_state(&with_current_state, device.id, device.state).unwrap();
    }
    assert_eq!(saved, with_current_state);
    let (_, _, delay) = saved.find_device(fixture.delay).unwrap();
    let raw: Value = serde_json::from_slice(&delay.state.as_ref().unwrap()[8..]).unwrap();
    assert_eq!(
        raw["fields"][pinned_clap_state::FACTORY_PRESET_FIELD],
        json!(serde_json::to_string(Internal::Delay.presets()[0].name).unwrap()),
    );

    app.shutdown();
    let relocated = work.join("relocated");
    std::fs::rename(project_folder, &relocated).unwrap();
    assert!(!sample.exists());
    let mut app = app_with(Project::new("Empty"), scan(&launcher, cache.path()));
    open(&mut app, &relocated.join("bundled.komponist"));
    assert_eq!(app.project().as_ref(), &saved);
    assert_ready(&mut app, &expected, bridge);
    assert_eq!(
        app.engine().host().param_base(fixture.scale, scale_mode),
        Some(3.0)
    );
    for (track, key) in fixture.sampler_tracks {
        assert_audible(&mut app, track, key);
    }

    let output = work.join("out/bundled.wav");
    let mut settings = export_settings(&work, ExportRange::Song);
    settings.file_name = "bundled.wav".into();
    settings.sample_rate = 48_000;
    settings.normalize = false;
    settings.tail_seconds = 0.0;
    export(&mut app, settings);
    let wav = read(&output);
    assert!(wav.samples.iter().all(|sample| sample.is_finite()));
    assert!(wav.samples.iter().any(|sample| sample.abs() > 0.01));
    if bridge {
        assert_export_parity(&results.join("direct/out/bundled.wav"), &output);
    }
    app.shutdown();
}

#[test]
#[ignore = "requires a runner with missing or modified release payload; run measure-bundled-clap.py --current-projects"]
fn unavailable_bundle_keeps_saved_project_intact() {
    let (launcher, results) = package_env();
    let path = results.join("direct/relocated/bundled.komponist");
    let bytes = std::fs::read(&path).unwrap();
    let saved = load_project(&path).unwrap();
    let eq = devices(&saved)
        .into_iter()
        .find(|(_, device)| device.plugin.id == format!("{DEVICE_PREFIX}eq"))
        .unwrap()
        .1;
    let work = tempfile::tempdir().unwrap();
    let mut app = app_with(Project::new("Empty"), scan(&launcher, work.path()));
    open(&mut app, &path);
    step_until(&mut app, "missing or altered EQ rejected", |app| {
        matches!(
            app.engine().device_status(eq.id),
            DeviceStatus::Crashed(_) | DeviceStatus::Missing
        )
    });
    let host = app.engine().host();
    assert!(host.verified_bundled_kind(eq.id).is_none());
    assert!(host.bridge_pid(eq.id).is_none());
    assert_eq!(app.project().as_ref(), &saved);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    app.shutdown();
}
