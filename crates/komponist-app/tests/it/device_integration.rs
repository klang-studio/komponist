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
use komponist_builtin::Internal;
use komponist_i18n::{Lang, de};
use komponist_model::edits::{NewDevice, NewTrack};
use komponist_model::fixtures::demo_project;
use komponist_model::params::ParamId;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, TrackId, edits};
use komponist_tokens::device;

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

/// One hexa track with a Hall behind it, the dock on „Geräte“.
fn app_with_hall() -> (App, TrackId, DeviceId) {
    let mut app = App::headless(Project::new("Intern"));
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Hall.plugin_ref(),
        target: DropTarget::Track(track),
    }));
    let hall = chain(&app, track)[1].0;
    (app, track, hall)
}

fn hall_values(app: &App, device: DeviceId) -> Vec<f64> {
    let (_, _, d) = app.project().find_device(device).unwrap();
    Internal::Hall.decode_state(d.state.as_deref()).unwrap()
}

fn decay_wire() -> usize {
    Internal::Hall.wire(&ParamId::new("decay")).unwrap() as usize
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
    assert_eq!((touch.device, touch.param.as_str()), (hall, "decay"));
    assert!(touch.dragging);
    let status = statusbar::model(h.state(), Instant::now());
    assert!(status.left.starts_with("Decay · "), "{}", status.left);
    assert_eq!(status.mid, komponist_i18n::de::STATUS_PARAM_HINT);
    release(&mut h, decay.center() - vec2(0.0, 30.0), Modifiers::NONE);
    assert!(!h.state().ui().devices.touched.as_ref().unwrap().dragging);
    let status = statusbar::model(h.state(), Instant::now());
    assert!(!status.left.starts_with("Decay · "), "{}", status.left);
    // The inspector shows name, place, value, default and range.
    let section = h.get_by_role_and_label(Role::Group, "Parameter");
    section.get_by_label("Decay");
    let track = h.state().project().tracks[0].name.clone();
    section.get_by_label(&format!("Hall · {track}"));
    let value = hall_values(h.state(), hall)[decay_wire()];
    let text = Internal::Hall.params()[decay_wire()].format(value);
    section.get_by_label(&format!("Wert {text}"));
    let default =
        Internal::Hall.params()[decay_wire()].format(Internal::Hall.defaults()[decay_wire()]);
    section.get_by_label(&format!("Standard {default}"));
    let range = section
        .query_all_by_label_contains("Bereich ")
        .next()
        .expect("range field")
        .accesskit_node();
    let range = range.label().or_else(|| range.value()).unwrap_or_default();
    assert!(range.contains(" bis "), "{range}");
    // No buttons for what is only prepared.
    for hidden in ["Automation zeigen", "MIDI lernen", "Makro"] {
        assert!(h.query_by_label_contains(hidden).is_none(), "{hidden}");
    }
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
    let default = Internal::Hall.defaults()[decay_wire()];
    let turned = hall_values(h.state(), hall)[decay_wire()];
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
    let up = hall_values(h.state(), hall)[decay_wire()];
    assert!(up > turned, "{turned} -> {up}");
    wheel(&mut h, decay.center(), 1.0, Modifiers::SHIFT);
    let fine = hall_values(h.state(), hall)[decay_wire()];
    assert!(fine > up && (fine - up) < (up - turned), "{up} -> {fine}");
    // The chain did not scroll sideways under Shift + wheel.
    assert_eq!(h.state().ui().devices.scroll_x, 0.0);
    // Not hovered: nothing.
    wheel(&mut h, pos2(900.0, 500.0), 3.0, Modifiers::NONE);
    assert_eq!(hall_values(h.state(), hall)[decay_wire()], fine);

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
    let reset = hall_values(h.state(), hall)[decay_wire()];
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
fn the_context_menu_of_a_parameter_has_standard_and_wert_eingeben_only() {
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
    let default = Internal::Hall.defaults()[decay_wire()];
    assert_ne!(hall_values(h.state(), hall)[decay_wire()], default);

    right_click(&mut h, decay.center());
    let menu = h.get_by_role_and_label(Role::Menu, de::DEVICES_PARAMETER);
    let labels: Vec<String> = menu
        .query_all_by_role(Role::MenuItem)
        .map(|n| n.accesskit_node().label().unwrap_or_default())
        .collect();
    assert_eq!(
        labels.len(),
        2,
        "nothing else for now (automation, MIDI learn and macros are only prepared): {labels:?}"
    );
    assert!(labels.iter().any(|l| l.starts_with(de::DEVICES_TO_DEFAULT)));
    assert!(
        labels
            .iter()
            .any(|l| l.starts_with(de::DEVICES_ENTER_VALUE))
    );

    // "Auf Standard" is the Alt-click: the default goes out as SetParam.
    let item = h.get_by_label(de::DEVICES_TO_DEFAULT).rect();
    click(&mut h, item.center());
    assert_eq!(hall_values(h.state(), hall)[decay_wire()], default);

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
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let a = app.project().tracks[0].id;
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Arpeggiator.plugin_ref(),
        target: DropTarget::Track(a),
    }));
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Hall.plugin_ref(),
        target: DropTarget::Track(a),
    }));
    let names_of =
        |app: &App, t| -> Vec<String> { chain(app, t).into_iter().map(|c| c.1).collect() };
    assert_eq!(names_of(&app, a), ["Arpeggiator", "hexa", "Hall"]);
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
        plugin: Internal::Hall.plugin_ref(),
        target: DropTarget::Track(empty),
    }));
    // The instrument goes right behind the MIDI effects of the track that has none.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[1],
        to: ChannelRef::Track(empty),
        index: usize::MAX,
    }));
    assert_eq!(names_of(&app, empty), ["hexa", "Hall"]);
    // A MIDI effect lands in the MIDI part, in front of the instrument.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[0],
        to: ChannelRef::Track(empty),
        index: usize::MAX,
    }));
    assert_eq!(names_of(&app, empty), ["Arpeggiator", "hexa", "Hall"]);
    // The track has an instrument now: another copy is refused, nothing changes.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[1],
        to: ChannelRef::Track(empty),
        index: usize::MAX,
    }));
    assert_eq!(names_of(&app, empty), ["Arpeggiator", "hexa", "Hall"]);
    assert!(app.last_error().is_some());
    // An effect goes behind the instrument.
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: ids[2],
        to: ChannelRef::Track(empty),
        index: 0,
    }));
    assert_eq!(
        names_of(&app, empty),
        ["Arpeggiator", "hexa", "Hall", "Hall"]
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
    assert_eq!(copied_texts(&mut h, 3), ["komponist: 1 Gerät"]);
    // The desktop now has text on the clipboard and delivers it with Strg V: that marker.
    h.event(Event::Paste("komponist: 1 Gerät".into()));
    h.run_steps(3);
    assert_eq!(chain(h.state(), track).len(), ids.len() + 1);
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
    let hexa = app.engine().host().catalog().entries()[0].plugin.clone();
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: hexa,
        target: DropTarget::NewTrack { index: None },
    }));
    let track = app.project().tracks[0].id;
    select_track(&mut app, track);
    app.dispatch(Action::Plugin(PluginAction::Add {
        plugin: Internal::Limiter.plugin_ref(),
        target: DropTarget::Track(track),
    }));
    let limiter = chain(&app, track)[1].0;
    // A low ceiling and +24 dB input: every note is far above it.
    for (key, value) in [("gain", 24.0), ("ceiling", -12.0)] {
        let id = Internal::Limiter.wire(&ParamId::new(key)).unwrap();
        app.dispatch(Action::Plugin(PluginAction::SetParam {
            device: limiter,
            id,
            value,
        }));
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
    use komponist_builtin::{algorithms, default_patch, encode_state};
    use komponist_model::Hue;
    // A patch whose routing no table algorithm has: operator 1 modulates operator 6, and
    // operator 6 modulates operator 1.
    let mut patch = default_patch();
    algorithms::apply(&mut patch, 31);
    patch.operators[0].modulates[5] = true;
    patch.operators[5].modulates[0] = true;
    assert_eq!(algorithms::detect(&patch), None);
    let mut project = Project::new("Frei");
    let (p, track) = edits::add_track(
        &project,
        NewTrack {
            name: Some("FM".into()),
            hue: Some(Hue::BLUE),
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    project = p;
    let hexa = App::headless(Project::new("x"))
        .engine()
        .host()
        .catalog()
        .entries()[0]
        .plugin
        .clone();
    let (project, _) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            role: DeviceRole::Instrument,
            plugin: hexa,
            state: Some(encode_state(&patch).into()),
            sidechain: None,
        },
    )
    .unwrap();
    let mut app = App::headless(project);
    select_track(&mut app, track);
    let h = harness(app);
    let label = Lang::De.devices_card("hexa");
    let card = h.get_by_role_and_label(Role::Group, &label);
    card.get_by_role_and_label(Role::Group, de::DEVICES_FM_FREE);
    // The cards model carries the real masks (operator 1 → 6, operator 6 → 1).
    let cards = komponist_app::views::devices::model::cards(
        Lang::De,
        h.state().project().track(track).unwrap(),
        h.state().engine().host(),
        &[],
    );
    let routing = cards[0].routing.expect("hexa has a routing");
    assert_eq!(routing.modulates[0] & 0b100000, 0b100000);
    assert_eq!(routing.modulates[5] & 0b000001, 0b000001);
    assert_ne!(routing.modulates, algorithms::ALGORITHMS[0].modulates);
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

#[test]
fn snapshot_of_the_fm_display_with_free_routing() {
    use komponist_builtin::{algorithms, default_patch, encode_state};
    use komponist_model::Hue;
    let mut patch = default_patch();
    // All carriers, then 2 and 3 modulate 1 and 4 modulates 5: no table algorithm has that.
    algorithms::apply(&mut patch, 31);
    patch.operators[1].modulates[0] = true;
    patch.operators[2].modulates[0] = true;
    patch.operators[3].modulates[4] = true;
    assert_eq!(algorithms::detect(&patch), None);
    let hexa = App::headless(Project::new("x"))
        .engine()
        .host()
        .catalog()
        .entries()[0]
        .plugin
        .clone();
    let (project, track) = edits::add_track(
        &Project::new("Frei"),
        NewTrack {
            name: Some("FM".into()),
            hue: Some(Hue::BLUE),
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (project, _) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            role: DeviceRole::Instrument,
            plugin: hexa,
            state: Some(encode_state(&patch).into()),
            sidechain: None,
        },
    )
    .unwrap();
    let mut app = App::headless(project);
    select_track(&mut app, track);
    let mut h = harness(app);
    h.run_steps(30);
    mask_cpu(&mut h);
    let mut results = SnapshotResults::new();
    h.snapshot("devices-fm-free-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

#[test]
fn snapshot_of_the_fm_display_with_a_cycle() {
    use komponist_builtin::{algorithms, default_patch, encode_state};
    use komponist_model::Hue;
    let mut patch = default_patch();
    // 1 and 6 modulate each other and are both heard; 2 → 3 → 4 is a chain on the side.
    algorithms::apply(&mut patch, 31);
    patch.operators[0].modulates[5] = true;
    patch.operators[5].modulates[0] = true;
    patch.operators[1].modulates[2] = true;
    patch.operators[2].modulates[3] = true;
    assert_eq!(algorithms::detect(&patch), None);
    let hexa = App::headless(Project::new("x"))
        .engine()
        .host()
        .catalog()
        .entries()[0]
        .plugin
        .clone();
    let (project, track) = edits::add_track(
        &Project::new("Frei"),
        NewTrack {
            name: Some("FM".into()),
            hue: Some(Hue::BLUE),
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (project, _) = edits::insert_device(
        &project,
        ChannelRef::Track(track),
        0,
        NewDevice {
            role: DeviceRole::Instrument,
            plugin: hexa,
            state: Some(encode_state(&patch).into()),
            sidechain: None,
        },
    )
    .unwrap();
    let mut app = App::headless(project);
    select_track(&mut app, track);
    let mut h = harness(app);
    h.run_steps(30);
    mask_cpu(&mut h);
    let mut results = SnapshotResults::new();
    h.snapshot("devices-fm-cycle-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
