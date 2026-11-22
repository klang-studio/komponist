//! Device editor and preset flows against an authenticated all-device release payload.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui_kittest::{Harness, kittest::Queryable as _};
use komponist_app::actions::{DropTarget, EditAction};
use komponist_app::device_host::{DeviceHost, PluginHost};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_builtin::{BuiltinKind, Internal};
use komponist_device_core::pinned_clap_state;
use komponist_hexa::device::{Field, OpField};
use komponist_i18n::Lang;
use komponist_model::params::ParamId;
use komponist_model::{Device, DeviceId, PluginFormat, Project};
use komponist_plugins::{PluginDirs, PluginManagerEvent};

use crate::acceptance_support::{click, drag, harness, rect_of};
use crate::common::{PATIENCE, app_with, config, step_until};

#[test]
#[ignore = "requires an authenticated all-device release package"]
fn packaged_synth_editors_and_factory_presets_follow_clap_state() {
    let launcher = std::env::var_os("KOMPONIST_CURRENT_BUNDLED_APP")
        .or_else(|| std::env::var_os("KOMPONIST_HISTORICAL_BUNDLED_APP"))
        .expect("set the unpacked app executable");
    let force_bridge =
        std::env::var_os("KOMPONIST_BUNDLED_CLAP_FORCE_BRIDGE").is_some_and(|value| value == "1");
    let scratch = tempfile::tempdir().unwrap();
    let mut cfg = config(scratch.path());
    cfg.bridge_exe = launcher.into();
    let mut host = PluginHost::new(cfg);
    host.start_scan(&PluginDirs {
        clap: Vec::new(),
        vst3: Vec::new(),
    });
    let deadline = Instant::now() + PATIENCE;
    while !host
        .poll()
        .iter()
        .any(|event| matches!(event, PluginManagerEvent::ScanFinished(_)))
    {
        assert!(Instant::now() < deadline, "bundled synth scan timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
    let lang = match std::env::var("KOMPONIST_SYNTH_UI_LANG").as_deref() {
        Ok("en") => Lang::En,
        _ => Lang::De,
    };
    let mut app = app_with(Project::new("Bundled synths"), host).with_lang(lang);
    app.dispatch(Action::Plugin(PluginAction::NewHexaTrack { preset: 1 }));
    let hexa = newest_instrument(&app).id;
    assert_eq!(newest_instrument(&app).plugin.format, PluginFormat::Clap);
    assert_eq!(
        newest_instrument(&app)
            .state
            .as_deref()
            .and_then(komponist_hexa::state::decode_clap_patch),
        komponist_hexa::preset_patch(1)
    );
    wait_for_verified(&mut app, hexa, BuiltinKind::Hexa, force_bridge);
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Button, &lang.devices_preset_name("E-Piano"));

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
    h.get_by_role_and_label(
        Role::Slider,
        &Field::Op(2, OpField::Attack).info_in(lang).name,
    );
    click(
        &mut h,
        Role::RadioButton,
        &format!("{}: 5", lang.devices_operator()),
    );
    h.get_by_role_and_label(
        Role::Slider,
        &Field::Op(4, OpField::Attack).info_in(lang).name,
    );
    snapshot(&mut h, "hexa-envelopes", lang);
    // The card reacts to the host's optimistic value. Commit Hexa's actual edit before
    // testing Volt's history so a late Hexa snapshot cannot become the next undo step.
    h.state_mut().engine_mut().host_mut().request_state(hexa);
    step_until(
        h.state_mut(),
        "Hexa algorithm captured in project history",
        |app| {
            let recorded = app.device_events().iter().any(|event| {
            matches!(event, PluginManagerEvent::StateSnapshot { device, .. } if *device == hexa)
        });
            recorded
                && app
                    .project()
                    .find_device(hexa)
                    .and_then(|(_, _, device)| device.state.as_deref())
                    .and_then(komponist_hexa::state::decode_clap_patch)
                    .is_some_and(|patch| komponist_hexa::Macro::Algorithm.get(&patch) == 7.0)
        },
    );

    let volt_kind = BuiltinKind::Internal(Internal::Volt);
    let plugin = h
        .state()
        .engine()
        .host()
        .catalog()
        .packaged(volt_kind)
        .unwrap()
        .plugin
        .clone();
    h.state_mut().dispatch(Action::Plugin(PluginAction::Add {
        plugin,
        target: DropTarget::NewTrack { index: None },
    }));
    let volt = newest_instrument(h.state()).id;
    wait_for_verified(h.state_mut(), volt, volt_kind, force_bridge);

    const ACID_PLUCK: usize = 2;
    let apply_volt = Action::Plugin(PluginAction::ApplyInternalPreset {
        device: volt,
        index: ACID_PLUCK,
    });
    let cutoff = alias(h.state(), volt, "cutoff");
    let cutoff_info = Internal::Volt
        .params()
        .iter()
        .find(|p| p.id == ParamId::new("cutoff"))
        .unwrap();
    let preset_cutoff = pinned_clap_state::to_clap(cutoff_info, 350.0);
    let default_cutoff = pinned_clap_state::to_clap(cutoff_info, cutoff_info.default);
    assert!(newest_instrument(h.state()).state.is_none());
    h.state_mut().dispatch(apply_volt);
    wait_for_value(
        h.state_mut(),
        volt,
        cutoff,
        preset_cutoff,
        "apply Volt preset",
    );
    h.run_steps(3);
    h.get_by_role_and_label(Role::Button, &lang.devices_preset_name("Acid Pluck"));
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
    wait_for_verified(h.state_mut(), volt, volt_kind, force_bridge);
    let snapshots = h
        .state_mut()
        .engine_mut()
        .host_mut()
        .snapshot_selected_states(&[volt], PATIENCE)
        .expect("snapshot after immediate redo");
    let state = pinned_clap_state::decode(&snapshots[0].1).unwrap();
    let actual = state["params"]["cutoff"]["f32"].as_f64().unwrap();
    assert!(
        (actual - preset_cutoff).abs() < 1e-6,
        "cutoff after immediate redo: {actual}"
    );

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
    h.state_mut().engine_mut().host_mut().request_state(volt);
    step_until(h.state_mut(), "Volt saved the LFO 2 edit", |app| {
        app.device_events().iter().any(|event| {
            let PluginManagerEvent::StateSnapshot { device, state } = event else {
                return false;
            };
            if *device != volt {
                return false;
            }
            let Some(snapshot) = pinned_clap_state::decode(state) else {
                return false;
            };
            let matches = |id: &str, expected: f64| {
                snapshot["params"][id]["f32"]
                    .as_f64()
                    .is_some_and(|actual| (actual - expected).abs() < 1e-6)
            };
            matches("lfo1_rate", one) && matches("lfo2_rate", two_edited)
        })
    });
    h.run_steps(3);
    snapshot(&mut h, "volt-envelopes", lang);
    h.state_mut().shutdown();
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

fn wait_for_verified(app: &mut App, device: DeviceId, kind: BuiltinKind, bridge: bool) {
    step_until(app, "authenticated synth ready", |app| {
        app.engine().host().verified_bundled_kind(device) == Some(kind)
            && app.engine().host().native_layout(device).is_some()
    });
    assert_eq!(app.engine().host().bridge_pid(device).is_some(), bridge);
    assert!(app.engine().host().state_warning(device).is_none());
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
    step_until(app, &format!("{phase} reached cutoff {value}"), |app| {
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
