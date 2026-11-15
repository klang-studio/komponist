//! The device chain (WP-44) through `egui_kittest`: one test per row of the S05 notes that
//! concerns the chain (Geräte-Karte, Ziehen, Öffnen, Parameter, Lädt / abgestürzt), the missing
//! placeholder (S06), the state warning, hexa's macros (saved with the project), and
//! snapshots of every device state next to S05.

use crate::common;
use crate::fake_host;

use std::path::PathBuf;

use egui::accesskit::{Role, Toggled};
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Node;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::{BrowserSegment, DockTab, PluginView, PluginWindow};
use komponist_app::{Action, App, EditAction, PluginAction, SettingsAction, UiAction};
use komponist_i18n::{Lang, de};
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::params::ParamId;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, TrackId};
use komponist_plugins::{DeviceStatus, FailReason};

use fake_host::{FakeHost, param, plugin, screens_catalog};

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn harness_at(app: App, ppp: f32) -> Harness<'static, App> {
    komponist_ui::snapshot::builder::<App>(ppp)
        .with_size(vec2(W, H))
        .with_options(SnapshotOptions::new().output_path(snapshot_dir()))
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app)
}

fn harness(app: App) -> Harness<'static, App> {
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    h
}

/// The devices of the S05 chain on the Pad track.
#[derive(Debug, Clone, Copy)]
struct Chain {
    pad: TrackId,
    surge: DeviceId,
    eq: DeviceId,
    tal: DeviceId,
    hall: DeviceId,
    airw: DeviceId,
}

/// The demo with S05's chain on Pad: Surge XT, LSP Para EQ x8, TAL-Chorus-LX (bypassed),
/// Dragonfly Hall, Airwindows (`devicesChain()`).
fn s05_project() -> (Project, Chain) {
    let catalog = screens_catalog();
    let p = demo_project();
    let pad = p.tracks[3].id;
    let fx = |id: &str| NewDevice {
        role: DeviceRole::Effect,
        plugin: plugin(&catalog, id),
        state: None,
    };
    let (p, tal) = edits::insert_device(&p, ChannelRef::Track(pad), 2, fx("chorus")).unwrap();
    let p = edits::set_device_bypass(&p, tal, true).unwrap();
    let (p, airw) = edits::insert_device(&p, ChannelRef::Track(pad), 4, fx("airw")).unwrap();
    let ids: Vec<DeviceId> = p.tracks[3].channel.devices.iter().map(|d| d.id).collect();
    let chain = Chain {
        pad,
        surge: ids[0],
        eq: ids[1],
        tal,
        hall: ids[3],
        airw,
    };
    assert_eq!(ids[2], tal);
    assert_eq!(ids[4], airw);
    (p, chain)
}

/// The S05 host: Surge with its own GUI and 8 macros, the EQ without GUI, TAL with 3 parameters,
/// Dragonfly loading, Airwindows crashed (values and texts from `devicesChain()`).
fn s05_host(c: &Chain) -> FakeHost {
    let makros: Vec<_> = (0..8)
        .map(|i| param(i, &format!("Makro {}", i + 1), 0.5))
        .collect();
    let eq = [
        "Low Cut", "Low", "Mid", "Mid Freq", "High", "Q", "Output", "Mix",
    ];
    let tal = ["Rate", "Depth", "Mix"];
    FakeHost::new()
        .gui(c.surge)
        .params(
            c.surge,
            makros,
            &[
                (0.4, "40 %"),
                (0.7, "70 %"),
                (0.2, "20 %"),
                (0.5, "50 %"),
                (0.0, "0 %"),
                (0.6, "60 %"),
                (0.3, "30 %"),
                (0.85, "85 %"),
            ],
        )
        .params(
            c.eq,
            eq.iter()
                .enumerate()
                .map(|(i, n)| param(i as u32, n, 0.5))
                .collect(),
            &[
                (0.18, "80 Hz"),
                (0.5, "0.0 dB"),
                (0.42, "−1.5 dB"),
                (0.55, "1.2 kHz"),
                (0.62, "+2.0 dB"),
                (0.3, "0.7"),
                (0.5, "0.0 dB"),
                (1.0, "100 %"),
            ],
        )
        .gui(c.tal)
        .params(
            c.tal,
            tal.iter()
                .enumerate()
                .map(|(i, n)| param(i as u32, n, 0.5))
                .collect(),
            &[(0.3, "0.4 Hz"), (0.5, "50 %"), (0.4, "40 %")],
        )
        .status(c.hall, DeviceStatus::Loading)
        .status(
            c.airw,
            DeviceStatus::Crashed(FailReason::Exited {
                code: None,
                signal: Some(11),
            }),
        )
}

/// S05: Pad selected, the dock on "Geräte", Surge's window open, the browser searching "hall" in
/// Effekte.
fn s05_with_late() -> (App, Chain, fake_host::LateBlocks) {
    let (p, c) = s05_project();
    let host = s05_host(&c);
    let late = host.late.clone();
    let mut app = App::headless(p).with_device_host(Box::new(host));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![c.pad],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    (app, c, late)
}

fn s05() -> (App, Chain, fake_host::Statuses, fake_host::SetLog) {
    let (p, c) = s05_project();
    let host = s05_host(&c);
    let (statuses, set) = (host.statuses.clone(), host.set.clone());
    let mut app = App::headless(p).with_device_host(Box::new(host));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![c.pad],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    app.ui_mut()
        .plugin_windows
        .push(PluginWindow::new(c.surge, PluginView::Gui, None));
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(
        BrowserSegment::Effects,
    )));
    app.dispatch(Action::Ui(UiAction::SetBrowserQuery("hall".into())));
    // S01/S05 star Dragonfly Hall Reverb.
    app.dispatch(Action::Settings(SettingsAction::ToggleFavorite(plugin(
        &screens_catalog(),
        "hall",
    ))));
    (app, c, statuses, set)
}

/// The card group named `label` (`devices_card`).
fn card<'h>(h: &'h Harness<'_, App>, label: &'h str) -> Node<'h> {
    h.get_by_role_and_label(Role::Group, label)
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

// ------------------------------------------------------------------------------------------------
// Pointer helpers

fn press(h: &mut Harness<'_, App>, pos: Pos2, button: PointerButton, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

fn click_at(h: &mut Harness<'_, App>, pos: Pos2) {
    h.hover_at(pos);
    h.step();
    press(h, pos, PointerButton::Primary, true);
    press(h, pos, PointerButton::Primary, false);
    h.run_steps(2);
}

/// Press at `from`, move to `to` over several frames, release.
fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.step();
    press(h, from, PointerButton::Primary, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    press(h, to, PointerButton::Primary, false);
    h.run_steps(3);
}

fn chain_of(app: &App, track: TrackId) -> Vec<DeviceId> {
    app.project()
        .track(track)
        .unwrap()
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect()
}

fn dispatched<'a>(app: &'a App, f: impl Fn(&Action) -> bool + 'a) -> Vec<&'a Action> {
    app.dispatched().filter(|a| f(a)).collect()
}

/// The head of a card: the strip of its group's top `size.panel-header`.
fn head_of(h: &Harness<'_, App>, label: &str) -> Rect {
    let r = h.get_by_role_and_label(Role::Group, label).rect();
    Rect::from_min_size(r.min, vec2(r.width(), komponist_tokens::size::PANEL_HEADER))
}

// ------------------------------------------------------------------------------------------------
// S05 "Geräte-Karte": head with switch (bypass), name, format, open; body with macros or the
// generic parameters.

#[test]
fn a_card_has_switch_name_format_open_and_knobs() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    let surge_label = Lang::De.devices_card("Surge XT");
    let surge = card(&h, &surge_label);
    let switch = surge.get_by_role_and_label(Role::Switch, de::DEVICES_ACTIVE);
    assert_eq!(switch.accesskit_node().toggled(), Some(Toggled::True));
    surge.get_by_role_and_label(Role::Label, "Surge XT");
    surge.get_by_role_and_label(Role::Label, "CLAP");
    let open = surge.get_by_role_and_label(Role::Button, de::DEVICES_OPENED);
    assert_eq!(
        open.accesskit_node().toggled(),
        Some(Toggled::True),
        "Geöffnet is on"
    );
    let knobs: Vec<String> = surge
        .get_all_by_role(Role::Slider)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(knobs.len(), 8);
    assert_eq!(knobs[0], "Makro 1");
    // The plugin's value text.
    let eq_label = Lang::De.devices_card("LSP Para EQ x8");
    let eq = card(&h, &eq_label);
    let low_cut = eq.get_by_role_and_label(Role::Slider, "Low Cut");
    assert_eq!(low_cut.value().as_deref(), Some("80 Hz"));
    eq.get_by_role_and_label(Role::Button, de::DEVICES_PARAMS);

    // The switch is the bypass: TAL is bypassed, a click switches it on.
    let tal_label = Lang::De.devices_card("TAL-Chorus-LX");
    let tal_switch = card(&h, &tal_label)
        .get_by_role_and_label(Role::Switch, de::DEVICES_ACTIVE)
        .rect();
    assert!(h.state().project().find_device(c.tal).unwrap().2.bypass);
    click_at(&mut h, tal_switch.center());
    assert!(!h.state().project().find_device(c.tal).unwrap().2.bypass);
    assert!(h.state().dispatched().any(|a| *a
        == Action::Edit(EditAction::SetDeviceBypass {
            device: c.tal,
            bypass: false
        })));
    // Bypass back on from Surge's switch: Surge has no bypass yet.
    let surge_switch = card(&h, &surge_label)
        .get_by_role_and_label(Role::Switch, de::DEVICES_ACTIVE)
        .rect();
    click_at(&mut h, surge_switch.center());
    assert!(h.state().project().find_device(c.surge).unwrap().2.bypass);
}

#[test]
fn hexa_shows_its_card_and_turning_a_knob_is_saved() {
    let dir = std::env::temp_dir().join(format!("komponist-wp44-macro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Nachtfahrt.komponist");
    std::fs::write(
        &path,
        komponist_model::persist::to_json(&demo_project()).unwrap(),
    )
    .unwrap();
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Project(komponist_app::ProjectAction::OpenPath(
        path.clone(),
    )));
    // The headless host has only hexa: S06 lists the demo's other plugins; go on without them.
    assert!(matches!(
        app.ui().dialog,
        Some(komponist_app::ui_state::Dialog::MissingPlugins(_))
    ));
    app.dispatch(Action::Project(
        komponist_app::ProjectAction::ContinueWithMissing,
    ));
    let lead = app.project().tracks[2].id;
    let device = app.project().tracks[2].channel.devices[0].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![lead],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    let label = Lang::De.devices_card("hexa");
    let names: Vec<String> = card(&h, &label)
        .get_all_by_role(Role::Slider)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    // The first page, "Operatoren": a ratio knob per operator and the global knobs.
    for name in [
        "Ratio 1",
        "Ratio 6",
        "Feedback",
        "Lautstärke",
        "Glide",
        "Tune",
        "Spread",
    ] {
        assert!(names.iter().any(|n| n == name), "{name} in {names:?}");
    }
    let glide = komponist_builtin::device::param(&ParamId::new("glide")).unwrap();

    // Drag "Glide" up: the engine gets SetParam and the device state the new value.
    let before = glide.field.get(&patch_of(h.state(), device));
    let knob = card(&h, &label)
        .get_by_role_and_label(Role::Slider, "Glide")
        .rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    let after = glide.field.get(&patch_of(h.state(), device));
    assert!(after > before + 0.1, "{before} → {after}");
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::SetParam { device: d, id, .. })
            if *d == device && *id == glide.wire
    )));
    // The value text follows.
    let text = card(&h, &label)
        .get_by_role_and_label(Role::Slider, "Glide")
        .value()
        .unwrap();
    assert_eq!(text, glide.info.format(after));

    // Saved and loaded again: the value is in the file.
    h.state_mut()
        .dispatch(Action::Project(komponist_app::ProjectAction::Save));
    let loaded =
        komponist_model::persist::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (_, _, d) = loaded.find_device(device).unwrap();
    let patch = komponist_builtin::decode_state(d.state.as_deref()).unwrap();
    assert!((glide.field.get(&patch) - after).abs() < 1e-6);
    let _ = std::fs::remove_dir_all(&dir);
}

fn patch_of(app: &App, device: DeviceId) -> komponist_builtin::Patch {
    let (_, _, d) = app.project().find_device(device).unwrap();
    komponist_builtin::decode_state(d.state.as_deref()).unwrap()
}

// ------------------------------------------------------------------------------------------------
// S05 "Ziehen": reorder with a 2 px insert line; browser plugins dropped into the chain.

#[test]
fn dragging_a_card_reorders_the_chain() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    // TAL (index 2) behind Airwindows (the end).
    let from = head_of(&h, &Lang::De.devices_card("TAL-Chorus-LX"));
    let airw = h
        .get_by_role_and_label(
            Role::Group,
            &Lang::De.devices_card("Airwindows Consolidated"),
        )
        .rect();
    let from = pos2(from.left() + 110.0, from.center().y);
    drag(&mut h, from, pos2(airw.right() - 10.0, airw.center().y));
    assert_eq!(
        chain_of(h.state(), c.pad),
        [c.surge, c.eq, c.hall, c.airw, c.tal]
    );
    assert_eq!(
        dispatched(h.state(), |a| matches!(
            a,
            Action::Edit(EditAction::MoveDevice { .. })
        )),
        [&Action::Edit(EditAction::MoveDevice {
            device: c.tal,
            to_index: 4
        })]
    );
    // Back before the EQ; never before the instrument.
    let from = head_of(&h, &Lang::De.devices_card("TAL-Chorus-LX"));
    let surge = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("Surge XT"))
        .rect();
    drag(
        &mut h,
        pos2(from.left() + 110.0, from.center().y),
        pos2(surge.left() + 10.0, surge.center().y),
    );
    assert_eq!(
        chain_of(h.state(), c.pad),
        [c.surge, c.tal, c.eq, c.hall, c.airw]
    );
    // The instrument does not move.
    let surge_head = head_of(&h, &Lang::De.devices_card("Surge XT"));
    drag(
        &mut h,
        pos2(surge_head.left() + 90.0, surge_head.center().y),
        pos2(airw.right(), airw.center().y),
    );
    assert_eq!(chain_of(h.state(), c.pad)[0], c.surge);
}

#[test]
fn the_insert_line_shows_while_dragging() {
    let (app, ..) = s05();
    let mut h = harness_at(app, 1.0);
    h.run_steps(3);
    // Hold Dragonfly over the gap between Surge and the EQ: S05's line after the EQ is the slot
    // before TAL; here the drag of the hall card to that place.
    let from = head_of(&h, &Lang::De.devices_card("Dragonfly Hall Reverb"));
    let tal = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("TAL-Chorus-LX"))
        .rect();
    let start = pos2(from.left() + 110.0, from.center().y);
    let to = pos2(tal.left() + 20.0, tal.center().y);
    h.hover_at(start);
    h.step();
    press(&mut h, start, PointerButton::Primary, true);
    h.step();
    for i in 1..=8 {
        h.hover_at(start + (to - start) * (i as f32 / 8.0));
        h.step();
    }
    mask_cpu(&mut h);
    h.snapshot("devices-drag-1x");
    press(&mut h, to, PointerButton::Primary, false);
    h.run_steps(2);
}

#[test]
fn a_browser_plugin_dropped_on_the_chain_goes_into_the_marked_slot() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    // "Dragonfly Plate Reverb" from the search result, between Surge and the EQ.
    let row = h
        .get_by_role_and_label(Role::ListItem, "Dragonfly Plate Reverb")
        .rect();
    let eq = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("LSP Para EQ x8"))
        .rect();
    drag(&mut h, row.center(), pos2(eq.left() + 20.0, eq.center().y));
    let chain = chain_of(h.state(), c.pad);
    assert_eq!(chain.len(), 6);
    let (_, _, new) = h.state().project().find_device(chain[1]).unwrap();
    assert_eq!(new.plugin.name, "Dragonfly Plate Reverb");
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::Add {
            target: DropTarget::Chain { index: 1, .. },
            ..
        })
    )));
    // An instrument dropped on the chain replaces the instrument.
    let row = h.get_by_role_and_label(Role::ListItem, "Dragonfly Plate Reverb");
    let _ = row;
    let p = plugin(&screens_catalog(), "vital");
    let mid = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("LSP Para EQ x8"))
        .rect()
        .center();
    let from = pos2(100.0, 600.0);
    h.hover_at(from);
    h.step();
    press(&mut h, from, PointerButton::Primary, true);
    h.step();
    egui::DragAndDrop::set_payload(&h.ctx, p);
    for i in 1..=4 {
        h.hover_at(from + (mid - from) * (i as f32 / 4.0));
        h.step();
    }
    press(&mut h, mid, PointerButton::Primary, false);
    h.run_steps(3);
    let first = chain_of(h.state(), c.pad)[0];
    let (_, _, d) = h.state().project().find_device(first).unwrap();
    assert_eq!(d.plugin.name, "Vital");
}

// ------------------------------------------------------------------------------------------------
// S05 "Öffnen" and "Parameter": the plugin's window, or the generic view for plugins without GUI.

#[test]
fn open_toggles_the_plugin_window() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    let surge_label = Lang::De.devices_card("Surge XT");
    let opened = card(&h, &surge_label)
        .get_by_role_and_label(Role::Button, de::DEVICES_OPENED)
        .rect();
    click_at(&mut h, opened.center());
    assert!(h.state().ui().plugin_windows.is_empty());
    card(&h, &surge_label).get_by_role_and_label(Role::Button, de::DEVICES_OPEN);
    // The EQ has no own GUI: "Parameter" opens the generic view.
    let eq_label = Lang::De.devices_card("LSP Para EQ x8");
    let params = card(&h, &eq_label)
        .get_by_role_and_label(Role::Button, de::DEVICES_PARAMS)
        .rect();
    click_at(&mut h, params.center());
    let w = &h.state().ui().plugin_windows;
    assert_eq!(
        (w.len(), w[0].device, w[0].view),
        (1, c.eq, PluginView::Params)
    );
    card(&h, &eq_label).get_by_role_and_label(Role::Button, de::DEVICES_OPENED);
}

#[test]
fn generic_knobs_set_the_plugin_parameter() {
    let (app, c, _, set) = s05();
    let mut h = harness(app);
    let eq_label = Lang::De.devices_card("LSP Para EQ x8");
    let knob = card(&h, &eq_label)
        .get_by_role_and_label(Role::Slider, "Low Cut")
        .rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 50.0));
    let set = set.lock().unwrap();
    let last = set.last().expect("set_param");
    assert_eq!((last.0, last.1), (c.eq, 0));
    assert!(last.2 > 0.18 + 0.1, "{}", last.2);
    // Bypassed TAL: the body is dimmed, but the knobs stay operable (device handoff §3).
    let tal_label = Lang::De.devices_card("TAL-Chorus-LX");
    let rate = card(&h, &tal_label).get_by_role_and_label(Role::Slider, "Rate");
    assert!(!rate.accesskit_node().is_disabled());
}

// ------------------------------------------------------------------------------------------------
// S05 "Lädt / abgestürzt", S06 "Fehlt", the state warning.

#[test]
fn loading_shows_progress_then_the_knobs() {
    let (app, c, statuses, _) = s05();
    let mut h = harness(app);
    let label = Lang::De.devices_card("Dragonfly Hall Reverb");
    let hall = card(&h, &label);
    hall.get_by_role_and_label(Role::Label, de::DEVICES_LOADING);
    hall.get_by_role_and_label(Role::ProgressIndicator, de::DEVICES_LOADING);
    assert!(
        hall.query_by_label(de::DEVICES_PARAMS).is_none()
            && hall.query_by_label(de::DEVICES_OPEN).is_none(),
        "no open button while loading"
    );
    statuses.lock().unwrap().insert(c.hall, DeviceStatus::Ok);
    h.run_steps(2);
    let hall = card(&h, &label);
    assert!(hall.query_by_label(de::DEVICES_LOADING).is_none());
    hall.get_by_role_and_label(Role::Button, de::DEVICES_PARAMS);
}

#[test]
fn crashed_shows_the_red_edge_and_reloads() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    let label = Lang::De.devices_card("Airwindows Consolidated");
    let airw = card(&h, &label);
    airw.get_by_role_and_label(Role::Label, de::DEVICES_CRASHED);
    airw.get_by_role_and_label(Role::Label, de::DEVICES_CRASHED_TEXT);
    let reload = airw
        .get_by_role_and_label(Role::Button, de::DEVICES_RELOAD)
        .rect();
    click_at(&mut h, reload.center());
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Plugin(PluginAction::Reload(c.airw)))
    );
}

#[test]
fn a_failed_load_or_a_hang_does_not_read_abgestuerzt_on_the_card() {
    let (app, c, statuses, _) = s05();
    let mut h = harness(app);
    let label = Lang::De.devices_card("Airwindows Consolidated");
    statuses.lock().unwrap().insert(
        c.airw,
        DeviceStatus::Crashed(FailReason::LoadFailed("no such file".into())),
    );
    h.run_steps(2);
    let airw = card(&h, &label);
    assert!(airw.query_by_label(de::DEVICES_CRASHED).is_none());
    airw.get_by_role_and_label(Role::Label, de::DEVICES_LOAD_FAILED);
    assert!(airw.query_by_label_contains("no such file").is_some());
    statuses
        .lock()
        .unwrap()
        .insert(c.airw, DeviceStatus::Crashed(FailReason::Unresponsive));
    h.run_steps(2);
    let airw = card(&h, &label);
    airw.get_by_role_and_label(Role::Label, de::DEVICES_UNRESPONSIVE);
    airw.get_by_role_and_label(Role::Button, de::DEVICES_RELOAD);
}

#[test]
fn a_missing_plugin_is_a_red_placeholder_that_keeps_its_state() {
    // The built-in host knows only hexa: Lead's effects are missing (S06).
    let mut app = App::headless(demo_project());
    let lead = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![lead],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    let label = Lang::De.devices_card("LSP Para EQ x8");
    let eq = card(&h, &label);
    eq.get_by_role_and_label(Role::Label, de::DEVICES_MISSING);
    eq.get_by_role_and_label(Role::Label, de::DEVICES_MISSING_TEXT);
    assert!(eq.query_by_role(Role::Slider).is_none());
    assert!(
        eq.query_by_role_and_label(Role::Button, de::DEVICES_OPEN)
            .is_none()
    );
    h.run_steps(1);
    let mut results = SnapshotResults::new();
    mask_cpu(&mut h);
    h.snapshot("devices-missing-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

#[test]
fn a_state_that_did_not_load_is_shown_on_the_card() {
    let (p, c) = s05_project();
    let host = s05_host(&c).warning(c.eq, "state rejected by the plugin");
    let mut app = App::headless(p).with_device_host(Box::new(host));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![c.pad],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    let label = Lang::De.devices_card("LSP Para EQ x8");
    let eq = card(&h, &label);
    eq.get_by_role_and_label(Role::Label, de::DEVICES_STATE_WARNING);
    // The knobs stay.
    eq.get_by_role_and_label(Role::Slider, "Low Cut");
    h.run_steps(1);
    let mut results = SnapshotResults::new();
    mask_cpu(&mut h);
    h.snapshot("devices-state-warning-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

/// WP-81 auto-layout: a plugin with 20 parameters gets 12 knobs, the card draws the rows that
/// fit its body (2 until WP-82), the plugin frame's generic view keeps all of them.
#[test]
fn a_plugin_with_many_parameters_shows_the_rows_that_fit() {
    let (p, c) = s05_project();
    let infos: Vec<_> = (0..20)
        .map(|i| param(i, &format!("Regler {}", i + 1), 0.5))
        .collect();
    let values: Vec<(f64, String)> = (0..20)
        .map(|i| (f64::from(i) / 20.0, format!("{} %", i * 5)))
        .collect();
    let values: Vec<(f64, &str)> = values.iter().map(|(v, t)| (*v, t.as_str())).collect();
    let host = s05_host(&c).params(c.eq, infos, &values);
    let mut app = App::headless(p).with_device_host(Box::new(host));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![c.pad],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    let label = Lang::De.devices_card("LSP Para EQ x8");
    let eq = card(&h, &label);
    let knobs: Vec<String> = eq
        .get_all_by_role(Role::Slider)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(
        knobs.len(),
        12,
        "the auto-layout: 3 rows of 4 fit the 216 px body"
    );
    assert_eq!(
        (knobs[0].as_str(), knobs[11].as_str()),
        ("Regler 1", "Regler 12")
    );
    assert_eq!(
        eq.get_by_role_and_label(Role::Slider, "Regler 8")
            .value()
            .as_deref(),
        Some("35 %")
    );
    h.run_steps(1);
    let mut results = SnapshotResults::new();
    mask_cpu(&mut h);
    h.snapshot("devices-auto-layout-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// Header, context menu, selection, scrolling.

#[test]
fn header_shows_the_track_and_opens_the_add_menu() {
    let (mut app, ..) = s05();
    app.dispatch(Action::Ui(UiAction::TogglePanel(
        komponist_app::ui_state::Panel::Browser,
    )));
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(BrowserSegment::All)));
    let mut h = harness(app);
    // The chain head lists the devices as `a › b › c`; a name scrolls to its device.
    for name in ["Surge XT", "LSP Para EQ x8", "Airwindows Consolidated"] {
        h.get_by_role_and_label(Role::Button, &Lang::De.devices_reveal(name));
    }
    // „+ Gerät“ opens the add menu below the button instead of the browser.
    let add = h
        .get_by_role_and_label(Role::Button, de::DEVICES_ADD)
        .rect();
    click_at(&mut h, add.center());
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, de::DEVICES_ADD_SEARCH);
    assert_eq!(
        h.state().ui().browser.segment,
        BrowserSegment::All,
        "the browser is not touched"
    );
}

#[test]
fn click_selects_and_the_context_menu_removes() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    let head = head_of(&h, &Lang::De.devices_card("LSP Para EQ x8"));
    let at = pos2(head.left() + 90.0, head.center().y);
    click_at(&mut h, at);
    assert!(h.state().ui().selection.devices.contains(&c.eq));
    h.hover_at(at);
    h.step();
    press(&mut h, at, PointerButton::Secondary, true);
    press(&mut h, at, PointerButton::Secondary, false);
    h.run_steps(2);
    let remove = h.get_by_label(de::DEVICES_REMOVE).rect();
    click_at(&mut h, remove.center());
    assert_eq!(chain_of(h.state(), c.pad), [c.surge, c.tal, c.hall, c.airw]);
}

#[test]
fn shift_wheel_in_the_body_scrolls_a_long_chain() {
    // Wide side panels leave the chain less room than its 1364 px.
    let (mut app, ..) = s05();
    app.ui_mut().layout.browser_width = komponist_tokens::size::browser::MAX;
    app.ui_mut().layout.inspector_width = komponist_tokens::size::inspector::MAX;
    let mut h = harness(app);
    let first = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("Surge XT"))
        .rect();
    h.hover_at(first.center());
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        modifiers: Modifiers::SHIFT,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    h.run_steps(3);
    let scroll = h.state().ui().devices.scroll_x;
    assert!(scroll > 0.0, "{scroll}");
    let moved = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("Surge XT"))
        .rect();
    assert!(moved.left() < first.left());
}

// ------------------------------------------------------------------------------------------------
// A real test plugin through the bridge: added from the browser, its card goes Lädt → Ok.

#[test]
fn a_test_plugin_added_from_the_browser_loads_and_shows_its_parameters() {
    let Some(dirs) = common::fixture_dirs() else {
        return;
    };
    let dir = common::scratch("wp44-load");
    let host = komponist_app::device_host::PluginHost::new(common::config(&dir));
    let mut app = common::app_with(demo_project(), host);
    app.engine_mut().host_mut().start_scan(&dirs);
    common::step_until(&mut app, "scan", |a| {
        a.engine()
            .host()
            .catalog()
            .entries()
            .iter()
            .any(|e| e.plugin.id == common::FX)
    });
    let lead = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![lead],
        mode: SelectMode::Replace,
    })));
    let mut h = harness(app);
    // Strg F, type, Enter: the effect goes onto Lead, the dock shows its chain.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::F);
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Suchen")
        .type_text("kp-test-fx");
    h.run_steps(2);
    // One frame: Enter adds the device and the sync starts its bridge; the frame after it draws
    // the card, still loading.
    h.key_press(egui::Key::Enter);
    h.step();
    assert_eq!(h.state().ui().layout.dock_tab, DockTab::Devices);
    let device = *chain_of(h.state(), lead).last().unwrap();
    // On a fast machine the bridge is already up one frame later.
    assert!(
        matches!(
            h.state().engine().device_status(device),
            DeviceStatus::Loading | DeviceStatus::Ok
        ),
        "{:?}",
        h.state().engine().device_status(device)
    );
    // The test plugin loads within a frame or two, so the Lädt card itself is checked with the
    // fake host (`loading_shows_progress_then_the_knobs`) and live with Surge XT.
    let label = Lang::De.devices_card("kp-test-fx");
    let deadline = std::time::Instant::now() + common::PATIENCE;
    loop {
        h.step();
        let status = h.state().engine().device_status(device);
        if status == DeviceStatus::Ok
            && card(&h, &label)
                .query_by_role_and_label(Role::Slider, "Gain")
                .is_some()
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no Ok card; status {status:?}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let gain = card(&h, &label).get_by_role_and_label(Role::Slider, "Gain");
    let text = gain.value().unwrap_or_default();
    assert!(text.contains("dB"), "the plugin's text: {text}");
    h.state_mut().shutdown();
}

/// The same screen in English (#5): "Hall" shows as "Reverb", the parameters have English names.
#[test]
fn snapshot_next_to_s05_in_english() {
    let (app, ..) = s05();
    let mut h = harness_at(app.with_lang(komponist_i18n::Lang::En), 1.0);
    h.run_steps(4);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::en::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    h.snapshot("devices-s05-en-1x");
}

#[test]
fn snapshots_next_to_s05() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let (app, ..) = s05();
        let mut h = harness_at(app, ppp);
        h.run_steps(4);
        mask_cpu(&mut h);
        h.snapshot(format!("devices-s05-{ppp}x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

#[test]
fn a_card_whose_blocks_arrive_late_reads_zu_spaet_for_a_while() {
    let (app, c, late) = s05_with_late();
    let mut h = harness(app);
    let label = Lang::De.devices_card("Surge XT");
    assert!(card(&h, &label).query_by_label("zu spät").is_none());
    late.lock().unwrap().insert(c.surge, 2);
    h.run_steps(2);
    card(&h, &label).get_by_role_and_label(Role::Label, "zu spät");
    h.run_steps(150);
    assert!(card(&h, &label).query_by_label("zu spät").is_none());
}

#[test]
fn a_card_that_appears_is_scrolled_into_view() {
    use komponist_app::ui_state::Reveal;
    use komponist_app::views::devices::{metrics, model};
    let (p, c) = s05_project();
    let track = p.track(c.pad).unwrap();
    let mut cards = model::cards(Lang::De, track, &FakeHost::new(), &[]);
    for card in &mut cards {
        card.width = card.frame_width(0.0);
    }
    let widths: Vec<f32> = cards.iter().map(|c| c.width).collect();
    let spans = model::spans(&widths, metrics::BODY_PAD, metrics::CARD_GAP);
    let scroll = |i: usize, from: f32| {
        model::reveal_scroll(spans[i], Reveal::Visible, 600.0, from, metrics::BODY_PAD)
    };
    // A narrow body: the last card lies right of it; the first is already visible.
    assert!(scroll(cards.len() - 1, 0.0) > 0.0);
    assert_eq!(scroll(0, 0.0), 0.0);
    // Scrolled far right, the first card comes back.
    assert!(scroll(0, 900.0) < 900.0);
}

// ------------------------------------------------------------------------------------------------
// The drop slot is an add button: a menu with a search field (owner request 2026-11-15).

mod add_menu {
    use super::*;
    use egui::Key;
    use komponist_app::views::devices::model::{Placement, place_plugin};
    use komponist_builtin::Internal;

    /// A project with one hexa track, the dock on „Geräte“; the built-in catalog.
    fn app_with_track() -> (App, TrackId) {
        let mut app = App::headless(Project::new("Menü"));
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
        (app, track)
    }

    fn names_of(app: &App, track: TrackId) -> Vec<String> {
        app.project()
            .track(track)
            .unwrap()
            .channel
            .devices
            .iter()
            .map(|d| d.plugin.name.clone())
            .collect()
    }

    fn slot<'h>(h: &'h Harness<'_, App>) -> Node<'h> {
        h.get_by_role_and_label(Role::Button, de::DEVICES_ADD_DEVICE)
    }

    fn search<'h>(h: &'h Harness<'_, App>) -> Option<Node<'h>> {
        h.query_by_role_and_label(Role::TextInput, de::DEVICES_ADD_SEARCH)
    }

    fn option_names(h: &Harness<'_, App>) -> Vec<String> {
        h.query_all_by_role(Role::ListBoxOption)
            .map(|n| n.accesskit_node().label().unwrap_or_default())
            .collect()
    }

    fn open_by_click(h: &mut Harness<'_, App>) {
        let at = slot(h).rect().center();
        click_at(h, at);
        h.run_steps(3);
    }

    fn type_text(h: &mut Harness<'_, App>, text: &str) {
        h.event(Event::Text(text.into()));
        h.run_steps(2);
    }

    fn key(h: &mut Harness<'_, App>, key: Key) {
        h.key_press(key);
        h.run_steps(2);
    }

    #[test]
    fn a_click_on_the_slot_opens_the_menu_with_the_search_field_focused() {
        let (app, _) = app_with_track();
        let mut h = harness(app);
        assert!(search(&h).is_none());
        open_by_click(&mut h);
        let field = search(&h).expect("the search field");
        assert!(field.accesskit_node().is_focused(), "focused right away");
        let listed = option_names(&h);
        for name in ["hexa", Internal::Eq.name(), Internal::Arpeggiator.name()] {
            assert!(listed.iter().any(|n| n == name), "{name} in {listed:?}");
        }
    }

    #[test]
    fn enter_and_space_on_the_focused_slot_open_the_menu() {
        for key_ in [Key::Enter, Key::Space] {
            let (app, _) = app_with_track();
            let mut h = harness(app);
            slot(&h).focus();
            h.run_steps(2);
            key(&mut h, key_);
            assert!(search(&h).is_some(), "{key_:?} opens it");
        }
    }

    #[test]
    fn typing_filters_with_the_browsers_matching_and_enter_adds_the_first_hit() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        // „delay“ is Delay's name and a feature of Hall, as in the browser's search.
        type_text(&mut h, "delay");
        assert_eq!(
            option_names(&h),
            [Internal::Delay.name(), Internal::Hall.name()]
        );
        key(&mut h, Key::Enter);
        assert!(search(&h).is_none(), "the menu closes");
        assert_eq!(names_of(h.state(), track), ["hexa", Internal::Delay.name()]);
    }

    #[test]
    fn no_match_shows_a_hint_and_enter_adds_nothing() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, "zzzz");
        assert!(option_names(&h).is_empty());
        h.get_by_role_and_label(Role::Label, de::DEVICES_ADD_NO_MATCH);
        key(&mut h, Key::Enter);
        assert_eq!(names_of(h.state(), track), ["hexa"]);
    }

    #[test]
    fn the_arrow_keys_move_the_selection() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, "delay");
        let selected = |h: &Harness<'_, App>| {
            h.query_all_by_role(Role::ListBoxOption)
                .find(|n| n.accesskit_node().is_selected() == Some(true))
                .and_then(|n| n.accesskit_node().label().map(|l| l.to_string()))
        };
        assert_eq!(selected(&h).as_deref(), Some(Internal::Delay.name()));
        key(&mut h, Key::ArrowDown);
        assert_eq!(selected(&h).as_deref(), Some(Internal::Hall.name()));
        key(&mut h, Key::ArrowDown);
        assert_eq!(
            selected(&h).as_deref(),
            Some(Internal::Hall.name()),
            "stops at the end"
        );
        key(&mut h, Key::ArrowUp);
        key(&mut h, Key::ArrowDown);
        key(&mut h, Key::Enter);
        assert_eq!(names_of(h.state(), track), ["hexa", Internal::Hall.name()]);
    }

    #[test]
    fn a_click_on_a_device_adds_it() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        let at = h
            .get_by_role_and_label(Role::ListBoxOption, Internal::Limiter.name())
            .rect()
            .center();
        click_at(&mut h, at);
        assert!(search(&h).is_none());
        assert_eq!(
            names_of(h.state(), track),
            ["hexa", Internal::Limiter.name()]
        );
    }

    #[test]
    fn a_midi_effect_goes_in_front_of_the_instrument() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, Internal::Arpeggiator.name());
        key(&mut h, Key::Enter);
        assert_eq!(
            names_of(h.state(), track),
            [Internal::Arpeggiator.name(), "hexa"]
        );
    }

    #[test]
    fn an_instrument_replaces_the_instrument_or_fills_a_track_without_one() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, Internal::Sampler.name());
        key(&mut h, Key::Enter);
        assert_eq!(names_of(h.state(), track), [Internal::Sampler.name()]);
        // Without an instrument: an EQ is there, the instrument comes in front of it.
        let (mut app, track) = app_with_track();
        let hexa = app.project().track(track).unwrap().channel.devices[0].id;
        app.dispatch(Action::Plugin(PluginAction::Add {
            plugin: Internal::Eq.plugin_ref(),
            target: DropTarget::Track(track),
        }));
        app.dispatch(Action::Edit(EditAction::RemoveDevice(hexa)));
        assert_eq!(names_of(&app, track), [Internal::Eq.name()]);
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, Internal::Sampler.name());
        key(&mut h, Key::Enter);
        assert_eq!(
            names_of(h.state(), track),
            [Internal::Sampler.name(), Internal::Eq.name()]
        );
    }

    #[test]
    fn the_rule_is_the_one_of_the_drop_and_the_master_takes_effects_only() {
        // `place_plugin(role, master, slot, first, midi)`: a track with `midi` MIDI effects, an
        // instrument and the slot at the end of 5 devices.
        let at_end = |role, master, first, midi| place_plugin(role, master, 5, first, midi);
        assert_eq!(
            at_end(DeviceRole::Effect, false, 3, 2),
            Some(Placement::Line(5))
        );
        assert_eq!(
            at_end(DeviceRole::MidiEffect, false, 3, 2),
            Some(Placement::Line(2))
        );
        assert_eq!(
            at_end(DeviceRole::Instrument, false, 3, 2),
            Some(Placement::Instrument(2))
        );
        // No instrument: it goes in front of the audio effects.
        assert_eq!(
            at_end(DeviceRole::Instrument, false, 2, 2),
            Some(Placement::Line(2))
        );
        // The master chain: audio effects at the end, nothing else.
        assert_eq!(
            at_end(DeviceRole::Effect, true, 0, 0),
            Some(Placement::Line(5))
        );
        assert_eq!(at_end(DeviceRole::MidiEffect, true, 0, 0), None);
        assert_eq!(at_end(DeviceRole::Instrument, true, 0, 0), None);
    }

    #[test]
    fn escape_and_a_click_outside_close_the_menu_without_a_change() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, "delay");
        key(&mut h, Key::Escape);
        assert!(search(&h).is_none());
        assert_eq!(names_of(h.state(), track), ["hexa"]);
        open_by_click(&mut h);
        assert!(search(&h).is_some());
        click_at(&mut h, pos2(60.0, 300.0));
        assert!(search(&h).is_none());
        assert_eq!(names_of(h.state(), track), ["hexa"]);
    }

    #[test]
    fn one_undo_reverts_the_add() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, "dela");
        key(&mut h, Key::Enter);
        assert_eq!(names_of(h.state(), track).len(), 2);
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        assert_eq!(names_of(h.state(), track), ["hexa"]);
    }

    #[test]
    fn the_add_goes_through_the_action_of_the_browser_drop() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, "dela");
        key(&mut h, Key::Enter);
        assert!(h.state().dispatched().any(|a| matches!(
            a,
            Action::Plugin(PluginAction::Add {
                target: DropTarget::Chain {
                    channel: ChannelRef::Track(t),
                    index: 1,
                },
                ..
            }) if *t == track
        )));
    }

    #[test]
    fn a_browser_drag_onto_the_slot_still_drops() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        let slot_rect = slot(&h).rect();
        let eq = Internal::Eq.plugin_ref();
        let from = pos2(100.0, 600.0);
        h.hover_at(from);
        h.step();
        press(&mut h, from, PointerButton::Primary, true);
        h.step();
        egui::DragAndDrop::set_payload(&h.ctx, eq);
        for i in 1..=4 {
            h.hover_at(from + (slot_rect.center() - from) * (i as f32 / 4.0));
            h.step();
        }
        press(&mut h, slot_rect.center(), PointerButton::Primary, false);
        h.run_steps(3);
        assert!(search(&h).is_none(), "a drop is not a click");
        assert_eq!(names_of(h.state(), track), ["hexa", Internal::Eq.name()]);
    }

    #[test]
    fn snapshot_of_the_open_menu() {
        let (app, _) = app_with_track();
        let mut h = harness(app);
        open_by_click(&mut h);
        type_text(&mut h, "e");
        mask_cpu(&mut h);
        let mut results = SnapshotResults::new();
        h.snapshot("devices-add-menu-1x");
        results.extend_harness(&mut h);
        results.unwrap();
    }

    #[test]
    fn snapshot_of_the_hovered_slot() {
        let (app, _) = app_with_track();
        let mut h = harness(app);
        let at = slot(&h).rect().center();
        h.hover_at(at);
        h.run_steps(3);
        mask_cpu(&mut h);
        let mut results = SnapshotResults::new();
        h.snapshot("devices-add-slot-hover-1x");
        results.extend_harness(&mut h);
        results.unwrap();
    }

    /// „+ Gerät“ in the dock header opens the menu below the button, for the selected track.
    #[test]
    fn the_header_button_opens_the_menu_at_the_button_and_adds_to_the_selected_track() {
        let (app, track) = app_with_track();
        let mut h = harness(app);
        let button = h
            .get_by_role_and_label(Role::Button, de::DEVICES_ADD)
            .rect();
        click_at(&mut h, button.center());
        h.run_steps(3);
        assert!(search(&h).expect("search").accesskit_node().is_focused());
        // The dock sits at the bottom of the window, so the menu opens above the button; either
        // way it never covers it and starts on its left edge (plus the frame's padding).
        let field = search(&h).expect("search").rect();
        assert!(!field.intersects(button), "{field:?} covers {button:?}");
        assert!(
            (field.left() - button.left()).abs() < 8.0,
            "{field:?} {button:?}"
        );
        assert!(field.right() <= h.ctx.content_rect().right());
        type_text(&mut h, "delay");
        key(&mut h, Key::Enter);
        assert!(search(&h).is_none());
        assert_eq!(names_of(h.state(), track), ["hexa", Internal::Delay.name()]);
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        assert_eq!(names_of(h.state(), track), ["hexa"]);
        // A second click on the button closes an open menu.
        click_at(&mut h, button.center());
        h.run_steps(3);
        assert!(search(&h).is_some());
        click_at(&mut h, button.center());
        h.run_steps(3);
        assert!(search(&h).is_none());
    }

    #[test]
    fn snapshot_of_the_menu_at_the_header_button() {
        let (app, _) = app_with_track();
        let mut h = harness(app);
        let button = h
            .get_by_role_and_label(Role::Button, de::DEVICES_ADD)
            .rect();
        click_at(&mut h, button.center());
        h.run_steps(3);
        mask_cpu(&mut h);
        let mut results = SnapshotResults::new();
        h.snapshot("devices-add-menu-header-1x");
        results.extend_harness(&mut h);
        results.unwrap();
    }
}
