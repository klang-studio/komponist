//! Acceptance flow 4, UI part (HANDOFF §9): load a plugin (double-click in the browser, or drag
//! it onto a track) and ask for its GUI. The real bridge and kp-test-synth / kp-test-fx (WP-27,
//! `scripts/build-test-plugins.sh`; skipped with a note without them).
//!
//! The plugin's own window is a real X11 window and only exists in the real event loop:
//! `acceptance_flow4_gui.rs` (Xvfb, `scripts/check-gui.sh`) continues there.

mod acceptance_support;
mod common;

use acceptance_support::*;
use common::{app_with, config, fixture_dirs, scratch, step_until};
use egui::accesskit::Role;
use egui_kittest::kittest::Queryable as _;
use komponist_app::device_host::PluginHost;
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, UiAction};
use komponist_model::DeviceRole;
use komponist_model::fixtures::empty_project;
use komponist_plugins::DeviceStatus;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[test]
fn a_plugin_loads_by_double_click_and_by_drag_and_offers_its_gui() {
    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("flow4");
    let mut app = app_with(empty_project(), PluginHost::new(config(&dir)));
    app.engine_mut().host_mut().start_scan(&dirs);
    step_until(&mut app, "the scan", |a| {
        !a.engine().host().is_scanning() && a.engine().host().last_scan().is_some()
    });
    let mut h = harness(app);
    let row = |h: &egui_kittest::Harness<'_, komponist_app::App>, name: &str| {
        h.get_all_by_role_and_label(Role::ListItem, name)
            .next()
            .unwrap_or_else(|| panic!("no browser row {name}"))
            .rect()
    };

    // Double-click on the instrument: a new track, the plugin loads in its bridge.
    let at = row(&h, "kp-test-synth").center();
    double_click(&mut h, at);
    let p = h.state().project();
    assert_eq!(p.tracks.len(), 1);
    let synth = p.tracks[0].channel.devices[0].clone();
    assert_eq!(synth.plugin.name, "kp-test-synth");
    assert_eq!(synth.role, DeviceRole::Instrument);

    // Drag the effect onto the track header: it joins the track's chain.
    let header = rect_of(&h, Role::Row, "Spur ");
    let from = row(&h, "kp-test-fx").center();
    drag(&mut h, from, header.center());
    let p = h.state().project();
    assert_eq!(p.tracks.len(), 1, "an effect makes no track");
    let devices = &p.tracks[0].channel.devices;
    assert_eq!(devices.len(), 2);
    assert_eq!(devices[1].plugin.name, "kp-test-fx");
    assert_eq!(devices[1].role, DeviceRole::Effect);

    // Both load in their bridges.
    let (a, b) = (synth.id, devices[1].id);
    let until = std::time::Instant::now() + common::PATIENCE;
    while ![a, b]
        .iter()
        .all(|&d| h.state().engine().device_status(d) == DeviceStatus::Ok)
    {
        assert!(std::time::Instant::now() < until, "bridges did not load");
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // "Öffnen" on the synth's card asks for its window.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    h.run_steps(3);
    let open = h
        .get_all_by_role_and_label(Role::Button, "Öffnen")
        .next()
        .expect("Öffnen on a device card");
    let at = open.rect().center();
    click_at(&mut h, at);
    assert!(
        h.state()
            .ui()
            .plugin_windows
            .iter()
            .any(|w| w.device == synth.id),
        "the plugin window is requested"
    );
}
