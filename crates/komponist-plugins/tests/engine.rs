//! Plugins in bridges, played by the engine (PLAN WP-45 "Done when": load → sound). Every
//! `Engine::process`, `Engine::render` and direct `ExternalProcessor::process` call runs inside
//! `assert_no_alloc`, which aborts on any allocation or free.

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;

use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;
use common::*;
use komponist_engine::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, ProcessStatus, Processor, StereoIo,
    TransportInfo,
};
use komponist_model::{DeviceId, DeviceRole, Meter, Tick, TickRange};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};

/// Zero crossings per second of `samples` at `rate`, halved: the frequency of a sine.
fn frequency(samples: &[f32], rate: f32) -> f32 {
    let crossings = samples
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    crossings as f32 / 2.0 / (samples.len() as f32 / rate)
}

#[test]
fn synth_and_effect_sound_through_the_engine() {
    let dir = scratch("engine-sound");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(10, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let fx = device(11, DeviceRole::Effect, clap_plugin(FX), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth, fx])]));
    assert_eq!(rig.manager.status(DeviceId(10)), DeviceStatus::Loading);
    assert_eq!(rig.manager.status(DeviceId(11)), DeviceStatus::Loading);

    // While loading the engine runs on (fallbacks, no allocation) and stays silent.
    rig.note(1, 69, 127);
    let loading = rig.run(4);
    assert_eq!(rig.peak(loading), 0.0);
    rig.note(1, 69, 0);

    rig.wait_status(10, DeviceStatus::Ok);
    rig.wait_status(11, DeviceStatus::Ok);
    assert!(rig.events.contains(&PluginManagerEvent::StatusChanged {
        device: DeviceId(10),
        status: DeviceStatus::Ok,
    }));
    assert!(rig.events.contains(&PluginManagerEvent::LatencyChanged {
        device: DeviceId(11),
        latency: 64,
    }));
    assert_eq!(rig.manager.latency(DeviceId(11)), 64);
    assert_eq!(rig.manager.latency(DeviceId(10)), 0);
    let params: Vec<&str> = rig
        .manager
        .params(DeviceId(10))
        .unwrap()
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    assert_eq!(params, ["Gain", "Decay"]);
    assert!(rig.manager.has_gui(DeviceId(10)));
    assert!(!rig.manager.has_gui(DeviceId(11)));
    rig.recompile();

    // Offline render: no deadline, so every block reaches the bridges. A4 at velocity 127 and
    // −12 dB, delayed by the effect's 64 frames.
    rig.render(BLOCK);
    rig.note(1, 69, 127);
    let range = rig.render(8 * BLOCK);
    let start = range.start;
    assert_eq!(rig.peak(start..start + 64), 0.0, "the effect delays by 64");
    let level = rig.peak(start + 64..range.end);
    assert!((0.2..0.3).contains(&level), "level {level}");
    let hz = frequency(&rig.l[start + 64..range.end], RATE);
    assert!((430.0..450.0).contains(&hz), "{hz} Hz");
    rig.note(1, 69, 0);
    let release = rig.render(8 * BLOCK);
    let tail = release.end - BLOCK..release.end;
    assert!(rig.peak(tail) < 1e-4);

    // Realtime: blocks with the 60 % deadline (a late block is silent: the instrument's fallback).
    // The machine may be loaded, so only most blocks need to make it. The note decays by 60 dB/s.
    rig.note(1, 72, 127);
    let blocks = 50;
    let range = rig.run(blocks);
    let loud = range
        .clone()
        .step_by(BLOCK)
        .filter(|&b| rig.peak(b..b + BLOCK) > 0.01)
        .count();
    let pattern: String = range
        .clone()
        .step_by(BLOCK)
        .map(|b| {
            if rig.peak(b..b + BLOCK) > 0.01 {
                '#'
            } else {
                '.'
            }
        })
        .collect();
    eprintln!("realtime: {loud} of {blocks} blocks with sound: {pattern}");
    assert!(loud > blocks / 2, "{loud} of {blocks}");
    rig.note(1, 72, 0);
    rig.run(4);
    assert!(rig.engine_events.is_empty(), "{:?}", rig.engine_events);
    assert_eq!(rig.manager.status(DeviceId(10)), DeviceStatus::Ok);
}

/// A context for direct processor calls.
fn context(frames: usize) -> ProcessContext {
    ProcessContext {
        frames: frames as u32,
        sample_rate: RATE,
        transport: TransportInfo {
            playing: true,
            recording: false,
            looping: false,
            tempo_bpm: 120.0,
            meter: Meter::FOUR_FOUR,
            position_ticks: 0.0,
            loop_range: TickRange::new(Tick(0), Tick(3840)),
        },
        offline: false,
    }
}

fn note(offset: u32, key: u8, on: bool) -> Event {
    let kind = if on {
        EventKind::NoteOn {
            key,
            velocity: 127,
            channel: 0,
            note_id: NOTE_ID_NONE,
        }
    } else {
        EventKind::NoteOff {
            key,
            channel: 0,
            note_id: NOTE_ID_NONE,
        }
    };
    Event { offset, kind }
}

/// One direct `process` call inside `assert_no_alloc`; returns the status and the call's time.
fn call(
    processor: &mut dyn Processor,
    input: &[f32],
    output: &mut [Vec<f32>; 2],
    events: &[Event],
) -> (ProcessStatus, Duration) {
    let ctx = context(input.len());
    let [l, r] = output;
    let mut io = StereoIo {
        input: [input, input],
        output: [&mut l[..], &mut r[..]],
    };
    let t0 = Instant::now();
    let status = assert_no_alloc(|| processor.process(&ctx, events, &mut io));
    (status, t0.elapsed())
}

#[test]
fn fallbacks_while_loading_late_and_busy() {
    let dir = scratch("engine-fallback");
    let Some(mut manager) = scanned_manager(&dir) else {
        return;
    };
    let input: Vec<f32> = (0..BLOCK).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
    let mut output = [vec![9.0; BLOCK], vec![9.0; BLOCK]];

    // Loading: the effect passes through, the instrument is silent.
    let slow = device(20, DeviceRole::Effect, clap_plugin(SLOW), None);
    let mut fx = manager.create_processor(&slow, SETUP).unwrap();
    let synth = device(21, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut inst = manager.create_processor(&synth, SETUP).unwrap();
    assert_eq!(fx.latency(), 0);
    let (status, _) = call(&mut *fx, &input, &mut output, &[note(0, 60, true)]);
    assert_eq!(status, ProcessStatus::Continue);
    assert_eq!(output[0], input);
    assert_eq!(output[1], input);
    let (status, _) = call(&mut *inst, &input, &mut output, &[note(0, 60, true)]);
    assert_eq!(status, ProcessStatus::Continue);
    assert!(output.iter().all(|c| c.iter().all(|&s| s == 0.0)));

    let deadline = Instant::now() + PATIENCE;
    while manager.status(DeviceId(20)) != DeviceStatus::Ok
        || manager.status(DeviceId(21)) != DeviceStatus::Ok
    {
        assert!(Instant::now() < deadline, "not loaded");
        manager.poll();
        std::thread::sleep(Duration::from_millis(5));
    }

    // Running: kp-test-slow passes audio through itself.
    let (status, _) = call(&mut *fx, &input, &mut output, &[]);
    assert_eq!(status, ProcessStatus::Continue);
    assert_eq!(output[0], input);

    // Key 126 makes it sleep 50 ms in `process`: the call returns at its deadline (60 % of
    // 5.3 ms) with the dry input, and the next calls find the bridge busy and pass through too.
    let (status, took) = call(&mut *fx, &input, &mut output, &[note(0, 126, true)]);
    assert_eq!(status, ProcessStatus::Continue);
    assert!(took < Duration::from_millis(40), "waited {took:?}");
    assert_eq!(output[0], input);
    let mut busy_calls = 0;
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(30) {
        let (status, took) = call(&mut *fx, &input, &mut output, &[note(0, 126, false)]);
        assert_eq!(status, ProcessStatus::Continue);
        assert!(
            took < Duration::from_millis(5),
            "a busy call waited {took:?}"
        );
        assert_eq!(output[1], input);
        busy_calls += 1;
    }
    eprintln!("slow plugin: late call, then {busy_calls} busy calls passed through");

    // The instrument plays, and a reset silences it at the next block.
    let silence = vec![0.0; BLOCK];
    let mut level = 0.0f32;
    for _ in 0..20 {
        call(&mut *inst, &silence, &mut output, &[note(0, 69, true)]);
        level = output[0].iter().fold(0.0, |m, s| m.max(s.abs()));
        if level > 0.1 {
            break;
        }
    }
    assert!(level > 0.1, "synth level {level}");
    assert_no_alloc(|| inst.reset());
    let mut reset_ok = false;
    for _ in 0..20 {
        call(&mut *inst, &silence, &mut output, &[]);
        if output[0].iter().all(|&s| s == 0.0) {
            reset_ok = true;
            break;
        }
    }
    assert!(reset_ok, "reset did not silence the synth");

    // Released devices: the processors fail over to the fallback (no blocking, no allocation).
    manager.release(DeviceId(20));
    manager.release(DeviceId(21));
    let (status, took) = call(&mut *fx, &input, &mut output, &[]);
    assert_eq!(status, ProcessStatus::Failed);
    assert!(took < Duration::from_millis(5));
    assert_eq!(output[0], input);
    let (status, _) = call(&mut *inst, &input, &mut output, &[]);
    assert_eq!(status, ProcessStatus::Failed);
    assert!(output[0].iter().all(|&s| s == 0.0));
    manager.shutdown();
}

#[test]
fn sample_rate_change_reactivates_the_bridge() {
    let dir = scratch("engine-rate");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    let synth = device(30, DeviceRole::Instrument, clap_plugin(SYNTH), None);
    let mut rig = Rig::new(manager, project(vec![track(1, vec![synth])]));
    rig.wait_status(30, DeviceStatus::Ok);

    // The engine re-activates its processors; the bridge follows in the background.
    rig.engine.set_sample_rate(44_100.0).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        rig.note(1, 69, 127);
        let range = rig.render(8 * BLOCK);
        rig.note(1, 69, 0);
        rig.render(8 * BLOCK);
        if rig.peak(range.clone()) > 0.1 {
            let hz = frequency(&rig.l[range], 44_100.0);
            assert!((430.0..450.0).contains(&hz), "{hz} Hz: still at 48 kHz?");
            break;
        }
        assert!(Instant::now() < deadline, "no sound after the rate change");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(rig.manager.status(DeviceId(30)), DeviceStatus::Ok);
}
