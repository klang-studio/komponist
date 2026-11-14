//! Plugin frames as real X11 windows with the WP-27 test plugins (WP-50): kp-test-synth as CLAP
//! (open, fixed size, Space from the focused plugin, close, reopen) and as VST3 (bridge killed →
//! "Abgestürzt" without the child → "Neu laden"), kp-test-crash in the parameter view crashing
//! from its knob.
//!
//! `#[ignore]`, and only with `KP_GUI_TESTS=1` under Xvfb (`frame_gui` module docs):
//!
//! ```text
//! scripts/build-test-plugins.sh
//! KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!     cargo test -p komponist-app --test plugin_frame_gui -- --ignored --nocapture
//! ```

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;
mod frame_gui;

use std::time::Duration;

use common::{CRASH, SYNTH, app_with, config, scratch};
use frame_gui::*;
use komponist_app::Action;
use komponist_app::actions::PluginAction;
use komponist_app::device_host::PluginHost;
use komponist_app::ui_state::PluginView;
use komponist_model::PluginFormat;
use komponist_model::fixtures::empty_project;
use komponist_plugins::{DeviceStatus, PluginDirs};

fn fixture_dirs() -> Option<PluginDirs> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-plugins");
    if !root.join("clap/kp-test-synth.clap").exists() {
        eprintln!("skipped: run scripts/build-test-plugins.sh");
        return None;
    }
    Some(PluginDirs {
        clap: vec![std::fs::canonicalize(root.join("clap")).unwrap()],
        vst3: vec![std::fs::canonicalize(root.join("vst3")).unwrap()],
    })
}

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1 (module docs)"]
fn test_plugins_in_real_frames() {
    if !gui_allowed() {
        return;
    }
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("frame-gui");
    let mut app = app_with(empty_project(), PluginHost::new(config(&dir)));
    app.engine_mut().host_mut().start_scan(&dirs);

    let mut steps = vec![
        add("synth-clap", |p| {
            p.id == SYNTH && p.format == PluginFormat::Clap
        }),
        add("synth-vst3", |p| {
            p.name == "kp-test-synth" && p.format == PluginFormat::Vst3
        }),
        add("crash", |p| p.id == CRASH && p.format == PluginFormat::Clap),
        loaded("synth-clap"),
        loaded("synth-vst3"),
        loaded("crash"),
    ];

    // kp-test-synth CLAP: fixed size (240 × 80), narrower than the header.
    steps.push(open("synth-clap"));
    steps.push(gui_shown("synth-clap"));
    steps.extend(shot("kp-test-synth-clap-open"));
    steps.extend(fixed_size_checks("synth-clap"));
    steps.extend(shot("kp-test-synth-clap-forced-resize"));
    steps.extend(space_reaches_komponist("synth-clap", None));
    steps.extend(closed("synth-clap"));
    steps.push(open("synth-clap"));
    steps.push(gui_shown("synth-clap"));
    steps.extend(shot("kp-test-synth-clap-reopened"));
    steps.extend(closed("synth-clap"));

    // kp-test-synth VST3: the bridge dies, the frame shows it, "Neu laden" brings the GUI back.
    steps.push(open("synth-vst3"));
    steps.push(gui_shown("synth-vst3"));
    steps.extend(shot("kp-test-synth-vst3-open"));
    steps.extend(crash_and_reload("synth-vst3", "kp-test-synth-vst3"));
    steps.extend(closed("synth-vst3"));

    // kp-test-crash has no GUI: the parameter view, then its "crash" knob kills it.
    steps.push(open("crash"));
    steps.push(until("kp-test-crash parameter view", |w| {
        let device = w.device("crash");
        let params = w
            .app
            .ui()
            .plugin_windows
            .iter()
            .any(|win| win.device == device && win.view == PluginView::Params);
        let f = w.frame("crash");
        params && f.is_some_and(|f| f.window.is_some() && f.child.is_none())
    }));
    steps.extend(shot("kp-test-crash-params"));
    steps.push(once("kp-test-crash: crash knob to 1", |w| {
        let device = w.device("crash");
        let id = w
            .app
            .engine()
            .host()
            .params(device)
            .and_then(|p| p.iter().find(|p| p.name.to_lowercase().contains("crash")))
            .map(|p| p.id)
            .expect("crash parameter");
        w.app.dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id,
            value: 1.0,
        }));
    }));
    steps.push(until("kp-test-crash crashed", |w| {
        matches!(w.status("crash"), DeviceStatus::Crashed(_))
    }));
    steps.extend(shot("kp-test-crash-crashed"));
    steps.push(wait(Duration::from_millis(200)));

    let outcome = run(app, steps);
    eprintln!("--- notes");
    for n in &outcome.notes {
        eprintln!("{n}");
    }
    assert!(outcome.finished, "{:?}", outcome.failure);
}
