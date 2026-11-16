//! `komponist --bridge` (WP-94): the app's own executable is the plugin bridge. Runs the built
//! `komponist` binary as a child process, the way the plugin manager starts it. The process name
//! (`komponist-bridg`, found by `plugin_exit` through `/proc/<pid>/comm`) and instance mode are
//! covered by the plugin tests, which start every bridge this way.

use std::process::{Command, Stdio};

use crate::common::target_dir;

const KOMPONIST: &str = env!("CARGO_BIN_EXE_komponist");

#[test]
fn scan_mode_prints_the_descriptors_of_a_bundle() {
    let bundle = target_dir()
        .join("test-plugins")
        .join(std::env::consts::OS)
        .join("clap/kp-test-synth.clap");
    if !bundle.exists() {
        eprintln!(
            "skipped: {} is missing; run scripts/build-test-plugins.sh",
            bundle.display()
        );
        return;
    }
    let out = Command::new(KOMPONIST)
        .args(["--bridge", "--scan"])
        .arg(&bundle)
        .stdin(Stdio::null())
        .output()
        .expect("run komponist --bridge");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
    let plugins = json.as_array().expect("an array");
    assert_eq!(plugins.len(), 1);
    assert_eq!(plugins[0]["id"], "de.klang-studio.kp-test-synth");
}

#[test]
fn scan_mode_exit_codes_of_the_bridge_are_kept() {
    // A bundle that does not exist: the scan fails (exit 1) with the reason on stderr.
    let out = Command::new(KOMPONIST)
        .args(["--bridge", "--scan", "/nonexistent/none.clap"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("komponist-bridge:"));
    // Bad arguments: usage error (exit 2), and no app window or settings are touched.
    let out = Command::new(KOMPONIST)
        .args(["--bridge", "--bogus"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage: komponist-bridge"));
}
