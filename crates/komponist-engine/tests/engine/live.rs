//! Live input: MIDI routing, `LiveNote`, recording capture with latency correction.

use komponist_engine::program::{InputFilter, MidiPort};
use komponist_engine::testing::{Logged, TestSine};
use komponist_engine::{Command, EngineEvent, EventKind, MidiInEvent};
use komponist_model::{Tick, TickRange, TrackId};

use crate::common::*;

fn midi(frame: u32, port: u16, data: [u8; 3]) -> MidiInEvent {
    MidiInEvent {
        frame,
        port: MidiPort(port),
        data,
    }
}

/// Events other than calls: `(frame, kind)`.
fn events(log: &[Logged]) -> Vec<(u64, EventKind)> {
    log.iter()
        .filter_map(|e| match *e {
            Logged::Event { frame, kind } => Some((frame, kind)),
            _ => None,
        })
        .collect()
}

fn on(key: u8, velocity: u8, channel: u8) -> impl Fn(&EventKind) -> bool {
    move |k| {
        matches!(*k, EventKind::NoteOn { key: k2, velocity: v, channel: c, .. }
            if k2 == key && v == velocity && c == channel)
    }
}

fn off(key: u8, channel: u8) -> impl Fn(&EventKind) -> bool {
    move |k| matches!(*k, EventKind::NoteOff { key: k2, channel: c, .. } if k2 == key && c == channel)
}

#[test]
fn midi_reaches_monitored_tracks_whose_filter_accepts_it() {
    let all = track(1, 0, vec![]);
    let mut picky = track(2, 1, vec![]);
    picky.input = InputFilter {
        port: Some(MidiPort(2)),
        channel: Some(3),
        port_missing: false,
    };
    let mut unmonitored = track(3, 2, vec![]);
    unmonitored.monitor = false;
    let mut missing = track(4, 3, vec![]);
    missing.input.port_missing = true;
    let mut tracks = vec![all, picky, unmonitored, missing];
    for t in &mut tracks[..2] {
        t.monitor = true;
    }
    tracks[3].monitor = true;
    let p = program(120.0, tracks);
    let mut rig = Rig::new(RATE, 512);
    let mut logs = Vec::new();
    for slot in 0..4 {
        let (sine, log) = TestSine::logged(1 << 12);
        rig.insert(slot, sine);
        logs.push(log);
    }
    rig.set_program(&p);
    let input = [
        midi(10, 1, [0x90, 60, 100]),
        midi(20, 2, [0x93, 62, 90]),
        midi(30, 2, [0x92, 64, 80]),
        midi(40, 2, [0x93, 62, 0]),
        midi(50, 1, [0xb0, 7, 100]),
        // In the second processor call of the block (max block 512).
        midi(600, 1, [0x80, 60, 0]),
    ];
    rig.process_with(1000, &input, 0);
    let got: Vec<_> = logs.iter_mut().map(|l| events(&drain(l))).collect();
    // Omni, all ports: everything, with note-ids pairing ons and offs.
    let first = &got[0];
    assert_eq!(first.len(), 6, "{first:?}");
    let frames: Vec<u64> = first.iter().map(|e| e.0).collect();
    assert_eq!(frames, [10, 20, 30, 40, 50, 600]);
    assert!(on(60, 100, 0)(&first[0].1));
    assert!(on(62, 90, 3)(&first[1].1));
    assert!(on(64, 80, 2)(&first[2].1));
    assert!(off(62, 3)(&first[3].1));
    assert_eq!(first[4].1, EventKind::Midi([0xb0, 7, 100]));
    assert!(off(60, 0)(&first[5].1));
    // Port 2, channel 4 only.
    assert_eq!(got[1].len(), 2, "{:?}", got[1]);
    assert!(on(62, 90, 3)(&got[1][0].1) && off(62, 3)(&got[1][1].1));
    // Not monitored, or the named port is absent: nothing.
    assert!(got[2].is_empty() && got[3].is_empty());
    // `LiveNote` plays on its track even without monitoring.
    rig.send(Command::LiveNote {
        track: TrackId(3),
        key: 70,
        velocity: 100,
    });
    rig.process(10, 10);
    rig.send(Command::LiveNote {
        track: TrackId(3),
        key: 70,
        velocity: 0,
    });
    rig.process(10, 10);
    let third = events(&drain(&mut logs[2]));
    assert_eq!(third.len(), 2);
    assert_eq!(third[0].0, 1000);
    assert!(on(70, 100, 0)(&third[0].1));
    assert_eq!(third[1].0, 1010);
    assert!(off(70, 0)(&third[1].1));
}

#[test]
fn recording_captures_after_the_count_in_with_latency_correction() {
    let mut armed = track(1, 0, vec![]);
    armed.arm = true;
    armed.monitor = true;
    let mut other = track(2, 1, vec![]);
    other.monitor = true;
    let p = program(120.0, vec![armed, other]);
    let mut rig = Rig::new(RATE, 2048);
    let (sine, mut log) = TestSine::logged(1 << 14);
    rig.insert(0, sine);
    rig.insert(1, TestSine::new());
    rig.set_program(&p);
    rig.send(Command::Record { from: Tick(BAR) });
    let latency = 480;
    // Count-in (ticks 0 .. 3840): played, not captured.
    rig.process(50_000, 1000);
    rig.process_with(1000, &[midi(0, 1, [0x90, 60, 100])], latency);
    rig.process(45_000, 1000);
    rig.process_with(1000, &[], latency);
    // Capture: 106 000 − 480 samples = tick 4220.8 → 4220.
    rig.process(9_000, 1000);
    rig.process_with(
        1000,
        &[midi(0, 1, [0x90, 62, 90]), midi(10, 1, [0x80, 60, 0])],
        latency,
    );
    rig.process(3_000, 1000);
    rig.process_with(1000, &[midi(0, 1, [0x90, 62, 0])], latency);
    rig.process(9_000, 1000);
    rig.process_with(1000, &[midi(0, 1, [0x90, 64, 70])], latency);
    rig.send(Command::StopRecording);
    rig.process(1, 1);
    let recorded: Vec<String> = rig
        .take_events()
        .iter()
        .map(|e| match e {
            EngineEvent::RecordedNoteOn {
                track,
                tick,
                key,
                velocity,
            } => format!("on {} {} {key} {velocity}", track.0, tick.0),
            EngineEvent::RecordedNoteOff { track, tick, key } => {
                format!("off {} {} {key}", track.0, tick.0)
            }
            EngineEvent::CountInDone { at } => format!("count-in done {}", at.0),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        recorded,
        [
            "count-in done 3840",
            "on 1 4220 62 90",
            "off 1 4380 62",
            "on 1 4780 64 70",
            // Still held at StopRecording: ends at the playhead (121 000 samples).
            "off 1 4840 64",
        ]
    );
    // Monitoring went on throughout.
    let played = events(&drain(&mut log));
    assert!(on(60, 100, 0)(&played[0].1));
    assert_eq!(played[0].0, 50_000);
    assert_eq!(
        rig.handle.telemetry().transport(),
        komponist_engine::TransportState::Playing
    );
}

#[test]
fn loop_recording_corrects_latency_back_over_the_loop_start() {
    let mut armed = track(1, 0, vec![]);
    armed.arm = true;
    let mut p = program(120.0, vec![armed]);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(0), Tick(BAR));
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(0, TestSine::new());
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.send(Command::Record { from: Tick(0) });
    rig.process(96_000, 1000);
    // Heard 380 samples before the loop end, arriving 100 samples after the wrap.
    rig.process_with(1000, &[midi(100, 1, [0x90, 60, 100])], 480);
    let events = rig.take_events();
    assert!(matches!(
        events[0],
        EngineEvent::CountInDone { at: Tick(0) }
    ));
    assert!(matches!(
        events[1],
        EngineEvent::LoopWrapped { to: Tick(0) }
    ));
    assert!(
        matches!(
            events[2],
            EngineEvent::RecordedNoteOn {
                tick: Tick(3824),
                key: 60,
                ..
            }
        ),
        "{events:?}"
    );
}

#[test]
fn live_notes_are_captured_only_on_armed_tracks_while_recording() {
    let mut armed = track(1, 0, vec![]);
    armed.arm = true;
    let p = program(120.0, vec![armed, track(2, 1, vec![])]);
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(0, TestSine::new());
    rig.insert(1, TestSine::new());
    rig.set_program(&p);
    let live = |track, velocity| Command::LiveNote {
        track: TrackId(track),
        key: 60,
        velocity,
    };
    // Not recording: no capture.
    rig.send(live(1, 100));
    rig.send(live(1, 0));
    rig.process(100, 100);
    assert!(rig.take_events().is_empty());
    rig.send(Command::Play);
    rig.send(Command::Record { from: Tick(0) });
    rig.process(1000, 100);
    rig.take_events();
    rig.send(live(1, 100));
    rig.send(live(2, 100));
    rig.process(100, 100);
    rig.send(live(1, 0));
    rig.send(live(2, 0));
    rig.process(100, 100);
    let events = rig.take_events();
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(matches!(
        events[0],
        EngineEvent::RecordedNoteOn {
            track: TrackId(1),
            tick: Tick(40),
            key: 60,
            velocity: 100
        }
    ));
    assert!(matches!(
        events[1],
        EngineEvent::RecordedNoteOff {
            track: TrackId(1),
            tick: Tick(44),
            key: 60
        }
    ));
}

#[test]
fn keys_held_across_a_loop_wrap_end_with_the_lap() {
    let mut armed = track(1, 0, vec![]);
    armed.arm = true;
    let mut p = program(120.0, vec![armed]);
    p.loop_on = true;
    p.loop_range = TickRange::new(Tick(0), Tick(BAR));
    let mut rig = Rig::new(RATE, 2048);
    rig.insert(0, TestSine::new());
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.send(Command::Record { from: Tick(0) });
    rig.process_with(1000, &[midi(500, 1, [0x90, 60, 100])], 0);
    rig.process(95_000, 1000);
    rig.process_with(1000, &[midi(500, 1, [0x80, 60, 0])], 0);
    let events = rig.take_events();
    assert!(
        matches!(
            events[..],
            [
                EngineEvent::CountInDone { at: Tick(0) },
                EngineEvent::RecordedNoteOn {
                    tick: Tick(20),
                    key: 60,
                    ..
                },
                EngineEvent::RecordedNoteOff {
                    tick: Tick(BAR),
                    key: 60,
                    ..
                },
                EngineEvent::LoopWrapped { to: Tick(0) },
            ]
        ),
        "{events:?}"
    );
}
