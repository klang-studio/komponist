//! Acceptance flow 2 (HANDOFF §9): record a clip (R, MIDI keyboard or computer keyboard) and edit
//! it in the piano roll (drag, shorten, Q, pencil, velocity lane).
//!
//! The app runs on the null backend with the engine held by the test (`EngineSync::hold_engine`):
//! time passes only in `run_held`, and MIDI arrives at exact frames of it, so positions do not
//! depend on wall-clock time or machine load (48 kHz, 120 bpm: 25 frames per tick, 250 ms is 480
//! ticks). The real-time input path (`TestMidiSource`, `MidiIn::frame_of`) is covered in
//! komponist-audio; the edits afterwards are exact.

use crate::acceptance_support;

use acceptance_support::*;
use egui::accesskit::Role;
use egui::{Key, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use komponist_app::App;
use komponist_app::ui_state::Tool;
use komponist_app::views::piano_roll::geometry::Geometry;
use komponist_engine::{MidiInEvent, MidiPort, TransportState};
use komponist_i18n::de;
use komponist_model::fixtures::empty_project;
use komponist_model::{ClipContent, ClipId, Note, Project, Tick};
use komponist_tokens::size;

const FRAMES_PER_MS: usize = 48;

fn on(key: u8, velocity: u8) -> [u8; 3] {
    [0x90, key, velocity]
}

fn off(key: u8) -> [u8; 3] {
    [0x80, key, 0]
}

/// Runs the held engine for `ms` of audio time with `midi` (ms offsets from now, sorted), then
/// one app frame.
fn run(h: &mut Harness<'_, App>, ms: usize, midi: &[(usize, [u8; 3])]) {
    let events: Vec<MidiInEvent> = midi
        .iter()
        .map(|&(at, data)| MidiInEvent {
            frame: (at * FRAMES_PER_MS) as u32,
            port: MidiPort(1),
            data,
        })
        .collect();
    assert!(
        h.state_mut()
            .engine_mut()
            .run_held(ms * FRAMES_PER_MS, &events, 0)
    );
    h.step();
}

/// An empty project with an armed hexa track (Strg T) and the count-in switched off by its
/// transport button; the dock tall enough for the piano roll tests.
fn rig() -> Harness<'static, App> {
    let mut app = App::headless(empty_project());
    app.ui_mut().layout.dock_height = 500.0;
    let mut h = harness(app);
    ctrl(&mut h, Key::T);
    assert!(h.state().project().tracks[0].arm);
    assert_eq!(h.state().project().count_in_bars, 1);
    click(&mut h, Role::Button, de::TRANSPORT_COUNT_IN);
    assert_eq!(h.state().project().count_in_bars, 0, "count-in off");
    h.state_mut()
        .engine_mut()
        .hold_engine()
        .expect("engine held");
    h
}

fn start_recording(h: &mut Harness<'_, App>) {
    key(h, Key::R);
    run(h, 40, &[]);
    assert_eq!(h.state().transport(), TransportState::Recording);
}

fn stop_recording(h: &mut Harness<'_, App>) -> ClipId {
    key(h, Key::Space);
    run(h, 10, &[]);
    assert_eq!(h.state().transport(), TransportState::Stopped);
    assert!(h.state().recording().is_none());
    let p = h.state().project();
    assert_eq!(p.tracks[0].clips.len(), 1, "one clip for the take");
    p.tracks[0].clips[0].id
}

fn notes_of(app: &App, clip: ClipId) -> Vec<Note> {
    let (_, c) = app.project().find_clip(clip).unwrap();
    match &c.content {
        ClipContent::Midi(m) => m.active().notes.to_vec(),
        ClipContent::Step(_) | ClipContent::Audio(_) => unreachable!(),
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
        // 250 ms held = 480 ticks, exactly.
        assert_eq!(n.length, Tick(480), "length of note {}", n.pitch);
    }
    // The second note starts 350 ms (672 ticks) after the first.
    assert_eq!(sorted[1].start, sorted[0].start + Tick(672));
}

#[test]
fn midi_keyboard_records_and_the_piano_roll_edits_the_take() {
    let mut h = rig();
    start_recording(&mut h);
    run(&mut h, 100, &[]);
    run(&mut h, 250, &[(0, on(60, 100))]);
    run(&mut h, 100, &[(0, off(60))]);
    run(&mut h, 250, &[(0, on(64, 90))]);
    run(&mut h, 100, &[(0, off(64))]);
    let clip = stop_recording(&mut h);
    assert_recorded(h.state().project(), clip, [60, 64]);
    assert_eq!(h.state().undo_text().as_deref(), Some("Take 1"));
    let velocities: Vec<u8> = notes_of(h.state(), clip)
        .iter()
        .map(|n| n.velocity)
        .collect();
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
    drag(
        &mut h,
        edge,
        edge - vec2(g.axis.width(Tick(240)) * 1.0, 0.0),
    );
    let shorter = note(h.state(), clip, 64);
    assert!(
        shorter.length < e4.length,
        "{:?} → {:?}",
        e4.length,
        shorter.length
    );
    assert_eq!(shorter.start, e4.start);

    // Q: select all, quantize; every start sits on the grid afterwards.
    ctrl(&mut h, Key::A);
    key(&mut h, Key::Q);
    for n in notes_of(h.state(), clip) {
        assert_eq!(n.start.0 % 240, 0, "start {:?} on the grid", n.start);
    }
    assert_eq!(h.state().undo_text().as_deref(), Some("Quantisieren"));

    // Pencil: a click on an empty spot sets a grid note.
    click(&mut h, Role::RadioButton, de::PIANO_ROLL_PENCIL);
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
    assert_eq!(h.state().undo_text().as_deref(), Some("Velocity"));
}

#[test]
fn computer_keyboard_records_a_take() {
    let mut h = rig();
    start_recording(&mut h);
    run(&mut h, 100, &[]);
    h.key_down(Key::A);
    h.step();
    run(&mut h, 250, &[]);
    h.key_up(Key::A);
    h.step();
    run(&mut h, 100, &[]);
    h.key_down(Key::K);
    h.step();
    run(&mut h, 250, &[]);
    h.key_up(Key::K);
    h.step();
    run(&mut h, 100, &[]);
    let clip = stop_recording(&mut h);
    // A is C4 (60), K is C5 (72) like in the recording tests.
    assert_recorded(h.state().project(), clip, [60, 72]);
}
