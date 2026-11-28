//! The „SC“ chip in the device head and its popover (WP-104): devices with a sidechain input
//! (presse) have the chip, the popover lists the tracks and buses with the loops greyed out,
//! choosing a source is one undo step, and an EQ has no chip. Copying, pasting and duplicating a
//! device keep its source where it still fits (#7).

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::ui_state::DockTab;
use komponist_app::{Action, App, EditAction, UiAction};
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

/// The routing scene with verified shipped devices.
fn scene() -> Scene {
    let host = komponist_session::device_host::PluginHost::headless_fixture();
    let shipped = |name: &str| crate::common::shipped(&host, name);
    let hexa = shipped("hexa");
    let mut p = Project::new("Sidechain");
    let track = |p: &mut Project, name: &str| {
        let spec = NewTrack {
            name: Some(name.to_owned()),
            hue: None,
            instrument: Some(hexa.clone()),
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
    let device = |p: &mut Project, plugin: komponist_model::PluginRef| {
        let at = p.track(pad).unwrap().channel.devices.len();
        let spec = NewDevice {
            role: DeviceRole::Effect,
            plugin,
            state: None,
            sidechain: None,
            preset: None,
        };
        let (next, id) = edits::insert_device(p, ChannelRef::Track(pad), at, spec).unwrap();
        *p = next;
        id
    };
    let presse = device(&mut p, shipped("presse"));
    let eq = device(&mut p, shipped("eq"));
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
    let eq = h.get_by_role_and_label(Role::Group, Lang::De.devices_card("EQ").leak());
    assert!(eq.query_all_by_role(Role::Button).all(|b| {
        !b.accesskit_node()
            .label()
            .is_some_and(|l| l.starts_with("Sidechain"))
    }));
    let _ = s.eq;
}

/// #6: bridged plugins have the chip exactly when they have a sidechain port: `kp-test-sc` does,
/// `kp-test-fx` does not (the test plugins from `scripts/build-test-plugins.sh`; skipped without).
#[test]
fn a_bridged_plugin_has_the_chip_only_with_a_sidechain_port() {
    use crate::common::{all_ok, app_with, config, fixture_dirs, scratch, step_until};
    use komponist_session::device_host::{PluginCatalogHost, PluginHost, ProcessorHost};

    let Some(dirs) = fixture_dirs() else {
        return;
    };
    let dir = scratch("sidechain-chip");
    // A scan fills the cache the app's host starts from.
    let mut scan = PluginHost::new(config(&dir));
    scan.start_scan(&dirs);
    let deadline = std::time::Instant::now() + crate::common::PATIENCE;
    while scan.is_scanning() {
        assert!(std::time::Instant::now() < deadline, "scan");
        scan.poll();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let plugin = |id: &str| {
        scan.catalog()
            .entries()
            .iter()
            .find(|e| e.plugin.id == id)
            .unwrap_or_else(|| panic!("{id} not scanned"))
            .clone()
    };
    let (sc, fx) = (
        plugin("de.klang-studio.kp-test-sc"),
        plugin(crate::common::FX),
    );
    let instrument = plugin(crate::common::SYNTH);
    assert!(sc.sidechain && !fx.sidechain);
    scan.shutdown();
    drop(scan);

    let mut p = Project::new("Sidechain");
    let (next, pad) = edits::add_track(
        &p,
        NewTrack {
            name: Some("Pad".to_owned()),
            hue: None,
            instrument: Some(instrument.plugin),
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    p = next;
    let mut ids = Vec::new();
    for entry in [&sc, &fx] {
        let at = p.track(pad).unwrap().channel.devices.len();
        let spec = NewDevice {
            role: DeviceRole::Effect,
            plugin: entry.plugin.clone(),
            state: None,
            sidechain: None,
            preset: None,
        };
        let (next, id) = edits::insert_device(&p, ChannelRef::Track(pad), at, spec).unwrap();
        p = next;
        ids.push(id);
    }
    let mut app = app_with(p, PluginHost::new(config(&dir)));
    step_until(&mut app, "every device Ok", all_ok);
    assert!(app.engine().host().has_sidechain(ids[0]));
    assert!(!app.engine().host().has_sidechain(ids[1]));
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![pad],
        mode: SelectMode::Replace,
    })));
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Devices)));
    let mut h = harness(app);
    let is_chip = |b: &egui_kittest::Node<'_>| {
        b.accesskit_node()
            .label()
            .is_some_and(|l| l.starts_with("Sidechain"))
    };
    let card = |h: &Harness<'_, App>, name: &str| -> usize {
        h.get_by_role_and_label(Role::Group, Lang::De.devices_card(name).leak())
            .query_all_by_role(Role::Button)
            .filter(is_chip)
            .count()
    };
    assert_eq!(card(&h, &sc.plugin.name), 1);
    assert_eq!(card(&h, &fx.plugin.name), 0);
    chip(&h, "Sidechain: aus");
    h.state_mut().shutdown();
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

// ------------------------------------------------------------------------------------------------
// Copies keep the source (#7)

/// The ids of `track`'s chain.
fn chain(app: &App, track: TrackId) -> Vec<DeviceId> {
    let t = app.project().track(track).unwrap();
    t.channel.devices.iter().map(|d| d.id).collect()
}

/// The device of `track`'s chain that `before` did not have.
fn new_device(app: &App, track: TrackId, before: &[DeviceId]) -> DeviceId {
    let added: Vec<DeviceId> = chain(app, track)
        .into_iter()
        .filter(|d| !before.contains(d))
        .collect();
    assert_eq!(added.len(), 1, "one copy");
    added[0]
}

fn select_device(app: &mut App, device: DeviceId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Devices {
        devices: vec![device],
        mode: SelectMode::Replace,
    })));
}

fn undo_once(app: &mut App, track: TrackId, before: &[DeviceId]) {
    app.dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(
        chain(app, track),
        before,
        "one undo step takes the copy away"
    );
}

#[test]
fn copy_paste_duplicate_and_alt_drag_keep_the_sidechain_source_in_one_undo_step() {
    let s = scene();
    let mut app = app(&s);
    let source = Sidechain {
        source: s.drums,
        tap: Tap::Post,
    };
    app.dispatch(Action::Edit(EditAction::SetSidechain {
        device: s.presse,
        sidechain: Some(source),
    }));
    assert_eq!(sidechain_of(&app, s.presse), Some(source));
    let before = chain(&app, s.pad);

    // Ctrl C, Ctrl V.
    select_device(&mut app, s.presse);
    app.dispatch(Action::Ui(UiAction::CopyDevices));
    app.dispatch(Action::Edit(EditAction::PasteDevices));
    let copy = new_device(&app, s.pad, &before);
    assert_eq!(sidechain_of(&app, copy), Some(source), "paste");
    undo_once(&mut app, s.pad, &before);

    // Ctrl D.
    select_device(&mut app, s.presse);
    app.dispatch(Action::Edit(EditAction::DuplicateDevices));
    let copy = new_device(&app, s.pad, &before);
    assert_eq!(sidechain_of(&app, copy), Some(source), "duplicate");
    undo_once(&mut app, s.pad, &before);

    // Alt-drag onto another track and onto the master.
    let lead = app
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Lead")
        .unwrap()
        .id;
    let lead_chain = chain(&app, lead);
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: s.presse,
        to: ChannelRef::Track(lead),
        index: usize::MAX,
    }));
    let copy = new_device(&app, lead, &lead_chain);
    assert_eq!(sidechain_of(&app, copy), Some(source), "alt-drag");
    undo_once(&mut app, lead, &lead_chain);
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: s.presse,
        to: ChannelRef::Master,
        index: usize::MAX,
    }));
    let copy = app.project().master.devices.last().unwrap().id;
    assert_eq!(sidechain_of(&app, copy), Some(source), "onto the master");
    assert_eq!(
        sidechain_of(&app, s.presse),
        Some(source),
        "the original keeps it"
    );
}

#[test]
fn a_copy_that_would_close_a_loop_or_whose_source_is_gone_has_no_sidechain() {
    let s = scene();
    let mut app = app(&s);
    app.dispatch(Action::Edit(EditAction::SetSidechain {
        device: s.presse,
        sidechain: Some(Sidechain::new(s.drums)),
    }));

    // Onto the source track itself: no sidechain, no error, one undo step.
    let drums = chain(&app, s.drums);
    app.dispatch(Action::Edit(EditAction::CopyDevice {
        device: s.presse,
        to: ChannelRef::Track(s.drums),
        index: usize::MAX,
    }));
    let copy = new_device(&app, s.drums, &drums);
    assert_eq!(sidechain_of(&app, copy), None);
    assert_eq!(sidechain_of(&app, s.presse), Some(Sidechain::new(s.drums)));
    undo_once(&mut app, s.drums, &drums);

    // Copied, then the source deleted, then pasted: the copy has none.
    select_device(&mut app, s.presse);
    app.dispatch(Action::Ui(UiAction::CopyDevices));
    app.dispatch(Action::Edit(EditAction::RemoveTracks(vec![s.drums])));
    assert_eq!(sidechain_of(&app, s.presse), None);
    let before = chain(&app, s.pad);
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![s.pad],
        mode: SelectMode::Replace,
    })));
    select_device(&mut app, s.presse);
    app.dispatch(Action::Edit(EditAction::PasteDevices));
    let copy = new_device(&app, s.pad, &before);
    assert_eq!(sidechain_of(&app, copy), None);
}
