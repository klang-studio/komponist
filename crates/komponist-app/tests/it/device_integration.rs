//! WP-89: the device integration. Touching a parameter (inspector section, status line), the
//! right-click menu, the maximised dock, Alt-drag of a device onto another track, Strg C / V as
//! egui-winit delivers them (`Event::Copy` / `Event::Paste`), the level feed of the Limiter's
//! meter, and the FM display with free routing.

use std::path::PathBuf;
use std::time::Instant;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::panels::statusbar;
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, EditAction, PluginAction, UiAction};
use komponist_i18n::{Lang, de};
use komponist_model::edits::NewTrack;
use komponist_model::fixtures::demo_project;
use komponist_model::params::ParamId;
use komponist_model::{ChannelRef, DeviceId, Project, TrackId, edits};
use komponist_tokens::device;

use crate::common::{
    author_base, author_parameter, set_authored_param, shipped_plugin, step_until, wait_for_layout,
};
use crate::fake_host::FakeHost;

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

fn button(h: &mut Harness<'_, App>, pos: Pos2, b: PointerButton, pressed: bool, m: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: b,
        pressed,
        modifiers: m,
    });
}

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    button(h, pos, PointerButton::Primary, true, Modifiers::NONE);
    button(h, pos, PointerButton::Primary, false, Modifiers::NONE);
    h.run_steps(3);
}

fn right_click(h: &mut Harness<'_, App>, pos: Pos2) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    button(h, pos, PointerButton::Secondary, true, Modifiers::NONE);
    button(h, pos, PointerButton::Secondary, false, Modifiers::NONE);
    h.run_steps(3);
}

/// Presses at `from`, moves in 8 steps and returns without releasing.
fn press_and_move(h: &mut Harness<'_, App>, from: Pos2, to: Pos2, m: Modifiers) {
    h.run_steps(30);
    h.hover_at(from);
    h.step();
    if m != Modifiers::NONE {
        h.event(Event::ModifiersChanged(m));
    }
    button(h, from, PointerButton::Primary, true, m);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
}

fn release(h: &mut Harness<'_, App>, at: Pos2, m: Modifiers) {
    button(h, at, PointerButton::Primary, false, m);
    h.run_steps(3);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

fn chain(app: &App, track: TrackId) -> Vec<(DeviceId, String)> {
    app.project()
        .track(track)
        .unwrap()
        .channel
        .devices
        .iter()
        .map(|d| (d.id, d.plugin.name.clone()))
        .collect()
}

fn select_track(app: &mut App, track: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
}

fn add_hall_track(
    mut app: App,
    hexa: komponist_model::PluginRef,
    hall: komponist_model::PluginRef,
) -> (App, TrackId, DeviceId) {
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hall,
        target: DropTarget::Track(track),
    }));
    let device = chain(&app, track)[1].0;
    (app, track, device)
}

/// A shipped Hexa and Hall on one track for the device interaction tests.
fn app_with_hall() -> (App, TrackId, DeviceId) {
    let app = App::headless(Project::new("Intern"));
    let hexa = shipped(&app, "hexa");
    let hall = shipped(&app, "hall");
    let (mut app, track, hall) = add_hall_track(app, hexa, hall);
    wait_for_layout(&mut app, hall);
    (app, track, hall)
}

fn shipped(app: &App, slug: &str) -> komponist_model::PluginRef {
    let id = format!("org.klang-studio.komponist.{slug}");
    shipped_plugin(app.engine().host(), &id)
        .unwrap_or_else(|| panic!("build {id} with scripts/build-test-plugins.sh"))
}

fn decay_value(app: &App, device: DeviceId) -> f64 {
    author_base(app, device, "decay")
}

fn decay_parameter(app: &App, device: DeviceId) -> ParamId {
    ParamId::plugin(author_parameter(app, device, "decay"))
}

fn decay_default(app: &App, device: DeviceId) -> f64 {
    let id = author_parameter(app, device, "decay");
    app.engine()
        .host()
        .params(device)
        .unwrap()
        .iter()
        .find(|param| param.id == id)
        .unwrap()
        .default
}

fn knob(h: &Harness<'_, App>, name: &str) -> Rect {
    h.get_by_role_and_label(Role::Group, &Lang::De.devices_card("Hall"))
        .get_by_role_and_label(Role::Slider, name)
        .rect()
}

// ------------------------------------------------------------------------------------------------
// Parameter touch: inspector section and status line

#[test]
fn touching_a_parameter_shows_it_in_the_inspector_and_while_dragging_in_the_status_line() {
    let (app, _, hall) = app_with_hall();
    let mut h = harness(app);
    h.run_steps(30);
    // Nothing touched: no "Parameter" section.
    assert!(
        h.query_by_role_and_label(Role::Group, "Parameter")
            .is_none()
    );
    let decay = knob(&h, "Decay");
    press_and_move(
        &mut h,
        decay.center(),
        decay.center() - vec2(0.0, 30.0),
        Modifiers::NONE,
    );
    // Held and moving: the status line has the name and the value, the hint the shortcuts.
    let touch = h.state().ui().devices.touched.clone().expect("touched");
    assert_eq!(
        (touch.device, touch.param),
        (hall, decay_parameter(h.state(), hall))
    );
    assert!(touch.dragging);
    let status = statusbar::model(h.state(), Instant::now());
    assert!(status.left.starts_with("Decay · "), "{}", status.left);
    assert_eq!(
        status.mid,
        komponist_app::platform::Platform::current().pick(
            komponist_i18n::de::STATUS_PARAM_HINT,
            komponist_i18n::de::STATUS_PARAM_HINT_MAC
        )
    );
    release(&mut h, decay.center() - vec2(0.0, 30.0), Modifiers::NONE);
    assert!(!h.state().ui().devices.touched.as_ref().unwrap().dragging);
    let status = statusbar::model(h.state(), Instant::now());
    assert!(!status.left.starts_with("Decay · "), "{}", status.left);
    // The inspector shows name, place, value, default and range.
    let section = h.get_by_role_and_label(Role::Group, "Parameter");
    section.get_by_label("Decay");
    let track = h.state().project().tracks[0].name.clone();
    section.get_by_label(&format!("Hall · {track}"));
    let readout = h
        .get_by_role_and_label(Role::Slider, "Decay")
        .value()
        .unwrap();
    section.get_by_label(&format!("Wert {readout}"));
    assert!(section.query_by_label_contains("Standard ").is_some());
    let range = section
        .query_all_by_label_contains("Bereich ")
        .next()
        .expect("range field")
        .accesskit_node();
    let range = range.label().or_else(|| range.value()).unwrap_or_default();
    assert!(range.contains(" bis "), "{range}");
    section.get_by_label("Automation zeigen");
    assert!(h.query_by_label_contains("MIDI lernen").is_none());
}

#[test]
fn a_stopped_read_override_is_shown_on_the_device_card() {
    use komponist_app::views::automation::{Action as Automation, Source};
    use komponist_engine::Command;
    use komponist_model::Tick;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};

    let (mut app, track, device) = app_with_hall();
    let owner = AutomationOwner::Track(track);
    let target = AutomationTarget::Param {
        device,
        param: decay_parameter(&app, device),
    };
    app.dispatch(Action::Automation(Automation::ShowTarget {
        owner,
        replace: None,
        target: target.clone(),
        label: "Hall · Decay".into(),
    }));
    let lane = app.project().track(track).unwrap().automation.lanes[0].id;
    app.dispatch(Action::Automation(Automation::Points(
        Source::Lane(owner, lane),
        Points::new(vec![Point::linear(Tick::ZERO, 0.25)]),
        "undo-automation-add-point",
    )));
    app.engine_mut().hold_engine().unwrap();
    let mut h = harness(app);
    let readout = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Group, &Lang::De.devices_card("Hall"))
            .get_by_role_and_label(Role::Slider, "Decay")
            .accesskit_node()
            .numeric_value()
            .unwrap()
    };
    let decay = author_parameter(h.state(), device, "decay");
    assert!((readout(&h) - 0.25).abs() < 1e-4);

    let program = h.state().engine().program().unwrap();
    let resolved = program.automation_target(owner, &target).unwrap();
    let slot = program.slots.slot(device).unwrap();
    let manual = 0.75;
    let engine = h.state_mut().engine_mut();
    engine.send(Command::Touch {
        owner,
        target: resolved,
        begin: true,
    });
    engine.send(Command::SetParam {
        slot,
        id: decay,
        value: manual,
    });
    engine.send(Command::Touch {
        owner,
        target: resolved,
        begin: false,
    });
    assert!(engine.run_held(256, &[], 0));
    h.run_steps(2);
    assert!((readout(&h) - 0.75).abs() < 1e-4);
}

/// Owner decision 2026-11-15: double-click resets a knob; the wheel over it turns it (Shift
/// fine); elsewhere the wheel keeps scrolling the chain.
#[test]
fn double_click_resets_a_knob_and_the_wheel_turns_it() {
    let (app, _, hall) = app_with_hall();
    let mut h = harness(app);
    h.run_steps(30);
    let decay = knob(&h, "Decay");
    press_and_move(
        &mut h,
        decay.center(),
        decay.center() - vec2(0.0, 40.0),
        Modifiers::NONE,
    );
    release(&mut h, decay.center() - vec2(0.0, 40.0), Modifiers::NONE);
    let default = decay_default(h.state(), hall);
    let turned = decay_value(h.state(), hall);
    assert_ne!(turned, default);

    // The wheel over the knob: one notch up = 1 % of the range up, Shift a tenth of it.
    let wheel = |h: &mut Harness<'_, App>, at: Pos2, delta: f32, m: Modifiers| {
        h.run_steps(30);
        h.hover_at(at);
        h.step();
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, delta),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: m,
        });
        h.run_steps(5);
    };
    wheel(&mut h, decay.center(), 1.0, Modifiers::NONE);
    let up = decay_value(h.state(), hall);
    assert!(up > turned, "{turned} -> {up}");
    wheel(&mut h, decay.center(), 1.0, Modifiers::SHIFT);
    let fine = decay_value(h.state(), hall);
    assert!(fine > up && (fine - up) < (up - turned), "{up} -> {fine}");
    // The chain did not scroll sideways under Shift + wheel.
    assert_eq!(h.state().ui().devices.scroll_x, 0.0);
    // Not hovered: nothing.
    wheel(&mut h, pos2(900.0, 500.0), 3.0, Modifiers::NONE);
    assert_eq!(decay_value(h.state(), hall), fine);

    // Double-click: back to the default, no text entry.
    let at = decay.center();
    h.run_steps(30);
    h.hover_at(at);
    h.step();
    for _ in 0..2 {
        button(&mut h, at, PointerButton::Primary, true, Modifiers::NONE);
        button(&mut h, at, PointerButton::Primary, false, Modifiers::NONE);
        h.step();
    }
    h.run_steps(3);
    let reset = decay_value(h.state(), hall);
    assert!((reset - default).abs() < 1e-6, "{reset} vs {default}");
    assert!(h.query_by_label("Decay Eingabe").is_none());
}

#[test]
fn touching_another_parameter_replaces_the_selection() {
    let (app, _, _) = app_with_hall();
    let mut h = harness(app);
    h.run_steps(30);
    for name in ["Decay", "Mix"] {
        let k = knob(&h, name);
        click(&mut h, k.center());
        let section = h.get_by_role_and_label(Role::Group, "Parameter");
        section.get_by_label(name);
    }
}

// ------------------------------------------------------------------------------------------------
// The right-click menu

#[test]
fn the_context_menu_of_a_parameter_offers_automation_alongside_value_edits() {
    let (app, _, hall) = app_with_hall();
    let mut h = harness(app);
    h.run_steps(30);
    let decay = knob(&h, "Decay");
    // Turn it away from the default first.
    press_and_move(
        &mut h,
        decay.center(),
        decay.center() - vec2(0.0, 40.0),
        Modifiers::NONE,
    );
    release(&mut h, decay.center() - vec2(0.0, 40.0), Modifiers::NONE);
    let default = decay_default(h.state(), hall);
    assert_ne!(decay_value(h.state(), hall), default);

    right_click(&mut h, decay.center());
    let menu = h.get_by_role_and_label(Role::Menu, de::DEVICES_PARAMETER);
    let labels: Vec<String> = menu
        .query_all_by_role(Role::MenuItem)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(
        labels.len(),
        4,
        "automation, mapping and value edits: {labels:?}"
    );
    assert!(labels.iter().any(|l| l.starts_with(de::MACROS_MAP_TO)));
    assert!(labels.iter().any(|l| l.starts_with(de::DEVICES_TO_DEFAULT)));
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with(de::AUTOMATION_SHOW_MENU))
    );
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with(de::DEVICES_ENTER_VALUE))
    );

    // "Auf Standard" is the Alt-click: the default goes out as SetParam.
    let item = h.get_by_label(de::DEVICES_TO_DEFAULT).rect();
    click(&mut h, item.center());
    assert!((decay_value(h.state(), hall) - default).abs() < 1e-6);

    // "Wert eingeben" opens the text entry over the knob.
    right_click(&mut h, decay.center());
    let item = h.get_by_label(de::DEVICES_ENTER_VALUE).rect();
    click(&mut h, item.center());
    h.run_steps(2);
    assert!(
        h.query_by_label("Decay Eingabe").is_some(),
        "the value entry is open"
    );
}

// ------------------------------------------------------------------------------------------------
// Dock maximised

#[test]
fn a_maximised_dock_keeps_the_frames_244_high_and_leaves_the_space_below_empty() {
    let (app, _, _) = app_with_hall();
    let mut h = harness(app);
    h.run_steps(5);
    let frame = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Group, &Lang::De.devices_card("Hall"))
            .rect()
    };
    let normal = frame(&h);
    let chain_normal = h
        .get_by_role_and_label(Role::Group, de::DEVICES_CHAIN)
        .rect();
    h.state_mut()
        .dispatch(Action::Ui(UiAction::ToggleDockMaximized));
    h.run_steps(5);
    assert!(h.state().ui().layout.dock_maximized);
    let maximised = frame(&h);
    assert_eq!(maximised.height(), device::frame::TOTAL_H);
    assert_eq!(normal.height(), maximised.height());
    let chain_max = h
        .get_by_role_and_label(Role::Group, de::DEVICES_CHAIN)
        .rect();
    assert!(chain_max.height() > chain_normal.height() + 100.0);
    // The frames sit at the top of the chain area (padding 10); below them nothing is drawn or
    // reachable: the space is reserved for the automation of a later wave.
    assert_eq!(maximised.top() - chain_max.top(), device::chain::PADDING);
    assert!(h.state().ui().devices.scroll_y == 0.0);
}

// ------------------------------------------------------------------------------------------------
// Alt-drag onto another track, Strg C / V

fn lead() -> (App, TrackId, Vec<DeviceId>) {
    let mut app = App::headless(demo_project()).with_device_host(Box::new(FakeHost::new()));
    let track = app.project().tracks[2].id;
    select_track(&mut app, track);
    let ids = chain(&app, track).into_iter().map(|c| c.0).collect();
    (app, track, ids)
}

fn track_named(app: &App, name: &str) -> TrackId {
    app.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn frame_of(h: &Harness<'_, App>, name: &str) -> Rect {
    h.get_by_role_and_label(Role::Group, &Lang::De.devices_card(name))
        .rect()
}

fn head_point(h: &Harness<'_, App>, name: &str) -> Pos2 {
    let f = frame_of(h, name);
    pos2(f.left() + 45.0, f.top() + device::frame::HEAD_H / 2.0)
}

/// The lane of the track `name` in the arrangement.
fn lane_point(h: &Harness<'_, App>, name: &str) -> Pos2 {
    let label = format!("Spur {name}, ");
    let row = h.get_by_label_contains(&label);
    let r = row.rect();
    pos2(r.right() - 60.0, r.center().y)
}

#[test]
fn alt_drag_of_an_effect_onto_another_track_copies_it_there_behind_the_instrument() {
    let (app, lead, ids) = lead();
    let bass = track_named(&app, "Bass");
    let before = chain(&app, bass);
    let mut h = harness(app);
    let from = head_point(&h, "LSP Para EQ x8");
    let to = lane_point(&h, "Bass");
    press_and_move(&mut h, from, to, Modifiers::ALT);
    release(&mut h, to, Modifiers::ALT);
    let after = chain(h.state(), bass);
    assert_eq!(after.len(), before.len() + 1, "{after:?}");
    assert_eq!(after.last().unwrap().1, "LSP Para EQ x8");
    assert_eq!(after[0].1, before[0].1, "the instrument stays first");
    assert_eq!(
        chain(h.state(), lead).len(),
        ids.len(),
        "the original stays"
    );
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Edit(EditAction::CopyDevice { device, to: ChannelRef::Track(t), .. })
            if *device == ids[1] && *t == bass
    )));
}

#[test]
fn a_plain_drag_onto_another_track_does_nothing() {
    let (app, _, _) = lead();
    let bass = track_named(&app, "Bass");
    let before = chain(&app, bass);
    let mut h = harness(app);
    let from = head_point(&h, "LSP Para EQ x8");
    let to = lane_point(&h, "Bass");
    press_and_move(&mut h, from, to, Modifiers::NONE);
    release(&mut h, to, Modifiers::NONE);
    assert_eq!(chain(h.state(), bass), before);
}

#[test]
fn copying_an_instrument_needs_a_track_without_one_and_a_midi_effect_goes_before_it() {
    let mut app = App::headless(Project::new("Kopie"));
    let hexa = shipped(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let a = app.project().tracks[0].id;
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "arpeggiator"),
        target: DropTarget::Track(a),
    }));
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "hall"),
        target: DropTarget::Track(a),
    }));
    let names_of =
        |app: &App, t| -> Vec<String> { chain(app, t).into_iter().map(|c| c.1).collect() };
    assert_eq!(names_of(&app, a), ["Arpeggiator", "Hexa", "Hall"]);
    let ids: Vec<DeviceId> = chain(&app, a).into_iter().map(|c| c.0).collect();
    // A second track with an instrument refuses the instrument; MIDI effect and effect go to
    // their parts.
    app.dispatch(Action::Edit(EditAction::AddTrack(NewTrack {
        name: Some("Leer".into()),
        hue: None,
        instrument: None,
        index: None,
        arm_exclusive: false,
    })));
    let empty = track_named(&app, "Leer");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "hall"),
        target: DropTarget::Track(empty),
    }));
    // The instrument goes right behind the MIDI effects of the track that has none.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[1],
        to: ChannelRef::Track(empty),
        index: usize::MAX,
    }));
    assert_eq!(names_of(&app, empty), ["Hexa", "Hall"]);
    // A MIDI effect lands in the MIDI part, in front of the instrument.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[0],
        to: ChannelRef::Track(empty),
        index: usize::MAX,
    }));
    assert_eq!(names_of(&app, empty), ["Arpeggiator", "Hexa", "Hall"]);
    // The track has an instrument now: another copy is refused, nothing changes.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[1],
        to: ChannelRef::Track(empty),
        index: usize::MAX,
    }));
    assert_eq!(names_of(&app, empty), ["Arpeggiator", "Hexa", "Hall"]);
    assert!(app.last_error().is_some());
    // An effect goes behind the instrument.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[2],
        to: ChannelRef::Track(empty),
        index: 0,
    }));
    assert_eq!(
        names_of(&app, empty),
        ["Arpeggiator", "Hexa", "Hall", "Hall"]
    );
}

#[test]
fn strg_c_and_v_work_as_copy_and_paste_events_of_the_desktop() {
    let (app, track, ids) = lead();
    let mut h = harness(app);
    // Select the EQ and give the chain the focus.
    let head = head_point(&h, "LSP Para EQ x8");
    click(&mut h, head);
    assert!(h.state().ui().selection.devices.contains(&ids[1]));
    // egui-winit sends `Event::Copy` / `Event::Paste` and no key event for Strg C / V.
    h.event(Event::Copy);
    h.run_steps(2);
    assert_eq!(h.state().ui().devices.clipboard.len(), 1);
    h.event(Event::Paste("irrelevant".into()));
    h.run_steps(3);
    let after = chain(h.state(), track);
    assert_eq!(after.len(), ids.len() + 1);
    assert_eq!(after[2].1, "LSP Para EQ x8", "behind the selected device");
}

/// The text the frames since the last call put on the system clipboard.
fn copied_texts(h: &mut Harness<'_, App>, steps: usize) -> Vec<String> {
    let mut texts = Vec::new();
    for _ in 0..steps {
        h.step();
        texts.extend(
            h.output()
                .platform_output
                .commands
                .iter()
                .filter_map(|c| match c {
                    egui::OutputCommand::CopyText(t) => Some(t.clone()),
                    _ => None,
                }),
        );
    }
    texts
}

#[test]
fn copying_devices_puts_a_marker_on_the_system_clipboard_so_strg_v_arrives() {
    let (app, track, ids) = lead();
    let mut h = harness(app);
    let head = head_point(&h, "LSP Para EQ x8");
    click(&mut h, head);
    h.event(Event::Copy);
    assert_eq!(copied_texts(&mut h, 3), ["komponist-devices/1"]);
    // The desktop now has text on the clipboard and delivers it with Strg V: that marker.
    h.event(Event::Paste("komponist-devices/1".into()));
    h.run_steps(3);
    assert_eq!(chain(h.state(), track).len(), ids.len() + 1);
    // The markers of earlier versions (German and English) still paste.
    for text in [
        "komponist: 1 Gerät",
        "komponist: 2 Geräte",
        "komponist: 1 device",
    ] {
        let before = chain(h.state(), track).len();
        h.event(Event::Paste(text.into()));
        h.run_steps(3);
        assert_eq!(chain(h.state(), track).len(), before + 1, "{text}");
    }
    // Nothing copied, nothing written.
    h.state_mut().ui_mut().selection.devices.clear();
    h.event(Event::Copy);
    assert!(copied_texts(&mut h, 3).is_empty());
}

// ------------------------------------------------------------------------------------------------
// The level feed of the Limiter's meter

#[test]
fn the_limiter_meter_shows_the_reduction_the_engine_measured() {
    use komponist_engine::{MidiInEvent, MidiPort};
    let mut app = App::headless(Project::new("Pegel"));
    let hexa = shipped(&app, "hexa");
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: shipped(&app, "limiter"),
        target: DropTarget::Track(track),
    }));
    let limiter = chain(&app, track)[1].0;
    wait_for_layout(&mut app, limiter);
    // A low ceiling and +24 dB input: every note is far above it.
    for (key, value) in [("gain", 24.0), ("ceiling", -12.0)] {
        set_authored_param(&mut app, limiter, key, value);
    }
    app.dispatch(Action::Edit(EditAction::SetTrackProp {
        tracks: vec![track],
        prop: komponist_model::edits::TrackProp::Arm(true),
    }));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    h.run_steps(2);
    let readout = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Meter, komponist_i18n::de::UI_METER_GR_NAME)
            .accesskit_node()
            .value()
            .unwrap_or_default()
    };
    assert!(
        readout(&h).contains(" 0.0 dB"),
        "silent before a note: {}",
        readout(&h)
    );
    let note = MidiInEvent {
        frame: 0,
        port: MidiPort(1),
        data: [0x90, 60, 127],
    };
    for events in [vec![note], vec![]] {
        assert!(h.state_mut().engine_mut().run_held(4800, &events, 0));
        h.step();
    }
    let text = readout(&h);
    assert!(text.contains("\u{2212}"), "a reduction is shown: {text}");
}

// ------------------------------------------------------------------------------------------------
// The FM display with free routing

#[test]
fn the_fm_display_draws_the_real_routing_for_algorithm_zero() {
    let host = App::headless_plugin_host();
    let prepared = host
        .manager()
        .prepare_bundled_device(
            "org.klang-studio.komponist.hexa",
            Some("Init"),
            &[("algorithm", 1.0), ("op1_output", 0.0)],
            &[],
        )
        .unwrap();
    let (mut project, track, device) =
        crate::common::instrument_project("Frei", prepared.plugin.clone());
    std::sync::Arc::make_mut(&mut project.tracks[0])
        .channel
        .devices[0] = std::sync::Arc::new(prepared.into_device(device));
    let mut app = App::headless(project);
    select_track(&mut app, track);
    wait_for_layout(&mut app, device);
    step_until(&mut app, "the edited free routing", |app| {
        app.engine()
            .host()
            .device_display(device)
            .and_then(|display| display.fm_topologies.first())
            .is_some_and(|routing| routing.algorithm.is_none() && routing.outputs == 0b000100)
    });
    let mut h = harness(app);
    h.run_steps(30);
    let label = Lang::De.devices_card("Hexa");
    h.get_by_role_and_label(Role::Group, &label)
        .get_by_role_and_label(Role::Group, de::DEVICES_FM_FREE);
}

// ------------------------------------------------------------------------------------------------
// Snapshots

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

#[test]
fn snapshot_of_a_touched_parameter_with_its_context_menu() {
    let (app, _, _) = app_with_hall();
    let mut h = harness(app);
    h.run_steps(30);
    let decay = knob(&h, "Decay");
    click(&mut h, decay.center());
    right_click(&mut h, decay.center());
    mask_cpu(&mut h);
    let mut results = SnapshotResults::new();
    h.snapshot("devices-parameter-touched-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

/// The exact topology is part of the real bundle state. A free wire graph is not editable as a
/// CLAP parameter, so seed it in the initial state exported by the bundle.
fn snapshot_free_routing(name: &str, expected: [u8; 6]) {
    use komponist_model::Hue;

    let host = App::headless_plugin_host();
    let prepared = host
        .manager()
        .prepare_bundled_device("org.klang-studio.komponist.hexa", Some("Init"), &[], &[])
        .unwrap();
    let (mut project, track, device) =
        crate::common::instrument_project("Frei", prepared.plugin.clone());
    let mut saved = prepared.into_device(device);

    let state = saved.state.as_deref().unwrap();
    let (length, payload) = state.split_first_chunk::<8>().unwrap();
    assert_eq!(u64::from_le_bytes(*length), payload.len() as u64);
    let mut document: serde_json::Value = serde_json::from_slice(payload).unwrap();
    let field = &mut document["fields"]["hexa-patch-v1"];
    let mut patch: serde_json::Value = serde_json::from_str(field.as_str().unwrap()).unwrap();
    for (operator, edges) in patch["patch"]["operators"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .zip(expected)
    {
        let routes: [bool; 6] = std::array::from_fn(|target| edges & (1 << target) != 0);
        operator["output"] = true.into();
        operator["modulates"] = serde_json::json!(routes);
    }
    *field = serde_json::to_string(&patch).unwrap().into();
    let payload = serde_json::to_vec(&document).unwrap();
    let mut state = (payload.len() as u64).to_le_bytes().to_vec();
    state.extend_from_slice(&payload);
    saved.state = Some(state.into());

    let channel = std::sync::Arc::make_mut(&mut project.tracks[0]);
    channel.name = "FM".into();
    channel.hue = Hue::BLUE;
    channel.channel.devices[0] = std::sync::Arc::new(saved);

    let mut app = App::headless(project);
    select_track(&mut app, track);
    wait_for_layout(&mut app, device);
    step_until(&mut app, "the restored free FM graph", |app| {
        app.engine()
            .host()
            .device_display(device)
            .and_then(|display| display.fm_topologies.first())
            .is_some_and(|routing| {
                routing.algorithm.is_none()
                    && routing.outputs == 0b111111
                    && routing.modulates.as_slice() == expected.as_slice()
            })
    });

    let mut h = harness(app);
    h.run_steps(30);
    h.get_by_role_and_label(Role::Group, &Lang::De.devices_card("Hexa"))
        .get_by_role_and_label(Role::Group, de::DEVICES_FM_FREE);
    mask_cpu(&mut h);
    h.try_snapshot(name).unwrap();
}

#[test]
fn snapshot_of_the_fm_display_with_free_routing() {
    snapshot_free_routing("devices-fm-free-1x", [0, 1, 1, 16, 0, 0]);
}

#[test]
fn snapshot_of_the_fm_display_with_a_cycle() {
    snapshot_free_routing("devices-fm-cycle-1x", [32, 4, 8, 0, 0, 1]);
}

#[test]
fn track_macro_ui_maps_a_clap_control_without_moving_it() {
    use komponist_model::automation::AutomationTarget;

    let (mut app, track, hall) = app_with_hall();
    app.engine_mut().hold_engine().unwrap();
    app.step();
    let base = decay_value(&app, hall);
    let norm = base as f32;
    let key = AutomationTarget::Param {
        device: hall,
        param: decay_parameter(&app, hall),
    };
    let mut h = harness(app);
    let map_button = h
        .query_all_by_label(Lang::De.macros_map())
        .filter(|node| node.accesskit_node().role() == Role::Button)
        .max_by(|a, b| a.rect().center().y.total_cmp(&b.rect().center().y))
        .expect("macro map toggle")
        .rect()
        .center();
    click(&mut h, map_button);
    assert_eq!(h.state().ui().track_macros.mapping, Some(track));
    let from = knob(&h, "Decay").center();
    let to = from - vec2(0.0, 40.0);
    press_and_move(&mut h, from, to, Modifiers::NONE);
    release(&mut h, to, Modifiers::NONE);

    let project = h.state().project().track(track).unwrap();
    let slot = &project.macros.0[0];
    assert_eq!(slot.mappings.len(), 1);
    assert_eq!(slot.mappings[0].target, key);
    assert!((slot.mappings[0].value_at(slot.value) - norm).abs() < 1e-6);
    assert_eq!(decay_value(h.state(), hall), base);

    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert_eq!(h.state().ui().track_macros.mapping, None);
    let dial = h
        .query_all_by_label(&Lang::De.macros_a11y(1, Lang::De.macros_unused()))
        .filter(|node| node.accesskit_node().role() == Role::Slider)
        .max_by(|a, b| a.rect().center().y.total_cmp(&b.rect().center().y))
        .expect("visible macro knob")
        .rect()
        .center();
    let end = dial - vec2(0.0, 80.0);
    press_and_move(&mut h, dial, end, Modifiers::NONE);
    release(&mut h, end, Modifiers::NONE);
    let app = h.state_mut();
    app.engine_mut().run_held_capture(256, &[], 0).unwrap();
    let value = app.project().track(track).unwrap().macros.0[0].value;
    assert!(value > 0.1, "dial should change the macro, got {value}");
    assert_eq!(
        app.engine().telemetry().macro_values(track).unwrap()[0],
        value
    );
    assert_eq!(decay_value(app, hall), base);
    h.run_steps(3);
    let driver = Lang::De.macros_controlled_tip(Lang::De.macros_slot(1));
    let card_label = Lang::De.devices_card("Hall");
    let control = h
        .get_by_role_and_label(Role::Group, &card_label)
        .get_by_role_and_label(Role::Slider, "Decay")
        .accesskit_node();
    assert!(control.is_read_only());
    assert_eq!(control.description().as_deref(), Some(driver.as_str()));

    press_and_move(&mut h, from, to, Modifiers::NONE);
    release(&mut h, to, Modifiers::NONE);
    let status = statusbar::model(h.state(), Instant::now());
    assert_eq!(
        status.left,
        Lang::De.status_macros_locked("Decay", Lang::De.macros_slot(1))
    );
    assert_eq!(
        h.state().project().track(track).unwrap().macros.0[0].value,
        value
    );
}

#[test]
fn track_macro_snapshots_cover_card_mapping_list_and_missing_targets() {
    use komponist_app::views::track_macros::Action as Macro;
    use komponist_model::automation::AutomationTarget;
    use komponist_model::macros::Mapping;

    let mut results = SnapshotResults::new();
    for lang in Lang::ALL {
        let (app, track, hall) = app_with_hall();
        let mut h = harness(app.with_lang(lang));
        let snapshot = |h: &mut Harness<'_, App>, name: &str| {
            let cpu = h
                .get_by_role_and_label(Role::Status, lang.transport_cpu())
                .rect();
            h.mask(cpu.expand(2.0));
            h.snapshot(format!("track-macro-{name}-{}-1x", lang.code()));
        };
        snapshot(&mut h, "card");
        let target = |id| AutomationTarget::Param {
            device: hall,
            param: ParamId::plugin(id),
        };
        let decay_target = target(author_parameter(h.state(), hall, "decay"));
        let mix_target = target(author_parameter(h.state(), hall, "mix"));
        let assert_mapping_count = |app: &App, expected| {
            assert_eq!(
                app.project().track(track).unwrap().macros.0[0]
                    .mappings
                    .len(),
                expected
            );
        };
        h.state_mut().dispatch(Action::Macro(Macro::Rename {
            track,
            index: 0,
            name: "Open".into(),
        }));
        h.state_mut()
            .dispatch(Action::Macro(Macro::ToggleMapping { track }));
        h.state_mut().dispatch(Action::Macro(Macro::Map {
            track,
            index: 0,
            target: decay_target,
        }));
        h.run_steps(3);
        assert_mapping_count(h.state(), 1);
        snapshot(&mut h, "mapping");
        h.state_mut()
            .dispatch(Action::Macro(Macro::ToggleMapping { track }));
        h.state_mut()
            .dispatch(Action::Macro(Macro::Select { track, index: 0 }));
        h.state_mut().dispatch(Action::Macro(Macro::Map {
            track,
            index: 0,
            target: mix_target.clone(),
        }));
        h.state_mut().dispatch(Action::Macro(Macro::Bend {
            track,
            index: 0,
            target: mix_target,
            bend: 0.4,
        }));
        h.run_steps(3);
        assert_mapping_count(h.state(), 2);
        snapshot(&mut h, "list");
        let name = lang.param_decay();
        let reverb = h.state().project().find_device(hall).unwrap().2;
        let device_name = h
            .state()
            .engine()
            .host()
            .catalog()
            .resolve(&reverb.plugin)
            .unwrap()
            .display_name(lang.code());
        let decay = h
            .get_by_role_and_label(Role::Group, &lang.devices_card(device_name))
            .get_by_role_and_label(Role::Slider, name)
            .rect()
            .center();
        right_click(&mut h, decay);
        snapshot(&mut h, "driven");
        results.extend_harness(&mut h);

        let missing = Mapping {
            target: target(u32::MAX),
            label: "Hall · Former target".into(),
            min: 0.0,
            max: 1.0,
            invert: false,
            bend: 0.0,
        };
        let project = edits::macros::add_mapping(h.state().project(), track, 0, missing).unwrap();
        let mut app = App::headless(project).with_lang(lang);
        select_track(&mut app, track);
        app.dispatch(Action::Macro(Macro::Select { track, index: 0 }));
        let mut missing = harness(app);
        assert_mapping_count(missing.state(), 3);
        snapshot(&mut missing, "missing");
        results.extend_harness(&mut missing);
    }
    results.unwrap();
}

#[test]
fn track_macro_mapping_range_inversion_and_bend_are_undoable() {
    use komponist_app::views::track_macros::Action as Macro;
    use komponist_model::automation::AutomationTarget;

    let (mut app, track, hall) = app_with_hall();
    let target = AutomationTarget::Param {
        device: hall,
        param: decay_parameter(&app, hall),
    };
    app.dispatch(Action::Macro(Macro::Map {
        track,
        index: 0,
        target: target.clone(),
    }));
    app.dispatch(Action::Macro(Macro::Select { track, index: 0 }));
    let mut h = harness(app);
    let mapping = |h: &Harness<'_, App>| {
        h.state().project().track(track).unwrap().macros.0[0].mappings[0].clone()
    };
    let edit = |h: &mut Harness<'_, App>, action| {
        h.state_mut().dispatch(Action::Edit(action));
        h.run_steps(3);
    };
    let original = mapping(&h);
    let minimum = Lang::De.macros_a11y_min(&original.label);
    let from = h
        .get_by_role_and_label(Role::Slider, &minimum)
        .rect()
        .center();
    let to = from + vec2(30.0, 0.0);
    press_and_move(&mut h, from, to, Modifiers::NONE);
    release(&mut h, to, Modifiers::NONE);
    let ranged = mapping(&h);
    assert!(ranged.min > original.min, "range handle should move");
    assert_eq!(ranged.max, original.max);
    edit(&mut h, EditAction::Undo);
    assert_eq!(mapping(&h), original);
    edit(&mut h, EditAction::Redo);
    assert_eq!(mapping(&h), ranged);

    let invert = h
        .get_by_role_and_label(Role::Button, Lang::De.macros_invert())
        .rect()
        .center();
    click(&mut h, invert);
    let inverted = mapping(&h);
    assert!(inverted.invert);
    edit(&mut h, EditAction::Undo);
    assert_eq!(mapping(&h), ranged);
    edit(&mut h, EditAction::Redo);
    assert_eq!(mapping(&h), inverted);

    let bend_label = Lang::De.macros_a11y_bend(Lang::De.param_decay());
    let from = h
        .get_by_role_and_label(Role::Slider, &bend_label)
        .rect()
        .center();
    let to = from - vec2(0.0, 25.0);
    press_and_move(&mut h, from, to, Modifiers::NONE);
    release(&mut h, to, Modifiers::NONE);
    let bent = mapping(&h);
    assert!(
        (0.1..1.0).contains(&bent.bend),
        "expected a partial positive bend: {bent:?}"
    );
    edit(&mut h, EditAction::Undo);
    assert_eq!(mapping(&h), inverted);
    edit(&mut h, EditAction::Redo);
    assert_eq!(mapping(&h), bent);

    h.run_steps(30);
    h.hover_at(from);
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    button(&mut h, from, PointerButton::Primary, true, Modifiers::ALT);
    h.step();
    button(&mut h, from, PointerButton::Primary, false, Modifiers::ALT);
    h.run_steps(3);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    assert_eq!(mapping(&h), inverted);
    edit(&mut h, EditAction::Undo);
    assert_eq!(mapping(&h), bent);
}

#[test]
fn track_macro_range_handles_support_collapsed_drags_and_accesskit() {
    use komponist_app::views::track_macros::Action as Macro;
    use komponist_model::automation::AutomationTarget;

    let (mut app, track, _) = app_with_hall();
    let target = AutomationTarget::Pan;
    app.dispatch(Action::Macro(Macro::Map {
        track,
        index: 0,
        target: target.clone(),
    }));
    app.dispatch(Action::Macro(Macro::Select { track, index: 0 }));
    app.dispatch(Action::Macro(Macro::Range {
        track,
        index: 0,
        target,
        min: 0.5,
        max: 0.5,
    }));
    let mut h = harness(app);
    let bounds = |h: &Harness<'_, App>| {
        let mapping = &h.state().project().track(track).unwrap().macros.0[0].mappings[0];
        (mapping.min, mapping.max)
    };
    let label = &h.state().project().track(track).unwrap().macros.0[0].mappings[0].label;
    let min = Lang::De.macros_a11y_min(label);
    let max = Lang::De.macros_a11y_max(label);
    let anchor = h.get_by_role_and_label(Role::Slider, &min).rect().center();
    for dx in [-30.0, 30.0] {
        let to = anchor + vec2(dx, 0.0);
        press_and_move(&mut h, anchor, to, Modifiers::NONE);
        release(&mut h, to, Modifiers::NONE);
        let (low, high) = bounds(&h);
        if dx < 0.0 {
            assert!(low < 0.5);
            assert_eq!(high, 0.5);
        } else {
            assert_eq!(low, 0.5);
            assert!(high > 0.5);
        }
        h.state_mut().dispatch(Action::Edit(EditAction::Undo));
        h.run_steps(3);
        assert_eq!(bounds(&h), (0.5, 0.5));
    }

    h.get_by_role_and_label(Role::Slider, &max)
        .set_value_accesskit(0.8);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, &min)
        .set_value_accesskit(0.2);
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, &min)
        .increment_accesskit();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, &max)
        .decrement_accesskit();
    h.run_steps(3);
    let (low, high) = bounds(&h);
    assert!((low - 0.21).abs() < 1e-6 && (high - 0.79).abs() < 1e-6);

    let min_handle = h.get_by_role_and_label(Role::Slider, &min).rect().center();
    let max_handle = h.get_by_role_and_label(Role::Slider, &max).rect().center();
    let start = min_handle.lerp(max_handle, 0.5);
    let to = start + vec2(18.0, 0.0);
    press_and_move(&mut h, start, to, Modifiers::ALT);
    assert!(bounds(&h).0 > low);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert_eq!(bounds(&h), (low, high));
    h.hover_at(to + vec2(20.0, 0.0));
    h.run_steps(3);
    release(&mut h, to, Modifiers::NONE);
    assert_eq!(bounds(&h), (low, high));

    let min_handle = h.get_by_role_and_label(Role::Slider, &min).rect().center();
    click(&mut h, min_handle);
    for key in [egui::Key::Enter, egui::Key::Escape] {
        h.key_press(egui::Key::Enter);
        h.run_steps(4);
        assert!(h.ctx.text_edit_focused());
        h.key_press(key);
        h.run_steps(3);
        assert!(!h.ctx.text_edit_focused());
        assert_eq!(bounds(&h), (low, high));
    }
    for _ in 0..2 {
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(3);
    }
    let (new_low, new_high) = bounds(&h);
    assert!((new_low - low - 0.02).abs() < 1e-6);
    assert_eq!(new_high, high);
}

#[test]
fn macro_automation_chooser_disables_driven_targets_and_retains_their_lanes() {
    use komponist_app::views::automation::Action as Automation;
    use komponist_app::views::track_macros::Action as Macro;
    use komponist_model::Tick;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};

    let (mut app, track, _) = app_with_hall();
    let owner = AutomationOwner::Track(track);
    let pan = AutomationTarget::Pan;
    app.dispatch(Action::Automation(Automation::ShowTarget {
        owner,
        replace: None,
        target: pan.clone(),
        label: Lang::De.param_pan().to_owned(),
    }));
    let track_name = app.project().track(track).unwrap().name.clone();
    let lane = app
        .project()
        .track(track)
        .unwrap()
        .automation
        .lane(&pan)
        .unwrap()
        .id;
    let points = Points::new(vec![Point::linear(Tick::ZERO, 0.7)]);
    app.dispatch(Action::Automation(Automation::Points(
        komponist_app::views::automation::Source::Lane(owner, lane),
        points.clone(),
        "undo-automation-add-point",
    )));
    app.dispatch(Action::Macro(Macro::Map {
        track,
        index: 0,
        target: pan.clone(),
    }));
    app.dispatch(Action::Macro(Macro::Rename {
        track,
        index: 0,
        name: "Breite".into(),
    }));
    let mut h = harness(app);
    let heading = Lang::De.a11y_automation_lane(Lang::De.param_pan(), track_name);
    let group = h
        .get_all_by_role_and_label(Role::Group, &heading)
        .find(|node| node.accesskit_node().description().is_some())
        .unwrap();
    let reason = Lang::De.macros_lane_overridden(Lang::De.macros_ref(1, "Breite"));
    assert_eq!(
        group.accesskit_node().description().as_deref(),
        Some(reason.as_str())
    );
    let chooser = group
        .get_by_role_and_label(Role::ComboBox, Lang::De.a11y_automation_chooser())
        .rect()
        .center();
    click(&mut h, chooser);

    let driven = format!("{} · {reason}", Lang::De.param_pan());
    let row = h.get_by_role_and_label(Role::ListBoxOption, &driven);
    assert!(row.accesskit_node().is_disabled());
    let macro_label = Lang::De.macros_slot_named(1, "Breite");
    let rect = h
        .get_by_role_and_label(Role::ListBoxOption, &macro_label)
        .rect();
    click(&mut h, rect.center());
    let track_state = h.state().project().track(track).unwrap();
    let target = AutomationTarget::Macro { index: 0 };
    assert_eq!(
        track_state.automation.lane(&target).unwrap().label,
        macro_label
    );
    assert_eq!(track_state.automation.lane(&pan).unwrap().points, points);
    assert!(
        h.state()
            .engine()
            .program()
            .unwrap()
            .automation_target(owner, &pan)
            .is_none()
    );

    h.state_mut().dispatch(Action::Macro(Macro::Remove {
        track,
        index: 0,
        target: pan.clone(),
    }));
    h.run_steps(3);
    assert!(
        h.state()
            .engine()
            .program()
            .unwrap()
            .automation_target(owner, &pan)
            .is_some()
    );
    assert_eq!(
        h.state()
            .project()
            .track(track)
            .unwrap()
            .automation
            .lane(&pan)
            .unwrap()
            .points,
        points
    );
}
