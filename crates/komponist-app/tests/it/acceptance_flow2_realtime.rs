//! Smoke test for the real-time MIDI input path of flow 2: the null stream runs in real time and
//! MIDI comes through a `TestMidiSource`, the same queue a midir port feeds, so the notes take the
//! path including `MidiIn::frame_of`. The deterministic flow-2 tests live in `acceptance_flow2`.
//!
//! Only timing-independent facts are asserted (pitches, velocities, order, count); positions and
//! lengths only within very wide bounds, so machine load cannot make this flake.

use crate::acceptance_support;

use std::time::{Duration, Instant};

use acceptance_support::*;
use egui::Key;
use egui::accesskit::Role;
use egui_kittest::Harness;
use komponist_app::App;
use komponist_audio::{MidiPortId, TestMidiSource};
use komponist_engine::TransportState;
use komponist_i18n::de;
use komponist_model::fixtures::empty_project;
use komponist_model::{ClipContent, Note};

const PORT: MidiPortId = MidiPortId(1);

/// Steps the app for `ms` of wall-clock time.
fn wait(h: &mut Harness<'_, App>, ms: u64) {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
}

fn wait_for(h: &mut Harness<'_, App>, what: &str, mut done: impl FnMut(&App) -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !done(h.state()) {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
        h.step();
    }
}

#[test]
fn real_time_midi_input_records_the_played_notes() {
    let (midi, receiver) = TestMidiSource::new();
    let mut app = App::headless_with_midi(empty_project(), receiver);
    app.ui_mut().layout.dock_height = 500.0;
    let mut h = harness(app);
    ctrl(&mut h, Key::T);
    assert!(h.state().project().tracks[0].arm);
    click(&mut h, Role::Button, de::TRANSPORT_COUNT_IN);
    assert_eq!(h.state().project().count_in_bars, 0, "count-in off");

    key(&mut h, Key::R);
    wait_for(&mut h, "recording", |a| {
        a.transport() == TransportState::Recording
    });
    wait(&mut h, 100);
    assert!(midi.note_on(PORT, 60, 100));
    wait(&mut h, 250);
    assert!(midi.note_off(PORT, 60));
    wait(&mut h, 100);
    assert!(midi.note_on(PORT, 64, 90));
    wait(&mut h, 250);
    assert!(midi.note_off(PORT, 64));
    wait(&mut h, 100);
    key(&mut h, Key::Space);
    wait_for(&mut h, "stop", |a| a.transport() == TransportState::Stopped);

    let p = h.state().project();
    assert_eq!(p.tracks[0].clips.len(), 1, "one clip for the take");
    let ClipContent::Midi(m) = &p.tracks[0].clips[0].content else {
        panic!("no MIDI clip")
    };
    let mut notes: Vec<Note> = m.active().notes.to_vec();
    notes.sort_by_key(|n| n.start);
    let played: Vec<(u8, u8)> = notes.iter().map(|n| (n.pitch, n.velocity)).collect();
    assert_eq!(played, [(60, 100), (64, 90)]);
    for n in &notes {
        // Nominally 480 ticks; only reject nonsense under load.
        assert!((1..=19_200).contains(&n.length.0), "length {:?}", n.length);
    }
    assert!(notes[0].start < notes[1].start);
}
