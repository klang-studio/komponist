//! The preset dropdown of the internal devices (WP-96): every device with factory presets has the
//! dropdown in its head, loading one is a single undo step that reaches the engine, „ *“ marks
//! changed values, and the loaded preset's name is saved with the device state.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, EditAction, PluginAction, TransportAction, UiAction};
use komponist_builtin::Internal;
use komponist_i18n::{Lang, de};
use komponist_model::params::ParamId;
use komponist_model::{DeviceId, Project, TrackId};

use crate::common::{
    author_base, set_authored_param as set_param, shipped_plugin, step_for, step_until,
    wait_for_layout,
};
use komponist_ipc::clap_presets::ClapPreset;

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

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(3);
}

/// A project with a hexa track and one `kind` device behind it; the dock shows the devices.
fn reference_app_with(kind: Internal) -> (App, TrackId, DeviceId) {
    let mut app = crate::common::reference_app(Project::new("Presets"));
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: kind.plugin_ref(),
        target: DropTarget::Track(track),
    }));
    let device = app.project().tracks[0].channel.devices.last().unwrap().id;
    (app, track, device)
}

fn app_with(id: &str) -> (App, TrackId, DeviceId) {
    let mut app = App::headless(Project::new("Presets"));
    let hexa = plugin(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let plugin = plugin(&app, id);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: plugin.clone(),
        target: DropTarget::Track(track),
    }));
    let device = app.project().tracks[0]
        .channel
        .devices
        .iter()
        .find(|device| device.plugin == plugin)
        .unwrap()
        .id;
    wait_for_layout(&mut app, device);
    (app, track, device)
}

fn state_of(app: &App, device: DeviceId) -> Option<Vec<u8>> {
    let (_, _, d) = app.project().find_device(device).unwrap();
    d.state.as_deref().map(<[u8]>::to_vec)
}

fn plugin(app: &App, id: &str) -> komponist_model::PluginRef {
    let id = format!("org.klang-studio.komponist.{id}");
    shipped_plugin(app.engine().host(), &id)
        .unwrap_or_else(|| panic!("build {id} with scripts/build-test-plugins.sh"))
}

fn presets(app: &App, device: DeviceId) -> &[ClapPreset] {
    let model = app.project().find_device(device).unwrap().2;
    app.engine().host().clap_presets(model)
}

fn loaded(app: &App, device: DeviceId) -> Option<(&str, bool)> {
    let model = app.project().find_device(device).unwrap().2;
    let presets = presets(app, device);
    komponist_app::device_host::selected_preset(model, presets)
        .map(|(index, edited)| (presets[index].name.as_str(), edited))
}

fn factory(app: &mut App, device: DeviceId, name: &str) {
    let source = presets(app, device)
        .iter()
        .find(|preset| preset.name == name)
        .unwrap_or_else(|| panic!("no fixture preset named {name}"))
        .source
        .clone();
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device,
        source,
    }));
    step_until(app, "the selected CLAP preset", |app| {
        loaded(app, device) == Some((name, false))
    });
}

fn reference_card<'h>(h: &'h Harness<'_, App>, kind: Internal) -> egui_kittest::Node<'h> {
    // Test code: leaking the name keeps the node's borrow simple.
    let label: &'static str = Box::leak(Lang::De.devices_card(kind.name()).into_boxed_str());
    h.get_by_role_and_label(Role::Group, label)
}

fn card<'h>(h: &'h Harness<'_, App>, name: &str) -> egui_kittest::Node<'h> {
    // Test code: leaking the name keeps the node's borrow simple.
    let label: &'static str = Box::leak(Lang::De.devices_card(name).into_boxed_str());
    h.get_by_role_and_label(Role::Group, label)
}

/// The preset button of the card: „Preset“ or „Preset: <name>“.
fn reference_preset_button<'h>(h: &'h Harness<'_, App>, kind: Internal) -> egui_kittest::Node<'h> {
    reference_card(h, kind)
        .query_all_by_role(Role::Button)
        .find(|b| {
            b.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with(de::DEVICES_PRESET))
        })
        .expect("a preset button")
}

fn preset_button<'h>(h: &'h Harness<'_, App>, name: &str) -> egui_kittest::Node<'h> {
    card(h, name)
        .query_all_by_role(Role::Button)
        .find(|b| {
            b.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with(de::DEVICES_PRESET))
        })
        .expect("a preset button")
}

fn reference_open_menu(h: &mut Harness<'_, App>, kind: Internal) {
    let rect = reference_preset_button(h, kind).rect();
    click(h, rect.center());
}

fn open_menu(h: &mut Harness<'_, App>, name: &str) {
    let rect = preset_button(h, name).rect();
    click(h, rect.center());
}

fn pick(h: &mut Harness<'_, App>, name: &str) {
    let rect = h.get_by_role_and_label(Role::MenuItem, name).rect();
    click(h, rect.center());
    h.run_steps(2);
}

#[test]
fn loading_a_preset_is_one_undo_step_and_the_engine_follows_it_and_its_undo() {
    let (mut app, track, presse) = app_with("presse");
    // A second track waits for the Presse lookahead when the preset activates it.
    let hexa = plugin(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let plain = app.project().tracks[1].id;
    let pdc = |app: &App, t: TrackId| app.engine().program().unwrap().track(t).unwrap().pdc_delay;
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    step_until(&mut app, "playing before the preset", |app| {
        app.transport() == komponist_engine::TransportState::Playing
    });
    assert_eq!(app.engine().host().latency(presse), 0);
    let before = state_of(&app, presse);

    factory(&mut app, presse, "Hard Limit");
    step_until(&mut app, "Presse's preset lookahead", |app| {
        let frames = app.engine().host().latency(presse);
        frames > 0 && pdc(app, plain) == frames
    });
    let frames = app.engine().host().latency(presse);
    assert_eq!(pdc(&app, track), 0);

    app.dispatch(Action::Edit(EditAction::Undo));
    step_until(&mut app, "undo restores the original latency", |app| {
        loaded(app, presse).is_none()
            && app.engine().host().latency(presse) == 0
            && pdc(app, plain) == 0
    });
    assert_eq!(state_of(&app, presse), before);

    app.dispatch(Action::Edit(EditAction::Redo));
    step_until(&mut app, "redo restores lookahead and the preset", |app| {
        loaded(app, presse) == Some(("Hard Limit", false))
            && app.engine().host().latency(presse) == frames
            && pdc(app, plain) == frames
    });
}

#[test]
fn loading_a_preset_stores_the_factory_state_and_an_unknown_source_changes_nothing() {
    let (mut app, _, limiter) = app_with("limiter");
    let preset = presets(&app, limiter)[1].clone();
    let expected = App::headless_plugin_host()
        .manager()
        .prepare_bundled_device(&preset.plugin_id, Some(&preset.name), &[], &[])
        .unwrap()
        .state;
    factory(&mut app, limiter, &preset.name);
    let before = state_of(&app, limiter);
    assert_eq!(before.as_deref(), expected.as_deref());
    let mut source = preset.source;
    source.load_key = Some(b"unpublished-fixture-preset".to_vec());
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device: limiter,
        source,
    }));
    app.step();
    assert_eq!(state_of(&app, limiter), before);
}

#[test]
fn the_dropdown_shows_the_loaded_preset_and_a_star_after_an_edit_until_undo() {
    let (app, _, device) = app_with("delay");
    let mut h = harness(app);
    assert_eq!(
        preset_button(&h, "Delay")
            .accesskit_node()
            .label()
            .as_deref(),
        Some(de::DEVICES_PRESET)
    );
    open_menu(&mut h, "Delay");
    pick(&mut h, "Slapback");
    step_until(h.state_mut(), "Slapback in the document", |app| {
        loaded(app, device) == Some(("Slapback", false))
    });
    let original = author_base(h.state(), device, "mix");
    let label = Lang::De.devices_preset_name("Slapback");
    h.run_steps(3);
    assert_eq!(
        preset_button(&h, "Delay")
            .accesskit_node()
            .label()
            .as_deref(),
        Some(label.as_str())
    );
    set_param(h.state_mut(), device, "mix", 0.9);
    h.run_steps(3);
    let starred = Lang::De.devices_preset_name("Slapback *");
    assert_eq!(
        preset_button(&h, "Delay")
            .accesskit_node()
            .label()
            .as_deref(),
        Some(starred.as_str())
    );
    // Turning the value back is another edit: the mark stays until undo takes the edits back.
    set_param(h.state_mut(), device, "mix", original);
    h.run_steps(3);
    assert_eq!(
        preset_button(&h, "Delay")
            .accesskit_node()
            .label()
            .as_deref(),
        Some(starred.as_str())
    );
    let app = h.state_mut();
    while loaded(app, device) != Some(("Slapback", false)) {
        assert!(
            app.doc().undo_label().is_some(),
            "undo reaches the loaded preset"
        );
        app.dispatch(Action::Edit(EditAction::Undo));
    }
    h.run_steps(3);
    assert_eq!(
        preset_button(&h, "Delay")
            .accesskit_node()
            .label()
            .as_deref(),
        Some(label.as_str())
    );
}

#[test]
fn a_fresh_device_shows_the_preset_it_starts_in_without_an_undo_step() {
    let (mut app, track, _) = app_with("delay");
    let hexa = app
        .project()
        .track(track)
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1
        .id;
    let undo = app.doc().undo_label().map(str::to_owned);
    step_until(&mut app, "Hexa's start preset", |app| {
        loaded(app, hexa) == Some(("Init", false))
    });
    assert_eq!(
        app.doc().undo_label().map(str::to_owned),
        undo,
        "no undo step"
    );
}

#[test]
fn a_saved_modified_preset_reopens_with_its_mark_not_the_start_preset() {
    use komponist_model::edits::{self, NewTrack};

    const HEXA: &str = "org.klang-studio.komponist.hexa";
    let host = App::headless_plugin_host();
    let prepare = |preset| {
        host.manager()
            .prepare_bundled_device(HEXA, preset, &[], &[])
            .unwrap()
    };
    // Edited back to the values of Init after loading E-Piano.
    let (epiano, init) = (prepare(Some("E-Piano")), prepare(None));
    let mut preset = epiano.preset.clone().expect("the prepared preset");
    preset.modified = true;
    let (project, track) = edits::add_track(
        &Project::new("Presets"),
        NewTrack {
            name: None,
            hue: None,
            instrument: Some(epiano.plugin.clone()),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let device = project
        .track(track)
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1
        .id;
    let project = edits::set_device_state(&project, device, init.state.clone()).unwrap();
    let project = edits::set_device_preset(&project, device, Some(preset)).unwrap();
    let serialized = komponist_model::persist::to_json(&project).unwrap();

    let mut app = App::headless(komponist_model::persist::from_json(&serialized).unwrap());
    let mut seen = step_until(&mut app, "the reopened Hexa", |app| {
        app.engine().host().status(device) == komponist_plugins::DeviceStatus::Ok
    });
    seen.extend(step_for(&mut app, std::time::Duration::from_millis(300)));
    assert!(
        seen.iter().any(|event| matches!(
            event,
            komponist_plugins::PluginManagerEvent::ClapPresetReported { startup: true, .. }
        )),
        "Hexa reports Init at startup: {seen:?}"
    );
    assert_eq!(loaded(&app, device), Some(("E-Piano", true)));
    assert!(app.doc().undo_label().is_none());
}

#[test]
fn the_dropdown_lists_the_factory_presets_of_its_own_device() {
    for (id, name) in [
        ("eq", "EQ"),
        ("delay", "Delay"),
        ("hall", "Hall"),
        ("limiter", "Limiter"),
        ("presse", "Presse"),
        ("arpeggiator", "Arpeggiator"),
        ("scale", "Tonleiter"),
    ] {
        let (app, _, device) = app_with(id);
        let mut h = harness(app);
        open_menu(&mut h, name);
        let items: Vec<_> = h
            .query_all_by_role(Role::MenuItem)
            .filter_map(|n| n.accesskit_node().label())
            .collect();
        let presets = presets(h.state(), device);
        assert!(!presets.is_empty(), "{name} has a factory inventory");
        for preset in presets {
            let expected = preset.display_name("de");
            assert!(
                items.iter().any(|item| item == expected),
                "{name}: {expected} in {items:?}"
            );
        }
    }
}

#[test]
fn volt_groups_its_presets_and_the_drum_sampler_has_no_factory_presets() {
    let (app, _, volt) = app_with("volt");
    let mut h = harness(app);
    open_menu(&mut h, "Volt");
    let items: Vec<_> = h
        .query_all_by_role(Role::MenuItem)
        .filter_map(|n| n.accesskit_node().label())
        .collect();
    let categories: std::collections::BTreeSet<_> = presets(h.state(), volt)
        .iter()
        .filter_map(ClapPreset::category)
        .collect();
    assert!(categories.len() > 1);
    for category in categories {
        assert!(
            items.iter().any(|item| item == category),
            "{category} in {items:?}"
        );
    }

    let (app, _, sampler) = app_with("sampler");
    assert!(presets(&app, sampler).iter().all(|preset| !preset.factory));
}

#[test]
fn the_loaded_preset_survives_saving_and_reopening() {
    let (mut app, _, device) = app_with("hall");
    let preset = presets(&app, device)[2].name.clone();
    factory(&mut app, device, &preset);
    set_param(&mut app, device, "mix", 0.9);
    let project = crate::common::project_with_snapshot(&mut app, device);
    let serialized = komponist_model::persist::to_json(&project).unwrap();
    let project = komponist_model::persist::from_json(&serialized).unwrap();
    let mut reopened = App::headless(project);
    wait_for_layout(&mut reopened, device);
    assert_eq!(loaded(&reopened, device), Some((preset.as_str(), true)));
}

#[test]
fn snapshot_of_the_eq_head_with_the_open_preset_dropdown() {
    let kind = Internal::Eq;
    let (mut app, _, device) = reference_app_with(kind);
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device,
        source: komponist_plugins::presets::legacy_source(2),
    }));
    set_reference_param(&mut app, device, kind, "out", 1.5);
    let mut h = harness(app);
    h.run_steps(2);
    reference_open_menu(&mut h, kind);
    h.run_steps(4);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    let mut results = SnapshotResults::new();
    h.snapshot("devices-preset-dropdown-eq-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

fn set_reference_param(app: &mut App, device: DeviceId, kind: Internal, key: &str, value: f64) {
    let id = kind.wire(&ParamId::new(key)).expect("known parameter");
    app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
}
