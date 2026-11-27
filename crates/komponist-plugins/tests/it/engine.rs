//! Plugins in bridges, played by the engine (PLAN WP-45 "Done when": load → sound). Every
//! `Engine::process`, `Engine::render` and direct `ExternalProcessor::process` call runs inside
//! `assert_no_alloc`, which aborts on any allocation or free.

use crate::common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use assert_no_alloc::assert_no_alloc;
use common::*;
use komponist_engine::{Command as EngineCommand, LiveInput};
use komponist_model::{DeviceId, DeviceRole, Meter, PluginFormat, PluginRef, Tick, TickRange};
use komponist_plugins::{DeviceStatus, PluginManagerEvent};
use komponist_processor::processor::DEADLINE_SHARE;
use komponist_processor::{
    Event, EventKind, NOTE_ID_NONE, ProcessContext, ProcessStatus, Processor, ProcessorError,
    StereoIo, TransportInfo,
};

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
    // The synth has its own GUI everywhere (X11 on Linux; Cocoa or Win32 in the bridge's frame on
    // macOS and Windows, #52 and port W6).
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

    // The calls below run on a realtime thread, as the device's audio thread does: a timeshare
    // thread's deadline wait gets the process's timer leeway, tens of milliseconds on a
    // QoS-clamped process such as the GitHub macOS runner (a busy call took 27 ms there, #101).
    #[cfg(target_os = "macos")]
    komponist_rt::policy::set_current_thread_time_constraint(Duration::from_secs_f64(
        BLOCK as f64 / f64::from(RATE),
    ))
    .expect("time-constraint policy");

    // Wait for a normal block to complete before submitting the note that stalls the effect.
    let ready_by = Instant::now() + PATIENCE;
    let late_before = loop {
        let late = manager.late_blocks(slow.id);
        let (status, _) = call(&mut *fx, &input, &mut output, &[]);
        assert_eq!(status, ProcessStatus::Continue);
        assert_eq!(output[0], input);
        if manager.late_blocks(slow.id) == late {
            break late;
        }
        assert!(Instant::now() < ready_by, "the slow fixture did not settle");
        std::thread::sleep(Duration::from_millis(5));
    };

    // Each note 126 sleeps for 50 ms. Two notes leave room to probe the busy bridge after
    // the first callback returns at its deadline, even if the runner schedules it late.
    let stall = [note(0, 126, true), note(1, 126, true)];
    let earliest_completion = Instant::now() + Duration::from_millis(100);
    let (status, took) = call(&mut *fx, &input, &mut output, &stall);
    assert_eq!(status, ProcessStatus::Continue);
    assert!(took < Duration::from_millis(40), "waited {took:?}");
    assert_eq!(output[0], input);
    assert_eq!(manager.late_blocks(slow.id), late_before + 1);

    let mut busy_calls = 0;
    let probe_until = Instant::now() + Duration::from_millis(30);
    while Instant::now() < probe_until {
        let late = manager.late_blocks(slow.id);
        let (status, took) = call(&mut *fx, &input, &mut output, &[note(0, 126, false)]);
        if Instant::now() >= earliest_completion {
            // A scheduler pause may have let the fixture finish before this call.
            break;
        }
        assert_eq!(status, ProcessStatus::Continue);
        assert!(
            took < Duration::from_millis(25),
            "a busy call waited {took:?}"
        );
        assert_eq!(output[1], input);
        assert_eq!(manager.late_blocks(slow.id), late + 1);
        busy_calls += 1;
    }
    assert!(busy_calls > 0, "no call observed the stalled plugin");
    eprintln!("slow plugin: late call, then {busy_calls} busy calls passed through");
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

    // The engine re-activates its processors; the bridge follows in the background. The device
    // reads `Loading` at once (before any poll) and until the bridge confirmed and warmed up.
    rig.events.clear();
    rig.engine.set_sample_rate(44_100.0).unwrap();
    assert_eq!(rig.manager.status(DeviceId(30)), DeviceStatus::Loading);
    let deadline = Instant::now() + PATIENCE;
    while rig.manager.status(DeviceId(30)) != DeviceStatus::Ok {
        assert!(Instant::now() < deadline, "no Ok after the rate change");
        rig.render(BLOCK);
        std::thread::sleep(Duration::from_millis(2));
    }
    let statuses: Vec<_> = rig
        .events
        .iter()
        .filter_map(|e| match e {
            PluginManagerEvent::StatusChanged { status, .. } => Some(status.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(statuses, [DeviceStatus::Loading, DeviceStatus::Ok]);
    // `Ok` means it runs at the new rate: the first note sounds, at 44.1 kHz.
    rig.note(1, 69, 127);
    let range = rig.render(8 * BLOCK);
    rig.note(1, 69, 0);
    rig.render(8 * BLOCK);
    assert!(rig.peak(range.clone()) > 0.1, "silent after Ok");
    let hz = frequency(&rig.l[range], 44_100.0);
    assert!((430.0..450.0).contains(&hz), "{hz} Hz: still at 48 kHz?");
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
        format: PluginFormat::Clap,
        id: "fixture.probe".into(),
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
    let late_blocks = |rig: &Rig| [10, 20].map(|id| rig.manager.late_blocks(DeviceId(id)));
    // A realtime warm-up block can time out with its request still in flight. Wait for both
    // bridges to complete an offline block so the measured call can submit both requests.
    let warm_up_deadline = Instant::now() + PATIENCE;
    let late_before = loop {
        let before = late_blocks(&rig);
        rig.render(BLOCK);
        let after = late_blocks(&rig);
        if after == before {
            break after;
        }
        assert!(Instant::now() < warm_up_deadline, "bridges stayed busy");
        std::thread::sleep(Duration::from_millis(1));
    };

    for log in &logs {
        log.calls.store(0, Ordering::Relaxed);
    }
    // Both bridges sleep longer than the 25.6 ms budget of this 2048-frame callback.
    rig.note(1, 126, 127);
    rig.note(2, 126, 127);
    let frames = komponist_engine::MAX_BLOCK;
    let (mut l, mut r) = (vec![0.0f32; frames], vec![0.0f32; frames]);
    let input = LiveInput {
        midi: &[],
        output_latency: 0,
    };
    // The callback runs on a realtime thread, as the device's audio thread does. A timeshare
    // thread's deadline wait gets the process's timer leeway, which on a QoS-clamped process (the
    // GitHub macOS runner) is tens of milliseconds: the callback took 40 ms there (#94).
    #[cfg(target_os = "macos")]
    komponist_rt::policy::set_current_thread_time_constraint(Duration::from_secs_f64(
        frames as f64 / f64::from(RATE),
    ))
    .expect("time-constraint policy");
    #[cfg(windows)]
    komponist_rt::policy::set_current_thread_mmcss().expect("audio thread priority");
    let engine = &mut rig.engine;
    let before = Instant::now();
    assert_no_alloc(|| engine.process(&mut l, &mut r, &input));
    let after = Instant::now();
    rig.poll();

    let share = Duration::from_secs_f64(frames as f64 / f64::from(RATE) * DEADLINE_SHARE);
    let deadline = |log: &ProbeLog| log.deadline_ns.load(Ordering::Relaxed);
    assert!(logs.iter().all(|log| log.calls.load(Ordering::Relaxed) > 0));
    assert_eq!(
        deadline(&logs[0]),
        deadline(&logs[1]),
        "one deadline per callback"
    );
    assert_ne!(deadline(&logs[0]), u64::MAX, "missing callback deadline");
    let seen = base + Duration::from_nanos(deadline(&logs[0]));
    assert!(
        (before + share..=after + share).contains(&seen),
        "deadline {seen:?} is not {share:?} after an instant in {before:?}..={after:?}"
    );
    assert!(
        after + Duration::from_millis(1) >= seen,
        "callback returned before the bridges' deadline"
    );
    // A descheduled test can spend arbitrarily long between the two probes. The
    // external processor's wait deadline is checked directly in its unit test.
    assert_eq!(
        late_blocks(&rig),
        late_before.map(|count| count + 1),
        "both bridges missed the callback deadline"
    );
}

/// WP-65: bridged instruments at the head of a chain start before the engine runs any chain
/// (`Processor::begin`) and are completed by whichever thread runs their track. Offline, four
/// synth tracks (each followed by a bridged effect) render bit-exact the same with two worker
/// threads as single-threaded; in realtime the blocks reach the bridges.
#[test]
fn bridged_instruments_with_workers_match_single_threaded() {
    let play = |workers: usize| -> Option<(Vec<f32>, Vec<f32>, usize)> {
        let dir = scratch(&format!("engine-workers-{workers}"));
        let manager = scanned_manager(&dir)?;
        let tracks = (1..=4)
            .map(|t| {
                track(
                    t,
                    vec![
                        device(10 * t, DeviceRole::Instrument, clap_plugin(SYNTH), None),
                        device(10 * t + 1, DeviceRole::Effect, clap_plugin(FX), None),
                    ],
                )
            })
            .collect();
        let mut rig = Rig::new(manager, project(tracks));
        assert_eq!(rig.engine.set_workers(workers, None).threads, workers);
        for t in 1..=4 {
            rig.wait_status(10 * t, DeviceStatus::Ok);
            rig.wait_status(10 * t + 1, DeviceStatus::Ok);
        }
        rig.recompile();
        rig.render(BLOCK);
        for t in 1..=4u8 {
            rig.note(u64::from(t), 57 + 3 * t, 127);
        }
        let range = rig.render(16 * BLOCK);
        let out = (rig.l[range.clone()].to_vec(), rig.r[range].to_vec());
        for t in 1..=4u8 {
            rig.note(u64::from(t), 57 + 3 * t, 0);
        }
        rig.render(8 * BLOCK);
        // Realtime: most blocks make the 60 % deadline (the machine may be loaded).
        for t in 1..=4u8 {
            rig.note(u64::from(t), 60 + t, 127);
        }
        let blocks = 40;
        let range = rig.run(blocks);
        let loud = range
            .step_by(BLOCK)
            .filter(|&b| rig.peak(b..b + BLOCK) > 0.01)
            .count();
        assert!(rig.engine_events.is_empty(), "{:?}", rig.engine_events);
        Some((out.0, out.1, loud))
    };
    let Some(single) = play(0) else {
        return;
    };
    let parallel = play(2).expect("fixtures found once");
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    assert!(single.0.iter().any(|x| x.abs() > 0.01), "silent");
    assert_eq!(bits(&single.0), bits(&parallel.0), "left");
    assert_eq!(bits(&single.1), bits(&parallel.1), "right");
    eprintln!(
        "realtime: {} (single) and {} (2 workers) of 40 blocks with sound",
        single.2, parallel.2
    );
    assert!(
        single.2 > 20 && parallel.2 > 20,
        "{} {}",
        single.2,
        parallel.2
    );
}
