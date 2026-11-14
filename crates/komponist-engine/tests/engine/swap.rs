//! Program swaps, stop, locate and the active-notes table.

use komponist_engine::testing::{EventLog, Logged, TestSine};
use komponist_engine::{Command, EngineEvent, EventKind, Program};
use komponist_model::{Tempo, TempoMap, Tick, TrackId};

use crate::common::*;

/// A rig playing `p` with a logged `TestSine` in slot 0.
fn playing(p: &Program) -> (Rig, EventLog) {
    let mut rig = Rig::new(RATE, 512);
    let (sine, log) = TestSine::logged(1 << 16);
    rig.insert(0, sine);
    rig.set_program(p);
    rig.send(Command::Play);
    (rig, log)
}

/// Note events of key 60 as `(frame, on/off)`.
fn key60(log: &[Logged]) -> Vec<(u64, &'static str)> {
    notes(log)
        .into_iter()
        .filter(|n| n.2 == 60)
        .map(|n| (n.0, n.1))
        .collect()
}

#[test]
fn program_swap_during_a_held_note_gives_exactly_one_note_off() {
    // A two-bar note (120 bpm: bar = 96 000 samples), swapped after one bar.
    let held = note(0, 2 * BAR, 60);
    let before = program(120.0, vec![track(1, 0, vec![held])]);
    let variants: [(&str, Vec<_>, u64); 5] = [
        ("unchanged", vec![held], 192_000),
        ("note removed", vec![], 192_000),
        ("note longer", vec![note(0, 4 * BAR, 60)], 192_000),
        ("note shorter", vec![note(0, BAR / 2, 60)], 192_000),
        ("other notes", vec![note(BAR + 100, 50, 72)], 192_000),
    ];
    for (name, notes_after, off) in variants {
        let (mut rig, mut log) = playing(&before);
        rig.process(96_000, 1000);
        let after = program(120.0, vec![track(1, 0, notes_after)]);
        rig.set_program(&after);
        rig.process(4 * 96_000, 1000);
        assert_eq!(key60(&drain(&mut log)), [(0, "on"), (off, "off")], "{name}");
        // The old programs came back as garbage.
        assert_eq!(rig.garbage, 1, "{name}");
    }
}

#[test]
fn program_swap_with_a_tempo_change_keeps_the_scheduled_note_off() {
    let before = program(120.0, vec![track(1, 0, vec![note(0, 2 * BAR, 60)])]);
    let (mut rig, mut log) = playing(&before);
    rig.process(96_000, 1000);
    // Twice as fast: bar 2 is now at 48 000 samples, the note's end (bar 3) at 96 000.
    let mut after = before.clone();
    after.tempo_map = TempoMap::constant(Tempo::new(240.0), after.tempo_map.meter);
    rig.set_program(&after);
    rig.process(1, 1);
    assert_eq!(rig.handle.telemetry().playhead(), Tick(BAR));
    rig.process(96_000, 1000);
    assert_eq!(
        key60(&drain(&mut log)),
        [(0, "on"), (96_000 + 48_000, "off")]
    );
}

#[test]
fn many_swaps_never_cut_or_double_a_note() {
    let notes_in = vec![note(0, 2 * BAR, 60), note(BAR / 2, BAR, 64)];
    let p = program(120.0, vec![track(1, 0, notes_in)]);
    let (mut rig, mut log) = playing(&p);
    for _ in 0..200 {
        rig.set_program(&p);
        rig.process(1000, 333);
    }
    let got: Vec<_> = notes(&drain(&mut log))
        .into_iter()
        .map(|n| (n.0, n.1, n.2))
        .collect();
    assert_eq!(
        got,
        [
            (0, "on", 60),
            (48_000, "on", 64),
            (144_000, "off", 64),
            (192_000, "off", 60)
        ]
    );
    assert_eq!(rig.garbage, 200);
}

#[test]
fn removed_track_and_replaced_instrument_get_no_stray_note_off() {
    let p = program(120.0, vec![track(1, 0, vec![note(0, 2 * BAR, 60)])]);
    let (mut rig, mut log) = playing(&p);
    rig.process(1000, 1000);
    // The track's instrument moves to slot 1 (a new device): the note in slot 0 is left alone
    // (its processor is removed next), slot 1 gets nothing for it.
    let (sine, mut log1) = TestSine::logged(1 << 12);
    rig.insert(1, sine);
    rig.set_program(&program(120.0, vec![track(1, 1, vec![])]));
    rig.send(Command::RemoveProcessor {
        slot: komponist_engine::SlotId(0),
    });
    rig.process(200_000, 1000);
    assert_eq!(key60(&drain(&mut log)), [(0, "on")]);
    assert_eq!(key60(&drain(&mut log1)), []);
    // Program and processor came back.
    assert_eq!(rig.garbage, 2);
    // Without the track, nothing is played and nothing breaks.
    rig.set_program(&program(120.0, vec![]));
    rig.process(10_000, 1000);
    assert!(rig.l[rig.frames() - 10_000..].iter().all(|&x| x == 0.0));
}

#[test]
fn stop_ends_notes_resets_and_keeps_the_position() {
    let p = program(120.0, vec![track(1, 0, vec![note(0, 2 * BAR, 60)])]);
    let (mut rig, mut log) = playing(&p);
    rig.process(10_000, 1000);
    let live = Command::LiveNote {
        track: TrackId(1),
        key: 67,
        velocity: 100,
    };
    rig.send(live);
    rig.process(1000, 1000);
    rig.send(Command::Stop);
    rig.process(1000, 1000);
    let log = drain(&mut log);
    // Reset first (at the command), then note-offs for both notes and AllNotesOff at offset 0.
    let tail: Vec<_> = log
        .iter()
        .skip_while(|e| !matches!(e, Logged::Reset { .. }))
        .filter(|e| !matches!(e, Logged::Call { .. }))
        .map(|e| match *e {
            Logged::Reset { frame } => (frame, "reset", 0),
            Logged::Event {
                frame,
                kind: EventKind::NoteOff { key, .. },
            } => (frame, "off", key),
            Logged::Event {
                frame,
                kind: EventKind::AllNotesOff,
            } => (frame, "all", 0),
            other => panic!("{other:?}"),
        })
        .collect();
    let mut offs = tail[1..3].to_vec();
    offs.sort();
    assert_eq!(tail[0], (11_000, "reset", 0));
    assert_eq!(offs, [(11_000, "off", 60), (11_000, "off", 67)]);
    assert_eq!(tail[3..], [(11_000, "all", 0)]);
    let t = rig.handle.telemetry();
    assert_eq!(t.transport(), komponist_engine::TransportState::Stopped);
    assert_eq!(t.playhead(), Tick(11_000 / 25));
    // Stopped and reset: silence.
    assert!(rig.l[11_000..].iter().all(|&x| x == 0.0));
}

#[test]
fn locate_while_playing_ends_notes_and_plays_on_from_there() {
    let p = program(
        120.0,
        vec![track(1, 0, vec![note(0, 2 * BAR, 60), note(BAR, 100, 62)])],
    );
    let (mut rig, mut log) = playing(&p);
    rig.process(10_000, 1000);
    rig.send(Command::Locate(Tick(BAR)));
    rig.process(10_000, 1000);
    let got: Vec<_> = notes(&drain(&mut log))
        .into_iter()
        .map(|n| (n.0, n.1, n.2))
        .collect();
    // The held note ends at the locate; the note at the new position starts on its first frame,
    // and the long note sounding across the new position is chased (it starts again there).
    assert_eq!(
        got,
        [
            (0, "on", 60),
            (10_000, "off", 60),
            (10_000, "on", 60),
            (10_000, "on", 62),
            (12_500, "off", 62)
        ]
    );
    assert!(
        !rig.take_events()
            .iter()
            .any(|e| matches!(e, EngineEvent::LoopWrapped { .. }))
    );
}

#[test]
fn panic_ends_all_notes_without_reset() {
    let p = program(120.0, vec![track(1, 0, vec![note(0, 2 * BAR, 60)])]);
    let (mut rig, mut log) = playing(&p);
    rig.process(1000, 1000);
    rig.send(Command::AllNotesOff);
    rig.process(1000, 1000);
    let log = drain(&mut log);
    assert!(!log.iter().any(|e| matches!(e, Logged::Reset { .. })));
    assert!(log.contains(&Logged::Event {
        frame: 1000,
        kind: EventKind::AllNotesOff
    }));
    assert_eq!(key60(&log), [(0, "on"), (1000, "off")]);
    // Playback goes on.
    assert_eq!(
        rig.handle.telemetry().transport(),
        komponist_engine::TransportState::Playing
    );
}

#[test]
fn probability_is_reproducible_per_playback_start() {
    let notes_in: Vec<_> = (0..64)
        .map(|i| komponist_engine::program::SeqNote {
            probability: 50,
            ..note(i * 120, 60, 60)
        })
        .collect();
    let p = program(120.0, vec![track(1, 0, notes_in)]);
    let run = |seed: u32| {
        let mut rig = Rig::with_config(komponist_engine::EngineConfig {
            seed,
            ..Default::default()
        });
        let (sine, mut log) = TestSine::logged(1 << 14);
        rig.insert(0, sine);
        rig.set_program(&p);
        let mut passes = Vec::new();
        for _ in 0..2 {
            rig.send(Command::Play);
            rig.render(200_000, 2048);
            rig.send(Command::Stop);
            rig.send(Command::Locate(Tick(0)));
            rig.render(16, 16);
            let start = passes.len() as u64 * 200_016;
            let ons: Vec<u64> = key60(&drain(&mut log))
                .into_iter()
                .filter(|n| n.1 == "on")
                .map(|n| n.0 - start)
                .collect();
            passes.push(ons);
        }
        passes
    };
    let a = run(1);
    assert_eq!(a[0], a[1], "reseeded at every playback start");
    assert!((16..48).contains(&a[0].len()), "{}", a[0].len());
    assert_eq!(a, run(1));
    assert_ne!(a[0], run(2)[0]);
}
