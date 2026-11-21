//! Ignored benchmark launched by scripts/measure-bundled-clap.py from a verified package.

#[path = "bundled/memory.rs"]
mod memory;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_app::device_host::{BuiltinHost, DeviceHost, PluginHost};
use komponist_audio::HostKind;
use komponist_builtin::Internal;
use komponist_model::{Device, DeviceId, Project};
use komponist_plugins::bundled_clap::BundledClap;
use komponist_plugins::legacy_bundled::{
    migrate_legacy_bundled_project, plan_legacy_bundled_device,
};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent};

use super::{
    PATIENCE, app_at_buffer, config, measure, project, require_audio_config, step_until,
    worker_counts,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Legacy,
    Direct,
    Bridge,
}

impl Mode {
    fn from_environment() -> Option<Self> {
        match std::env::var("KOMPONIST_PERF_BUNDLED_MODE").as_deref() {
            Ok("legacy") => Some(Self::Legacy),
            Ok("direct") => Some(Self::Direct),
            Ok("bridge") => Some(Self::Bridge),
            Err(std::env::VarError::NotPresent) => None,
            _ => panic!("launch via scripts/measure-bundled-clap.py"),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Workload {
    ParallelInstruments,
    SerialEffects,
    MidiBeforeInstruments,
}

fn install(project: &mut Project, kind: Internal) -> Arc<Device> {
    let id = DeviceId(project.alloc_id().expect("workload device ID"));
    Arc::new(Device {
        id,
        plugin: kind.plugin_ref(),
        role: kind.role(),
        bypass: false,
        state: None,
        sidechain: None,
    })
}

fn build_project(workload: Workload) -> Project {
    let mut workload_project = project(false);
    match workload {
        Workload::ParallelInstruments => {}
        Workload::SerialEffects => {
            for index in 0..4 {
                let delay = install(&mut workload_project, Internal::Delay);
                let hall = install(&mut workload_project, Internal::Hall);
                Arc::make_mut(&mut workload_project.tracks[index])
                    .channel
                    .devices
                    .extend([delay, hall]);
            }
            let master = [Internal::Eq, Internal::Presse, Internal::Limiter]
                .map(|kind| install(&mut workload_project, kind));
            Arc::make_mut(&mut workload_project.master)
                .devices
                .extend(master);
        }
        Workload::MidiBeforeInstruments => {
            for index in 0..8 {
                let arpeggiator = install(&mut workload_project, Internal::Arpeggiator);
                let scale = install(&mut workload_project, Internal::Tonleiter);
                Arc::make_mut(&mut workload_project.tracks[index])
                    .channel
                    .devices
                    .splice(..0, [arpeggiator, scale]);
            }
        }
    }
    workload_project
}

fn project_devices(project: &Project) -> impl Iterator<Item = &Arc<Device>> {
    project
        .tracks
        .iter()
        .flat_map(|track| &track.channel.devices)
        .chain(&project.master.devices)
}

fn migrate(project: &Project, inventory: &BundledClap) -> Project {
    let plans = project_devices(project)
        .map(|device| {
            plan_legacy_bundled_device(inventory, device, None)
                .unwrap_or_else(|error| panic!("{}: {error}", device.plugin.id))
        })
        .collect::<Vec<_>>();
    migrate_legacy_bundled_project(project, plans)
        .expect("translate the complete benchmark project")
        .project
}

fn audio_host() -> HostKind {
    let setting = std::env::var("KOMPONIST_PERF_AUDIO_HOST").unwrap_or_else(|_| "null".into());
    let host: HostKind = serde_json::from_value(serde_json::Value::String(setting.clone()))
        .unwrap_or_else(|error| panic!("invalid KOMPONIST_PERF_AUDIO_HOST {setting}: {error}"));
    assert!(
        host.is_supported_here(),
        "{host:?} is unavailable in this build"
    );
    host
}

fn buffer_sizes() -> Vec<u32> {
    let value = std::env::var("KOMPONIST_PERF_BUFFERS").unwrap_or_else(|_| "64,128,256".into());
    value
        .split(',')
        .map(|size| match size.trim().parse::<u32>() {
            Ok(size @ (64 | 128 | 256)) => size,
            _ => panic!("KOMPONIST_PERF_BUFFERS must list 64, 128 or 256 frame periods"),
        })
        .collect()
}

#[test]
#[ignore = "timing: launch from the verified CLAP package with scripts/measure-bundled-clap.py"]
fn bundled_clap_workloads() {
    let Some(mode) = Mode::from_environment() else {
        eprintln!("Bundled CLAP workloads require scripts/measure-bundled-clap.py");
        return;
    };
    let launcher = std::env::var_os("KOMPONIST_PERF_BUNDLED_APP")
        .map(PathBuf::from)
        .expect("packaged bridge launcher");
    let scratch = tempfile::tempdir().expect("benchmark cache directory");
    let inventory = match mode {
        Mode::Legacy => None,
        _ => Some(
            BundledClap::discover(scratch.path())
                .expect("trusted benchmark inventory")
                .expect("runner must contain the app's authenticated manifest hash"),
        ),
    };

    let audio = audio_host();
    for workload in [
        Workload::ParallelInstruments,
        Workload::SerialEffects,
        Workload::MidiBeforeInstruments,
    ] {
        let source = build_project(workload);
        let source = match &inventory {
            Some(inventory) => migrate(&source, inventory),
            None => source,
        };
        let ids: Vec<_> = project_devices(&source).map(|device| device.id).collect();
        for frames in buffer_sizes() {
            for workers in worker_counts() {
                let start = Instant::now();
                // Keep the cache alive through shutdown and lazy verified snapshot reads.
                let cache = tempfile::tempdir_in(scratch.path()).expect("isolated scan cache");
                let host: Box<dyn DeviceHost> = if mode == Mode::Legacy {
                    Box::new(BuiltinHost::new())
                } else {
                    let mut config = config(cache.path());
                    config.bridge_exe = launcher.clone();
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
                        assert!(
                            Instant::now() < deadline,
                            "timed out scanning the bundled inventory"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Box::new(host)
                };
                run(
                    &format!(
                        "{mode:?} {workload:?} / {audio:?} / {frames} frames / {workers} workers"
                    ),
                    mode,
                    app_at_buffer(source.clone(), host, workers, audio, frames),
                    frames,
                    &ids,
                    start,
                );
            }
        }
    }
}

fn run(
    label: &str,
    mode: Mode,
    mut app: komponist_app::App,
    buffer: u32,
    ids: &[DeviceId],
    started: Instant,
) {
    step_until(&mut app, "all benchmark devices active", |app| {
        ids.iter()
            .all(|&id| app.engine().device_status(id) == DeviceStatus::Ok)
    });
    for &id in ids {
        let host = app.engine().host();
        assert_eq!(
            host.bridge_pid(id).is_some(),
            mode == Mode::Bridge,
            "wrong route for {id:?}"
        );
        assert!(host.state_warning(id).is_none(), "state refused for {id:?}");
        if mode != Mode::Legacy {
            assert!(
                host.native_layout(id).is_some(),
                "missing native layout for {id:?}"
            );
        }
    }
    eprintln!(
        "{label}: {} devices ready in {:.3} s (scan and attach)",
        ids.len(),
        started.elapsed().as_secs_f64()
    );
    let config = app.settings().audio.clone();
    measure(label, &mut app, buffer, ids);
    require_audio_config(app.engine(), config.host, config.sample_rate, buffer);
    let host = app.engine().host();
    memory::report(label, ids.iter().filter_map(|&id| host.bridge_pid(id)));
    app.shutdown();
}
