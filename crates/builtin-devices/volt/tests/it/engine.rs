//! Volt (WP-95) on the real engine: sequenced and live
//! notes, pitch bend, mod wheel and pressure from the live input reach the instrument, and the
//! output is bit-exact with and without workers (WP-65).

use komponist_device_core::InternalProcessor;
use komponist_engine::{Command, MidiInEvent, MidiPort};
use komponist_model::params::ParamId;
use komponist_model::{Tick, TickRange};
use komponist_volt::{SPEC, dsp};

use crate::common::*;

/// `tracks` volt tracks (preset `i`), looped, the track `monitored` listens to the live input.
fn rig(workers: usize, tracks: u32, monitored: u32) -> Rig {
    rig_with(workers, tracks, monitored, &[])
}

/// As [`rig`], with parameters changed on every track (by model id).
fn rig_with(workers: usize, tracks: u32, monitored: u32, tweak: &[(&str, f64)]) -> Rig {
    let mut rig = Rig::new(RATE, 256);
    assert_eq!(rig.engine.set_workers(workers, None).threads, workers);
    let mut list = Vec::new();
    for i in 0..tracks {
        let mut t = track(u64::from(i) + 1, i, crate::material::material(i));
        t.monitor = i == monitored;
        list.push(t);
        let mut values = SPEC
            .preset_values(i as usize % SPEC.presets().len())
            .unwrap();
        for (key, value) in tweak {
            let wire = SPEC.wire(&ParamId::new(*key)).unwrap();
            values[wire as usize] = *value;
        }
        let mut p = InternalProcessor::from_dsp(&SPEC, &values, dsp());
        komponist_processor::Processor::activate(&mut p, RATE, 256).unwrap();
        rig.insert(i, p);
    }
    let mut p = program(127.0, list);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(2 * BAR));
    p.loop_on = true;
    rig.set_program(&p);
    rig.send(Command::Play);
    rig
}

fn live(data: [u8; 3], frame: u32) -> MidiInEvent {
    MidiInEvent {
        frame,
        port: MidiPort(0),
        data,
    }
}

/// 12 tracks with live notes, bends, wheel and pressure on one of them, a locate on the way.
fn many(workers: usize) -> (Vec<f32>, Vec<f32>) {
    let mut rig = rig(workers, 12, 3);
    let blocks = [256, 100, 512, 37, 2048, 3000, 64];
    for call in 0..60usize {
        let n = blocks[call % blocks.len()];
        let key = 48 + (call % 12) as u8;
        let mut midi = vec![live([0x90, key, 100], (n / 3) as u32)];
        match call % 4 {
            0 => midi.push(live([0xE0, 0, 0x60 + (call % 30) as u8], (n / 2) as u32)),
            1 => midi.push(live([0xB0, 1, (call * 5 % 128) as u8], (n / 2) as u32)),
            2 => midi.push(live([0xD0, (call * 7 % 128) as u8, 0], (n / 2) as u32)),
            _ => midi.push(live([0x80, key, 0], (n / 2) as u32)),
        }
        rig.process_with(n, &midi, 0);
        if call == 30 {
            rig.send(Command::Locate(Tick(BAR / 3)));
        }
    }
    rig.render(20_000, 333);
    (rig.l, rig.r)
}

#[test]
fn output_with_workers_is_bit_exact_with_the_single_threaded_engine() {
    let single = many(0);
    assert!(single.0.iter().any(|&s| s != 0.0));
    assert!(single.0.iter().chain(&single.1).all(|s| s.is_finite()));
    for workers in [2, 4] {
        let parallel = many(workers);
        let same = |a: &[f32], b: &[f32]| {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
        };
        assert!(same(&parallel.0, &single.0), "left, {workers} workers");
        assert!(same(&parallel.1, &single.1), "right, {workers} workers");
    }
}

#[test]
fn the_live_wheels_reach_the_instrument_through_the_engine() {
    // One track on the live input; the same note with and without a bend, wheel and pressure.
    let play = |extra: Vec<MidiInEvent>| {
        // Pressure and the wheel are routed: pressure opens the filter, the wheel adds vibrato.
        let tweak = [("at_cut", 0.8), ("wheel_vib", 200.0), ("lfo1_fade", 0.001)];
        let mut rig = rig_with(0, 1, 0, &tweak);
        rig.process_with(512, &[live([0x90, 60, 100], 10)], 0);
        rig.process_with(512, &extra, 0);
        rig.render(8192, 512);
        rig.l
    };
    let plain = play(vec![]);
    assert!(plain.iter().any(|&s| s != 0.0));
    for (name, event) in [
        ("pitch bend", live([0xE0, 0, 0x7F], 0)),
        ("pressure", live([0xD0, 127, 0], 0)),
        ("mod wheel", live([0xB0, 1, 127], 0)),
    ] {
        let moved = play(vec![event]);
        assert!(
            plain
                .iter()
                .zip(&moved)
                .any(|(a, b)| a.to_bits() != b.to_bits()),
            "{name} changed nothing"
        );
    }
}
