//! Recording through the app (WP-51, S02): R, count-in, the growing take, Stop as one undo step
//! "Take n", loop takes, Esc, and the computer keyboard.
//!
//! The test holds the app's engine (`EngineSync::hold_engine`) and runs it itself with simulated
//! MIDI input at exact frames (`EngineSync::run_held`), so recorded positions are exact. At 120 bpm
//! and 48 kHz one tick is 25 frames.

use std::path::PathBuf;
use std::time::Instant;

use egui::accesskit::{Role, Toggled};
use egui::{Modifiers, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, SnapshotOptions};
use komponist_app::panels::statusbar;
use komponist_app::ui_state::{DockTab, FocusArea};
use komponist_app::{Action, App, EditAction, TransportAction, UiAction};
use komponist_engine::{MidiInEvent, MidiPort, TransportState};
use komponist_i18n::{Lang, de};
use komponist_model::edits::{ChannelProp, TrackProp};
use komponist_model::fixtures::{demo_project, empty_project};
use komponist_model::{ClipContent, Project, Tick, TickRange, TrackId};

const BAR: i64 = 3840;

fn harness(app: App) -> Harness<'static, App> {
    let mut h = komponist_ui::snapshot::builder::<App>(1.0)
        .with_size(vec2(1920.0, 1080.0))
        .with_options(
            SnapshotOptions::new()
                .output_path(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")),
        )
        .with_max_steps(64)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(|ui, app: &mut App| app.show(ui), app);
    h.run_steps(2);
    h
}

/// `project` in the app with the engine held by the test.
fn held(project: Project) -> Harness<'static, App> {
    let mut app = App::headless(project);
    app.engine_mut().hold_engine().expect("engine held");
    harness(app)
}

/// An empty project (120 bpm) with one armed hexa track (Strg T), count-in off unless
/// `count_in`.
fn rig(count_in: bool) -> (Harness<'static, App>, TrackId) {
    let mut app = App::headless(empty_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    if !count_in {
        app.dispatch(Action::Transport(TransportAction::ToggleCountIn));
    }
    let track = app.project().tracks[0].id;
    assert!(app.project().tracks[0].arm && app.project().tracks[0].monitor);
    app.engine_mut().hold_engine().expect("engine held");
    (harness(app), track)
}

fn frames_per_tick(p: &Project) -> f64 {
    48_000.0 * 60.0 / (p.tempo.bpm * 960.0)
}

/// Runs the held engine for `ticks` with MIDI at tick offsets from now, heard `latency` ticks
/// late, then one app frame.
fn run_late(h: &mut Harness<'_, App>, ticks: i64, midi: &[(i64, [u8; 3])], latency: i64) {
    let fpt = frames_per_tick(h.state().project());
    let frame = |t: i64| (t as f64 * fpt).round() as u32;
    let events: Vec<MidiInEvent> = midi
        .iter()
        .map(|&(t, data)| MidiInEvent {
            frame: frame(t),
            port: MidiPort(1),
            data,
        })
        .collect();
    let engine = h.state_mut().engine_mut();
    assert!(engine.run_held(frame(ticks) as usize, &events, frame(latency)));
    h.step();
}

fn run(h: &mut Harness<'_, App>, ticks: i64, midi: &[(i64, [u8; 3])]) {
    run_late(h, ticks, midi, 0);
}

fn on(key: u8, velocity: u8) -> [u8; 3] {
    [0x90, key, velocity]
}

fn off(key: u8) -> [u8; 3] {
    [0x80, key, 0]
}

fn press(h: &mut Harness<'_, App>, key: egui::Key) {
    h.key_press(key);
    h.step();
}

/// (start, length, pitch, velocity) of take `take` of `track`'s clip `clip`.
fn take_notes(p: &Project, track: TrackId, clip: usize, take: usize) -> Vec<(i64, i64, u8, u8)> {
    let c = &p.track(track).unwrap().clips[clip];
    let ClipContent::Midi(m) = &c.content else {
        panic!("{} is no MIDI clip", c.name)
    };
    m.takes[take]
        .notes
        .iter()
        .map(|n| (n.start.0, n.length.0, n.pitch, n.velocity))
        .collect()
}

/// Labels and values of all AccessKit nodes (labels carry their text as value).
fn labels(h: &Harness<'_, App>) -> Vec<String> {
    h.query_all_by_label_contains("")
        .flat_map(|n| [n.accesskit_node().label(), n.accesskit_node().value()])
        .flatten()
        .collect()
}

fn status_left(h: &Harness<'_, App>) -> String {
    statusbar::model(h.state(), Instant::now()).left
}

#[test]
fn midi_input_becomes_take_1_in_one_undo_step() {
    let (mut h, track) = rig(false);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(BAR))));
    run(&mut h, 1, &[]);
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    let rec = h.state().recording().expect("recording");
    assert_eq!(rec.start(), Tick(BAR));
    assert_eq!(h.state().transport(), TransportState::Recording);

    // Simulated MIDI from 3880: C4 3940–4420, E4 from 4800, held.
    run(
        &mut h,
        1000,
        &[(60, on(60, 100)), (540, off(60)), (920, on(64, 80))],
    );
    assert_eq!(h.state().engine().playhead(), Tick(4880));
    // Live: the clip grows on the track with a red head, the editor shows the take.
    let live = h.state().recording().unwrap().clip(track).unwrap().clone();
    assert_eq!(live.name, "● Aufnahme · Take 1");
    assert_eq!((live.start, live.end()), (Tick(BAR), Tick(4881)));
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
    h.step();
    let all = labels(&h);
    let info = Lang::De.record_info(&h.state().project().tracks[0].name);
    for want in [
        "Clip Aufnahme · Take 1, Takt 2",
        "Aufnahme · Piano Roll · live",
        "Note C4, 2.1.025",
        "Note E4, 2.2.000",
        &info,
    ] {
        assert!(all.iter().any(|l| l.starts_with(want)), "{want}: {all:?}");
    }
    assert!(all.iter().any(|l| l == de::RECORD_LIVE), "{all:?}");
    assert!(status_left(&h).starts_with("● Aufnahme läuft · "));
    assert!(status_left(&h).ends_with(" · Take 1"));
    assert_eq!(h.state().ui().focus, FocusArea::PianoRoll);
    assert_eq!(h.state().ui().layout.dock_tab, DockTab::Editor);

    // Leertaste stops: one clip "Take 1" in track colour, selected, open in the editor.
    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    assert_eq!(h.state().transport(), TransportState::Stopped);
    assert!(h.state().recording().is_none());
    let p = h.state().project().clone();
    let clips = &p.track(track).unwrap().clips;
    assert_eq!(clips.len(), 1);
    let clip = &clips[0];
    assert_eq!(clip.name, "Take 1");
    assert_eq!((clip.start, clip.length), (Tick(BAR), Tick(BAR)));
    // Positions exact: the held note ends at the head (4880).
    assert_eq!(
        take_notes(&p, track, 0, 0),
        [(100, 480, 60, 100), (960, 80, 64, 80)]
    );
    let ui = h.state().ui();
    assert_eq!(ui.editor.clip, Some(clip.id));
    assert!(ui.selection.clips.contains(&clip.id) && ui.selection.clips.len() == 1);
    assert_eq!(ui.focus, FocusArea::PianoRoll);
    assert_eq!(h.state().undo_text().as_deref(), Some("Take 1"));
    assert_eq!(
        status_left(&h),
        "Take 1 aufgenommen · 2 Noten · Q quantisiert"
    );
    h.step();
    assert!(
        labels(&h)
            .iter()
            .any(|l| l.starts_with("Take 1 · Piano Roll")),
        "the editor tab shows the take"
    );

    // One undo removes the whole take.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
    assert_eq!(h.state().undo_text().as_deref(), Some("Neue MIDI-Spur"));
}

#[test]
fn the_computer_keyboard_records_like_midi_input() {
    let (mut h, track) = rig(false);
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    assert_eq!(h.state().recording().unwrap().start(), Tick::ZERO);
    // A from 40 to 540, K from 640, held until Stop at 840.
    h.key_down(egui::Key::A);
    h.step();
    run(&mut h, 500, &[]);
    h.key_up(egui::Key::A);
    h.step();
    run(&mut h, 100, &[]);
    h.key_down(egui::Key::K);
    h.step();
    run(&mut h, 200, &[]);
    let live = h.state().recording().unwrap().clip(track).unwrap().clone();
    let ClipContent::Midi(m) = &live.content else {
        panic!()
    };
    assert_eq!(m.active().notes.len(), 2, "keys appear at once");
    press(&mut h, egui::Key::Space);
    h.key_up(egui::Key::K);
    run(&mut h, 10, &[]);
    let p = h.state().project().clone();
    assert_eq!(
        take_notes(&p, track, 0, 0),
        [(40, 500, 60, 100), (640, 200, 72, 100)]
    );
}

#[test]
fn loop_recording_makes_a_take_per_lap() {
    let (mut h, track) = rig(false);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::SetLoopRange(
            TickRange::new(Tick::ZERO, Tick(BAR)),
        )));
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    // Lap 1: C4 480–960; at 3830 the loop is about to wrap.
    run(&mut h, 3790, &[(440, on(60, 100)), (920, off(60))]);
    assert_eq!(h.state().recording().unwrap().laps(), 1);
    // The wrap at 3840. G4 heard at 3824, arriving 26 ticks later (output latency) after the
    // wrap; released in lap 2. It belongs to lap 1 and ends at the loop end.
    run_late(&mut h, 200, &[(20, on(67, 90)), (100, off(67))], 26);
    assert_eq!(h.state().recording().unwrap().laps(), 2);
    assert_eq!(h.state().engine().playhead(), Tick(190));
    // Lap 2: E4 1440–1920.
    run(&mut h, 2000, &[(1250, on(64, 80)), (1730, off(64))]);
    let live = h.state().recording().unwrap().clip(track).unwrap().clone();
    assert_eq!(live.name, "● Aufnahme · Take 2");
    assert_eq!(live.end(), Tick(BAR), "the clip keeps the loop's length");

    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    let p = h.state().project().clone();
    let clips = &p.track(track).unwrap().clips;
    assert_eq!(clips.len(), 1, "both laps in the same clip");
    let ClipContent::Midi(m) = &clips[0].content else {
        panic!()
    };
    let takes: Vec<&str> = m.takes.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(takes, ["Take 1", "Take 2"]);
    assert_eq!(m.active_take, 1);
    assert_eq!((clips[0].start, clips[0].length), (Tick::ZERO, Tick(BAR)));
    assert_eq!(
        take_notes(&p, track, 0, 0),
        [(480, 480, 60, 100), (3824, 16, 67, 90)]
    );
    assert_eq!(take_notes(&p, track, 0, 1), [(1440, 480, 64, 80)]);
    // The clip is named after the take that plays.
    assert_eq!(clips[0].name, "Take 2");
    assert_eq!(h.state().undo_text().as_deref(), Some("Take 2"));
    assert_eq!(
        status_left(&h),
        "Take 2 aufgenommen · 1 Note · Q quantisiert"
    );

    // The inspector (WP-43) lists `takes` and picks one with `SelectTake`.
    let clip = clips[0].id;
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SelectTake { clip, take: 0 }));
    let ClipContent::Midi(m) = &h.state().project().find_clip(clip).unwrap().1.content else {
        panic!()
    };
    assert_eq!(m.active_take, 0);

    // One undo for the take choice, one for both takes.
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    h.state_mut().dispatch(Action::Edit(EditAction::Undo));
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
}

#[test]
fn esc_discards_the_take_and_playback_goes_on() {
    use komponist_app::views::track_macros::Action as Macro;

    let (mut h, track) = rig(false);
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    run(&mut h, 500, &[(100, on(60, 100)), (300, off(60))]);
    assert!(h.state().recording().is_some());
    h.state_mut()
        .dispatch(Action::Macro(Macro::ToggleMapping { track }));
    press(&mut h, egui::Key::Escape);
    assert!(h.state().recording().is_none());
    assert_eq!(h.state().ui().track_macros.mapping, Some(track));
    run(&mut h, 100, &[(10, on(62, 100))]);
    assert_eq!(h.state().transport(), TransportState::Playing);
    press(&mut h, egui::Key::Escape);
    assert_eq!(h.state().ui().track_macros.mapping, None);
    assert!(
        h.state().engine().playhead() > Tick(540),
        "playback goes on"
    );
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
    assert_eq!(h.state().undo_text().as_deref(), Some("Neue MIDI-Spur"));
    // Stopping afterwards writes nothing either.
    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
    assert_eq!(h.state().undo_text().as_deref(), Some("Neue MIDI-Spur"));
}

#[test]
fn the_count_in_shows_in_the_position_display() {
    let (mut h, _) = rig(true);
    press(&mut h, egui::Key::R);
    run(&mut h, 1000, &[]);
    h.step();
    assert_eq!(h.state().transport(), TransportState::CountIn);
    assert!(
        h.state().recording().is_none(),
        "no take before the count-in ends"
    );
    let position = |h: &Harness<'_, App>| {
        h.get_by_role_and_label(Role::Status, de::TRANSPORT_POSITION)
            .value()
            .unwrap_or_default()
    };
    assert_eq!(position(&h), "Vorzähler");
    assert_eq!(status_left(&h), "Vorzähler …");
    let record = h.get_by_role_and_label(Role::Button, de::TRANSPORT_RECORD);
    assert_eq!(record.accesskit_node().toggled(), Some(Toggled::True));
    // After one bar the take starts at 1.1.000.
    run(&mut h, BAR - 1000 + 100, &[]);
    h.step();
    assert_eq!(h.state().transport(), TransportState::Recording);
    assert_eq!(h.state().recording().unwrap().start(), Tick::ZERO);
    assert_eq!(position(&h), "1.1.025");
}

#[test]
fn r_during_the_count_in_cancels_and_stops_at_the_recording_start() {
    let (mut h, track) = rig(true);
    h.state_mut()
        .dispatch(Action::Transport(TransportAction::Locate(Tick(BAR + 500))));
    run(&mut h, 1, &[]);
    press(&mut h, egui::Key::R);
    run(&mut h, 1000, &[]);
    assert_eq!(h.state().transport(), TransportState::CountIn);
    // Prototype `toggleRec` with `recPend`: no take, stopped, back at `recAt` (bar 2).
    press(&mut h, egui::Key::R);
    run(&mut h, 10, &[]);
    assert_eq!(h.state().transport(), TransportState::Stopped);
    assert!(h.state().recording().is_none());
    assert_eq!(h.state().engine().playhead(), Tick(BAR));
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
    // Leertaste during the count-in does the same.
    press(&mut h, egui::Key::R);
    run(&mut h, 1000, &[]);
    assert_eq!(h.state().transport(), TransportState::CountIn);
    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    assert_eq!(h.state().transport(), TransportState::Stopped);
    assert_eq!(h.state().engine().playhead(), Tick(BAR));
    assert!(h.state().project().track(track).unwrap().clips.is_empty());
}

#[test]
fn r_while_recording_punches_out_and_playback_goes_on() {
    let (mut h, track) = rig(false);
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    run(&mut h, 500, &[(100, on(60, 100)), (300, off(60))]);
    press(&mut h, egui::Key::R);
    run(&mut h, 100, &[]);
    assert_eq!(h.state().transport(), TransportState::Playing);
    assert!(h.state().recording().is_none());
    assert!(h.state().engine().playhead() > Tick(540));
    let clips = &h.state().project().track(track).unwrap().clips;
    assert_eq!(clips.len(), 1);
    assert_eq!(clips[0].name, "Take 1");
}

#[test]
fn a_recording_without_notes_keeps_an_empty_clip() {
    let (mut h, track) = rig(false);
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    run(&mut h, 2000, &[]);
    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    let p = h.state().project().clone();
    let clips = &p.track(track).unwrap().clips;
    assert_eq!(clips.len(), 1, "unlike the prototype, no demo phrase");
    assert_eq!(clips[0].name, "Take 1");
    assert_eq!((clips[0].start, clips[0].length), (Tick::ZERO, Tick(BAR)));
    assert!(take_notes(&p, track, 0, 0).is_empty());
    assert_eq!(
        status_left(&h),
        "Take 1 aufgenommen · 0 Noten · Q quantisiert"
    );
}

#[test]
fn r_arms_the_selected_track_with_monitoring_or_asks_for_one() {
    let mut h = held(demo_project());
    press(&mut h, egui::Key::R);
    run(&mut h, 10, &[]);
    assert_eq!(h.state().transport(), TransportState::Stopped);
    assert_eq!(status_left(&h), de::RECORD_NO_TRACK);

    let lead = h.state().project().tracks[2].id;
    h.state_mut().dispatch(Action::Ui(UiAction::Select(
        komponist_app::actions::SelectionChange::Tracks {
            tracks: vec![lead],
            mode: komponist_app::actions::SelectMode::Replace,
        },
    )));
    press(&mut h, egui::Key::R);
    let t = h.state().project().track(lead).unwrap().clone();
    assert!(t.arm && t.monitor, "arming switches monitoring on");
    run(&mut h, 4000, &[]);
    assert_eq!(h.state().transport(), TransportState::Recording);
    assert_eq!(
        h.state().recording().unwrap().tracks().collect::<Vec<_>>(),
        [lead]
    );

    // Arming from the header (R) also switches monitoring on.
    let bass = h.state().project().tracks[1].id;
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![bass],
            prop: TrackProp::Arm(true),
        }));
    assert!(h.state().project().track(bass).unwrap().monitor);
}

#[test]
fn edits_and_undo_go_on_while_recording_and_deleted_tracks_drop_their_take() {
    let mut app = App::headless(empty_project());
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.dispatch(Action::Edit(EditAction::NewMidiTrack));
    app.dispatch(Action::Transport(TransportAction::ToggleCountIn));
    app.engine_mut().hold_engine().expect("engine held");
    let (other, track) = (app.project().tracks[0].id, app.project().tracks[1].id);
    let mut h = harness(app);
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    run(&mut h, 200, &[(60, on(60, 100))]);
    // An edit and its undo meanwhile: the take goes on.
    h.state_mut()
        .dispatch(Action::Edit(EditAction::SetTrackProp {
            tracks: vec![other],
            prop: TrackProp::Channel(ChannelProp::Mute(true)),
        }));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.step();
    assert!(!h.state().project().track(other).unwrap().channel.mute);
    run(&mut h, 200, &[(60, off(60))]);
    assert!(h.state().recording().is_some());
    assert_eq!(h.state().transport(), TransportState::Recording);
    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    let p = h.state().project().clone();
    assert_eq!(take_notes(&p, track, 0, 0), [(100, 200, 60, 100)]);
    assert_eq!(h.state().undo_text().as_deref(), Some("Take 1"));

    // The recorded track is deleted while recording: its take goes with it.
    press(&mut h, egui::Key::R);
    run(&mut h, 40, &[]);
    run(&mut h, 200, &[(60, on(62, 100))]);
    h.state_mut()
        .dispatch(Action::Edit(EditAction::RemoveTracks(vec![track])));
    press(&mut h, egui::Key::Space);
    run(&mut h, 10, &[]);
    assert!(h.state().recording().is_none());
    assert_eq!(h.state().undo_text().as_deref(), Some("Spuren löschen"));
}

/// S02: Lead armed and recording from bar 13 in the demo, the S02 phrase played in, the
/// playhead at 14.3 with three keys held.
#[test]
fn snapshot_next_to_s02() {
    let mut app = crate::common::reference_app(demo_project());
    let lead = app.project().tracks[2].clone();
    // S02 has no "Lead B" at bar 13, and the loop is off.
    app.dispatch(Action::Edit(EditAction::RemoveClips(vec![
        lead.clips[2].id,
    ])));
    app.dispatch(Action::Transport(TransportAction::ToggleLoop));
    app.dispatch(Action::Ui(UiAction::Select(
        komponist_app::actions::SelectionChange::Tracks {
            tracks: vec![lead.id],
            mode: komponist_app::actions::SelectMode::Replace,
        },
    )));
    app.dispatch(Action::Transport(TransportAction::Locate(Tick(12 * BAR))));
    app.engine_mut().hold_engine().expect("engine held");
    let mut h = harness(app);
    run(&mut h, 1, &[]);
    press(&mut h, egui::Key::R);
    // `Screens.dc.html` NOTES (sixteenths: start, length, key, velocity) up to the playhead at
    // 25.5 sixteenths after bar 13; notes still sounding there are held.
    const NOTES: [(i64, i64, u8, u8); 17] = [
        (0, 8, 60, 70),
        (0, 8, 64, 62),
        (0, 2, 67, 100),
        (2, 2, 69, 80),
        (4, 4, 71, 112),
        (8, 8, 62, 58),
        (8, 2, 69, 72),
        (10, 2, 67, 60),
        (12, 4, 64, 96),
        (16, 8, 57, 82),
        (16, 8, 60, 74),
        (16, 6, 72, 124),
        (22, 2, 71, 76),
        (24, 8, 55, 64),
        (24, 8, 59, 58),
        (24, 4, 67, 90),
        (28, 4, 69, 48),
    ];
    let head = 255 * 24; // 25.5 sixteenths
    // From the count-in start, one bar before bar 13.
    let mut midi = Vec::new();
    for (s, l, key, velocity) in NOTES {
        let (start, end) = (s * 240, (s + l) * 240);
        if start >= head {
            continue;
        }
        midi.push((BAR + start, on(key, velocity)));
        if end < head {
            midi.push((BAR + end, off(key)));
        }
    }
    midi.sort_by_key(|(t, _)| *t);
    run(&mut h, BAR + head, &midi);
    h.run_steps(2);
    assert_eq!(h.state().transport(), TransportState::Recording);
    let cpu = h
        .get_by_role_and_label(Role::Status, de::TRANSPORT_CPU)
        .rect();
    h.mask(cpu.expand(2.0));
    h.snapshot("record-s02-1x");
}
