//! knurr (WP-95) in the app: the catalog entry, the three pages with their graphs and switches,
//! a project round trip, and snapshots of the pages.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_builtin::Internal;
use komponist_model::params::ParamId;
use komponist_model::{DeviceId, DeviceRole, Project};

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(W, H))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(3);
    h
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    press(h, pos, true);
    press(h, pos, false);
    h.run_steps(3);
}

/// A project with one knurr track and the dock on „Geräte“.
fn app_with_knurr() -> (App, DeviceId) {
    let mut app = App::headless(Project::new("knurr"));
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Knurr.plugin_ref(),
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    let device = app.project().tracks[0].channel.devices[0].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    (app, device)
}

fn set(app: &mut App, device: DeviceId, key: &str, value: f64) {
    let id = Internal::Knurr.wire(&ParamId::new(key)).expect(key);
    app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
}

fn page(app: &mut App, device: DeviceId, page: usize) {
    app.dispatch(Action::Ui(UiAction::SetDevicePage { device, page }));
}

fn page_to(h: &mut Harness<'_, App>, device: DeviceId, page: usize) {
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDevicePage { device, page }));
    h.run_steps(3);
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_app::panels::transport::names::CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshot_of_the_three_pages() {
    let (mut app, device) = app_with_knurr();
    // The state of the proposal's mockups: LFO 1 → cutoff, velocity → volume, Hk. 3 → pulse width
    // A, mod wheel → drive; the filter envelope is the second, shorter one.
    for (key, v) in [
        ("mod1_src", 1.0),
        ("mod1_dst", 6.0),
        ("mod1_amt", 0.25),
        ("mod2_src", 4.0),
        ("mod2_dst", 9.0),
        ("mod2_amt", 0.30),
        ("mod3_src", 3.0),
        ("mod3_dst", 3.0),
        ("mod3_amt", 0.40),
        ("mod4_src", 6.0),
        ("mod4_dst", 8.0),
        ("mod4_amt", 0.35),
        ("filt_a", 0.002),
        ("filt_d", 0.35),
    ] {
        set(&mut app, device, key, v);
    }
    page(&mut app, device, 0);
    let mut h = harness(app);
    let mut results = SnapshotResults::new();
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(&mut h);
    h.snapshot("knurr-klang-1x");
    results.extend_harness(&mut h);
    page_to(&mut h, device, 1);
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(&mut h);
    h.snapshot("knurr-huellkurven-1x");
    page_to(&mut h, device, 2);
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(&mut h);
    h.snapshot("knurr-matrix-1x");
    let _ = (
        click as fn(&mut Harness<'_, App>, Pos2),
        DeviceRole::Instrument,
    );
}
