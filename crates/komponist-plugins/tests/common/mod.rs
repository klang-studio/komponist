//! Shared harness of the integration tests: the real `komponist-bridge` binary, the WP-27 test
//! plugins, projects with external devices, and a rig that compiles a project, creates the
//! processors through the `PluginManager` and drives the engine with every `Engine::process` /
//! `Engine::render` call inside `assert_no_alloc`.
//!
//! The bridge binary: `$KOMPONIST_BRIDGE_EXE` if set, else `target/<profile>/komponist-bridge`
//! (built by `cargo test --workspace` for the bridge's own tests). If that is missing or older than
//! the sources of komponist-bridge, komponist-ipc or komponist-hosting, it is built with cargo first.

#![allow(dead_code)]

#[allow(dead_code)]
#[path = "../../../../scripts/test-fixtures.rs"]
mod test_fixtures;

use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use assert_no_alloc::assert_no_alloc;
use komponist_engine::compile::{self, CompileEnv};
use komponist_engine::{
    Command as EngineCommand, Engine, EngineConfig, EngineEvent, EngineHandle, LiveInput, MidiPort,
    Program, SlotId,
};
use komponist_model::project::BUILTIN_KLOPFER;
use komponist_model::{
    Channel, Device, DeviceId, DeviceRole, Hue, MidiChannelSel, MidiInputSel, PluginFormat,
    PluginRef, Project, Track, TrackId, TrackKind,
};
use komponist_plugins::{
    DeviceStatus, PluginDirs, PluginManager, PluginManagerConfig, PluginManagerEvent,
    ProcessorSetup,
};

pub const RATE: f32 = 48_000.0;
/// Frames per engine call in the tests.
pub const BLOCK: usize = 256;
/// How long loading, scans and answers may take on a loaded machine.
pub const PATIENCE: Duration = Duration::from_secs(30);

pub const SYNTH: &str = "de.klang-studio.kp-test-synth";
pub const FX: &str = "de.klang-studio.kp-test-fx";
pub const CRASH: &str = "de.klang-studio.kp-test-crash";
pub const SLOW: &str = "de.klang-studio.kp-test-slow";

pub const SETUP: ProcessorSetup = ProcessorSetup {
    sample_rate: RATE,
    max_block: komponist_engine::MAX_BLOCK,
};

/// How a null-pointer write in a plugin ends its bridge: SIGSEGV, or on Windows the access
/// violation `0xC000_0005` (komponist-ipc reports an NTSTATUS crash as the signal).
#[cfg(unix)]
pub const SEGV: i32 = 11;
#[cfg(windows)]
pub const SEGV: i32 = 0xC000_0005_u32 as i32;

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
        // target/<profile>/deps/<test> → target/<profile>/komponist-bridge; Cargo's newer layout
        // puts tests deeper (target/<profile>/build/<package>/<hash>/<test>), so the profile
        // folder is the nearest `debug` or `release` above the test.
        let test = std::env::current_exe().expect("test executable");
        let profile_dir = test
            .ancestors()
            .skip(1)
            .find(|dir| {
                dir.file_name()
                    .is_some_and(|n| n == "debug" || n == "release")
            })
            .or_else(|| test.parent().and_then(Path::parent))
            .expect("profile dir");
        let exe = profile_dir.join(format!("komponist-bridge{}", std::env::consts::EXE_SUFFIX));
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

/// The canonical form of `path` as the scan keys bundles by it: on Windows without the verbatim
/// prefix (`\\?\C:\…`) that `fs::canonicalize` adds.
pub fn canonical(path: &Path) -> PathBuf {
    let path = std::fs::canonicalize(path).unwrap();
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(rest) if cfg!(windows) && rest.as_bytes().get(1) == Some(&b':') => rest.into(),
        _ => path,
    }
}

/// The folder of the WP-27 CLAP fixtures, or `None` with a note if they are not built.
pub fn fixture_dir() -> Option<PathBuf> {
    let dir = target_dir()
        .join("test-plugins")
        .join(std::env::consts::OS)
        .join("clap");
    if test_fixtures::bundle_dir(&dir, "clap") {
        Some(canonical(&dir))
    } else {
        None
    }
}

/// A fresh scratch folder for one test.
pub fn scratch(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("komponist-plugins-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A manager with its own cache file in `dir`.
pub fn config(dir: &Path) -> PluginManagerConfig {
    PluginManagerConfig {
        bridge_exe: bridge_exe(),
        bridge_args: Vec::new(),
        cache_file: dir.join("cache/plugins.json"),
        parallel_scans: 4,
    }
}

/// Runs `poll` until `pick` accepts an event (the others are returned in order before it).
pub fn wait_for<T>(
    manager: &mut PluginManager,
    what: &str,
    mut pick: impl FnMut(&PluginManagerEvent) -> Option<T>,
) -> (T, Vec<PluginManagerEvent>) {
    let deadline = Instant::now() + PATIENCE;
    let mut seen = Vec::new();
    loop {
        for event in manager.poll() {
            if let Some(found) = pick(&event) {
                return (found, seen);
            }
            seen.push(event);
        }
        assert!(Instant::now() < deadline, "no {what}; seen {seen:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A manager whose catalog holds the scanned fixtures.
pub fn scanned_manager(dir: &Path) -> Option<PluginManager> {
    let fixtures = fixture_dir()?;
    let mut manager = PluginManager::new(config(dir));
    manager.start_scan(&PluginDirs {
        clap: vec![fixtures],
        vst3: Vec::new(),
    });
    wait_for(&mut manager, "ScanFinished", |e| {
        matches!(e, PluginManagerEvent::ScanFinished(_)).then_some(())
    });
    Some(manager)
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

/// The reference a project from before the sampler (WP-87) holds for klopfer ("INTERN").
pub fn klopfer() -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: BUILTIN_KLOPFER.into(),
        name: "klopfer".into(),
        vendor: "Klopfer".into(),
        version: "0.1.0".into(),
        module: Box::default(),
    }
}

pub fn device(id: u64, role: DeviceRole, plugin: PluginRef, state: Option<&[u8]>) -> Device {
    Device {
        id: DeviceId(id),
        role,
        plugin,
        bypass: false,
        state: state.map(Arc::from),
        sidechain: None,
    }
}

/// A MIDI track `id` with `devices`.
pub fn track(id: u64, devices: Vec<Device>) -> Track {
    Track {
        id: TrackId(id),
        name: format!("Spur {id}"),
        hue: Hue::BLUE,
        kind: TrackKind::Midi,
        channel: Channel {
            devices: devices.into_iter().map(Arc::new).collect(),
            ..Channel::default()
        },
        arm: false,
        monitor: false,
        input: MidiInputSel::All,
        midi_channel: MidiChannelSel::Omni,
        audio: komponist_model::AudioIo::default(),
        height: 56,
        clips: Vec::new(),
        output: komponist_model::TrackOutput::Master,
        sends: Vec::new(),
        folder: false,
        folded: false,
    }
}

/// A project with `tracks` at 120 bpm.
pub fn project(tracks: Vec<Track>) -> Project {
    let mut p = Project::new("Test");
    p.next_id = 1000;
    p.tracks = tracks.into_iter().map(Arc::new).collect();
    p
}

/// Device latencies from the plugin manager, as the app's `EngineSync` passes them.
pub struct Env<'a>(pub &'a PluginManager);

impl CompileEnv for Env<'_> {
    fn midi_port(&self, _device_name: &str) -> Option<MidiPort> {
        None
    }

    fn device_latency(&self, device: DeviceId) -> u32 {
        self.0.latency(device)
    }
}

/// An engine playing a project whose external devices run in bridges.
pub struct Rig {
    pub manager: PluginManager,
    pub engine: Box<Engine>,
    pub handle: EngineHandle,
    pub project: Project,
    pub program: Program,
    /// The slot of each device.
    pub slots: BTreeMap<DeviceId, SlotId>,
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    /// Engine events except garbage.
    pub engine_events: Vec<EngineEvent>,
    /// Plugin manager events.
    pub events: Vec<PluginManagerEvent>,
}

impl Rig {
    /// Compiles `project`, creates a processor per device and installs everything.
    pub fn new(mut manager: PluginManager, project: Project) -> Rig {
        let (engine, mut handle) = Engine::new(EngineConfig {
            sample_rate: RATE,
            max_block: komponist_engine::MAX_BLOCK,
            seed: 1,
        });
        let program = compile::build(&project, None, &Env(&manager));
        let mut slots = BTreeMap::new();
        for (device, slot) in compile::slot_changes(None, &program).added {
            slots.insert(device, slot);
            let model = find_device(&project, device);
            match manager.create_processor(model, SETUP) {
                Ok(processor) => handle
                    .send(EngineCommand::InsertProcessor { slot, processor })
                    .expect("command queue"),
                Err(e) => eprintln!("device {}: {e}", device.0),
            }
        }
        handle
            .send(EngineCommand::SetProgram(Box::new(program.clone())))
            .expect("command queue");
        Rig {
            manager,
            engine,
            handle,
            project,
            program,
            slots,
            l: Vec::new(),
            r: Vec::new(),
            engine_events: Vec::new(),
            events: Vec::new(),
        }
    }

    pub fn device(&self, id: u64) -> &Device {
        find_device(&self.project, DeviceId(id))
    }

    pub fn send(&mut self, command: EngineCommand) {
        self.handle.send(command).expect("command queue");
    }

    /// A live note on `track` at the next block.
    pub fn note(&mut self, track: u64, key: u8, velocity: u8) {
        self.send(EngineCommand::LiveNote {
            track: TrackId(track),
            key,
            velocity,
        });
    }

    /// Recompiles after the latencies changed (delay compensation), as `EngineSync` does.
    pub fn recompile(&mut self) {
        let next = compile::build(&self.project, Some(&self.program), &Env(&self.manager));
        self.program = next.clone();
        self.send(EngineCommand::SetProgram(Box::new(next)));
    }

    /// Polls the engine and the manager (UI-thread work between callbacks).
    pub fn poll(&mut self) {
        while let Some(event) = self.handle.poll() {
            if !matches!(event, EngineEvent::Retired(_)) {
                self.engine_events.push(event);
            }
        }
        self.events.extend(self.manager.poll());
    }

    /// One realtime callback of `frames`, inside `assert_no_alloc`; returns its output range.
    pub fn process(&mut self, frames: usize) -> Range<usize> {
        let start = self.l.len();
        self.l.resize(start + frames, f32::NAN);
        self.r.resize(start + frames, f32::NAN);
        let (l, r) = (&mut self.l[start..], &mut self.r[start..]);
        let engine = &mut self.engine;
        let input = LiveInput {
            midi: &[],
            output_latency: 0,
        };
        assert_no_alloc(|| engine.process(l, r, &input));
        self.poll();
        start..self.l.len()
    }

    /// `blocks` realtime callbacks of [`BLOCK`] frames; returns the output range.
    pub fn run(&mut self, blocks: usize) -> Range<usize> {
        let start = self.l.len();
        for _ in 0..blocks {
            self.process(BLOCK);
        }
        start..self.l.len()
    }

    /// Offline render of `frames` in calls of [`BLOCK`] (no timing deadline), inside
    /// `assert_no_alloc`; returns the output range.
    pub fn render(&mut self, frames: usize) -> Range<usize> {
        let start = self.l.len();
        let mut done = 0;
        self.l.resize(start + frames, f32::NAN);
        self.r.resize(start + frames, f32::NAN);
        while done < frames {
            let n = BLOCK.min(frames - done);
            let engine = &mut self.engine;
            let at = start + done;
            let (l, r) = (&mut self.l[at..at + n], &mut self.r[at..at + n]);
            assert_no_alloc(|| {
                let out = engine.render(n);
                for (i, frame) in out.as_chunks::<2>().0.iter().enumerate() {
                    l[i] = frame[0];
                    r[i] = frame[1];
                }
            });
            done += n;
            self.poll();
        }
        start..self.l.len()
    }

    /// Polls (and keeps the engine running in realtime blocks) until `device` has `status`.
    pub fn wait_status(&mut self, device: u64, status: DeviceStatus) {
        let deadline = Instant::now() + PATIENCE;
        while self.manager.status(DeviceId(device)) != status {
            assert!(
                Instant::now() < deadline,
                "device {device}: {:?}, expected {status:?}; events {:?}",
                self.manager.status(DeviceId(device)),
                self.events
            );
            self.process(BLOCK);
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Peak of both channels over `range`.
    pub fn peak(&self, range: Range<usize>) -> f32 {
        self.l[range.clone()]
            .iter()
            .chain(&self.r[range])
            .fold(0.0f32, |m, s| m.max(s.abs()))
    }

    /// Whether the engine reported `ProcessorFailed` for the slot of `device`.
    pub fn failed(&self, device: u64) -> bool {
        let slot = self.slots[&DeviceId(device)];
        self.engine_events
            .iter()
            .any(|e| matches!(e, EngineEvent::ProcessorFailed { slot: s } if *s == slot))
    }

    /// "Neu laden" as the app does it: a new processor into the same slot.
    pub fn reload(&mut self, device: u64) {
        let model = find_device(&self.project, DeviceId(device)).clone();
        let processor = self.manager.reload(&model, SETUP).expect("reload");
        let slot = self.slots[&DeviceId(device)];
        self.send(EngineCommand::InsertProcessor { slot, processor });
    }
}

pub fn find_device(project: &Project, id: DeviceId) -> &Device {
    project
        .tracks
        .iter()
        .flat_map(|t| t.channel.devices.iter())
        .chain(project.master.devices.iter())
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("no device {}", id.0))
}
