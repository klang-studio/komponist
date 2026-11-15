//! The „SC“ chip in the device head and its popover (WP-104): devices with a sidechain input
//! (presse) have the chip, the popover lists the tracks and buses with the loops greyed out,
//! choosing a source is one undo step, and an EQ has no chip.

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_builtin::Internal;
use komponist_i18n::{Lang, de};
use komponist_model::edits::{self, NewBus, NewDevice, NewTrack};
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, Sidechain, Tap, TrackId};

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

fn key(h: &mut Harness<'_, App>, key: Key) {
    for pressed in [true, false] {
        h.event(Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(3);
}

/// The scene: tracks „Drums“, „Pad“ and „Lead“; a group „Rhythmus“ holding „Pad“; an effect bus
/// „Hall“ that „Pad“ sends to; presse and an EQ on „Pad“.
struct Scene {
    drums: TrackId,
    pad: TrackId,
    presse: DeviceId,
    eq: DeviceId,
    project: Project,
}

fn scene() -> Scene {
    let probe = App::headless(Project::new("probe"));
    let pisynth = probe.engine().host().catalog().entries()[0].plugin.clone();
    let mut p = Project::new("Sidechain");
    let track = |p: &mut Project, name: &str| {
        let spec = NewTrack {
            name: Some(name.to_owned()),
            hue: None,
            instrument: Some(pisynth.clone()),
            index: None,
            arm_exclusive: false,
        };
        let (next, id) = edits::add_track(p, spec).unwrap();
        *p = next;
        id
    };
    let drums = track(&mut p, "Drums");
    let pad = track(&mut p, "Pad");
    let _lead = track(&mut p, "Lead");
    let device = |p: &mut Project, kind: Internal| {
        let at = p.track(pad).unwrap().channel.devices.len();
        let spec = NewDevice {
            role: DeviceRole::Effect,
            plugin: kind.plugin_ref(),
            state: None,
        };
        let (next, id) = edits::insert_device(p, ChannelRef::Track(pad), at, spec).unwrap();
        *p = next;
        id
    };
    let presse = device(&mut p, Internal::Presse);
    let eq = device(&mut p, Internal::Eq);
    let (next, _) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Rhythmus".into()),
            ..NewBus::group(vec![pad])
        },
    )
    .unwrap();
    p = next;
    let (next, _) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Hall".into()),
            ..NewBus::effect(vec![pad])
        },
    )
    .unwrap();
    Scene {
        drums,
        pad,
        presse,
        eq,
        project: next,
    }
}

fn app(s: &Scene) -> App {
    let mut app = App::headless(s.project.clone());
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![s.pad],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    app
}

fn sidechain_of(app: &App, device: DeviceId) -> Option<Sidechain> {
    app.project().find_device(device).unwrap().2.sidechain
}

fn chip<'h>(h: &'h Harness<'_, App>, label: &'h str) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::Button, label)
}

fn open(h: &mut Harness<'_, App>) {
    let rect = chip(h, "Sidechain: aus").rect();
    click(h, rect.center());
    h.run_steps(3);
}

#[test]
fn the_chip_opens_the_popover_and_choosing_a_source_sets_it_in_one_undo_step() {
    let s = scene();
    let mut h = harness(app(&s));
    open(&mut h);
    let rect = h.get_by_role_and_label(Role::MenuItemRadio, "Drums").rect();
    click(&mut h, rect.center());
    let want = Sidechain {
        source: s.drums,
        tap: Tap::Pre,
    };
    assert_eq!(sidechain_of(h.state(), s.presse), Some(want));
    // The popover is closed and the chip names the source.
    assert!(h.query_by_role(Role::MenuItemRadio).is_none());
    chip(&h, "Sidechain: Drums");
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(3);
    assert_eq!(sidechain_of(h.state(), s.presse), None);
    chip(&h, "Sidechain: aus");
}

#[test]
fn the_tap_segments_change_the_tap_and_keep_the_source() {
    let s = scene();
    let mut app = app(&s);
    app.dispatch(Action::Edit(EditAction::SetSidechain {
        device: s.presse,
        sidechain: Some(Sidechain::new(s.drums)),
    }));
    let mut h = harness(app);
    let rect = chip(&h, "Sidechain: Drums").rect();
    click(&mut h, rect.center());
    let post = h
        .get_by_role_and_label(Role::RadioButton, de::ROUTING_POST)
        .rect();
    click(&mut h, post.center());
    assert_eq!(
        sidechain_of(h.state(), s.presse),
        Some(Sidechain {
            source: s.drums,
            tap: Tap::Post
        })
    );
    // The tap stays open for more; „— (intern …)“ clears the source.
    let none = h
        .get_by_role_and_label(Role::MenuItemRadio, de::DEVICES_SC_INTERNAL)
        .rect();
    click(&mut h, none.center());
    assert_eq!(sidechain_of(h.state(), s.presse), None);
}

#[test]
fn sources_that_close_a_loop_are_disabled_with_their_reason() {
    let s = scene();
    let mut h = harness(app(&s));
    open(&mut h);
    for name in [
        "Pad (diese Spur), nicht möglich: Rückkopplung",
        "Rhythmus, nicht möglich: Rückkopplung",
        "Hall, nicht möglich: Rückkopplung",
    ] {
        let row = h.get_by_role_and_label(Role::MenuItemRadio, name);
        assert!(row.accesskit_node().is_disabled(), "{name}");
        let rect = row.rect();
        click(&mut h, rect.center());
        assert_eq!(sidechain_of(h.state(), s.presse), None, "{name} clicked");
        // The popover stays open.
        h.get_by_role_and_label(Role::MenuItemRadio, "Drums");
    }
    for name in ["Drums", "Lead", de::DEVICES_SC_INTERNAL] {
        let row = h.get_by_role_and_label(Role::MenuItemRadio, name);
        assert!(!row.accesskit_node().is_disabled(), "{name}");
    }
}

#[test]
fn the_keys_move_over_the_rows_choose_with_enter_and_close_with_escape() {
    let s = scene();
    let mut h = harness(app(&s));
    open(&mut h);
    key(&mut h, Key::Escape);
    assert!(h.query_by_role(Role::MenuItemRadio).is_none());
    open(&mut h);
    // The highlight starts on „— (intern …)“; ↓ goes to „Drums“.
    key(&mut h, Key::ArrowDown);
    key(&mut h, Key::Enter);
    assert_eq!(
        sidechain_of(h.state(), s.presse).map(|c| c.source),
        Some(s.drums)
    );
    assert!(h.query_by_role(Role::MenuItemRadio).is_none());
}

#[test]
fn a_device_without_a_sidechain_input_has_no_chip() {
    let s = scene();
    let h = harness(app(&s));
    let chips = h
        .query_all_by_role(Role::Button)
        .filter(|b| {
            b.accesskit_node()
                .label()
                .is_some_and(|l| l.starts_with("Sidechain"))
        })
        .count();
    // presse has one, the EQ does not.
    assert_eq!(chips, 1);
    let eq = h.get_by_role_and_label(
        Role::Group,
        Lang::De.devices_card(Internal::Eq.name()).leak(),
    );
    assert!(eq.query_all_by_role(Role::Button).all(|b| {
        !b.accesskit_node()
            .label()
            .is_some_and(|l| l.starts_with("Sidechain"))
    }));
    let _ = s.eq;
}

#[test]
fn the_context_menu_of_the_chip_removes_the_source() {
    let s = scene();
    let mut app = app(&s);
    app.dispatch(Action::Edit(EditAction::SetSidechain {
        device: s.presse,
        sidechain: Some(Sidechain::new(s.drums)),
    }));
    let mut h = harness(app);
    let rect = chip(&h, "Sidechain: Drums").rect();
    h.run_steps(30);
    h.hover_at(rect.center());
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: rect.center(),
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(3);
    let item = h
        .get_by_role_and_label(Role::MenuItem, de::DEVICES_SC_REMOVE)
        .rect();
    click(&mut h, item.center());
    assert_eq!(sidechain_of(h.state(), s.presse), None);
}

#[test]
fn snapshots_of_the_head_with_the_chip_off_on_and_the_popover_open() {
    let s = scene();
    let mut results = SnapshotResults::new();
    let mask = |h: &mut Harness<'_, App>| {
        let cpu = h
            .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
            .rect();
        h.mask(cpu.expand(2.0));
    };
    let mut h = harness(app(&s));
    h.run_steps(2);
    mask(&mut h);
    h.snapshot("devices-sidechain-off-1x");
    results.extend_harness(&mut h);

    let mut app = app(&s);
    app.dispatch(Action::Edit(EditAction::SetSidechain {
        device: s.presse,
        sidechain: Some(Sidechain::new(s.drums)),
    }));
    let mut h = harness(app);
    h.run_steps(2);
    mask(&mut h);
    h.snapshot("devices-sidechain-on-1x");
    results.extend_harness(&mut h);

    let rect = chip(&h, "Sidechain: Drums").rect();
    click(&mut h, rect.center());
    h.run_steps(4);
    mask(&mut h);
    h.snapshot("devices-sidechain-popover-1x");
    results.extend_harness(&mut h);

    // The post-fader tap shows its hint: it follows fader and mute, not solo.
    let post = h
        .get_by_role_and_label(Role::RadioButton, de::ROUTING_POST)
        .rect();
    click(&mut h, post.center());
    h.run_steps(3);
    mask(&mut h);
    h.snapshot("devices-sidechain-popover-post-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}
