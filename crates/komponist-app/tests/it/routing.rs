//! The routing UI (WP-103): Strg G, folders, sends and the output list in the mixer, the dock
//! chip, the arrangement's group and bus rows, the inspector's „Routing“ section, the
//! new-track dialog's „Bus“ type. Snapshots next to the routing proposal's mockups (internal
//! notes).

use crate::fake_host;

use std::path::PathBuf;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::views::routing::names;
use komponist_app::{Action, App, EditAction, UiAction};
use komponist_model::edits::{self, NewBus, SendProp};
use komponist_model::fixtures::demo_project;
use komponist_model::{Project, Tap, TrackId, TrackOutput, routing};
use std::sync::Arc;

use fake_host::FakeHost;

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

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

fn id_of(p: &Project, name: &str) -> TrackId {
    p.tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap_or_else(|| panic!("no track {name}"))
        .id
}

/// The demo (Drums Bass Lead Pad Keys Arp) with two groups („Rhythmus“: Drums, Bass; „Melodie“:
/// Lead, Arp), two effect buses („Hall“ and „Delay“) and sends: Drums → Hall −10, Lead → Hall
/// −6 pre, Lead → Delay −16, Pad → Hall −6, Keys → Delay −12.
fn routed() -> Project {
    let p = demo_project();
    let t = |p: &Project, n: &str| id_of(p, n);
    let (p, _) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Rhythmus".into()),
            ..NewBus::group(vec![t(&p, "Drums"), t(&p, "Bass")])
        },
    )
    .unwrap();
    let (p, _) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Melodie".into()),
            ..NewBus::group(vec![t(&p, "Lead"), t(&p, "Arp")])
        },
    )
    .unwrap();
    let senders = |p: &Project, names: &[&str]| names.iter().map(|n| t(p, n)).collect::<Vec<_>>();
    let (p, hall) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Hall".into()),
            ..NewBus::effect(senders(&p, &["Drums", "Lead", "Pad"]))
        },
    )
    .unwrap();
    let (p, delay) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Delay".into()),
            ..NewBus::effect(senders(&p, &["Lead", "Keys"]))
        },
    )
    .unwrap();
    let send_of = |p: &Project, track: &str, bus: TrackId| {
        p.track(t(p, track)).unwrap().send_to(bus).unwrap().id
    };
    let mut p = p;
    for (track, bus, db, tap) in [
        ("Drums", hall, -10.0, Tap::Post),
        ("Lead", hall, -6.0, Tap::Pre),
        ("Lead", delay, -16.0, Tap::Post),
        ("Pad", hall, -6.0, Tap::Post),
        ("Keys", delay, -12.0, Tap::Post),
    ] {
        let id = send_of(&p, track, bus);
        p = edits::set_send(&p, id, SendProp::LevelDb(db)).unwrap();
        p = edits::set_send(&p, id, SendProp::Tap(tap)).unwrap();
    }
    p
}

fn app_with(p: Project) -> App {
    App::headless(p).with_device_host(Box::new(FakeHost::new()))
}

fn select(app: &mut App, t: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })));
}

#[test]
fn the_full_mixer_with_groups_buses_and_sends() {
    let mut app = app_with(routed());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    mask_cpu(&mut h);
    h.snapshot("routing-mixer-full-1x");
}

#[test]
fn the_arrangement_with_folder_and_busse_section() {
    let mut app = app_with(routed());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    let mut h = harness(app);
    mask_cpu(&mut h);
    h.snapshot("routing-arrangement-1x");
}

// ------------------------------------------------------------------------------------------------
// Helpers

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool, button: PointerButton) {
    h.event(Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers: Modifiers::NONE,
    });
}

/// Half a second without input, so the next click is no double-click.
fn settle(h: &mut Harness<'_, App>) {
    h.run_steps(30);
}

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    press(h, pos, true, PointerButton::Primary);
    press(h, pos, false, PointerButton::Primary);
    h.run_steps(3);
}

fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        press(h, pos, true, PointerButton::Primary);
        press(h, pos, false, PointerButton::Primary);
        h.step();
    }
    h.run_steps(2);
}

fn right_click(h: &mut Harness<'_, App>, pos: Pos2) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    press(h, pos, true, PointerButton::Secondary);
    press(h, pos, false, PointerButton::Secondary);
    h.run_steps(3);
}

/// A right-click at `pos`, then a click on the menu entry `label`.
fn menu_choice(h: &mut Harness<'_, App>, pos: Pos2, label: &str) {
    right_click(h, pos);
    let item = h.get_by_role_and_label(Role::MenuItem, label).rect();
    click(h, item.center());
    h.run_steps(2);
}

/// Press at `from` and move to `to` in eight frames, without releasing.
fn drag_to(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    settle(h);
    h.hover_at(from);
    h.step();
    press(h, from, true, PointerButton::Primary);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
}

fn release(h: &mut Harness<'_, App>, at: Pos2) {
    press(h, at, false, PointerButton::Primary);
    h.run_steps(3);
}

fn edits_of<'a>(app: &'a App, f: impl Fn(&EditAction) -> bool + 'a) -> Vec<&'a EditAction> {
    app.dispatched()
        .filter_map(|a| match a {
            Action::Edit(e) if f(e) => Some(e),
            _ => None,
        })
        .collect()
}

fn mixer_app(p: Project) -> App {
    let mut app = app_with(p);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    app
}

/// The strip group of track or bus `name` (`Kanal …` or `Busspur …`).
fn strip_rect(h: &Harness<'_, App>, label: &str) -> Rect {
    h.get_by_role_and_label(Role::Group, label).rect()
}

/// The arrangement's header row of a track (`Spur Drums, Sampler`) or a group.
fn header_rect(h: &Harness<'_, App>, name: &str) -> Rect {
    let prefixes = [
        format!("Spur {name}"),
        names::group(name, false),
        names::group(name, true),
        names::effect_bus(name),
    ];
    h.get_all_by_role(Role::Row)
        .find(|n| {
            n.accesskit_node().label().is_some_and(|l| {
                prefixes
                    .iter()
                    .any(|p| l == *p || l.starts_with(&format!("{p},")))
            })
        })
        .unwrap_or_else(|| panic!("no header {name}"))
        .rect()
}

fn has_header(h: &Harness<'_, App>, name: &str) -> bool {
    let prefix = format!("Spur {name}");
    h.get_all_by_role(Role::Row).any(|n| {
        n.accesskit_node()
            .label()
            .is_some_and(|l| l == prefix || l.starts_with(&format!("{prefix},")))
    })
}

fn select_many(app: &mut App, tracks: &[TrackId]) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: tracks.to_vec(),
        mode: SelectMode::Replace,
    })));
}

// ------------------------------------------------------------------------------------------------
// Creating: Strg G, „Send zu neuem Bus“

#[test]
fn ctrl_g_groups_the_selected_tracks_in_one_undo_step() {
    let p = demo_project();
    let (drums, bass) = (id_of(&p, "Drums"), id_of(&p, "Bass"));
    let mut app = app_with(p);
    select_many(&mut app, &[drums, bass]);
    let mut h = harness(app);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::G);
    h.run_steps(3);
    let p = h.state().project();
    let group = p.tracks.iter().find(|t| t.is_group()).expect("a group");
    assert_eq!(group.name, "Gruppe 1");
    assert_eq!(p.tracks[0].id, group.id, "where the topmost track was");
    for t in [drums, bass] {
        assert_eq!(p.track(t).unwrap().output, TrackOutput::Bus(group.id));
    }
    assert_eq!(routing::group_members(p, group.id), vec![drums, bass]);
    // The group is selected and its name is in the edit mode.
    assert!(h.state().ui().selection.tracks.contains(&group.id));
    assert!(
        h.query_by_role_and_label(Role::TextInput, "Spurname")
            .is_some(),
        "the name field is open"
    );
    // One undo step for all of it.
    let before = h.state().project().tracks.len();
    assert_eq!(before, 7);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks.len(), 6);
    assert!(h.state().project().tracks.iter().all(|t| !t.is_bus()));
    assert_eq!(
        h.state().project().track(drums).unwrap().output,
        TrackOutput::Master
    );
}

#[test]
fn send_to_new_bus_creates_a_bus_and_a_send_at_minus_6_post() {
    let mut app = app_with(demo_project());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    let mut h = harness(app);
    let at = header_rect(&h, "Lead").center();
    menu_choice(&mut h, at, names::SEND_TO_NEW_BUS);
    let p = h.state().project();
    let bus = p.tracks.iter().find(|t| t.is_bus()).expect("a bus");
    assert!(!bus.folder, "an effect bus");
    assert_eq!(bus.name, "Bus 1");
    let send = p.track(lead).unwrap().send_to(bus.id).expect("a send");
    assert_eq!(
        (send.level_db, send.tap, send.enabled),
        (-6.0, Tap::Post, true)
    );
    // It stands in the „Busse“ section and one undo removes both.
    assert!(
        h.query_by_role_and_label(Role::Label, names::SECTION)
            .is_some()
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert!(h.state().project().tracks.iter().all(|t| !t.is_bus()));
    assert!(h.state().project().track(lead).unwrap().sends.is_empty());
}

#[test]
fn the_context_menu_groups_the_selection_and_names_the_shortcut() {
    let p = demo_project();
    let (drums, bass) = (id_of(&p, "Drums"), id_of(&p, "Bass"));
    let mut app = app_with(p);
    select_many(&mut app, &[drums, bass]);
    let mut h = harness(app);
    let at = header_rect(&h, "Drums").center();
    right_click(&mut h, at);
    let item = h.get_by_role_and_label(Role::MenuItem, names::GROUP_TRACKS);
    let rect = item.rect();
    click(&mut h, rect.center());
    h.run_steps(2);
    assert!(h.state().project().tracks.iter().any(|t| t.is_group()));
    assert_eq!(
        edits_of(h.state(), |e| matches!(e, EditAction::GroupTracks(_))).len(),
        1
    );
}

// ------------------------------------------------------------------------------------------------
// Groups in the arrangement

#[test]
fn a_group_folds_and_unfolds() {
    let mut h = harness(app_with(routed()));
    assert!(has_header(&h, "Drums") && has_header(&h, "Bass"));
    let fold = h
        .get_by_role_and_label(Role::Button, "Gruppe Rhythmus einklappen")
        .rect();
    click(&mut h, fold.center());
    let rhythmus = id_of(h.state().project(), "Rhythmus");
    assert!(h.state().project().track(rhythmus).unwrap().folded);
    assert!(
        !has_header(&h, "Drums") && !has_header(&h, "Bass"),
        "members are folded away"
    );
    assert!(
        h.query_by_role_and_label(Role::Row, &names::group("Rhythmus", true))
            .is_some()
    );
    // The other group stays open.
    assert!(has_header(&h, "Lead"));
    let unfold = h
        .get_by_role_and_label(Role::Button, "Gruppe Rhythmus ausklappen")
        .rect();
    click(&mut h, unfold.center());
    assert!(!h.state().project().track(rhythmus).unwrap().folded);
    assert!(has_header(&h, "Drums"));
}

#[test]
fn deleting_a_group_dissolves_it() {
    let mut h = harness(app_with(routed()));
    let (drums, bass) = (
        id_of(h.state().project(), "Drums"),
        id_of(h.state().project(), "Bass"),
    );
    let at = header_rect(&h, "Rhythmus").center();
    menu_choice(&mut h, at, names::UNGROUP);
    let p = h.state().project();
    assert!(
        p.tracks.iter().all(|t| t.name != "Rhythmus"),
        "the group is gone"
    );
    for t in [drums, bass] {
        assert_eq!(
            p.track(t).unwrap().output,
            TrackOutput::Master,
            "the tracks stay"
        );
    }
    // One undo brings the group back with its members.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    let p = h.state().project();
    let group = id_of(p, "Rhythmus");
    assert_eq!(routing::group_members(p, group), vec![drums, bass]);
}

/// Deleting an effect bus acts at once (owner, 2026-11-15): no dialog, one undo step that brings
/// back the bus, the sends to it and the outputs into it.
fn deleted_bus_comes_back_with_one_undo(
    mut h: Harness<'static, App>,
    delete: impl Fn(&mut Harness<'_, App>),
) {
    let before = Arc::clone(h.state().project());
    let hall = id_of(&before, "Hall");
    let keys = id_of(&before, "Keys");
    assert_eq!(before.track(keys).unwrap().output, TrackOutput::Bus(hall));
    delete(&mut h);
    assert!(h.state().ui().dialog.is_none(), "no confirmation dialog");
    let p = h.state().project();
    assert!(p.tracks.iter().all(|t| t.name != "Hall"), "the bus is gone");
    assert!(
        p.tracks
            .iter()
            .flat_map(|t| &t.sends)
            .all(|s| s.target != hall),
        "its sends are gone"
    );
    assert_eq!(p.track(keys).unwrap().output, TrackOutput::Master);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    let p = h.state().project();
    assert_eq!(p.tracks.len(), before.tracks.len());
    for t in &before.tracks {
        let now = p.track(t.id).expect("track is back");
        assert_eq!(now.output, t.output, "{}", t.name);
        assert_eq!(now.sends, t.sends, "{}", t.name);
    }
}

fn routed_with_output_to_hall() -> Project {
    let p = routed();
    let (keys, hall) = (id_of(&p, "Keys"), id_of(&p, "Hall"));
    edits::set_output(&p, &[keys], TrackOutput::Bus(hall)).unwrap()
}

#[test]
fn deleting_a_bus_from_its_menu_needs_no_confirmation() {
    let h = harness(app_with(routed_with_output_to_hall()));
    deleted_bus_comes_back_with_one_undo(h, |h| {
        let at = header_rect(h, "Hall").center();
        menu_choice(h, at, names::DELETE_BUS);
    });
}

#[test]
fn del_deletes_the_selected_bus_at_once() {
    let p = routed_with_output_to_hall();
    let hall = id_of(&p, "Hall");
    let mut app = app_with(p);
    select(&mut app, hall);
    let h = harness(app);
    deleted_bus_comes_back_with_one_undo(h, |h| {
        h.key_press(egui::Key::Delete);
        h.run_steps(3);
    });
}

#[test]
fn clips_are_refused_on_buses() {
    let mut h = harness(app_with(routed()));
    let p = Arc::clone(h.state().project());
    let (group, hall) = (id_of(&p, "Rhythmus"), id_of(&p, "Hall"));
    // The model refuses.
    for bus in [group, hall] {
        let before = Arc::clone(h.state().project());
        h.state_mut().dispatch(Action::Edit(EditAction::AddClip {
            track: bus,
            clip: komponist_model::edits::NewClip {
                start: komponist_model::Tick(0),
                length: komponist_model::Tick(3840),
                name: "x".into(),
                content: komponist_model::edits::NewClipContent::Midi {
                    notes: Vec::new(),
                    take_name: String::new(),
                },
            },
        }));
        assert!(Arc::ptr_eq(&before, h.state().project()) || *before == **h.state().project());
        assert!(h.state().project().track(bus).unwrap().clips.is_empty());
    }
    // A double-click on the lane of an effect bus adds nothing and says why.
    let lane = header_rect(&h, "Hall");
    h.state_mut().clear_dispatched();
    let at = pos2(900.0, lane.center().y);
    double_click(&mut h, at);
    assert!(h.state().project().track(hall).unwrap().clips.is_empty());
    assert!(
        edits_of(h.state(), |e| matches!(e, EditAction::AddClip { .. })).is_empty(),
        "no AddClip is sent"
    );
    let status = h.state().ui().status.as_ref().map(|s| s.text.clone());
    assert_eq!(status.as_deref(), Some(names::NO_CLIPS));
}

// ------------------------------------------------------------------------------------------------
// The mixer: bus strips, sends, the output list

#[test]
fn bus_strips_stand_right_of_the_tracks_before_the_master() {
    let h = harness(mixer_app(routed()));
    let lead = strip_rect(&h, "Kanal Lead");
    let arp = strip_rect(&h, "Kanal Arp");
    let master = strip_rect(&h, "Kanal Master");
    let buses: Vec<Rect> = ["Rhythmus", "Melodie", "Hall", "Delay"]
        .iter()
        .map(|n| strip_rect(&h, &names::bus_strip(n)))
        .collect();
    assert!(arp.right() <= buses[0].left(), "buses right of every track");
    assert!(lead.left() < arp.left());
    for w in buses.windows(2) {
        assert!(w[0].right() <= w[1].left(), "buses in display order");
    }
    assert!(buses[3].right() <= master.left(), "right before the master");
    // The buses do not wander when tracks scroll: they stay at the master's side.
    assert!((master.left() - buses[3].right()).abs() < 2.0);
    // A bus strip has M and S but no R.
    let label = names::bus_strip("Hall");
    let group = h.get_by_role_and_label(Role::Group, &label);
    // M and S, no R: the strip of a track has the „Record-Arm“ toggle, a bus's has not.
    let record = |label: &str| {
        h.get_all_by_role(Role::Button)
            .filter(|n| {
                n.accesskit_node()
                    .label()
                    .is_some_and(|l| l.contains("Record-Arm"))
            })
            .any(|n| {
                let r = n.rect();
                r.center().x > strip_rect(&h, label).left()
                    && r.center().x < strip_rect(&h, label).right()
            })
    };
    assert!(record("Kanal Lead"));
    assert!(!record(&label));
    let _ = group;
}

/// A project with one effect bus „Bus 1“ fed by Lead (−6 dB post) and nothing else routed.
fn lead_to_a_bus() -> (Project, TrackId, TrackId) {
    let p = demo_project();
    let lead = id_of(&p, "Lead");
    let (p, bus) = edits::add_bus(&p, NewBus::effect(vec![lead])).unwrap();
    (p, lead, bus)
}

/// Runs the held engine for `frames` and returns the highest peak `track` reached meanwhile.
fn peak_while(h: &mut Harness<'_, App>, track: TrackId, frames: usize) -> f32 {
    let meter = Arc::clone(h.state().engine().track_meter(track).expect("meter"));
    let _ = meter.take();
    assert!(h.state_mut().engine_mut().run_held(frames, &[], 0));
    let (l, r) = meter.take();
    l.max(r)
}

#[test]
fn a_send_drag_is_one_undo_step_and_uses_the_fast_path() {
    let (p, lead, bus) = lead_to_a_bus();
    let mut app = mixer_app(p);
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    h.state_mut()
        .dispatch(Action::Transport(komponist_app::TransportAction::Locate(
            komponist_model::Tick(9 * 3840),
        )));
    h.state_mut().dispatch(Action::Transport(
        komponist_app::TransportAction::TogglePlay,
    ));
    h.run_steps(2);
    let loud = peak_while(&mut h, bus, 24_000);
    assert!(loud > 0.005, "the bus hears Lead through the send: {loud}");
    let sent = h.state().engine().programs_sent();
    h.state_mut().clear_dispatched();

    // Drag the send to the left end: silent, no recompile while the pointer is down.
    let label = names::send("Bus 1", -6.0, Tap::Post, true);
    let row = strip_rect(&h, "Kanal Lead");
    let slider = h
        .get_by_role_and_label(Role::Group, "Kanal Lead")
        .get_by_role_and_label(Role::Slider, &label)
        .rect();
    assert!(row.contains_rect(slider));
    let from = slider.center();
    let to = pos2(from.x - 300.0, from.y);
    drag_to(&mut h, from, to);
    assert!(h.state().doc().in_gesture());
    let level = h
        .state()
        .project()
        .track(lead)
        .unwrap()
        .send_to(bus)
        .unwrap()
        .level_db;
    assert!(level < -60.0, "the level followed the pointer: {level}");
    assert_eq!(
        h.state().engine().programs_sent(),
        sent,
        "no recompile during the drag"
    );
    let _ = peak_while(&mut h, bus, 4_800);
    let silent = peak_while(&mut h, bus, 9_600);
    assert!(silent < 1e-4, "the fast path reached the engine: {silent}");

    release(&mut h, to);
    assert!(!h.state().doc().in_gesture());
    assert_eq!(
        h.state().engine().programs_sent(),
        sent + 1,
        "one compile at the end"
    );
    let begins = edits_of(h.state(), |e| matches!(e, EditAction::BeginGesture { .. }));
    assert_eq!(
        begins,
        [&EditAction::BeginGesture {
            label: "undo-send-level".to_owned()
        }]
    );
    assert_eq!(
        edits_of(h.state(), |e| matches!(e, EditAction::EndGesture)).len(),
        1
    );
    assert!(edits_of(h.state(), |e| matches!(e, EditAction::SetSend { .. })).len() > 1);
    assert_eq!(h.state().undo_text().as_deref(), Some("Send-Pegel"));
    // One undo brings the level back.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    let back = h
        .state()
        .project()
        .track(lead)
        .unwrap()
        .send_to(bus)
        .unwrap()
        .level_db;
    assert_eq!(back, -6.0);
}

#[test]
fn the_send_row_sets_the_default_with_alt_click_and_names_pre_and_post() {
    let mut p = routed();
    let (lead, hall) = (id_of(&p, "Lead"), id_of(&p, "Hall"));
    let id = p.track(lead).unwrap().send_to(hall).unwrap().id;
    p = edits::set_send(&p, id, SendProp::LevelDb(-20.0)).unwrap();
    let mut h = harness(mixer_app(p));
    // Pre: „vor Fader“ in the accessible name, as the proposal's names.
    let label = names::send("Hall", -20.0, Tap::Pre, true);
    assert_eq!(label, "Send Hall, \u{2212}20.0 dB, vor Fader");
    let slider = h
        .get_by_role_and_label(Role::Group, "Kanal Lead")
        .get_by_role_and_label(Role::Slider, &label)
        .rect();
    // Alt-click: the default, −6 dB.
    settle(&mut h);
    h.event(Event::PointerMoved(slider.center()));
    h.step();
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    h.event(Event::PointerButton {
        pos: slider.center(),
        button: PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::ALT,
    });
    h.event(Event::PointerButton {
        pos: slider.center(),
        button: PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::ALT,
    });
    h.run_steps(3);
    let level = h
        .state()
        .project()
        .track(lead)
        .unwrap()
        .send_to(hall)
        .unwrap()
        .level_db;
    assert_eq!(level, -6.0);
}

#[test]
fn the_pre_post_switch_of_the_inspector_flips_the_tap() {
    let p = routed();
    let (lead, hall) = (id_of(&p, "Lead"), id_of(&p, "Hall"));
    let mut app = app_with(p);
    select(&mut app, lead);
    let mut h = harness(app);
    let name = names::send_tap("Lead", "Hall", Tap::Pre);
    let chip = h.get_by_role_and_label(Role::Button, &name).rect();
    click(&mut h, chip.center());
    let tap = h
        .state()
        .project()
        .track(lead)
        .unwrap()
        .send_to(hall)
        .unwrap()
        .tap;
    assert_eq!(tap, Tap::Post);
    assert_eq!(h.state().undo_text().as_deref(), Some("Send ändern"));
    // And back.
    let name = names::send_tap("Lead", "Hall", Tap::Post);
    let chip = h.get_by_role_and_label(Role::Button, &name).rect();
    click(&mut h, chip.center());
    assert_eq!(
        h.state()
            .project()
            .track(lead)
            .unwrap()
            .send_to(hall)
            .unwrap()
            .tap,
        Tap::Pre
    );
}

#[test]
fn the_output_list_offers_only_valid_targets() {
    // Rhythmus (Drums, Bass) outputs to Hall: Hall cannot output to Rhythmus (a loop), nor to
    // itself.
    let p = demo_project();
    let (drums, bass) = (id_of(&p, "Drums"), id_of(&p, "Bass"));
    let (p, group) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Rhythmus".into()),
            ..NewBus::group(vec![drums, bass])
        },
    )
    .unwrap();
    let (p, hall) = edits::add_bus(
        &p,
        NewBus {
            name: Some("Hall".into()),
            ..NewBus::effect(vec![drums])
        },
    )
    .unwrap();
    let p = edits::set_output(&p, &[group], TrackOutput::Bus(hall)).unwrap();
    let rows = komponist_app::views::routing::output_targets(&p, hall);
    let blocked: Vec<&str> = rows
        .iter()
        .filter(|r| r.blocked.is_some())
        .map(|r| r.label.as_str())
        .collect();
    assert_eq!(blocked, ["Rhythmus", "Hall"]);
    assert_eq!(rows[0].label, "Master");
    assert!(rows[0].blocked.is_none());

    let mut h = harness(mixer_app(p));
    let button = h
        .get_by_role_and_label(Role::Group, &names::bus_strip("Hall"))
        .get_by_role_and_label(Role::Button, &names::output("Master"))
        .rect();
    click(&mut h, button.center());
    let item = |h: &Harness<'_, App>, label: &str| {
        h.get_by_role_and_label(Role::MenuItemRadio, label)
            .accesskit_node()
            .is_disabled()
    };
    assert!(!item(&h, "Master"));
    assert!(item(&h, &names::blocked("Rhythmus")), "the loop is greyed");
    assert!(item(&h, &names::blocked("Hall")), "so is the bus itself");
    h.get_by_role_and_label(Role::MenuItem, names::NEW_BUS);
    h.get_by_role_and_label(Role::MenuItem, names::NEW_GROUP);
    // Choosing the greyed row does nothing.
    h.state_mut().clear_dispatched();
    let blocked = h
        .get_by_role_and_label(Role::MenuItemRadio, &names::blocked("Rhythmus"))
        .rect();
    click(&mut h, blocked.center());
    assert!(edits_of(h.state(), |e| matches!(e, EditAction::SetOutput { .. })).is_empty());
    assert_eq!(
        h.state().project().track(hall).unwrap().output,
        TrackOutput::Master
    );
}

#[test]
fn choosing_an_output_routes_the_track_into_the_group() {
    let p = routed();
    let (pad, rhythmus) = (id_of(&p, "Pad"), id_of(&p, "Rhythmus"));
    let mut h = harness(mixer_app(p));
    let button = h
        .get_by_role_and_label(Role::Group, "Kanal Pad")
        .get_by_role_and_label(Role::Button, &names::output("Master"))
        .rect();
    click(&mut h, button.center());
    let rhythmus_row = h
        .get_by_role_and_label(Role::MenuItemRadio, "Rhythmus")
        .rect();
    click(&mut h, rhythmus_row.center());
    assert_eq!(
        h.state().project().track(pad).unwrap().output,
        TrackOutput::Bus(rhythmus)
    );
    assert_eq!(routing::group_of(h.state().project(), pad), Some(rhythmus));
}

// ------------------------------------------------------------------------------------------------
// Solo follows the signal path

#[test]
fn solo_of_a_member_keeps_the_group_audible_through_the_gate_table() {
    let p = routed();
    let (drums, pad) = (id_of(&p, "Drums"), id_of(&p, "Pad"));
    let group = id_of(&p, "Rhythmus");
    let mut app = app_with(p);
    let mut h = {
        let mut h = harness(mixer_app_from(&mut app));
        h.run_steps(2);
        h
    };
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![drums],
            prop: komponist_model::edits::TrackProp::Channel(
                komponist_model::edits::ChannelProp::Solo(true),
            ),
        }));
    h.run_steps(3);
    // The engine's table: the group's output is open, Pad's is not.
    let program = h.state().engine().program().expect("a program");
    let index = |t: TrackId| program.tracks.iter().position(|x| x.id == t).unwrap();
    let (group_i, pad_i, drums_i) = (index(group), index(pad), index(drums));
    let Some(komponist_engine::Command::SetGates(table)) =
        h.state().engine().gates(h.state().project())
    else {
        panic!("a gate table");
    };
    assert!(table.output_open(drums_i), "the soloed member");
    assert!(
        table.output_open(group_i),
        "the group it feeds stays audible"
    );
    assert!(!table.output_open(pad_i), "Pad is solo-muted");
    // The mixer says the same: the group strip is not dimmed, Pad's is (stumm durch Solo).
    let strips = komponist_app::views::mixer::model::strips(
        h.state().project(),
        &Default::default(),
        h.state().engine().host(),
    );
    let by = |name: &str| strips.iter().find(|s| s.name == name).unwrap();
    assert!(!by("Rhythmus").dim && !by("Rhythmus").implied);
    assert!(by("Pad").implied);
    assert!(by("Melodie").implied, "a group that is not on the path");
    assert!(
        by("Rhythmus").follows_solo,
        "its S shows that it only follows"
    );
}

fn mixer_app_from(app: &mut App) -> App {
    let project = (**app.project()).clone();
    mixer_app(project)
}

// ------------------------------------------------------------------------------------------------
// The new-track dialog

#[test]
fn the_new_track_dialog_creates_a_bus() {
    let mut app = app_with(demo_project());
    app.dispatch(Action::Ui(UiAction::OpenNewTrackDialog));
    let mut h = harness(app);
    assert!(
        h.query_by_role_and_label(Role::ListBox, "Instrument")
            .is_some(),
        "a MIDI track has an instrument list"
    );
    let bus = h.get_by_role_and_label(Role::RadioButton, "Bus").rect();
    click(&mut h, bus.center());
    assert!(
        h.query_by_role_and_label(Role::ListBox, "Instrument")
            .is_none(),
        "the instrument picker is hidden for a bus"
    );
    mask_cpu(&mut h);
    h.snapshot("routing-new-bus-dialog-1x");
    let create = h.get_by_role_and_label(Role::Button, "Anlegen").rect();
    click(&mut h, create.center());
    let p = h.state().project();
    assert_eq!(p.tracks.len(), 7);
    let bus = p.tracks.last().unwrap();
    assert!(bus.is_bus() && !bus.folder);
    assert_eq!(bus.name, "Bus 1");
    assert!(bus.channel.devices.is_empty());
    assert_eq!(h.state().undo_text().as_deref(), Some("Bus anlegen"));
}

// ------------------------------------------------------------------------------------------------
// Snapshots next to the mockups

#[test]
fn the_dock_strips_show_the_routing_chip_and_its_popover() {
    let p = routed();
    let lead = id_of(&p, "Lead");
    let mut app = app_with(p);
    select(&mut app, lead);
    app.dispatch(Action::Ui(UiAction::SetDockTab(
        komponist_app::ui_state::DockTab::Mixer,
    )));
    let mut h = harness(app);
    h.run_steps(2);
    // Drums outputs to a group: chip with arrow and dot; Pad has sends only: dots; Arp is in a
    // group; the master and buses have no chip unless they output to a bus.
    let chip = |name: &str, output: &str, sends: usize| names::chip(name, output, sends);
    for (n, o, s) in [
        ("Drums", "Rhythmus", 1),
        ("Pad", "Master", 1),
        ("Keys", "Master", 1),
    ] {
        h.get_by_role_and_label(Role::Button, &chip(n, o, s));
    }
    // A track without routing has no chip.
    assert!(h.query_all_by_role(Role::Button).all(|n| {
        n.accesskit_node()
            .label()
            .is_none_or(|l| !l.starts_with("Routing Hall"))
    }));
    mask_cpu(&mut h);
    h.snapshot("routing-dock-1x");
    let lead_chip = h
        .get_by_role_and_label(Role::Button, &chip("Lead", "Melodie", 2))
        .rect();
    click(&mut h, lead_chip.center());
    // The popover has its own output button; the inspector shows another one.
    let outputs = |h: &Harness<'_, App>| {
        h.query_all_by_role_and_label(Role::Button, &names::output("Melodie"))
            .count()
    };
    assert_eq!(outputs(&h), 2, "the popover is open");
    mask_cpu(&mut h);
    h.snapshot("routing-dock-popover-1x");
    // Esc closes it.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(outputs(&h), 1, "closed");
}

#[test]
fn the_inspector_shows_the_routing_section() {
    let p = routed();
    let lead = id_of(&p, "Lead");
    let mut app = app_with(p);
    select(&mut app, lead);
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Group, names::ROUTING);
    h.get_by_role_and_label(Role::Button, &names::output("Melodie"));
    h.get_by_role_and_label(Role::Slider, &names::send("Hall", -6.0, Tap::Pre, true));
    mask_cpu(&mut h);
    h.snapshot("routing-inspector-1x");
}

#[test]
fn the_inspector_of_a_bus_lists_its_inputs() {
    let p = routed();
    let hall = id_of(&p, "Hall");
    let mut app = app_with(p);
    select(&mut app, hall);
    let mut h = harness(app);
    h.get_by_role_and_label(Role::Group, "Bus");
    h.get_by_role_and_label(Role::Label, "Eingänge (3 Sends)");
    h.get_by_role_and_label(Role::Button, "Eingang Lead, \u{2212}6.0 dB pre");
    mask_cpu(&mut h);
    h.snapshot("routing-inspector-bus-1x");
    // A click on an input selects its track.
    let input = h
        .get_by_role_and_label(Role::Button, "Eingang Drums, \u{2212}10.0 dB")
        .rect();
    click(&mut h, input.center());
    let drums = id_of(h.state().project(), "Drums");
    assert!(h.state().ui().selection.tracks.contains(&drums));
}

#[test]
fn the_track_context_menu() {
    let p = routed();
    let lead = id_of(&p, "Lead");
    let mut app = app_with(p);
    select(&mut app, lead);
    let mut h = harness(app);
    let at = header_rect(&h, "Pad").center();
    right_click(&mut h, at);
    for label in [
        names::RENAME,
        names::GROUP_TRACKS,
        names::SEND_TO_NEW_BUS,
        names::SEND_TO,
        names::OUTPUT_TO,
    ] {
        h.get_by_role_and_label(Role::MenuItem, label);
    }
    mask_cpu(&mut h);
    h.snapshot("routing-context-menu-1x");
}

#[test]
fn a_project_without_buses_has_the_send_area_but_no_bus_furniture() {
    // The routing section and the send area are there (owner, 2026-11-15); the label band over
    // the buses and the dock chip need a bus or a route.
    let mut app = app_with(demo_project());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    let h = harness(app);
    h.get_by_role_and_label(Role::Group, names::ROUTING);
    h.get_by_role_and_label(Role::Button, &names::output("Master"));
    plus_of(&h, "Lead");
    let mut app = mixer_app(demo_project());
    select(&mut app, lead);
    let h = harness(app);
    plus_of(&h, "Lead");
    assert!(
        h.query_by_role_and_label(Role::Label, names::BUSSES)
            .is_none()
    );
}

/// The „+“ of `track`'s send list; with the mixer and the inspector open the left one (the
/// mixer's).
fn plus_of(h: &Harness<'_, App>, track: &str) -> Rect {
    h.get_all_by_role_and_label(Role::Button, &names::add_send(track))
        .map(|n| n.rect())
        .min_by(|a, b| a.left().total_cmp(&b.left()))
        .expect("a „+“")
}

/// The first bus without a menu: „+“ offers only „Neuer Effekt-Bus“; it creates the bus and a
/// send at −6 dB post in one undo step.
fn first_bus_through_the_plus(mut h: Harness<'static, App>, lead: TrackId) {
    let plus = plus_of(&h, "Lead");
    click(&mut h, plus.center());
    assert!(
        h.query_all_by_role(Role::MenuItemRadio).next().is_none(),
        "no bus to list"
    );
    let item = h
        .get_by_role_and_label(Role::MenuItem, names::NEW_EFFECT_BUS)
        .rect();
    let before = h.state().project().tracks.len();
    click(&mut h, item.center());
    let p = h.state().project();
    let bus = p.tracks.iter().find(|t| t.is_bus()).expect("a bus");
    assert!(!bus.folder);
    let send = p.track(lead).unwrap().send_to(bus.id).expect("a send");
    assert_eq!(
        (send.level_db, send.tap, send.enabled),
        (-6.0, Tap::Post, true)
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks.len(), before);
    assert!(h.state().project().track(lead).unwrap().sends.is_empty());
}

#[test]
fn the_first_bus_comes_from_the_plus_in_the_mixer() {
    let mut app = mixer_app(demo_project());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    first_bus_through_the_plus(harness(app), lead);
}

#[test]
fn the_first_bus_comes_from_the_plus_in_the_inspector() {
    let mut app = app_with(demo_project());
    let lead = id_of(app.project(), "Lead");
    select(&mut app, lead);
    first_bus_through_the_plus(harness(app), lead);
}

#[test]
fn the_plus_lists_the_buses_and_the_new_bus_below() {
    let p = routed();
    let pad = id_of(&p, "Pad");
    let mut app = app_with(p);
    select(&mut app, pad);
    let mut h = harness(app);
    let plus = h
        .get_by_role_and_label(Role::Button, &names::add_send("Pad"))
        .rect();
    click(&mut h, plus.center());
    h.get_by_role_and_label(Role::MenuItemRadio, "Delay");
    h.get_by_role_and_label(Role::MenuItem, names::NEW_EFFECT_BUS);
}
