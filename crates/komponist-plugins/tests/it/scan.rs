//! Scanning through the real bridge binary (and a fake one for crashes and hangs): parallel scan
//! processes, the report, the catalog and the cache (PLAN WP-45).

use crate::common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use common::*;
use komponist_ipc::PluginKind;
use komponist_plugins::{PluginDirs, PluginManager, PluginManagerEvent, ScanCache, ScanReport};

/// Copies the file or folder `from` into the folder `to` (`cp -R`, without a `cp` on Windows).
fn copy(from: &Path, to: &Path) {
    let target = to.join(from.file_name().unwrap());
    if from.is_dir() {
        std::fs::create_dir_all(&target).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            copy(&entry.unwrap().path(), &target);
        }
    } else {
        std::fs::copy(from, &target).unwrap();
    }
}

/// A file name that is not valid UTF-8: a byte 0xFF on Unix, an unpaired surrogate on Windows.
fn bad_file_name() -> std::ffi::OsString {
    #[cfg(unix)]
    return std::os::unix::ffi::OsStringExt::from_vec(b"\xff.clap".to_vec());
    #[cfg(windows)]
    return std::os::windows::ffi::OsStringExt::from_wide(&[0xD800, 0x2E, 0x63, 0x6C, 0x61, 0x70]);
}

fn finish_scan(manager: &mut PluginManager) -> ScanReport {
    wait_for(manager, "ScanFinished", |e| match e {
        PluginManagerEvent::ScanFinished(report) => Some(report.clone()),
        _ => None,
    })
    .0
}

/// The catalog's names: hexa and the scanned plugins. The internal devices (WP-85, WP-88) are
/// built-ins like hexa but not what these tests are about, so they are left out.
fn names(manager: &PluginManager) -> Vec<String> {
    manager
        .catalog()
        .entries()
        .iter()
        .filter(|e| !e.is_builtin() || e.plugin.name == "Hexa")
        .map(|e| e.plugin.name.clone())
        .collect()
}

fn reason(report: &ScanReport, file: impl AsRef<std::ffi::OsStr>) -> &str {
    let file = file.as_ref();
    report
        .skipped
        .iter()
        .find(|s| s.path.file_name() == Some(file))
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
    // A name that is not valid UTF-8. APFS (and HFS+) refuse to create one (EILSEQ, "Illegal byte
    // sequence"), so on macOS the case cannot occur on disk and the test leaves it out.
    let bad_name = !cfg!(target_os = "macos");
    if bad_name {
        std::fs::write(clap.join(bad_file_name()), b"bad name").unwrap();
    }
    // An empty VST3 folder: VST3 scanning itself is covered by the hosting and bridge tests (WP-32).
    // Putting the VST3 fixture here made the counts depend on whether its bundle was built.
    let vst3 = dir.join("vst3");
    std::fs::create_dir_all(&vst3).unwrap();
    let dirs = PluginDirs {
        clap: vec![clap.clone()],
        vst3: vec![vst3.clone(), dir.join("missing")],
    };

    let mut manager = PluginManager::new(config(&dir));
    // The built-in entries (hexa and the internal devices of WP-85) come before any scan.
    let builtins = names(&manager);
    assert_eq!(builtins[0], "Hexa");
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
    if bad_name {
        assert_eq!(reason(&report, bad_file_name()), "path is not valid UTF-8");
    }
    assert_eq!(
        report.skipped.len(),
        1 + usize::from(bad_name),
        "{report:?}"
    );
    assert!(report.finished <= SystemTime::now());

    let mut expected = builtins.clone();
    expected.extend(
        [
            "kp-test-crash",
            "kp-test-fx",
            "kp-test-slow",
            "kp-test-synth",
        ]
        .map(String::from),
    );
    assert_eq!(names(&manager), expected);
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
    assert!(
        cache
            .bundles
            .iter()
            .any(|b| b.path == canonical(&clap.join("vendor/kp-test-slow.clap")))
    );
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
    // (A macOS bundle is a folder: its binary changes, as when the plugin is updated.)
    let changed = clap.join("kp-test-fx.clap");
    let changed = if changed.is_dir() {
        changed.join("Contents/MacOS/kp-test-fx")
    } else {
        changed
    };
    let file = std::fs::File::options().write(true).open(changed).unwrap();
    file.set_modified(SystemTime::now() + Duration::from_secs(10))
        .unwrap();
    // Closed before the scan: Windows maps no DLL that is open for writing.
    drop(file);
    manager.start_scan(&dirs);
    finish_scan(&mut manager);
    assert!(names(&manager).contains(&"kp-test-fx".to_owned()));
    assert!(!names(&manager).contains(&"Cached FX".to_owned()));
}

/// Child processes of this test process with command name `comm` (macOS has no `/proc`: asks `ps`).
#[cfg(target_os = "macos")]
fn children_named(comm: &str) -> Vec<u32> {
    let me = std::process::id();
    let out = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let ppid = fields.next()?.parse::<u32>().ok()?;
            let name = fields.next()?.rsplit('/').next()?;
            (ppid == me && name == comm).then_some(pid)
        })
        .collect()
}

/// Child processes of this test process with command name `comm`.
#[cfg(target_os = "linux")]
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

/// A stand-in for the bridge's scan mode in `dir` (SIGTERM as the crash: no core dump): the
/// launcher and its leading arguments.
#[cfg(unix)]
fn fake_bridge(dir: &Path) -> (PathBuf, Vec<std::ffi::OsString>) {
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
    (fake, Vec::new())
}

/// Windows: the stand-in is a PowerShell script (the crash: the exit code of an access
/// violation). A hanging scan leaves `hang-<pid>.pid` in `dir`, for [`hanging_scans`].
#[cfg(windows)]
fn fake_bridge(dir: &Path) -> (PathBuf, Vec<std::ffi::OsString>) {
    let script = dir.join("fake-bridge.ps1");
    std::fs::write(
        &script,
        "$bundle = $args[$args.Count - 1]\r\n\
         if ($bundle -like '*Crash*') { exit -1073741819 }\r\n\
         if ($bundle -like '*Hang*') {\r\n\
         \x20 Set-Content -Path (Join-Path $PSScriptRoot \"hang-$PID.pid\") -Value $PID\r\n\
         \x20 Start-Sleep -Seconds 60\r\n\
         \x20 exit 0\r\n\
         }\r\n\
         if ($bundle -like '*Fail*') {\r\n\
         \x20 [Console]::Error.WriteLine('komponist-bridge: no clap_entry')\r\n\
         \x20 exit 1\r\n\
         }\r\n\
         [Console]::Out.WriteLine('[]')\r\n\
         exit 0\r\n",
    )
    .unwrap();
    let args = [
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ]
    .into_iter()
    .map(std::ffi::OsString::from)
    .chain([script.into_os_string()])
    .collect();
    (PathBuf::from("powershell.exe"), args)
}

/// Hanging scan processes still alive.
#[cfg(unix)]
fn hanging_scans(_dir: &Path) -> usize {
    children_named("sleep").len()
}

/// Windows: the hanging scans whose process is still alive (by their pid files).
#[cfg(windows)]
fn hanging_scans(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("hang-")?
                .strip_suffix(".pid")?
                .parse::<u32>()
                .ok()
        })
        .filter(|&pid| {
            komponist_ipc::ProcessWatcher::new(pid)
                .is_ok_and(|mut w| w.try_exit().is_ok_and(|exit| exit.is_none()))
        })
        .count()
}

/// What a crashing scan reads in the report: the signal, on Windows the exception code.
#[cfg(unix)]
const CRASHED: &str = "crashed (signal 15)";
#[cfg(windows)]
const CRASHED: &str = "crashed (exception 0xC0000005)";

#[test]
fn crashing_hanging_and_empty_bundles_are_skipped() {
    let dir = scratch("fake-scan");
    let (fake, fake_args) = fake_bridge(&dir);
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
    config.bridge_exe = fake;
    config.bridge_args = fake_args;

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
    assert_eq!(reason(&report, "Crash.clap"), CRASHED);
    assert_eq!(reason(&report, "Hang.clap"), "timed out after 10 s");
    assert_eq!(reason(&report, "Fail.clap"), "no clap_entry");
    assert_eq!(reason(&report, "Empty.clap"), "no plugins in the bundle");
    assert_eq!(hanging_scans(&dir), 0, "the hanging scan was killed");
    // Timeouts are not cached; the others are.
    let cache = ScanCache::load(&config.cache_file);
    let cached: Vec<String> = cache
        .bundles
        .iter()
        .map(|b| b.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(cached, ["Crash.clap", "Empty.clap", "Fail.clap"]);

    // A restarted scan and a dropped manager kill their scan processes.
    {
        manager.start_scan(&dirs);
        wait_until(|| hanging_scans(&dir) > 0);
        manager.start_scan(&dirs);
        wait_until(|| hanging_scans(&dir) == 1);
        let t0 = Instant::now();
        drop(manager);
        assert!(t0.elapsed() < Duration::from_secs(2));
        assert_eq!(hanging_scans(&dir), 0);
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !condition() {
        assert!(Instant::now() < deadline, "condition not reached");
        std::thread::sleep(Duration::from_millis(10));
    }
}
