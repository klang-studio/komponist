#![cfg(target_os = "linux")]
//! Acceptance flow 4, GUI part (HANDOFF §9): the loaded plugin's own window opens as a real X11
//! frame, shows the plugin's GUI at its size, and closes again. The loading by double-click and
//! drag is `acceptance_flow4.rs`; here the same `PluginAction::Add` the browser sends loads
//! kp-test-synth, then the real event loop opens its window.
//!
//! `#[ignore]`, and only with `KP_GUI_TESTS=1` under Xvfb: `scripts/check-gui.sh`.

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;
mod frame_gui;

use common::{SYNTH, app_with, config, scratch};
use frame_gui::*;
use komponist_app::device_host::PluginHost;
use komponist_model::PluginFormat;
use komponist_model::fixtures::empty_project;
use komponist_plugins::PluginDirs;

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1 (scripts/check-gui.sh)"]
fn flow_4_the_plugin_gui_opens_in_a_real_frame() {
    if !gui_allowed() {
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-plugins")
        .join(std::env::consts::OS);
    if !root.join("clap/kp-test-synth.clap").exists() {
        eprintln!("skipped: run scripts/build-test-plugins.sh");
        return;
    }
    let dirs = PluginDirs {
        clap: vec![std::fs::canonicalize(root.join("clap")).unwrap()],
        vst3: Vec::new(),
    };
    let dir = scratch("flow4-gui");
    let mut app = app_with(
        empty_project(),
        PluginHost::with_reference_devices(config(&dir)),
    );
    open_scratch_project(&mut app, &dir);
    app.engine_mut().host_mut().start_scan(&dirs);

    let mut steps = vec![
        add("synth", |p| p.id == SYNTH && p.format == PluginFormat::Clap),
        loaded("synth"),
        open("synth"),
        gui_shown("synth"),
    ];
    steps.extend(shot("flow4-gui-open"));
    steps.extend(fixed_size_checks("synth"));
    steps.extend(closed("synth"));

    let outcome = run(app, steps);
    for n in &outcome.notes {
        eprintln!("{n}");
    }
    assert!(outcome.finished, "{:?}", outcome.failure);
}
