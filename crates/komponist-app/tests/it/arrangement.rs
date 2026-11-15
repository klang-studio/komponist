//! The arrangement (WP-40) through `egui_kittest` in the whole app: one test per row of the
//! design system's `ix.header` and `ix.clip` (and the ruler, zoom, S10 and drop rows), the
//! snapshot of the demo project next to S01, and the frame time with 200 clips / 10 000 notes.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::accesskit::Role;
use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions, SnapshotResults};
use komponist_app::actions::{SelectMode, SelectionChange};
use komponist_app::device_host::{BuiltinHost, DeviceHost};
use komponist_app::ui_state::FocusArea;
use komponist_app::views::arrangement::names;
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_engine::Processor;
use komponist_model::fixtures::demo_project;
use komponist_model::{ClipId, Device, DeviceId, DeviceRole, Project, Tick, TrackId};
use komponist_plugins::{DeviceStatus, PluginManagerEvent, ProcessorSetup};

const W: f32 = 1920.0;
const H: f32 = 1080.0;

fn snapshot_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

/// The built-in host, but every device reports `Ok` (the screens show the third-party
/// instruments as installed).
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

fn demo() -> App {
    App::headless(demo_project())
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
    let ruler = rect_of(h, Role::Slider, names::RULER);
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
    let mut app = demo().with_device_host(Box::new(AllInstalled(BuiltinHost::new())));
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

// ------------------------------------------------------------------------------------------------
// ix.header

/// A point on the header's text, away from the toggles and the meter.
fn on_header(r: Rect) -> Pos2 {
    pos2(r.left() + 40.0, r.center().y)
}

fn lead_header(h: &Harness<'_, App>) -> Rect {
    header_rect(h, "Spur Lead, hexa")
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
    let node = h.get_by_role_and_label(Role::Row, "Spur Lead, hexa");
    assert_eq!(node.accesskit_node().is_selected(), Some(true));
}

#[test]
fn shift_click_selects_a_range_and_ctrl_click_adds_one() {
    let mut h = harness(demo());
    let ids: Vec<TrackId> = (0..6).map(|i| track_id(&h, i)).collect();
    click(on_header(header_rect(&h, "Spur Bass, hexa")), &mut h);
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
    h.get_by_role_and_label(Role::TextInput, names::RENAME);
    h.event(Event::Text("Melodie".into()));
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().project().tracks[2].name, "Melodie");
    assert!(
        h.query_by_role_and_label(Role::TextInput, names::RENAME)
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

#[test]
fn dragging_a_header_reorders_tracks() {
    let mut h = harness(demo());
    let ids: Vec<TrackId> = (0..6).map(|i| track_id(&h, i)).collect();
    let drums = header_rect(&h, "Spur Drums, Sampler");
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
    let grip = rect_of(&h, Role::Splitter, &names::height_grip("Drums"));
    drag(grip.center(), grip.center() + vec2(0.0, 40.0), &mut h);
    assert_eq!(h.state().project().tracks[0].height, 96);
    assert_eq!(h.state().project().tracks[1].height, 56);
    // Clamped to 160.
    let grip = rect_of(&h, Role::Splitter, &names::height_grip("Drums"));
    drag(grip.center(), grip.center() + vec2(0.0, 300.0), &mut h);
    assert_eq!(h.state().project().tracks[0].height, 160);
    // One undo step per drag.
    undo(&mut h);
    assert_eq!(h.state().project().tracks[0].height, 96);
    // Alt: every track.
    let grip = rect_of(&h, Role::Splitter, &names::height_grip("Bass"));
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
    h.get_by_role_and_label(Role::Row, &format!("Spur {}, hexa", p.tracks[6].name));
    // Entf deletes the selected track.
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert_eq!(h.state().project().tracks.len(), 6);
}

#[test]
fn an_instrument_dropped_on_the_empty_area_makes_an_armed_track() {
    let mut h = harness(demo());
    let below = lead_header(&h).left_bottom() + vec2(300.0, 300.0);
    drop_plugin(komponist_app::app::hexa_ref(), below, &mut h);
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
    drop_plugin(komponist_app::app::hexa_ref(), on_header(keys), &mut h);
    let p = project(&h);
    assert_eq!(p.tracks.len(), 6);
    let t = p.track(keys_id).unwrap();
    assert_eq!(t.channel.devices[0].plugin.name, "hexa");
    h.get_by_role_and_label(Role::Row, "Spur Keys, hexa");
}

/// A browser drag (WP-44): the payload is a `PluginRef` in egui's drag and drop, released at
/// `at`.
fn drop_plugin(plugin: komponist_model::PluginRef, at: Pos2, h: &mut Harness<'_, App>) {
    let from = pos2(100.0, 500.0);
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
    h.get_by_role_and_label(Role::Status, &names::clips_selected(1));
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
    assert_eq!(new.name, names::NEW_MIDI_CLIP);
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
fn ruler_top_sets_the_playhead_bottom_drags_the_loop() {
    let mut h = harness(demo());
    let ruler = rect_of(&h, Role::Slider, names::RULER);
    // Top half at bar 5 + a bit: on the beat.
    let x = x_of(&h, Tick(4 * BAR + 100));
    click(pos2(x, ruler.top() + 4.0), &mut h);
    assert!(step_until(&mut h, |a| a.engine().playhead() == Tick(4 * BAR)));
    assert_eq!(
        value_of(&h, Role::Slider, names::RULER).as_deref(),
        Some("5.1.000")
    );
    // Bottom half: from bar 3 to the end of bar 6.
    let y = ruler.bottom() - 3.0;
    let from = pos2(x_of(&h, Tick(2 * BAR + 500)), y);
    let to = pos2(x_of(&h, Tick(6 * BAR - 400)), y);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::ToggleLoop));
    assert!(!h.state().project().loop_on);
    drag(from, to, &mut h);
    let p = project(&h);
    assert!(p.loop_on);
    assert_eq!(
        (p.loop_range.start, p.loop_range.end),
        (Tick(2 * BAR), Tick(6 * BAR))
    );
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
    h.get_by_role_and_label(Role::Button, names::ZOOM_IN)
        .click();
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.px_per_bar, 90.0);
    assert!((x_of(&h, Tick(12 * BAR)) - before).abs() < 0.5);
    h.get_by_role_and_label(Role::Button, names::ZOOM_OUT)
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
    let ruler = rect_of(&h, Role::Slider, names::RULER);
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
        value_of(&h, Role::ComboBox, names::GRID).as_deref(),
        Some("1/2")
    );
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::CloseBracket);
    h.run_steps(2);
    assert_eq!(h.state().ui().arrangement.grid, komponist_model::Grid::Bar);
    // Snap off from the header.
    h.get_by_role_and_label(Role::Button, names::SNAP).click();
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
        images.push((grid, render_without_grid_labels(&mut h, names::GRID)));
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
        render_without_grid_labels(&mut harness(app), names::GRID)
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
        let mut app = demo();
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
    h.get_by_role_and_label(Role::Label, names::EMPTY_TITLE);
    h.get_by_role_and_label(Role::Label, names::EMPTY_TEXT);
    h.get_by_role_and_label(Role::Button, names::NEW_TRACK_BUTTON);
    h.get_by_label_contains("0 Spuren");
    h.get_by_role_and_label(Role::Button, names::OPEN_PROJECT);
    h.get_by_role_and_label(Role::Button, names::ADD_MIDI_TRACK)
        .click();
    h.run_steps(2);
    let p = project(&h);
    assert_eq!(p.tracks.len(), 1);
    assert!(p.tracks[0].arm);
    assert!(h.state().ui().selection.tracks.contains(&p.tracks[0].id));
    // The hint disappears with the first track.
    assert!(
        h.query_by_role_and_label(Role::Label, names::EMPTY_TITLE)
            .is_none()
    );
    // A double-click into the new track makes a one-bar clip and opens it (S10 "Leere Spur").
    let r = header_rect(&h, &format!("Spur {}, hexa", p.tracks[0].name));
    let at = pos2(x_of(&h, Tick(BAR / 2)), r.center().y);
    double_click(at, &mut h);
    let p = project(&h);
    assert_eq!(p.tracks[0].clips.len(), 1);
    assert_eq!(h.state().ui().editor.clip, Some(p.tracks[0].clips[0].id));
}

#[test]
fn snapshot_of_an_empty_project_next_to_s10() {
    let mut h = harness_at(
        App::headless(komponist_model::fixtures::empty_project()),
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
        rect_of(h, Role::Button, names::NEW_TRACK_BUTTON)
    }

    fn dialog_open(h: &Harness<'_, App>) -> bool {
        h.query_by_role_and_label(Role::Dialog, de::NEW_TRACK_TITLE).is_some()
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
        let sel = h.get_by_role_and_label(Role::ListBoxOption, "hexa");
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
        h.get_by_role_and_label(Role::Button, de::DIALOG_CANCEL).click();
        h.run_steps(3);
        assert!(!dialog_open(&h));
        assert_eq!(names_of(&h), before);
    }

    #[test]
    fn enter_and_space_on_the_focused_box_open_the_dialog() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let mut h = harness(demo());
            h.get_by_role_and_label(Role::Button, names::NEW_TRACK_BUTTON)
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
        drop_plugin(komponist_app::app::hexa_ref(), at, &mut dropped);
        assert!(!dialog_open(&dropped), "a drop does not open the dialog");

        let mut h = harness(demo());
        open_by_click(&mut h);
        h.get_by_role_and_label(Role::Button, de::NEW_TRACK_CREATE).click();
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
        assert_eq!(made.channel.devices[0].plugin.name, "hexa");
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
        h.get_by_role_and_label(Role::Button, de::NEW_TRACK_CREATE).click();
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
            h.query_by_role_and_label(Role::ListBoxOption, "hexa")
                .is_some()
        );
        h.get_by_role_and_label(Role::ListBoxOption, de::NEW_TRACK_NO_INSTRUMENT);
        type_in(&mut h, de::NEW_TRACK_SEARCH, "sampl");
        assert!(
            h.query_by_role_and_label(Role::ListBoxOption, "hexa")
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
        let mut h = harness(demo());
        let at = new_track_box(&h).center();
        h.event(Event::PointerMoved(at));
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot("arrangement-new-track-hover-1x");
        results.extend_harness(&mut h);
        let mut h = harness(demo());
        open_by_click(&mut h);
        h.run_steps(3);
        mask_cpu(&mut h);
        h.snapshot("new-track-dialog-1x");
        results.extend_harness(&mut h);
        results.unwrap();
    }
}
