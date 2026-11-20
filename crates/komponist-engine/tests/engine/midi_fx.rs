//! The MIDI effect stage (WP-88): events in front of the instrument, the note guard (no hanging
//! notes on bypass, removal, stop, locate, panic and program swaps), live input and recording, and
//! bit-exact output with and without workers.

use komponist_engine::program::TrackProgram;
use komponist_engine::testing::{
    EventLog, Logged, TEST_TRANSPOSE_PARAM, TestMidiFx, TestMidiMode, TestSine,
};
use komponist_engine::{Command, EngineEvent, MidiInEvent, MidiPort, Program, SlotId};
use komponist_model::{Tick, TickRange};

use crate::common::*;

/// Slot of the instrument and of the MIDI effect.
const SYNTH: u32 = 0;
const FX: u32 = 1;

/// A track with a MIDI effect in `FX` in front of the instrument in `SYNTH`.
fn staged(id: u64, notes: Vec<komponist_engine::program::SeqNote>) -> TrackProgram {
    let mut t = track(id, SYNTH, notes);
    t.chain = vec![midi_effect(FX), instrument(SYNTH)];
    t
}

fn rig_with(fx: TestMidiFx, p: &Program) -> (Rig, EventLog) {
    let mut rig = Rig::new(RATE, 512);
    let (sine, log) = TestSine::logged(1 << 16);
    rig.insert(SYNTH, sine);
    rig.insert(FX, fx);
    rig.set_program(p);
    rig.send(Command::Play);
    (rig, log)
}

/// The keys the instrument still holds after `log`: note-ons without a note-off of their id.
fn held(log: &[Logged]) -> Vec<u8> {
    let mut open: Vec<(u32, u8)> = Vec::new();
    for (_, kind, key, id) in notes(log) {
        if kind == "on" {
            open.push((id, key));
        } else {
            let at = open.iter().position(|(i, _)| *i == id);
            assert!(
                at.is_some(),
                "note-off for a note that never started: {key}"
            );
            open.remove(at.unwrap());
        }
    }
    open.into_iter().map(|(_, key)| key).collect()
}

#[test]
fn the_instrument_gets_the_stage_output_with_the_timing_of_the_input() {
    // 120 bpm: a quarter note (960 ticks) is 24 000 samples.
    let p = program(120.0, vec![staged(1, vec![note(960, 960, 60)])]);
    let (mut rig, mut log) = rig_with(TestMidiFx::transposing(2), &p);
    rig.process(100_000, 777);
    let got = notes(&drain(&mut log));
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!((got[0].0, got[0].1, got[0].2), (24_000, "on", 62));
    assert_eq!((got[1].0, got[1].1, got[1].2), (48_000, "off", 62));
    assert_eq!(got[0].3, got[1].3, "the off carries the note's id");
    assert_ne!(got[0].3, komponist_engine::NOTE_ID_NONE);
}

#[test]
fn a_stage_that_never_sends_note_offs_cannot_hang_a_note() {
    let fx = || TestMidiFx::new(TestMidiMode::SwallowOffs);
    let long = vec![note(0, 4 * BAR, 60), note(100, 4 * BAR, 64)];
    let p = program(120.0, vec![staged(1, long)]);

    // The note ends at its natural end? No: the stage swallowed the off, so only a stop does.
    let (mut rig, mut log) = rig_with(fx(), &p);
    rig.process(5 * 96_000, 1000);
    assert_eq!(held(&drain(&mut log)), [60, 64], "the stage's own fault");

    // Stop, locate, panic and a loop wrap each leave nothing behind.
    type Act = fn(&mut Rig);
    let ways: [(&str, Act); 3] = [
        ("stop", |rig| rig.send(Command::Stop)),
        ("locate", |rig| rig.send(Command::Locate(Tick(BAR)))),
        ("panic", |rig| rig.send(Command::AllNotesOff)),
    ];
    for (name, act) in ways {
        let (mut rig, mut log) = rig_with(fx(), &p);
        rig.process(10_000, 1000);
        act(&mut rig);
        rig.process(2000, 1000);
        let rest = held(&drain(&mut log));
        // After a locate the chased notes start again: they are the only ones held.
        let expected: &[u8] = if name == "locate" { &[60, 64] } else { &[] };
        assert_eq!(rest, expected, "{name}");
    }
    let mut looped = p.clone();
    looped.loop_range = TickRange::new(Tick::ZERO, Tick(BAR));
    looped.loop_on = true;
    let (mut rig, mut log) = rig_with(fx(), &looped);
    rig.process(5 * 96_000, 1000);
    let mut stopped = drain(&mut log);
    rig.send(Command::Stop);
    rig.process(1000, 1000);
    stopped.extend(drain(&mut log));
    // Notes chased at every wrap retrigger the held key: at most one instance per key at a time.
    assert_eq!(held(&stopped), Vec::<u8>::new());
}

#[test]
fn bypass_and_removal_end_the_notes_and_the_events_then_pass_straight_on() {
    let long = vec![note(0, 4 * BAR, 60), note(2 * BAR, BAR, 70)];
    let before = program(120.0, vec![staged(1, long.clone())]);
    let resets = |fx: &TestMidiFx| fx.resets();

    // Bypass turns on after the first bar: the transposed note ends at once, the effect is reset
    // once, and the note at bar 3 comes through unchanged (instruments never see two copies).
    let fx = TestMidiFx::transposing(2);
    let counter = resets(&fx);
    let (mut rig, mut log) = rig_with(fx, &before);
    rig.process(96_000, 1000);
    let mut bypassed = before.clone();
    let mut t = (*bypassed.tracks[0]).clone();
    t.chain[0].bypass = true;
    bypassed.tracks[0] = std::sync::Arc::new(t);
    rig.set_program(&bypassed);
    rig.process(4 * 96_000, 1000);
    let got = notes(&drain(&mut log));
    let keys: Vec<_> = got.iter().map(|n| (n.1, n.2)).collect();
    assert_eq!(
        keys,
        [("on", 62), ("off", 62), ("on", 70), ("off", 70)],
        "{got:?}"
    );
    assert_eq!(got[1].0, 96_000, "the note ends at the swap");
    assert!(counter.load(std::sync::atomic::Ordering::Relaxed) >= 1);

    // Removal: the chain loses the stage; the held note ends, the instrument plays the rest as is.
    let (mut rig, mut log) = rig_with(TestMidiFx::transposing(2), &before);
    rig.process(96_000, 1000);
    let mut removed = before.clone();
    let mut t = (*removed.tracks[0]).clone();
    t.chain.remove(0);
    removed.tracks[0] = std::sync::Arc::new(t);
    rig.set_program(&removed);
    rig.send(Command::RemoveProcessor { slot: SlotId(FX) });
    rig.process(4 * 96_000, 1000);
    let got = notes(&drain(&mut log));
    let keys: Vec<_> = got.iter().map(|n| (n.1, n.2)).collect();
    assert_eq!(keys, [("on", 62), ("off", 62), ("on", 70), ("off", 70)]);
    assert_eq!(got[1].0, 96_000);
}

#[test]
fn a_stage_put_in_front_of_a_sounding_note_takes_over_its_note_off() {
    let long = vec![note(0, 4 * BAR, 60), note(2 * BAR, BAR, 70)];
    let plain = program(120.0, vec![track(1, SYNTH, long.clone())]);
    let (mut rig, mut log) = rig_with(TestMidiFx::transposing(2), &plain);
    rig.process(96_000, 1000);
    let with = program(120.0, vec![staged(1, long)]);
    rig.set_program(&with);
    rig.process(4 * 96_000, 1000);
    let got = notes(&drain(&mut log));
    let keys: Vec<_> = got.iter().map(|n| (n.1, n.2)).collect();
    // The plain note ends at the swap (once!), the later note is transposed.
    assert_eq!(
        keys,
        [("on", 60), ("off", 60), ("on", 72), ("off", 72)],
        "{got:?}"
    );
    assert_eq!(got[1].0, 96_000);
    assert_eq!(got.len(), 4);
}

#[test]
fn parameters_reach_the_stage_before_the_next_call() {
    let p = program(
        120.0,
        vec![staged(1, vec![note(0, 100, 60), note(BAR, 100, 60)])],
    );
    let (mut rig, mut log) = rig_with(TestMidiFx::transposing(0), &p);
    rig.process(1000, 100);
    rig.send(Command::SetParam {
        slot: SlotId(FX),
        id: TEST_TRANSPOSE_PARAM,
        value: 5.0,
    });
    rig.process(96_000, 1000);
    let got = notes(&drain(&mut log));
    let ons: Vec<_> = got.iter().filter(|n| n.1 == "on").map(|n| n.2).collect();
    assert_eq!(ons, [60, 65]);
}

#[test]
fn live_input_goes_through_the_stage_and_recording_captures_the_input() {
    let mut t = staged(1, vec![]);
    t.arm = true;
    t.monitor = true;
    let p = program(120.0, vec![t]);
    let (mut rig, mut log) = rig_with(TestMidiFx::transposing(7), &p);
    rig.send(Command::Stop);
    rig.send(Command::Record { from: Tick::ZERO });
    rig.process(96_000, 512);
    rig.take_events();
    let m = |frame, data| MidiInEvent {
        frame,
        port: MidiPort(0),
        data,
    };
    rig.process_with(512, &[m(10, [0x90, 60, 100]), m(300, [0x80, 60, 0])], 0);
    rig.process(1000, 512);
    // The instrument heard key 67; the take gets key 60 (the player's input, not the output).
    let got = notes(&drain(&mut log));
    let heard: Vec<_> = got.iter().map(|n| (n.1, n.2)).collect();
    assert_eq!(heard, [("on", 67), ("off", 67)], "{got:?}");
    let recorded: Vec<_> = rig
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            EngineEvent::RecordedNoteOn { key, .. } => Some(("on", key)),
            EngineEvent::RecordedNoteOff { key, .. } => Some(("off", key)),
            _ => None,
        })
        .collect();
    assert_eq!(recorded, [("on", 60), ("off", 60)]);
}

#[test]
fn a_stage_without_an_instrument_or_with_a_bypassed_one_plays_nothing_and_holds_nothing() {
    let mut t = staged(1, vec![note(0, BAR, 60)]);
    t.chain[1].bypass = true;
    let p = program(120.0, vec![t]);
    let (mut rig, mut log) = rig_with(TestMidiFx::transposing(2), &p);
    rig.process(96_000, 1000);
    assert!(notes(&drain(&mut log)).is_empty());
    // Instrument back on: later notes play.
    let mut on = p.clone();
    let mut t = (*on.tracks[0]).clone();
    t.chain[1].bypass = false;
    t.notes = std::sync::Arc::new(komponist_engine::program::NoteSchedule::from_sorted(vec![
        note(BAR + 960, 960, 60),
    ]));
    on.tracks[0] = std::sync::Arc::new(t);
    rig.set_program(&on);
    rig.process(96_000, 1000);
    let got = notes(&drain(&mut log));
    assert_eq!(got.iter().map(|n| n.2).collect::<Vec<_>>(), [62, 62]);
}

#[test]
fn stage_chains_run_in_order_and_a_bypassed_stage_passes_through() {
    let mut t = track(1, SYNTH, vec![note(0, 960, 60)]);
    t.chain = vec![midi_effect(1), midi_effect(2), instrument(SYNTH)];
    let p = program(120.0, vec![t]);
    let mut rig = Rig::new(RATE, 512);
    let (sine, mut log) = TestSine::logged(1 << 12);
    rig.insert(SYNTH, sine);
    rig.insert(1, TestMidiFx::transposing(2));
    rig.insert(2, TestMidiFx::transposing(10));
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(48_000, 1000);
    let keys: Vec<_> = notes(&drain(&mut log)).iter().map(|n| n.2).collect();
    assert_eq!(keys, [72, 72]);
    // The first one bypassed: only the second transposes.
    let mut p2 = p.clone();
    let mut t = (*p2.tracks[0]).clone();
    t.chain[0].bypass = true;
    p2.tracks[0] = std::sync::Arc::new(t);
    rig.set_program(&p2);
    rig.send(Command::Locate(Tick::ZERO));
    rig.process(48_000, 1000);
    let keys: Vec<_> = notes(&drain(&mut log)).iter().map(|n| n.2).collect();
    assert_eq!(keys, [70, 70]);
}

/// 12 tracks, each with a stage (transposing, doubling or swallowing note-offs) in front of a
/// `TestSine`, looped, with live input, a locate and a bypass change on the way.
fn many(workers: usize) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::new(RATE, 256);
    assert_eq!(rig.engine.set_workers(workers, None).threads, workers);
    let mut tracks = Vec::new();
    for i in 0..12u32 {
        let mut t = track(u64::from(i) + 1, i, crate::bench::material(i));
        t.chain = vec![midi_effect(100 + i), instrument(i)];
        t.monitor = i == 3;
        tracks.push(t);
        rig.insert(i, TestSine::new());
        let fx = match i % 3 {
            0 => TestMidiFx::transposing(i as i32 - 5),
            1 => TestMidiFx::new(TestMidiMode::Fifth),
            _ => TestMidiFx::new(TestMidiMode::SwallowOffs),
        };
        rig.insert(100 + i, fx);
    }
    let mut p = program(127.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(2 * BAR));
    p.loop_on = true;
    rig.set_program(&p);
    rig.send(Command::Play);
    let blocks = [256, 100, 512, 37, 2048, 3000, 64];
    for call in 0..60usize {
        let n = blocks[call % blocks.len()];
        let midi = [MidiInEvent {
            frame: (n / 2) as u32,
            port: MidiPort(0),
            data: [0x90, 60 + (call % 12) as u8, 100],
        }];
        rig.process_with(n, &midi, 0);
        if call == 20 {
            rig.send(Command::Locate(Tick(BAR / 3)));
        }
        if call == 30 {
            let mut q = p.clone();
            let mut t = (*q.tracks[4]).clone();
            t.chain[0].bypass = true;
            q.tracks[4] = std::sync::Arc::new(t);
            rig.set_program(&q);
        }
    }
    rig.render(20_000, 333);
    (rig.l, rig.r)
}

#[test]
fn output_with_workers_is_bit_exact_with_the_single_threaded_engine() {
    let single = many(0);
    assert!(single.0.iter().any(|&s| s != 0.0));
    for workers in [2, 4] {
        let parallel = many(workers);
        assert_eq!(parallel.0.len(), single.0.len());
        let same = |a: &[f32], b: &[f32]| a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits());
        assert!(same(&parallel.0, &single.0), "left, {workers} workers");
        assert!(same(&parallel.1, &single.1), "right, {workers} workers");
    }
}

// ------------------------------------------------------------------------------------------------
// The real devices on the engine (WP-88): Arpeggiator and Tonleiter from komponist-builtin.
// ------------------------------------------------------------------------------------------------

mod devices {
    use komponist_builtin::Internal;
    use komponist_engine::testing::TestSine;
    use komponist_engine::{Command, SlotId};
    use komponist_model::params::ParamId;
    use komponist_model::{Tempo, TempoMap, Tick, TickRange};

    use super::{FX, SYNTH, held, staged};
    use crate::common::*;

    /// An activated internal device with `values` by parameter key.
    fn device(kind: Internal, values: &[(&str, f64)]) -> komponist_builtin::InternalProcessor {
        let mut all = kind.defaults();
        for (key, value) in values {
            all[kind.wire(&ParamId::new(*key)).unwrap() as usize] = *value;
        }
        let mut p = kind.processor(&all);
        komponist_engine::Processor::activate(&mut p, RATE, 512).unwrap();
        p
    }

    fn rig(
        arp: komponist_builtin::InternalProcessor,
        p: &komponist_engine::Program,
    ) -> (Rig, komponist_engine::testing::EventLog) {
        let mut rig = Rig::new(RATE, 512);
        let (sine, log) = TestSine::logged(1 << 16);
        rig.insert(SYNTH, sine);
        rig.insert(FX, arp);
        rig.set_program(p);
        rig.send(Command::Play);
        (rig, log)
    }

    /// A chord C E G for four bars on a track with the arpeggiator in front of the instrument.
    fn chord_program() -> komponist_engine::Program {
        let notes = vec![
            note(0, 4 * BAR, 60),
            note(0, 4 * BAR, 64),
            note(0, 4 * BAR, 67),
        ];
        program(120.0, vec![staged(1, notes)])
    }

    #[test]
    fn the_arpeggiator_plays_the_chord_on_the_sequencers_grid() {
        let (mut rig, mut log) = rig(device(Internal::Arpeggiator, &[]), &chord_program());
        rig.process(5 * 6000, 777);
        let got = notes(&drain(&mut log));
        let ons: Vec<(u64, u8)> = got
            .iter()
            .filter(|n| n.1 == "on")
            .map(|n| (n.0, n.2))
            .collect();
        // 1/16 at 120 bpm: every 6000 samples, up through the chord.
        assert_eq!(
            ons,
            [
                (0, 60),
                (6000, 64),
                (12_000, 67),
                (18_000, 60),
                (24_000, 64)
            ]
        );
        // Gate 60 %: each note is 3600 samples long, one at a time.
        let offs: Vec<u64> = got.iter().filter(|n| n.1 == "off").map(|n| n.0).collect();
        assert_eq!(offs, [3600, 9600, 15_600, 21_600, 27_600]);
    }

    #[test]
    fn a_tempo_change_by_a_program_swap_keeps_the_steps_on_the_tick_grid() {
        let p = chord_program();
        let (mut rig, mut log) = rig(device(Internal::Arpeggiator, &[]), &p);
        let before = 9_999;
        rig.process(before, 777);
        let mut fast = p.clone();
        fast.tempo_map = TempoMap::constant(Tempo::new(240.0), p.tempo_map.meter);
        rig.set_program(&fast);
        rig.process(40_000, 513);
        let got = notes(&drain(&mut log));
        let ons: Vec<u64> = got.iter().filter(|n| n.1 == "on").map(|n| n.0).collect();
        // Before the swap: steps at 0 and 6000. Tick at the swap: 9 999 / 25 = 399.96, so the next
        // grid step is tick 480; after the rescale its sample is 480 × 12.5 = 6000 in song time.
        let swap_tick = f64::from(before as u32) / 25.0;
        let next = 240.0 * (swap_tick / 240.0).ceil();
        let rescaled = (swap_tick * 12.5 + 0.5).floor() as i64;
        let first_after = before as i64 + (next * 12.5) as i64 - rescaled;
        assert_eq!(ons[..2], [0, 6000]);
        assert_eq!(ons[2], first_after as u64, "{ons:?}");
        // Then every 3000 samples (240 ticks at 240 bpm).
        for pair in ons[2..].windows(2) {
            assert_eq!(pair[1] - pair[0], 3000);
        }
    }

    #[test]
    fn no_note_hangs_on_bypass_removal_stop_locate_panic_or_loop_wrap() {
        let p = chord_program();
        let mut looped = p.clone();
        looped.loop_range = TickRange::new(Tick::ZERO, Tick(BAR));
        looped.loop_on = true;
        type Act = fn(&mut Rig, &komponist_engine::Program);
        let ways: [(&str, Act); 5] = [
            ("stop", |rig, _| rig.send(Command::Stop)),
            ("locate", |rig, _| rig.send(Command::Locate(Tick(BAR / 2)))),
            ("panic", |rig, _| rig.send(Command::AllNotesOff)),
            ("bypass", |rig, p| {
                let mut q = p.clone();
                let mut t = (*q.tracks[0]).clone();
                t.chain[0].bypass = true;
                q.tracks[0] = std::sync::Arc::new(t);
                rig.set_program(&q);
            }),
            ("removal", |rig, p| {
                let mut q = p.clone();
                let mut t = (*q.tracks[0]).clone();
                t.chain.remove(0);
                q.tracks[0] = std::sync::Arc::new(t);
                rig.set_program(&q);
                rig.send(Command::RemoveProcessor { slot: SlotId(FX) });
            }),
        ];
        // Several cut points, so a note is sounding or not, at a block's start, middle and end.
        for cut in [3_000, 3_600, 3_601, 6_000, 11_999, 12_000, 20_001] {
            for (name, act) in ways {
                let (mut rig, mut log) = rig(device(Internal::Arpeggiator, &[]), &p);
                rig.process(cut, 512);
                act(&mut rig, &p);
                // Locate: the arp plays on from the new position, so stop there as well before
                // asking.
                rig.process(1_000, 512);
                if name == "locate" {
                    rig.send(Command::Stop);
                    rig.process(100, 100);
                }
                // Notes held across a swap do not start in the middle (no chasing on a program
                // swap), so after bypass or removal the instrument holds nothing either.
                let rest = held(&drain(&mut log));
                assert!(rest.is_empty(), "{name} at {cut}: {rest:?}");
            }
        }
        // The loop wrap, many laps, then stop: nothing left.
        let (mut rig, mut log) = rig(
            device(Internal::Arpeggiator, &[("gate", 1.0), ("rate", 6.0)]),
            &looped,
        );
        rig.process(7 * 96_000 + 123, 4096);
        rig.send(Command::Stop);
        rig.process(100, 100);
        assert!(held(&drain(&mut log)).is_empty());
    }

    #[test]
    fn the_tonleiter_maps_sequenced_notes_and_no_note_hangs_when_the_scale_changes() {
        let notes = vec![note(0, 2 * BAR, 61), note(BAR, BAR, 66)];
        let p = program(120.0, vec![staged(1, notes)]);
        let (mut rig, mut log) = rig(device(Internal::Tonleiter, &[("mode", 1.0)]), &p);
        rig.process(10_000, 512);
        // Change the scale while the first note is held.
        rig.send(Command::SetParam {
            slot: SlotId(FX),
            id: Internal::Tonleiter.wire(&ParamId::new("root")).unwrap(),
            value: 1.0,
        });
        rig.process(4 * 96_000, 512);
        let got = notes_of(&drain(&mut log));
        // C major, snapping up: C# becomes D (62) and ends as D at its own end (bar 3), although
        // the root changed in between. F# (66) is in C# major and stays; both end at bar 3.
        let ons: Vec<_> = got
            .iter()
            .filter(|n| n.0 == "on")
            .map(|n| (n.1, n.2))
            .collect();
        assert_eq!(ons, [(62, 0), (66, 96_000)]);
        let mut offs: Vec<_> = got
            .iter()
            .filter(|n| n.0 == "off")
            .map(|n| (n.1, n.2))
            .collect();
        offs.sort_unstable();
        assert_eq!(offs, [(62, 192_000), (66, 192_000)]);
    }

    /// 8 tracks, each with an arpeggiator (different modes and rates), a scale, or both in front of
    /// a `TestSine`; live input, a locate and a tempo change on the way.
    fn arps(workers: usize) -> (Vec<f32>, Vec<f32>) {
        use komponist_engine::{MidiInEvent, MidiPort};
        let mut rig = Rig::new(RATE, 256);
        assert_eq!(rig.engine.set_workers(workers, None).threads, workers);
        let mut tracks = Vec::new();
        for i in 0..8u32 {
            let mut t = track(u64::from(i) + 1, i, crate::bench::material(i));
            let fx = 100 + 2 * i;
            t.chain = if i % 4 == 3 {
                vec![midi_effect(fx), midi_effect(fx + 1), instrument(i)]
            } else {
                vec![midi_effect(fx), instrument(i)]
            };
            t.monitor = i == 1;
            tracks.push(t);
            rig.insert(i, TestSine::new());
            let arp = device(
                Internal::Arpeggiator,
                &[
                    ("mode", f64::from(i % 5)),
                    ("rate", f64::from(i % 8)),
                    ("octaves", 2.0),
                ],
            );
            if i % 4 == 3 {
                rig.insert(
                    fx,
                    device(Internal::Tonleiter, &[("scale", 9.0), ("mode", 2.0)]),
                );
                rig.insert(fx + 1, arp);
            } else {
                rig.insert(fx, arp);
            }
        }
        let mut p = program(127.0, tracks);
        p.loop_range = TickRange::new(Tick::ZERO, Tick(2 * BAR));
        p.loop_on = true;
        rig.set_program(&p);
        rig.send(Command::Play);
        let blocks = [256, 100, 512, 37, 2048, 3000, 64];
        for call in 0..70usize {
            let n = blocks[call % blocks.len()];
            let midi = [MidiInEvent {
                frame: (n / 2) as u32,
                port: MidiPort(0),
                data: [
                    if call % 2 == 0 { 0x90 } else { 0x80 },
                    60 + (call % 5) as u8,
                    100,
                ],
            }];
            rig.process_with(n, &midi, 0);
            match call {
                20 => rig.send(Command::Locate(Tick(BAR / 3))),
                35 => {
                    let mut q = p.clone();
                    q.tempo_map = TempoMap::constant(Tempo::new(150.0), p.tempo_map.meter);
                    rig.set_program(&q);
                }
                50 => rig.send(Command::Stop),
                55 => rig.send(Command::Play),
                _ => {}
            }
        }
        rig.render(20_000, 333);
        (rig.l, rig.r)
    }

    #[test]
    fn the_real_devices_are_bit_exact_with_and_without_workers() {
        let single = arps(0);
        assert!(single.0.iter().any(|&s| s != 0.0));
        for workers in [2, 4] {
            let parallel = arps(workers);
            let same = |a: &[f32], b: &[f32]| {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
            };
            assert!(same(&parallel.0, &single.0), "left, {workers} workers");
            assert!(same(&parallel.1, &single.1), "right, {workers} workers");
        }
    }

    fn notes_of(log: &[komponist_engine::testing::Logged]) -> Vec<(&'static str, u8, u64)> {
        notes(log).into_iter().map(|n| (n.1, n.2, n.0)).collect()
    }
}
