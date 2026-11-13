//! Scanning through the real bridge binary (and a fake one for crashes and hangs): parallel scan
//! processes, the report, the catalog and the cache (PLAN WP-45).

mod common;

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use common::*;
use komponist_ipc::PluginKind;
use komponist_plugins::{PluginDirs, PluginManager, PluginManagerEvent, ScanCache, ScanReport};

fn copy(from: &Path, to: &Path) {
    let status = std::process::Command::new("cp")
        .arg("-R")
        .arg(from)
        .arg(to)
        .status()
        .unwrap();
    assert!(status.success());
}

fn finish_scan(manager: &mut PluginManager) -> ScanReport {
    wait_for(manager, "ScanFinished", |e| match e {
        PluginManagerEvent::ScanFinished(report) => Some(report.clone()),
        _ => None,
    })
    .0
}

fn names(manager: &PluginManager) -> Vec<String> {
    manager
        .catalog()
        .entries()
        .iter()
        .map(|e| e.plugin.name.clone())
        .collect()
}

fn reason(report: &ScanReport, file: impl AsRef<[u8]>) -> &str {
    let file = file.as_ref();
    report
        .skipped
        .iter()
        .find(|s| s.path.file_name().map(|n| n.as_bytes()) == Some(file))
        .map(|s| s.reason.as_str())
        .unwrap_or_else(|| panic!("{file:?} not skipped: {:?}", report.skipped))
}

#[test]
fn scan_finds_the_fixtures_and_reuses_the_cache() {
    let Some(fixtures) = fixture_dir() else {
        return;
    };
    let dir = scratch("scan");
    let clap = dir.join("clap");
    std::fs::create_dir_all(clap.join("vendor")).unwrap();
    for name in ["kp-test-synth", "kp-test-fx", "kp-test-crash"] {
        copy(&fixtures.join(format!("{name}.clap")), &clap);
    }
    // One level down, as vendors install them.
    copy(&fixtures.join("kp-test-slow.clap"), &clap.join("vendor"));
    std::fs::write(clap.join("Broken.clap"), b"not a shared object").unwrap();
    std::fs::write(
        clap.join(std::ffi::OsStr::from_bytes(b"\xff.clap")),
        b"bad name",
    )
    .unwrap();
    // An empty VST3 folder: VST3 scanning itself is covered by the hosting and bridge tests (WP-32).
    // Putting the VST3 fixture here made the counts depend on whether its bundle was built.
    let vst3 = dir.join("vst3");
    std::fs::create_dir_all(&vst3).unwrap();
    let dirs = PluginDirs {
        clap: vec![clap.clone()],
        vst3: vec![vst3.clone(), dir.join("missing")],
    };

    let mut manager = PluginManager::new(config(&dir));
    assert_eq!(names(&manager), ["pisynth"]);
    assert!(!manager.is_scanning());
    let t0 = Instant::now();
    manager.start_scan(&dirs);
    assert!(manager.is_scanning());
    let report = finish_scan(&mut manager);
    eprintln!("scan of 6 bundles in {:.1?}: {report:?}", t0.elapsed());
    assert!(!manager.is_scanning());
    assert_eq!(manager.last_scan(), Some(&report));

    assert_eq!(report.found, 4);
    let counts: Vec<(PathBuf, usize)> = report
        .per_dir
        .iter()
        .map(|d| (d.path.clone(), d.plugins))
        .collect();
    assert_eq!(
        counts,
        [
            (clap.clone(), 4),
            (vst3.clone(), 0),
            (dir.join("missing"), 0)
        ]
    );
    assert!(!reason(&report, "Broken.clap").is_empty());
    assert_eq!(reason(&report, b"\xff.clap"), "path is not valid UTF-8");
    assert_eq!(report.skipped.len(), 2, "{report:?}");
    assert!(report.finished <= SystemTime::now());

    assert_eq!(
        names(&manager),
        [
            "pisynth",
            "kp-test-crash",
            "kp-test-fx",
            "kp-test-slow",
            "kp-test-synth"
        ]
    );
    let catalog = manager.catalog();
    let synth = catalog.resolve(&clap_plugin(SYNTH)).unwrap();
    assert_eq!(synth.kind, PluginKind::Instrument);
    assert_eq!(synth.category, None);
    let fx = catalog.resolve(&clap_plugin(FX)).unwrap();
    assert_eq!(fx.kind, PluginKind::Effect);
    assert!(fx.category.is_some());
    eprintln!("kp-test-fx features {:?} → {:?}", fx.features, fx.category);
    assert_eq!(catalog.search("kp-test").len(), 4);

    // The cache holds the scanned bundles and the broken one, keyed by canonical path.
    let cache_file = dir.join("cache/plugins.json");
    let mut cache = ScanCache::load(&cache_file);
    assert_eq!(cache.bundles.len(), 5, "{cache:?}");
    assert!(cache.bundles.iter().any(|b| {
        b.path
            == clap
                .join("vendor/kp-test-slow.clap")
                .canonicalize()
                .unwrap()
    }));
    assert!(cache.bundles.iter().all(|b| b.path.is_absolute()));
    let broken = cache
        .bundles
        .iter()
        .find(|b| b.path.ends_with("Broken.clap"))
        .unwrap();
    assert!(broken.error.is_some() && broken.descriptors.is_empty());

    // A doctored cache entry proves the cache is used: by a new manager without scanning…
    let fx_entry = cache
        .bundles
        .iter_mut()
        .find(|b| b.path.ends_with("kp-test-fx.clap"))
        .unwrap();
    fx_entry.descriptors[0].name = "Cached FX".into();
    cache.save(&cache_file).unwrap();
    let mut manager = PluginManager::new(config(&dir));
    assert!(names(&manager).contains(&"Cached FX".to_owned()));
    assert!(!manager.is_scanning());
    // …and by a rescan, as long as the bundle is unchanged.
    manager.start_scan(&dirs);
    let again = finish_scan(&mut manager);
    assert_eq!(again.found, 4);
    assert!(names(&manager).contains(&"Cached FX".to_owned()));
    // A changed bundle is scanned again.
    let file = std::fs::File::options()
        .write(true)
        .open(clap.join("kp-test-fx.clap"))
        .unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    manager.start_scan(&dirs);
    finish_scan(&mut manager);
    assert!(names(&manager).contains(&"kp-test-fx".to_owned()));
    assert!(!names(&manager).contains(&"Cached FX".to_owned()));
}

/// Child processes of this test process with command name `comm`.
fn children_named(comm: &str) -> Vec<u32> {
    let mut found = Vec::new();
    let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
        return found;
    };
    for task in tasks.flatten() {
        let children = std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
        for pid in children
            .split_whitespace()
            .filter_map(|p| p.parse::<u32>().ok())
        {
            let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            if name.trim() == comm {
                found.push(pid);
            }
        }
    }
    found
}

#[test]
fn crashing_hanging_and_empty_bundles_are_skipped() {
    let dir = scratch("fake-scan");
    // A stand-in for the bridge's scan mode (SIGTERM as the crash: no core dump).
    let fake = dir.join("fake-bridge");
    std::fs::write(
        &fake,
        "#!/bin/sh\n\
         case \"$2\" in\n\
         *Crash*) kill -TERM $$ ;;\n\
         *Hang*) exec sleep 60 ;;\n\
         *Fail*) echo 'komponist-bridge: no clap_entry' >&2; exit 1 ;;\n\
         *) echo '[]'; exit 0 ;;\n\
         esac\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let plugins = dir.join("clap");
    std::fs::create_dir_all(&plugins).unwrap();
    for name in ["Crash", "Hang", "Fail", "Empty"] {
        std::fs::write(plugins.join(format!("{name}.clap")), b"x").unwrap();
    }
    let dirs = PluginDirs {
        clap: vec![plugins.clone()],
        vst3: Vec::new(),
    };
    let mut config = config(&dir);
    config.bridge_exe = fake.clone();

    let mut manager = PluginManager::new(config.clone());
    let t0 = Instant::now();
    manager.start_scan(&dirs);
    let report = finish_scan(&mut manager);
    let took = t0.elapsed();
    eprintln!("fake scan in {took:.1?}: {report:?}");
    assert!(
        took >= Duration::from_secs(10),
        "the hang ran into the timeout"
    );
    assert!(took < Duration::from_secs(25));
    assert_eq!(report.found, 0);
    assert_eq!(reason(&report, "Crash.clap"), "crashed (signal 15)");
    assert_eq!(reason(&report, "Hang.clap"), "timed out after 10 s");
    assert_eq!(reason(&report, "Fail.clap"), "no clap_entry");
    assert_eq!(reason(&report, "Empty.clap"), "no plugins in the bundle");
    assert!(
        children_named("sleep").is_empty(),
        "the hanging scan was killed"
    );
    // Timeouts are not cached; the others are.
    let cache = ScanCache::load(&config.cache_file);
    let cached: Vec<String> = cache
        .bundles
        .iter()
        .map(|b| b.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(cached, ["Crash.clap", "Empty.clap", "Fail.clap"]);

    // A restarted scan and a dropped manager kill their scan processes.
    manager.start_scan(&dirs);
    wait_until(|| !children_named("sleep").is_empty());
    manager.start_scan(&dirs);
    wait_until(|| children_named("sleep").len() == 1);
    let t0 = Instant::now();
    drop(manager);
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert!(children_named("sleep").is_empty());
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached");
        std::thread::sleep(Duration::from_millis(10));
    }
}
