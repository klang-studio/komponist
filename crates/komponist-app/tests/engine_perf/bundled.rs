//! Ignored benchmark launched by scripts/measure-bundled-clap.py from a verified package.

#[path = "bundled/memory.rs"]
mod memory;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_audio::HostKind;
use komponist_model::{Device, DeviceId, DeviceRole, PluginRef, Project};
use komponist_plugins::{DeviceStatus, PluginDirs, PluginManagerEvent, ProcessorSetup};

use super::{
    HEXA_PRESETS, PATIENCE, app_at_buffer, config, measure, project_with_hexa,
    require_audio_config, step_until, worker_counts,
};

#[derive(Clone, Copy, Debug)]
enum Workload {
    ParallelInstruments,
    SerialEffects,
    MidiBeforeInstruments,
}

impl Workload {
    fn selected() -> Vec<Self> {
        match std::env::var("KOMPONIST_PERF_WORKLOAD").as_deref() {
            Ok("instruments") => vec![Self::ParallelInstruments],
            Ok("effects") => vec![Self::SerialEffects],
            Ok("midi") => vec![Self::MidiBeforeInstruments],
            Err(std::env::VarError::NotPresent) => vec![
                Self::ParallelInstruments,
                Self::SerialEffects,
                Self::MidiBeforeInstruments,
            ],
            _ => panic!("KOMPONIST_PERF_WORKLOAD must select instruments, effects or midi"),
        }
    }
}

fn build_project(workload: Workload, host: &PluginHost) -> Project {
    let presets: BTreeMap<_, _> = HEXA_PRESETS
        .into_iter()
        .map(|name| {
            let hexa = host
                .manager()
                .prepare_bundled_device("org.klang-studio.komponist.hexa", Some(name), &[], &[])
                .unwrap_or_else(|error| panic!("benchmark preset {name}: {error}"));
            assert_eq!(hexa.role, DeviceRole::Instrument);
            (
                name,
                (hexa.plugin, hexa.state.expect("factory preset state")),
            )
        })
        .collect();
    let mut workload_project = project_with_hexa(false, |name| presets[name].clone());

    let mut references = BTreeMap::<_, PluginRef>::new();
    let mut install = |project: &mut Project, slug, role| {
        let plugin = references.entry(slug).or_insert_with(|| {
            let id = format!("org.klang-studio.komponist.{slug}");
            let prepared = host
                .manager()
                .prepare_bundled_device(&id, None, &[], &[])
                .unwrap_or_else(|error| panic!("benchmark device {id}: {error}"));
            assert_eq!(prepared.role, role);
            prepared.plugin
        });
        Arc::new(Device {
            id: DeviceId(project.alloc_id().expect("workload device ID")),
            plugin: plugin.clone(),
            role,
            bypass: false,
            parameter_bases: Default::default(),
            asset_links: Default::default(),
            state: None,
            sidechain: None,
        })
    };
    match workload {
        Workload::ParallelInstruments => {}
        Workload::SerialEffects => {
            for index in 0..4 {
                let effects = ["delay", "hall"]
                    .map(|slug| install(&mut workload_project, slug, DeviceRole::Effect));
                Arc::make_mut(&mut workload_project.tracks[index])
                    .channel
                    .devices
                    .extend(effects);
            }
            let master = ["eq", "presse", "limiter"]
                .map(|slug| install(&mut workload_project, slug, DeviceRole::Effect));
            Arc::make_mut(&mut workload_project.master)
                .devices
                .extend(master);
        }
        Workload::MidiBeforeInstruments => {
            for index in 0..8 {
                let midi = ["arpeggiator", "scale"]
                    .map(|slug| install(&mut workload_project, slug, DeviceRole::MidiEffect));
                Arc::make_mut(&mut workload_project.tracks[index])
                    .channel
                    .devices
                    .splice(..0, midi);
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
    let Some(launcher) = std::env::var_os("KOMPONIST_PERF_BUNDLED_APP").map(PathBuf::from) else {
        eprintln!("Bundled CLAP workloads require scripts/measure-bundled-clap.py");
        return;
    };
    let scratch = tempfile::tempdir().expect("benchmark cache directory");
    let package = {
        let mut launch = config(scratch.path());
        launch.bridge_exe = launcher.clone();
        PluginHost::new(launch)
    };

    let audio = audio_host();
    for workload in Workload::selected() {
        let source = build_project(workload, &package);
        let ids: Vec<_> = project_devices(&source).map(|device| device.id).collect();
        for frames in buffer_sizes() {
            for workers in worker_counts() {
                let start = Instant::now();
                // Keep the cache alive through shutdown and lazy verified snapshot reads.
                let cache = tempfile::tempdir_in(scratch.path()).expect("isolated scan cache");
                let host: Box<dyn DeviceHost> = {
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
                    &format!("{workload:?} / {audio:?} / {frames} frames / {workers} workers"),
                    app_at_buffer(source.clone(), host, workers, audio, frames),
                    frames,
                    &ids,
                    start,
                );
            }
        }
    }
}

fn run(label: &str, mut app: komponist_app::App, buffer: u32, ids: &[DeviceId], started: Instant) {
    step_until(&mut app, "all benchmark devices active", |app| {
        ids.iter()
            .all(|&id| app.engine().device_status(id) == DeviceStatus::Ok)
    });
    for &id in ids {
        let host = app.engine().host();
        assert!(
            host.bridge_pid(id).is_none(),
            "bundled device {id:?} must run in-process"
        );
        assert!(host.state_warning(id).is_none(), "state refused for {id:?}");
        assert!(
            host.native_layout(id).is_some() && host.verified_bundle(id).is_some(),
            "missing verified native layout for {id:?}"
        );
    }
    eprintln!(
        "{label}: {} devices ready in {:.3} s (scan and attach)",
        ids.len(),
        started.elapsed().as_secs_f64()
    );
    let config = app.settings().audio.clone();
    measure(label, &mut app, buffer, ids);
    require_audio_config(app.engine(), config.host, config.sample_rate, buffer);
    memory::report(label);
    app.shutdown();
}

fn scan_inventory(host: &mut PluginHost) {
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
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn mib_value(bytes: Option<u64>) -> String {
    bytes.map_or_else(
        || "n/a".to_owned(),
        |bytes| format!("{:.1}", bytes as f64 / 1_048_576.0),
    )
}

/// Start-up cost of the bundled devices, all hosted in-process (D11): the catalog scan with a
/// cold and a warm cache, then the time from `prepare_bundled_device` (library load) through `create_processor` to `DeviceStatus::Ok` and the
/// resident memory added by each device. `KOMPONIST_PERF_STARTUP_DEVICES` is a comma-separated
/// list of device slugs (`hexa,volt`); `all` loads every bundle. The lines starting with
/// `startup:` and `device:` are read by `scripts/measure-bundled-clap.py --startup-memory`, which
/// runs every case in a fresh process.
#[test]
#[ignore = "timing: launch from the verified CLAP package with scripts/measure-bundled-clap.py --startup-memory"]
fn bundled_clap_startup_and_memory() {
    let Some(launcher) = std::env::var_os("KOMPONIST_PERF_BUNDLED_APP").map(PathBuf::from) else {
        eprintln!("Bundled CLAP start-up requires scripts/measure-bundled-clap.py");
        return;
    };
    let selection =
        std::env::var("KOMPONIST_PERF_STARTUP_DEVICES").unwrap_or_else(|_| "all".into());
    let scratch = tempfile::tempdir().expect("benchmark cache directory");
    let cache = scratch.path().join("cache");
    std::fs::create_dir(&cache).unwrap();
    let new_host = || {
        let mut launch = config(&cache);
        launch.bridge_exe = launcher.clone();
        PluginHost::new(launch)
    };

    // Warm-up of the scan cache: a second host on the same cache folder reads it back.
    let started = Instant::now();
    let mut host = new_host();
    scan_inventory(&mut host);
    let scan_cold = started.elapsed();
    drop(host);
    let started = Instant::now();
    let mut host = new_host();
    scan_inventory(&mut host);
    let scan_warm = started.elapsed();

    const SLUGS: [&str; 12] = [
        "hall",
        "delay",
        "eq",
        "presse",
        "limiter",
        "hexa",
        "volt",
        "surf",
        "sampler",
        "general-sampler",
        "arpeggiator",
        "scale",
    ];
    let slugs: Vec<&str> = if selection == "all" {
        SLUGS.to_vec()
    } else {
        selection
            .split(',')
            .map(|slug| {
                *SLUGS
                    .iter()
                    .find(|known| **known == slug.trim())
                    .unwrap_or_else(|| panic!("unknown device {slug}"))
            })
            .collect()
    };
    let setup = ProcessorSetup {
        sample_rate: 48_000.0,
        max_block: 256,
    };
    let baseline = memory::runner_resident();
    let mut processors = Vec::new();
    let mut load_total = Duration::ZERO;
    for (index, slug) in slugs.iter().enumerate() {
        let id = format!("org.klang-studio.komponist.{slug}");
        // `prepare_bundled_device` loads the library (and keeps it loaded), so the per-device
        // interval starts before it; prepare and instance creation are also reported separately.
        let before = memory::runner_resident();
        let started = Instant::now();
        let prepared = host
            .manager()
            .prepare_bundled_device(&id, None, &[], &[])
            .unwrap_or_else(|error| panic!("{id}: {error}"));
        let prepare = started.elapsed();
        let device = Device {
            id: DeviceId(index as u64 + 1),
            plugin: prepared.plugin,
            role: prepared.role,
            bypass: false,
            parameter_bases: Default::default(),
            asset_links: Default::default(),
            state: prepared.state,
            sidechain: None,
        };
        let created = Instant::now();
        let processor = host
            .create_processor(&device, setup)
            .unwrap_or_else(|| panic!("{id} produced no processor"));
        let deadline = Instant::now() + PATIENCE;
        while host.status(device.id) != DeviceStatus::Ok {
            assert!(Instant::now() < deadline, "{id} did not become ready");
            host.poll();
            std::thread::sleep(Duration::from_micros(200));
        }
        let elapsed = started.elapsed();
        assert!(
            host.bridge_pid(device.id).is_none(),
            "{id} must run in-process"
        );
        load_total += elapsed;
        let after = memory::runner_resident();
        let delta = after
            .zip(before)
            .map(|(after, before)| after.saturating_sub(before));
        eprintln!(
            "device: {slug} load_ms={:.2} prepare_ms={:.2} create_ms={:.2} rss_delta_mib={}",
            elapsed.as_secs_f64() * 1e3,
            prepare.as_secs_f64() * 1e3,
            created.elapsed().as_secs_f64() * 1e3,
            mib_value(delta)
        );
        processors.push(processor);
    }
    let after = memory::runner_resident();
    let delta = after
        .zip(baseline)
        .map(|(after, before)| after.saturating_sub(before));
    eprintln!(
        "startup: devices={} scan_cold_ms={:.1} scan_warm_ms={:.1} load_total_ms={:.2} \
         rss_base_mib={} rss_delta_mib={}",
        slugs.len(),
        scan_cold.as_secs_f64() * 1e3,
        scan_warm.as_secs_f64() * 1e3,
        load_total.as_secs_f64() * 1e3,
        mib_value(baseline),
        mib_value(delta),
    );
    drop(processors);
    host.poll();
}
