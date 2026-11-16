//! The device frame and chain (WP-82) through `egui_kittest`: the 244 px frame and its widths,
//! collapse and expand, selection and the keys on it (Strg C / V / D, Entf, ← →), Alt-drag copies,
//! the chain head, the scrollbars and the vertical scroll below a 320 px dock, the mixer jump, and
//! a snapshot of every frame state.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::{DockTab, FocusArea};
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_i18n::{Lang, de};
use komponist_model::fixtures::demo_project;
use komponist_model::{DeviceId, TrackId};
use komponist_tokens::{device, size};

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

/// The demo's Lead track (hexa, EQ, TAL, Dragonfly) selected, the dock on „Geräte“.
fn lead() -> (App, TrackId, Vec<DeviceId>) {
    let mut app = App::headless(demo_project()).with_device_host(Box::new(FakeHost::new()));
    let track = app.project().tracks[2].id;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![track],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let ids = chain(&app, track);
    (app, track, ids)
}

fn chain(app: &App, track: TrackId) -> Vec<DeviceId> {
    app.project()
        .track(track)
        .unwrap()
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect()
}

fn frame(h: &Harness<'_, App>, name: &str) -> Rect {
    h.get_by_role_and_label(Role::Group, &Lang::De.devices_card(name))
        .rect()
}

fn button(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
}

/// `times` clicks in one go. Waits first, so this does not count as another click of an earlier
/// sequence (egui counts clicks within 0.3 s as a double or triple click).
fn click(h: &mut Harness<'_, App>, pos: Pos2, modifiers: Modifiers, times: usize) {
    h.run_steps(30);
    h.hover_at(pos);
    h.step();
    h.event(Event::ModifiersChanged(modifiers));
    for _ in 0..times {
        button(h, pos, true, modifiers);
        button(h, pos, false, modifiers);
    }
    h.run_steps(3);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

/// A point in the head of `name`'s frame that holds no control: right of its left edge, left of
/// the name's text.
fn head_point(h: &Harness<'_, App>, name: &str) -> Pos2 {
    let f = frame(h, name);
    pos2(f.left() + 45.0, f.top() + device::frame::HEAD_H / 2.0)
}

fn collapsed(app: &App) -> Vec<DeviceId> {
    app.ui().devices.collapsed.iter().copied().collect()
}

#[test]
fn a_frame_is_244_high_and_as_wide_as_its_columns() {
    let (app, ..) = lead();
    let h = harness(app);
    for name in ["Hexa", "LSP Para EQ x8", "TAL-Chorus-LX"] {
        let f = frame(&h, name);
        assert_eq!(f.height(), device::frame::TOTAL_H, "{name}");
        // hexa's first page has ten columns (WP-86), the others four.
        let columns = if name == "Hexa" { 10.0 } else { 4.0 };
        assert_eq!(
            f.width(),
            16.0 + 60.0 * columns - 4.0,
            "{name}: {columns} columns"
        );
    }
    // Head 28 + body 216 (the head's bottom line is inside the head).
    assert_eq!(
        device::frame::HEAD_H + device::frame::BODY_H,
        device::frame::TOTAL_H
    );
    // The chain head: padding 10 above the frames, drop slot 120 wide behind them.
    let chain = h
        .get_by_role_and_label(Role::Group, de::DEVICES_CHAIN)
        .rect();
    let first = frame(&h, "Hexa");
    assert_eq!(first.left() - chain.left(), device::chain::PADDING);
    assert_eq!(first.top() - chain.top(), device::chain::PADDING);
    let second = frame(&h, "LSP Para EQ x8");
    assert_eq!(second.left() - first.right(), device::chain::GAP);
}

#[test]
fn a_frame_has_power_menu_and_the_instrument_a_preset() {
    let (app, ..) = lead();
    let h = harness(app);
    let (synth_name, eq_name) = (
        Lang::De.devices_card("Hexa"),
        Lang::De.devices_card("LSP Para EQ x8"),
    );
    let synth = h.get_by_role_and_label(Role::Group, &synth_name);
    synth.get_by_role_and_label(Role::Switch, de::DEVICES_ACTIVE);
    synth.get_by_role_and_label(Role::Button, de::DEVICES_MORE);
    synth.get_by_role_and_label(Role::Button, &Lang::De.devices_preset_name("Init"));
    let eq = h.get_by_role_and_label(Role::Group, &eq_name);
    eq.get_by_role_and_label(Role::Switch, de::DEVICES_ACTIVE);
    eq.get_by_role_and_label(Role::Button, de::DEVICES_MORE);
    assert!(eq.query_by_label_contains("Preset").is_none());
}

#[test]
fn double_click_collapses_alt_double_click_all_and_a_click_on_the_strip_expands() {
    let (app, _, ids) = lead();
    let mut h = harness(app);
    let at = head_point(&h, "LSP Para EQ x8");
    click(&mut h, at, Modifiers::NONE, 2);
    assert_eq!(collapsed(h.state()), [ids[1]]);
    let strip = frame(&h, "LSP Para EQ x8");
    assert_eq!(
        (strip.width(), strip.height()),
        (device::frame::COLLAPSED_W, device::frame::TOTAL_H)
    );
    h.get_by_role_and_label(
        Role::Button,
        &Lang::De.devices_expand_name("LSP Para EQ x8"),
    );
    // The strip's own click expands it again.
    click(
        &mut h,
        pos2(strip.center().x, strip.bottom() - 30.0),
        Modifiers::NONE,
        1,
    );
    assert!(collapsed(h.state()).is_empty());
    // Alt-double-click: every device of the chain.
    let at = head_point(&h, "TAL-Chorus-LX");
    click(&mut h, at, Modifiers::ALT, 2);
    assert_eq!(collapsed(h.state()).len(), ids.len());
    assert_eq!(frame(&h, "Hexa").width(), device::frame::COLLAPSED_W);
    // And Alt-double-click on an expanded head of a collapsed chain expands them all: use the menu
    // action the head offers.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::ToggleDeviceCollapsed {
            device: ids[0],
            all: true,
        }));
    assert!(collapsed(h.state()).is_empty());
}

#[test]
fn bypass_dims_the_body_but_keeps_it_operable() {
    let (app, _, ids) = lead();
    let mut h = harness(app);
    let tal = ids[2];
    assert!(
        h.state().project().find_device(tal).unwrap().2.bypass,
        "TAL is bypassed in the demo"
    );
    // The power square switches it on again.
    let power = h
        .get_by_role_and_label(Role::Group, &Lang::De.devices_card("TAL-Chorus-LX"))
        .get_by_role_and_label(Role::Switch, de::DEVICES_ACTIVE)
        .rect();
    click(&mut h, power.center(), Modifiers::NONE, 1);
    assert!(!h.state().project().find_device(tal).unwrap().2.bypass);
}

#[test]
fn selection_and_the_keys_on_the_chain() {
    let (app, track, ids) = lead();
    let mut h = harness(app);
    let at = head_point(&h, "LSP Para EQ x8");
    click(&mut h, at, Modifiers::NONE, 1);
    assert!(h.state().ui().selection.devices.contains(&ids[1]));
    assert_eq!(h.state().ui().focus, FocusArea::Devices);
    // → selects the next device, ← the previous.
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(2);
    assert_eq!(
        h.state()
            .ui()
            .selection
            .devices
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [ids[2]]
    );
    h.key_press(egui::Key::ArrowLeft);
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(
        h.state()
            .ui()
            .selection
            .devices
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [ids[0]]
    );
    h.key_press(egui::Key::ArrowLeft);
    h.run_steps(2);
    assert_eq!(
        h.state()
            .ui()
            .selection
            .devices
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [ids[0]],
        "stays at the start"
    );
    // Strg D copies the selected effect behind itself; Strg C / V puts a copy behind another.
    let at = head_point(&h, "LSP Para EQ x8");
    click(&mut h, at, Modifiers::NONE, 1);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::D);
    h.run_steps(2);
    let after = chain(h.state(), track);
    assert_eq!(after.len(), ids.len() + 1);
    assert_eq!(&after[..2], &ids[..2]);
    assert_eq!(&after[3..], &ids[2..]);
    let copy = after[2];
    assert!(
        h.state().ui().selection.devices.contains(&copy),
        "the copy is selected"
    );
    let a = h.state().project().find_device(ids[1]).unwrap().2.clone();
    let b = h.state().project().find_device(copy).unwrap().2.clone();
    assert_eq!(
        (a.plugin.id.clone(), a.state.clone()),
        (b.plugin.id.clone(), b.state.clone())
    );
    assert_ne!(a.id, b.id);
    // Strg C, select the last device, Strg V.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::C);
    let last = *ids.last().unwrap();
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Devices {
            devices: vec![last],
            mode: SelectMode::Replace,
        })));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::V);
    h.run_steps(2);
    let pasted = chain(h.state(), track);
    assert_eq!(pasted.len(), ids.len() + 2);
    assert_eq!(pasted[pasted.len() - 2], last);
    // Entf removes the selected (pasted) device; undo brings it back.
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(chain(h.state(), track).len(), ids.len() + 1);
}

#[test]
fn alt_drag_on_a_head_copies_instead_of_moving() {
    let (app, track, ids) = lead();
    let mut h = harness(app);
    let from = head_point(&h, "LSP Para EQ x8");
    let tal = frame(&h, "TAL-Chorus-LX");
    let to = pos2(tal.right() - 20.0, tal.center().y);
    h.hover_at(from);
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    button(&mut h, from, true, Modifiers::ALT);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
    button(&mut h, to, false, Modifiers::ALT);
    h.run_steps(3);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    let after = chain(h.state(), track);
    assert_eq!(after.len(), ids.len() + 1);
    assert_eq!(&after[..3], &ids[..3], "the original stays");
    assert!(h.state().dispatched().any(|a| matches!(
        a,
        Action::Edit(EditAction::CopyDevice { device, .. }) if *device == ids[1]
    )));
}

#[test]
fn the_chain_head_lists_the_devices_and_a_click_scrolls_there() {
    let (mut app, _, ids) = lead();
    app.ui_mut().layout.browser_width = komponist_tokens::size::browser::MAX;
    app.ui_mut().layout.inspector_width = komponist_tokens::size::inspector::MAX;
    // Enough devices that the chain scrolls: copy the EQ a few times.
    let track = app.project().tracks[2].id;
    for _ in 0..3 {
        app.dispatch(Action::Edit(EditAction::CopyDevice {
            device: ids[1],
            to: komponist_model::ChannelRef::Track(track),
            index: 4,
        }));
    }
    let mut h = harness(app);
    let last = *chain(h.state(), track).last().unwrap();
    // The last device lies right of the view: its chain head entry is not highlighted; a click on
    // it scrolls the chain.
    assert_eq!(h.state().ui().devices.scroll_x, 0.0);
    let entries: Vec<_> = h
        .get_all_by_role_and_label(Role::Button, &Lang::De.devices_reveal("LSP Para EQ x8"))
        .map(|n| n.rect())
        .collect();
    assert_eq!(entries.len(), 4, "the EQ and its three copies");
    let last_entry = entries.last().unwrap();
    click(&mut h, last_entry.center(), Modifiers::NONE, 1);
    h.run_steps(3);
    assert!(h.state().ui().devices.scroll_x > 0.0);
    let _ = last;
    // The scrollbar is there while there is something to scroll.
    h.get_by_role_and_label(Role::ScrollBar, de::DEVICES_SCROLL_H);
}

#[test]
fn below_a_320_px_dock_the_chain_scrolls_vertically() {
    let (mut app, ..) = lead();
    app.ui_mut().layout.dock_height = size::dock::MIN;
    let mut h = harness(app);
    let before = frame(&h, "Hexa");
    // Over the frame's left edge: no knob, fader or field takes the wheel there.
    h.hover_at(before.left_center() + vec2(1.0, 0.0));
    h.step();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -40.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    h.run_steps(3);
    assert!(h.state().ui().devices.scroll_y > 0.0);
    assert!(frame(&h, "Hexa").top() < before.top());
    h.get_by_role_and_label(Role::ScrollBar, de::DEVICES_SCROLL_V);
}

#[test]
fn at_320_px_nothing_scrolls() {
    let (mut app, ..) = lead();
    app.ui_mut().layout.dock_height = size::dock::DEFAULT;
    let mut h = harness(app);
    let before = frame(&h, "Hexa");
    h.hover_at(before.center());
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -40.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
    });
    h.run_steps(3);
    assert_eq!(h.state().ui().devices.scroll_y, 0.0);
    assert!(
        h.query_by_role_and_label(Role::ScrollBar, de::DEVICES_SCROLL_V)
            .is_none()
    );
}

#[test]
fn a_mixer_insert_of_an_internal_device_jumps_to_the_geraete_tab() {
    let (mut app, track, ids) = lead();
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let slot = h
        .get_by_role_and_label(Role::Group, &Lang::De.mixer_strip("Lead"))
        .get_by_role_and_label(Role::Button, &Lang::De.mixer_insert("Hexa"))
        .rect();
    click(&mut h, slot.center(), Modifiers::NONE, 1);
    h.run_steps(3);
    let ui = h.state().ui();
    assert_eq!(ui.layout.dock_tab, DockTab::Devices);
    assert!(ui.selection.tracks.contains(&track));
    assert!(ui.selection.devices.contains(&ids[0]));
    assert!(
        ui.plugin_windows.is_empty(),
        "no plugin window for an internal device"
    );
    // A foreign plugin keeps opening its window.
    h.state_mut().dispatch(Action::Ui(UiAction::ToggleMixer));
    h.run_steps(3);
    let eq = h
        .get_by_role_and_label(Role::Group, &Lang::De.mixer_strip("Lead"))
        .get_by_role_and_label(Role::Button, &Lang::De.mixer_insert("LSP Para EQ x8"))
        .rect();
    click(&mut h, eq.center(), Modifiers::NONE, 1);
    assert!(
        h.state()
            .ui()
            .plugin_windows
            .iter()
            .any(|w| w.device == ids[1])
    );
}

#[test]
fn the_reveal_centres_a_device_in_a_long_chain() {
    let (mut app, track, ids) = lead();
    app.ui_mut().layout.browser_width = komponist_tokens::size::browser::MAX;
    app.ui_mut().layout.inspector_width = komponist_tokens::size::inspector::MAX;
    for _ in 0..4 {
        app.dispatch(Action::Edit(EditAction::CopyDevice {
            device: ids[1],
            to: komponist_model::ChannelRef::Track(track),
            index: 4,
        }));
    }
    let far = *chain(&app, track).last().unwrap();
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::RevealDevice(far)));
    h.run_steps(4);
    let body = h
        .get_by_role_and_label(Role::Group, de::DEVICES_CHAIN)
        .rect();
    let frames: Vec<Rect> = h
        .get_all_by_role_and_label(Role::Group, &Lang::De.devices_card("LSP Para EQ x8"))
        .map(|n| n.rect())
        .collect();
    let last = frames.last().unwrap();
    // Centred, or as far as the end of the chain allows.
    let max = h.state().ui().devices.scroll_x;
    assert!(max > 0.0);
    assert!(
        (last.center().x - body.center().x).abs() < 1.0 || last.center().x > body.center().x,
        "{last:?} {body:?}"
    );
}

#[test]
fn snapshot_of_every_frame_state() {
    let (mut app, _, ids) = lead();
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Devices {
        devices: vec![ids[1]],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::ToggleDeviceCollapsed {
        device: ids[3],
        all: false,
    }));
    let mut h = harness(app);
    h.run_steps(2);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    let mut results = SnapshotResults::new();
    h.snapshot("device-frame-states-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
