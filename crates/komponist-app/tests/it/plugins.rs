//! The plugin manager in the app (PLAN WP-46 "Done when"): the demo project with the WP-27 test
//! plugins in place of its external devices sounds through bridges; a crashing plugin shows
//! `Crashed` and its track plays on; state snapshots reach the document outside the history and
//! the saved file. App exit and drop: `tests/it/plugin_exit.rs`.
//!
//! The test plugins come from `scripts/build-test-plugins.sh`; without them the tests skip. Every
//! `Engine::render` call here runs inside `assert_no_alloc`; the app's audio callback does so in
//! `komponist-audio` (debug builds).

use crate::common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;
use common::*;
use komponist_app::actions::{PluginAction, ProjectAction, TransportAction};
use komponist_app::device_host::{DeviceHost, PluginHost, is_hexa};
use komponist_app::ui_state::StatusKind;
use komponist_app::{Action, App};
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::{Command, Engine, EngineConfig, MAX_BLOCK, MidiPort, Program};
use komponist_model::{DeviceId, DeviceRole, Project, Tick, TrackId, persist};
use komponist_plugins::{DeviceStatus, FailReason, PluginDirs, PluginManagerEvent, ProcessorSetup};

const RATE: f32 = 48_000.0;

/// Frames per `Engine::render` call.
const RENDER_BLOCK: usize = 1024;

/// Bar 5 (index 4) of the demo: Drums, Bass, Pad and Keys play there (Lead starts at bar 9).
fn bar_5() -> Tick {
    Tick::from_steps(64)
}

fn track_named(project: &Project, name: &str) -> TrackId {
    project.tracks.iter().find(|t| t.name == name).unwrap().id
}

/// Device latencies from the device host, as `EngineSync` passes them.
struct Env<'a>(&'a dyn DeviceHost);

impl CompileEnv for Env<'_> {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.0.latency(device)
    }
}

/// Polls `host` until `done` accepts one of its events.
fn poll_until(
    host: &mut PluginHost,
    what: &str,
    mut done: impl FnMut(&PluginManagerEvent) -> bool,
) {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        for event in host.poll() {
            if done(&event) {
                return;
            }
            seen.push(event);
        }
        assert!(Instant::now() < deadline, "no {what}; seen {seen:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_demo_with_test_plugins_sounds_through_bridges_in_an_offline_render() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("offline");
    let mut host = PluginHost::new(config(&dir));
    host.start_scan(&dirs);
    poll_until(&mut host, "ScanFinished", |e| {
        matches!(e, PluginManagerEvent::ScanFinished(_))
    });
    let project = demo_with_test_plugins();

    // What `EngineSync::send_project` does, with an engine of our own that renders offline.
    let (mut engine, mut handle) = Engine::new(EngineConfig {
        sample_rate: RATE,
        max_block: MAX_BLOCK,
        seed: 1,
    });
    let setup = ProcessorSetup {
        sample_rate: RATE,
        max_block: MAX_BLOCK,
    };
    let program = compile::build(&project, None, &Env(&host));
    for (device, slot) in compile::slot_changes(None, &program).added {
        let (_, _, model) = project.find_device(device).unwrap();
        let processor = host.create_processor(model, setup);
        let processor = processor.unwrap_or_else(|| panic!("{}", model.plugin.name));
        send(&mut handle, Command::InsertProcessor { slot, processor });
    }
    send(&mut handle, Command::SetProgram(Box::new(program.clone())));
    let bridged = bridged(&project);
    assert_eq!(bridged.len(), 13, "all 13 devices use the bridge");
    for &device in &bridged {
        assert_eq!(host.status(device), DeviceStatus::Loading);
    }
    let deadline = Instant::now() + PATIENCE;
    while !bridged.iter().all(|&d| host.status(d) == DeviceStatus::Ok) {
        assert!(Instant::now() < deadline, "bridges did not load");
        host.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    // kp-test-fx delays by 64 frames: the compiler sees the bridges' latencies.
    let effect = project.tracks[0].channel.devices[1].id;
    assert_eq!(host.latency(effect), 64);
    let previous = program;
    let program: Program = compile::build(&project, Some(&previous), &Env(&host));
    let changes = compile::slot_changes(Some(&previous), &program);
    assert!(changes.added.is_empty() && changes.removed.is_empty());
    let keys = track_named(&project, "Keys");
    let keys_pdc = program
        .tracks
        .iter()
        .find(|t| t.id == keys)
        .unwrap()
        .pdc_delay;
    // Lead is the longest chain: an instrument and two effects (the third is bypassed).
    let instrument = project.tracks[2].channel.devices[0].id;
    let lead = host.latency(instrument) + 2 * 64;
    assert_eq!(keys_pdc, lead, "Keys waits for Lead");
    send(&mut handle, Command::SetProgram(Box::new(program.clone())));

    // Two bars from bar 5, offline (no deadline: every block reaches every bridge).
    send(&mut handle, Command::Locate(bar_5()));
    send(&mut handle, Command::Play);
    let frames = (2.0 * 4.0 * 60.0 / project.tempo.bpm * f64::from(RATE)) as usize;
    let mut master_peak = 0.0f32;
    let mut done = 0;
    while done < frames {
        let n = RENDER_BLOCK.min(frames - done);
        let engine = &mut engine;
        let peak = assert_no_alloc(|| engine.render(n).iter().fold(0.0f32, |m, s| m.max(s.abs())));
        master_peak = master_peak.max(peak);
        done += n;
        // Garbage (replaced programs) is dropped here, outside `assert_no_alloc`.
        while handle.poll().is_some() {}
        host.poll();
    }
    assert!(master_peak > 0.05, "master {master_peak}");
    // Instruments and effects all play through their bridges.
    for name in ["Drums", "Bass", "Pad", "Keys"] {
        let (l, r) = program.meter(track_named(&project, name)).unwrap().take();
        eprintln!("{name}: peak {l:.3} / {r:.3}");
        assert!(l.max(r) > 0.05, "{name} is silent");
    }
    for &device in &bridged {
        assert_eq!(host.status(device), DeviceStatus::Ok);
    }
    host.shutdown();
}

fn send(handle: &mut komponist_engine::EngineHandle, command: Command) {
    assert!(handle.send(command).is_ok(), "command queue full");
}

/// The demo with test plugins, but kp-test-crash as the Drums track's effect.
fn demo_with_crash() -> (Project, DeviceId) {
    let mut project = demo_with_test_plugins();
    let drums = Arc::make_mut(&mut project.tracks[0]);
    let effect = Arc::make_mut(&mut drums.channel.devices[1]);
    assert_eq!(effect.role, DeviceRole::Effect);
    effect.plugin = clap_plugin(CRASH);
    let id = effect.id;
    (project, id)
}

/// Peak of `track`'s meter over `time` of playback.
fn level(app: &mut App, track: TrackId, time: Duration) -> f32 {
    app.engine().track_meter(track).unwrap().take();
    step_for(app, time);
    let (l, r) = app.engine().track_meter(track).unwrap().take();
    l.max(r)
}

#[test]
fn the_app_loads_scanned_plugins_plays_them_and_survives_a_crash() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("app");
    let (project, crash) = demo_with_crash();
    let file = dir.join("Night Drive.komponist");
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();
    let mut app = app_with(
        komponist_model::fixtures::empty_project(),
        PluginHost::new(config(&dir)),
    );
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    assert_eq!(app.project_path(), Some(file.as_path()));

    // An empty scan cache: every bridged device is missing until the scan finds its plugin.
    app.step();
    let bridged = bridged(app.project());
    for (_, name, status) in statuses(&app) {
        assert_eq!(status, DeviceStatus::Missing, "{name}");
    }
    app.engine_mut().host_mut().start_scan(&dirs);
    let events = step_until(&mut app, "every device Ok", all_ok);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PluginManagerEvent::ScanFinished(_)))
    );
    let loading = |d: &DeviceId| {
        events.contains(&PluginManagerEvent::StatusChanged {
            device: *d,
            status: DeviceStatus::Loading,
        })
    };
    assert!(bridged.iter().all(loading), "{events:?}");
    assert!(app.engine().host().needs_polling());
    assert_eq!(app.engine().host().catalog().entries().len(), 5);

    // Latencies recompile the program. Lead is the longest chain (the synth and two 64-frame
    // effects; the third is bypassed), Pad has two effects after a synth without latency, Keys
    // none.
    let pdc = |app: &App, name: &str| {
        let track = track_named(app.project(), name);
        let program = app.engine().program().unwrap();
        program
            .tracks
            .iter()
            .find(|t| t.id == track)
            .unwrap()
            .pdc_delay
    };
    let instrument = app.project().tracks[2].channel.devices[0].id;
    let synth = app.engine().host().latency(instrument);
    step_until(&mut app, "delay compensation", |app| {
        pdc(app, "Keys") == synth + 128
    });
    assert_eq!(pdc(&app, "Pad"), synth);
    assert_eq!(pdc(&app, "Lead"), 0);

    // Playback through the null backend: realtime blocks with deadlines, so a loaded machine may
    // make single blocks late (silent); one block that gets through is enough here.
    app.dispatch(Action::Transport(TransportAction::Locate(bar_5())));
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    let drums = track_named(app.project(), "Drums");
    let drums_level = level(&mut app, drums, Duration::from_millis(1500));
    let late: Vec<(String, u64)> = statuses(&app)
        .into_iter()
        .map(|(id, name, _)| (name, app.engine().host().late_blocks(id)))
        .filter(|(_, late)| *late > 0)
        .collect();
    eprintln!("late blocks: {late:?}");
    assert!(drums_level > 0.05, "Drums {drums_level}");
    for name in ["Pad", "Keys"] {
        let track = track_named(app.project(), name);
        let (l, r) = app.engine().track_meter(track).unwrap().take();
        assert!(l.max(r) > 0.01, "{name} is silent; late blocks {late:?}");
    }

    // Parameters of a bridged device go to its bridge: "Crash" past 0.9 crashes kp-test-crash
    // in `process` (SIGSEGV).
    let params = app.engine().host().params(crash).expect("params");
    let crash_param = params.iter().find(|p| p.name == "Crash").unwrap().id;
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: crash,
        id: crash_param,
        value: 1.0,
    }));
    // SIGSEGV; on Windows the access violation `0xC000_0005`, which komponist-ipc reports as the
    // signal (an NTSTATUS crash).
    let segv = if cfg!(windows) {
        0xC000_0005_u32 as i32
    } else {
        11
    };
    let crashed = DeviceStatus::Crashed(FailReason::Exited {
        code: None,
        signal: Some(segv),
    });
    step_until(&mut app, "the crash", |app| {
        app.engine().device_status(crash) == crashed
    });
    let status = app.ui().status.clone().expect("status bar message");
    assert_eq!(
        status.text,
        "kp-test-crash abgestürzt · die Spur läuft ohne dieses Gerät weiter"
    );
    assert_eq!(status.kind, StatusKind::Error);
    // The track plays on: the synth through the failed slot, every other device unaffected.
    let after = level(&mut app, drums, Duration::from_millis(1500));
    assert!(after > 0.05, "Drums after the crash: {after}");
    assert!(app.engine().playhead() > bar_5());
    for (id, name, status) in statuses(&app) {
        if id != crash {
            assert_eq!(status, DeviceStatus::Ok, "{name}");
        }
    }

    // "Neu laden": a new bridge in the same slot.
    app.dispatch(Action::Plugin(PluginAction::Reload(crash)));
    assert_eq!(app.engine().device_status(crash), DeviceStatus::Loading);
    step_until(&mut app, "the reloaded device", all_ok);
    app.dispatch(Action::Transport(TransportAction::TogglePlay));

    // A parameter turned on the card reaches the document with the plugin's state snapshot, as an
    // undo step named after the parameter. (A snapshot of an unchanged plugin is no change.)
    let pad_synth = app.project().tracks[3].channel.devices[0].id;
    assert_eq!(app.project().tracks[3].channel.devices[0].state, None);
    let undo = app.doc().undo_label().map(str::to_owned);
    let gain = app
        .engine()
        .host()
        .params(pad_synth)
        .unwrap()
        .iter()
        .find(|p| p.name == "Gain")
        .expect("a gain parameter")
        .id;
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device: pad_synth,
        id: gain,
        value: 0.5,
    }));
    step_until(&mut app, "the Pad's state", |app| {
        app.project().tracks[3].channel.devices[0].state.is_some()
    });
    assert_ne!(app.doc().undo_label().map(str::to_owned), undo);
    assert_eq!(app.undo_text().as_deref(), Some("Gain ändern"));
    assert!(app.doc().is_dirty());

    // Saving snapshots every bridged device into the file.
    app.dispatch(Action::Project(ProjectAction::Save));
    assert!(!app.doc().is_dirty());
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for (_, device) in devices(&saved) {
        if is_hexa(&device) {
            assert_eq!(device.state, None);
        } else {
            assert!(
                device.state.is_some(),
                "{} without state",
                device.plugin.name
            );
        }
    }
    app.shutdown();
}

/// The demo with the plugins installed on this machine: the Sampler, Surge XT and Dexed load (the
/// latter two sound), the others (Vital, LSP, Dragonfly, TAL) stay `Missing` with their state.
/// Run with `cargo test -p komponist-app --test it plugins:: -- --ignored` (scans the default folders
/// into a scratch cache; Surge XT may take seconds to load).
#[test]
#[ignore = "needs Surge XT and Dexed installed"]
fn the_demo_plays_the_installed_plugins() {
    let dir = scratch("installed");
    let config = config(&dir);
    let mut project = komponist_model::fixtures::demo_project();
    // A state blob for every device that is not installed here, to see it kept.
    let blob: Arc<[u8]> = Arc::from(&b"komponist test state \x00\x01\x02"[..]);
    let installed = ["Hexa", "Klopfer", "Surge XT", "Dexed"];
    for track in &mut project.tracks {
        for device in &mut Arc::make_mut(track).channel.devices {
            if !installed.contains(&device.plugin.name.as_str()) {
                Arc::make_mut(device).state = Some(Arc::clone(&blob));
            }
        }
    }
    let file = dir.join("Night Drive.komponist");
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();
    let mut app = app_with(
        komponist_model::fixtures::empty_project(),
        PluginHost::with_reference_devices(config),
    );
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.step();
    app.engine_mut()
        .host_mut()
        .start_scan(&PluginDirs::defaults());
    let settled = |app: &App| {
        !app.engine().host().is_scanning()
            && statuses(app)
                .iter()
                .all(|(_, _, s)| matches!(s, DeviceStatus::Ok | DeviceStatus::Missing))
    };
    step_until(&mut app, "all installed plugins loaded", settled);
    for (_, name, status) in statuses(&app) {
        let expected = if installed.contains(&name.as_str()) {
            DeviceStatus::Ok
        } else {
            DeviceStatus::Missing
        };
        assert_eq!(status, expected, "{name}");
    }
    let missing = app.engine().host().missing_plugins(app.project());
    assert_eq!(missing.len(), 8, "{missing:?}");

    app.dispatch(Action::Transport(TransportAction::Locate(bar_5())));
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    // Surge XT and Dexed play; the Sampler without samples stays silent, so Drums only has to be
    // `Ok`.
    let pad = track_named(app.project(), "Pad");
    let pad_level = level(&mut app, pad, Duration::from_secs(2));
    let keys = track_named(app.project(), "Keys");
    let (l, r) = app.engine().track_meter(keys).unwrap().take();
    eprintln!(
        "Pad (Surge XT): peak {pad_level:.3}, Keys (Dexed): peak {:.3}",
        l.max(r)
    );
    assert!(pad_level > 0.01, "Pad (Surge XT) is silent");
    assert!(l.max(r) > 0.01, "Keys (Dexed) is silent");
    app.dispatch(Action::Transport(TransportAction::TogglePlay));

    // The missing devices' states survive a save byte for byte.
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    for (_, device) in devices(&saved) {
        if !installed.contains(&device.plugin.name.as_str()) {
            assert_eq!(
                device.state.as_deref(),
                Some(&blob[..]),
                "{}",
                device.plugin.name
            );
        }
    }
    app.shutdown();
}

#[test]
fn standalone_clap_preset_round_trips_through_catalog_browser_and_bridge() {
    use egui::{Key, Modifiers};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use komponist_app::actions::EditAction;
    use komponist_app::settings::Favorites;
    use komponist_app::ui_state::BrowserSegment;
    use komponist_app::views::browser::model::{self, Input};
    use komponist_i18n::{Lang, de};
    use komponist_model::edits::{NewTrack, add_track};
    use std::collections::BTreeSet;

    let Some(bundle) = std::env::var_os("KOMPONIST_LAYOUT_FIXTURE") else {
        eprintln!("set KOMPONIST_LAYOUT_FIXTURE to the standalone C CLAP fixture");
        return;
    };
    let bundle = std::path::PathBuf::from(bundle);
    let dir = scratch("external-clap-preset");
    let installed = dir.join("installed");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::copy(&bundle, installed.join("layout-fixture.clap")).unwrap();
    let mut host = PluginHost::new(config(&dir));
    host.start_scan(&PluginDirs {
        clap: vec![installed],
        vst3: Vec::new(),
    });
    poll_until(&mut host, "CLAP preset scan", |event| {
        matches!(event, PluginManagerEvent::ScanFinished(_))
    });

    let plugin = clap_plugin("org.klang-studio.komponist.layout-fixture.valid");
    let preset = &host.catalog().resolve(&plugin).unwrap().clap_presets[0];
    assert_eq!(preset.display_name("de"), "Testanhebung");
    let source = preset.source.clone();
    let (project, track) = add_track(
        &Project::new("CLAP presets"),
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let rows = model::rows(
        Lang::De,
        &Input {
            catalog: host.catalog(),
            favorites: &Favorites::default(),
            segment: BrowserSegment::Effects,
            query: "Testanhebung",
            collapsed: &BTreeSet::new(),
            project: &project,
            exports: &[],
        },
    );
    let row = rows
        .iter()
        .find(|row| {
            matches!(&row.kind, model::RowKind::Preset { source: row_source, .. } if row_source == &source)
        })
        .expect("localized name finds the preset");
    assert!(row.name.contains("Testanhebung"));
    let actions = model::activate(row, &project, Some(track));
    let mut app = app_with(project, host);
    for action in actions {
        app.dispatch(action);
    }
    step_until(&mut app, "new preset-initialized effect", |app| {
        app.project()
            .track(track)
            .is_some_and(|track| !track.channel.devices.is_empty())
    });
    let device = app.project().track(track).unwrap().channel.devices[0].id;
    step_until(&mut app, "preset effect ready", |app| {
        app.engine().host().status(device) == DeviceStatus::Ok
            && app.engine().host().param_base(device, 42) == Some(0.75)
    });
    assert_eq!(app.engine().host().param_base(device, 7), Some(2.0));
    let serialized = persist::to_json(app.project()).unwrap();
    assert!(
        persist::from_json(&serialized)
            .unwrap()
            .find_device(device)
            .unwrap()
            .2
            .state
            .is_some()
    );

    let preset_file = dir.join("Fixture Boost.kppreset");
    app.dispatch(Action::Plugin(PluginAction::SaveUserPreset(device)));
    let mut h = crate::acceptance_support::harness(app);
    let enter_path = |h: &mut Harness<'_, App>, field: &str| {
        h.get_by_label(field).focus();
        h.run_steps(1);
        h.key_press_modifiers(Modifiers::COMMAND, Key::A);
        h.run_steps(2);
        h.get_by_label(field)
            .type_text(preset_file.to_str().unwrap());
        h.run_steps(1);
        h.key_press(Key::Enter);
        h.run_steps(2);
    };
    enter_path(&mut h, de::FILE_NAME);
    step_until(h.state_mut(), "CLAP user preset written", |_| {
        preset_file.is_file()
    });
    let saved: serde_json::Value =
        serde_json::from_reader(std::fs::File::open(&preset_file).unwrap()).unwrap();
    assert_eq!(saved["plugin_id"], plugin.id);
    assert_eq!(saved["version"], 1);
    assert!(!saved["state"].as_array().unwrap().is_empty());

    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: 42,
            value: 0.25,
        }));
    step_until(h.state_mut(), "different plugin value persisted", |app| {
        app.engine().host().param_base(device, 42) == Some(0.25)
            && app.device_events().iter().any(|event| matches!(event, PluginManagerEvent::StateSnapshot { device: id, .. } if *id == device))
    });
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::LoadUserPreset(device)));
    h.run_steps(2);
    h.get_by_label("Fixture Boost.kppreset").click();
    h.run_steps(2);
    h.get_by_label(de::FILE_LOAD).click();
    h.run_steps(2);
    let app = h.state_mut();
    step_until(app, "CLAP user preset applied", |app| {
        app.engine().host().param_base(device, 42) == Some(0.75)
    });
    app.dispatch(Action::Edit(EditAction::Undo));
    step_until(app, "undo reveals prior value", |app| {
        app.engine().host().param_base(device, 42) == Some(0.25)
    });
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device,
        source,
    }));
    step_until(app, "discovered preset applied", |app| {
        app.device_events().iter().any(|event| matches!(event, PluginManagerEvent::ClapPresetLoaded { device: id, .. } if *id == device))
    });
    assert_eq!(app.engine().host().param_base(device, 42), Some(0.75));
    let host = app.engine_mut().host_mut();
    assert!(host.set_param(device, 42, 0.375));
    assert!(host.capture_clap_preset(device));
    let mut captured = None;
    step_until(
        app,
        "preset capture after the queued parameter edit",
        |app| {
            captured = app.device_events().iter().find_map(|event| match event {
                PluginManagerEvent::ClapPresetCaptured { device: id, result } if *id == device => {
                    Some(result.clone().unwrap())
                }
                _ => None,
            });
            captured.is_some()
        },
    );
    let captured = captured.unwrap();
    // The standalone fixture stores gain as the first native double.
    assert_eq!(f64::from_ne_bytes(captured[..8].try_into().unwrap()), 0.375);

    assert!(app.engine_mut().host_mut().capture_clap_preset(device));
    app.dispatch(Action::Plugin(PluginAction::Reload(device)));
    assert!(app.engine_mut().host_mut().poll().iter().any(|event| {
        matches!(
            event,
            PluginManagerEvent::ClapPresetCaptured { device: captured, result: Err(_) }
                if *captured == device
        )
    }));
    app.shutdown();
}

#[test]
fn scanned_device_metadata_and_availability_follow_the_bundle() {
    use komponist_app::settings::Favorites;
    use komponist_app::ui_state::{BrowserSegment, Dialog};
    use komponist_app::views::browser::model::{self, Input, RowKind};
    use komponist_i18n::Lang;
    use komponist_model::ChannelRef;
    use komponist_model::edits::{NewDevice, NewTrack, add_track, insert_device};
    use std::collections::BTreeSet;

    let Some(bundle) = std::env::var_os("KOMPONIST_LAYOUT_FIXTURE") else {
        eprintln!("set KOMPONIST_LAYOUT_FIXTURE to the standalone C CLAP fixture");
        return;
    };
    let dir = scratch("catalog-device-identity");
    let installed = dir.join("installed");
    let installation = installed.join("layout-fixture.clap");
    std::fs::create_dir_all(&installed).unwrap();
    std::fs::copy(&bundle, &installation).unwrap();
    let dirs = PluginDirs {
        clap: vec![installed],
        vst3: Vec::new(),
    };
    let config = config(&dir);
    #[cfg(unix)]
    let (config, probe_failure) = {
        let mut config = config;
        let probe_failure = dir.join("fail-probe");
        let bridge = std::mem::replace(&mut config.bridge_exe, "/bin/sh".into());
        config.bridge_args.splice(
            0..0,
            [
                "-c".into(),
                r#"failure="$1"; bridge="$2"; shift 2
                if [ "$2" = "--scan" ] && [ -f "$failure" ]; then
                    exit 1
                fi
                exec "$bridge" "$@"
            "#
                .into(),
                "fixture-probe".into(),
                probe_failure.as_os_str().into(),
                bridge.into_os_string(),
            ],
        );
        (config, probe_failure)
    };
    let mut host = PluginHost::new(config.clone());
    host.start_scan(&dirs);
    poll_until(&mut host, "fixture catalog scan", |event| {
        matches!(event, PluginManagerEvent::ScanFinished(_))
    });

    let catalog = host.catalog();
    let id = "org.klang-studio.komponist.layout-fixture.graphs";
    let entry = catalog.resolve(&clap_plugin(id)).unwrap();
    assert_eq!(entry.plugin.vendor, "klang-studio");
    assert!(
        entry
            .features
            .iter()
            .any(|feature| feature == "audio-effect")
    );
    let fallback = catalog
        .resolve(&clap_plugin(
            "org.klang-studio.komponist.layout-fixture.missing",
        ))
        .unwrap();
    assert_eq!(fallback.display_name("de"), "Layout Fixture (missing)");
    assert_eq!(
        fallback.description("de"),
        Some("Standalone stereo CLAP fixture.")
    );
    let mut plugin = entry.plugin.clone();
    plugin.name = "Saved fixture name".into();
    let (project, track) = add_track(
        &Project::new("Catalog identity"),
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (project, device) = insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            plugin: plugin.clone(),
            role: DeviceRole::Effect,
            state: None,
            sidechain: None,
        },
    )
    .unwrap();

    for (lang, name, description) in [
        (Lang::De, "Layout-Testgerät", "Prüft die native Anzeige."),
        (Lang::En, "Layout Fixture", "Exercises the native display."),
    ] {
        assert_eq!(
            komponist_app::views::catalog_plugin_name(lang, catalog, &plugin),
            name,
        );
        let input = Input {
            catalog,
            favorites: &Favorites::default(),
            segment: BrowserSegment::Effects,
            query: description,
            collapsed: &BTreeSet::new(),
            project: &project,
            exports: &[],
        };
        for rows in [
            model::rows(lang, &input),
            model::picker_rows(lang, &input, false),
        ] {
            let row = rows
                .iter()
                .find(|row| matches!(&row.kind, RowKind::Plugin { plugin, .. } if plugin.id == id))
                .unwrap();
            assert_eq!(row.name, name);
            assert_eq!(row.description.as_deref(), Some(description));
        }
    }

    let mut app = app_with(project, host);
    step_until(&mut app, "installed device ready", |app| {
        app.engine().device_status(device) == DeviceStatus::Ok
    });
    #[cfg(unix)]
    {
        let pid = app.engine().host().bridge_pid(device).unwrap();
        std::fs::write(&probe_failure, []).unwrap();
        // Force a descriptor refresh while leaving the bundle and its loaded instance intact.
        std::fs::File::open(&installation)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(5)),
            )
            .unwrap();
        app.engine_mut().host_mut().start_scan(&dirs);
        step_until(&mut app, "probe failed", |app| {
            app.device_events().iter().any(|event| {
                matches!(
                    event, PluginManagerEvent::ScanFinished(report) if report.skipped.len() == 1
                )
            })
        });
        let host = app.engine().host();
        assert!(host.catalog().resolve(&plugin).is_none());
        assert_eq!(host.status(device), DeviceStatus::Ok);
        assert_eq!(host.bridge_pid(device), Some(pid));
        assert!(host.missing_plugins(app.project()).is_empty());
        assert!(app.ui().dialog.is_none());

        std::fs::remove_file(probe_failure).unwrap();
        app.engine_mut().host_mut().start_scan(&dirs);
        step_until(&mut app, "unchanged bundle probe retried", |app| {
            app.engine().host().catalog().resolve(&plugin).is_some()
        });
        assert_eq!(app.engine().host().bridge_pid(device), Some(pid));
    }

    // Send a live edit immediately before removing the bundle, without waiting for the
    // normal debounced snapshot to put it into the project.
    let prior = app.project().find_device(device).unwrap().2.state.clone();
    assert!(app.engine_mut().host_mut().set_param(device, 42, 0.75));
    assert_eq!(app.project().find_device(device).unwrap().2.state, prior);

    let mut removed_dirs = dirs.clone();
    if cfg!(windows) {
        // Windows may keep the loaded library open. Removing its folder from the scan
        // exercises the same catalog transition while the bridge remains live.
        removed_dirs.clap.clear();
    } else {
        std::fs::remove_file(&installation).unwrap();
        let restarted = PluginHost::new(config);
        assert!(restarted.catalog().resolve(&plugin).is_none());
    }
    app.engine_mut().host_mut().start_scan(&removed_dirs);
    step_until(&mut app, "removed device missing", |app| {
        app.engine().device_status(device) == DeviceStatus::Missing
    });
    assert!(app.engine().host().catalog().resolve(&plugin).is_none());
    assert!(matches!(
        &app.ui().dialog,
        Some(Dialog::MissingPlugins(missing))
            if missing.len() == 1 && missing[0].device == device && missing[0].plugin == plugin
    ));
    let saved = app.project().clone();
    let saved_device = saved.find_device(device).unwrap().2;
    assert_eq!(saved_device.plugin.id, plugin.id);
    let captured = saved_device
        .state
        .as_deref()
        .expect("live state captured before retirement");
    assert_eq!(f64::from_ne_bytes(captured[..8].try_into().unwrap()), 0.75);

    if !installation.exists() {
        std::fs::copy(&bundle, &installation).unwrap();
    }
    app.engine_mut().host_mut().start_scan(&dirs);
    step_until(&mut app, "reinstalled device ready", |app| {
        app.engine().device_status(device) == DeviceStatus::Ok
    });
    assert!(app.engine().host().catalog().resolve(&plugin).is_some());
    assert!(
        matches!(&app.ui().dialog, Some(Dialog::MissingPlugins(missing)) if missing.is_empty())
    );
    assert_eq!(app.project(), &saved);
    assert_eq!(app.engine().host().param_base(device, 42), Some(0.75));
    app.shutdown();
}

#[test]
fn native_layout_is_discovered_edited_saved_and_revalidated() {
    use egui_kittest::kittest::Queryable as _;
    use komponist_app::actions::{EditAction, UiAction};
    use komponist_app::views::devices::model::cards;
    use komponist_i18n::Lang;
    use komponist_ipc::device_canvas::Primitive;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
    use komponist_model::edits::{
        NewClip, NewClipContent, NewDevice, NewNote, NewTrack, add_clip, add_track, insert_device,
    };
    use komponist_model::params::ParamId;
    use komponist_model::{ChannelRef, PPQ};
    use std::path::PathBuf;

    let Some(bundle) = std::env::var_os("KOMPONIST_LAYOUT_FIXTURE") else {
        eprintln!("set KOMPONIST_LAYOUT_FIXTURE to the standalone C CLAP fixture");
        return;
    };
    let dir = scratch("native-clap-layout");
    let installed = dir.join("installed");
    std::fs::create_dir_all(&installed).unwrap();
    let dirs = PluginDirs {
        clap: vec![installed.clone()],
        vst3: Vec::new(),
    };
    let (mut project, track) = add_track(
        &Project::new("Native layout"),
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    (project, _) = add_clip(
        &project,
        track,
        NewClip {
            start: Tick::ZERO,
            length: Tick(16 * PPQ),
            name: "Fixture notes".into(),
            content: NewClipContent::Midi {
                notes: vec![NewNote {
                    start: Tick::ZERO,
                    length: Tick(PPQ),
                    pitch: 60,
                    velocity: 100,
                }],
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let mut device_ids = Vec::new();
    for (index, name) in [
        "presentation",
        "missing",
        "unknown-param",
        "graphs",
        "invalid-graphs",
    ]
    .into_iter()
    .enumerate()
    {
        let (modified, id) = insert_device(
            &project,
            ChannelRef::Track(track),
            index,
            NewDevice {
                plugin: clap_plugin(&format!("org.klang-studio.komponist.layout-fixture.{name}")),
                role: DeviceRole::Effect,
                state: None,
                sidechain: None,
            },
        )
        .unwrap();
        project = modified;
        device_ids.push(id);
    }
    let [valid, missing, invalid, graphs, invalid_graphs] = device_ids.try_into().unwrap();
    let owner = AutomationOwner::Track(track);
    let target = AutomationTarget::Param {
        device: graphs,
        param: ParamId::plugin(42),
    };
    let (project, lane) = komponist_model::edits::automation::add_lane(
        &project,
        owner,
        target.clone(),
        "Gain".into(),
    )
    .unwrap();
    let project = komponist_model::edits::automation::set_lane_points(
        &project,
        owner,
        lane,
        Points::new(vec![
            Point {
                tick: Tick::ZERO,
                value: 0.5,
                shape: Shape::Hold,
            },
            Point::linear(Tick(2 * PPQ), 0.75),
        ]),
    )
    .unwrap();
    let file = dir.join("Layout.komponist");
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();
    let mut app = app_with(Project::new("Empty"), PluginHost::new(config(&dir)));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    app.engine_mut().host_mut().start_scan(&dirs);
    step_until(&mut app, "empty scan", |app| {
        app.device_events()
            .iter()
            .any(|e| matches!(e, PluginManagerEvent::ScanFinished(_)))
    });
    assert_eq!(app.engine().host().status(valid), DeviceStatus::Missing);

    let bundle = PathBuf::from(bundle);
    std::fs::copy(&bundle, installed.join(bundle.file_name().unwrap())).unwrap();
    app.engine_mut().host_mut().start_scan(&dirs);
    step_until(&mut app, "all fixture instances ready", |app| {
        [valid, missing, invalid, graphs, invalid_graphs]
            .into_iter()
            .all(|id| app.engine().host().status(id) == DeviceStatus::Ok)
    });
    let host = app.engine().host();
    assert!(host.native_layout(valid).is_some());
    assert!(host.native_layout(missing).is_none());
    assert!(host.native_layout(invalid).is_none());
    assert!(host.native_layout(graphs).unwrap().has_canvas());
    assert!(host.native_layout(invalid_graphs).is_none());
    let cards = cards(Lang::De, app.project().track(track).unwrap(), host, &[]);
    assert_eq!(cards[0].layout.pages[0].name, "Übersicht");
    let valid_card_label = Lang::De.devices_card(&cards[0].name);
    let gain = cards[0].param(&ParamId::plugin(42)).unwrap();
    assert_eq!(gain.name(), "Verstärkung");
    assert_eq!(gain.info.unit, komponist_model::params::Unit::Ms);
    assert_eq!(gain.info.range(), (10.0, 1000.0));
    assert!((gain.info.to_plain(0.5) - 100.0).abs() < 1e-9);

    assert_eq!(cards[0].param(&ParamId::plugin(7)).unwrap().wire, 7);
    assert!(
        cards[0]
            .param(&ParamId::plugin(7))
            .unwrap()
            .info
            .automatable
    );
    assert_eq!(cards[1].layout.pages[0].cells.len(), 2);
    assert_eq!(cards[3].layout.pages[0].name, "Diagramme");
    assert_eq!(cards[3].layout.pages[1].name, "Anzeige");
    assert_eq!(cards[3].param(&ParamId::plugin(42)).unwrap().wire, 42);

    let rendered_gain = |app: &App| {
        let display = app.engine().host().device_display(graphs)?;
        let frame = display.frames.iter().find(|frame| frame.key == "scope")?;
        match frame.primitives.as_slice() {
            [Primitive::Line { to, .. }] => Some(to[1]),
            _ => None,
        }
    };
    step_until(&mut app, "independent canvas display list", |app| {
        rendered_gain(app) == Some(0.5)
    });

    app.dispatch(Action::Project(ProjectAction::ContinueWithMissing));
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    let program = app.engine().program().expect("fixture engine program");
    assert!(program.slots.slot(graphs).is_some());
    assert!(
        program.automation_reads(owner, &target),
        "fixture gain curve is unresolved"
    );
    step_until(&mut app, "fixture transport plays", |app| {
        app.engine().playhead() > Tick::ZERO
    });
    step_until(
        &mut app,
        "automation reaches CLAP ID 42 during processing",
        |app| rendered_gain(app) == Some(0.75),
    );
    app.dispatch(Action::Transport(TransportAction::Stop));

    app.dispatch(Action::Ui(UiAction::RevealDevice(valid)));
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(egui::vec2(1920.0, 1080.0))
        .build_ui_state(|ui, app| app.show(ui), app);
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::Group, &valid_card_label)
        .get_by_label("Modus: Warm")
        .click_accesskit();
    h.run_steps(3);
    let app = h.state_mut();
    step_until(app, "choice snapshot", |app| {
        app.project().find_device(valid).unwrap().2.state.is_some()
            && app.engine().host().param_base(valid, 7) == Some(1.0)
    });
    app.dispatch(Action::Edit(EditAction::Undo));
    step_until(app, "undo restored mode", |app| {
        app.engine().host().param_base(valid, 7) == Some(0.0)
    });
    app.dispatch(Action::Edit(EditAction::Redo));
    step_until(app, "redo restored mode", |app| {
        app.engine().host().param_base(valid, 7) == Some(1.0)
    });
    app.dispatch(Action::Project(ProjectAction::Save));
    app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
    step_until(app, "reopened native layout and state", |app| {
        let host = app.engine().host();
        host.status(valid) == DeviceStatus::Ok
            && host.native_layout(valid).is_some()
            && host.param_base(valid, 7) == Some(1.0)
    });

    let saved_before = std::fs::read(&file).unwrap();
    app.dispatch(Action::Plugin(PluginAction::SetParamText {
        device: valid,
        id: 42,
        text: "Focus".into(),
        fallback: None,
    }));
    app.dispatch(Action::Project(ProjectAction::Save));
    assert_eq!(std::fs::read(&file).unwrap(), saved_before);
    step_until(
        app,
        "committed CLAP text converted, displayed and saved",
        |app| {
            let host = app.engine().host();
            let saved = komponist_app::project_io::load_project(&file).unwrap();
            let state = saved
                .find_device(valid)
                .unwrap()
                .2
                .state
                .as_deref()
                .unwrap();
            f64::from_ne_bytes(state[..8].try_into().unwrap()) == 0.625
                && host
                    .param_value(valid, 42)
                    .is_some_and(|value| value.value == 0.625 && value.text == "Focus")
        },
    );

    let refreshed = komponist_app::views::devices::model::cards(
        Lang::De,
        app.project().track(track).unwrap(),
        app.engine().host(),
        &[],
    );
    let gain = refreshed[0].param(&ParamId::plugin(42)).unwrap();
    assert_eq!(gain.text_at(gain.value), "Focus");
    // During a gesture the next value is already shown before the plugin sends its text.
    assert_eq!(gain.text_at(100.0), "100 ms");
    assert_eq!(gain.text_at(10.0), "Aus");

    app.dispatch(Action::Plugin(PluginAction::SetParamText {
        device: valid,
        id: 42,
        text: "invalid parameter input".into(),
        fallback: None,
    }));
    assert!(!app.close_requested(), "wait for the committed entry");
    step_until(app, "rejected text keeps the document open", |app| {
        app.ui()
            .status
            .as_ref()
            .is_some_and(|status| status.text == app.lang().devices_parameter_text_failed())
    });
    assert_eq!(app.ui().screen, komponist_app::ui_state::Screen::Project);
    assert_eq!(app.engine().host().param_base(valid, 42), Some(0.625));

    let state = |hidden| -> Arc<[u8]> {
        [0.5f64, 1.0, hidden]
            .into_iter()
            .flat_map(f64::to_ne_bytes)
            .collect::<Vec<_>>()
            .into()
    };
    assert!(app.engine_mut().host_mut().set_state(valid, &state(1.0)));
    step_until(app, "incompatible rescan removes layout", |app| {
        app.engine().host().native_layout(valid).is_none()
    });
    assert!(app.engine_mut().host_mut().set_state(valid, &state(0.0)));
    step_until(app, "compatible rescan restores layout", |app| {
        app.engine().host().native_layout(valid).is_some()
    });

    #[cfg(unix)]
    {
        let bridge = app.engine().host().bridge_pid(valid).unwrap();
        assert!(
            std::process::Command::new("kill")
                .args(["-KILL", &bridge.to_string()])
                .status()
                .unwrap()
                .success()
        );
        step_until(app, "bridge exit", |app| {
            matches!(app.engine().host().status(valid), DeviceStatus::Crashed(_))
        });
        assert!(app.engine().host().native_layout(valid).is_none());
        app.dispatch(Action::Plugin(PluginAction::Reload(valid)));
        step_until(app, "layout restored by new bridge", |app| {
            app.engine().host().native_layout(valid).is_some()
        });
    }
    app.shutdown();
}

#[test]
fn standalone_clap_layout_renders_three_graphs() {
    use egui::accesskit::Role;
    use egui_kittest::{SnapshotOptions, SnapshotResults, kittest::Queryable as _};
    use komponist_app::UiAction;
    use komponist_app::ui_state::Panel;
    use komponist_i18n::Lang;
    use komponist_model::ChannelRef;
    use komponist_model::edits::{NewDevice, NewTrack, add_track, insert_device};
    use std::path::PathBuf;

    let Some(bundle) = std::env::var_os("KOMPONIST_LAYOUT_FIXTURE") else {
        eprintln!("set KOMPONIST_LAYOUT_FIXTURE to the standalone C CLAP fixture");
        return;
    };
    let bundle = PathBuf::from(bundle);
    assert!(bundle.is_file());
    let plugin = clap_plugin("org.klang-studio.komponist.layout-fixture.graphs");
    let (project, track) = add_track(
        // Preserve the project caption painted in the reference snapshots.
        &Project::new("Layout v2"),
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (project, device) = insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            plugin,
            role: DeviceRole::Effect,
            state: None,
            sidechain: None,
        },
    )
    .unwrap();

    let mut snapshots = SnapshotResults::new();
    for (lang, page, envelope, waveform) in [
        (Lang::De, "Diagramme", "Hüllkurve", "Wellenform"),
        (Lang::En, "Graphs", "Envelope", "Waveform"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let mut host = PluginHost::new(config(dir.path()));
        host.start_scan(&PluginDirs {
            clap: vec![bundle.parent().unwrap().to_owned()],
            vst3: Vec::new(),
        });
        poll_until(&mut host, "standalone layout fixture scan", |event| {
            matches!(event, PluginManagerEvent::ScanFinished(_))
        });
        let mut app = app_with(project.clone(), host).with_lang(lang);
        step_until(&mut app, "standalone layout through the bridge", |app| {
            let host = app.engine().host();
            host.status(device) == DeviceStatus::Ok && host.native_layout(device).is_some()
        });
        let host = app.engine().host();
        assert!(host.native_layout(device).unwrap().has_canvas());
        assert!(host.bridge_pid(device).is_some());
        app.dispatch(Action::Ui(UiAction::TogglePanel(Panel::Browser)));
        app.dispatch(Action::Ui(UiAction::TogglePanel(Panel::Inspector)));
        app.dispatch(Action::Ui(UiAction::RevealDevice(device)));

        let mut h = komponist_ui::snapshot::builder::<App>(1.0)
            .with_size(egui::vec2(1260.0, 600.0))
            .with_options(
                SnapshotOptions::new()
                    .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
            )
            .build_ui_state(|ui, app| app.show(ui), app);
        h.run_steps(3);
        h.get_by_label(page);
        for graph in ["Equalizer", envelope, waveform] {
            h.get_by_role_and_label(Role::Group, graph);
        }
        // These diagnostics depend on the test machine's audio scheduling.
        h.state_mut().ui_mut().status = None;
        h.step();
        let cpu = h
            .get_by_role_and_label(Role::Status, lang.transport_cpu())
            .rect();
        h.mask(cpu.expand(2.0));
        h.snapshot(format!("standalone-clap-layout-{}", lang.code()));
        snapshots.extend_harness(&mut h);
        h.state_mut().shutdown();
    }
    snapshots.unwrap();
}

#[test]
fn sdk_example_uses_native_layout_and_falls_back() {
    use egui::accesskit::Role;
    use egui_kittest::kittest::Queryable as _;
    use komponist_app::UiAction;
    use komponist_app::views::devices::model::cards;
    use komponist_i18n::Lang;
    use komponist_ipc::device_assets::{AssetStatus, OfflineRequest, OfflineStatus};
    use komponist_model::ChannelRef;
    use komponist_model::edits::{NewDevice, NewTrack, add_track, insert_device};
    use komponist_model::params::ParamId;
    use std::path::{Path, PathBuf};

    const PLUGIN_ID: &str = "org.klang-studio.komponist.native-clap-example";
    let Some(native) = std::env::var_os("KOMPONIST_SDK_EXAMPLE") else {
        eprintln!(
            "set KOMPONIST_SDK_EXAMPLE and KOMPONIST_SDK_EXAMPLE_PLAIN to the bundled SDK example builds"
        );
        return;
    };
    let native = PathBuf::from(native);
    let plain = PathBuf::from(
        std::env::var_os("KOMPONIST_SDK_EXAMPLE_PLAIN")
            .expect("the fallback check needs the SDK example built without native-layout"),
    );
    assert!(native.exists() && plain.exists());
    assert_ne!(
        native.parent(),
        plain.parent(),
        "each variant needs a separate CLAP scan folder",
    );

    let dir = scratch("sdk-clap-example");
    let file = dir.join("SDK example.komponist");
    let plugin = clap_plugin(PLUGIN_ID);
    let (project, track) = add_track(
        &Project::new("SDK example"),
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (project, device) = insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            plugin: plugin.clone(),
            role: DeviceRole::Effect,
            state: None,
            sidechain: None,
        },
    )
    .unwrap();
    let (project, _) = komponist_model::edits::add_clip(
        &project,
        track,
        komponist_model::edits::NewClip {
            start: Tick::ZERO,
            length: Tick(3840),
            content: komponist_model::edits::NewClipContent::Midi {
                notes: Vec::new(),
                take_name: String::new(),
            },
            name: "Asset check".into(),
        },
    )
    .unwrap();
    std::fs::write(&file, persist::to_json(&project).unwrap()).unwrap();

    let open = |bundle: &Path, run: &str| {
        let mut host = PluginHost::new(config(&dir.join(run)));
        host.start_scan(&PluginDirs {
            clap: vec![bundle.parent().unwrap().to_owned()],
            vst3: Vec::new(),
        });
        poll_until(&mut host, "SDK example scan", |event| {
            matches!(event, PluginManagerEvent::ScanFinished(_))
        });
        let descriptor = host
            .catalog()
            .resolve(&plugin)
            .expect("scanned SDK example");
        let komponist_plugins::catalog::PluginLocation::Bundle { path, .. } = &descriptor.location
        else {
            panic!("the SDK example must be a scanned CLAP bundle");
        };
        assert_eq!(
            std::fs::canonicalize(path).unwrap(),
            std::fs::canonicalize(bundle).unwrap(),
        );
        let mut app = app_with(Project::new("Empty"), host);
        app.dispatch(Action::Project(ProjectAction::OpenPath(file.clone())));
        step_until(&mut app, "SDK example loaded", |app| {
            app.engine().host().status(device) == DeviceStatus::Ok
        });
        app
    };

    let mut app = open(&native, "native");
    let host = app.engine().host();
    let layout = host.native_layout(device).expect("SDK native layout");
    let params = host.params(device).unwrap();
    layout.validate_params(params).unwrap();
    let gain = params.iter().find(|p| p.name == "Gain").unwrap();
    let mix = params.iter().find(|p| p.name == "Mix").unwrap();
    let gain_id = gain.id;
    let mix_id = mix.id;
    assert_eq!(
        layout.pages[0]
            .cells
            .iter()
            .flat_map(|c| c.parameter_ids())
            .collect::<Vec<_>>(),
        [gain_id, mix_id],
    );
    assert_eq!(host.param_base(device, gain_id), Some(gain.default));
    let german = cards(Lang::De, app.project().track(track).unwrap(), host, &[]);
    let english = cards(Lang::En, app.project().track(track).unwrap(), host, &[]);
    assert_eq!(german[0].layout.pages[0].name, "Hauptseite");
    assert_eq!(english[0].layout.pages[0].name, "Main");
    assert_eq!(german[0].name, "Pegelbeispiel");
    assert_eq!(english[0].name, "Native Gain Example");
    assert_eq!(
        german[0].param(&ParamId::plugin(gain_id)).unwrap().wire,
        gain_id
    );

    let reference = dir.join("Reference.txt");
    std::fs::write(&reference, b"fixture reference").unwrap();
    step_until(&mut app, "SDK file slots and preparation", |app| {
        let host = app.engine().host();
        host.supports_offline_preparation(device)
            && host.asset_slots(device).is_some_and(|snapshot| {
                snapshot.slots[0].id == 42 && snapshot.slots[0].status == AssetStatus::Empty
            })
    });
    app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
        device,
        slot: 42,
        path: Some(reference.clone()),
    }));
    step_until(&mut app, "SDK file slot ready", |app| {
        app.engine()
            .host()
            .asset_slots(device)
            .is_some_and(|snapshot| snapshot.slots[0].status == AssetStatus::Ready)
    });
    let detached = app.engine_mut().detach().unwrap();
    for id in 1..=2 {
        app.engine_mut().host_mut().set_offline_preparation(
            device,
            Some(OfflineRequest {
                id,
                sample_rate: 48_000.0,
                tempo: 120.0,
            }),
        );
        assert_eq!(
            app.engine().host().offline_preparation_status(device),
            Some(OfflineStatus::Preparing),
        );
        step_until(&mut app, "SDK offline request completed", |app| {
            app.engine().host().offline_preparation_status(device) == Some(OfflineStatus::Ready)
        });
        app.engine_mut()
            .host_mut()
            .set_offline_preparation(device, None);
    }
    app.engine_mut().attach(detached);

    app.dispatch(Action::Ui(UiAction::RevealDevice(device)));
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(egui::vec2(1920.0, 1080.0))
        .build_ui_state(|ui, app| app.show(ui), app);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, "Pegel")
        .set_value_accesskit(0.5);
    h.run_steps(3);
    let app = h.state_mut();
    step_until(app, "SDK Gain changed and snapshotted", |app| {
        app.engine().host().param_base(device, gain_id) == Some(0.5)
            && app.project().find_device(device).unwrap().2.state.is_some()
    });
    app.dispatch(Action::Project(ProjectAction::Save));
    let saved = persist::from_json(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(saved.find_device(device).unwrap().2.state.is_some());
    assert_eq!(
        saved.find_device(device).unwrap().2.asset_links[&42].rel,
        Path::new("Reference.txt")
    );
    app.shutdown();
    drop(h);

    let mut reopened = open(&native, "reopen");
    step_until(&mut reopened, "native SDK example state restored", |app| {
        let host = app.engine().host();
        host.native_layout(device).is_some()
            && host.param_base(device, gain_id) == Some(0.5)
            && host
                .asset_slots(device)
                .is_some_and(|snapshot| snapshot.slots[0].status == AssetStatus::Ready)
    });
    let destination = dir.join("moved");
    std::fs::create_dir(&destination).unwrap();
    let moved = destination.join("Saved.komponist");
    reopened.dispatch(Action::Project(ProjectAction::SaveAs));
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(egui::vec2(1920.0, 1080.0))
        .build_ui_state(|ui, app| app.show(ui), reopened);
    h.run_steps(3);
    h.get_by_label("Dateiname").focus();
    h.run_steps(1);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Dateiname")
        .type_text(moved.to_str().unwrap());
    h.run_steps(1);
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    let app = h.state_mut();
    assert_eq!(app.project_path(), Some(moved.as_path()));
    assert_eq!(
        app.project().find_device(device).unwrap().2.asset_links[&42].resolve(Some(&destination)),
        Some(reference.clone())
    );

    let collected = destination.join("Saved Samples/Reference.txt");
    // Copying a restored non-audio file requires a fresh selection.
    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    assert!(!collected.exists());
    let unapproved = persist::from_json(&std::fs::read_to_string(&moved).unwrap()).unwrap();
    assert_eq!(
        unapproved.find_device(device).unwrap().2.asset_links[&42].resolve(Some(&destination)),
        Some(reference.clone())
    );

    app.dispatch(Action::Plugin(PluginAction::SetPluginAsset {
        device,
        slot: 42,
        path: Some(reference.clone()),
    }));
    app.dispatch(Action::Project(ProjectAction::CollectAndSave));
    assert_eq!(std::fs::read(&collected).unwrap(), b"fixture reference");
    assert_eq!(
        app.project().find_device(device).unwrap().2.asset_links[&42].resolve(Some(&destination)),
        Some(collected)
    );
    let settings = crate::export::export_settings(&dir, komponist_app::actions::ExportRange::Song);
    app.dispatch(Action::Project(ProjectAction::StartExport(settings)));
    step_until(app, "SDK export requested preparation", |app| {
        app.is_exporting()
            && app.engine().is_detached()
            && app.engine().host().offline_preparation_status(device)
                == Some(OfflineStatus::Preparing)
    });
    step_until(app, "SDK asset offline export", |app| !app.is_exporting());
    assert_eq!(app.exported().len(), 1);
    assert!(app.exported()[0].is_file());
    app.shutdown();
    drop(h);

    // The same project and author IDs must remain usable in an ordinary CLAP build.
    let mut fallback = open(&plain, "standard");
    step_until(
        &mut fallback,
        "standard SDK example state restored",
        |app| app.engine().host().param_base(device, gain_id) == Some(0.5),
    );
    let host = fallback.engine().host();
    assert!(host.native_layout(device).is_none());
    let generic = cards(
        Lang::De,
        fallback.project().track(track).unwrap(),
        host,
        &[],
    );
    assert_eq!(
        generic[0].param(&ParamId::plugin(gain_id)).unwrap().wire,
        gain_id
    );
    assert_eq!(
        generic[0].param(&ParamId::plugin(mix_id)).unwrap().wire,
        mix_id
    );
    fallback.shutdown();
}

#[test]
fn third_party_clap_macros_and_automation_restore_their_bases() {
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points, Shape};
    use komponist_model::edits::{self, NewDevice, NewTrack};
    use komponist_model::macros::Mapping;
    use komponist_model::params::ParamId;
    use komponist_model::{ChannelRef, PPQ};

    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("third-party-automation");
    let path = dir.join("Automation.komponist");
    let mut host = PluginHost::new(config(&dir));
    host.start_scan(&dirs);
    poll_until(&mut host, "ScanFinished", |event| {
        matches!(event, PluginManagerEvent::ScanFinished(_))
    });
    let (project, track) = edits::add_track(
        &Project::new("Opaque CLAP state"),
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(clap_plugin(SYNTH)),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let synth = project.track(track).unwrap().channel.devices[0].id;
    let (project, fx) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        1,
        NewDevice {
            role: DeviceRole::Effect,
            plugin: clap_plugin(FX),
            state: None,
            sidechain: None,
        },
    )
    .unwrap();

    let mut app = app_with(project.clone(), host);
    step_until(&mut app, "third-party plugins ready", all_ok);
    let gain_id = |device| {
        let info = app
            .engine()
            .host()
            .params(device)
            .unwrap()
            .iter()
            .find(|info| info.name == "Gain")
            .unwrap();
        assert!(komponist_plugins::params::is_automatable(info));
        info.id
    };
    let synth_gain = gain_id(synth);
    let fx_gain = gain_id(fx);
    let synth_base = app.engine().host().param_base(synth, synth_gain).unwrap();
    let fx_default = app.engine().host().param_base(fx, fx_gain).unwrap();
    let note_level = |app: &mut App, phase: &str| {
        use komponist_ipc::control::RenderMode;
        app.engine_mut()
            .hold_engine()
            .expect("hold the null backend for the tone probe");
        // Rendered like an export: the bridges in offline mode, the held engine offline. A
        // realtime block (`run_held_capture`) has a callback's deadline, and on a loaded machine
        // one late block falls back to the plugin's fallback output (#347).
        app.engine_mut()
            .host_mut()
            .set_render_mode(RenderMode::Offline);
        step_until(
            app,
            "offline bridge reactivation for the tone probe",
            all_ok,
        );
        let engine = app.engine_mut();
        engine.send(Command::LiveNote {
            track,
            key: 60,
            velocity: 127,
        });
        let (left, right) = engine.render_held(8192).unwrap();
        engine.send(Command::LiveNote {
            track,
            key: 60,
            velocity: 0,
        });
        assert!(engine.render_held(8192).is_some());
        engine.host_mut().set_render_mode(RenderMode::Realtime);
        engine.release_engine();
        let peak = left
            .iter()
            .chain(&right)
            .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
        assert!(peak > 0.0, "{phase} must sound");
        peak
    };
    let baseline = note_level(&mut app, "the reference tone");
    assert!(baseline > 0.01, "the base values produce a reference tone");

    let target = |device, id| AutomationTarget::Param {
        device,
        param: ParamId::plugin(id),
    };
    let owner = AutomationOwner::Track(track);
    let synth_target = target(synth, synth_gain);
    let project = edits::macros::add_mapping(
        &project,
        track,
        0,
        Mapping {
            target: target(fx, fx_gain),
            label: "Effect gain".into(),
            min: 0.0,
            max: 1.0,
            invert: false,
            bend: 0.0,
        },
    )
    .unwrap();
    let project = edits::macros::set_macro_value(&project, track, 0, 0.75).unwrap();
    let add_lane = |project: &Project, target, value| {
        let (project, lane) =
            edits::automation::add_lane(project, owner, target, "Gain curve".into()).unwrap();
        edits::automation::set_lane_points(
            &project,
            owner,
            lane,
            Points::new(vec![
                Point {
                    tick: Tick::ZERO,
                    value,
                    shape: Shape::Hold,
                },
                Point::linear(Tick(16 * PPQ), value),
            ]),
        )
        .unwrap()
    };
    let project = add_lane(&project, synth_target.clone(), 0.6);
    let project = add_lane(&project, AutomationTarget::Macro { index: 0 }, 0.2);
    std::fs::write(&path, persist::to_json(&project).unwrap()).unwrap();
    let open = |app: &mut App| {
        app.dispatch(Action::Project(ProjectAction::OpenPath(path.clone())));
        step_until(app, "compiled third-party automation", |app| {
            all_ok(app)
                && app
                    .engine()
                    .program()
                    .is_some_and(|program| program.automation_reads(owner, &synth_target))
        });
    };
    open(&mut app);
    let automated_level = |app: &mut App, phase| {
        app.engine_mut().send(Command::Play);
        step_until(app, "track macro automation applied", |app| {
            app.engine()
                .telemetry()
                .macro_values(track)
                .is_some_and(|values| (values[0] - 0.2).abs() < 1e-6)
        });
        note_level(app, phase)
    };
    let automated = automated_level(&mut app, "automation before save");
    assert!(
        automated > 0.0 && automated < baseline * 0.2,
        "both lanes attenuate the reference tone: {baseline} -> {automated}"
    );
    app.dispatch(Action::Project(ProjectAction::Save));

    let saved = persist::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let base = |device, id| saved.find_device(device).unwrap().2.parameter_bases[&id];
    assert_eq!(base(synth, synth_gain), synth_base);
    let fx_base = base(fx, fx_gain);
    assert!((fx_base - 0.75).abs() < 1e-6 && (fx_base - fx_default).abs() > 1e-6);
    assert_eq!(
        saved.track(track).unwrap().automation,
        project.track(track).unwrap().automation
    );
    assert_eq!(
        saved.track(track).unwrap().macros,
        project.track(track).unwrap().macros
    );

    app.shutdown();
    let mut app = app_with(Project::new("Empty"), PluginHost::new(config(&dir)));
    open(&mut app);
    assert_eq!(
        app.engine().host().param_base(synth, synth_gain),
        Some(synth_base)
    );
    let restored_fx = app.engine().host().param_base(fx, fx_gain).unwrap();
    assert!(
        (restored_fx - fx_base).abs() < 0.002,
        "plugin may quantize a base"
    );
    let reopened = automated_level(&mut app, "automation after reopening");
    assert!(
        (reopened - automated).abs() < automated * 0.05,
        "reopened automation level {reopened}, before save {automated}"
    );
    app.shutdown();
}

/// #347: `render_held` waits for a bridge block, as an export does. kp-test-slow sleeps 50 ms in
/// `process` for a note-on of key 126, far beyond the deadline of a realtime 256-frame block
/// (`run_held_capture`), which would give up after about 3 ms and play the fallback.
#[test]
fn held_offline_render_waits_for_a_slow_bridge() {
    use komponist_ipc::control::RenderMode;
    use komponist_model::edits::{self, NewTrack};

    const SLOW: &str = "de.klang-studio.kp-test-slow";
    const SLOW_KEY: u8 = 126;
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("held-offline-render");
    let mut host = PluginHost::new(config(&dir));
    host.start_scan(&dirs);
    poll_until(&mut host, "ScanFinished", |event| {
        matches!(event, PluginManagerEvent::ScanFinished(_))
    });
    let (project, track) = edits::add_track(
        &Project::new("Slow bridge"),
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(clap_plugin(SLOW)),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let slow = project.track(track).unwrap().channel.devices[0].id;
    let mut app = app_with(project, host);
    step_until(&mut app, "the slow plugin ready", all_ok);
    app.engine_mut().hold_engine().unwrap();
    app.engine_mut()
        .host_mut()
        .set_render_mode(RenderMode::Offline);
    step_until(&mut app, "offline bridge reactivation", all_ok);
    let late = app.engine().host().late_blocks(slow);
    app.engine_mut().send(Command::LiveNote {
        track,
        key: SLOW_KEY,
        velocity: 127,
    });
    let started = Instant::now();
    assert!(app.engine_mut().render_held(512).is_some());
    assert!(
        started.elapsed() >= Duration::from_millis(50),
        "the render waited for the stalled block"
    );
    assert_eq!(
        app.engine().host().late_blocks(slow),
        late,
        "an offline render waits for the bridge"
    );
    app.engine_mut().release_engine();
    app.shutdown();
}
