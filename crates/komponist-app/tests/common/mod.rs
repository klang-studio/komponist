//! Shared harness of the plugin tests: the real bridge, the WP-27 test plugins,
//! the demo project with the test plugins in place of its external devices, and an app on the
//! null backend whose device host is a [`PluginHost`].
//!
//! The bridge: the `komponist` binary of this package run as `komponist --bridge` (cargo builds it
//! for the integration tests), exactly how the app starts its bridges (WP-94). The test plugins come from
//! `scripts/build-test-plugins.sh` (`target/test-plugins/<os>/clap/`); without them the tests skip with
//! a note.

#![allow(dead_code)]

#[allow(dead_code)]
#[path = "../../../../scripts/test-fixtures.rs"]
mod test_fixtures;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_app::App;
use komponist_app::device_host::{DeviceHost as _, PluginHost};
use komponist_app::engine_sync::{EngineSync, MidiSetup};
use komponist_app::settings::Settings;
use komponist_audio::{HostKind, StreamConfig};
use komponist_model::edits::{self, NewTrack};
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
/// the block) leaves room for unoptimised hexa in the same callback.
pub const NULL_BUFFER: u32 = komponist_engine::MAX_BLOCK as u32;

/// The workspace's `target/` directory.
pub fn target_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target")
}

/// The folder of the WP-27 CLAP fixtures as scan folders, or `None` with a note if they are not
/// built.
pub fn fixture_dirs() -> Option<PluginDirs> {
    let dir = target_dir()
        .join("test-plugins")
        .join(std::env::consts::OS)
        .join("clap");
    if test_fixtures::bundle_dir(&dir, "clap") {
        Some(PluginDirs {
            clap: vec![std::fs::canonicalize(dir).unwrap()],
            vst3: Vec::new(),
        })
    } else {
        None
    }
}

/// Scan only the fixture group that contains the required bundle.
pub fn clap_fixture_dirs(group: &str, bundle: &str) -> Option<PluginDirs> {
    let dir = target_dir()
        .join("test-plugins")
        .join(std::env::consts::OS)
        .join(group);
    test_fixtures::available(&dir.join(bundle)).then(|| PluginDirs {
        clap: vec![std::fs::canonicalize(dir).unwrap()],
        vst3: Vec::new(),
    })
}

/// Scan test folders through the ordinary external CLAP host.
pub fn scan_fixture_host(dir: &Path, dirs: &PluginDirs) -> PluginHost {
    let mut host = PluginHost::new(config(dir));
    host.start_scan(dirs);
    let deadline = Instant::now() + PATIENCE;
    while !host
        .poll()
        .iter()
        .any(|event| matches!(event, PluginManagerEvent::ScanFinished(_)))
    {
        assert!(
            Instant::now() < deadline,
            "fixture scan timed out for {:?}",
            dirs.clap
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    host
}

/// Scan a real device fixture as an ordinary external CLAP.
pub fn native_fixture(dir: &Path, bundle: &str, id: &str) -> Option<(PluginHost, PluginRef)> {
    let dirs = clap_fixture_dirs("builtin-clap", bundle)?;
    let host = scan_fixture_host(dir, &dirs);
    let plugin = host
        .catalog()
        .entries()
        .iter()
        .find(|entry| {
            entry.plugin.format == PluginFormat::Clap
                && entry.plugin.id == id
                && entry.plugin.module.as_ref() == bundle
        })
        .unwrap_or_else(|| panic!("{bundle} fixture missing from the external scan"))
        .plugin
        .clone();
    Some((host, plugin))
}

/// A project with one instrument and its track and device IDs.
pub fn instrument_project(name: &str, plugin: PluginRef) -> (Project, TrackId, DeviceId) {
    let (project, track) = edits::add_track(
        &Project::new(name),
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(plugin),
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
    (project, track, device)
}

/// A fresh scratch folder for one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-app-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A plugin manager setup with its own (empty) scan cache in `dir`.
pub fn config(dir: &Path) -> PluginManagerConfig {
    PluginManagerConfig {
        // The way the app starts bridges (WP-94): its own executable, `komponist --bridge`.
        bridge_exe: PathBuf::from(env!("CARGO_BIN_EXE_komponist")),
        bridge_args: vec![komponist_plugins::BRIDGE_FLAG.into()],
        cache_file: dir.join("cache/plugins.json"),
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

/// The model test song, played exclusively by the test synth and effect through bridges.
pub fn demo_with_test_plugins() -> Project {
    let mut project = demo_project();
    for track in &mut project.tracks {
        let track = Arc::make_mut(track);
        for device in &mut track.channel.devices {
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

/// The external fixture devices that run in bridges for tests using the ordinary scan host.
pub fn bridged(project: &Project) -> Vec<DeviceId> {
    devices(project)
        .into_iter()
        .filter(|(_, d)| matches!(d.plugin.format, PluginFormat::Clap | PluginFormat::Vst3))
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
#[cfg(target_os = "linux")]
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

/// Process ids of this process's child processes running `komponist --bridge` (scans included). macOS has
/// no `/proc` and the bridge keeps its name there, so this asks `ps` for the arguments.
#[cfg(target_os = "macos")]
pub fn bridge_children() -> Vec<u32> {
    let me = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let ppid = fields.next()?.parse::<u32>().ok()?;
            (ppid == me && fields.any(|arg| arg == "--bridge")).then_some(pid)
        })
        .collect()
}

/// Whether process `pid` exists (a zombie counts: it is not reaped yet).
#[cfg(target_os = "linux")]
pub fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Whether process `pid` exists (a zombie counts: it is not reaped yet).
#[cfg(target_os = "macos")]
pub fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Process ids of this process's child processes running `komponist.exe` (scans included). The
/// test process starts `komponist.exe` for bridges only (`komponist --bridge`), and a Toolhelp
/// snapshot has no command lines, so the image name and the parent id identify them.
#[cfg(windows)]
pub fn bridge_children() -> Vec<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let me = std::process::id();
    let mut pids = Vec::new();
    // SAFETY: the snapshot handle is checked and closed; `entry` has `dwSize` set as the API
    // requires and lives across the calls.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        assert!(snapshot != INVALID_HANDLE_VALUE, "process snapshot");
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let len = entry
                .szExeFile
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(entry.szExeFile.len());
            let image = String::from_utf16_lossy(&entry.szExeFile[..len]);
            if entry.th32ParentProcessID == me && image.eq_ignore_ascii_case("komponist.exe") {
                pids.push(entry.th32ProcessID);
            }
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    pids
}

/// Whether process `pid` is running (an exited process whose handle someone still holds is not).
#[cfg(windows)]
pub fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: the handle is checked, used for one query and closed.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        ok && code == STILL_ACTIVE as u32
    }
}
