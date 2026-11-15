//! The mixer (WP-43) through `egui_kittest`: one test per row of the S04 notes (Tab, Kanalzug
//! Klick, Fader / Pan, Insert-Slot, Peak-Wert / LED, Sends), the fader gesture as one undo step
//! with the fast path while it lasts, the normal-height dock, and snapshots next to S04.

use crate::fake_host;

use std::path::PathBuf;
use std::sync::Arc;

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, Node, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{DropTarget, SelectMode, SelectionChange};
use komponist_app::ui_state::{BrowserSegment, DockTab, FocusArea};
use komponist_app::views::mixer::model::names;
use komponist_app::{Action, App, EditAction, PluginAction, TransportAction, UiAction};
use komponist_model::edits::{self, NewDevice};
use komponist_model::fixtures::demo_project;
use komponist_model::project::VOLUME_DB_MIN;
use komponist_model::{ChannelRef, DeviceId, DeviceRole, Project, Tick, TrackId};
use komponist_plugins::DeviceStatus;

use fake_host::{FakeHost, plugin, screens_catalog};

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

/// S04's project: the demo with "LSP Limiter" on the master, every plugin in the screens'
/// catalog (so the inserts run).
fn s04_project() -> Project {
    let catalog = screens_catalog();
    let p = demo_project();
    let limiter = NewDevice {
        role: DeviceRole::Effect,
        plugin: plugin(&catalog, "comp"),
        state: None,
    };
    let (p, _) = edits::insert_device(&p, ChannelRef::Master, 0, limiter).unwrap();
    p
}

fn track(app: &App, i: usize) -> TrackId {
    app.project().tracks[i].id
}

fn select(app: &mut App, t: TrackId) {
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
        tracks: vec![t],
        mode: SelectMode::Replace,
    })));
}

/// S04: Lead selected, Tab from the arrangement (mixer maximised).
fn s04() -> App {
    let mut app = App::headless(s04_project()).with_device_host(Box::new(FakeHost::new()));
    let lead = track(&app, 2);
    select(&mut app, lead);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    app
}

fn mask_cpu(h: &mut Harness<'_, App>) {
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_app::panels::transport::names::CPU)
        .rect();
    h.mask(cpu.expand(2.0));
}

fn strip<'h>(h: &'h Harness<'_, App>, label: &'h str) -> Node<'h> {
    h.get_by_role_and_label(Role::Group, label)
}

fn press(h: &mut Harness<'_, App>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
}

/// Half a second without input, so the next click is no double-click.
fn settle(h: &mut Harness<'_, App>) {
    h.run_steps(30);
}

fn click_with(h: &mut Harness<'_, App>, pos: Pos2, modifiers: Modifiers) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::ModifiersChanged(modifiers));
    press(h, pos, true, modifiers);
    press(h, pos, false, modifiers);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
}

fn click(h: &mut Harness<'_, App>, pos: Pos2) {
    click_with(h, pos, Modifiers::NONE);
}

fn double_click(h: &mut Harness<'_, App>, pos: Pos2) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        press(h, pos, true, Modifiers::NONE);
        press(h, pos, false, Modifiers::NONE);
        h.step();
    }
    h.run_steps(2);
}

/// A right-click at `pos`, then a click on the menu entry `label`.
fn menu_choice(h: &mut Harness<'_, App>, pos: Pos2, label: &str) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos,
            button: PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(3);
    let item = h.get_by_role_and_label(Role::MenuItem, label).rect();
    click(h, item.center());
    h.run_steps(2);
}

/// `notches` wheel lines over `pos` in one burst (one frame each), with `modifiers`.
fn wheel(h: &mut Harness<'_, App>, pos: Pos2, notches: &[f32], modifiers: Modifiers) {
    h.event(Event::PointerMoved(pos));
    h.step();
    for n in notches {
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0.0, *n),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers,
        });
        h.run_steps(2);
    }
}

/// Press at `from` and move to `to` in eight frames, without releasing.
fn drag_to(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    settle(h);
    h.hover_at(from);
    h.step();
    press(h, from, true, Modifiers::NONE);
    h.step();
    for i in 1..=8 {
        h.hover_at(from + (to - from) * (i as f32 / 8.0));
        h.step();
    }
}

fn release(h: &mut Harness<'_, App>, at: Pos2) {
    press(h, at, false, Modifiers::NONE);
    h.run_steps(3);
}

fn drag(h: &mut Harness<'_, App>, from: Pos2, to: Pos2) {
    drag_to(h, from, to);
    release(h, to);
}

/// The rect of the node `role` / `label` inside strip `strip`.
fn in_strip(h: &Harness<'_, App>, strip: &str, role: Role, label: &str) -> Rect {
    let group = names::strip(strip);
    h.get_by_role_and_label(Role::Group, &group)
        .get_by_role_and_label(role, label)
        .rect()
}

/// Clicks the node `role` / `label` inside strip `strip` with `modifiers`.
fn click_in(h: &mut Harness<'_, App>, strip: &str, role: Role, label: &str, modifiers: Modifiers) {
    let at = in_strip(h, strip, role, label).center();
    click_with(h, at, modifiers);
}

/// The arrangement's header row of track `name` ("Spur Lead, …").
fn header<'h>(h: &'h Harness<'_, App>, name: &str) -> Node<'h> {
    let prefix = format!("Spur {name}");
    h.get_all_by_role(Role::Row)
        .find(|n| {
            n.accesskit_node()
                .label()
                .is_some_and(|l| l == prefix || l.starts_with(&format!("{prefix},")))
        })
        .unwrap_or_else(|| panic!("no header {prefix}"))
}

fn value_in_strip(h: &Harness<'_, App>, strip: &str, role: Role, label: &str) -> String {
    let group = names::strip(strip);
    h.get_by_role_and_label(Role::Group, &group)
        .get_by_role_and_label(role, label)
        .value()
        .unwrap_or_default()
}

fn edits_of<'a>(app: &'a App, f: impl Fn(&EditAction) -> bool + 'a) -> Vec<&'a EditAction> {
    app.dispatched()
        .filter_map(|a| match a {
            Action::Edit(e) if f(e) => Some(e),
            _ => None,
        })
        .collect()
}

/// Runs the held engine for `frames` and returns the highest peak `track` reached meanwhile.
fn peak_while(h: &mut Harness<'_, App>, track: TrackId, frames: usize) -> f32 {
    let meter = Arc::clone(h.state().engine().track_meter(track).expect("meter"));
    let _ = meter.take();
    assert!(h.state_mut().engine_mut().run_held(frames, &[], 0));
    let (l, r) = meter.take();
    l.max(r)
}

// ------------------------------------------------------------------------------------------------
// S04 "Tab": mixer maximised ↔ previous layout; the mixer tab also works in the normal dock.

#[test]
fn tab_maximises_the_mixer_and_the_normal_dock_shows_it_too() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::Focus(FocusArea::Arrangement)));
    let mut h = harness(app);
    assert!(
        h.query_by_role_and_label(Role::Group, &names::strip("Lead"))
            .is_none()
    );
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    assert!(h.state().ui().layout.dock_maximized);
    // Every track and the master; the empty sends stay hidden until busses exist (owner
    // decision 2026-11-14).
    for name in ["Drums", "Bass", "Lead", "Pad", "Keys", "Arp", "Master"] {
        strip(&h, &names::strip(name));
    }
    let lead = names::strip("Lead");
    assert!(
        strip(&h, &lead)
            .query_by_role_and_label(Role::Label, names::SENDS)
            .is_none()
    );
    strip(&h, &lead).get_by_role_and_label(Role::Label, names::INSERTS);
    strip(&h, &lead).get_by_role_and_label(Role::Slider, &names::pan("Lead"));
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_maximized);
    assert!(h.query_by_role_and_label(Role::Group, &lead).is_none());

    // The mixer tab in the normal dock: the lean strip shows name, M S R, fader and values; no
    // sends, inserts or instrument line; a small pan knob stands in the values row (Q8).
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_maximized);
    let s = strip(&h, &lead);
    assert!(
        s.query_by_role_and_label(Role::Label, names::SENDS)
            .is_none()
    );
    s.get_by_role_and_label(Role::Slider, &names::volume("Lead"));
    s.get_by_role_and_label(Role::Slider, &names::pan("Lead"));
    s.get_by_role_and_label(Role::Button, "Mute · Lead");
    s.get_by_role_and_label(Role::Button, "Solo · Lead");
    assert!(
        s.query_by_role_and_label(Role::Button, &names::insert("pisynth"))
            .is_none()
    );
    // The instrument line is gone; the name carries it as its AccessKit description.
    assert!(s.query_by_role_and_label(Role::Label, "pisynth").is_none());
    let name = s.get_by_role_and_label(Role::Button, "Lead");
    assert_eq!(
        name.accesskit_node().description().as_deref(),
        Some("pisynth")
    );
    let fader = s
        .get_by_role_and_label(Role::Slider, &names::volume("Lead"))
        .rect();
    assert!(fader.height() >= komponist_app::views::mixer::metrics::FADER_AREA_MIN - 1.0);
    // The arrangement stays above it.
    header(&h, "Lead");
}

#[test]
fn the_lean_dock_strip_has_a_pan_knob_that_costs_the_fader_no_height() {
    // Owner decision Q8 (2026-11-14): drag, Alt-click and text entry like the maximised strip.
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    let mut h = harness(app);
    assert!(!h.state().ui().layout.dock_maximized);
    let name = names::pan("Bass");
    let knob = in_strip(&h, "Bass", Role::Slider, &name);
    let diameter = komponist_tokens::size::KNOB_SMALL;
    assert_eq!((knob.width(), knob.height()), (diameter, diameter));
    // It sits in the values row, right of the level and at the end of the row; the name row
    // gives the name its full width, and there is no peak readout in the lean strip.
    let toggles = in_strip(&h, "Bass", Role::Button, "Mute · Bass");
    let level = in_strip(&h, "Bass", Role::SpinButton, &names::volume_value("Bass"));
    assert!(knob.top() > toggles.bottom());
    assert!(knob.left() >= level.right(), "{knob:?} {level:?}");
    assert!((knob.center().y - level.center().y).abs() < 1.0);
    let strip_rect = strip(&h, &names::strip("Bass")).rect();
    assert!(
        (strip_rect.right() - komponist_app::views::mixer::metrics::PAD_X - knob.right()).abs()
            < 1.0
    );
    assert!(
        strip(&h, &names::strip("Bass"))
            .query_by_role_and_label(Role::Button, &names::peak("Bass"))
            .is_none()
    );
    // Nothing of the knob stands in the name row any more.
    let name_row = in_strip(&h, "Bass", Role::Button, "Bass");
    assert!(knob.top() > name_row.bottom());
    // The fader keeps the height it had before the knob (215 px of track + thumb at 1x).
    let fader = in_strip(&h, "Bass", Role::Slider, &names::volume("Bass"));
    assert!(fader.height() >= 215.0 - 0.5, "fader {}", fader.height());
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 50.0));
    let pan = h.state().project().tracks[1].channel.pan;
    assert!(pan > 0.3, "{pan}");
    assert_eq!(h.state().doc().undo_label(), Some(names::UNDO_PAN));
    click_with(&mut h, knob.center(), Modifiers::ALT);
    assert_eq!(h.state().project().tracks[1].channel.pan, 0.0);
    menu_choice(&mut h, knob.center(), "Wert eingeben");
    h.get_by_role_and_label(Role::TextInput, &format!("{name} Eingabe"))
        .type_text("L 20");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!((h.state().project().tracks[1].channel.pan + 0.4).abs() < 1e-6);
}

// ------------------------------------------------------------------------------------------------
// S04 "Kanalzug Klick": selects the track, synchronous with the arrangement.

#[test]
fn a_strip_click_selects_the_track_in_sync_with_the_arrangement() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    let mut h = harness(app);
    let bass = track(h.state(), 1);
    let keys = track(h.state(), 4);
    click_in(&mut h, "Bass", Role::Button, "Bass", Modifiers::NONE);
    let sel: Vec<TrackId> = h.state().ui().selection.tracks.iter().copied().collect();
    assert_eq!(sel, [bass]);
    // The header in the arrangement and the strip show it.
    assert_eq!(
        header(&h, "Bass").accesskit_node().is_selected(),
        Some(true)
    );
    assert_eq!(
        strip(&h, &names::strip("Bass"))
            .accesskit_node()
            .is_selected(),
        Some(true)
    );
    // Strg-click on the strip's empty background adds Keys.
    let keys_strip = strip(&h, &names::strip("Keys")).rect();
    // The lean strip of the normal dock: the gap right below the name row.
    let name = in_strip(&h, "Keys", Role::Button, "Keys");
    let below_name = pos2(keys_strip.center().x, name.bottom() + 2.0);
    click_with(&mut h, below_name, Modifiers::COMMAND);
    let sel: Vec<TrackId> = h.state().ui().selection.tracks.iter().copied().collect();
    assert_eq!(sel, [bass, keys]);
    // A selection made in the arrangement shows in the mixer.
    let lead_header = header(&h, "Lead").rect();
    click(
        &mut h,
        pos2(lead_header.left() + 60.0, lead_header.top() + 12.0),
    );
    assert_eq!(
        strip(&h, &names::strip("Lead"))
            .accesskit_node()
            .is_selected(),
        Some(true)
    );
    assert_eq!(
        strip(&h, &names::strip("Bass"))
            .accesskit_node()
            .is_selected(),
        Some(false)
    );
}

// ------------------------------------------------------------------------------------------------
// S04 "Fader / Pan": drag relative, Shift fine, Alt-click default, double-click text entry. A
// drag is one undo step, and the engine gets the level on the fast path while it lasts.

#[test]
fn a_fader_drag_is_one_undo_step_and_sounds_at_once_through_the_fast_path() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    let bass = track(h.state(), 1);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::TogglePlay));
    h.run_steps(2);
    let loud = peak_while(&mut h, bass, 24_000);
    assert!(loud > 0.01, "Bass plays: {loud}");
    let before_db = h.state().project().tracks[1].channel.volume_db;
    let program_track = |app: &App| {
        let p = app.engine().program().unwrap();
        Arc::clone(p.tracks.iter().find(|t| t.id == bass).unwrap())
    };
    let compiled = program_track(h.state());
    h.state_mut().clear_dispatched();

    // Drag the fader to the bottom, still holding it.
    let fader = in_strip(&h, "Bass", Role::Slider, &names::volume("Bass"));
    let from = fader.center();
    let to = pos2(from.x, fader.bottom() + 400.0);
    drag_to(&mut h, from, to);
    assert!(h.state().doc().in_gesture());
    assert_eq!(
        h.state().project().tracks[1].channel.volume_db,
        VOLUME_DB_MIN
    );
    assert!(
        Arc::ptr_eq(&compiled, &program_track(h.state())),
        "no recompile during the drag"
    );
    // The engine got the level on the fast path: Bass is silent at once.
    let _ = peak_while(&mut h, bass, 4_800);
    let silent = peak_while(&mut h, bass, 9_600);
    assert!(silent < 1e-4, "fast path: {silent}");

    release(&mut h, to);
    assert!(!h.state().doc().in_gesture());
    assert!(!Arc::ptr_eq(&compiled, &program_track(h.state())));
    let begins = edits_of(h.state(), |e| matches!(e, EditAction::BeginGesture { .. }));
    assert_eq!(
        begins,
        [&EditAction::BeginGesture {
            label: names::UNDO_VOLUME.to_owned()
        }]
    );
    assert_eq!(
        edits_of(h.state(), |e| matches!(e, EditAction::EndGesture)).len(),
        1
    );
    assert!(edits_of(h.state(), |e| matches!(e, EditAction::SetTrackProp { .. })).len() > 1);
    assert_eq!(h.state().doc().undo_label(), Some(names::UNDO_VOLUME));
    // One undo brings the level back.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks[1].channel.volume_db, before_db);
    h.run_steps(2);
    let back = peak_while(&mut h, bass, 24_000);
    assert!(back > 0.01, "audible again: {back}");
}

#[test]
fn fader_and_pan_take_alt_click_and_text_entry() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    // Alt-click: 0 dB, one undo step.
    let fader = in_strip(&h, "Drums", Role::Slider, &names::volume("Drums"));
    click_with(&mut h, fader.center(), Modifiers::ALT);
    assert_eq!(h.state().project().tracks[0].channel.volume_db, 0.0);
    assert_eq!(h.state().doc().undo_label(), Some("Spur ändern"));
    // Double-click on the level under the fader: type −12.
    let value = in_strip(&h, "Drums", Role::SpinButton, &names::volume_value("Drums"));
    double_click(&mut h, value.center());
    let entry = format!("{} Eingabe", names::volume_value("Drums"));
    h.get_by_role_and_label(Role::TextInput, &entry)
        .type_text("-12");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().project().tracks[0].channel.volume_db, -12.0);
    assert_eq!(
        value_in_strip(&h, "Drums", Role::Slider, &names::volume("Drums")),
        "\u{2212}12.0 dB"
    );

    // Pan: drag up turns right, Alt-click centres, the menu's "Wert eingeben" takes "L 20".
    let knob = in_strip(&h, "Bass", Role::Slider, &names::pan("Bass"));
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 50.0));
    let pan = h.state().project().tracks[1].channel.pan;
    assert!(pan > 0.3, "{pan}");
    assert_eq!(h.state().doc().undo_label(), Some(names::UNDO_PAN));
    assert_eq!(
        value_in_strip(&h, "Bass", Role::Slider, &names::pan("Bass")),
        komponist_model::format::fmt_pan(pan)
    );
    click_with(&mut h, knob.center(), Modifiers::ALT);
    assert_eq!(h.state().project().tracks[1].channel.pan, 0.0);
    menu_choice(&mut h, knob.center(), "Wert eingeben");
    let entry = format!("{} Eingabe", names::pan("Bass"));
    h.get_by_role_and_label(Role::TextInput, &entry)
        .type_text("L 20");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert!((h.state().project().tracks[1].channel.pan + 0.4).abs() < 1e-6);

    // The master fader: SetMasterProp.
    let master = in_strip(&h, "Master", Role::Slider, &names::volume("Master"));
    drag(&mut h, master.center(), master.center() + vec2(0.0, 80.0));
    assert!(h.state().project().master.volume_db < -3.0);
    assert!(!edits_of(h.state(), |e| matches!(e, EditAction::SetMasterProp(_))).is_empty());
}

/// Owner decision 2026-11-15: a double-click on a fader or pan knob resets it (one undo step) and
/// opens no text entry; text entry is the right-click menu or Enter.
#[test]
fn double_click_resets_fader_and_pan_in_one_undo_step() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let before_db = h.state().project().tracks[0].channel.volume_db;
    assert_ne!(before_db, 0.0);
    let fader = in_strip(&h, "Drums", Role::Slider, &names::volume("Drums"));
    double_click(&mut h, fader.center());
    assert_eq!(h.state().project().tracks[0].channel.volume_db, 0.0);
    assert_eq!(h.state().doc().undo_label(), Some("Spur ändern"));
    assert!(
        h.query_by_label(&format!("{} Eingabe", names::volume("Drums")))
            .is_none()
    );
    assert!(edits_of(h.state(), |e| matches!(e, EditAction::BeginGesture { .. })).is_empty());
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks[0].channel.volume_db, before_db);

    let knob = in_strip(&h, "Bass", Role::Slider, &names::pan("Bass"));
    drag(&mut h, knob.center(), knob.center() - vec2(0.0, 50.0));
    let pan = h.state().project().tracks[1].channel.pan;
    assert!(pan > 0.3);
    double_click(&mut h, knob.center());
    assert_eq!(h.state().project().tracks[1].channel.pan, 0.0);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks[1].channel.pan, pan);
}

#[test]
fn fader_menu_and_enter_reset_and_enter_values() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let name = names::volume("Drums");
    let fader = in_strip(&h, "Drums", Role::Slider, &name);
    menu_choice(&mut h, fader.center(), "Auf Standard");
    assert_eq!(h.state().project().tracks[0].channel.volume_db, 0.0);
    assert_eq!(h.state().doc().undo_label(), Some("Spur ändern"));
    menu_choice(&mut h, fader.center(), "Wert eingeben");
    h.get_by_role_and_label(Role::TextInput, &format!("{name} Eingabe"))
        .type_text("-9");
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    assert_eq!(h.state().project().tracks[0].channel.volume_db, -9.0);
    // Enter on the focused fader opens the entry too.
    click(&mut h, fader.center());
    h.key_press(egui::Key::Enter);
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, &format!("{name} Eingabe"));
}

#[test]
fn wheel_over_a_fader_is_one_undo_step_and_needs_the_pointer_on_it() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let before = h.state().project().tracks[0].channel.volume_db;
    let fader = in_strip(&h, "Drums", Role::Slider, &names::volume("Drums"));
    let name_row = h
        .get_by_role_and_label(Role::Group, &names::strip("Drums"))
        .rect()
        .center_top()
        + vec2(0.0, 12.0);
    // Off the fader: nothing.
    wheel(&mut h, name_row, &[1.0, 1.0], Modifiers::NONE);
    h.run_steps(60);
    assert_eq!(h.state().project().tracks[0].channel.volume_db, before);
    // A burst of three notches up: +3 dB, one gesture, one undo step.
    wheel(&mut h, fader.center(), &[1.0, 1.0, 1.0], Modifiers::NONE);
    h.run_steps(60);
    let after = h.state().project().tracks[0].channel.volume_db;
    assert!((after - (before + 3.0)).abs() < 1e-4, "{before} -> {after}");
    assert_eq!(
        edits_of(h.state(), |e| matches!(e, EditAction::BeginGesture { .. })).len(),
        1
    );
    assert_eq!(
        edits_of(h.state(), |e| matches!(e, EditAction::EndGesture)).len(),
        1
    );
    assert_eq!(h.state().doc().undo_label(), Some(names::UNDO_VOLUME));
    // Shift: fine (0.1 dB); a second burst after the pause is a second step.
    wheel(&mut h, fader.center(), &[-1.0], Modifiers::SHIFT);
    h.run_steps(60);
    let fine = h.state().project().tracks[0].channel.volume_db;
    assert!((fine - (after - 0.1)).abs() < 1e-3, "{after} -> {fine}");
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert!((h.state().project().tracks[0].channel.volume_db - after).abs() < 1e-4);
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert_eq!(h.state().project().tracks[0].channel.volume_db, before);
}

// ------------------------------------------------------------------------------------------------
// M S R as in the track headers: press, drag across strips, Alt-click exclusive.

#[test]
fn toggles_switch_alt_is_exclusive_and_a_drag_spreads_across_strips() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    click_in(&mut h, "Bass", Role::Button, "Mute · Bass", Modifiers::NONE);
    assert!(h.state().project().tracks[1].channel.mute);
    click_in(
        &mut h,
        "Drums",
        Role::Button,
        "Solo · Drums",
        Modifiers::NONE,
    );
    click_in(&mut h, "Keys", Role::Button, "Solo · Keys", Modifiers::ALT);
    let solos: Vec<bool> = h
        .state()
        .project()
        .tracks
        .iter()
        .map(|t| t.channel.solo)
        .collect();
    assert_eq!(solos, [false, false, false, false, true, false]);
    // Drag across R from Drums to Pad arms all four.
    let from = in_strip(&h, "Drums", Role::Button, "Record-Arm · Drums").center();
    let to = in_strip(&h, "Pad", Role::Button, "Record-Arm · Pad").center();
    drag(&mut h, from, to);
    let armed: Vec<bool> = h.state().project().tracks.iter().map(|t| t.arm).collect();
    assert_eq!(armed, [true, true, true, true, false, false]);
    assert_eq!(h.state().doc().undo_label(), Some(names::UNDO_TOGGLES));
    // The master has M only.
    let master = names::strip("Master");
    assert!(
        strip(&h, &master)
            .query_by_role_and_label(Role::Button, "Solo · Master")
            .is_none()
    );
    click_in(
        &mut h,
        "Master",
        Role::Button,
        "Mute · Master",
        Modifiers::NONE,
    );
    assert!(h.state().project().master.mute);
}

// ------------------------------------------------------------------------------------------------
// S04 "Insert-Slot": click opens the plugin, drag reorders, Alt-click bypass; "+" opens the
// browser filtered to effects; a browser effect dropped on a strip is inserted.

#[test]
fn insert_slots_open_reorder_and_bypass() {
    let mut h = harness(s04());
    let p = h.state().project().clone();
    let lead = &p.tracks[2];
    let [synth, eq, tal, hall] = [0, 1, 2, 3].map(|i| lead.channel.devices[i].id);
    // Click opens the plugin window.
    click_in(
        &mut h,
        "Lead",
        Role::Button,
        &names::insert("LSP Para EQ x8"),
        Modifiers::NONE,
    );
    assert!(
        h.state()
            .dispatched()
            .any(|a| *a == Action::Plugin(PluginAction::OpenWindow(eq)))
    );
    assert!(h.state().ui().plugin_windows.iter().any(|w| w.device == eq));
    // Alt-click switches bypass (TAL is bypassed in the demo).
    let tal_slot = in_strip(&h, "Lead", Role::Button, &names::insert("TAL-Chorus-LX"));
    assert_eq!(
        value_in_strip(&h, "Lead", Role::Button, &names::insert("TAL-Chorus-LX")),
        names::BYPASS
    );
    click_with(&mut h, tal_slot.center(), Modifiers::ALT);
    assert!(!h.state().project().find_device(tal).unwrap().2.bypass);
    // Drag Dragonfly above the EQ.
    let hall_slot = in_strip(
        &h,
        "Lead",
        Role::Button,
        &names::insert("Dragonfly Hall Reverb"),
    );
    let eq_slot = in_strip(&h, "Lead", Role::Button, &names::insert("LSP Para EQ x8"));
    drag(
        &mut h,
        hall_slot.center(),
        pos2(eq_slot.center().x, eq_slot.top() + 3.0),
    );
    let chain: Vec<DeviceId> = h.state().project().tracks[2]
        .channel
        .devices
        .iter()
        .map(|d| d.id)
        .collect();
    assert_eq!(chain, [synth, hall, eq, tal]);
    // The instrument does not move.
    let synth_slot = in_strip(&h, "Lead", Role::Button, &names::insert("pisynth"));
    let tal_slot = in_strip(&h, "Lead", Role::Button, &names::insert("TAL-Chorus-LX"));
    drag(
        &mut h,
        synth_slot.center(),
        tal_slot.center() + vec2(0.0, 8.0),
    );
    assert_eq!(h.state().project().tracks[2].channel.devices[0].id, synth);
}

#[test]
fn plus_opens_the_browser_on_effects_and_a_dropped_effect_is_inserted() {
    let mut h = harness(s04());
    let bass = track(h.state(), 1);
    click_in(
        &mut h,
        "Bass",
        Role::Button,
        names::ADD_EFFECT,
        Modifiers::NONE,
    );
    let ui = h.state().ui();
    assert_eq!(ui.browser.segment, BrowserSegment::Effects);
    assert_eq!(ui.focus, FocusArea::Browser);
    assert_eq!(
        ui.selection.tracks.iter().copied().collect::<Vec<_>>(),
        [bass]
    );

    // Drag "LSP Delay" from the browser onto the Bass strip: appended after the EQ.
    h.state_mut()
        .dispatch(Action::Ui(UiAction::SetBrowserQuery("delay".into())));
    h.run_steps(2);
    let row = h.get_by_role_and_label(Role::ListItem, "LSP Delay").rect();
    let target = in_strip(&h, "Bass", Role::Button, names::ADD_EFFECT).center();
    drag(&mut h, row.center(), target);
    let delay = plugin(&screens_catalog(), "delay");
    assert!(h.state().dispatched().any(|a| *a
        == Action::Plugin(PluginAction::Add {
            plugin: delay.clone(),
            target: DropTarget::Chain {
                channel: ChannelRef::Track(bass),
                index: 2,
            },
        })));
    let names_now: Vec<String> = h.state().project().tracks[1]
        .channel
        .devices
        .iter()
        .map(|d| d.plugin.name.clone())
        .collect();
    assert_eq!(names_now, ["pisynth", "LSP Para EQ x8", "LSP Delay"]);
}

#[test]
fn the_lean_strip_meter_click_resets_the_peak() {
    let mut app = App::headless(demo_project());
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    let mut h = harness(app);
    let meter = names::meter("Bass");
    click_in(&mut h, "Bass", Role::Meter, &meter, Modifiers::NONE);
    let bass = h.state().project().tracks[1].id;
    assert!(h.state().dispatched().any(|a| *a
        == Action::Ui(UiAction::ResetMeters {
            track: Some(bass),
            all: false
        })));
    click_in(
        &mut h,
        "Master",
        Role::Meter,
        &names::meter("Master"),
        Modifiers::ALT,
    );
    assert!(h.state().dispatched().any(|a| *a
        == Action::Ui(UiAction::ResetMeters {
            track: None,
            all: true
        })));
}

// ------------------------------------------------------------------------------------------------
// S04 "Peak-Wert / LED": click resets peak and clip, Alt-click all strips.

#[test]
fn peak_value_and_led_reset_alt_for_all() {
    let mut p = demo_project();
    // Loud: every track and the master at +6 dB, so the master clips.
    let bass_id = p.tracks[1].id;
    let all: Vec<TrackId> = p.tracks.iter().map(|t| t.id).collect();
    p = edits::set_track_prop(
        &p,
        &all,
        komponist_model::edits::TrackProp::Channel(komponist_model::edits::ChannelProp::VolumeDb(
            6.0,
        )),
    )
    .unwrap();
    p = edits::set_master_prop(&p, komponist_model::edits::ChannelProp::VolumeDb(6.0)).unwrap();
    let mut app = App::headless(p);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    // Bars 9 …: Bass and Lead play.
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(8 * 3840))));
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::TogglePlay));
    h.run_steps(1);
    assert!(h.state_mut().engine_mut().run_held(48_000, &[], 0));
    h.run_steps(1);
    // Stop, so nothing new arrives.
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::TogglePlay));
    h.run_steps(1);
    assert!(h.state_mut().engine_mut().run_held(4_800, &[], 0));
    h.run_steps(1);
    let silent = "\u{2212}\u{221e} dB";
    let bass_peak = value_in_strip(&h, "Bass", Role::Button, &names::peak("Bass"));
    let lead_peak = value_in_strip(&h, "Lead", Role::Button, &names::peak("Lead"));
    assert_ne!(bass_peak, silent);
    assert_ne!(lead_peak, silent);
    let master_meter = value_in_strip(&h, "Master", Role::Meter, &names::meter("Master"));
    assert!(master_meter.ends_with(", Clip"), "{master_meter}");
    // Click on Bass's peak: only Bass.
    click_in(
        &mut h,
        "Bass",
        Role::Button,
        &names::peak("Bass"),
        Modifiers::NONE,
    );
    assert_eq!(
        value_in_strip(&h, "Bass", Role::Button, &names::peak("Bass")),
        silent
    );
    assert_ne!(
        value_in_strip(&h, "Lead", Role::Button, &names::peak("Lead")),
        silent
    );
    // Click on the master's meter (LED): the clip goes.
    click_in(
        &mut h,
        "Master",
        Role::Meter,
        &names::meter("Master"),
        Modifiers::NONE,
    );
    let master_meter = value_in_strip(&h, "Master", Role::Meter, &names::meter("Master"));
    assert!(!master_meter.contains("Clip"), "{master_meter}");
    // Alt-click on a peak: every strip.
    click_in(
        &mut h,
        "Bass",
        Role::Button,
        &names::peak("Bass"),
        Modifiers::ALT,
    );
    assert_eq!(
        value_in_strip(&h, "Lead", Role::Button, &names::peak("Lead")),
        silent
    );
    assert!(edits_of(h.state(), |_| false).is_empty());
    assert!(h.state().dispatched().any(|a| *a
        == Action::Ui(UiAction::ResetMeters {
            track: Some(bass_id),
            all: true
        })));
}

// ------------------------------------------------------------------------------------------------
// S04 "Sends": hidden until busses exist (owner decision 2026-11-14, ARCHITECTURE §13.7).

#[test]
fn sends_stay_hidden_until_busses_exist() {
    let h = harness(s04());
    let lead = names::strip("Lead");
    let s = strip(&h, &lead);
    assert!(
        s.query_by_role_and_label(Role::Label, names::SENDS)
            .is_none()
    );
}

// ------------------------------------------------------------------------------------------------
// Many tracks: the strips scroll sideways, the master stays at the right edge.

#[test]
fn many_strips_scroll_sideways_and_the_master_stays() {
    let mut p = demo_project();
    for i in 0..14 {
        let spec = komponist_model::edits::NewTrack {
            name: Some(format!("Spur {}", i + 7)),
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        };
        p = edits::add_track(&p, spec).unwrap().0;
    }
    let mut app = App::headless(p);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let master = strip(&h, &names::strip("Master")).rect();
    let inspector_left = W - h.state().ui().layout.inspector_width;
    assert!((master.right() - inspector_left).abs() <= 2.0, "{master:?}");
    // Only the visible strips are there.
    assert!(
        h.query_by_role_and_label(Role::Group, &names::strip("Spur 20"))
            .is_none()
    );
    // Over the name row: the fader and the pan knob take the wheel themselves.
    let drums = strip(&h, &names::strip("Drums")).rect();
    h.hover_at(drums.center_top() + vec2(0.0, 12.0));
    h.step();
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -2000.0),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(30);
    assert!(h.state().ui().mixer.scroll_x > 0.0);
    assert!(
        h.query_by_role_and_label(Role::Group, &names::strip("Drums"))
            .is_none()
    );
    strip(&h, &names::strip("Spur 20"));
    // The master did not move.
    assert_eq!(strip(&h, &names::strip("Master")).rect(), master);
}

#[test]
fn a_track_selected_elsewhere_is_scrolled_into_view() {
    let mut p = demo_project();
    for i in 0..14 {
        let spec = komponist_model::edits::NewTrack {
            name: Some(format!("Spur {}", i + 7)),
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        };
        p = edits::add_track(&p, spec).unwrap().0;
    }
    let last = p.tracks.last().unwrap().id;
    let mut app = App::headless(p);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    assert_eq!(h.state().ui().mixer.scroll_x, 0.0);
    // Selected in the arrangement or inspector, not in the mixer.
    select(h.state_mut(), last);
    h.run_steps(3);
    assert!(h.state().ui().mixer.scroll_x > 0.0);
    strip(&h, &names::strip("Spur 20"));
    // Selecting a visible track back scrolls only as far as needed.
    let first = h.state().project().tracks[0].id;
    select(h.state_mut(), first);
    h.run_steps(3);
    assert_eq!(h.state().ui().mixer.scroll_x, 0.0);
}

#[test]
fn tab_always_leaves_the_maximised_mixer_even_over_a_focus_ring() {
    // Owner decision Q9 (2026-11-14): one key back to the arrangement, whatever has the focus.
    let mut h = harness(s04());
    let pan = names::pan("Lead");
    click_in(&mut h, "Lead", Role::Slider, &pan, Modifiers::NONE);
    h.run_steps(2);
    komponist_ui::focus::set_visible(&h.ctx, true);
    assert!(h.ctx.memory(|m| m.focused().is_some()));
    h.key_press(egui::Key::Tab);
    h.run_steps(2);
    assert!(!h.state().ui().layout.dock_maximized, "Tab left the mixer");
}

#[test]
fn shift_tab_still_moves_the_widget_focus_in_the_maximised_mixer() {
    let mut h = harness(s04());
    let pan = names::pan("Lead");
    click_in(&mut h, "Lead", Role::Slider, &pan, Modifiers::NONE);
    h.run_steps(2);
    komponist_ui::focus::set_visible(&h.ctx, true);
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::Tab);
    h.run_steps(2);
    assert!(
        h.state().ui().layout.dock_maximized,
        "Shift Tab moves the focus"
    );
}

#[test]
fn scroll_to_strip_is_the_least_change() {
    use komponist_app::views::mixer::model::scroll_to_strip;
    // Strips 81 apart, 80 wide, an area of 400.
    assert_eq!(scroll_to_strip(1, 81.0, 80.0, 400.0, 0.0), 0.0);
    assert_eq!(
        scroll_to_strip(10, 81.0, 80.0, 400.0, 0.0),
        810.0 + 80.0 - 400.0
    );
    assert_eq!(scroll_to_strip(2, 81.0, 80.0, 400.0, 500.0), 162.0);
    assert_eq!(scroll_to_strip(7, 81.0, 80.0, 400.0, 300.0), 300.0);
}

// ------------------------------------------------------------------------------------------------
// Insert status: loading, crashed, missing (accessible text and tooltip detail).

#[test]
fn insert_slots_show_the_device_status() {
    let p = s04_project();
    let pad = &p.tracks[3];
    let (surge, eq) = (pad.channel.devices[0].id, pad.channel.devices[1].id);
    let host = FakeHost::new().status(surge, DeviceStatus::Loading).status(
        eq,
        DeviceStatus::Crashed(komponist_plugins::FailReason::Exited {
            code: None,
            signal: Some(11),
        }),
    );
    let mut app = App::headless(p).with_device_host(Box::new(host));
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let h = harness(app);
    let group = names::strip("Pad");
    let s = strip(&h, &group);
    let (surge_label, eq_label) = (names::insert("Surge XT"), names::insert("LSP Para EQ x8"));
    let surge_slot = s.get_by_role_and_label(Role::Button, &surge_label);
    let eq_slot = s.get_by_role_and_label(Role::Button, &eq_label);
    assert!(
        surge_slot
            .accesskit_node()
            .description()
            .unwrap_or_default()
            .contains(names::LOADING)
    );
    assert!(
        eq_slot
            .accesskit_node()
            .description()
            .unwrap_or_default()
            .contains(names::CRASHED)
    );
}

/// S04 with sound: the demo plays from 10.1.1 (Bass and Lead are pisynth and sound; the other
/// instruments are fakes), so the meters and peak values show.
fn s04_playing(ppp: f32) -> Harness<'static, App> {
    let mut app = s04();
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness_at(app, ppp);
    h.run_steps(2);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(9 * 3840))));
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::TogglePlay));
    h.run_steps(1);
    for _ in 0..30 {
        assert!(h.state_mut().engine_mut().run_held(800, &[], 0));
        h.step();
    }
    h
}

#[test]
fn snapshots_next_to_s04() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let mut h = s04_playing(ppp);
        mask_cpu(&mut h);
        h.snapshot(format!("mixer-s04-{ppp}x"));
        results.extend_harness(&mut h);
    }
    // The mixer tab in the normal dock.
    let mut app = App::headless(s04_project()).with_device_host(Box::new(FakeHost::new()));
    let lead = track(&app, 2);
    select(&mut app, lead);
    app.dispatch(Action::Ui(UiAction::SetDockTab(DockTab::Mixer)));
    let mut h = harness_at(app, 1.0);
    h.run_steps(4);
    mask_cpu(&mut h);
    h.snapshot("mixer-dock-1x");
    results.extend_harness(&mut h);
    results.unwrap();
}

#[test]
fn an_insert_whose_blocks_arrive_late_reads_zu_spaet_for_a_while() {
    let p = s04_project();
    let surge = p.tracks[3].channel.devices[0].id;
    let host = FakeHost::new();
    let late = host.late.clone();
    let mut app = App::headless(p).with_device_host(Box::new(host));
    let lead = track(&app, 2);
    select(&mut app, lead);
    app.dispatch(Action::Ui(UiAction::ToggleMixer));
    let mut h = harness(app);
    let slot = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Button, &names::insert("Surge XT"))
            .accesskit_node()
            .value()
    };
    assert_eq!(slot(&h), None);
    late.lock().unwrap().insert(surge, 3);
    h.run_steps(2);
    assert_eq!(slot(&h).as_deref(), Some("zu spät"));
    h.run_steps(150);
    assert_eq!(slot(&h), None, "the indicator goes out after its hold time");
}
