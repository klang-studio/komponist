//! #218: a bridge ends with the app, even while its main thread is stuck in a plugin call and so
//! never sees its control socket close.
//!
//! The "app" is this test binary run again in a process of its own (only this test, with
//! [`APP_ENV`] set): it loads kp-test-crash with `KP_TEST_CRASH_HANG`, so the bridge hangs in the
//! plugin's `activate` for 30 s after writing its pid to a file. The test kills the app with
//! SIGKILL (Windows: `TerminateProcess`) and expects the bridge gone well before the hang ends.
//! It identifies the bridge by that pid and counts no child processes, so it can share the `it`
//! binary with tests that start bridges in parallel.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use komponist_model::DeviceRole;

use crate::common::*;

/// Set in the "app" process: the folder it works in.
const APP_ENV: &str = "KOMPONIST_TEST_ORPHAN_APP";
/// kp-test-crash hangs in `activate` and writes its pid to this file (its README).
const HANG_ENV: &str = "KP_TEST_CRASH_HANG";
/// This test's name, for running it alone in the "app" process.
const NAME: &str = "orphans::a_bridge_stuck_in_a_plugin_call_ends_with_a_killed_app";
/// How long the bridge may take to end after the app was killed; kp-test-crash hangs for 30 s.
const GRACE: Duration = Duration::from_secs(10);

#[test]
fn a_bridge_stuck_in_a_plugin_call_ends_with_a_killed_app() {
    if let Some(dir) = std::env::var_os(APP_ENV) {
        return app(Path::new(&dir));
    }
    if fixture_dir().is_none() {
        return;
    }
    let dir = scratch("orphan-app");
    let marker = dir.join("bridge.pid");
    let child = Command::new(std::env::current_exe().unwrap())
        .args([NAME, "--exact", "--test-threads=1", "--nocapture"])
        .env(APP_ENV, &dir)
        .env(HANG_ENV, &marker)
        .stdin(Stdio::null())
        .spawn()
        .expect("start the app process");
    let mut app = AppGuard(child);

    let bridge = BridgeGuard(wait_for_pid(&marker, &mut app.0));
    assert!(running(bridge.0), "bridge {} is not running", bridge.0);
    // Stuck in `activate`: the app dies without a word to it.
    app.0.kill().expect("kill the app");
    app.0.wait().expect("reap the app");

    let t0 = Instant::now();
    while running(bridge.0) {
        assert!(
            t0.elapsed() < GRACE,
            "bridge {} outlived the killed app by {GRACE:?}",
            bridge.0
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    eprintln!("bridge ended {:.0?} after the app", t0.elapsed());
}

/// The "app": loads the hanging device and polls until it is killed (or gives up after a while,
/// dropping the manager, which kills the bridge).
fn app(dir: &Path) {
    let Some(mut manager) = scanned_manager(dir) else {
        return;
    };
    let hanging = device(10, DeviceRole::Effect, clap_plugin(CRASH), None);
    let _processor = manager.create_processor(&hanging, SETUP).expect("start");
    let deadline = Instant::now() + 2 * PATIENCE;
    while Instant::now() < deadline {
        manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits for kp-test-crash to name its process (the bridge, in `activate`).
fn wait_for_pid(marker: &Path, app: &mut Child) -> u32 {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(pid) = std::fs::read_to_string(marker)
            .ok()
            .and_then(|s| s.trim().parse().ok())
        {
            return pid;
        }
        if let Some(status) = app.try_wait().unwrap() {
            panic!("the app process ended before its bridge hung: {status}");
        }
        assert!(Instant::now() < deadline, "the bridge never hung");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Kills and reaps the app process, also when the test fails.
struct AppGuard(Child);

impl Drop for AppGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Kills the bridge if it still runs when the test ends (a failure): it is not our child, so
/// nobody else would before its hang ends.
struct BridgeGuard(u32);

impl Drop for BridgeGuard {
    fn drop(&mut self) {
        if running(self.0) {
            kill(self.0);
        }
    }
}

/// Whether `pid` still runs: not a zombie (the reparented bridge may wait for an init that does
/// not reap, a container's).
#[cfg(target_os = "linux")]
fn running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    // "pid (comm) state …"
    let state = stat
        .rfind(')')
        .and_then(|i| stat[i + 1..].trim_start().chars().next());
    state.is_some_and(|s| s != 'Z' && s != 'X')
}

/// Whether `pid` still runs: not a zombie.
#[cfg(target_os = "macos")]
fn running(pid: u32) -> bool {
    let out = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .expect("run ps");
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Whether `pid` still runs (an exited process whose handle someone holds does not).
#[cfg(windows)]
fn running(pid: u32) -> bool {
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

#[cfg(unix)]
fn kill(pid: u32) {
    if let Some(pid) = rustix::process::Pid::from_raw(pid as i32) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}

#[cfg(windows)]
fn kill(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
    // SAFETY: the handle is checked, used once and closed.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !handle.is_null() {
            TerminateProcess(handle, 137);
            CloseHandle(handle);
        }
    }
}
