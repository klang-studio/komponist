//! Current-format projects exercised against the twelve verified release bundles.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use komponist_app::actions::{BitDepth, EditAction, ExportRange, PluginAction, ProjectAction};
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::project_io::{load_project, save_project};
use komponist_app::{Action, App};
use komponist_builtin::{BuiltinKind, Internal};
use komponist_device_core::pinned_clap_state;
use komponist_engine::compile::{self, NoEnv};
use komponist_engine::testing::{Logged, TestSine};
use komponist_engine::{Command, Engine, EngineConfig, EventKind, Processor, TransportState};
use komponist_ipc::device_assets::{OfflineRequest, OfflineStatus};
use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
use komponist_model::edits::{self, NewClip, NewClipContent, NewDevice, NewNote, NewTrack};
use komponist_model::macros::Mapping;
use komponist_model::params::ParamId;
use komponist_model::plugin_assets::AssetLink;
use komponist_model::{
    AudioFile, ChannelRef, DeviceId, DeviceRole, PPQ, PluginFormat, PluginRef, Project, Sidechain,
    Tick, TickRange, TrackId,
};
use komponist_plugins::{DeviceStatus, FailReason, PluginDirs, PluginManagerEvent, ProcessorSetup};

use crate::common::{PATIENCE, app_with, config, devices, step_for, step_until};
use crate::export::{export_settings, read};

const DEVICE_PREFIX: &str = "org.klang-studio.komponist.";
const DELAY_PRESET: &str = "Slapback";

fn package_env() -> (PathBuf, PathBuf) {
    let required = |key| {
        PathBuf::from(std::env::var_os(key).expect("run the current bundled project test script"))
    };
    (
        required("KOMPONIST_CURRENT_BUNDLED_APP"),
        required("KOMPONIST_CURRENT_BUNDLED_RESULTS"),
    )
}

pub(super) fn scan(launcher: &Path, work: &Path) -> PluginHost {
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

pub(super) fn shipped(host: &PluginHost, slug: &str) -> PluginRef {
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

fn initial_device(
    host: &PluginHost,
    slug: &str,
    preset: Option<&str>,
    edits: &[(&str, f64)],
) -> komponist_plugins::bundled_initial_state::InitialDevice {
    host.manager()
        .prepare_bundled_device(&format!("{DEVICE_PREFIX}{slug}"), preset, edits, &[])
        .unwrap_or_else(|error| panic!("fixture {slug}: {error}"))
}

fn project(host: &PluginHost, sample: &Path) -> Fixture {
    let mut project = Project::new("Bundled CLAP project");
    let mut tracks = Vec::new();
    let sampler_key = 36; // The first Klopfer pad plays C2.
    for (index, (name, note)) in [
        ("hexa", 60),
        ("volt", 64),
        ("sampler", sampler_key),
        ("general-sampler", 67),
        // Last, so the note starts of the other tracks (and the sampler's stretch window) stay.
        ("surf", 62),
    ]
    .into_iter()
    .enumerate()
    {
        let changes: &[(&str, f64)] = if name == "general-sampler" {
            &[("sync", 1.0)]
        } else {
            &[]
        };
        let prepared = initial_device(host, name, None, changes);
        let (next, track) = edits::add_track(
            &project,
            NewTrack {
                name: Some(name.into()),
                hue: None,
                instrument: Some(prepared.plugin),
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
        let instrument = next.track(track).unwrap().channel.instrument().unwrap().1;
        project = edits::set_device_state(&next, instrument.id, prepared.state).unwrap();
        tracks.push(track);
    }

    let [hexa, volt, sampler, general, _surf] = tracks[..] else {
        unreachable!("fixture always creates five tracks");
    };
    let mut insert = |slug, track, index, preset| {
        let prepared = initial_device(host, slug, preset, &[]);
        let (next, device) = edits::insert_device(
            &project,
            ChannelRef::Track(track),
            index,
            NewDevice {
                role: prepared.role,
                plugin: prepared.plugin,
                sidechain: None,
                state: prepared.state,
            },
        )
        .unwrap();
        project = next;
        device
    };
    insert("arpeggiator", hexa, 0, None);
    let scale = insert("scale", hexa, 1, None);
    let eq = insert("eq", hexa, 3, None);
    insert("hall", hexa, 4, None);
    let delay = insert("delay", general, 1, Some(DELAY_PRESET));
    let presse = insert("presse", volt, 1, None);
    insert("limiter", volt, 2, None);
    project = edits::set_sidechain(&project, presse, Some(Sidechain::new(sampler))).unwrap();
    let link = AssetLink::new(sample, sample.parent());
    for track in [sampler, general] {
        let device = project
            .track(track)
            .unwrap()
            .channel
            .instrument()
            .unwrap()
            .1;
        let id = device.id;
        project = edits::set_device_asset_links(&project, id, [(0, link.clone())].into()).unwrap();
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

fn open(app: &mut App, path: &Path) {
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.to_owned())));
    assert_eq!(app.project_path(), Some(path));
}

fn assert_ready(app: &mut App, expected: &[(TrackId, komponist_model::Device)]) {
    step_until(app, "authenticated packaged CLAP instances", |app| {
        expected.iter().all(|(_, device)| {
            let host = app.engine().host();
            host.verified_bundled_kind(device.id).is_some()
                && host.native_layout(device.id).is_some()
        })
    });
    for (_, device) in expected {
        let host = app.engine().host();
        assert!(
            host.bridge_pid(device.id).is_none(),
            "{} is hosted in-process",
            device.plugin.id
        );
        assert!(
            host.state_warning(device.id).is_none(),
            "{}",
            device.plugin.id
        );
    }
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn demo_and_new_tracks_use_verified_clap_only() {
    let (launcher, _) = package_env();
    let work = tempfile::tempdir().unwrap();
    let mut launch = config(work.path());
    launch.bridge_exe = launcher.clone();
    // The CLI constructs the project before it starts the background catalog scan.
    let cold_host = PluginHost::new(launch);
    assert!(cold_host.catalog().entries().is_empty());
    let project =
        komponist_app::showcase::bundled_demo(&cold_host, &work.path().join("kit")).unwrap();
    let host = scan(&launcher, work.path());
    assert!(
        host.catalog()
            .entries()
            .iter()
            .all(|entry| !entry.is_builtin())
    );
    let expected = devices(&project);
    assert!(!expected.is_empty());
    assert!(
        expected
            .iter()
            .all(|(_, device)| device.plugin.format == PluginFormat::Clap)
    );
    let mut app = app_with(project.clone(), host);
    assert_ready(&mut app, &expected);

    let repeat =
        komponist_app::showcase::bundled_demo(&cold_host, &work.path().join("repeat-kit")).unwrap();
    super::showcase::assert_packaged_showcase(&mut app, &project, &cold_host, &repeat);

    let count = app.project().tracks.len();
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    assert_eq!(app.project().tracks.len(), count + 1);
    let track = app.project().tracks.last().unwrap();
    let (_, instrument) = track.channel.instrument().unwrap();
    assert_eq!(instrument.plugin.id, format!("{DEVICE_PREFIX}hexa"));
    let added = [(track.id, instrument.as_ref().clone())];
    assert_ready(&mut app, &added);
    app.shutdown();
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

fn export_packaged(app: &mut App, settings: komponist_app::actions::ExportSettings) {
    let file_name = settings.file_name.clone();
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    let deadline = Instant::now() + Duration::from_secs(20);
    while app.is_exporting() {
        app.step();
        assert!(
            Instant::now() < deadline,
            "{file_name}: {:?}; devices (name, bypass, status, preparation): {:?}",
            app.ui().dialog,
            devices(app.project())
                .iter()
                .map(|(_, device)| {
                    let host = app.engine().host();
                    (
                        &device.plugin.name,
                        device.bypass,
                        host.status(device.id),
                        host.offline_preparation_status(device.id),
                    )
                })
                .collect::<Vec<_>>()
        );
        thread::sleep(Duration::from_millis(2));
    }
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn current_project_round_trip_and_offline_render() {
    let (launcher, results) = package_env();
    let work = results.join("direct");
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
    assert_eq!(expected.len(), 12);
    save_project(&path, &authored).unwrap();
    let mut app = app_with(Project::new("Empty"), host);
    open(&mut app, &path);
    assert_ready(&mut app, &expected);
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
    assert_ready(&mut app, &expected);

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
    let state = pinned_clap_state::decode(delay.state.as_deref().unwrap()).unwrap();
    assert_eq!(
        state["fields"][pinned_clap_state::FACTORY_PRESET_FIELD].as_str(),
        Some(serde_json::to_string(DELAY_PRESET).unwrap().as_str()),
    );

    app.shutdown();
    let relocated = work.join("relocated");
    std::fs::rename(project_folder, &relocated).unwrap();
    assert!(!sample.exists());
    let mut app = app_with(Project::new("Empty"), scan(&launcher, cache.path()));
    open(&mut app, &relocated.join("bundled.komponist"));
    assert_eq!(app.project().as_ref(), &saved);
    assert_ready(&mut app, &expected);
    assert_eq!(
        app.engine().host().param_base(fixture.scale, scale_mode),
        Some(3.0)
    );
    for (track, key) in fixture.sampler_tracks {
        assert_audible(&mut app, track, key);
    }

    let sampler = app
        .project()
        .track(fixture.sampler_tracks[0].0)
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1;
    let (device, state) = (sampler.id, sampler.state.clone().unwrap());
    let detached = app.engine_mut().detach().unwrap();
    let request = Some(OfflineRequest {
        id: u32::MAX,
        sample_rate: 48_000.0,
        tempo: 120.0,
    });
    app.engine_mut()
        .host_mut()
        .set_offline_preparation(device, request);
    let ready = |app: &App| {
        app.engine().host().offline_preparation_status(device) == Some(OfflineStatus::Ready)
    };
    step_until(&mut app, "packaged sampler prepared", ready);
    assert!(app.engine_mut().host_mut().set_state(device, &state));
    assert_eq!(
        app.engine().host().offline_preparation_status(device),
        Some(OfflineStatus::Preparing)
    );
    step_until(&mut app, "packaged sampler prepared after restore", ready);
    assert!(app.engine().host().state_warning(device).is_none());
    app.engine_mut()
        .host_mut()
        .set_offline_preparation(device, None);
    app.engine_mut().attach(detached);

    let output = work.join("out/bundled.wav");
    let mut settings = export_settings(&work, ExportRange::Song);
    settings.file_name = "bundled.wav".into();
    settings.sample_rate = 48_000;
    settings.normalize = false;
    settings.tail_seconds = 0.0;
    export_packaged(&mut app, settings.clone());
    let wav = read(&output);
    assert!(wav.samples.iter().all(|sample| sample.is_finite()));
    assert!(wav.samples.iter().any(|sample| sample.abs() > 0.01));
    // The General Sampler renders deterministically and changes tempo immediately before
    // export. The effect is bypassed so a delay tail cannot satisfy the stretch check.
    app.dispatch(Action::Edit(EditAction::SetExclusive {
        track: fixture.sampler_tracks[1].0,
        flag: edits::ExclusiveFlag::Solo,
    }));
    app.dispatch(Action::Edit(EditAction::SetDeviceBypass {
        device: fixture.delay,
        bypass: true,
    }));
    let mut tempo = app.project().tempo;
    tempo.bpm = 60.0;
    app.dispatch(Action::Edit(EditAction::SetProjectProp(
        edits::ProjectProp::Tempo(tempo),
    )));
    settings.file_name = "sampler.wav".into();
    settings.dither = false;
    export_packaged(&mut app, settings);
    let output = work.join("out/sampler.wav");
    let wav = read(&output);
    assert!(wav.samples.iter().all(|value| value.is_finite()));
    // The note starts at 375 ms and the original recording lasts 250 ms.
    let stretched_tail = &wav.samples[2 * 34_080..2 * 37_920];
    assert!(stretched_tail.iter().any(|value| value.abs() > 0.01));
    app.shutdown();
}

/// Drive the production pre-instrument MIDI chain and log what reaches the instrument.
fn midi_chain_events(host: &mut dyn DeviceHost, plugins: [PluginRef; 2]) -> Vec<(u64, EventKind)> {
    const RATE: f32 = 48_000.0;
    const BLOCK: usize = 64;

    let mut project = Project::new("Packaged MIDI effects");
    project.tempo.bpm = 120.0;
    project.loop_range = TickRange::new(Tick(80), Tick(240));
    project.loop_on = true;
    let (mut project, track) = edits::add_track(
        &project,
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(crate::common::clap_plugin(crate::common::SYNTH)),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (next, _) = edits::add_clip(
        &project,
        track,
        NewClip {
            start: Tick::ZERO,
            length: Tick(PPQ),
            name: "Held C sharp".into(),
            content: NewClipContent::Midi {
                notes: vec![NewNote {
                    start: Tick(80),
                    length: Tick(480),
                    pitch: 61,
                    velocity: 100,
                }],
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    project = next;

    for (index, plugin) in plugins.into_iter().enumerate() {
        (project, _) = edits::insert_device(
            &project,
            ChannelRef::Track(track),
            index,
            NewDevice {
                role: DeviceRole::MidiEffect,
                plugin,
                state: None,
                sidechain: None,
            },
        )
        .unwrap();
    }
    let program = compile::build(&project, None, &NoEnv);
    let (mut engine, mut handle) = Engine::new(EngineConfig {
        sample_rate: RATE,
        max_block: BLOCK,
        seed: 1,
    });
    let setup = ProcessorSetup {
        sample_rate: RATE,
        max_block: BLOCK,
    };
    let (mut instrument, mut log) = TestSine::logged(512);
    instrument.activate(RATE, BLOCK).unwrap();
    let chain = &project.track(track).unwrap().channel.devices;
    let [arpeggiator, scale, instrument_model] = &chain[..] else {
        panic!("expected both MIDI effects before the instrument");
    };

    for device in [arpeggiator, scale] {
        let processor = host.create_processor(device, setup).unwrap();
        handle
            .send(Command::InsertProcessor {
                slot: program.slots.slot(device.id).unwrap(),
                processor,
            })
            .unwrap();
    }
    let deadline = Instant::now() + PATIENCE;
    while [arpeggiator, scale]
        .iter()
        .any(|device| host.status(device.id) != DeviceStatus::Ok)
    {
        assert!(
            Instant::now() < deadline,
            "MIDI processors did not activate"
        );
        host.poll();
        thread::sleep(Duration::from_millis(2));
    }
    for device in [arpeggiator, scale] {
        assert!(host.bridge_pid(device.id).is_none());
        assert!(host.verified_bundled_kind(device.id).is_some());
    }
    let rate_id = host
        .native_layout(arpeggiator.id)
        .unwrap()
        .author_ids()
        .unwrap()["rate"];
    handle
        .send(Command::InsertProcessor {
            slot: program.slots.slot(instrument_model.id).unwrap(),
            processor: Box::new(instrument),
        })
        .unwrap();
    let arp_slot = program.slots.slot(arpeggiator.id).unwrap();
    handle.send(Command::SetProgram(Box::new(program))).unwrap();
    handle
        .send(Command::SetParam {
            slot: arp_slot,
            id: rate_id,
            value: 7.0,
        })
        .unwrap();
    handle.send(Command::Locate(Tick(80))).unwrap();
    handle.send(Command::Play).unwrap();
    // The loop wraps at 4,000 samples. Visit both sides of that boundary.
    for _ in 0..80 {
        assert_no_alloc::assert_no_alloc(|| {
            engine.render(BLOCK);
        });
    }
    handle.send(Command::Stop).unwrap();
    assert_no_alloc::assert_no_alloc(|| {
        engine.render(BLOCK);
    });
    assert_eq!(log.lost(), 0);
    log.drain()
        .into_iter()
        .filter_map(|entry| match entry {
            Logged::Event {
                frame,
                kind: kind @ (EventKind::NoteOn { .. } | EventKind::NoteOff { .. }),
            } => Some((frame, kind)),
            _ => None,
        })
        .collect()
}

fn assert_midi_effect_chain(host: &mut PluginHost) {
    let plugins = [shipped(host, "arpeggiator"), shipped(host, "scale")];
    let actual = midi_chain_events(host, plugins);
    // Sixteenth-note rate at 120 BPM. The second loop starts at frame 4000,
    // and stopping at frame 5120 must release the third note immediately.
    let expected: Vec<_> = [(0, 1200), (2000, 3200), (4000, 5120)]
        .into_iter()
        .enumerate()
        .flat_map(|(index, (start, end))| {
            let note_id = index as u32;
            [
                (
                    start,
                    EventKind::NoteOn {
                        key: 60,
                        channel: 0,
                        velocity: 100,
                        note_id,
                    },
                ),
                (
                    end,
                    EventKind::NoteOff {
                        key: 60,
                        channel: 0,
                        note_id,
                    },
                ),
            ]
        })
        .collect();
    assert_eq!(actual, expected, "notes and identities at the instrument");
}

#[test]
fn fixture_midi_effects_preserve_instrument_notes() {
    let mut host = App::headless_plugin_host();
    if host.catalog().entries().is_empty() {
        eprintln!("skipping: build the verified CLAP fixtures with scripts/build-test-plugins.sh");
        return;
    }
    assert_midi_effect_chain(&mut host);
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn packaged_midi_effects_preserve_instrument_notes() {
    let (launcher, _) = package_env();
    let cache = tempfile::tempdir().unwrap();
    let mut host = scan(&launcher, cache.path());
    assert_midi_effect_chain(&mut host);
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn bundled_presse_lookahead_updates_delay_compensation_during_playback() {
    let (launcher, _) = package_env();
    let cache = tempfile::tempdir().unwrap();
    let host = scan(&launcher, cache.path());
    let mut project = Project::new("Bundled Presse latency");
    project.loop_range = TickRange::new(Tick::ZERO, Tick(PPQ * 4));
    project.loop_on = true;
    let [with_presse, plain] = ["Presse", "Plain"].map(|name| {
        let (next, track) = edits::add_track(
            &project,
            NewTrack {
                name: Some(name.into()),
                hue: None,
                instrument: Some(shipped(&host, "hexa")),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        (project, _) = edits::add_clip(
            &next,
            track,
            NewClip {
                start: Tick::ZERO,
                length: Tick(PPQ * 4),
                name: name.into(),
                content: NewClipContent::Midi {
                    // A note at transport start can coincide with activation or a control poll.
                    // Keep delivering notes so the fixture can observe active processing.
                    notes: (0..4)
                        .map(|beat| NewNote {
                            start: Tick(PPQ * beat),
                            length: Tick(PPQ / 2),
                            pitch: 60,
                            velocity: 76,
                        })
                        .collect(),
                    take_name: String::new(),
                },
            },
        )
        .unwrap();
        track
    });
    let (project, presse) = edits::insert_device(
        &project,
        ChannelRef::Track(with_presse),
        1,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: shipped(&host, "presse"),
            sidechain: None,
            state: None,
        },
    )
    .unwrap();
    let expected = devices(&project);
    let mut app = app_with(project, host);
    assert_ready(&mut app, &expected);
    step_until(&mut app, "Presse running", |app| {
        app.engine().device_status(presse) == DeviceStatus::Ok
    });
    let lookahead = app
        .engine()
        .host()
        .native_layout(presse)
        .unwrap()
        .author_ids()
        .unwrap()["lookahead"];
    let pdc = |app: &App, track: TrackId| {
        app.engine()
            .program()
            .unwrap()
            .tracks
            .iter()
            .find(|candidate| candidate.id == track)
            .unwrap()
            .pdc_delay
    };
    app.dispatch(Action::Transport(
        komponist_app::TransportAction::TogglePlay,
    ));
    eprintln!("Waiting for audio on the Presse track ");
    step_until(&mut app, "Presse track audible during playback", |app| {
        let (left, right) = app.engine().track_meter(with_presse).unwrap().take();
        app.transport() == TransportState::Playing && left.max(right) > 1e-4
    });
    assert_eq!(app.engine().host().latency(presse), 0);
    assert_eq!(pdc(&app, plain), 0);

    let lookahead_frames = (f64::from(app.engine().sample_rate()) * komponist_presse::LOOKAHEAD_MS
        / 1000.0)
        .round() as u32;
    for (value, delay) in [(1.0, lookahead_frames), (0.0, 0)] {
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device: presse,
            id: lookahead,
            value,
        }));
        eprintln!("Waiting for Presse lookahead={value} and track delay={delay} ");
        step_until(
            &mut app,
            "Presse reactivated with matching track delay",
            |app| {
                app.engine().device_status(presse) == DeviceStatus::Ok
                    && app.engine().host().latency(presse) == delay
                    && pdc(app, plain) == delay
            },
        );
        assert_eq!(pdc(&app, with_presse), 0);
        assert_eq!(app.transport(), TransportState::Playing);
    }
    app.shutdown();
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn saves_manual_eq_edit_during_automation_and_renders_the_reopened_curve() {
    const RATE: u32 = 48_000;
    const BASE_FREQUENCY: f64 = 1_000.0;
    const HIGH_SHELF: f64 = 3.0;
    const MACRO_BASE: f32 = 0.75;
    const MACRO_AUTOMATION: f32 = 0.58;
    let (launcher, _) = package_env();
    let work = tempfile::tempdir().unwrap();
    let host = scan(&launcher, work.path());
    let (project, track) = edits::add_audio_track(
        &Project::new("EQ automation and manual edit"),
        edits::NewAudioTrack::default(),
    )
    .unwrap();
    let sample = work.path().join("tone.wav");
    let mut tone = hound::WavWriter::create(
        &sample,
        hound::WavSpec {
            channels: 1,
            sample_rate: RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .unwrap();
    for frame in 0..RATE * 4 {
        let phase = std::f64::consts::TAU * f64::from(frame % 48) / 48.0;
        tone.write_sample((4_096.0 * phase.sin()) as i16).unwrap();
    }
    tone.finalize().unwrap();
    let (project, _) = edits::add_clip(
        &project,
        track,
        NewClip {
            start: Tick::ZERO,
            length: Tick(8 * PPQ),
            name: "1 kHz tone".into(),
            content: NewClipContent::Audio {
                file: AudioFile::new(&sample, Some(work.path()), u64::from(RATE) * 4, RATE, 1),
                offset: 0,
            },
        },
    )
    .unwrap();
    // The bundled EQ's frequency control exposes a normalized CLAP value.
    let base_wire = (BASE_FREQUENCY / 20.0).ln() / 1000.0_f64.ln();
    let prepared = initial_device(&host, "eq", None, &[("b1_freq", base_wire)]);
    let (project, eq) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: prepared.plugin,
            sidechain: None,
            state: prepared.state,
        },
    )
    .unwrap();
    let path = work.path().join("automated.komponist");
    save_project(&path, &project).unwrap();
    let expected = devices(&project);
    let mut app = app_with(Project::new("Empty"), host);
    open(&mut app, &path);
    assert_ready(&mut app, &expected);
    let ids = app
        .engine()
        .host()
        .native_layout(eq)
        .unwrap()
        .author_ids()
        .unwrap();
    let (frequency, gain, shape) = (ids["b1_freq"], ids["b1_gain"], ids["b1_type"]);
    let owner = AutomationOwner::Track(track);
    let (project, lane) = edits::automation::add_lane(
        &project,
        owner,
        AutomationTarget::Param {
            device: eq,
            param: ParamId::plugin(frequency),
        },
        "Shelf frequency".into(),
    )
    .unwrap();
    let points = Points::new(vec![
        Point {
            tick: Tick::ZERO,
            value: 0.22,
            shape: Shape::Hold,
        },
        Point {
            tick: Tick(2 * PPQ),
            value: 0.9,
            shape: Shape::Hold,
        },
        Point::linear(Tick(4 * PPQ), 0.22),
    ]);
    let project = edits::automation::set_lane_points(&project, owner, lane, points).unwrap();
    let gain_mapping = Mapping {
        target: AutomationTarget::Param {
            device: eq,
            param: ParamId::plugin(gain),
        },
        label: "Shelf gain".into(),
        min: 0.65,
        max: 0.8,
        invert: false,
        bend: 0.25,
    };
    let gain_base = f64::from(gain_mapping.value_at(MACRO_BASE));
    let project = edits::macros::add_mapping(&project, track, 0, gain_mapping).unwrap();
    let project = edits::macros::set_macro_value(&project, track, 0, MACRO_BASE).unwrap();
    let (project, macro_lane) = edits::automation::add_lane(
        &project,
        owner,
        AutomationTarget::Macro { index: 0 },
        "Gain macro".into(),
    )
    .unwrap();
    let project = edits::automation::set_lane_points(
        &project,
        owner,
        macro_lane,
        Points::new(vec![
            Point {
                tick: Tick::ZERO,
                value: MACRO_AUTOMATION,
                shape: Shape::Hold,
            },
            Point::linear(Tick(6 * PPQ), 0.05),
        ]),
    )
    .unwrap();
    save_project(&path, &project).unwrap();
    open(&mut app, &path);
    assert_ready(&mut app, &expected);
    assert_eq!(app.engine().host().param_base(eq, shape), Some(2.0));
    app.engine_mut().send(Command::Play);
    step_until(&mut app, "active EQ automation", |app| {
        app.engine().transport() == TransportState::Playing
            && app.engine().playhead() >= Tick(PPQ / 2)
            && app
                .engine()
                .telemetry()
                .macro_values(track)
                .is_some_and(|values| (values[0] - MACRO_AUTOMATION).abs() < 1e-6)
    });
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: eq,
        id: shape,
        value: HIGH_SHELF,
    }));
    app.dispatch(Action::Project(ProjectAction::Save));

    let saved = load_project(&path).unwrap();
    let saved_device = saved.find_device(eq).unwrap().2;
    assert_ne!(
        saved_device.state,
        project.find_device(eq).unwrap().2.state,
        "saving with an automated parameter and a pending edit must update the file",
    );
    let state = pinned_clap_state::decode(saved_device.state.as_deref().unwrap()).unwrap();
    let saved_base = state["params"]["b1_freq"]["f32"].as_f64().unwrap();
    assert!(
        (saved_base - base_wire).abs() < 1e-6,
        "saved automation sample as the EQ base value: {saved_base}"
    );
    let saved_gain = state["params"]["b1_gain"]["f32"].as_f64().unwrap();
    assert!(
        (saved_gain - gain_base).abs() < 1e-6,
        "saved automated macro output as the EQ gain base value: {saved_gain}"
    );
    let authored_track = project.track(track).unwrap();
    let saved_track = saved.track(track).unwrap();
    assert_eq!(saved_track.automation, authored_track.automation);
    assert_eq!(saved_track.macros, authored_track.macros);

    app.shutdown();
    let mut app = app_with(Project::new("Empty"), scan(&launcher, work.path()));
    open(&mut app, &path);
    assert_eq!(app.project().as_ref(), &saved);
    assert_ready(&mut app, &expected);
    let host = app.engine().host();
    assert_eq!(host.param_base(eq, shape), Some(HIGH_SHELF));
    let reopened_base = host.param_base(eq, frequency).unwrap();
    assert!((reopened_base - base_wire).abs() < 1e-6);
    let reopened_gain = host.param_base(eq, gain).unwrap();
    assert!((reopened_gain - gain_base).abs() < 1e-6);

    let frequency_target = AutomationTarget::Param {
        device: eq,
        param: ParamId::plugin(frequency),
    };
    assert!(
        app.engine()
            .program()
            .is_some_and(|program| program.automation_reads(owner, &frequency_target)),
        "the reopened EQ lane must be compiled as soon as the device is ready for export",
    );

    let mut settings = export_settings(work.path(), ExportRange::Song);
    settings.file_name = "automated-eq.wav".into();
    settings.sample_rate = RATE;
    settings.bit_depth = BitDepth::Float32;
    settings.normalize = false;
    settings.tail_seconds = 0.0;
    let output = settings.folder.join(&settings.file_name);
    export_packaged(&mut app, settings);
    let wav = read(&output);
    let channels = usize::from(wav.spec.channels);
    let rms = |first: usize, last: usize| {
        let window = &wav.samples[first * channels..last * channels];
        (window.iter().map(|value| value * value).sum::<f32>() / window.len() as f32).sqrt()
    };
    // The tone is unchanged. The shelf boosts it before the automation step at 1 s,
    // then leaves it alone once the cutoff is above 1 kHz. Both windows avoid the glide.
    let boosted = rms(12_000, 36_000);
    let unboosted = rms(60_000, 84_000);
    assert!(
        unboosted > 0.03 && boosted > 2.0 * unboosted,
        "reopened EQ automation did not change the rendered tone: {boosted} vs {unboosted}"
    );
    // The shelf is back below the tone in both windows. The macro lowers the gain at 3 s.
    let before_gain_step = rms(108_000, 132_000);
    let after_gain_step = rms(156_000, 180_000);
    assert!(
        before_gain_step > 1.3 * after_gain_step && after_gain_step > 0.03,
        "reopened macro automation did not lower the rendered shelf gain: {before_gain_step} vs {after_gain_step}"
    );
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
    let status = app.engine().device_status(eq.id);
    match std::env::var("KOMPONIST_CURRENT_EQ_CASE").as_deref() {
        Ok("missing") => assert_eq!(status, DeviceStatus::Missing),
        Ok("altered") => assert!(
            matches!(&status, DeviceStatus::Crashed(FailReason::LoadFailed(reason))
                if reason.contains("bundle file hash mismatch")),
            "expected the EQ verifier to identify the modified file, got {status:?}"
        ),
        _ => panic!("expected missing or altered EQ case"),
    }
    let host = app.engine().host();
    assert!(host.verified_bundled_kind(eq.id).is_none());
    assert!(host.bridge_pid(eq.id).is_none());
    assert_eq!(app.project().as_ref(), &saved);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    app.shutdown();
}

#[test]
#[ignore = "requires the authenticated release payload; run measure-bundled-clap.py --current-projects"]
fn packaged_effect_cards_and_presets() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;
    use komponist_app::actions::{SelectMode, SelectionChange, UiAction};
    use komponist_app::ui_state::DockTab;
    use komponist_i18n::Lang;

    let (launcher, _) = package_env();
    let work = tempfile::tempdir().unwrap();
    let host = scan(&launcher, work.path());
    let lang = match std::env::var("KOMPONIST_SYNTH_UI_LANG").as_deref() {
        Ok("en") => Lang::En,
        _ => Lang::De,
    };
    let mut project = Project::new("Bundled CLAP effect cards");
    let mut cards = Vec::new();
    for slug in ["eq", "presse", "limiter", "delay"] {
        let (next, track) = edits::add_track(
            &project,
            NewTrack {
                name: Some(slug.into()),
                hue: None,
                instrument: Some(shipped(&host, "volt")),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let (next, device) = edits::insert_device(
            &next,
            ChannelRef::Track(track),
            1,
            NewDevice {
                role: DeviceRole::Effect,
                plugin: shipped(&host, slug),
                sidechain: None,
                state: None,
            },
        )
        .unwrap();
        project = next;
        cards.push((slug, track, device));
    }
    let expected = devices(&project);
    let mut app = app_with(project, host).with_lang(lang);
    assert_ready(&mut app, &expected);
    let mut harness = crate::acceptance_support::harness(app);
    for (slug, track, device) in cards {
        let app = harness.state_mut();
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
            tracks: vec![track],
            mode: SelectMode::Replace,
        })));
        app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
        app.dispatch(Action::Ui(UiAction::RevealDevice(device)));
        if slug == "eq" {
            let preset = "Low Cut 80 Hz";
            let target = app.project().find_device(device).unwrap().2;
            let source = app
                .engine()
                .host()
                .clap_presets(target)
                .iter()
                .find(|entry| entry.name == preset)
                .unwrap()
                .source
                .clone();
            app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
                device,
                source,
            }));
            step_until(app, "bundled EQ preset applied", |app| {
                app.device_events().iter().any(|event| {
                    matches!(event,
                    PluginManagerEvent::ClapPresetLoaded { device: id, .. } if *id == device)
                })
            });
            harness.run_steps(3);
            harness.get_by_role_and_label(Role::Button, &lang.devices_preset_name(preset));
            harness.get_by_role_and_label(Role::Group, lang.ui_graph_eq());
        } else if slug == "delay" {
            let layout = app.engine().host().native_layout(device).unwrap();

            let time = layout.author_ids().unwrap()["time"];
            assert_eq!(
                layout.parameter_display(time).unwrap().range(),
                (1.0, 2000.0)
            );
            harness.run_steps(3);
            harness.get_by_role_and_label(Role::Group, lang.ui_graph_xy());
            harness.get_by_role_and_label(Role::Button, lang.param_delay_ping_pong());
            harness.get_by_role_and_label(Role::ComboBox, lang.param_delay_note());
            let axis = format!("{} {}", lang.ui_graph_xy(), lang.param_time());
            let slider = harness.get_by_role_and_label(Role::Slider, &axis);
            assert_eq!(slider.value().as_deref(), Some("375 ms"));
            slider.increment_accesskit();
            harness.run_steps(3);
            assert!(harness.state().dispatched().any(|action| matches!(
                action,
                Action::Plugin(PluginAction::SetParam { device: id, id: wire, value })
                    if *id == device && *wire == time && (0.0..=1.0).contains(value)
            )));
        } else {
            harness.run_steps(3);
            harness.get_by_role_and_label(Role::Meter, lang.ui_meter_gr_name());
        }
        if let Some(dir) = std::env::var_os("KOMPONIST_EFFECT_UI_PNG_DIR") {
            let path = PathBuf::from(dir).join(format!("{slug}-{}.png", lang.code()));
            harness.render().unwrap().save(path).unwrap();
        }
        if slug == "eq" {
            check_eq_panel(&mut harness, track, device, lang);
        }
        if slug == "presse" {
            check_presse_panel(&mut harness, track, device, lang);
        }
    }
    harness.state_mut().shutdown();
}

fn check_eq_panel(
    harness: &mut egui_kittest::Harness<'_, App>,
    track: TrackId,
    device: DeviceId,
    lang: komponist_i18n::Lang,
) {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;

    let host = harness.state().engine().host();
    let layout = host.native_layout(device).unwrap();
    let ids = layout.author_ids().unwrap();
    let freq = ids["b2_freq"];
    let kind = ids["b2_type"];
    let analyse = ids["analyse"];
    let spectrum = host
        .signal_tap(device, "spectrum")
        .unwrap()
        .samples()
        .clone();

    harness.get_by_role_and_label(Role::Slider, &lang.param_eq_band_freq(1));
    let axis = format!("{} {}", lang.ui_graph_band(2), lang.ui_graph_frequency());
    let handle = harness.get_by_role_and_label(Role::Slider, &axis);
    let pos = handle.rect().center();
    harness.hover_at(pos);
    harness.step();
    for pressed in [true, false] {
        harness.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        harness.step();
    }
    harness.run_steps(3);
    assert!(
        harness
            .query_by_role_and_label(Role::Slider, &lang.param_eq_band_freq(1))
            .is_none()
    );
    harness.get_by_role_and_label(Role::Slider, &lang.param_eq_band_freq(2));
    harness
        .get_by_role_and_label(Role::Slider, &axis)
        .increment_accesskit();
    let low_cut = format!(
        "{}: {}",
        lang.param_eq_band_type(2),
        lang.choice_eq_type_low_cut_short()
    );
    harness
        .get_by_role_and_label(Role::RadioButton, &low_cut)
        .click();
    harness.run_steps(3);
    assert!(harness.state().dispatched().any(|action| matches!(
        action,
        Action::Plugin(PluginAction::SetParam { device: id, id: wire, value })
            if *id == device && *wire == freq && (0.0..=1.0).contains(value)
    )));
    assert!(harness.state().dispatched().any(|action| matches!(
        action,
        Action::Plugin(PluginAction::SetParam { device: id, id: wire, value })
            if *id == device && *wire == kind && *value == 0.0
    )));

    harness.state_mut().engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 120,
    });
    let deadline = Instant::now() + PATIENCE;
    loop {
        harness.run_steps(2);
        let mut samples = [0.0; 32];
        if spectrum.read_latest(&mut samples).is_some()
            && samples.into_iter().any(|sample| sample.abs() > 0.0001)
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the EQ graph must lease its live spectrum"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    harness
        .get_by_role_and_label(Role::Button, lang.param_eq_analyse())
        .click();
    harness.run_steps(3);
    assert!(harness.state().dispatched().any(|action| matches!(
        action,
        Action::Plugin(PluginAction::SetParam { device: id, id: wire, value })
            if *id == device && *wire == analyse && *value == 0.0
    )));
    let deadline = Instant::now() + PATIENCE;
    while spectrum.is_wanted() {
        harness.run_steps(2);
        assert!(
            Instant::now() < deadline,
            "Analyse off must stop renewing the spectrum lease"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.state_mut().engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 0,
    });
}

fn check_presse_panel(
    harness: &mut egui_kittest::Harness<'_, App>,
    track: TrackId,
    device: DeviceId,
    lang: komponist_i18n::Lang,
) {
    use egui::accesskit::Role;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};

    let host = harness.state().engine().host();
    let layout = host.native_layout(device).unwrap();
    let threshold = layout.author_ids().unwrap()["threshold"];
    let detector = host
        .signal_tap(device, "detector")
        .unwrap()
        .samples()
        .clone();
    let reduction = host
        .signal_tap(device, "reduction")
        .unwrap()
        .samples()
        .clone();

    harness.get_by_role_and_label(Role::Group, lang.ui_graph_transfer());
    harness.get_by_role_and_label(Role::Group, lang.ui_graph_gr_history());
    for (label, value) in [
        (lang.param_presse_knee(), "6 dB"),
        (lang.param_ratio(), "4.0:1"),
        (lang.param_threshold(), "−18.0 dB"),
    ] {
        assert_eq!(
            harness
                .get_by_role_and_label(Role::Slider, label)
                .value()
                .as_deref(),
            Some(value),
        );
    }
    let makeup_disabled = |h: &egui_kittest::Harness<'_, App>| {
        h.get_by_role_and_label(Role::Slider, lang.param_makeup())
            .accesskit_node()
            .is_disabled()
    };
    for expected in [true, false] {
        assert_ne!(makeup_disabled(harness), expected);
        harness
            .get_by_role_and_label(Role::Button, lang.param_presse_auto_makeup())
            .click();
        harness.run_steps(3);
        assert_eq!(makeup_disabled(harness), expected);
    }
    let axis = format!("{} {}", lang.ui_graph_knee(), lang.ui_graph_threshold());
    harness
        .get_by_role_and_label(Role::Slider, &axis)
        .increment_accesskit();
    harness.run_steps(3);
    assert!(harness.state().dispatched().any(|action| matches!(
        action,
        Action::Plugin(PluginAction::SetParam { device: id, id: wire, value })
            if *id == device && *wire == threshold && (0.0..=1.0).contains(value)
    )));

    let app = harness.state_mut();
    assert!(
        app.engine_mut()
            .host_mut()
            .set_param(device, threshold, 0.0)
    );
    app.engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 120,
    });
    let live = |tap: &komponist_engine::SampleTap, above| {
        let mut samples = [0.0; 32];
        tap.read_latest(&mut samples).is_some() && samples.into_iter().any(|sample| sample > above)
    };
    let deadline = Instant::now() + PATIENCE;
    loop {
        harness.run_steps(2);
        if live(&detector, -60.0) && live(&reduction, 0.1) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Presse's visible graphs must lease live detector and reduction taps"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    if let Some(dir) = std::env::var_os("KOMPONIST_EFFECT_UI_PNG_DIR") {
        let path = PathBuf::from(dir).join(format!("presse-live-{}.png", lang.code()));
        harness.render().unwrap().save(path).unwrap();
    }
    harness.state_mut().engine_mut().send(Command::LiveNote {
        track,
        key: 60,
        velocity: 0,
    });
}
