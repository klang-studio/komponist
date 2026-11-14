//! Plugins in bridges, played by the engine (PLAN WP-45 "Done when": load → sound). Every
//! `Engine::process`, `Engine::render` and direct `ExternalProcessor::process` call runs inside
//! `assert_no_alloc`, which aborts on any allocation or free.

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;
use common::*;
use komponist_engine::processor::DEADLINE_SHARE;
use komponist_engine::{
    Command as EngineCommand, Event, EventKind, LiveInput, NOTE_ID_NONE, ProcessContext,
    ProcessStatus, Processor, ProcessorError, StereoIo, TransportInfo,
};
use komponist_model::project::BUILTIN_PISYNTH;
use komponist_model::{DeviceId, DeviceRole, Meter, PluginFormat, PluginRef, Tick, TickRange};
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
        deadline: None,
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
        // Far below the plugin's 50 ms (with room for preemption on a loaded machine).
        assert!(
            took < Duration::from_millis(25),
            "a busy call waited {took:?}"
        );
        assert_eq!(output[1], input);
        busy_calls += 1;
    }
    eprintln!("slow plugin: late call, then {busy_calls} busy calls passed through");
    // Every one of them counts as late.
    assert!(manager.late_blocks(DeviceId(20)) > busy_calls);
    assert_eq!(manager.late_blocks(DeviceId(21)), 0);

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
    assert!(took < Duration::from_millis(25));
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

/// What a [`DeadlineProbe`] saw in its last call.
#[derive(Default)]
struct ProbeLog {
    /// `ctx.deadline` in nanoseconds after the test's base instant; `u64::MAX` for `None`.
    deadline_ns: AtomicU64,
    calls: AtomicU32,
}

/// An effect that passes audio through and records the deadline of its calls.
struct DeadlineProbe {
    base: Instant,
    log: Arc<ProbeLog>,
}

impl Processor for DeadlineProbe {
    fn activate(&mut self, _sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        _events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        io.passthrough();
        let ns = ctx.deadline.map_or(u64::MAX, |d| {
            d.saturating_duration_since(self.base).as_nanos() as u64
        });
        self.log.deadline_ns.store(ns, Ordering::Relaxed);
        self.log.calls.fetch_add(1, Ordering::Relaxed);
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

#[test]
fn bridges_in_series_share_the_callback_deadline() {
    let dir = scratch("engine-deadline");
    let Some(manager) = scanned_manager(&dir) else {
        return;
    };
    // Two tracks, each a kp-test-slow (sleeps 50 ms on key 126) followed by a probe; the probes are
    // in-process devices whose processors the test inserts itself.
    let probe_ref = PluginRef {
        format: PluginFormat::Builtin,
        id: BUILTIN_PISYNTH.into(),
        name: "probe".into(),
        vendor: String::new(),
        version: String::new(),
        module: Box::default(),
    };
    let tracks = (1..=2)
        .map(|t| {
            track(
                t,
                vec![
                    device(10 * t, DeviceRole::Instrument, clap_plugin(SLOW), None),
                    device(10 * t + 1, DeviceRole::Effect, probe_ref.clone(), None),
                ],
            )
        })
        .collect();
    let mut rig = Rig::new(manager, project(tracks));
    let base = Instant::now();
    let logs: Vec<Arc<ProbeLog>> = [11u64, 21]
        .iter()
        .map(|&id| {
            let log = Arc::new(ProbeLog::default());
            let slot = rig.slots[&DeviceId(id)];
            let processor = Box::new(DeadlineProbe {
                base,
                log: log.clone(),
            });
            rig.send(EngineCommand::InsertProcessor { slot, processor });
            log
        })
        .collect();
    rig.wait_status(10, DeviceStatus::Ok);
    rig.wait_status(20, DeviceStatus::Ok);
    rig.run(4);
    let late_before = rig.manager.late_blocks(DeviceId(10)) + rig.manager.late_blocks(DeviceId(20));

    // One callback of 2048 frames (42.7 ms): the deadline is 25.6 ms after its start. Both
    // bridges sleep 50 ms. With one shared deadline the callback returns after about 25.6 ms;
    // with a budget per processor it would take at least 51.2 ms.
    rig.note(1, 126, 127);
    rig.note(2, 126, 127);
    let frames = komponist_engine::MAX_BLOCK;
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let input = LiveInput {
        midi: &[],
        output_latency: 0,
    };
    let engine = &mut rig.engine;
    let before = Instant::now();
    assert_no_alloc(|| engine.process(&mut l, &mut r, &input));
    let took = before.elapsed();
    rig.poll();

    let share = Duration::from_secs_f64(frames as f64 / f64::from(RATE) * DEADLINE_SHARE);
    let deadline = |log: &ProbeLog| log.deadline_ns.load(Ordering::Relaxed);
    assert!(logs.iter().all(|log| log.calls.load(Ordering::Relaxed) > 0));
    assert_eq!(
        deadline(&logs[0]),
        deadline(&logs[1]),
        "one deadline per callback"
    );
    let seen = base + Duration::from_nanos(deadline(&logs[0]));
    let offset = seen.duration_since(before);
    assert!(
        offset >= share && offset < share + Duration::from_millis(5),
        "deadline {offset:?} after the call, expected {share:?}"
    );
    eprintln!("two late bridges in series: callback took {took:.1?}, deadline {offset:.1?}");
    assert!(took >= share - Duration::from_millis(1), "{took:?}");
    assert!(
        took < share * 3 / 2,
        "both waited their own budget: {took:?}"
    );
    let late_after = rig.manager.late_blocks(DeviceId(10)) + rig.manager.late_blocks(DeviceId(20));
    assert!(
        late_after >= late_before + 2,
        "{late_before} → {late_after}"
    );
}
