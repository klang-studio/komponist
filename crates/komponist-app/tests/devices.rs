//! The device chain (WP-44) through `egui_kittest`: one test per row of the S05 notes that
//! concerns the chain (Geräte-Karte, Ziehen, Öffnen, Parameter, Lädt / abgestürzt), the missing
//! placeholder (S06), the state warning, pisynth's macros (saved with the project), and
//! snapshots of every device state next to S05.

mod common;
mod fake_host;

use std::path::PathBuf;

use egui::accesskit::{Role, Toggled};
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::Node;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::{BrowserSegment, DockTab, PluginView, PluginWindow};
use komponist_app::views::devices::model::names;
use komponist_app::{Action, App, EditAction, PluginAction, SettingsAction, UiAction};
use komponist_builtin::Macro;
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, TrackId};
use komponist_plugins::{DeviceStatus, FailReason};

use fake_host::{FakeHost, param, plugin, screens_catalog};

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

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

/// The card group named `label` (`names::card`).
fn card<'h>(h: &'h Harness<'_, App>, label: &'h str) -> Node<'h> {
    h.get_by_role_and_label(Role::Group, label)
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_app::panels::transport::names::CPU)
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
    let surge_label = names::card("Surge XT");
    let surge = card(&h, &surge_label);
    let switch = surge.get_by_role_and_label(Role::Switch, names::ACTIVE);
    assert_eq!(switch.accesskit_node().toggled(), Some(Toggled::True));
    surge.get_by_role_and_label(Role::Label, "Surge XT");
    surge.get_by_role_and_label(Role::Label, "CLAP");
    let open = surge.get_by_role_and_label(Role::Button, names::OPENED);
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
    let eq_label = names::card("LSP Para EQ x8");
    let eq = card(&h, &eq_label);
    let low_cut = eq.get_by_role_and_label(Role::Slider, "Low Cut");
    assert_eq!(low_cut.value().as_deref(), Some("80 Hz"));
    eq.get_by_role_and_label(Role::Button, names::PARAMS);

    // The switch is the bypass: TAL is bypassed, a click switches it on.
    let tal_label = names::card("TAL-Chorus-LX");
    let tal_switch = card(&h, &tal_label)
        .get_by_role_and_label(Role::Switch, names::ACTIVE)
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
        .get_by_role_and_label(Role::Switch, names::ACTIVE)
        .rect();
    click_at(&mut h, surge_switch.center());
    assert!(h.state().project().find_device(c.surge).unwrap().2.bypass);
}

#[test]
fn pisynth_shows_its_eight_macros_and_turning_one_is_saved() {
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
    // The headless host has only pisynth: S06 lists the demo's other plugins; go on without them.
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
    let label = names::card("pisynth");
    let names: Vec<String> = card(&h, &label)
        .get_all_by_role(Role::Slider)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    let expected: Vec<&str> = Macro::ALL.iter().map(|m| m.name()).collect();
    assert_eq!(names, expected);

    // Drag "Sustain" up: the engine gets SetParam and the device state the new value.
    let before = Macro::Sustain.get(&patch_of(h.state(), device));
    let knob = card(&h, &label)
        .get_by_role_and_label(Role::Slider, "Sustain")
        .rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 40.0));
    let after = Macro::Sustain.get(&patch_of(h.state(), device));
    assert!(after > before + 0.1, "{before} → {after}");
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Plugin(PluginAction::SetParam { device: d, id, .. })
            if *d == device && *id == Macro::Sustain.id()
    )));
    // The value text follows.
    let text = card(&h, &label)
        .get_by_role_and_label(Role::Slider, "Sustain")
        .value()
        .unwrap();
    assert_eq!(text, Macro::Sustain.text(after));

    // Saved and loaded again: the macro value is in the file.
    h.state_mut()
        .dispatch(Action::Project(komponist_app::ProjectAction::Save));
    let loaded =
        komponist_model::persist::from_json(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let (_, _, d) = loaded.find_device(device).unwrap();
    let patch = komponist_builtin::decode_state(d.state.as_deref()).unwrap();
    assert!((Macro::Sustain.get(&patch) - after).abs() < 1e-6);
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
    let from = head_of(&h, &names::card("TAL-Chorus-LX"));
    let airw = h
        .get_by_role_and_label(Role::Group, &names::card("Airwindows Consolidated"))
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
    let from = head_of(&h, &names::card("TAL-Chorus-LX"));
    let surge = h
        .get_by_role_and_label(Role::Group, &names::card("Surge XT"))
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
    let surge_head = head_of(&h, &names::card("Surge XT"));
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
    let from = head_of(&h, &names::card("Dragonfly Hall Reverb"));
    let tal = h
        .get_by_role_and_label(Role::Group, &names::card("TAL-Chorus-LX"))
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
        .get_by_role_and_label(Role::Group, &names::card("LSP Para EQ x8"))
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
        .get_by_role_and_label(Role::Group, &names::card("LSP Para EQ x8"))
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
    let surge_label = names::card("Surge XT");
    let opened = card(&h, &surge_label)
        .get_by_role_and_label(Role::Button, names::OPENED)
        .rect();
    click_at(&mut h, opened.center());
    assert!(h.state().ui().plugin_windows.is_empty());
    card(&h, &surge_label).get_by_role_and_label(Role::Button, names::OPEN);
    // The EQ has no own GUI: "Parameter" opens the generic view.
    let eq_label = names::card("LSP Para EQ x8");
    let params = card(&h, &eq_label)
        .get_by_role_and_label(Role::Button, names::PARAMS)
        .rect();
    click_at(&mut h, params.center());
    let w = &h.state().ui().plugin_windows;
    assert_eq!(
        (w.len(), w[0].device, w[0].view),
        (1, c.eq, PluginView::Params)
    );
    card(&h, &eq_label).get_by_role_and_label(Role::Button, names::OPENED);
}

#[test]
fn generic_knobs_set_the_plugin_parameter() {
    let (app, c, _, set) = s05();
    let mut h = harness(app);
    let eq_label = names::card("LSP Para EQ x8");
    let knob = card(&h, &eq_label)
        .get_by_role_and_label(Role::Slider, "Low Cut")
        .rect();
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 50.0));
    let set = set.lock().unwrap();
    let last = set.last().expect("set_param");
    assert_eq!((last.0, last.1), (c.eq, 0));
    assert!(last.2 > 0.18 + 0.1, "{}", last.2);
    // Bypassed TAL: the knobs are still pictures.
    let tal_label = names::card("TAL-Chorus-LX");
    let rate = card(&h, &tal_label).get_by_role_and_label(Role::Slider, "Rate");
    assert!(rate.accesskit_node().is_disabled());
}

// ------------------------------------------------------------------------------------------------
// S05 "Lädt / abgestürzt", S06 "Fehlt", the state warning.

#[test]
fn loading_shows_progress_then_the_knobs() {
    let (app, c, statuses, _) = s05();
    let mut h = harness(app);
    let label = names::card("Dragonfly Hall Reverb");
    let hall = card(&h, &label);
    hall.get_by_role_and_label(Role::Label, names::LOADING);
    hall.get_by_role_and_label(Role::ProgressIndicator, names::LOADING);
    assert!(
        hall.query_by_role(Role::Button).is_none(),
        "no open button while loading"
    );
    statuses.lock().unwrap().insert(c.hall, DeviceStatus::Ok);
    h.run_steps(2);
    let hall = card(&h, &label);
    assert!(hall.query_by_label(names::LOADING).is_none());
    hall.get_by_role_and_label(Role::Button, names::PARAMS);
}

#[test]
fn crashed_shows_the_red_edge_and_reloads() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    let label = names::card("Airwindows Consolidated");
    let airw = card(&h, &label);
    airw.get_by_role_and_label(Role::Label, names::CRASHED);
    airw.get_by_role_and_label(Role::Label, names::CRASHED_TEXT);
    let reload = airw
        .get_by_role_and_label(Role::Button, names::RELOAD)
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
    let label = names::card("Airwindows Consolidated");
    statuses.lock().unwrap().insert(
        c.airw,
        DeviceStatus::Crashed(FailReason::LoadFailed("no such file".into())),
    );
    h.run_steps(2);
    let airw = card(&h, &label);
    assert!(airw.query_by_label(names::CRASHED).is_none());
    airw.get_by_role_and_label(Role::Label, names::LOAD_FAILED);
    assert!(airw.query_by_label_contains("no such file").is_some());
    statuses
        .lock()
        .unwrap()
        .insert(c.airw, DeviceStatus::Crashed(FailReason::Unresponsive));
    h.run_steps(2);
    let airw = card(&h, &label);
    airw.get_by_role_and_label(Role::Label, names::UNRESPONSIVE);
    airw.get_by_role_and_label(Role::Button, names::RELOAD);
}

#[test]
fn a_missing_plugin_is_a_red_placeholder_that_keeps_its_state() {
    // The built-in host knows only pisynth: Lead's effects are missing (S06).
    let mut app = App::headless(demo_project());
    let lead = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![lead],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    let label = names::card("LSP Para EQ x8");
    let eq = card(&h, &label);
    eq.get_by_role_and_label(Role::Label, names::MISSING);
    eq.get_by_role_and_label(Role::Label, names::MISSING_TEXT);
    assert!(eq.query_by_role(Role::Slider).is_none());
    assert!(
        eq.query_by_role_and_label(Role::Button, names::OPEN)
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
    let label = names::card("LSP Para EQ x8");
    let eq = card(&h, &label);
    eq.get_by_role_and_label(Role::Label, names::STATE_WARNING);
    // The knobs stay.
    eq.get_by_role_and_label(Role::Slider, "Low Cut");
    h.run_steps(1);
    let mut results = SnapshotResults::new();
    mask_cpu(&mut h);
    h.snapshot("devices-state-warning-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

// ------------------------------------------------------------------------------------------------
// Header, context menu, selection, scrolling.

#[test]
fn header_shows_the_track_and_opens_the_browser_on_effects() {
    let (mut app, ..) = s05();
    app.dispatch(Action::Ui(UiAction::TogglePanel(
        komponist_app::ui_state::Panel::Browser,
    )));
    app.dispatch(Action::Ui(UiAction::SetBrowserSegment(BrowserSegment::All)));
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Label, &names::info(5));
    let add = h.get_by_role_and_label(Role::Button, names::ADD).rect();
    click_at(&mut h, add.center());
    let ui = h.state().ui();
    assert!(ui.layout.browser_open);
    assert_eq!(ui.browser.segment, BrowserSegment::Effects);
    assert_eq!(ui.focus, komponist_app::FocusArea::Browser);
}

#[test]
fn click_selects_and_the_context_menu_removes() {
    let (app, c, ..) = s05();
    let mut h = harness(app);
    let head = head_of(&h, &names::card("LSP Para EQ x8"));
    let at = pos2(head.left() + 90.0, head.center().y);
    click_at(&mut h, at);
    assert!(h.state().ui().selection.devices.contains(&c.eq));
    h.hover_at(at);
    h.step();
    press(&mut h, at, PointerButton::Secondary, true);
    press(&mut h, at, PointerButton::Secondary, false);
    h.run_steps(2);
    let remove = h.get_by_label(names::REMOVE).rect();
    click_at(&mut h, remove.center());
    assert_eq!(chain_of(h.state(), c.pad), [c.surge, c.tal, c.hall, c.airw]);
}

#[test]
fn the_wheel_scrolls_a_long_chain() {
    // Wide side panels leave the chain less room than its 1364 px.
    let (mut app, ..) = s05();
    app.ui_mut().layout.browser_width = komponist_tokens::size::browser::MAX;
    app.ui_mut().layout.inspector_width = komponist_tokens::size::inspector::MAX;
    let mut h = harness(app);
    let first = h
        .get_by_role_and_label(Role::Group, &names::card("Surge XT"))
        .rect();
    h.hover_at(first.center());
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    h.run_steps(3);
    let scroll = h.state().ui().devices.scroll_x;
    assert!(scroll > 0.0, "{scroll}");
    let moved = h
        .get_by_role_and_label(Role::Group, &names::card("Surge XT"))
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
    let label = names::card("kp-test-fx");
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
    let label = names::card("Surge XT");
    assert!(card(&h, &label).query_by_label("zu spät").is_none());
    late.lock().unwrap().insert(c.surge, 2);
    h.run_steps(2);
    card(&h, &label).get_by_role_and_label(Role::Label, "zu spät");
    h.run_steps(150);
    assert!(card(&h, &label).query_by_label("zu spät").is_none());
}

#[test]
fn a_card_that_appears_is_scrolled_into_view() {
    use komponist_app::views::devices::scroll_to_card;
    let (p, c) = s05_project();
    let track = p.track(c.pad).unwrap();
    let cards = komponist_app::views::devices::model::cards(track, &FakeHost::new(), &[]);
    let last = cards.last().unwrap().device;
    // A narrow body: the last card lies right of it; the first is already visible.
    assert!(scroll_to_card(&cards, last, 600.0, 0.0) > 0.0);
    assert_eq!(scroll_to_card(&cards, cards[0].device, 600.0, 0.0), 0.0);
    // Scrolled far right, the first card comes back.
    assert!(scroll_to_card(&cards, cards[0].device, 600.0, 900.0) < 900.0);
}
