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
use komponist_app::{
    Action, App, EditAction, PluginAction, ProjectAction, TransportAction, UiAction,
};
use komponist_builtin::Internal;
use komponist_i18n::{Lang, de};
use komponist_model::params::ParamId;
use komponist_model::{DeviceId, Project, TrackId};

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
fn app_with(kind: Internal) -> (App, TrackId, DeviceId) {
    let mut app = App::headless(Project::new("Presets"));
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

fn state_of(app: &App, device: DeviceId) -> Option<Vec<u8>> {
    let (_, _, d) = app.project().find_device(device).unwrap();
    d.state.as_deref().map(<[u8]>::to_vec)
}

fn loaded(app: &App, kind: Internal, device: DeviceId) -> Option<(usize, bool)> {
    kind.loaded_preset(state_of(app, device).as_deref())
}

fn set_param(app: &mut App, device: DeviceId, kind: Internal, key: &str, value: f64) {
    let id = kind.wire(&ParamId::new(key)).expect("known parameter");
    app.dispatch(Action::Plugin(PluginAction::SetParam { device, id, value }));
}

fn card<'h>(h: &'h Harness<'_, App>, kind: Internal) -> egui_kittest::Node<'h> {
    // Test code: leaking the name keeps the node's borrow simple.
    let label: &'static str = Box::leak(Lang::De.devices_card(kind.name()).into_boxed_str());
    h.get_by_role_and_label(Role::Group, label)
}

/// The preset button of the card: „Preset“ or „Preset: <name>“.
fn preset_button<'h>(h: &'h Harness<'_, App>, kind: Internal) -> egui_kittest::Node<'h> {
    card(h, kind)
        .query_all_by_role(Role::Button)
        .find(|b| {
            b.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with(de::DEVICES_PRESET))
        })
        .expect("a preset button")
}

fn open_menu(h: &mut Harness<'_, App>, kind: Internal) {
    let rect = preset_button(h, kind).rect();
    click(h, rect.center());
}

fn pick(h: &mut Harness<'_, App>, name: &str) {
    let rect = h.get_by_role_and_label(Role::MenuItem, name).rect();
    click(h, rect.center());
    h.run_steps(2);
}

#[test]
fn loading_a_preset_is_one_undo_step_and_the_engine_follows_it_and_its_undo() {
    let (mut app, track, _) = app_with(Internal::Presse);
    let presse = app.project().tracks[0].channel.devices.last().unwrap().id;
    // A second track the first one has to wait for once the presse gets lookahead.
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let plain = app.project().tracks[1].id;
    let pdc = |app: &App, t: TrackId| {
        app.engine()
            .program()
            .unwrap()
            .tracks
            .iter()
            .find(|x| x.id == t)
            .unwrap()
            .pdc_delay
    };
    app.engine_mut().hold_engine().expect("engine held");
    app.dispatch(Action::Transport(TransportAction::TogglePlay));
    app.step();
    assert_eq!(app.engine().host().latency(presse), 0);
    let hard = Internal::Presse
        .presets()
        .iter()
        .position(|p| p.name == "Hard Limit")
        .unwrap();

    // "Hard Limit" switches the lookahead on: the processor got the values.
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device: presse,
        source: komponist_plugins::presets::legacy_source(hard),
    }));
    assert_eq!(loaded(&app, Internal::Presse, presse), Some((hard, false)));
    assert!(app.engine_mut().run_held(1024, &[], 0));
    app.step();
    let frames = app.engine().host().latency(presse);
    assert!(frames > 0, "the preset's lookahead reached the engine");
    assert_eq!(pdc(&app, plain), frames);
    assert_eq!(pdc(&app, track), 0);
    let values = Internal::Presse
        .decode_state(state_of(&app, presse).as_deref())
        .unwrap();
    assert_eq!(Internal::Presse.preset_values(hard).unwrap(), values);

    // One undo takes the whole preset back, in the document and in the engine.
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(loaded(&app, Internal::Presse, presse), None);
    assert_eq!(
        Internal::Presse
            .decode_state(state_of(&app, presse).as_deref())
            .unwrap(),
        Internal::Presse.defaults()
    );
    assert!(app.engine_mut().run_held(1024, &[], 0));
    app.step();
    assert_eq!(app.engine().host().latency(presse), 0);
    assert_eq!(pdc(&app, plain), 0);

    // Redo brings it back.
    app.dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(loaded(&app, Internal::Presse, presse), Some((hard, false)));
    assert!(app.engine_mut().run_held(1024, &[], 0));
    app.step();
    assert_eq!(app.engine().host().latency(presse), frames);
}

#[test]
fn loading_a_preset_stores_exactly_its_values_and_a_bad_index_changes_nothing() {
    let (mut app, _, limiter) = app_with(Internal::Limiter);
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device: limiter,
        source: komponist_plugins::presets::legacy_source(1),
    }));
    let values = Internal::Limiter
        .decode_state(state_of(&app, limiter).as_deref())
        .unwrap();
    assert_eq!(Internal::Limiter.preset_values(1).unwrap(), values);
    // An index past the list changes nothing.
    let before = state_of(&app, limiter);
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device: limiter,
        source: komponist_plugins::presets::legacy_source(99),
    }));
    assert_eq!(state_of(&app, limiter), before);
}

#[test]
fn the_dropdown_shows_the_loaded_preset_and_a_star_while_a_value_differs() {
    let kind = Internal::Delay;
    let (app, _, device) = app_with(kind);
    let mut h = harness(app);
    // No preset yet: „Preset“.
    assert_eq!(
        preset_button(&h, kind).accesskit_node().label().as_deref(),
        Some(de::DEVICES_PRESET)
    );
    open_menu(&mut h, kind);
    pick(&mut h, "Slapback");
    assert_eq!(loaded(h.state(), kind, device), Some((0, false)));
    let label = Lang::De.devices_preset_name("Slapback");
    assert_eq!(
        preset_button(&h, kind).accesskit_node().label().as_deref(),
        Some(label.as_str())
    );

    // A changed value: „ *“.
    set_param(h.state_mut(), device, kind, "mix", 0.9);
    h.run_steps(3);
    let starred = Lang::De.devices_preset_name("Slapback *");
    assert_eq!(
        preset_button(&h, kind).accesskit_node().label().as_deref(),
        Some(starred.as_str())
    );
    // The same value again: the star goes.
    let preset_mix =
        kind.preset_values(0).unwrap()[kind.wire(&ParamId::new("mix")).unwrap() as usize];
    set_param(h.state_mut(), device, kind, "mix", preset_mix);
    h.run_steps(3);
    assert_eq!(
        preset_button(&h, kind).accesskit_node().label().as_deref(),
        Some(label.as_str())
    );
}

#[test]
fn the_dropdown_lists_the_presets_of_its_own_device() {
    for kind in [
        Internal::Eq,
        Internal::Delay,
        Internal::Hall,
        Internal::Limiter,
        Internal::Presse,
        Internal::Arpeggiator,
        Internal::Tonleiter,
    ] {
        let (app, _, _) = app_with(kind);
        let mut h = harness(app);
        open_menu(&mut h, kind);
        let items: Vec<String> = h
            .query_all_by_role(Role::MenuItem)
            .filter_map(|n| n.accesskit_node().label())
            .collect();
        for p in kind.presets() {
            assert!(
                items.iter().any(|i| i == p.name),
                "{}: {} in {items:?}",
                kind.name(),
                p.name
            );
        }
        assert_eq!(
            items.len(),
            kind.presets().len(),
            "{}: {items:?}",
            kind.name()
        );
    }
}

#[test]
fn volt_groups_its_presets_by_category_and_the_sampler_has_no_dropdown() {
    let (app, _, _) = app_with(Internal::Volt);
    let mut h = harness(app);
    open_menu(&mut h, Internal::Volt);
    let items: Vec<String> = h
        .query_all_by_role(Role::MenuItem)
        .filter_map(|n| n.accesskit_node().label())
        .collect();
    let mut categories: Vec<&str> = Vec::new();
    for c in Internal::Volt.presets().iter().filter_map(|p| p.category) {
        if !categories.contains(&c) {
            categories.push(c);
        }
    }
    assert!(categories.len() > 1);
    assert_eq!(items.len(), categories.len(), "{items:?}");
    for c in categories {
        assert!(items.iter().any(|i| i == c), "{c} in {items:?}");
    }

    let (app, _, _) = app_with(Internal::Sampler);
    let h = harness(app);
    assert!(
        !card(&h, Internal::Sampler)
            .query_all_by_role(Role::Button)
            .any(|b| b
                .accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with(de::DEVICES_PRESET)))
    );
}

#[test]
fn the_loaded_preset_survives_saving_and_old_projects_load_without_one() {
    let dir = std::env::temp_dir().join(format!("komponist-wp96-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Presets.komponist");
    std::fs::write(
        &path,
        komponist_model::persist::to_json(&Project::new("Presets")).unwrap(),
    )
    .unwrap();
    let kind = Internal::Hall;
    let mut app = App::headless(Project::new("Presets"));
    app.dispatch(Action::Project(ProjectAction::OpenPath(path.clone())));
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: kind.plugin_ref(),
        target: DropTarget::Track(track),
    }));
    let device = app.project().tracks[0].channel.devices[1].id;
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device,
        source: komponist_plugins::presets::legacy_source(2),
    }));
    set_param(&mut app, device, kind, "mix", 0.9);
    app.dispatch(Action::Project(ProjectAction::Save));

    let text = std::fs::read_to_string(&path).unwrap();
    let project = komponist_model::persist::from_json(&text).unwrap();
    let (_, _, d) = project.find_device(device).unwrap();
    let state: serde_json::Value = serde_json::from_slice(d.state.as_deref().unwrap()).unwrap();
    assert_eq!(state["preset"], kind.presets()[2].name);
    // Loaded again: the name and the „changed“ flag come back.
    assert_eq!(kind.loaded_preset(d.state.as_deref()), Some((2, true)));
    let reopened = App::headless(project.clone());
    assert_eq!(loaded(&reopened, kind, device), Some((2, true)));

    // A project from before WP-96: the same state without the name.
    let mut old: serde_json::Value = state;
    old.as_object_mut().unwrap().remove("preset");
    let old_state: std::sync::Arc<[u8]> = serde_json::to_vec(&old).unwrap().into();
    let project =
        komponist_model::edits::set_device_state(&project, device, Some(old_state)).unwrap();
    let mut app = App::headless(project);
    assert_eq!(loaded(&app, kind, device), None);
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![app.project().tracks[0].id],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let h = harness(app);
    assert_eq!(
        preset_button(&h, kind).accesskit_node().label().as_deref(),
        Some(de::DEVICES_PRESET)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn snapshot_of_the_eq_head_with_the_open_preset_dropdown() {
    let kind = Internal::Eq;
    let (mut app, _, device) = app_with(kind);
    app.dispatch(Action::Plugin(PluginAction::ApplyClapPreset {
        device,
        source: komponist_plugins::presets::legacy_source(2),
    }));
    set_param(&mut app, device, kind, "out", 1.5);
    let mut h = harness(app);
    h.run_steps(2);
    open_menu(&mut h, kind);
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
