//! The Arpeggiator and Tonleiter through the engine.

use komponist_builtin::Internal;
use komponist_engine::testing::TestSine;
use komponist_engine::{Command, SlotId};
use komponist_model::params::ParamId;
use komponist_model::{Tempo, TempoMap, Tick, TickRange};

use crate::common::*;

/// An activated internal device with `values` by parameter key.
fn device(kind: Internal, values: &[(&str, f64)]) -> komponist_device_core::InternalProcessor {
    let mut all = kind.defaults();
    for (key, value) in values {
        all[kind.wire(&ParamId::new(*key)).unwrap() as usize] = *value;
    }
    let mut p = kind.processor(&all);
    komponist_engine::Processor::activate(&mut p, RATE, 512).unwrap();
    p
}

fn rig(
    arp: komponist_device_core::InternalProcessor,
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
        let mut t = track(u64::from(i) + 1, i, crate::material::material(i));
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
