//! Acceptance flow 2 (HANDOFF §9): record a clip (R, MIDI keyboard or computer keyboard) and edit
//! it in the piano roll (drag, shorten, Q, pencil, velocity lane).
//!
//! The app runs on the null backend in real time. MIDI comes through a `TestMidiSource`, the same
//! queue a midir port feeds, so the notes take the real input path including `MidiIn::frame_of`.
//! Positions are therefore wall-clock dependent and asserted within bounds (120 bpm: 1 s is
//! 1920 ticks); the edits afterwards are exact.

mod acceptance_support;

use std::time::{Duration, Instant};

use acceptance_support::*;
use egui::accesskit::Role;
use egui::{Key, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use komponist_app::App;
use komponist_app::panels::transport::names as tp;
use komponist_app::ui_state::Tool;
use komponist_app::views::piano_roll::geometry::Geometry;
use komponist_app::views::piano_roll::names as pr;
use komponist_audio::{MidiPortId, TestMidiSource};
use komponist_engine::TransportState;
use komponist_model::fixtures::empty_project;
use komponist_model::{ClipContent, ClipId, Note, Project, Tick};
use komponist_tokens::size;

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

const PORT: MidiPortId = MidiPortId(1);

/// Steps the app for `ms` of wall-clock time (the null stream runs in real time).
fn wait(h: &mut Harness<'_, App>, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
}

fn wait_for(h: &mut Harness<'_, App>, what: &str, mut done: impl FnMut(&App) -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done(h.state()) {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
}

/// An empty project with an armed pisynth track (Strg T) and the count-in switched off by its
/// transport button; the dock tall enough for the piano roll tests.
fn rig() -> (Harness<'static, App>, TestMidiSource) {
    let (source, receiver) = TestMidiSource::new();
    let mut app = App::headless_with_midi(empty_project(), receiver);
    app.ui_mut().layout.dock_height = 500.0;
    let mut h = harness(app);
    ctrl(&mut h, Key::T);
    assert!(h.state().project().tracks[0].arm);
    assert_eq!(h.state().project().count_in_bars, 1);
    click(&mut h, Role::Button, tp::COUNT_IN);
    assert_eq!(h.state().project().count_in_bars, 0, "count-in off");
    (h, source)
}

fn start_recording(h: &mut Harness<'_, App>) {
    key(h, Key::R);
    wait_for(h, "recording", |a| a.transport() == TransportState::Recording);
}

fn stop_recording(h: &mut Harness<'_, App>) -> ClipId {
    key(h, Key::Space);
    wait_for(h, "stop", |a| a.transport() == TransportState::Stopped);
    assert!(h.state().recording().is_none());
    let p = h.state().project();
    assert_eq!(p.tracks[0].clips.len(), 1, "one clip for the take");
    p.tracks[0].clips[0].id
}

fn notes_of(app: &App, clip: ClipId) -> Vec<Note> {
    let (_, c) = app.project().find_clip(clip).unwrap();
    match &c.content {
        ClipContent::Midi(m) => m.active().notes.to_vec(),
        ClipContent::Step(_) => unreachable!(),
    }
}

fn geometry(h: &Harness<'_, App>, clip: ClipId) -> Geometry {
    let app = h.state();
    let (_, c) = app.project().find_clip(clip).unwrap();
    let name = format!("{} · Piano Roll", c.name);
    let top = h.get_by_role_and_label(Role::Tab, &name).rect().bottom();
    let l = app.ui().layout;
    let body = egui::Rect::from_min_max(
        pos2(l.browser_width + 1.0, top),
        pos2(W - l.inspector_width - 1.0, H - size::STATUSBAR - 1.0),
    );
    Geometry::new(body, &app.ui().piano_roll, c.start, app.project().meter)
}

fn note(app: &App, clip: ClipId, pitch: u8) -> Note {
    *notes_of(app, clip)
        .iter()
        .find(|n| n.pitch == pitch)
        .unwrap_or_else(|| panic!("no note {pitch}"))
}

fn assert_recorded(p: &Project, clip: ClipId, pitches: [u8; 2]) {
    let (_, c) = p.find_clip(clip).unwrap();
    let ClipContent::Midi(m) = &c.content else {
        panic!("no MIDI clip")
    };
    let notes = &m.active().notes;
    assert_eq!(notes.len(), 2);
    let mut sorted: Vec<&Note> = notes.iter().collect();
    sorted.sort_by_key(|n| n.start);
    assert_eq!(sorted[0].pitch, pitches[0]);
    assert_eq!(sorted[1].pitch, pitches[1]);
    for n in &sorted {
        // 250 ms held = about 480 ticks.
        assert!((300..=900).contains(&n.length.0), "length {:?}", n.length);
    }
    assert!(sorted[1].start >= sorted[0].start + sorted[0].length);
}

#[test]
fn midi_keyboard_records_and_the_piano_roll_edits_the_take() {
    let (mut h, midi) = rig();
    start_recording(&mut h);
    wait(&mut h, 100);
    assert!(midi.note_on(PORT, 60, 100));
    wait(&mut h, 250);
    assert!(midi.note_off(PORT, 60));
    wait(&mut h, 100);
    assert!(midi.note_on(PORT, 64, 90));
    wait(&mut h, 250);
    assert!(midi.note_off(PORT, 64));
    wait(&mut h, 100);
    let clip = stop_recording(&mut h);
    assert_recorded(h.state().project(), clip, [60, 64]);
    assert_eq!(h.state().doc().undo_label(), Some("Take 1"));
    let velocities: Vec<u8> = notes_of(h.state(), clip).iter().map(|n| n.velocity).collect();
    assert!(velocities.contains(&100) && velocities.contains(&90));

    // --- Edit in the piano roll (the take is open in the editor)
    let g = geometry(&h, clip);
    let c4 = note(h.state(), clip, 60);

    // Drag: C4 up two semitones.
    let from = g.note_rect(c4.start, c4.length, 60).center();
    let to = from - vec2(0.0, g.row_top(60) - g.row_top(62));
    drag(&mut h, from, pos2(from.x, to.y));
    assert!(notes_of(h.state(), clip).iter().any(|n| n.pitch == 62));
    assert!(notes_of(h.state(), clip).iter().all(|n| n.pitch != 60));

    // Shorten: pull E4's right edge two grid steps (480 ticks) to the left.
    let e4 = note(h.state(), clip, 64);
    let r = g.note_rect(e4.start, e4.length, 64);
    let edge = pos2(r.right() - 2.0, r.center().y);
    drag(&mut h, edge, edge - vec2(g.axis.width(Tick(240)) * 1.0, 0.0));
    let shorter = note(h.state(), clip, 64);
    assert!(shorter.length < e4.length, "{:?} → {:?}", e4.length, shorter.length);
    assert_eq!(shorter.start, e4.start);

    // Q: select all, quantize; every start sits on the grid afterwards.
    ctrl(&mut h, Key::A);
    key(&mut h, Key::Q);
    for n in notes_of(h.state(), clip) {
        assert_eq!(n.start.0 % 240, 0, "start {:?} on the grid", n.start);
    }
    assert_eq!(h.state().doc().undo_label(), Some("Quantisieren"));

    // Pencil: a click on an empty spot sets a grid note.
    click(&mut h, Role::RadioButton, pr::PENCIL);
    assert_eq!(h.state().ui().piano_roll.tool, Tool::Pencil);
    let g = geometry(&h, clip);
    let before = notes_of(h.state(), clip).len();
    let at = pos2(g.x_of_local(Tick(240)) + 3.0, g.row_top(72) + 4.0);
    click_at(&mut h, at);
    let notes = notes_of(h.state(), clip);
    assert_eq!(notes.len(), before + 1);
    let new = notes.iter().find(|n| n.pitch == 72).expect("a note on C5");
    assert_eq!((new.start, new.length), (Tick(240), Tick(240)));

    // Velocity lane: a click at the new note's stem sets its velocity.
    let lane = g.lane.expect("velocity lane");
    let stem = pos2(g.x_of_local(new.start) + 2.0, lane.bottom() - 4.0 - 26.0);
    click_at(&mut h, stem);
    assert_eq!(note(h.state(), clip, 72).velocity, 64);
    assert_eq!(h.state().doc().undo_label(), Some("Velocity"));
}

#[test]
fn computer_keyboard_records_a_take() {
    let (mut h, _) = rig();
    start_recording(&mut h);
    wait(&mut h, 100);
    h.key_down(Key::A);
    h.step();
    wait(&mut h, 250);
    h.key_up(Key::A);
    h.step();
    wait(&mut h, 100);
    h.key_down(Key::K);
    h.step();
    wait(&mut h, 250);
    h.key_up(Key::K);
    h.step();
    wait(&mut h, 100);
    let clip = stop_recording(&mut h);
    // A is C4 (60), K is C5 (72) like in the recording tests.
    assert_recorded(h.state().project(), clip, [60, 72]);
}
