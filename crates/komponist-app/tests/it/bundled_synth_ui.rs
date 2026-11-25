//! Device editor and preset flows against an authenticated all-device release payload.

use std::path::{Path, PathBuf};

use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};
use komponist_app::actions::{DropTarget, EditAction};
use komponist_app::device_host::DeviceHost;
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_i18n::Lang;
use komponist_model::edits::{self, NewTrack};
use komponist_model::project::SavedPluginState;
use komponist_model::{Device, DeviceId, PluginRef, Project};
use komponist_plugins::{CatalogEntry, PluginManagerEvent, requires_bundled};

use crate::acceptance_support::{click, drag, harness, rect_of};
use crate::bundled_current::scan;
use crate::common::{PATIENCE, app_with, step_until};

const HEXA: &str = "org.klang-studio.komponist.hexa";
const VOLT: &str = "org.klang-studio.komponist.volt";
const SURF: &str = "org.klang-studio.komponist.surf";
const PIANO: &str = "E-Piano";
const PLUCK: &str = "Acid Pluck";

#[test]
#[ignore = "requires an authenticated all-device release package"]
fn packaged_synth_editors_and_factory_presets_follow_clap_state() {
    let launcher = PathBuf::from(
        std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP")
            .or_else(|| std::env::var_os("KOMPONIST_HISTORICAL_BUNDLED_APP"))
            .expect("set the unpacked app executable"),
    );
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let host = scan(&launcher, scratch.path());
    let expected_piano = host
        .manager()
        .prepare_bundled_device(HEXA, Some(PIANO), &[], &[])
        .expect("authenticated Hexa preset")
        .state
        .expect("Hexa factory state");
    let pluck_position = host
        .manager()
        .prepare_bundled_device(VOLT, Some(PLUCK), &[], &["cutoff"])
        .expect("authenticated Volt preset")
        .targets["cutoff"]
        .1;
    let lang = match std::env::var("KOMPONIST_SYNTH_UI_LANG").as_deref() {
        Ok("en") => Lang::En,
        _ => Lang::De,
    };
    let mut app = app_with(Project::new("Bundled synths"), host).with_lang(lang);
    let hexa_entry = packaged(app.engine().host(), HEXA);
    let plugin = hexa_entry.plugin.clone();
    let source = hexa_entry
        .clap_presets
        .iter()
        .find(|preset| preset.name == PIANO)
        .unwrap()
        .source
        .clone();
    app.dispatch(Action::Plugin(PluginAction::AddFromPreset {
        plugin,
        source,
        track: None,
    }));
    step_until(&mut app, "track prepared from Hexa preset", |app| {
        !app.project().tracks.is_empty()
    });
    let hexa = newest_instrument(&app).id;
    assert_eq!(
        newest_instrument(&app).state.as_ref(),
        Some(&expected_piano)
    );
    wait_for_verified(&mut app, hexa, HEXA, force_bridge);
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Button, &lang.devices_preset_name(PIANO));

    let algorithm = alias(h.state(), hexa, "algorithm");
    h.state_mut()
        .dispatch(Action::Plugin(PluginAction::SetParam {
            device: hexa,
            id: algorithm,
            value: 7.0,
        }));
    h.run_steps(3);
    h.get_by_role_and_label(Role::Group, &lang.ui_fm_algorithm(7));
    snapshot(&mut h, "hexa-operators", lang);
    click(&mut h, Role::Slider, "Ratio 3");
    page(&mut h, hexa, 1);
    h.get_by_role_and_label(Role::Slider, &format!("{} 3", lang.param_attack()));
    click(
        &mut h,
        Role::RadioButton,
        &format!("{}: 5", lang.devices_operator()),
    );
    h.get_by_role_and_label(Role::Slider, &format!("{} 5", lang.param_attack()));
    snapshot(&mut h, "hexa-envelopes", lang);
    // The card reacts to the host's optimistic value. Commit Hexa's actual edit before
    // testing Volt's history so a late Hexa snapshot cannot become the next undo step.
    wait_for_value(h.state_mut(), hexa, algorithm, 7.0, "edit Hexa algorithm");
    let hexa_state = capture_document_state(h.state_mut(), hexa, "Hexa algorithm captured");
    let hexa_plugin = newest_instrument(h.state()).plugin.clone();

    let plugin = packaged(h.state().engine().host(), VOLT).plugin.clone();
    h.state_mut().dispatch(Action::Plugin(PluginAction::Add {
        plugin,
        target: DropTarget::NewTrack { index: None },
    }));
    let volt = newest_instrument(h.state()).id;
    wait_for_verified(h.state_mut(), volt, VOLT, force_bridge);

    let target = h.state().project().find_device(volt).unwrap().2;
    let source = h
        .state()
        .engine()
        .host()
        .clap_presets(target)
        .iter()
        .find(|preset| preset.name == PLUCK)
        .unwrap()
        .source
        .clone();
    let apply_volt = Action::Plugin(PluginAction::ApplyClapPreset {
        device: volt,
        source,
    });
    let cutoff = alias(h.state(), volt, "cutoff");
    let host = h.state().engine().host();
    let cutoff_info = host
        .params(volt)
        .unwrap()
        .iter()
        .find(|param| param.id == cutoff)
        .unwrap();
    let preset_cutoff =
        cutoff_info.min + f64::from(pluck_position) * (cutoff_info.max - cutoff_info.min);
    let default_cutoff = cutoff_info.default;
    assert_ne!(default_cutoff, preset_cutoff);
    assert!(newest_instrument(h.state()).state.is_none());
    h.state_mut().dispatch(apply_volt.clone());
    wait_for_value(
        h.state_mut(),
        volt,
        cutoff,
        preset_cutoff,
        "apply Volt preset",
    );
    step_until(h.state_mut(), "Acid Pluck reports a 350 Hz cutoff", |app| {
        app.engine()
            .host()
            .param_value(volt, cutoff)
            .is_some_and(|value| value.text == "350 Hz")
    });
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, &lang.devices_preset_name(PLUCK));
    snapshot(&mut h, "volt-sound", lang);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    wait_for_value(
        h.state_mut(),
        volt,
        cutoff,
        default_cutoff,
        "undo Volt preset",
    );
    assert!(newest_instrument(h.state()).state.is_none());
    h.state_mut().dispatch(Action::Edit(EditAction::Redo));
    wait_for_value(
        h.state_mut(),
        volt,
        cutoff,
        preset_cutoff,
        "redo Volt preset",
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.state_mut().dispatch(Action::Edit(EditAction::Redo));
    wait_for_verified(h.state_mut(), volt, VOLT, force_bridge);
    let snapshots = h
        .state_mut()
        .engine_mut()
        .host_mut()
        .snapshot_selected_states(&[volt], PATIENCE)
        .expect("snapshot after immediate redo");
    let [(captured, redo_state)] = snapshots.try_into().expect("one Volt snapshot");
    assert_eq!(captured, volt);
    let volt_plugin = newest_instrument(h.state()).plugin.clone();

    page(&mut h, volt, 1);
    click(
        &mut h,
        Role::RadioButton,
        &format!("{}: LFO 2", lang.devices_lfo()),
    );
    let graph = rect_of(&h, Role::Group, &format!("{} 2 ·", lang.devices_lfo()));
    assert!(graph.is_positive());
    let first = alias(h.state(), volt, "lfo1_rate");
    let second = alias(h.state(), volt, "lfo2_rate");
    let one = h.state().engine().host().param_base(volt, first).unwrap();
    let two = h.state().engine().host().param_base(volt, second).unwrap();
    let rate = rect_of(&h, Role::Slider, "Rate").center();
    drag(&mut h, rate, rate - egui::vec2(0.0, 28.0));
    step_until(
        h.state_mut(),
        "LFO 2 edit reached the hosted processor",
        |app| {
            app.engine()
                .host()
                .param_base(volt, second)
                .is_some_and(|edited| (edited - two).abs() > 1e-6)
        },
    );
    let two_edited = h.state().engine().host().param_base(volt, second).unwrap();
    assert_eq!(h.state().engine().host().param_base(volt, first), Some(one));
    let lfo_state = capture_document_state(h.state_mut(), volt, "Volt saved the LFO 2 edit");
    h.run_steps(3);
    snapshot(&mut h, "volt-envelopes", lang);

    let stepped_state = if !force_bridge {
        let app = h.state_mut();
        let parameter = alias(app, volt, "osc_b_oct");
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device: volt,
            id: parameter,
            value: 4.6,
        }));
        app.dispatch(apply_volt);
        let events = step_until(app, "preset after a stepped parameter edit", |app| {
            app.device_events().iter().any(|event| {
                matches!(
                    event,
                    PluginManagerEvent::ClapPresetLoaded { device, .. } if *device == volt
                )
            })
        });
        let previous = events
            .into_iter()
            .find_map(|event| match event {
                PluginManagerEvent::ClapPresetLoaded {
                    device,
                    previous_state,
                    ..
                } if device == volt => Some(previous_state),
                _ => None,
            })
            .expect("the prior plugin state contains the snapped edit");
        Some(previous.into())
    } else {
        None
    };
    check_surf(&mut h, lang, force_bridge);
    h.state_mut().shutdown();
    drop(h);

    // Read the opaque blobs by restoring them into a fresh authenticated processor.
    assert_restored_params(
        &launcher,
        &hexa_plugin,
        &hexa_state,
        &[("algorithm", 7.0)],
        force_bridge,
    );
    assert_restored_params(
        &launcher,
        &volt_plugin,
        &redo_state,
        &[("cutoff", preset_cutoff)],
        force_bridge,
    );
    assert_restored_params(
        &launcher,
        &volt_plugin,
        &lfo_state,
        &[("lfo1_rate", one), ("lfo2_rate", two_edited)],
        force_bridge,
    );
    if let Some(state) = stepped_state {
        assert_restored_params(
            &launcher,
            &volt_plugin,
            &state,
            &[("osc_b_oct", 5.0)],
            force_bridge,
        );
    }
}

fn check_surf(h: &mut Harness<'_, App>, lang: Lang, force_bridge: bool) {
    let plugin = packaged(h.state().engine().host(), SURF).plugin.clone();
    h.state_mut().dispatch(Action::Plugin(PluginAction::Add {
        plugin,
        target: DropTarget::NewTrack { index: None },
    }));
    let surf = newest_instrument(h.state()).id;
    wait_for_verified(h.state_mut(), surf, SURF, force_bridge);
    let layout = h.state().engine().host().native_layout(surf).unwrap();
    assert_eq!(layout.schema_version, 2);
    assert_eq!(layout.pages.len(), 3);
    let a = alias(h.state(), surf, "osc_a_pos");
    let b = alias(h.state(), surf, "osc_b_pos");
    let a_before = h.state().engine().host().param_base(surf, a).unwrap();
    let b_before = h.state().engine().host().param_base(surf, b).unwrap();
    h.run_steps(3);
    let title = |osc| {
        format!(
            "{} {osc} · {}",
            lang.devices_osc(),
            lang.choice_surf_table_basic()
        )
    };
    h.get_by_role_and_label(Role::Group, &title("A"));
    h.get_by_role_and_label(Role::Slider, lang.param_surf_position_a());
    snapshot(h, "surf-sound-a", lang);
    click(
        h,
        Role::RadioButton,
        &format!("{}: B", lang.label_surf_osc()),
    );
    h.get_by_role_and_label(Role::Group, &title("B"));
    h.get_by_role_and_label(Role::Slider, lang.param_surf_position_b());
    snapshot(h, "surf-sound-b", lang);
    let axis = format!("{} {}", lang.ui_graph_wavetable(), lang.ui_graph_position());
    h.get_by_role_and_label(Role::Slider, &axis)
        .decrement_accesskit();
    h.run_steps(3);
    assert_eq!(
        h.state().engine().host().param_base(surf, a),
        Some(a_before)
    );
    assert!(h.state().engine().host().param_base(surf, b).unwrap() < b_before);

    page(h, surf, 1);
    click(
        h,
        Role::RadioButton,
        &format!("{}: Filter", lang.label_volt_envelope()),
    );
    h.get_by_role_and_label(Role::Group, &lang.graph_title_volt_envelope("Filter"));
    click(
        h,
        Role::RadioButton,
        &format!("{}: LFO 2", lang.label_volt_lfo()),
    );
    assert!(rect_of(h, Role::Group, &format!("{} 2 ·", lang.devices_lfo())).is_positive());
    snapshot(h, "surf-envelopes", lang);
    page(h, surf, 2);
    h.get_by_role_and_label(Role::ComboBox, &lang.param_volt_target(1));
    snapshot(h, "surf-matrix", lang);
}

fn newest_instrument(app: &App) -> &Device {
    app.project()
        .tracks
        .last()
        .unwrap()
        .channel
        .instrument()
        .unwrap()
        .1
}

fn packaged<'a>(host: &'a dyn DeviceHost, id: &str) -> &'a CatalogEntry {
    host.catalog()
        .entries()
        .iter()
        .find(|entry| entry.plugin.id == id && requires_bundled(&entry.plugin))
        .unwrap_or_else(|| panic!("missing authenticated synth {id}"))
}

fn wait_for_verified(app: &mut App, device: DeviceId, id: &str, bridge: bool) {
    let plugin = &app.project().find_device(device).unwrap().2.plugin;
    assert_eq!(plugin.id, id);
    assert!(requires_bundled(plugin));
    step_until(app, "authenticated synth ready", |app| {
        let host = app.engine().host();
        host.verified_bundled_kind(device).is_some() && host.native_layout(device).is_some()
    });
    assert_eq!(app.engine().host().bridge_pid(device).is_some(), bridge);
    assert!(app.engine().host().state_warning(device).is_none());
}

fn capture_document_state(app: &mut App, device: DeviceId, what: &str) -> SavedPluginState {
    app.engine_mut().host_mut().request_state(device);
    step_until(app, what, |app| {
        app.device_events().iter().any(|event| {
            matches!(event, PluginManagerEvent::StateSnapshot { device: updated, state }
                if *updated == device
                    && app.engine().host().is_current_param_snapshot(device, &state.bytes)
                    && app.project().find_device(device).unwrap().2.has_saved_state(state))
        })
    });
    app.project()
        .find_device(device)
        .unwrap()
        .2
        .saved_state()
        .unwrap()
}

fn assert_restored_params(
    launcher: &Path,
    plugin: &PluginRef,
    state: &SavedPluginState,
    params: &[(&str, f64)],
    bridge: bool,
) {
    assert!(
        state.parameter_bases.is_empty(),
        "read the plug-in's own state"
    );
    let scratch = tempfile::tempdir().unwrap();
    let host = scan(launcher, scratch.path());
    let (project, track) = edits::add_track(
        &Project::new("Captured synth state"),
        NewTrack {
            instrument: Some(plugin.clone()),
            name: None,
            hue: None,
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
    let project = edits::set_saved_plugin_state(&project, device, state).unwrap();
    let mut app = app_with(project, host);
    wait_for_verified(&mut app, device, &plugin.id, bridge);
    for &(name, expected) in params {
        let wire = alias(&app, device, name);
        wait_for_value(&mut app, device, wire, expected, name);
    }
    app.shutdown();
}

fn alias(app: &App, device: DeviceId, author: &str) -> u32 {
    app.engine()
        .host()
        .native_layout(device)
        .unwrap()
        .author_ids()
        .unwrap()[author]
}

fn wait_for_value(app: &mut App, device: DeviceId, wire: u32, value: f64, phase: &str) {
    step_until(app, &format!("{phase} reached {value}"), |app| {
        let host = app.engine().host();
        host.verified_bundled_kind(device).is_some()
            && host
                .param_base(device, wire)
                .is_some_and(|actual| (actual - value).abs() < 1e-6)
    });
}

fn page(h: &mut Harness<'_, App>, device: DeviceId, page: usize) {
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDevicePage { device, page }));
    h.run_steps(3);
}

fn snapshot(h: &mut Harness<'_, App>, file: &str, lang: Lang) {
    if let Some(dir) = std::env::var_os("KOMPONIST_SYNTH_UI_PNG_DIR") {
        let path = PathBuf::from(dir).join(format!("{file}-{}.png", lang.code()));
        h.render().unwrap().save(path).unwrap();
    }
}
