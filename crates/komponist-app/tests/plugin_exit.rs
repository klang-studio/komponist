//! App exit leaves no bridge process (PLAN WP-46 "Done when"): `App::shutdown` (window closed)
//! ends every bridge gracefully, dropping the app kills them. Checked by pid: this binary holds
//! one test only, so every `komponist-bridge` child of the test process belongs to it.

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;

use std::time::{Duration, Instant};

use common::*;
use komponist_app::App;
use komponist_app::device_host::PluginHost;

/// The demo with test plugins, scanned and loaded: every bridge running.
fn loaded_app(name: &str) -> Option<App> {
    let dirs = fixture_dirs()?;
    let mut app = app_with(
        demo_with_test_plugins(),
        PluginHost::new(config(&scratch(name))),
    );
    app.engine_mut().host_mut().start_scan(&dirs);
    step_until(&mut app, "every device Ok", all_ok);
    Some(app)
}

#[test]
fn shutdown_and_drop_leave_no_bridge_process() {
    let Some(mut app) = loaded_app("exit-shutdown") else {
        return;
    };
    let bridged = bridged(app.project()).len();
    let pids = bridge_children();
    assert_eq!(
        pids.len(),
        bridged,
        "one bridge per bridged device: {pids:?}"
    );

    // Window closed: `Shutdown` to every bridge; they exit by themselves within the grace time.
    let t0 = Instant::now();
    app.shutdown();
    eprintln!("shutdown of {} bridges: {:.0?}", pids.len(), t0.elapsed());
    for &pid in &pids {
        assert!(!alive(pid), "bridge {pid} survived the shutdown");
    }
    assert!(bridge_children().is_empty());
    drop(app);

    // Dropped without shutdown (a panic, a test): SIGKILL from the plugin manager's drop.
    let app = loaded_app("exit-drop").unwrap();
    let pids = bridge_children();
    assert_eq!(pids.len(), bridged, "{pids:?}");
    drop(app);
    for &pid in &pids {
        assert!(!alive(pid), "bridge {pid} survived the drop");
    }
    assert!(bridge_children().is_empty());

    // Dropped while the bridges are still loading and a scan runs.
    let dirs = fixture_dirs().unwrap();
    let mut app = app_with(
        demo_with_test_plugins(),
        PluginHost::new(config(&scratch("exit-loading"))),
    );
    app.engine_mut().host_mut().start_scan(&dirs);
    let deadline = Instant::now() + PATIENCE;
    while bridge_children().is_empty() {
        assert!(
            Instant::now() < deadline,
            "no scan or bridge process started"
        );
        app.step();
        std::thread::sleep(Duration::from_millis(1));
    }
    let pids = bridge_children();
    drop(app);
    for &pid in &pids {
        assert!(!alive(pid), "process {pid} survived the drop");
    }
    assert!(bridge_children().is_empty());
}
