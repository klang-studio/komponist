//! Shared harness of the plugin tests: the real `komponist-bridge` binary, the WP-27 test plugins,
//! the demo project with the test plugins in place of its external devices, and an app on the
//! null backend whose device host is a [`PluginHost`].
//!
//! The bridge binary: `$KOMPONIST_BRIDGE_EXE` if set, else `target/<profile>/komponist-bridge`. If
//! that is missing or older than the sources of komponist-bridge, komponist-ipc or
//! komponist-hosting, it is built with cargo first. The test plugins come from
//! `scripts/build-test-plugins.sh` (`target/test-plugins/clap/`); without them the tests skip with
//! a note.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use komponist_app::App;
use komponist_app::device_host::{PluginHost, is_pisynth};
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::settings::Settings;
use komponist_audio::{HostKind, StreamConfig};
use komponist_model::fixtures::demo_project;
use komponist_model::{
    Device, DeviceId, DeviceRole, Document, PluginFormat, PluginRef, Project, TrackId,
};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerConfig, PluginManagerEvent};

/// How long loading, scans and answers may take on a loaded machine.
pub const PATIENCE: Duration = Duration::from_secs(30);

pub const SYNTH: &str = "de.klang-studio.kp-test-synth";
pub const FX: &str = "de.klang-studio.kp-test-fx";
pub const CRASH: &str = "de.klang-studio.kp-test-crash";

/// The null backend's buffer in the app tests: the largest block, so a bridge's deadline (60 % of
/// the block) leaves room for unoptimised pisynth in the same callback.
pub const NULL_BUFFER: u32 = komponist_engine::MAX_BLOCK as u32;

/// The workspace's `target/` directory.
fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}

/// The `komponist-bridge` binary (see the module docs).
pub fn bridge_exe() -> PathBuf {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        if let Some(exe) = std::env::var_os("KOMPONIST_BRIDGE_EXE") {
            return PathBuf::from(exe);
        }
        // target/<profile>/deps/<test> → target/<profile>/komponist-bridge
        let test = std::env::current_exe().expect("test executable");
        let profile_dir = test.parent().and_then(Path::parent).expect("profile dir");
        let exe = profile_dir.join("komponist-bridge");
        if !is_fresh(&exe) {
            let mut cargo = Command::new(env!("CARGO"));
            cargo.args(["build", "-p", "komponist-bridge"]);
            if !cfg!(debug_assertions) {
                cargo.arg("--release");
            }
            let status = cargo.status().expect("run cargo build");
            assert!(status.success(), "cargo build -p komponist-bridge failed");
        }
        exe
    })
    .clone()
}

/// Whether `exe` exists and is newer than every source file it is built from.
fn is_fresh(exe: &Path) -> bool {
    let Ok(built) = std::fs::metadata(exe).and_then(|m| m.modified()) else {
        return false;
    };
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    ["komponist-bridge", "komponist-ipc", "komponist-hosting"]
        .iter()
        .all(|name| newest(&crates.join(name)) <= built)
}

fn newest(dir: &Path) -> SystemTime {
    let mut latest = SystemTime::UNIX_EPOCH;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return latest;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if name == "tests" || name == "target" {
            continue;
        }
        let time = if path.is_dir() {
            newest(&path)
        } else {
            entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(latest)
        };
        latest = latest.max(time);
    }
    latest
}

/// The folder of the WP-27 CLAP fixtures as scan folders, or `None` with a note if they are not
/// built.
pub fn fixture_dirs() -> Option<PluginDirs> {
    let dir = target_dir().join("test-plugins/clap");
    if dir.join("kp-test-synth.clap").exists() {
        Some(PluginDirs {
            clap: vec![std::fs::canonicalize(dir).unwrap()],
            vst3: Vec::new(),
        })
    } else {
        eprintln!(
            "skipped: {} is missing; run scripts/build-test-plugins.sh",
            dir.display()
        );
        None
    }
}

/// A fresh scratch folder for one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A plugin manager setup with its own (empty) scan cache in `dir` and no klopfer bundle.
pub fn config(dir: &Path) -> PluginManagerConfig {
    PluginManagerConfig {
        bridge_exe: bridge_exe(),
        cache_file: dir.join("cache/plugins.json"),
        klopfer_bundle: None,
        parallel_scans: 4,
    }
}

/// The reference a device stores for CLAP plugin `id`.
pub fn clap_plugin(id: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Clap,
        id: id.into(),
        name: id.rsplit('.').next().unwrap().into(),
        vendor: "klang-studio".into(),
        version: "0.1.0".into(),
        module: Box::default(),
    }
}

/// The demo project "Nachtfahrt" with kp-test-synth for every external instrument (klopfer,
/// Surge XT, Dexed, Vital) and kp-test-fx for every effect (LSP, TAL, Dragonfly); pisynth stays.
pub fn demo_with_test_plugins() -> Project {
    let mut project = demo_project();
    for track in &mut project.tracks {
        let track = Arc::make_mut(track);
        for device in &mut track.channel.devices {
            if is_pisynth(device) {
                continue;
            }
            let device = Arc::make_mut(device);
            device.plugin = match device.role {
                DeviceRole::Instrument => clap_plugin(SYNTH),
                DeviceRole::Effect | DeviceRole::MidiEffect => clap_plugin(FX),
            };
        }
    }
    project
}

/// Every device of `project` with its track, in document order.
pub fn devices(project: &Project) -> Vec<(TrackId, Device)> {
    project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter().map(|d| (t.id, (**d).clone())))
        .collect()
}

/// The devices of `project` that run in bridges (everything but pisynth).
pub fn bridged(project: &Project) -> Vec<DeviceId> {
    devices(project)
        .into_iter()
        .filter(|(_, d)| !is_pisynth(d))
        .map(|(_, d)| d.id)
        .collect()
}

/// The app on the null backend ([`NULL_BUFFER`] frames, no MIDI) with `project`, default settings
/// (nothing persisted) and `host` — the construction `main` uses, minus the window.
pub fn app_with(project: Project, host: PluginHost) -> App {
    let config = StreamConfig {
        host: HostKind::Null,
        buffer_frames: NULL_BUFFER,
        ..StreamConfig::default()
    };
    let (engine, started) = EngineSync::new(&config, MidiSetup::None, Box::new(host));
    started.expect("null backend");
    App::new(Document::new(project), engine, Settings::default(), None)
}

/// Steps `app` (one headless frame every 5 ms) until `done` holds; returns the device events seen
/// meanwhile. Panics after [`PATIENCE`].
pub fn step_until(
    app: &mut App,
    what: &str,
    mut done: impl FnMut(&App) -> bool,
) -> Vec<PluginManagerEvent> {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        app.step();
        seen.extend(app.device_events().iter().cloned());
        if done(app) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "no {what}; statuses {:?}; events {seen:?}",
            statuses(app)
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Steps `app` for `time`; returns the device events seen meanwhile.
pub fn step_for(app: &mut App, time: Duration) -> Vec<PluginManagerEvent> {
    let end = Instant::now() + time;
    let mut seen = Vec::new();
    while Instant::now() < end {
        app.step();
        seen.extend(app.device_events().iter().cloned());
        std::thread::sleep(Duration::from_millis(5));
    }
    seen
}

/// The status of every device in the app's project.
pub fn statuses(app: &App) -> Vec<(DeviceId, String, DeviceStatus)> {
    devices(app.project())
        .into_iter()
        .map(|(_, d)| (d.id, d.plugin.name, app.engine().device_status(d.id)))
        .collect()
}

/// Whether every device of the app's project is `Ok`.
pub fn all_ok(app: &App) -> bool {
    statuses(app).iter().all(|(_, _, s)| *s == DeviceStatus::Ok)
}

/// Process ids of this process's child processes running `komponist-bridge` (scans included).
pub fn bridge_children() -> Vec<u32> {
    let me = std::process::id();
    let mut pids = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // "pid (comm) state ppid …"; comm is cut to 15 bytes and may contain spaces.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let comm = &stat[open + 1..close];
        let ppid = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u32>().ok());
        if ppid == Some(me) && "komponist-bridge".starts_with(comm) && comm.len() >= 15 {
            pids.push(pid);
        }
    }
    pids
}

/// Whether process `pid` exists (a zombie counts: it is not reaped yet).
pub fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}
