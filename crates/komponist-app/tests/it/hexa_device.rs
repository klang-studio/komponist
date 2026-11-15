//! hexa's device layout (WP-86) in the app: the card with its two pages, the operator
//! selection (`{env_op}`), the carrier fill, a knob turn reaching the document, the presets, and
//! snapshots of both pages.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, PluginAction, UiAction};
use komponist_builtin::device::{self, Field};
use komponist_builtin::{Macro, Patch};
use komponist_i18n::Lang;
use komponist_model::params::ParamId;
use komponist_model::{DeviceId, Project};

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

/// A project with one hexa track (preset "E-Piano": six operators, carriers 1, 3 and 5) and the
/// dock on „Geräte“.
fn app_with_hexa(preset: usize) -> (App, DeviceId) {
    let mut app = App::headless(Project::new("Synth"));
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    let device = app.project().tracks[0].channel.devices[0].id;
    app.dispatch(Action::Plugin(PluginAction::ApplyHexaPreset {
        device,
        preset,
    }));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    (app, device)
}

fn patch_of(app: &App, device: DeviceId) -> Patch {
    let (_, _, d) = app.project().find_device(device).unwrap();
    komponist_builtin::decode_state(d.state.as_deref()).unwrap()
}

fn page(app: &mut App, device: DeviceId, page: usize) {
    app.dispatch(Action::Ui(UiAction::SetDevicePage { device, page }));
}

fn card<'h>(h: &'h Harness<'_, App>) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::Group, CARD)
}

/// The accessible name of the hexa card.
const CARD: &str = "Gerät hexa";

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
fn the_card_has_four_pages_and_the_preset_shows_in_the_head() {
    let (app, _) = app_with_hexa(1);
    let h = harness(app);
    for name in ["Operatoren", "Hüllkurven", "LFO", "Global"] {
        assert!(has(&h, Role::RadioButton, name), "page {name}");
    }
    // The factory preset is still recognised from the state: its name is on the dropdown.
    card(&h).get_by_role_and_label(Role::Button, &Lang::De.devices_preset_name("E-Piano"));
}

#[test]
fn every_factory_preset_still_loads_into_the_card() {
    for index in 0..komponist_builtin::PRESET_COUNT {
        let patch = komponist_builtin::preset_patch(index).unwrap();
        let state = komponist_builtin::encode_state(&patch);
        let back = komponist_builtin::decode_state(Some(&state)).unwrap();
        // Every card parameter reads a finite value from every preset.
        for p in device::params() {
            assert!(
                p.field.get(&back).is_finite(),
                "preset {index}, {}",
                p.info.id
            );
        }
    }
}

#[test]
fn clicking_an_operator_column_selects_it_and_the_envelope_page_follows() {
    let (mut app, device) = app_with_hexa(1);
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
    let (mut app, device) = app_with_hexa(1);
    page(&mut app, device, 0);
    let mut h = harness(app);
    let ratio = device::param(&ParamId::new("op2_ratio")).unwrap();
    let before = patch_of(h.state(), device).operators[1].ratio;
    let knob = card(&h)
        .get_by_role_and_label(Role::Slider, "Ratio 2")
        .rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    let after = patch_of(h.state(), device).operators[1].ratio;
    assert!(after > before + 0.5, "{before} → {after}");
    // The engine got the same value under the card parameter's wire id.
    let sent = h
        .state()
        .dispatched()
        .filter_map(|a| match a {
            Action::Plugin(PluginAction::SetParam {
                device: d,
                id,
                value,
            }) if *d == device && *id == ratio.wire => Some(*value),
            _ => None,
        })
        .last()
        .expect("SetParam for op2_ratio");
    assert!((sent - f64::from(after)).abs() < 1e-4, "{sent} vs {after}");
    assert_eq!(
        slider_value(&h, "Ratio 2"),
        ratio.info.format(f64::from(after))
    );
    // Everything else in the patch is untouched (here: the effects and modulation survive).
    let reference = {
        let mut p = komponist_builtin::preset_patch(1).unwrap();
        Field::Op(1, device::OpField::Ratio).set(&mut p, f64::from(after));
        p
    };
    assert_eq!(patch_of(h.state(), device), reference.sanitized());
}

#[test]
fn carriers_fill_in_the_track_colour_and_modulators_in_text_2() {
    // The layout asks `op{n}_output` per fader; the Algorithmus macro sets it from the table.
    let (mut app, device) = app_with_hexa(0);
    let algorithm = Field::Macro(Macro::Algorithm);
    app.dispatch(Action::Plugin(PluginAction::SetParam {
        device,
        id: algorithm.wire(),
        value: 1.0,
    }));
    let patch = patch_of(&app, device);
    for op in 0..6 {
        assert_eq!(
            patch.operators[op].output,
            device::is_carrier(1, op),
            "operator {}",
            op + 1
        );
    }
    // The fader colours are in the snapshot; here the card's own carrier values.
    let params = komponist_app::views::devices::model::hexa_params(
        &app.project().find_device(device).unwrap().2.clone(),
    );
    let carrier = |n: usize| {
        params
            .iter()
            .find(|p| p.info.id.as_str() == format!("op{n}_output"))
            .unwrap()
            .value
            >= 0.5
    };
    assert_eq!(
        [1, 2, 3, 4, 5, 6].map(carrier),
        [true, false, true, false, false, false]
    );
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
    let (mut app, device) = app_with_hexa(1);
    // A modulated operator to look at: operator 2 selected, a slower envelope.
    for (key, v) in [
        ("op2_attack", 0.08),
        ("op2_decay", 1.2),
        ("op2_sustain", 0.4),
    ] {
        let p = device::param(&ParamId::new(key)).unwrap();
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device,
            id: p.wire,
            value: v,
        }));
    }
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
    mask_cpu(&mut h);
    h.snapshot("devices-hexa-envelopes-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
