//! Tails after Stop: releases, delays and reverbs ring out (the first Stop and a Locate while
//! playing do not reset), a second Stop is the hard cut, and MIDI effects do not play on.
//! Every `process` call runs inside `assert_no_alloc` (the rig).

use komponist_builtin::{Internal, PisynthProcessor, preset_patch, presets};
use komponist_engine::testing::{Logged, TestSine};
use komponist_engine::{Command, Processor, Program, SlotId};
use komponist_model::params::ParamId;
use komponist_model::{Tick, TrackId};

use crate::common::*;

const BLOCK: usize = 512;

fn device(kind: Internal, values: &[(&str, f64)]) -> komponist_builtin::InternalProcessor {
    let mut all = kind.defaults();
    for (key, value) in values {
        all[kind.wire(&ParamId::new(*key)).unwrap() as usize] = *value;
    }
    let mut p = kind.processor(&all);
    p.activate(RATE, BLOCK).unwrap();
    p
}

/// One pisynth ("Strings") track with a Hall behind it; a two-bar note at bar 1.
fn strings_hall() -> (Rig, Program) {
    let mut rig = Rig::new(RATE, BLOCK);
    let preset = presets().find(|x| x.name == "Strings").unwrap();
    let mut synth = PisynthProcessor::new(preset_patch(preset.index).unwrap());
    synth.activate(RATE, BLOCK).unwrap();
    rig.insert(0, synth);
    rig.insert(1, device(Internal::Hall, &[("mix", 1.0)]));
    let mut t = track(1, 0, vec![note(0, 2 * BAR, 60)]);
    t.chain.push(effect(1));
    let p = program(120.0, vec![t]);
    rig.set_program(&p);
    rig.send(Command::Play);
    (rig, p)
}

fn rms(rig: &Rig, range: std::ops::Range<usize>) -> f32 {
    let n = range.len() as f32;
    (rig.l[range].iter().map(|x| x * x).sum::<f32>() / n).sqrt()
}

#[test]
fn stop_lets_release_and_reverb_ring_out_and_decay() {
    let (mut rig, _) = strings_hall();
    rig.process(20_000, BLOCK);
    rig.send(Command::Stop);
    let at = rig.frames();
    rig.process(2 * 48_000, BLOCK);
    let first = rms(&rig, at + BLOCK..at + 4 * BLOCK);
    let later = rms(&rig, at + 90_000..at + 96_000);
    assert!(first > 1e-4, "tail rings out after Stop: {first}");
    assert!(later < first, "and decays: {first} -> {later}");
    // It does not stop in the first block.
    assert!(rig.l[at..at + BLOCK].iter().any(|&x| x != 0.0));
    assert!(rig.l[at + BLOCK..].iter().all(|x| x.is_finite()));
}

#[test]
fn a_second_stop_cuts_at_once() {
    let (mut rig, _) = strings_hall();
    rig.process(20_000, BLOCK);
    rig.send(Command::Stop);
    rig.process(4 * BLOCK, BLOCK);
    let at = rig.frames();
    assert!(rms(&rig, at - BLOCK..at) > 1e-4, "still ringing");
    rig.send(Command::Stop);
    rig.process(2 * BLOCK, BLOCK);
    assert!(
        rig.l[at..].iter().chain(&rig.r[at..]).all(|&x| x == 0.0),
        "silent in the block of the second Stop"
    );
}

#[test]
fn locate_while_playing_keeps_the_tail() {
    let (mut rig, _) = strings_hall();
    rig.process(20_000, BLOCK);
    // Past the note: nothing is sounding there, so only the old voice's release and the hall remain.
    rig.send(Command::Locate(Tick(3 * BAR)));
    let at = rig.frames();
    rig.process(4 * BLOCK, BLOCK);
    assert!(
        rms(&rig, at..at + 4 * BLOCK) > 1e-4,
        "tail survives a locate"
    );
}

#[test]
fn stop_resets_nothing_and_second_stop_resets_every_processor() {
    let mut rig = Rig::new(RATE, BLOCK);
    let (sine, mut log) = TestSine::logged(1 << 12);
    rig.insert(0, sine);
    rig.set_program(&program(
        120.0,
        vec![track(1, 0, vec![note(0, 2 * BAR, 60)])],
    ));
    rig.send(Command::Play);
    rig.process(5_000, BLOCK);
    let resets = |log: &mut _| {
        drain(log)
            .iter()
            .filter(|e| matches!(e, Logged::Reset { .. }))
            .count()
    };
    rig.send(Command::Stop);
    rig.process(1_000, BLOCK);
    assert_eq!(resets(&mut log), 0);
    rig.send(Command::Stop);
    rig.process(1_000, BLOCK);
    assert_eq!(resets(&mut log), 1);
}

#[test]
fn a_delay_line_rings_on_after_stop() {
    let mut rig = Rig::new(RATE, BLOCK);
    let (sine, _) = TestSine::logged(1 << 12);
    rig.insert(0, sine);
    // Free-running delay, 400 ms, feedback high: echoes keep coming.
    rig.insert(
        1,
        device(
            Internal::Delay,
            &[("sync", 0.0), ("mix", 1.0), ("feedback", 0.7)],
        ),
    );
    let mut t = track(1, 0, vec![note(0, 960, 60)]);
    t.chain.push(effect(1));
    rig.set_program(&program(120.0, vec![t]));
    rig.send(Command::Play);
    rig.process(5_000, BLOCK);
    rig.send(Command::Stop);
    let at = rig.frames();
    rig.process(48_000, BLOCK);
    assert!(
        rms(&rig, at + 2 * BLOCK..at + 48_000) > 1e-5,
        "echoes after Stop"
    );
}

#[test]
fn an_arpeggiator_plays_nothing_after_stop() {
    let mut rig = Rig::new(RATE, BLOCK);
    let (sine, mut log) = TestSine::logged(1 << 16);
    rig.insert(0, sine);
    rig.insert(1, device(Internal::Arpeggiator, &[]));
    let mut t = track(1, 0, vec![note(0, 4 * BAR, 60), note(0, 4 * BAR, 64)]);
    t.chain = vec![midi_effect(1), instrument(0)];
    t.monitor = true;
    rig.set_program(&program(120.0, vec![t]));
    rig.send(Command::Play);
    rig.send(Command::LiveNote {
        track: TrackId(1),
        key: 67,
        velocity: 100,
    });
    rig.process(10_000, BLOCK);
    assert!(notes(&drain(&mut log)).iter().any(|n| n.1 == "on"));
    rig.send(Command::Stop);
    rig.process(BLOCK, BLOCK);
    let stopped = rig.frames() as u64 - BLOCK as u64;
    drain(&mut log);
    rig.process(100_000, BLOCK);
    let late: Vec<_> = notes(&drain(&mut log))
        .into_iter()
        .filter(|n| n.0 >= stopped && n.1 == "on")
        .collect();
    assert!(late.is_empty(), "new notes after Stop: {late:?}");
    let _ = SlotId(0);
}
