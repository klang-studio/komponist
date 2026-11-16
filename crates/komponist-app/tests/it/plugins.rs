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
    assert_eq!(bridged.len(), 11, "13 devices, all but the two hexas");
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
    // Lead is the longest chain: hexa and two effects (the third is bypassed); Keys has none.
    let hexa = project.tracks[2].channel.devices[0].id;
    let lead = host.latency(hexa) + 2 * 64;
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
    // Drums, Pad and Keys play only through bridged instruments (klopfer, Surge XT and Dexed in
    // the demo); Bass is hexa through a bridged effect.
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
    for (id, name, status) in statuses(&app) {
        let expected = if bridged.contains(&id) {
            DeviceStatus::Missing
        } else {
            DeviceStatus::Ok
        };
        assert_eq!(status, expected, "{name}");
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
    // hexa, the internal devices (WP-85) and the five fixtures.
    assert_eq!(
        app.engine().host().catalog().entries().len(),
        1 + komponist_builtin::internal::Internal::ALL.len() + 5
    );

    // Latencies recompile the program. Lead is the longest chain (hexa and two 64-frame
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
    let hexa = app.project().tracks[2].channel.devices[0].id;
    let synth = app.engine().host().latency(hexa);
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
    let installed = ["Hexa", "Sampler", "Surge XT", "Dexed"];
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
        PluginHost::new(config),
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
