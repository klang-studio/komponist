//! Plugin frames as real X11 windows with the plugins installed on this machine (WP-50):
//! Klopfer (fixed size, Space from the focused plugin, bridge killed → "Neu laden", close and
//! reopen), Surge XT as CLAP and VST3 (resizable: user resizes the frame, the plugin adjusts, the
//! frame follows), Dexed (fixed size, parameter view and back), and two frames at once.
//!
//! `#[ignore]`, and only with `KP_GUI_TESTS=1` under Xvfb (`frame_gui` module docs):
//!
//! ```text
//! KP_GUI_TESTS=1 xvfb-run -a -s "-screen 0 1920x1080x24" \
//!     cargo test -p komponist-app --test plugin_frame_installed -- --ignored --nocapture
//! ```

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;
mod frame_gui;

use std::time::Duration;

use common::{app_with, config, scratch};
use frame_gui::*;
use komponist_app::device_host::PluginHost;
use komponist_model::PluginFormat;
use komponist_model::fixtures::empty_project;
use komponist_plugins::{PluginDirs, PluginManagerConfig};

#[test]
#[ignore = "GUI: run under Xvfb with KP_GUI_TESTS=1; needs Klopfer, Surge XT and Dexed"]
fn installed_plugins_in_real_frames() {
    if !gui_allowed() {
        return;
    }
    let dir = scratch("frame-installed");
    let Some(klopfer) = PluginManagerConfig::defaults().klopfer_bundle else {
        panic!("Klopfer.clap not found (scripts/build-klopfer.sh)");
    };
    let config = PluginManagerConfig {
        klopfer_bundle: Some(klopfer),
        ..config(&dir)
    };
    let mut app = app_with(empty_project(), PluginHost::new(config));
    open_scratch_project(&mut app, &dir);
    app.engine_mut()
        .host_mut()
        .start_scan(&PluginDirs::defaults());

    let mut steps = vec![
        add("klopfer", |p| {
            p.format == PluginFormat::Builtin && p.name == "klopfer"
        }),
        add("surge-clap", |p| {
            p.format == PluginFormat::Clap && p.name == "Surge XT"
        }),
        add("surge-vst3", |p| {
            p.format == PluginFormat::Vst3 && p.name == "Surge XT"
        }),
        add("dexed", |p| {
            p.format == PluginFormat::Clap && p.name == "Dexed"
        }),
        loaded("klopfer"),
        loaded("surge-clap"),
        loaded("surge-vst3"),
        loaded("dexed"),
    ];

    // Klopfer: fixed size; a click on pad 1 gives it the focus.
    steps.push(open("klopfer"));
    steps.push(gui_shown("klopfer"));
    steps.extend(shot("klopfer-open"));
    steps.extend(fixed_size_checks("klopfer"));
    steps.extend(space_reaches_komponist("klopfer", Some((444, 150))));
    steps.extend(crash_and_reload("klopfer", "klopfer"));
    steps.extend(closed("klopfer"));
    steps.push(open("klopfer"));
    steps.push(gui_shown("klopfer"));
    steps.extend(closed("klopfer"));

    // Surge XT CLAP: grows and shrinks with the frame (aspect ratio kept by Surge).
    steps.push(open("surge-clap"));
    steps.push(gui_shown("surge-clap"));
    steps.extend(shot("surge-clap-open"));
    steps.extend(user_resize("surge-clap", 240, 160));
    steps.extend(shot("surge-clap-grown"));
    steps.extend(user_resize("surge-clap", -400, -260));
    steps.extend(shot("surge-clap-shrunk"));
    steps.extend(space_reaches_komponist("surge-clap", None));
    steps.extend(closed("surge-clap"));

    // Surge XT VST3.
    steps.push(open("surge-vst3"));
    steps.push(gui_shown("surge-vst3"));
    steps.extend(shot("surge-vst3-open"));
    steps.extend(user_resize("surge-vst3", 200, 120));
    steps.extend(shot("surge-vst3-resized"));
    steps.extend(closed("surge-vst3"));

    // Dexed: fixed size.
    steps.push(open("dexed"));
    steps.push(gui_shown("dexed"));
    steps.extend(shot("dexed-open"));
    steps.extend(fixed_size_checks("dexed"));
    steps.extend(view_switch("dexed", "dexed"));
    steps.extend(closed("dexed"));

    // Two frames at once.
    steps.push(open("dexed"));
    steps.push(open("klopfer"));
    steps.push(gui_shown("dexed"));
    steps.push(gui_shown("klopfer"));
    steps.extend(shot("two-frames"));
    steps.extend(closed("klopfer"));
    steps.extend(closed("dexed"));
    steps.push(wait(Duration::from_millis(200)));

    let outcome = run(app, steps);
    eprintln!("--- notes");
    for n in &outcome.notes {
        eprintln!("{n}");
    }
    assert!(outcome.finished, "{:?}", outcome.failure);
}
