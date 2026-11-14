//! Scan mode through the real binary: JSON on stdout, exit codes, no stray output.
//!
//! The installed plugins (`#[ignore]`): `cargo test -p komponist-bridge --test it scan:: -- --ignored`.

use crate::common;

use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::{Command, Output};

use common::*;
use komponist_ipc::{PluginDescriptor, PluginFormat, PluginKind};

/// Runs `komponist-bridge` with `args`.
fn bridge(args: &[&str]) -> Output {
    Command::new(BRIDGE).args(args).output().unwrap()
}

fn scan(bundle: &Path) -> Output {
    Command::new(BRIDGE)
        .arg("--scan")
        .arg(bundle)
        .output()
        .unwrap()
}

/// The descriptors of a successful scan; stdout must be exactly one JSON array and a newline.
fn descriptors(output: &Output) -> Vec<PluginDescriptor> {
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout).unwrap();
    assert!(stdout.ends_with("]\n"), "{stdout:?}");
    assert_eq!(stdout.lines().count(), 1, "{stdout:?}");
    serde_json::from_str(stdout).unwrap()
}

#[test]
fn scan_prints_descriptors_as_json() {
    for (name, kind) in [
        ("kp-test-synth", PluginKind::Instrument),
        ("kp-test-fx", PluginKind::Effect),
        ("kp-test-crash", PluginKind::Effect),
        ("kp-test-slow", PluginKind::Effect),
    ] {
        let Some(bundle) = fixture(name) else {
            continue;
        };
        let output = scan(&bundle);
        let found = descriptors(&output);
        assert_eq!(found.len(), 1, "{found:?}");
        let d = &found[0];
        assert_eq!(d.format, PluginFormat::Clap);
        assert_eq!(d.id, format!("de.klang-studio.{name}"));
        assert_eq!(d.name, name);
        assert_eq!(d.vendor, "klang-studio");
        assert_eq!(d.kind, kind, "{d:?}");
        assert_eq!(d.bundle, bundle);
        assert!(!d.features.is_empty());
        if name == "kp-test-synth" {
            eprintln!(
                "scan {name}: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }
}

#[test]
fn scan_prints_vst3_descriptors_as_json() {
    for (name, id, kind) in [
        ("kp-test-synth", SYNTH_VST3, PluginKind::Instrument),
        ("kp-test-fx", FX_VST3, PluginKind::Effect),
        ("kp-test-crash", CRASH_VST3, PluginKind::Effect),
        ("kp-test-slow", SLOW_VST3, PluginKind::Effect),
    ] {
        let Some(bundle) = fixture_in("vst3", name) else {
            continue;
        };
        let output = scan(&bundle);
        let found = descriptors(&output);
        assert_eq!(found.len(), 1, "{found:?}");
        let d = &found[0];
        assert_eq!(d.format, PluginFormat::Vst3);
        assert_eq!(d.id, id);
        assert_eq!(d.name, name);
        assert_eq!(d.vendor, "klang-studio");
        assert_eq!(d.kind, kind, "{d:?}");
        assert_eq!(d.bundle, bundle);
        assert!(!d.features.is_empty());
        if name == "kp-test-synth" {
            eprintln!(
                "scan {name}.vst3: {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }
}

#[test]
fn scan_failures_have_exit_codes() {
    // Missing bundle, and a file that is no bundle: 1 with the reason on stderr, nothing on stdout.
    for bundle in ["/nonexistent/X.clap", "/etc/hostname"] {
        let output = scan(Path::new(bundle));
        assert_eq!(output.status.code(), Some(1), "{bundle}");
        assert!(output.stdout.is_empty());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(bundle), "{stderr}");
    }
    // A VST3 bundle that is not there: 1 too.
    let output = scan(Path::new("/nonexistent/X.vst3"));
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    // A path the JSON cannot carry.
    let invalid = std::ffi::OsStr::from_bytes(b"/tmp/\xff.clap");
    let output = Command::new(BRIDGE)
        .arg("--scan")
        .arg(invalid)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    // Usage errors: 2.
    for args in [&[][..], &["--scan"], &["--bogus"], &["--instance"]] {
        let output = bridge(args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    }
    let help = bridge(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&help.stdout).contains("--scan <bundle>"));
}

#[test]
#[ignore = "installed third-party plugins; run with --ignored"]
fn scan_installed_plugins() {
    for (path, id) in [
        ("~/.clap/Klopfer.clap", "com.klopfer.drum-sampler"),
        ("/usr/lib/clap/Dexed.clap", "com.digital-suburban.dexed"),
        (
            "/usr/lib/clap/Surge XT.clap",
            "org.surge-synth-team.surge-xt",
        ),
        ("~/.vst3/Klopfer.vst3", "4B6C6F706665724472756D536D706C72"),
        (
            "/usr/lib/vst3/Dexed.vst3",
            "ABCDEF019182FAEB4447534244657864",
        ),
        (
            "/usr/lib/vst3/Surge XT.vst3",
            "ABCDEF019182FAEB566D624153675854",
        ),
        (
            "/usr/local/lib/vst3/Shortcircuit XT.vst3",
            "7B791499693F5465B590BA9435E6D873",
        ),
    ] {
        let Some(bundle) = installed(path) else {
            continue;
        };
        let t0 = std::time::Instant::now();
        let output = scan(&bundle);
        let found = descriptors(&output);
        assert!(found.iter().any(|d| d.id == id), "{found:?}");
        assert!(found.iter().all(|d| d.kind == PluginKind::Instrument));
        eprintln!(
            "scan {path} in {:.0?}: {}",
            t0.elapsed(),
            String::from_utf8_lossy(&output.stdout).trim()
        );
    }
}
