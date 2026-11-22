//! surf (#107) in the app: the catalog entry, the A/B switch and the wavetable graph, a project
//! round trip, and snapshots of the pages in German and English.

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

/// A project with one surf track in `lang` and the dock on „Geräte“.
fn app_with_surf(lang: Lang) -> (App, DeviceId) {
    let mut app = App::headless(Project::new("Surf")).with_lang(lang);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Surf.plugin_ref(),
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
    let id = Internal::Surf.wire(&ParamId::new(key)).expect(key);
    app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
}

fn page_to(h: &mut Harness<'_, App>, device: DeviceId, page: usize) {
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDevicePage { device, page }));
    h.run_steps(3);
}

fn mask_cpu(h: &mut Harness<'_, App>, lang: Lang) {
    let cpu = match lang {
        Lang::En => komponist_i18n::en::TRANSPORT_CPU,
        _ => komponist_i18n::de::TRANSPORT_CPU,
    };
    let cpu = h.get_by_role_and_label(Role::Status, cpu).rect();
    h.mask(cpu.expand(2.0));
}

fn values(app: &App, device: DeviceId) -> Vec<f64> {
    let (_, _, d) = app.project().find_device(device).unwrap();
    Internal::Surf.decode_state(d.state.as_deref()).unwrap()
}

fn value(app: &App, device: DeviceId, key: &str) -> f64 {
    values(app, device)[Internal::Surf.wire(&ParamId::new(key)).unwrap() as usize]
}

/// The „Vowel Pad“ of the demo: vowels on A, choir on B, LFO 1 → Position A, LFO 2 → Position B.
fn vowel_pad(app: &mut App, device: DeviceId) {
    for (key, v) in [
        ("osc_a_table", 6.0),
        ("osc_a_pos", 0.42),
        ("osc_b_table", 7.0),
        ("osc_b_pos", 0.4),
        ("osc_b_fine", 6.0),
        ("osc_mix", 0.45),
        ("sub_level", 0.0),
        ("cutoff", 2400.0),
        ("resonance", 0.22),
        ("env_amount", 0.35),
        ("mod1_src", 1.0),
        ("mod1_dst", 3.0),
        ("mod1_amt", 0.15),
        ("mod2_src", 2.0),
        ("mod2_dst", 4.0),
        ("mod2_amt", 0.3),
        ("mod3_src", 4.0),
        ("mod3_dst", 6.0),
        ("mod3_amt", 0.2),
        ("mod4_src", 6.0),
        ("mod4_dst", 3.0),
        ("mod4_amt", 0.25),
        ("unison", 3.0),
        ("volume", -8.0),
    ] {
        set(app, device, key, v);
    }
}

fn shot(h: &mut Harness<'_, App>, lang: Lang, name: &str, results: &mut SnapshotResults) {
    h.hover_at(Pos2::new(900.0, 300.0));
    h.run_steps(30);
    mask_cpu(h, lang);
    h.snapshot(name);
    results.extend_harness(h);
}

#[test]
fn snapshot_of_the_pages_in_both_languages() {
    let mut results = SnapshotResults::new();
    for (lang, suffix) in [(Lang::De, ""), (Lang::En, "-en")] {
        let (mut app, device) = app_with_surf(lang);
        vowel_pad(&mut app, device);
        let mut h = harness(app);
        shot(
            &mut h,
            lang,
            &format!("surf-klang-1x{suffix}"),
            &mut results,
        );
        page_to(&mut h, device, 1);
        shot(
            &mut h,
            lang,
            &format!("surf-huellkurven-1x{suffix}"),
            &mut results,
        );
        page_to(&mut h, device, 2);
        shot(
            &mut h,
            lang,
            &format!("surf-matrix-1x{suffix}"),
            &mut results,
        );
    }
}

const CARD: &str = "Surf";

fn card<'h>(h: &'h Harness<'_, App>, lang: Lang) -> egui_kittest::Node<'h> {
    let name: &'static str = Box::leak(lang.devices_card(CARD).into_boxed_str());
    h.get_by_role_and_label(Role::Group, name)
}

fn slider_value(h: &Harness<'_, App>, label: &str) -> String {
    card(h, Lang::De)
        .get_by_role_and_label(Role::Slider, label)
        .value()
        .unwrap_or_default()
}

#[test]
fn surf_is_an_instrument_in_the_catalog_and_adding_it_makes_a_track() {
    let mut app = App::headless(Project::new("Surf"));
    let entry = app
        .engine()
        .host()
        .catalog()
        .entries()
        .iter()
        .find(|e| e.plugin.name == "Surf")
        .expect("in the catalog")
        .clone();
    assert_eq!(entry.kind, komponist_ipc::PluginKind::Instrument);
    assert_eq!(entry.plugin, Internal::Surf.plugin_ref());
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: entry.plugin,
        target: DropTarget::NewTrack { index: None },
    }));
    let device = &app.project().tracks[0].channel.devices[0];
    assert_eq!(device.plugin.name, "Surf");
    assert_eq!(device.role, DeviceRole::Instrument);
}

#[test]
fn the_switch_shows_oscillator_b_and_the_graph_names_the_table() {
    let (mut app, device) = app_with_surf(Lang::De);
    vowel_pad(&mut app, device);
    let mut h = harness(app);
    for name in ["Klang", "Hüllkurven", "Matrix"] {
        card(&h, Lang::De).get_by_role_and_label(Role::RadioButton, name);
    }
    // Oscillator A first: its table and position.
    card(&h, Lang::De).get_by_role_and_label(Role::Group, "Osz A · Vokale");
    assert_eq!(slider_value(&h, "Position A"), "42 %");
    assert_eq!(
        card(&h, Lang::De)
            .get_by_role_and_label(Role::SpinButton, "A Okt.")
            .value()
            .unwrap_or_default(),
        "0 Okt."
    );
    // The switch shows B: its table, position and tuning.
    let b = card(&h, Lang::De)
        .get_by_role_and_label(Role::RadioButton, "Oszillator: B")
        .rect()
        .center();
    click(&mut h, b);
    card(&h, Lang::De).get_by_role_and_label(Role::Group, "Osz B · Chor");
    assert_eq!(slider_value(&h, "Position B"), "40 %");
    assert_eq!(slider_value(&h, "B Fein"), "6 ct");
    assert!(
        card(&h, Lang::De)
            .query_by_role_and_label(Role::Slider, "Position A")
            .is_none()
    );
}

#[test]
fn dragging_the_position_on_the_stack_moves_the_shown_oscillators_position() {
    let (mut app, device) = app_with_surf(Lang::De);
    set(&mut app, device, "osc_a_pos", 0.2);
    let mut h = harness(app);
    let handle = card(&h, Lang::De)
        .get_by_role_and_label(Role::Slider, "Wellentabelle Position")
        .rect()
        .center();
    h.hover_at(handle);
    h.step();
    press(&mut h, handle, true);
    h.step();
    let to = handle + vec2(40.0, 0.0);
    h.hover_at(to);
    h.step();
    press(&mut h, to, false);
    h.run_steps(3);
    let a = value(h.state(), device, "osc_a_pos");
    assert!(a > 0.3, "{a}");
    assert_eq!(value(h.state(), device, "osc_b_pos"), 1.0);
    // One drag is one undo step.
    h.state_mut()
        .dispatch(Action::Edit(komponist_app::EditAction::Undo));
    h.run_steps(3);
    assert_eq!(value(h.state(), device, "osc_a_pos"), 0.2);
}

#[test]
fn tables_positions_and_tuning_are_saved_in_the_project_and_load_again() {
    let (mut app, device) = app_with_surf(Lang::De);
    vowel_pad(&mut app, device);
    set(&mut app, device, "osc_a_oct", -1.0);
    set(&mut app, device, "osc_a_fine", 12.0);
    let before = values(&app, device);
    let json = komponist_model::persist::to_json(app.project()).unwrap();
    let loaded = komponist_model::persist::from_json(&json).unwrap();
    let (_, _, d) = loaded.find_device(device).unwrap();
    assert_eq!(
        (d.plugin.id.as_str(), d.plugin.name.as_str()),
        ("surf", "Surf")
    );
    let state: serde_json::Value = serde_json::from_slice(d.state.as_deref().unwrap()).unwrap();
    assert_eq!(state["params"]["osc_a_table"], 6.0);
    assert_eq!(state["params"]["osc_a_pos"], 0.42);
    assert_eq!(state["params"]["osc_a_oct"], -1.0);
    assert_eq!(
        Internal::Surf.decode_state(d.state.as_deref()).unwrap(),
        before
    );
}
