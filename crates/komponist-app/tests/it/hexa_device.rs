//! hexa's device layout (WP-86) in the app: the card with its two pages, the operator
//! selection (`{env_op}`), the carrier fill, a knob turn reaching the document, the presets, and
//! snapshots of both pages.

use std::path::PathBuf;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_i18n::Lang;
use komponist_model::DeviceId;
use komponist_plugins::DeviceStatus;

use crate::common::{author_wire_value, instrument_project, step_until};

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

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    press(h, from, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, false);
    h.run_steps(3);
}

fn app_with_hexa(preset: &str) -> (App, DeviceId) {
    app_with_hexa_edits(preset, &[])
}

fn app_with_hexa_edits(preset: &str, edits: &[(&str, f64)]) -> (App, DeviceId) {
    let host = App::headless_plugin_host();
    let prepared = host
        .manager()
        .prepare_bundled_device("org.klang-studio.komponist.hexa", Some(preset), edits, &[])
        .unwrap();
    let (mut project, track, device) = instrument_project("Synth", prepared.plugin.clone());
    Arc::make_mut(&mut project.tracks[0]).channel.devices[0] =
        Arc::new(prepared.into_device(device));
    let mut app = App::headless(project);
    step_until(&mut app, "Hexa's native layout", |app| {
        let host = app.engine().host();
        host.status(device) == DeviceStatus::Ok && host.native_layout(device).is_some()
    });
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    (app, device)
}

fn parameter(app: &App, device: DeviceId, key: &str) -> u32 {
    app.engine()
        .host()
        .native_layout(device)
        .unwrap()
        .author_ids()
        .unwrap()[key]
}

fn parameter_value(app: &App, device: DeviceId, id: u32) -> f64 {
    app.engine().host().param_value(device, id).unwrap().value
}

fn page(app: &mut App, device: DeviceId, page: usize) {
    app.dispatch(Action::Ui(UiAction::SetDevicePage { device, page }));
}

fn card<'h>(h: &'h Harness<'_, App>) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::Group, CARD)
}

/// The accessible name of the hexa card.
const CARD: &str = "Gerät Hexa";

/// Whether the column button of operator `n` is the selected one.
fn op_selected(h: &Harness<'_, App>, n: usize) -> bool {
    let name = Lang::De.devices_operator_n(n);
    card(h)
        .get_by_role_and_label(Role::Button, &name)
        .accesskit_node()
        .is_selected()
        .unwrap_or(false)
}

fn has(h: &Harness<'_, App>, role: Role, label: &str) -> bool {
    card(h).query_by_role_and_label(role, label).is_some()
}

fn slider_value(h: &Harness<'_, App>, label: &str) -> String {
    card(h)
        .get_by_role_and_label(Role::Slider, label)
        .value()
        .unwrap_or_default()
}

#[test]
fn the_card_has_three_authored_pages_and_the_preset_shows_in_the_head() {
    let (app, _) = app_with_hexa("E-Piano");
    let h = harness(app);
    let names: Vec<_> = card(&h)
        .query_all_by_role(Role::RadioButton)
        .map(|node| node.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(names, ["Operatoren", "Hüllkurven", "Global"]);
    // The factory preset is still recognised from the state: its name is on the dropdown.
    card(&h).get_by_role_and_label(Role::Button, &Lang::De.devices_preset_name("E-Piano"));
}

#[test]
fn clicking_an_operator_column_selects_it_and_the_envelope_page_follows() {
    let (mut app, device) = app_with_hexa("E-Piano");
    page(&mut app, device, 0);
    let mut h = harness(app);
    // Operator 1 is selected at the start; its column is a selected button.
    assert!(op_selected(&h, 1));
    // Click the empty top of the third operator's column (above the ratio knob's ring).
    let third = card(&h)
        .get_by_role_and_label(Role::Slider, "Ratio 3")
        .rect();
    click(&mut h, third.center());
    assert!(op_selected(&h, 3));
    assert!(!op_selected(&h, 1));
    // The envelope page edits operator 3's envelope.
    page_to(&mut h, device, 1);
    for name in [
        "Attack 3",
        "Decay 3",
        "Sustain 3",
        "Release 3",
        "Key Scale 3",
    ] {
        assert!(has(&h, Role::Slider, name), "{name}");
    }
    assert!(!has(&h, Role::Slider, "Attack 1"));
    // The op switch on that page does the same: pick operator 5.
    let five = card(&h)
        .get_by_role_and_label(Role::RadioButton, "Operator: 5")
        .rect();
    click(&mut h, five.center());
    assert!(has(&h, Role::Slider, "Attack 5"));
    assert!(!has(&h, Role::Slider, "Attack 3"));
    // Back on the first page the fifth column is the selected one.
    page_to(&mut h, device, 0);
    assert!(op_selected(&h, 5));
}

fn page_to(h: &mut Harness<'_, App>, device: DeviceId, index: usize) {
    h.state_mut().dispatch(Action::Ui(UiAction::SetDevicePage {
        device,
        page: index,
    }));
    h.run_steps(3);
}

#[test]
fn a_knob_change_reaches_the_engine_and_the_document_state() {
    let (mut app, device) = app_with_hexa("E-Piano");
    page(&mut app, device, 0);
    let mut h = harness(app);
    let ratio = parameter(h.state(), device, "op2_ratio");
    let before = parameter_value(h.state(), device, ratio);
    let original = h
        .state()
        .project()
        .find_device(device)
        .unwrap()
        .2
        .state
        .clone();
    let text_before = slider_value(&h, "Ratio 2");
    let knob = card(&h)
        .get_by_role_and_label(Role::Slider, "Ratio 2")
        .rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    step_until(h.state_mut(), "Hexa's edited CLAP state", |app| {
        let state = &app.project().find_device(device).unwrap().2.state;
        *state != original && parameter_value(app, device, ratio) > before
    });
    h.run_steps(2);
    let after = parameter_value(h.state(), device, ratio);
    // The engine got the same value under the card parameter's wire id.
    let sent = h
        .state()
        .dispatched()
        .filter_map(|a| match a {
            Action::Plugin(PluginAction::SetParam {
                device: d,
                id,
                value,
            }) if *d == device && *id == ratio => Some(*value),
            _ => None,
        })
        .last()
        .expect("SetParam for op2_ratio");
    assert!((sent - after).abs() < 1e-4, "{sent} vs {after}");
    assert_ne!(slider_value(&h, "Ratio 2"), text_before);
}

#[test]
fn carriers_fill_in_the_track_colour_and_modulators_in_text_2() {
    // The layout asks `op{n}_output` per fader; the Algorithmus macro sets it from the table.
    let (mut app, device) = app_with_hexa("Init");
    let algorithm = parameter(&app, device, "algorithm");
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device,
        id: algorithm,
        value: 1.0,
    }));
    step_until(&mut app, "Hexa's authored carrier routing", |app| {
        app.engine()
            .host()
            .device_display(device)
            .and_then(|display| display.fm_topologies.first())
            .is_some_and(|routing| routing.algorithm == Some(1) && routing.outputs == 0b000101)
    });
}

/// Masks the CPU readout, which changes from run to run; the rectangle is looked up right before
/// the snapshot.
fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshot_of_both_pages() {
    // Set all three values as part of the CLAP state used to open the fixture.
    let (defaults, device) = app_with_hexa("E-Piano");
    let edits = [
        ("op2_attack", 0.08),
        ("op2_decay", 1.2),
        ("op2_sustain", 0.4),
    ]
    .map(|(key, plain)| {
        let id = parameter(&defaults, device, key);
        (key, author_wire_value(&defaults, device, id, plain))
    });
    drop(defaults);
    let (mut app, device) = app_with_hexa_edits("E-Piano", &edits);
    page(&mut app, device, 0);
    let mut h = harness(app);
    h.run_steps(2);
    let mut results = SnapshotResults::new();
    let second = card(&h)
        .get_by_role_and_label(Role::Slider, "Ratio 2")
        .rect();
    click(&mut h, second.center());
    // Pointer away and settled: no hover or animation in the picture.
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(&mut h);
    h.snapshot("devices-hexa-operators-1x");
    results.extend_harness(&mut h);
    page_to(&mut h, device, 1);
    h.run_steps(30);
    assert_eq!(slider_value(&h, "Attack 2"), "80 ms");
    mask_cpu(&mut h);
    h.snapshot("devices-hexa-envelopes-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
