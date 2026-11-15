//! Tails after Stop: releases, delays and reverbs ring out (the first Stop and a Locate while
//! playing do not reset), a second Stop is the hard cut (5 ms fade, then reset), and MIDI effects do not play on.
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

/// The hard cut's fade: 5 ms at 48 kHz.
const CUT: usize = 240;

/// A loud tone that is still sounding after the song's note ended (echoes of a feedback delay),
/// playing at the end of the returned rig's output.
fn echoing_tone(block: usize) -> Rig {
    let mut rig = Rig::new(RATE, BLOCK);
    let (sine, _) = TestSine::logged(1 << 12);
    rig.insert(0, sine);
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
    rig.process(40_000, block);
    rig.send(Command::Stop);
    rig.process(block, block);
    let at = rig.frames();
    assert!(rms(&rig, at - block..at) > 0.01, "a loud tone rings");
    rig
}

fn peak(rig: &Rig, range: std::ops::Range<usize>) -> f32 {
    rig.l[range].iter().fold(0.0, |m, x| m.max(x.abs()))
}

#[test]
fn a_second_stop_fades_out_in_5_ms_without_a_click() {
    for block in [100, 128, BLOCK] {
        let mut rig = echoing_tone(block);
        let at = rig.frames();
        let loud = peak(&rig, at - 2_000..at);
        rig.send(Command::Stop);
        rig.send(Command::Stop); // during the fade: no-op
        rig.process(2 * BLOCK, block);
        // Alive at the start of the fade, decaying, exactly 0 after 5 ms (the cut length).
        assert!(rig.l[at].abs() > 0.1 * loud, "block {block}: no jump to 0");
        assert!(peak(&rig, at + 120..at + 180) < peak(&rig, at..at + 60));
        assert!(rig.l[at + CUT - 1].abs() < 0.02 * loud);
        for ch in [&rig.l, &rig.r] {
            assert!(ch[at + CUT..].iter().all(|&x| x == 0.0), "block {block}");
        }
        // No step between two samples bigger than the tone's own slope plus the ramp's.
        let step = rig.l[at - 1..at + CUT + 10]
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < 0.08 * loud, "block {block}: step {step} of {loud}");
    }
}

#[test]
fn the_cut_resets_every_processor_once_after_the_fade() {
    let mut rig = Rig::new(RATE, BLOCK);
    let (sine, mut log) = TestSine::logged(1 << 12);
    rig.insert(0, sine);
    rig.set_program(&program(
        120.0,
        vec![track(1, 0, vec![note(0, 2 * BAR, 60)])],
    ));
    rig.send(Command::Play);
    rig.process(5_000, 100);
    rig.send(Command::Stop);
    rig.process(100, 100);
    drain(&mut log);
    rig.send(Command::Stop);
    rig.process(100, 100); // 100 of 240 frames faded
    rig.send(Command::Stop);
    rig.send(Command::Locate(Tick(0))); // does not cancel the cut
    let resets = |log: &mut _| {
        drain(log)
            .iter()
            .filter(|e| matches!(e, Logged::Reset { .. }))
            .count()
    };
    assert_eq!(resets(&mut log), 0, "not before the fade is over");
    rig.process(100, 100);
    assert_eq!(resets(&mut log), 0);
    rig.process(100, 100);
    assert_eq!(resets(&mut log), 1, "after the fade");
    rig.process(2_000, 100);
    assert_eq!(resets(&mut log), 0, "once");
}

#[test]
fn play_during_the_fade_starts_clean_and_at_full_level() {
    let mut rig = Rig::new(RATE, BLOCK);
    let (sine, mut log) = TestSine::logged(1 << 12);
    rig.insert(0, sine);
    rig.set_program(&program(
        120.0,
        vec![track(1, 0, vec![note(0, 2 * BAR, 60)])],
    ));
    rig.send(Command::Play);
    rig.process(5_000, 100);
    let full = rig.l[0];
    assert!(full.abs() > 0.1);
    rig.send(Command::Stop);
    rig.process(100, 100);
    rig.send(Command::Stop);
    rig.process(100, 100);
    rig.send(Command::Locate(Tick(0)));
    rig.process(40, 100);
    drain(&mut log);
    let at = rig.frames();
    rig.send(Command::Play);
    rig.process(1_000, 100);
    assert_eq!(rig.l[at], full, "first sample of the new start: full level");
    assert!(rig.l[at..].iter().any(|&x| x.abs() > 0.1));
    let log = drain(&mut log);
    let resets = log
        .iter()
        .filter(|e| matches!(e, Logged::Reset { .. }))
        .count();
    assert_eq!(resets, 1, "the cut finished at the Play");
    assert!(notes(&log).iter().any(|n| n.1 == "on" && n.0 == at as u64));
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
