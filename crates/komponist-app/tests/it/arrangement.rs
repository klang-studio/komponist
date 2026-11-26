//! The arrangement (WP-40) through `egui_kittest` in the whole app: one test per row of the
//! design system's `ix.header` and `ix.clip` (and the ruler, zoom, S10 and drop rows), the
//! snapshot of the demo project next to S01, and the frame time with 200 clips / 10 000 notes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::device_host::{BuiltinHost, DeviceHost};
use komponist_app::ui_state::FocusArea;
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_engine::Processor;
use komponist_i18n::{Lang, de};
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipId, Device, DeviceId, DeviceRole, PluginRef, Project, Tick, TrackId};

use crate::common::song_with_shipped_instruments;
use komponist_plugins::{DeviceStatus, PluginManagerEvent, ProcessorSetup};

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// The reference screens show external instruments as installed, even without those bundles.
struct AllInstalled(BuiltinHost);

impl DeviceHost for AllInstalled {
    fn create_processor(
        &mut self,
        device: &Device,
        setup: ProcessorSetup,
    ) -> Option<Box<dyn Processor>> {
        self.0.create_processor(device, setup)
    }
    fn release(&mut self, device: DeviceId) {
        self.0.release(device);
    }
    fn status(&self, _device: DeviceId) -> DeviceStatus {
        DeviceStatus::Ok
    }
    fn latency(&self, device: DeviceId) -> u32 {
        self.0.latency(device)
    }
    fn poll(&mut self) -> Vec<PluginManagerEvent> {
        self.0.poll()
    }
    fn role(&self, plugin: &komponist_model::PluginRef) -> Option<DeviceRole> {
        self.0.role(plugin)
    }
    fn catalog(&self) -> &komponist_plugins::Catalog {
        self.0.catalog()
    }
    fn missing_plugins(&self, _project: &Project) -> Vec<komponist_plugins::MissingPlugin> {
        Vec::new()
    }
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
    h.run_steps(2);
    h
}

fn hexa(app: &App) -> PluginRef {
    crate::common::shipped_plugin(app.engine().host(), "org.klang-studio.komponist.hexa")
        .expect("build the Hexa fixture with scripts/build-test-plugins.sh")
}

fn reference_demo() -> App {
    crate::common::reference_app(demo_project())
}

fn demo() -> App {
    App::headless(song_with_shipped_instruments(&App::headless_plugin_host()))
}

fn project(h: &Harness<'_, App>) -> Project {
    Project::clone(h.state().project())
}

fn track_id(h: &Harness<'_, App>, i: usize) -> TrackId {
    h.state().project().tracks[i].id
}

fn clip_id(h: &Harness<'_, App>, track: usize, clip: usize) -> ClipId {
    h.state().project().tracks[track].clips[clip].id
}

/// The rect of the node with `role` and `label`.
/// The rect of the node with `role` and `label`. The piano roll in the dock has its own ruler
/// ("Lineal") and grid dropdown ("Raster"): of several matches, the arrangement's is the topmost.
fn rect_of(h: &Harness<'_, App>, role: Role, label: &str) -> Rect {
    h.get_all_by_role_and_label(role, label)
        .map(|n| n.rect())
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("node")
}

/// The value of the arrangement's node with `role` and `label` (topmost of several).
fn value_of(h: &Harness<'_, App>, role: Role, label: &str) -> Option<String> {
    h.get_all_by_role_and_label(role, label)
        .min_by(|a, b| a.rect().top().total_cmp(&b.rect().top()))
        .and_then(|n| n.value())
}

fn clip_rect(h: &Harness<'_, App>, label: &str) -> Rect {
    rect_of(h, Role::GridCell, label)
}

fn header_rect(h: &Harness<'_, App>, label: &str) -> Rect {
    rect_of(h, Role::Row, label)
}

fn press(pos: Pos2, pressed: bool, modifiers: Modifiers, h: &mut Harness<'_, App>) {
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

fn click_with(pos: Pos2, modifiers: Modifiers, h: &mut Harness<'_, App>) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    h.event(Event::ModifiersChanged(modifiers));
    press(pos, true, modifiers, h);
    press(pos, false, modifiers, h);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
}

fn click(pos: Pos2, h: &mut Harness<'_, App>) {
    click_with(pos, Modifiers::NONE, h);
}

fn double_click(pos: Pos2, h: &mut Harness<'_, App>) {
    settle(h);
    h.event(Event::PointerMoved(pos));
    h.step();
    for _ in 0..2 {
        press(pos, true, Modifiers::NONE, h);
        press(pos, false, Modifiers::NONE, h);
        h.step();
    }
    h.run_steps(2);
}

/// Press at `from`, move in steps to `to` with `modifiers` held, release.
fn drag_with(from: Pos2, to: Pos2, modifiers: Modifiers, h: &mut Harness<'_, App>) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(from));
    h.step();
    press(from, true, modifiers, h);
    h.step();
    let steps = 8;
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        h.event(Event::PointerMoved(from + (to - from) * t));
        h.step();
    }
    press(to, false, modifiers, h);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
}

fn drag(from: Pos2, to: Pos2, h: &mut Harness<'_, App>) {
    drag_with(from, to, Modifiers::NONE, h);
}

/// Screen x of `tick` in the arrangement (from the ruler: bar 1 starts at its left edge).
fn x_of(h: &Harness<'_, App>, tick: Tick) -> f32 {
    let ruler = rect_of(h, Role::Slider, de::A11Y_RULER);
    let v = h.state().ui().arrangement;
    let ppt = v.px_per_bar / 3840.0;
    ruler.left() + ((tick.0 as f64 - v.scroll_tick) as f32) * ppt
}

const BAR: i64 = 3840;

fn undo(h: &mut Harness<'_, App>) {
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.run_steps(2);
}

// ------------------------------------------------------------------------------------------------
// Snapshot

/// The S01 state: Lead A open in the piano roll, playhead at 10.2.120, every device installed.
fn s01() -> App {
    let mut app = reference_demo().with_device_host(Box::new(AllInstalled(BuiltinHost::new())));
    let lead = app.project().tracks[2].clone();
    app.dispatch(Action::Ui(UiAction::OpenClip(lead.clips[0].id)));
    app.dispatch(Action::Transport(TransportAction::Locate(Tick(36_000))));
    app
}

fn step_until(h: &mut Harness<'_, App>, mut done: impl FnMut(&App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        h.step();
        if done(h.state()) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

#[test]
fn snapshot_of_the_demo_next_to_s01() {
    let mut results = SnapshotResults::new();
    for ppp in [1.0, 2.0] {
        let mut h = harness_at(s01(), ppp);
        assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(36_000)));
        h.run_steps(2);
        // The CPU readout depends on the machine.
        let cpu = h
            .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
            .rect();
        h.mask(cpu.expand(2.0));
        h.snapshot(format!("arrangement-s01-{ppp}x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

/// The same screen in English (#5): the UI texts are English, the demo's names are content.
#[test]
fn snapshot_of_the_demo_next_to_s01_in_english() {
    let mut h = harness_at(s01().with_lang(komponist_i18n::Lang::En), 1.0);
    assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(36_000)));
    h.run_steps(2);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::en::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    h.snapshot("arrangement-s01-en-1x");
}

// ------------------------------------------------------------------------------------------------
// ix.header

/// A point on the header's text, away from the toggles and the meter.
fn on_header(r: Rect) -> Pos2 {
    pos2(r.left() + 40.0, r.center().y)
}

fn lead_header(h: &Harness<'_, App>) -> Rect {
    header_rect(h, "Spur Lead, Hexa")
}

#[test]
fn header_click_selects_the_track_for_the_inspector() {
    let mut h = harness(demo());
    let lead = track_id(&h, 2);
    click(on_header(lead_header(&h)), &mut h);
    let sel = &h.state().ui().selection;
    assert_eq!(sel.tracks.iter().copied().collect::<Vec<_>>(), vec![lead]);
    assert!(sel.clips.is_empty());
    assert_eq!(h.state().ui().focus, FocusArea::Arrangement);
    let node = h.get_by_role_and_label(Role::Row, "Spur Lead, Hexa");
    assert_eq!(node.accesskit_node().is_selected(), Some(true));
}

#[test]
fn shift_click_selects_a_range_and_ctrl_click_adds_one() {
    let mut h = harness(demo());
    let ids: Vec<TrackId> = (0..6).map(|i| track_id(&h, i)).collect();
    click(on_header(header_rect(&h, "Spur Bass, Hexa")), &mut h);
    click_with(
        on_header(header_rect(&h, "Spur Pad, Plugin fehlt: Surge XT")),
        Modifiers::SHIFT,
        &mut h,
    );
    let sel: Vec<TrackId> = h.state().ui().selection.tracks.iter().copied().collect();
    assert_eq!(sel, ids[1..=3].to_vec());
    click_with(
        on_header(header_rect(&h, "Spur Arp, Plugin fehlt: Vital")),
        Modifiers::COMMAND,
        &mut h,
    );
    assert!(h.state().ui().selection.tracks.contains(&ids[5]));
    assert_eq!(h.state().ui().selection.tracks.len(), 4);
    // Strg-click again takes it out.
    click_with(
        on_header(header_rect(&h, "Spur Arp, Plugin fehlt: Vital")),
        Modifiers::COMMAND,
        &mut h,
    );
    assert_eq!(h.state().ui().selection.tracks.len(), 3);
}

#[test]
fn double_click_on_the_name_renames_enter_applies_esc_discards() {
    let mut h = harness(demo());
    double_click(on_header(lead_header(&h)), &mut h);
    h.get_by_role_and_label(Role::TextInput, de::ARRANGEMENT_RENAME);
    h.event(Event::Text("Melodie".into()));
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().project().tracks[2].name, "Melodie");
    assert!(
        h.query_by_role_and_label(Role::TextInput, de::ARRANGEMENT_RENAME)
            .is_none()
    );
    // One undo step.
    undo(&mut h);
    assert_eq!(h.state().project().tracks[2].name, "Lead");
    // Esc discards.
    double_click(on_header(lead_header(&h)), &mut h);
    h.event(Event::Text("Weg".into()));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().project().tracks[2].name, "Lead");
}

/// Types `text` into the open name field (its text is selected) and presses `key`.
fn type_and_end(h: &mut Harness<'_, App>, text: &str, key: egui::Key) {
    h.get_by_role_and_label(Role::TextInput, de::ARRANGEMENT_RENAME);
    if text.is_empty() {
        h.key_press(egui::Key::Backspace);
    } else {
        h.event(Event::Text(text.into()));
    }
    h.run_steps(1);
    h.key_press(key);
    h.run_steps(3);
}

fn name_field_is_open(h: &Harness<'_, App>) -> bool {
    h.query_by_role_and_label(Role::TextInput, de::ARRANGEMENT_RENAME)
        .is_some()
}

fn right_click_menu(pos: Pos2, label: &str, h: &mut Harness<'_, App>) {
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
    click(item.center(), h);
    h.run_steps(2);
}

/// Every way to end a rename of track 2 ("Lead"), whatever opened the field: Enter applies as one
/// undo step, Esc discards, an empty name keeps the old one. `open` opens the field.
fn rename_ends_by_the_rules(h: &mut Harness<'_, App>, open: impl Fn(&mut Harness<'_, App>)) {
    open(h);
    assert!(name_field_is_open(h));
    type_and_end(h, "Melodie", egui::Key::Enter);
    assert_eq!(h.state().project().tracks[2].name, "Melodie");
    assert!(!name_field_is_open(h));
    undo(h);
    assert_eq!(h.state().project().tracks[2].name, "Lead", "one undo step");
    open(h);
    type_and_end(h, "Weg", egui::Key::Escape);
    assert_eq!(h.state().project().tracks[2].name, "Lead", "Esc discards");
    assert!(!name_field_is_open(h));
    open(h);
    type_and_end(h, "", egui::Key::Enter);
    assert_eq!(h.state().project().tracks[2].name, "Lead", "empty keeps it");
    assert!(!name_field_is_open(h));
    open(h);
    type_and_end(h, "  ", egui::Key::Enter);
    assert_eq!(h.state().project().tracks[2].name, "Lead", "blank keeps it");
}

#[test]
fn the_header_context_menu_renames_a_normal_track() {
    let mut h = harness(demo());
    rename_ends_by_the_rules(&mut h, |h| {
        let at = on_header(lead_header(h));
        right_click_menu(at, de::ROUTING_RENAME, h);
    });
}

#[test]
fn double_click_renames_by_the_same_rules() {
    let mut h = harness(demo());
    rename_ends_by_the_rules(&mut h, |h| {
        let at = on_header(lead_header(h));
        double_click(at, h);
    });
}

#[test]
fn f2_renames_the_selected_track_by_the_same_rules() {
    let mut h = harness(demo());
    let lead = track_id(&h, 2);
    click(on_header(lead_header(&h)), &mut h);
    assert!(h.state().ui().selection.tracks.contains(&lead));
    rename_ends_by_the_rules(&mut h, |h| {
        h.key_press(egui::Key::F2);
        h.run_steps(3);
    });
}

#[test]
fn f2_with_several_tracks_selected_renames_the_first() {
    let mut h = harness(demo());
    let drums = track_id(&h, 0);
    let lead = track_id(&h, 2);
    // Selected in the opposite order: "first" is the first in track order.
    for id in [lead, drums] {
        h.state_mut()
            .dispatch(Action::Ui(UiAction::Select(SelectionChange::Tracks {
                tracks: vec![id],
                mode: SelectMode::Toggle,
            })));
    }
    h.run_steps(2);
    h.key_press(egui::Key::F2);
    h.run_steps(3);
    type_and_end(&mut h, "Schlagzeug", egui::Key::Enter);
    let names: Vec<_> = project(&h).tracks.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names[0], "Schlagzeug");
    assert_eq!(names[2], "Lead");
}

#[test]
fn f2_without_a_selection_opens_no_field() {
    let mut h = harness(demo());
    h.state_mut().dispatch(Action::Ui(UiAction::ClearSelection));
    h.key_press(egui::Key::F2);
    h.run_steps(3);
    assert!(!name_field_is_open(&h));
}

#[test]
fn dragging_a_header_reorders_tracks() {
    let mut h = harness(demo());
    let ids: Vec<TrackId> = (0..6).map(|i| track_id(&h, i)).collect();
    let drums = header_rect(&h, "Spur Drums, Klopfer");
    // Lead above Drums: the insert line at the top of the first row.
    drag(
        on_header(lead_header(&h)),
        pos2(drums.left() + 40.0, drums.top() + 5.0),
        &mut h,
    );
    let order: Vec<TrackId> = h.state().project().tracks.iter().map(|t| t.id).collect();
    assert_eq!(order, vec![ids[2], ids[0], ids[1], ids[3], ids[4], ids[5]]);
    // The moved track is selected; one undo step.
    assert!(h.state().ui().selection.tracks.contains(&ids[2]));
    undo(&mut h);
    let order: Vec<TrackId> = h.state().project().tracks.iter().map(|t| t.id).collect();
    assert_eq!(order, ids);
}

#[test]
fn bottom_edge_sets_the_height_alt_for_all_tracks() {
    let mut h = harness(demo());
    let grip = rect_of(
        &h,
        Role::Splitter,
        &Lang::De.arrangement_height_grip("Drums"),
    );
    drag(grip.center(), grip.center() + vec2(0.0, 40.0), &mut h);
    assert_eq!(h.state().project().tracks[0].height, 96);
    assert_eq!(h.state().project().tracks[1].height, 56);
    // Clamped to 160.
    let grip = rect_of(
        &h,
        Role::Splitter,
        &Lang::De.arrangement_height_grip("Drums"),
    );
    drag(grip.center(), grip.center() + vec2(0.0, 300.0), &mut h);
    assert_eq!(h.state().project().tracks[0].height, 160);
    // One undo step per drag.
    undo(&mut h);
    assert_eq!(h.state().project().tracks[0].height, 96);
    // Alt: every track.
    let grip = rect_of(
        &h,
        Role::Splitter,
        &Lang::De.arrangement_height_grip("Bass"),
    );
    drag_with(
        grip.center(),
        grip.center() - vec2(0.0, 30.0),
        Modifiers::ALT,
        &mut h,
    );
    for t in &h.state().project().tracks {
        assert_eq!(t.height, 32, "{}", t.name);
    }
}

#[test]
fn ctrl_t_adds_a_midi_track_and_delete_removes_selected_tracks() {
    let mut h = harness(demo());
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::T);
    h.run_steps(2);
    let p = project(&h);
    assert_eq!(p.tracks.len(), 7);
    let new = p.tracks[6].id;
    assert!(p.tracks[6].arm, "armed exclusively");
    assert!(h.state().ui().selection.tracks.contains(&new));
    // Its header is there.
    h.get_by_role_and_label(Role::Row, &format!("Spur {}, Hexa", p.tracks[6].name));
    // Entf deletes the selected track.
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(h.state().project().tracks.len(), 6);
}

#[test]
fn an_instrument_dropped_on_the_empty_area_makes_an_armed_track() {
    let mut h = harness(demo());
    let below = lead_header(&h).left_bottom() + vec2(300.0, 300.0);
    drop_plugin(hexa(h.state()), below, &mut h);
    let p = project(&h);
    assert_eq!(p.tracks.len(), 7);
    assert!(p.tracks[6].arm);
    assert!(h.state().ui().selection.tracks.contains(&p.tracks[6].id));
    let dispatched: Vec<&Action> = h.state().dispatched().collect();
    assert!(dispatched.iter().any(|a| matches!(
        a,
        Action::Plugin(komponist_app::PluginAction::Add {
            target: komponist_app::actions::DropTarget::NewTrack { index: None },
            ..
        })
    )));
}

#[test]
fn an_instrument_dropped_on_a_header_replaces_the_instrument() {
    let mut h = harness(demo());
    let keys = header_rect(&h, "Spur Keys, Plugin fehlt: Dexed");
    let keys_id = track_id(&h, 4);
    drop_plugin(hexa(h.state()), on_header(keys), &mut h);
    let p = project(&h);
    assert_eq!(p.tracks.len(), 6);
    let t = p.track(keys_id).unwrap();
    assert_eq!(t.channel.devices[0].plugin.name, "Hexa");
    h.get_by_role_and_label(Role::Row, "Spur Keys, Hexa");
}

/// A browser drag (WP-44): the payload is a `PluginRef` in egui's drag and drop, released at
/// `at`.
fn drop_plugin(plugin: komponist_model::PluginRef, at: Pos2, h: &mut Harness<'_, App>) {
    // The payload is supplied below. Start on the edge of the window, outside browser rows.
    let from = pos2(1.0, 1.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(from, true, Modifiers::NONE, h);
    h.step();
    egui::DragAndDrop::set_payload(&h.ctx, plugin);
    for i in 1..=4 {
        h.event(Event::PointerMoved(from + (at - from) * (i as f32 / 4.0)));
        h.step();
    }
    press(at, false, Modifiers::NONE, h);
    h.run_steps(2);
}

// ------------------------------------------------------------------------------------------------
// ix.toggle (M S R I in the headers)

#[test]
fn toggles_switch_alt_is_exclusive_and_solo_mutes_the_others() {
    let mut h = harness(demo());
    click(rect_of(&h, Role::Button, "Solo · Drums").center(), &mut h);
    assert!(h.state().project().tracks[0].channel.solo);
    // The others show "stumm durch Solo".
    h.get_by_role_and_label(Role::Row, "Spur Bass, stumm durch Solo");
    // Alt-click on Bass: only Bass.
    click_with(
        rect_of(&h, Role::Button, "Solo · Bass").center(),
        Modifiers::ALT,
        &mut h,
    );
    let solos: Vec<bool> = h
        .state()
        .project()
        .tracks
        .iter()
        .map(|t| t.channel.solo)
        .collect();
    assert_eq!(solos, vec![false, true, false, false, false, false]);
    // R arms (and turns monitoring on); Alt-R arms only this track.
    click(
        rect_of(&h, Role::Button, "Record-Arm · Lead").center(),
        &mut h,
    );
    click_with(
        rect_of(&h, Role::Button, "Record-Arm · Pad").center(),
        Modifiers::ALT,
        &mut h,
    );
    let armed: Vec<bool> = h.state().project().tracks.iter().map(|t| t.arm).collect();
    assert_eq!(armed, vec![false, false, false, true, false, false]);
    assert!(h.state().project().tracks[3].monitor);
    // I toggles monitoring.
    click(
        rect_of(&h, Role::Button, "Monitoring · Pad").center(),
        &mut h,
    );
    assert!(!h.state().project().tracks[3].monitor);
    // Armed with I off, the track still monitors (auto-monitoring): I shows the implied look and
    // says why in its value; a track that is not armed says nothing.
    h.run();
    let pad = h.get_by_role_and_label(Role::Button, "Monitoring · Pad");
    assert_eq!(
        pad.value().as_deref(),
        Some("überwacht durch Record-Arm"),
        "armed, I off"
    );
    assert!(
        h.get_by_role_and_label(Role::Button, "Monitoring · Lead")
            .value()
            .is_none(),
        "not armed"
    );
}

#[test]
fn dragging_across_toggles_sweeps_the_state_in_one_undo_step() {
    let mut h = harness(demo());
    let from = rect_of(&h, Role::Button, "Mute · Drums").center();
    let to = rect_of(&h, Role::Button, "Mute · Lead").center();
    drag(from, to, &mut h);
    let muted: Vec<bool> = h
        .state()
        .project()
        .tracks
        .iter()
        .map(|t| t.channel.mute)
        .collect();
    assert_eq!(muted, vec![true, true, true, false, false, true]);
    undo(&mut h);
    let muted: Vec<bool> = h
        .state()
        .project()
        .tracks
        .iter()
        .map(|t| t.channel.mute)
        .collect();
    assert_eq!(muted, vec![false, false, false, false, false, true]);
}

// ------------------------------------------------------------------------------------------------
// ix.clip

const LEAD_A: &str = "Clip Lead A, Takt 9\u{2013}10";
const KEYS_1: &str = "Clip Keys, Takt 5\u{2013}8";

#[test]
fn click_selects_a_clip_and_its_track() {
    let mut h = harness(demo());
    let lead_a = clip_id(&h, 2, 0);
    click(clip_rect(&h, LEAD_A).center(), &mut h);
    let sel = &h.state().ui().selection;
    assert_eq!(sel.clips.iter().copied().collect::<Vec<_>>(), vec![lead_a]);
    // Selection sync: the inspector and the mixer show the clip's track.
    assert_eq!(
        sel.tracks.iter().copied().collect::<Vec<_>>(),
        vec![track_id(&h, 2)]
    );
    let node = h.get_by_role_and_label(Role::GridCell, LEAD_A);
    assert_eq!(node.accesskit_node().is_selected(), Some(true));
    h.get_by_role_and_label(Role::Status, &Lang::De.status_clips_selected(1));
    // Strg-click adds another, a click on empty lane space clears and selects that track.
    click_with(clip_rect(&h, KEYS_1).center(), Modifiers::COMMAND, &mut h);
    assert_eq!(h.state().ui().selection.clips.len(), 2);
    let empty = pos2(x_of(&h, Tick(BAR / 2)), lead_header(&h).center().y);
    click(empty, &mut h);
    assert!(h.state().ui().selection.clips.is_empty());
    assert!(h.state().ui().selection.tracks.contains(&track_id(&h, 2)));
}

#[test]
fn a_box_on_empty_space_selects_the_clips_it_touches() {
    let mut h = harness(demo());
    let lead = lead_header(&h);
    let keys = header_rect(&h, "Spur Keys, Plugin fehlt: Dexed");
    let from = pos2(x_of(&h, Tick(BAR + BAR / 2)), lead.center().y);
    let to = pos2(x_of(&h, Tick(6 * BAR + BAR / 2)), keys.center().y);
    drag(from, to, &mut h);
    let mut want = vec![clip_id(&h, 3, 0), clip_id(&h, 4, 0)];
    want.sort();
    let got: Vec<ClipId> = h.state().ui().selection.clips.iter().copied().collect();
    assert_eq!(got, want);
}

#[test]
fn dragging_moves_on_the_grid_shift_without_snap() {
    let mut h = harness(demo());
    let keys = clip_id(&h, 4, 0);
    let r = clip_rect(&h, KEYS_1);
    let px_per_bar = h.state().ui().arrangement.px_per_bar;
    // 4.3 bars right: snaps to 4 bars (start bar 9).
    drag(r.center(), r.center() + vec2(4.3 * px_per_bar, 0.0), &mut h);
    let start = |h: &Harness<'_, App>| h.state().project().find_clip(keys).unwrap().1.start;
    assert_eq!(start(&h), Tick(8 * BAR));
    // One undo step.
    undo(&mut h);
    assert_eq!(start(&h), Tick(4 * BAR));
    // Shift: off the grid.
    let r = clip_rect(&h, KEYS_1);
    drag_with(
        r.center(),
        r.center() + vec2(0.5 * px_per_bar, 0.0),
        Modifiers::SHIFT,
        &mut h,
    );
    let s = start(&h).0;
    assert!((s - (4 * BAR + BAR / 2)).abs() < 100, "{s}");
}

#[test]
fn dragging_to_another_track_moves_the_clip_there() {
    let mut h = harness(demo());
    let lead_a = clip_id(&h, 2, 0);
    let r = clip_rect(&h, LEAD_A);
    // Two rows down: Keys, which is free in bars 9–10.
    drag(r.center(), r.center() + vec2(0.0, 2.0 * 56.0), &mut h);
    let (track, clip) = h.state().project().find_clip(lead_a).unwrap();
    assert_eq!(track.name, "Keys");
    assert_eq!(clip.start, Tick(8 * BAR));
}

#[test]
fn alt_drag_copies_and_the_original_stays() {
    let mut h = harness(demo());
    let lead_a = clip_id(&h, 2, 0);
    let r = clip_rect(&h, LEAD_A);
    let from = r.center();
    let to = from + vec2(0.0, 2.0 * 56.0);
    // Mid-drag: the original shows dashed, the copy is selected.
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    h.event(Event::PointerMoved(from));
    h.step();
    press(from, true, Modifiers::ALT, &mut h);
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    assert!(
        h.query_by_role_and_label(Role::GridCell, LEAD_A).is_some(),
        "the copy on Keys has the same name and bars"
    );
    press(to, false, Modifiers::ALT, &mut h);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
    let p = project(&h);
    assert_eq!(
        p.find_clip(lead_a).unwrap().0.name,
        "Lead",
        "original stays"
    );
    let copy = p.tracks[4]
        .clips
        .iter()
        .find(|c| c.start == Tick(8 * BAR))
        .expect("copy on Keys");
    assert_eq!(copy.name, "Lead A");
    assert_eq!(
        h.state()
            .ui()
            .selection
            .clips
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![copy.id]
    );
    // One undo step removes the copy.
    undo(&mut h);
    assert_eq!(project(&h).tracks[4].clips.len(), 2);
}

#[test]
fn dragging_the_edge_trims_with_the_length_in_a_tooltip() {
    let mut h = harness(demo());
    let keys = clip_id(&h, 4, 0);
    let r = clip_rect(&h, KEYS_1);
    let px_per_bar = h.state().ui().arrangement.px_per_bar;
    let from = pos2(r.right() - 2.0, r.bottom() - 6.0);
    let to = from - vec2(1.4 * px_per_bar, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(from, true, Modifiers::NONE, &mut h);
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    // 4 bars − 1.4 → the end snaps to bar 8: 3 bars, "3.0.0".
    h.get_by_label("3.0.0");
    press(to, false, Modifiers::NONE, &mut h);
    h.run_steps(2);
    assert_eq!(
        h.state().project().find_clip(keys).unwrap().1.length,
        Tick(3 * BAR)
    );
    assert!(h.query_by_label("3.0.0").is_none(), "tooltip gone");
}

#[test]
fn the_top_right_corner_loops_the_clip() {
    let mut h = harness(demo());
    let keys = clip_id(&h, 4, 0);
    let r = clip_rect(&h, KEYS_1);
    let px_per_bar = h.state().ui().arrangement.px_per_bar;
    let from = pos2(r.right() - 3.0, r.top() + 4.0);
    drag(from, from + vec2(2.0 * px_per_bar, 0.0), &mut h);
    let c = h.state().project().find_clip(keys).unwrap().1.clone();
    assert_eq!(c.length, Tick(6 * BAR));
    assert_eq!(c.loop_length, Some(Tick(4 * BAR)));
    // The looped clip shows its ▾ and its repetition; AccessKit names the new bars.
    h.get_by_role_and_label(Role::GridCell, "Clip Keys, Takt 5\u{2013}10");
}

#[test]
fn double_click_opens_a_clip_or_makes_a_one_bar_clip() {
    let mut h = harness(demo());
    let lead_a = clip_id(&h, 2, 0);
    double_click(clip_rect(&h, LEAD_A).center(), &mut h);
    assert_eq!(h.state().ui().editor.clip, Some(lead_a));
    assert_eq!(h.state().ui().focus, FocusArea::PianoRoll);
    // Empty space of Lead in bar 3: an empty MIDI clip, one bar, opened with the pencil.
    let at = pos2(
        x_of(&h, Tick(2 * BAR + BAR / 3)),
        lead_header(&h).center().y,
    );
    double_click(at, &mut h);
    let p = project(&h);
    let new = p.tracks[2]
        .clips
        .iter()
        .find(|c| c.start == Tick(2 * BAR))
        .expect("new clip");
    assert_eq!(new.length, Tick(BAR));
    assert_eq!(new.name, de::ARRANGEMENT_NEW_MIDI_CLIP);
    assert_eq!(h.state().ui().editor.clip, Some(new.id));
    assert_eq!(
        h.state().ui().piano_roll.tool,
        komponist_app::ui_state::Tool::Pencil
    );
}

#[test]
fn ctrl_d_ctrl_e_and_0_act_on_the_selected_clip() {
    let mut h = harness(demo());
    let keys = clip_id(&h, 4, 0);
    click(clip_rect(&h, KEYS_1).center(), &mut h);
    // Strg D: a copy right behind it.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::D);
    h.run_steps(2);
    let p = project(&h);
    assert!(p.tracks[4].clips.iter().any(|c| c.start == Tick(8 * BAR)));
    // 0 mutes the selection (the copy).
    h.key_press(egui::Key::Num0);
    h.run_steps(2);
    let p = project(&h);
    let copy = p.tracks[4]
        .clips
        .iter()
        .find(|c| c.start == Tick(8 * BAR))
        .unwrap();
    assert!(copy.mute);
    h.get_by_role_and_label(Role::GridCell, "Clip Keys, Takt 9\u{2013}12, stumm");
    // Strg E splits the selected clip at the playhead.
    click(clip_rect(&h, KEYS_1).center(), &mut h);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(6 * BAR))));
    // The engine counts in samples: the playhead lands within a tick of bar 7.
    assert!(step_until(&mut h, |a| (a.engine().playhead().0 - 6 * BAR)
        .abs()
        <= 1));
    let at = h.state().engine().playhead();
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::E);
    h.run_steps(2);
    let p = project(&h);
    assert_eq!(p.find_clip(keys).unwrap().1.length, at - Tick(4 * BAR));
    assert!(p.tracks[4].clips.iter().any(|c| c.start == at));
}

#[test]
fn esc_during_a_drag_cancels_it() {
    let mut h = harness(demo());
    let keys = clip_id(&h, 4, 0);
    let r = clip_rect(&h, KEYS_1);
    let from = r.center();
    let to = from + vec2(200.0, 0.0);
    h.event(Event::PointerMoved(from));
    h.step();
    press(from, true, Modifiers::NONE, &mut h);
    h.step();
    for i in 1..=5 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 5.0)));
        h.step();
    }
    assert_ne!(
        h.state().project().find_clip(keys).unwrap().1.start,
        Tick(4 * BAR)
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // Moving on after Esc changes nothing.
    h.event(Event::PointerMoved(to + vec2(150.0, 0.0)));
    h.run_steps(2);
    press(to + vec2(150.0, 0.0), false, Modifiers::NONE, &mut h);
    h.run_steps(2);
    assert_eq!(
        h.state().project().find_clip(keys).unwrap().1.start,
        Tick(4 * BAR)
    );
    assert_eq!(h.state().doc().undo_label(), None);
    // The selection stays.
    assert!(h.state().ui().selection.clips.contains(&keys));
}

// ------------------------------------------------------------------------------------------------
// Ruler, zoom, grid, scroll

#[test]
fn ruler_sets_the_playhead_across_its_height() {
    let mut h = harness(demo());
    let ruler = rect_of(&h, Role::Slider, de::A11Y_RULER);
    // Top half at bar 5 + a bit: on the beat.
    let x = x_of(&h, Tick(4 * BAR + 100));
    click(pos2(x, ruler.top() + 4.0), &mut h);
    assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(4 * BAR)));
    // Telemetry may arrive after the previous frame painted its AccessKit tree. Paint a new
    // frame after observing the engine position before checking the ruler's accessible value.
    h.step();
    assert_eq!(
        value_of(&h, Role::Slider, de::A11Y_RULER).as_deref(),
        Some("5.1.000")
    );
    // Drag in the former loop strip while looping is off.
    let y = ruler.bottom() - 3.0;
    let from = pos2(x_of(&h, Tick(2 * BAR)), y);
    let target = Tick(6 * BAR);
    let to = pos2(x_of(&h, target), y);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::ToggleLoop));
    assert!(!h.state().project().loop_on);
    let loop_range = h.state().project().loop_range;
    drag(from, to, &mut h);
    // The tempo map rounds to a sample and floors back to a tick.
    assert!(
        step_until(&mut h, |a| a.engine().playhead().0.abs_diff(target.0) <= 1),
        "playhead at {:?}, expected {target:?}",
        h.state().engine().playhead()
    );
    let p = project(&h);
    assert!(!p.loop_on);
    assert_eq!(p.loop_range, loop_range);
}

#[test]
fn plus_zooms_around_the_mouse_and_alt_plus_vertically() {
    let mut h = harness(demo());
    let lead = lead_header(&h);
    let at = pos2(x_of(&h, Tick(8 * BAR)), lead.center().y);
    h.event(Event::PointerMoved(at));
    h.run_steps(2);
    h.key_press(egui::Key::Plus);
    h.run_steps(2);
    let v = h.state().ui().arrangement;
    assert_eq!(v.px_per_bar, 72.0 * 1.25);
    // Bar 9 stays under the mouse.
    assert!(
        (x_of(&h, Tick(8 * BAR)) - at.x).abs() < 0.5,
        "{}",
        x_of(&h, Tick(8 * BAR))
    );
    h.key_press(egui::Key::Minus);
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.px_per_bar, 72.0);
    // Alt +: taller rows.
    h.key_press_modifiers(Modifiers::ALT, egui::Key::Plus);
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.v_zoom, 1.25);
    assert_eq!(lead_header(&h).height(), 70.0);
}

#[test]
fn buttons_zoom_around_the_playhead() {
    let mut h = harness(demo());
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(12 * BAR))));
    assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(12 * BAR)));
    let before = x_of(&h, Tick(12 * BAR));
    h.get_by_role_and_label(Role::Button, de::VIEW_ZOOM_IN)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.px_per_bar, 90.0);
    assert!((x_of(&h, Tick(12 * BAR)) - before).abs() < 0.5);
    h.get_by_role_and_label(Role::Button, de::VIEW_ZOOM_OUT)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.px_per_bar, 72.0);
}

#[test]
fn z_fits_the_selection() {
    let mut h = harness(demo());
    click(
        clip_rect(&h, "Clip Lead B, Takt 13\u{2013}16").center(),
        &mut h,
    );
    h.key_press(egui::Key::Z);
    h.run_steps(2);
    let v = h.state().ui().arrangement;
    assert_eq!(v.scroll_tick, (12 * BAR) as f64);
    let ruler = rect_of(&h, Role::Slider, de::A11Y_RULER);
    // 4 bars fill the timeline (clamped to 160 px per bar).
    assert_eq!(v.px_per_bar, (ruler.width() / 4.0).min(160.0));
    // The fit is not re-anchored: Lead B starts at the left edge.
    assert!((x_of(&h, Tick(12 * BAR)) - ruler.left()).abs() < 0.5);
    assert!(
        (clip_rect(&h, "Clip Lead B, Takt 13\u{2013}16").left() - ruler.left() - 1.0).abs() < 0.5
    );
}

#[test]
fn z_brings_the_tracks_of_the_selection_into_view() {
    let mut h = harness(demo());
    // Tall rows: Arp lies below the view.
    for _ in 0..8 {
        h.key_press_modifiers(Modifiers::ALT, egui::Key::Plus);
    }
    h.run_steps(2);
    assert!(h.query_by_label_contains("Spur Arp").is_none());
    let arp = clip_id(&h, 5, 0);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![arp],
            mode: SelectMode::Replace,
        })));
    h.key_press(egui::Key::Z);
    h.run_steps(3);
    h.get_by_label_contains("Spur Arp");
    h.get_by_role_and_label(Role::GridCell, "Clip Arp, Takt 9\u{2013}16, stumm");
}

#[test]
fn ctrl_brackets_step_the_grid_and_the_header_shows_it() {
    let mut h = harness(demo());
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::OpenBracket);
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.grid, komponist_model::Grid::Half);
    assert_eq!(
        value_of(&h, Role::ComboBox, de::VIEW_GRID).as_deref(),
        Some("1/2")
    );
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.grid, komponist_model::Grid::Bar);
    // Snap off from the header.
    h.get_by_role_and_label(Role::Button, de::VIEW_SNAP).click();
    h.run_steps(2);
    assert!(!h.state().ui().arrangement.snap);
}

/// The rendered frame (1x) without the "Raster" dropdowns, whose label names the grid, and
/// without the CPU readout.
fn render_without_grid_labels(h: &mut Harness<'_, App>, label: &str) -> Vec<u8> {
    let mut rects: Vec<Rect> = h
        .get_all_by_role_and_label(Role::ComboBox, label)
        .map(|n| n.rect())
        .collect();
    rects.push(
        h.get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
            .rect(),
    );
    let img = h.render().expect("render");
    let (w, ht) = (img.width() as usize, img.height() as usize);
    let mut raw = img.into_raw();
    for r in rects {
        for y in r.top().max(0.0) as usize..(r.bottom().ceil() as usize).min(ht) {
            for x in r.left().max(0.0) as usize..(r.right().ceil() as usize).min(w) {
                raw[(y * w + x) * 4..(y * w + x) * 4 + 4].fill(0);
            }
        }
    }
    raw
}

/// The "Raster" dropdown changes the drawn grid lines of the lanes (every value, at a zoom where
/// it fits), and at low zoom the display thins out to a coarser level.
#[test]
fn the_grid_dropdown_changes_the_lane_grid_lines() {
    use komponist_model::Grid;
    let grids = [
        Grid::Bar,
        Grid::Half,
        Grid::Quarter,
        Grid::Eighth,
        Grid::Sixteenth,
        Grid::ThirtySecond,
        Grid::QuarterTriplet,
        Grid::EighthTriplet,
        Grid::SixteenthTriplet,
    ];
    let mut images = Vec::new();
    for grid in grids {
        let mut app = demo();
        app.ui_mut().arrangement.px_per_bar = 288.0;
        app.ui_mut().arrangement.grid = grid;
        let mut h = harness(app);
        images.push((grid, render_without_grid_labels(&mut h, de::VIEW_GRID)));
    }
    for (i, (a, ia)) in images.iter().enumerate() {
        for (b, ib) in &images[i + 1..] {
            assert!(ia != ib, "{a:?} and {b:?} draw the same grid");
        }
    }
    // At 72 px per bar a 1/32 raster (2.25 px) thins to the 1/8 level (9 px): same picture as 1/8.
    let render = |grid: Grid| {
        let mut app = demo();
        app.ui_mut().arrangement.grid = grid;
        render_without_grid_labels(&mut harness(app), de::VIEW_GRID)
    };
    assert!(
        render(Grid::ThirtySecond) == render(Grid::Eighth),
        "thinned 1/32 looks like 1/8"
    );
    // Snap keeps the chosen raster.
    let mut app = demo();
    app.ui_mut().arrangement.grid = Grid::ThirtySecond;
    let h = harness(app);
    assert_eq!(
        h.state().ui().arrangement.grid.ticks(demo_project().meter),
        Tick(120)
    );
}

/// The thinned grid (1/16 at 72 px per bar draws 1/8 lines) and the full one (288 px per bar).
#[test]
fn snapshots_of_the_arrangement_grid() {
    let mut results = SnapshotResults::new();
    for px_per_bar in [72.0, 288.0] {
        let mut app = reference_demo();
        app.ui_mut().arrangement.px_per_bar = px_per_bar;
        app.ui_mut().arrangement.grid = komponist_model::Grid::Sixteenth;
        let mut h = harness(app);
        let cpu = h
            .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
            .rect();
        h.mask(cpu.expand(2.0));
        h.snapshot(format!("arrangement-grid-16-{px_per_bar}px-1x"));
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

#[test]
fn the_wheel_scrolls_and_ctrl_wheel_zooms() {
    let mut h = harness(demo());
    let at = pos2(x_of(&h, Tick(4 * BAR)), lead_header(&h).center().y);
    h.event(Event::PointerMoved(at));
    h.step();
    h.event_modifiers(
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -200.0),
            modifiers: Modifiers::SHIFT,
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        },
        Modifiers::SHIFT,
    );
    h.run_steps(20);
    assert!(h.state().ui().arrangement.scroll_tick > 0.0);
    // Taller rows than the view: vertical scrolling.
    for _ in 0..8 {
        h.key_press_modifiers(Modifiers::ALT, egui::Key::Plus);
    }
    h.run_steps(2);
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: vec2(0.0, -120.0),
        modifiers: Modifiers::NONE,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel,
    });
    h.run_steps(20);
    assert!(h.state().ui().arrangement.scroll_y > 0.0);
}

#[test]
fn only_visible_tracks_and_clips_have_nodes() {
    let mut h = harness(demo());
    assert_eq!(h.query_all_by_role(Role::Row).count(), 6);
    let clips = h.query_all_by_role(Role::GridCell).count();
    assert_eq!(clips, 13);
    // Tall rows: the last tracks leave the view, their nodes go.
    for _ in 0..8 {
        h.key_press_modifiers(Modifiers::ALT, egui::Key::Plus);
    }
    h.run_steps(2);
    let rows = h.query_all_by_role(Role::Row).count();
    assert!(rows < 6, "{rows} rows");
    assert!(h.query_all_by_role(Role::GridCell).count() < clips);
}

// ------------------------------------------------------------------------------------------------
// S10

#[test]
fn an_empty_project_shows_the_hint_and_its_primary_action() {
    let mut h = harness(App::headless(komponist_model::fixtures::empty_project()));
    h.get_by_role_and_label(Role::Label, de::ARRANGEMENT_EMPTY_TITLE);
    h.get_by_role_and_label(Role::Label, de::ARRANGEMENT_EMPTY_TEXT);
    h.get_by_role_and_label(Role::Button, de::ARRANGEMENT_NEW_TRACK_BUTTON);
    h.get_by_label_contains("0 Spuren");
    h.get_by_role_and_label(Role::Button, de::ARRANGEMENT_OPEN_PROJECT);
    h.get_by_role_and_label(Role::Button, de::ARRANGEMENT_ADD_MIDI_TRACK)
        .click();
    h.run_steps(2);
    let p = project(&h);
    assert_eq!(p.tracks.len(), 1);
    assert!(p.tracks[0].arm);
    assert!(h.state().ui().selection.tracks.contains(&p.tracks[0].id));
    // The hint disappears with the first track.
    assert!(
        h.query_by_role_and_label(Role::Label, de::ARRANGEMENT_EMPTY_TITLE)
            .is_none()
    );
    // A double-click into the new track makes a one-bar clip and opens it (S10 "Leere Spur").
    let r = header_rect(&h, &format!("Spur {}, Hexa", p.tracks[0].name));
    let at = pos2(x_of(&h, Tick(BAR / 2)), r.center().y);
    double_click(at, &mut h);
    let p = project(&h);
    assert_eq!(p.tracks[0].clips.len(), 1);
    assert_eq!(h.state().ui().editor.clip, Some(p.tracks[0].clips[0].id));
}

#[test]
fn snapshot_of_an_empty_project_next_to_s10() {
    let mut h = harness_at(
        crate::common::reference_app(komponist_model::fixtures::empty_project()),
        1.0,
    );
    h.run_steps(3);
    let cpu = h
        .get_by_role_and_label(Role::Status, komponist_i18n::de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    h.snapshot("arrangement-s10-1x");
}

// ------------------------------------------------------------------------------------------------
// "Neue Spur": the box below the tracks and its dialog

mod new_track {
    use komponist_i18n::de;

    use super::*;

    fn new_track_box(h: &Harness<'_, App>) -> Rect {
        rect_of(h, Role::Button, de::ARRANGEMENT_NEW_TRACK_BUTTON)
    }

    fn dialog_open(h: &Harness<'_, App>) -> bool {
        h.query_by_role_and_label(Role::Dialog, de::NEW_TRACK_TITLE)
            .is_some()
    }

    fn open_by_click(h: &mut Harness<'_, App>) {
        click(new_track_box(h).center(), h);
        h.run_steps(3);
        assert!(dialog_open(h));
    }

    fn undo_text(h: &Harness<'_, App>) -> Option<String> {
        h.state().undo_text()
    }

    fn names_of(h: &Harness<'_, App>) -> Vec<String> {
        h.state()
            .project()
            .tracks
            .iter()
            .map(|t| t.name.clone())
            .collect()
    }

    fn option(h: &Harness<'_, App>, name: &str) -> Rect {
        h.get_by_role_and_label(Role::ListBoxOption, name).rect()
    }

    fn type_in(h: &mut Harness<'_, App>, label: &str, text: &str) {
        h.get_by_role_and_label(Role::TextInput, label).focus();
        h.run_steps(1);
        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
        h.run_steps(1);
        h.get_by_role_and_label(Role::TextInput, label)
            .type_text(text);
        h.run_steps(2);
    }

    #[test]
    fn a_click_opens_the_dialog_and_esc_cancels_without_a_change() {
        let mut h = harness(demo());
        let before = names_of(&h);
        let undo_before = undo_text(&h);
        assert!(!dialog_open(&h));
        open_by_click(&mut h);
        // Hexa is chosen, the name is the automatic one, the count 1.
        let sel = h.get_by_role_and_label(Role::ListBoxOption, "Hexa");
        assert!(sel.accesskit_node().is_selected() == Some(true));
        h.get_by_role_and_label(Role::SpinButton, de::NEW_TRACK_COUNT);
        h.get_by_role_and_label(Role::TextInput, de::NEW_TRACK_NAME);
        h.key_press(egui::Key::Escape);
        h.run_steps(3);
        assert!(!dialog_open(&h));
        assert_eq!(names_of(&h), before);
        assert_eq!(undo_text(&h), undo_before);
        // "Abbrechen" does the same.
        open_by_click(&mut h);
        h.get_by_role_and_label(Role::Button, de::DIALOG_CANCEL)
            .click();
        h.run_steps(3);
        assert!(!dialog_open(&h));
        assert_eq!(names_of(&h), before);
    }

    #[test]
    fn enter_and_space_on_the_focused_box_open_the_dialog() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let mut h = harness(demo());
            h.get_by_role_and_label(Role::Button, de::ARRANGEMENT_NEW_TRACK_BUTTON)
                .focus();
            h.run_steps(2);
            h.key_press(key);
            h.run_steps(3);
            assert!(dialog_open(&h), "{key:?}");
        }
    }

    #[test]
    fn one_track_with_hexa_is_what_a_drop_makes() {
        let mut dropped = harness(demo());
        let at = new_track_box(&dropped).center();
        drop_plugin(hexa(dropped.state()), at, &mut dropped);
        assert!(!dialog_open(&dropped), "a drop does not open the dialog");

        let mut h = harness(demo());
        open_by_click(&mut h);
        h.get_by_role_and_label(Role::Button, de::NEW_TRACK_CREATE)
            .click();
        h.run_steps(3);
        assert!(!dialog_open(&h));
        let (a, b) = (project(&h), project(&dropped));
        assert_eq!(a.tracks.len(), 7);
        let (made, drop) = (a.tracks.last().unwrap(), b.tracks.last().unwrap());
        assert_eq!(made.name, drop.name);
        assert_eq!(
            made.channel.devices[0].plugin,
            drop.channel.devices[0].plugin
        );
        assert_eq!(made.channel.devices[0].plugin.name, "Hexa");
        assert!(made.arm && made.monitor);
        assert_eq!(
            h.state()
                .ui()
                .selection
                .tracks
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [made.id]
        );
        assert_eq!(undo_text(&h), undo_text(&dropped));
        assert_eq!(undo_text(&h).as_deref(), Some(de::UNDO_NEW_MIDI_TRACK));
    }

    #[test]
    fn three_tracks_without_instrument_are_numbered_and_one_undo_removes_them() {
        let mut h = harness(App::headless(komponist_model::fixtures::empty_project()));
        click(new_track_box(&h).center(), &mut h);
        h.run_steps(3);
        assert!(dialog_open(&h));
        let none = option(&h, de::NEW_TRACK_NO_INSTRUMENT).center();
        click(none, &mut h);
        h.get_by_role_and_label(Role::SpinButton, de::NEW_TRACK_COUNT)
            .set_value_accesskit(3.0);
        h.run_steps(2);
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        assert!(!dialog_open(&h));
        assert_eq!(names_of(&h), ["MIDI", "MIDI 2", "MIDI 3"]);
        let p = project(&h);
        assert!(p.tracks.iter().all(|t| t.channel.devices.is_empty()));
        assert_eq!(
            p.tracks.iter().filter(|t| t.arm).count(),
            1,
            "only the last is armed"
        );
        assert!(p.tracks[2].arm);
        assert_eq!(h.state().ui().selection.tracks.len(), 3);
        assert_eq!(undo_text(&h).as_deref(), Some(de::UNDO_ADD_TRACKS));
        undo(&mut h);
        assert!(project(&h).tracks.is_empty(), "one undo removes all three");
    }

    #[test]
    fn a_typed_name_numbers_the_tracks_and_the_instrument_choice_is_remembered() {
        let mut h = harness(demo());
        open_by_click(&mut h);
        type_in(&mut h, de::NEW_TRACK_NAME, "Lead");
        h.get_by_role_and_label(Role::SpinButton, de::NEW_TRACK_COUNT)
            .set_value_accesskit(2.0);
        h.run_steps(2);
        click(option(&h, de::NEW_TRACK_NO_INSTRUMENT).center(), &mut h);
        h.get_by_role_and_label(Role::Button, de::NEW_TRACK_CREATE)
            .click();
        h.run_steps(3);
        let names = names_of(&h);
        assert_eq!(names[6..], ["Lead 1", "Lead 2"]);
        // The next dialog starts with "Kein Instrument".
        open_by_click(&mut h);
        let none = h.get_by_role_and_label(Role::ListBoxOption, de::NEW_TRACK_NO_INSTRUMENT);
        assert!(none.accesskit_node().is_selected() == Some(true));
    }

    #[test]
    fn the_search_filters_the_instruments() {
        let mut h = harness(demo());
        open_by_click(&mut h);
        assert!(
            h.query_by_role_and_label(Role::ListBoxOption, "Hexa")
                .is_some()
        );
        h.get_by_role_and_label(Role::ListBoxOption, de::NEW_TRACK_NO_INSTRUMENT);
        type_in(&mut h, de::NEW_TRACK_SEARCH, "sampl");
        assert!(
            h.query_by_role_and_label(Role::ListBoxOption, "Hexa")
                .is_none()
        );
        // The first hit is chosen; Enter creates with it.
        let sampler = h
            .query_all_by_role(Role::ListBoxOption)
            .find(|n| n.accesskit_node().is_selected() == Some(true))
            .expect("a chosen hit")
            .accesskit_node()
            .label()
            .unwrap_or_default();
        assert!(sampler.to_lowercase().contains("sampl"), "{sampler}");
        h.key_press(egui::Key::Enter);
        h.run_steps(3);
        let p = project(&h);
        assert_eq!(
            p.tracks.last().unwrap().channel.devices[0].plugin.name,
            sampler
        );
    }

    #[test]
    fn arrow_keys_move_the_choice() {
        let mut h = harness(demo());
        open_by_click(&mut h);
        h.key_press(egui::Key::ArrowUp);
        h.run_steps(2);
        let none = h.get_by_role_and_label(Role::ListBoxOption, de::NEW_TRACK_NO_INSTRUMENT);
        assert!(none.accesskit_node().is_selected() == Some(true));
    }

    fn mask_cpu(h: &mut Harness<'_, App>) {
        // The CPU readout depends on the machine.
        let cpu = h
            .get_by_role_and_label(Role::Status, de::TRANSPORT_CPU)
            .rect();
        h.mask(cpu.expand(2.0));
    }

    #[test]
    fn snapshots_of_the_box_and_the_dialog() {
        let mut results = SnapshotResults::new();
        let mut h = harness(reference_demo());
        let at = new_track_box(&h).center();
        h.event(Event::PointerMoved(at));
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot("arrangement-new-track-hover-1x");
        results.extend_harness(&mut h);
        let mut h = harness(reference_demo());
        open_by_click(&mut h);
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot("new-track-dialog-1x");
        results.extend_harness(&mut h);
        results.unwrap();
    }
}

// Audio clips use the same canvas and undo grouping as MIDI, with source trim and envelopes.
fn audio_project() -> (Project, ClipId) {
    use komponist_model::AudioFile;
    use komponist_model::edits::{self, NewAudioTrack, NewClip, NewClipContent};
    let mut p = Project::new("Audio gestures");
    p.tempo = komponist_model::Tempo::new(120.0);
    let (p, track) = edits::add_audio_track(&p, NewAudioTrack::default()).unwrap();
    let file = AudioFile::new(
        std::path::Path::new("/missing/verse.wav"),
        None,
        384000,
        48000,
        1,
    );
    let (p, clip) = edits::add_clip(
        &p,
        track,
        NewClip {
            start: Tick(BAR),
            length: Tick(2 * BAR),
            name: "Verse".into(),
            content: NewClipContent::Audio { file, offset: 0 },
        },
    )
    .unwrap();
    let (p, _) = edits::add_audio_track(&p, NewAudioTrack::default()).unwrap();
    (p, clip)
}
fn audio_data(h: &Harness<'_, App>, clip: ClipId) -> komponist_model::AudioClip {
    let (_, c) = h.state().project().find_clip(clip).unwrap();
    let komponist_model::ClipContent::Audio(a) = &c.content else {
        panic!("audio clip expected")
    };
    a.clone()
}
/// The arrangement rect of audio clip `clip`. Its label ends in the missing-file suffix, which
/// the file pool's asynchronous check decides: step (bounded) until the node is there instead of
/// assuming it after a fixed number of frames (#267).
fn audio_rect(h: &mut Harness<'_, App>, clip: ClipId) -> Rect {
    let (_, c) = h.state().project().find_clip(clip).unwrap();
    let palette = komponist_tokens::TrackPalette::new(150);
    let model = komponist_ui::timeline::ClipModel::new(&c.name, &palette);
    let name = komponist_ui::timeline::clip_access(
        Lang::De,
        &model,
        c.start,
        c.end(),
        h.state().project().meter,
        komponist_ui::timeline::ClipState::REST,
    )
    .name;
    let label = format!("{name}{}", Lang::De.a11y_audio_missing_suffix());
    for _ in 0..100 {
        let found = h
            .query_all_by_role_and_label(Role::GridCell, &label)
            .map(|n| n.rect())
            .min_by(|a, b| a.top().total_cmp(&b.top()));
        if let Some(rect) = found {
            return rect;
        }
        h.step();
    }
    clip_rect(h, &label)
}
#[test]
fn audio_gain_fades_and_source_trim_are_single_undo_steps() {
    let (p, id) = audio_project();
    let mut h = harness(App::headless(p));
    let r = audio_rect(&mut h, id);
    // The grip at unity is one third below the header. Move it down by twelve dB.
    let parts = komponist_ui::timeline::audio_parts(r, 0.0, 0.0, 0.0);
    drag(
        parts.gain.center(),
        parts.gain.center() + vec2(0.0, 12.0),
        &mut h,
    );
    assert!(
        (audio_data(&h, id).gain_db + 12.0).abs() < 0.1,
        "gain was {}",
        audio_data(&h, id).gain_db
    );
    undo(&mut h);
    assert_eq!(audio_data(&h, id).gain_db, 0.0);
    let parts = komponist_ui::timeline::audio_parts(audio_rect(&mut h, id), 0.0, 0.0, 0.0);
    drag(
        parts.fade_in.center(),
        parts.fade_in.center() + vec2(18.0, 0.0),
        &mut h,
    );
    assert!(audio_data(&h, id).fade_in > Tick::ZERO);
    undo(&mut h);
    assert_eq!(audio_data(&h, id).fade_in, Tick::ZERO);
    let r = audio_rect(&mut h, id);
    drag(
        pos2(r.left() + 2.0, r.bottom() - 4.0),
        pos2(r.left() + 74.0, r.bottom() - 4.0),
        &mut h,
    );
    let (_, c) = h.state().project().find_clip(id).unwrap();
    assert_eq!(c.start, Tick(2 * BAR));
    assert_eq!(audio_data(&h, id).offset, 96000);
    undo(&mut h);
    assert_eq!(audio_data(&h, id).offset, 0);
}
#[test]
fn audio_controls_expose_localized_accesskit_values() {
    for lang in [Lang::De, Lang::En] {
        let (p, id) = audio_project();
        let mut h = harness(App::headless(p).with_lang(lang));
        h.state_mut()
            .dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
                clips: vec![id],
                mode: SelectMode::Replace,
            })));
        h.run_steps(2);
        assert!(
            h.query_by_role_and_label(Role::Slider, lang.audio_clip_fade_in())
                .is_some()
        );
        assert_eq!(
            value_of(&h, Role::Slider, lang.audio_clip_gain_line()),
            Some("0.0 dB".into())
        );
        assert!(
            h.query_by_role_and_label(Role::Slider, lang.audio_clip_start())
                .is_some()
        );
        assert!(
            h.query_by_role_and_label(Role::Slider, lang.audio_clip_end())
                .is_some()
        );
    }
}
#[test]
fn audio_end_trim_never_loops_or_passes_the_source_end() {
    let (p, id) = audio_project();
    let mut h = harness(App::headless(p));
    let r = audio_rect(&mut h, id);
    drag(
        pos2(r.right() - 2.0, r.top() + 4.0),
        pos2(r.right() + 720.0, r.top() + 4.0),
        &mut h,
    );
    let (_, c) = h.state().project().find_clip(id).unwrap();
    assert!(c.loop_length.is_none());
    assert_eq!(
        c.length,
        audio_data(&h, id).max_length(h.state().project().tempo)
    );
    undo(&mut h);
    assert_eq!(
        h.state().project().find_clip(id).unwrap().1.length,
        Tick(2 * BAR)
    );
}
#[test]
fn audio_trimming_out_and_back_restores_covered_material() {
    use komponist_model::edits::{self, NewClip, NewClipContent};
    let (p, id) = audio_project();
    let track = p.tracks[0].id;
    let file = match &p.find_clip(id).unwrap().1.content {
        komponist_model::ClipContent::Audio(a) => a.file.clone(),
        _ => unreachable!(),
    };
    let (p, neighbor) = edits::add_clip(
        &p,
        track,
        NewClip {
            start: Tick(3 * BAR),
            length: Tick(BAR),
            name: "Next".into(),
            content: NewClipContent::Audio { file, offset: 0 },
        },
    )
    .unwrap();
    let mut app = App::headless(p.clone());
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "undo-clip-length".into(),
    }));
    app.dispatch(Action::Edit(EditAction::SetClipLength {
        clip: id,
        length: Tick(3 * BAR),
    }));
    assert!(app.project().find_clip(neighbor).is_none());
    app.dispatch(Action::Edit(EditAction::SetClipLength {
        clip: id,
        length: Tick(2 * BAR),
    }));
    assert_eq!(
        app.project().find_clip(neighbor).unwrap().1,
        p.find_clip(neighbor).unwrap().1
    );
    app.dispatch(Action::Edit(EditAction::EndGesture));
}

#[test]
fn audio_left_trim_out_and_back_restores_covered_material() {
    use komponist_model::edits::{self, NewClip, NewClipContent};
    let (mut p, id) = audio_project();
    let track = Arc::make_mut(&mut p.tracks[0]);
    let main = Arc::make_mut(&mut track.clips[0]);
    let komponist_model::ClipContent::Audio(a) = &mut main.content else {
        unreachable!()
    };
    a.offset = 96000;
    let file = a.file.clone();
    let (p, prior) = edits::add_clip(
        &p,
        p.tracks[0].id,
        NewClip {
            start: Tick::ZERO,
            length: Tick(BAR),
            name: "Before".into(),
            content: NewClipContent::Audio { file, offset: 0 },
        },
    )
    .unwrap();
    let mut app = App::headless(p.clone());
    app.dispatch(Action::Edit(EditAction::BeginGesture {
        label: "undo-clip-length".into(),
    }));
    app.dispatch(Action::Edit(EditAction::TrimAudioClipStart {
        clip: id,
        start: Tick::ZERO,
    }));
    assert!(app.project().find_clip(prior).is_none());
    app.dispatch(Action::Edit(EditAction::TrimAudioClipStart {
        clip: id,
        start: Tick(BAR),
    }));
    assert_eq!(
        app.project().find_clip(prior).unwrap().1,
        p.find_clip(prior).unwrap().1
    );
    assert_eq!(
        app.project().find_clip(id).unwrap().1,
        p.find_clip(id).unwrap().1
    );
}
#[test]
fn audio_split_and_duplicate_keep_file_windows() {
    let (p, id) = audio_project();
    let mut h = harness(App::headless(p));
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![id],
            mode: SelectMode::Replace,
        })));
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(2 * BAR))));
    assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(2 * BAR)));
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SplitAtPlayhead));
    h.run_steps(2);
    let right = h.state().project().tracks[0].clips[1].id;
    assert_eq!(audio_data(&h, right).offset, 96000);
    assert_eq!(audio_data(&h, id).file, audio_data(&h, right).file);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![right],
            mode: SelectMode::Replace,
        })));
    h.state_mut()
        .dispatch(Action::Edit(EditAction::DuplicateSelection));
    h.run_steps(2);
    let clips = &h.state().project().tracks[0].clips;
    assert_eq!(clips.len(), 3);
    let copy = clips[2].id;
    assert_eq!(clips[2].start, Tick(3 * BAR));
    assert_eq!(audio_data(&h, copy).offset, 96000);
    assert_eq!(audio_data(&h, copy).file, audio_data(&h, right).file);
}
#[test]
fn audio_mixed_selection_stays_on_grid_and_wrong_track_refuses() {
    use komponist_model::edits::{self, NewClip, NewClipContent, NewTrack};
    let (p, audio) = audio_project();
    let (p, midi_track) = edits::add_track(
        &p,
        NewTrack {
            name: None,
            hue: None,
            instrument: None,
            index: None,
            arm_exclusive: false,
        },
    )
    .unwrap();
    let (p, midi) = edits::add_clip(
        &p,
        midi_track,
        NewClip {
            start: Tick(1000),
            length: Tick(BAR),
            name: "MIDI".into(),
            content: NewClipContent::Midi {
                notes: vec![],
                take_name: String::new(),
            },
        },
    )
    .unwrap();
    let mut app = App::headless(p);
    app.ui_mut().arrangement.snap = true;
    app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
        clips: vec![audio, midi],
        mode: SelectMode::Replace,
    })));
    let mut h = harness(app);
    let name = Lang::De.a11y_clip("MIDI", Lang::De.a11y_bars(1, 2));
    let r = clip_rect(&h, &name);
    drag(
        pos2(r.center().x, r.bottom() - 4.0),
        pos2(r.center().x + 43.0, r.bottom() - 4.0),
        &mut h,
    );
    let (_, c) = h.state().project().find_clip(audio).unwrap();
    assert_eq!(c.start, Tick(2 * BAR));
    undo(&mut h);
    h.state_mut()
        .dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![audio],
            mode: SelectMode::Replace,
        })));
    h.run_steps(2);
    let r = audio_rect(&mut h, audio);
    drag(
        pos2(r.center().x, r.bottom() - 4.0),
        pos2(r.center().x + 72.0, r.bottom() + 108.0),
        &mut h,
    );
    let (track, c) = h.state().project().find_clip(audio).unwrap();
    assert_ne!(track.id, midi_track);
    assert_eq!(c.start, Tick(BAR));
}

#[test]
fn audio_move_and_both_trim_edges_honor_disabled_grid_snapping() {
    let (p, id) = audio_project();
    let mut app = App::headless(p);
    app.ui_mut().arrangement.snap = false;
    let mut h = harness(app);
    let grid = h
        .state()
        .ui()
        .arrangement
        .grid
        .ticks(h.state().project().meter)
        .0;
    let r = audio_rect(&mut h, id);
    drag(
        pos2(r.center().x, r.bottom() - 4.0),
        pos2(r.center().x + 19.0, r.bottom() - 4.0),
        &mut h,
    );
    let start = h.state().project().find_clip(id).unwrap().1.start.0;
    assert!(start > BAR && start < 2 * BAR);
    assert_ne!(
        start.rem_euclid(grid),
        0,
        "disabled snap must keep the free move"
    );
    undo(&mut h);
    let r = audio_rect(&mut h, id);
    drag(
        pos2(r.left() + 2.0, r.bottom() - 4.0),
        pos2(r.left() + 21.0, r.bottom() - 4.0),
        &mut h,
    );
    let start = h.state().project().find_clip(id).unwrap().1.start.0;
    assert!(start > BAR && start < 2 * BAR);
    assert_ne!(
        start.rem_euclid(grid),
        0,
        "source start follows the free pointer"
    );
    assert!(audio_data(&h, id).offset > 0);
    undo(&mut h);
    let r = audio_rect(&mut h, id);
    drag(
        pos2(r.right() - 2.0, r.top() + 3.0),
        pos2(r.right() - 21.0, r.top() + 3.0),
        &mut h,
    );
    let length = h.state().project().find_clip(id).unwrap().1.length.0;
    assert!(length < 2 * BAR && length > BAR);
    assert_ne!(
        length.rem_euclid(grid),
        0,
        "end trim follows the free pointer"
    );
    undo(&mut h);
    assert_eq!(
        h.state().project().find_clip(id).unwrap().1.start,
        Tick(BAR)
    );
    assert_eq!(
        h.state().project().find_clip(id).unwrap().1.length,
        Tick(2 * BAR)
    );
}

#[test]
fn automation_lane_edits_and_undo_keep_the_arrangement_clip_selection() {
    use komponist_app::views::automation::{Action as Automation, Source};
    use komponist_model::automation::{AutomationOwner, AutomationTarget};

    let mut app = demo();
    let track = app.project().tracks[0].id;
    let owner = AutomationOwner::Track(track);
    app.dispatch(Action::Automation(Automation::ShowTarget {
        owner,
        replace: None,
        target: AutomationTarget::Volume,
        label: de::PARAM_VOLUME.to_owned(),
    }));
    let mut h = harness(app);
    let lane = h.state().project().track(track).unwrap().automation.lanes[0].id;
    let name = format!(
        "Automation {}, {}",
        de::PARAM_VOLUME,
        h.state().project().tracks[0].name
    );
    let group = h
        .get_all_by_role_and_label(Role::Group, &name)
        .map(|node| node.rect())
        .max_by(|a, b| a.left().total_cmp(&b.left()))
        .unwrap();
    double_click(pos2(group.left() + 25.0, group.center().y), &mut h);
    let points = &h.state().project().track(track).unwrap().automation.lanes[0].points;
    let [created] = *points.as_slice() else {
        panic!("expected exactly one automation point");
    };
    assert!(h.state().ui().selection.clips.is_empty());
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert!(
        h.state().project().track(track).unwrap().automation.lanes[0]
            .points
            .is_empty()
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Redo));
    assert_eq!(
        h.state().project().track(track).unwrap().automation.lanes[0]
            .points
            .as_slice(),
        [created]
    );
    h.state_mut()
        .dispatch(Action::Automation(Automation::Remove(Source::Lane(
            owner, lane,
        ))));
    assert!(
        h.state()
            .project()
            .track(track)
            .unwrap()
            .automation
            .lanes
            .is_empty()
    );
}

#[test]
fn opening_an_offscreen_automation_lane_reveals_its_drop_target() {
    use komponist_app::actions::DropTarget;
    use komponist_app::views::automation::Action as Automation;
    use komponist_model::automation::{AutomationOwner, AutomationTarget};
    use komponist_model::edits::{self, TrackProp};

    let project = song_with_shipped_instruments(&App::headless_plugin_host());
    let tracks: Vec<_> = project.tracks.iter().map(|track| track.id).collect();
    let &track = tracks.last().unwrap();
    let project = edits::set_track_prop(&project, &tracks, TrackProp::Height(150)).unwrap();
    let name = Lang::De.a11y_automation_lane(de::PARAM_VOLUME, &project.track(track).unwrap().name);
    let mut h = harness(App::headless(project));
    assert_eq!(h.state().ui().arrangement.scroll_y, 0.0);

    h.state_mut()
        .dispatch(Action::Automation(Automation::ShowTarget {
            owner: AutomationOwner::Track(track),
            replace: None,
            target: AutomationTarget::Volume,
            label: de::PARAM_VOLUME.into(),
        }));
    h.run_steps(3);

    assert!(h.state().ui().arrangement.scroll_y > 0.0);
    let lane = h
        .get_all_by_role_and_label(Role::Group, &name)
        .map(|node| node.rect())
        .max_by(|a, b| a.left().total_cmp(&b.left()))
        .unwrap();
    drop_plugin(hexa(h.state()), lane.center(), &mut h);
    assert_eq!(h.state().project().tracks.len(), tracks.len());
    assert!(h.state().dispatched().any(|action| matches!(
        action,
        Action::Plugin(komponist_app::PluginAction::Add {
            target: DropTarget::Track(id),
            ..
        }) if *id == track
    )));
}

#[test]
fn compact_audio_header_keeps_input_and_automation_controls_separate() {
    use komponist_model::edits::{self, NewAudioTrack, TrackProp};

    let lang = Lang::De;
    let (project, track) =
        edits::add_audio_track(&Project::new("Compact"), NewAudioTrack::default()).unwrap();
    let project = edits::set_track_prop(&project, &[track], TrackProp::Height(32)).unwrap();
    let h = harness(App::headless(project));
    let input = h
        .get_by_role_and_label(Role::Button, &lang.audio_input_tip(lang.audio_input_none()))
        .rect();
    let toggle = h
        .get_all_by_role_and_label(Role::Button, lang.automation_show())
        .map(|node| node.rect())
        .find(|rect| rect.y_range().intersects(input.y_range()))
        .unwrap();
    assert!(input.right() < toggle.left());
}

#[test]
fn revealing_automation_unfolds_only_the_owners_ancestor_groups() {
    use komponist_app::views::automation::Action as Automation;
    use komponist_model::automation::{AutomationOwner, AutomationTarget};
    use komponist_model::edits::{self, NewAudioTrack, NewBus, TrackProp};

    let (project, track) =
        edits::add_audio_track(&Project::new("Nested"), NewAudioTrack::default()).unwrap();
    let (project, inner) = edits::add_bus(&project, NewBus::group(vec![track])).unwrap();
    let (project, outer) = edits::add_bus(&project, NewBus::group(vec![inner])).unwrap();
    let (project, unrelated) = edits::add_bus(&project, NewBus::group(vec![])).unwrap();
    let project = edits::set_track_prop(
        &project,
        &[inner, outer, unrelated],
        TrackProp::Folded(true),
    )
    .unwrap();
    let owner = AutomationOwner::Track(track);
    let (project, _) = edits::automation::add_lane(
        &project,
        owner,
        AutomationTarget::Volume,
        de::PARAM_VOLUME.to_owned(),
    )
    .unwrap();
    let project = edits::automation::set_shown(&project, owner, true).unwrap();
    let name = Lang::De.a11y_automation_lane(de::PARAM_VOLUME, &project.track(track).unwrap().name);
    let mut h = harness(App::headless(project));
    assert!(h.query_by_role_and_label(Role::Group, &name).is_none());

    h.state_mut()
        .dispatch(Action::Automation(Automation::ShowTarget {
            owner,
            replace: None,
            target: AutomationTarget::Volume,
            label: de::PARAM_VOLUME.to_owned(),
        }));
    h.run_steps(3);

    let revealed = h.state().project();
    assert!(!revealed.track(inner).unwrap().folded);
    assert!(!revealed.track(outer).unwrap().folded);
    assert!(revealed.track(unrelated).unwrap().folded);
    assert!(
        h.query_all_by_role_and_label(Role::Group, &name)
            .next()
            .is_some()
    );
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    let previous = h.state().project();
    assert!(previous.track(inner).unwrap().folded);
    assert!(previous.track(outer).unwrap().folded);
}

#[test]
fn opening_a_document_clears_automation_selection_even_with_reused_ids() {
    use komponist_app::ProjectAction;
    use komponist_app::project_io::save_project;
    use komponist_model::automation::{AutomationOwner, AutomationTarget, Point, Points};
    use komponist_model::edits::{self, NewAudioTrack};

    let (project, track) =
        edits::add_audio_track(&Project::new("Original"), NewAudioTrack::default()).unwrap();
    let owner = AutomationOwner::Track(track);
    let (project, lane) = edits::automation::add_lane(
        &project,
        owner,
        AutomationTarget::Volume,
        de::PARAM_VOLUME.to_owned(),
    )
    .unwrap();
    let project = edits::automation::set_shown(&project, owner, true).unwrap();
    let points = Points::new(vec![Point::linear(Tick(480), 0.4)]);
    let project =
        edits::automation::set_lane_points(&project, owner, lane, points.clone()).unwrap();
    let mut replacement = project.clone();
    replacement.name = "Replacement".to_owned();
    let dir = crate::common::scratch("automation-document");
    let path = dir.join("replacement.komponist");
    save_project(&path, &replacement).unwrap();
    let mut h = harness(App::headless(project));
    fn point<'a>(h: &'a Harness<'_, App>) -> egui_kittest::Node<'a> {
        h.get_all_by_role(Role::Slider)
            .find(|node| {
                node.accesskit_node()
                    .label()
                    .is_some_and(|label| label.starts_with("Punkt "))
            })
            .unwrap()
    }
    point(&h).click_accesskit();
    h.run_steps(2);
    assert_eq!(point(&h).accesskit_node().is_selected(), Some(true));

    h.state_mut()
        .dispatch(Action::Project(ProjectAction::OpenPath(path)));
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.state().project().name != replacement.name {
        assert!(Instant::now() < deadline);
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(2);

    assert_eq!(point(&h).accesskit_node().is_selected(), Some(false));
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    let opened = &h.state().project().track(track).unwrap().automation.lanes[0];
    assert_eq!(opened.id, lane);
    assert_eq!(opened.points, points);
}

#[test]
fn snapshot_of_arrangement_automation_in_german_and_english() {
    use komponist_app::views::automation::{Action as Automation, Source};
    use komponist_model::automation::{
        AutomationMode, AutomationOwner, AutomationTarget, Point, Points, Shape,
    };

    let mut results = SnapshotResults::new();
    for (lang, name, menu_name) in [
        (
            Lang::De,
            "automation-arrangement-de-1x",
            "automation-mode-menu-de-1x",
        ),
        (
            Lang::En,
            "automation-arrangement-en-1x",
            "automation-mode-menu-en-1x",
        ),
    ] {
        let mut app = reference_demo().with_lang(lang);
        app.ui_mut().layout.dock_open = false;
        let owner = AutomationOwner::Track(app.project().tracks[2].id);
        for (owner, target, label, mode) in [
            (
                owner,
                AutomationTarget::Volume,
                lang.param_volume(),
                AutomationMode::Touch,
            ),
            (
                owner,
                AutomationTarget::Pan,
                lang.param_pan(),
                AutomationMode::Off,
            ),
            (
                AutomationOwner::Master,
                AutomationTarget::Volume,
                lang.param_volume(),
                AutomationMode::Read,
            ),
        ] {
            app.dispatch(Action::Automation(Automation::ShowTarget {
                owner,
                replace: None,
                target: target.clone(),
                label: label.to_owned(),
            }));
            let id = komponist_app::views::automation::lanes(app.project(), owner)
                .unwrap()
                .lane(&target)
                .unwrap()
                .id;
            app.dispatch(Action::Automation(Automation::Points(
                Source::Lane(owner, id),
                Points::new(vec![
                    Point {
                        tick: Tick(0),
                        value: 0.7,
                        shape: Shape::Curve { tension: 0.4 },
                    },
                    Point {
                        tick: Tick(20_000),
                        value: 0.25,
                        shape: Shape::Hold,
                    },
                    Point::linear(Tick(28_000), 0.8),
                ]),
                "undo-automation-add-point",
            )));
            app.dispatch(Action::Automation(Automation::Mode(owner, id, mode)));
        }
        let mut h = harness(app);
        let snapshot = |h: &mut Harness<'_, App>, name| {
            for rect in h
                .query_all_by_role_and_label(Role::Status, lang.transport_cpu())
                .map(|n| n.rect())
                .collect::<Vec<_>>()
            {
                h.mask(rect.expand(2.0));
            }
            h.snapshot(name);
        };
        snapshot(&mut h, name);
        let pan_mode = lang.automation_mode_lane(lang.param_pan());
        h.get_by_role_and_label(Role::ComboBox, &pan_mode).click();
        h.run_steps(2);
        snapshot(&mut h, menu_name);
        h.get_by_role_and_label(Role::ListBoxOption, lang.automation_mode_latch())
            .click();
        h.run_steps(2);
        assert_eq!(
            komponist_app::views::automation::lanes(h.state().project(), owner)
                .unwrap()
                .lane(&AutomationTarget::Pan)
                .unwrap()
                .mode,
            AutomationMode::Latch,
        );
        results.extend_harness(&mut h);
    }
    results.unwrap();
}

#[test]
fn hiding_one_automation_lane_preserves_its_points_and_its_neighbour() {
    use komponist_app::views::automation::Action as Automation;
    use komponist_model::automation::{AutomationOwner, AutomationTarget};

    let mut app = demo();
    let track = &app.project().tracks[0];
    let owner = AutomationOwner::Track(track.id);
    let name = track.name.clone();
    let show = |target, label: &str| {
        Action::Automation(Automation::ShowTarget {
            owner,
            replace: None,
            target,
            label: label.to_owned(),
        })
    };
    app.dispatch(show(AutomationTarget::Volume, de::PARAM_VOLUME));
    app.dispatch(show(AutomationTarget::Pan, de::PARAM_PAN));
    let before = app.project().clone();
    let mut h = harness(app);
    let volume = format!("Automation {}, {name}", de::PARAM_VOLUME);
    let pan = format!("Automation {}, {name}", de::PARAM_PAN);
    let rect = rect_of(&h, Role::Group, &volume);
    right_click_menu(
        pos2(rect.left() + 22.0, rect.bottom() - 6.0),
        de::AUTOMATION_HIDE_LANE,
        &mut h,
    );
    assert!(h.query_by_role_and_label(Role::Group, &volume).is_none());
    assert!(
        h.query_all_by_role_and_label(Role::Group, &pan)
            .next()
            .is_some()
    );
    assert_eq!(h.state().project(), &before);

    h.state_mut()
        .dispatch(show(AutomationTarget::Volume, de::PARAM_VOLUME));
    h.run_steps(2);
    let canvas = h
        .get_all_by_role_and_label(Role::Group, &volume)
        .map(|node| node.rect())
        .max_by(|a, b| a.left().total_cmp(&b.left()))
        .unwrap();
    h.event(Event::PointerMoved(canvas.center()));
    h.run_steps(2);
    assert!(h.state().ui().automation_hint);
    h.event(Event::PointerMoved(pos2(0.0, 0.0)));
    h.run_steps(2);
    assert!(!h.state().ui().automation_hint);
}
