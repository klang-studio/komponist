//! volt (WP-95) in the app: the catalog entry, the three pages with their graphs and switches,
//! a project round trip, and snapshots of the pages.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_builtin::Internal;
use komponist_i18n::Lang;
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

/// A project with one volt track and the dock on „Geräte“.
fn app_with_volt() -> (App, DeviceId) {
    let mut app = App::headless(Project::new("Volt"));
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Volt.plugin_ref(),
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
    let id = Internal::Volt.wire(&ParamId::new(key)).expect(key);
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
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshot_of_the_three_pages() {
    let (mut app, device) = app_with_volt();
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
    h.snapshot("volt-klang-1x");
    results.extend_harness(&mut h);
    page_to(&mut h, device, 1);
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(&mut h);
    h.snapshot("volt-huellkurven-1x");
    results.extend_harness(&mut h);
    page_to(&mut h, device, 2);
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(&mut h);
    h.snapshot("volt-matrix-1x");
    results.extend_harness(&mut h);
    let _ = (
        click as fn(&mut Harness<'_, App>, Pos2),
        DeviceRole::Instrument,
    );
}

const CARD: &str = "Volt";

fn card<'h>(h: &'h Harness<'_, App>) -> egui_kittest::Node<'h> {
    // The accessible name of the card (`devices_card`) lives as long as the harness' tree.
    let name: &'static str = Box::leak(Lang::De.devices_card(CARD).into_boxed_str());
    h.get_by_role_and_label(Role::Group, name)
}

fn slider_value(h: &Harness<'_, App>, label: &str) -> String {
    card(h)
        .get_by_role_and_label(Role::Slider, label)
        .value()
        .unwrap_or_default()
}

fn press_label(h: &mut Harness<'_, App>, role: Role, label: &str) {
    let at = card(h).get_by_role_and_label(role, label).rect().center();
    click(h, at);
}

#[test]
fn volt_is_an_instrument_in_the_catalog_and_adding_it_makes_a_track() {
    let mut app = App::headless(Project::new("Volt"));
    let entry = app
        .engine()
        .host()
        .catalog()
        .entries()
        .iter()
        .find(|e| e.plugin.name == "Volt")
        .expect("in the catalog")
        .clone();
    assert_eq!(entry.kind, komponist_ipc::PluginKind::Instrument);
    assert_eq!(entry.plugin, Internal::Volt.plugin_ref());
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: entry.plugin,
        target: DropTarget::NewTrack { index: None },
    }));
    let device = &app.project().tracks[0].channel.devices[0];
    assert_eq!(device.plugin.name, "Volt");
    assert_eq!(device.role, DeviceRole::Instrument);
}

#[test]
fn the_card_has_three_pages_and_the_switches_choose_the_envelope_and_the_lfo() {
    let (app, device) = app_with_volt();
    let mut h = harness(app);
    for name in ["Klang", "Hüllkurven", "Matrix"] {
        card(&h).get_by_role_and_label(Role::RadioButton, name);
    }
    // Klang: the filter and the signal graph are there, the cutoff shows its value.
    assert_eq!(slider_value(&h, "Cutoff"), "1.2 kHz");
    assert_eq!(slider_value(&h, "Lautstärke"), "\u{2212}6.0 dB");
    page_to(&mut h, device, 1);
    // The amplitude envelope first; the switch picks the filter and the third envelope.
    assert_eq!(slider_value(&h, "A"), "3.0 ms");
    press_label(&mut h, Role::RadioButton, "Hüllkurve: Filter");
    assert_eq!(slider_value(&h, "A"), "2.0 ms");
    assert_eq!(slider_value(&h, "D"), "350 ms");
    press_label(&mut h, Role::RadioButton, "Hüllkurve: Mod");
    assert_eq!(slider_value(&h, "A"), "1.0 ms");
    assert_eq!(slider_value(&h, "D"), "600 ms");
    // LFO 1 first, then LFO 2.
    assert_eq!(slider_value(&h, "Rate"), "4.5 Hz");
    press_label(&mut h, Role::RadioButton, "LFO: LFO 2");
    assert_eq!(slider_value(&h, "Rate"), "0.6 Hz");
    // A knob turn goes to the parameter of the chosen envelope only.
    let a = card(&h).get_by_role_and_label(Role::Slider, "A").rect();
    h.hover_at(a.center());
    h.step();
    press(&mut h, a.center(), true);
    h.step();
    h.hover_at(a.center() - vec2(0.0, 30.0));
    h.step();
    press(&mut h, a.center() - vec2(0.0, 30.0), false);
    h.run_steps(3);
    let values = values(h.state(), device);
    let get = |k: &str| values[Internal::Volt.wire(&ParamId::new(k)).unwrap() as usize];
    assert!(get("mod_a") > 0.0011, "{}", get("mod_a"));
    assert_eq!((get("amp_a"), get("filt_a")), (0.003, 0.002));
}

#[test]
fn a_synced_lfo_shows_its_divider_instead_of_the_rate() {
    let (mut app, device) = app_with_volt();
    set(&mut app, device, "lfo1_sync", 1.0);
    page(&mut app, device, 1);
    let h = harness(app);
    assert!(
        card(&h)
            .query_by_role_and_label(Role::Slider, "Rate")
            .is_none()
    );
    assert_eq!(slider_value(&h, "Teiler"), "1/4");
}

fn values(app: &App, device: DeviceId) -> Vec<f64> {
    let (_, _, d) = app.project().find_device(device).unwrap();
    Internal::Volt.decode_state(d.state.as_deref()).unwrap()
}

#[test]
fn knob_turns_and_routing_are_saved_in_the_project_and_load_again() {
    let (mut app, device) = app_with_volt();
    for (key, v) in [
        ("cutoff", 640.0),
        ("mod2_src", 6.0),
        ("mod2_dst", 1.0),
        ("mod2_amt", -0.25),
        ("lfo2_div", 6.0),
        ("mode", 2.0),
        ("unison", 5.0),
    ] {
        set(&mut app, device, key, v);
    }
    let before = values(&app, device);
    let json = komponist_model::persist::to_json(app.project()).unwrap();
    let loaded = komponist_model::persist::from_json(&json).unwrap();
    let (_, _, d) = loaded.find_device(device).unwrap();
    assert_eq!(d.plugin.name, "Volt");
    let state: serde_json::Value = serde_json::from_slice(d.state.as_deref().unwrap()).unwrap();
    assert_eq!(state["params"]["cutoff"], 640.0);
    assert_eq!(state["params"]["mod2_amt"], -0.25);
    assert_eq!(
        Internal::Volt.decode_state(d.state.as_deref()).unwrap(),
        before
    );
    // A state written before a parameter existed still loads: the missing ones are defaults.
    let old = br#"{"params": {"cutoff": 500, "osc_a_wave": 1}}"#;
    let values = Internal::Volt.decode_state(Some(old)).unwrap();
    assert_eq!(
        values[Internal::Volt.wire(&ParamId::new("cutoff")).unwrap() as usize],
        500.0
    );
    assert_eq!(
        values[Internal::Volt.wire(&ParamId::new("resonance")).unwrap() as usize],
        0.3
    );
}

#[test]
fn an_old_file_that_calls_the_synth_knurr_shows_it_as_volt() {
    // The fixture stores plugin id and name "knurr". The id stays, the name is shown as volt.
    const OLD_FILE: &str = include_str!("../fixtures/knurr-project.komponist");
    assert!(OLD_FILE.contains("\"name\": \"knurr\""));
    let project = komponist_model::persist::from_json(OLD_FILE).expect("the old file loads");
    let device = project.tracks[0].channel.devices.first().expect("a device");
    assert_eq!(device.plugin.id, "knurr");
    assert_eq!(device.plugin.name, "Volt");
    assert_eq!(Internal::from_id(&device.plugin.id), Some(Internal::Volt));
    // Saving writes the new name and keeps the id.
    let saved = komponist_model::persist::to_json(&project).expect("saves");
    let v: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let plugin = &v["project"]["tracks"][0]["channel"]["devices"][0]["plugin"];
    assert_eq!(
        (plugin["id"].as_str(), plugin["name"].as_str()),
        (Some("knurr"), Some("Volt"))
    );
}
