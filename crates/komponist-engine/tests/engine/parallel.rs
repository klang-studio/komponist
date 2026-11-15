//! Parallel track processing (WP-65): the output with worker threads is bit-exact with the
//! single-threaded engine (realtime and offline, test processors and hexa), chains that share
//! a slot run serially, bridge-style processors are started early (`Processor::begin`) and their
//! tracks run last, and the pool can be changed and switched off.
//!
//! Every `process` / `render` call runs inside `assert_no_alloc` (the rig); the workers run their
//! jobs inside `assert_no_alloc` themselves, so an allocation on a worker aborts too.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use komponist_builtin::{HexaProcessor, Internal, preset_patch, presets};
use komponist_engine::testing::{TestGain, TestSine};
use komponist_engine::{
    Command, Event, MidiInEvent, MidiPort, ProcessContext, ProcessStatus, Processor,
    ProcessorError, Program, StereoIo,
};
use komponist_model::{Tick, TickRange, TrackId};

use crate::bench::material;
use crate::common::*;

/// A program and the processors of its slots.
type Setup = (Program, Vec<(u32, Box<dyn Processor>)>);

/// Tracks of the test-processor material.
const TRACKS: u32 = 12;

/// 12 tracks: a `TestSine` instrument (slot i) and a `TestGain` effect (slot 100 + i) with a
/// latency on every third track (delay compensation), varied gain, pan, mute and solo; looped.
fn sines() -> Setup {
    let mut tracks = Vec::new();
    let mut processors: Vec<(u32, Box<dyn Processor>)> = Vec::new();
    for i in 0..TRACKS {
        let mut t = track(u64::from(i) + 1, i, material(i));
        t.chain.push(effect(100 + i));
        t.gain = 0.2 + 0.05 * i as f32;
        t.pan = (i as f32 / TRACKS as f32) * 2.0 - 1.0;
        t.mute = i == 5;
        t.solo = false;
        t.monitor = i == 2;
        let latency = if i % 3 == 0 { 64 * (i + 1) } else { 0 };
        t.pdc_delay = 64 * 13 - latency;
        tracks.push(t);
        processors.push((i, Box::new(TestSine::new())));
        processors.push((
            100 + i,
            Box::new(TestGain::with_latency(0.5 + 0.01 * i as f32, latency)),
        ));
    }
    let mut p = program(127.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(2 * BAR));
    p.loop_on = true;
    p.metronome = true;
    (p, processors)
}

/// 8 hexa tracks with the bench's stress material (presets E-Piano, Strings, Supersaw, Init).
fn hexas() -> Setup {
    let names = ["E-Piano", "Strings", "Supersaw", "Init"];
    let mut processors: Vec<(u32, Box<dyn Processor>)> = Vec::new();
    let tracks = (0..8)
        .map(|i| {
            let preset = presets().find(|x| x.name == names[i as usize % 4]).unwrap();
            let mut p = HexaProcessor::new(preset_patch(preset.index).unwrap());
            p.activate(RATE, 256).unwrap();
            processors.push((i, Box::new(p)));
            track(u64::from(i) + 1, i, material(i))
        })
        .collect();
    let mut p = program(120.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(4 * BAR));
    p.loop_on = true;
    (p, processors)
}

/// 8 tracks, each a `TestSine` and five internal effects in a chain (EQ with active bands,
/// tempo-synced Delay, Hall, the compressor `presse` with lookahead, SC-HP, partial link and mix,
/// lookahead Limiter; WP-90, WP-93), so the transport tempo, the latencies and the effects'
/// control-rate counters all go through the worker pool.
fn effects() -> Setup {
    let mut tracks = Vec::new();
    let mut processors: Vec<(u32, Box<dyn Processor>)> = Vec::new();
    for i in 0..8u32 {
        let mut t = track(u64::from(i) + 1, i, material(i));
        let mut eq = Internal::Eq.defaults();
        eq[Internal::Eq.wire(&"b1_type".into()).unwrap() as usize] = 0.0;
        eq[Internal::Eq.wire(&"b3_gain".into()).unwrap() as usize] = 6.0 - f64::from(i);
        let mut delay = Internal::Delay.defaults();
        delay[Internal::Delay.wire(&"sync".into()).unwrap() as usize] = 1.0;
        delay[Internal::Delay.wire(&"note".into()).unwrap() as usize] = f64::from(2 + i % 6);
        delay[Internal::Delay.wire(&"mix".into()).unwrap() as usize] = 0.5;
        let mut limiter = Internal::Limiter.defaults();
        limiter[Internal::Limiter.wire(&"gain".into()).unwrap() as usize] = 12.0;
        let mut presse = Internal::Presse.defaults();
        for (key, value) in [
            ("threshold", -30.0),
            ("ratio", 6.0),
            ("detector", f64::from(i % 2)),
            ("link", 0.5),
            ("lookahead", 1.0),
            ("sc_hpf", 100.0),
            ("mix", 0.8),
        ] {
            presse[Internal::Presse.wire(&key.into()).unwrap() as usize] = value;
        }
        let chain = [
            (Internal::Eq, eq),
            (Internal::Delay, delay),
            (Internal::Hall, Internal::Hall.defaults()),
            (Internal::Presse, presse),
            (Internal::Limiter, limiter),
        ];
        for (k, (kind, values)) in chain.into_iter().enumerate() {
            let slot = 100 + i * 5 + k as u32;
            let mut p = kind.processor(&values);
            p.activate(RATE, 256).unwrap();
            t.chain.push(effect(slot));
            processors.push((slot, Box::new(p)));
        }
        // All tracks have the same latency (the lookaheads): nothing to compensate.
        t.pdc_delay = 0;
        tracks.push(t);
        processors.push((i, Box::new(TestSine::new())));
    }
    let mut p = program(127.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(2 * BAR));
    p.loop_on = true;
    (p, processors)
}

/// Plays `make`'s program with `workers` worker threads: `frames` frames in calls of varying
/// length (with live MIDI, a locate and a solo change on the way) and then an offline render; returns
/// the output.
fn play(workers: usize, frames: usize, make: fn() -> Setup) -> (Vec<f32>, Vec<f32>) {
    let mut rig = Rig::new(RATE, 256);
    let status = rig.engine.set_workers(workers, None);
    assert_eq!(status.threads, workers);
    assert_eq!(rig.engine.workers(), workers);
    let (program, processors) = make();
    for (slot, processor) in processors {
        rig.send(Command::InsertProcessor {
            slot: komponist_engine::SlotId(slot),
            processor,
        });
    }
    rig.set_program(&program);
    rig.send(Command::Play);
    let blocks = [256, 256, 100, 512, 37, 2048, 3000];
    let mut done = 0;
    let mut call = 0;
    while done < frames {
        let n = blocks[call % blocks.len()].min(frames - done);
        let midi = [MidiInEvent {
            frame: (n / 2) as u32,
            port: MidiPort(0),
            data: [0x90, 60 + (call % 12) as u8, 100],
        }];
        rig.process_with(n, &midi, 0);
        match call {
            7 => rig.send(Command::Locate(Tick(BAR / 3))),
            11 => rig.send(Command::SetTrackSolo {
                track: TrackId(4),
                solo: true,
            }),
            15 => rig.send(Command::SetTrackSolo {
                track: TrackId(4),
                solo: false,
            }),
            _ => {}
        }
        done += n;
        call += 1;
    }
    rig.render(frames / 2, 333);
    rig.send(Command::Stop);
    rig.process(1024, 256);
    // Not vacuous: the workers took part.
    let stats = rig.engine.worker_stats();
    assert_eq!(
        stats.worker_jobs() > 0,
        workers > 0,
        "{}",
        stats.worker_jobs()
    );
    (rig.l, rig.r)
}

fn assert_bit_exact(a: &(Vec<f32>, Vec<f32>), b: &(Vec<f32>, Vec<f32>), what: &str) {
    assert_eq!(a.0.len(), b.0.len(), "{what}: length");
    let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<u32>>();
    for (channel, (x, y)) in [(&a.0, &b.0), (&a.1, &b.1)].into_iter().enumerate() {
        let (x, y) = (bits(x), bits(y));
        if let Some(i) = x.iter().zip(&y).position(|(p, q)| p != q) {
            panic!("{what}: channel {channel} differs first at frame {i}");
        }
    }
    // Not vacuous: the material sounds.
    assert!(a.0.iter().any(|x| x.abs() > 0.01), "{what}: silent");
}

#[test]
fn test_processors_are_bit_exact_with_workers() {
    let frames = 3 * 48_000;
    let single = play(0, frames, sines);
    for workers in [1, 2, 4] {
        let parallel = play(workers, frames, sines);
        assert_bit_exact(&single, &parallel, &format!("{workers} workers"));
    }
}

#[test]
fn internal_effects_are_bit_exact_with_workers() {
    let frames = 2 * 48_000;
    let single = play(0, frames, effects);
    for workers in [2, 4] {
        let parallel = play(workers, frames, effects);
        assert_bit_exact(&single, &parallel, &format!("effects, {workers} workers"));
    }
}

#[test]
fn hexa_is_bit_exact_with_workers() {
    let frames = 48_000;
    let single = play(0, frames, hexas);
    let parallel = play(3, frames, hexas);
    assert_bit_exact(&single, &parallel, "hexa, 3 workers");
}

/// Two tracks whose chains share an effect slot cannot run in parallel: the engine runs them
/// serially (the same output as without workers) and dispatches nothing to the pool.
#[test]
fn shared_slots_run_serially() {
    let run = |workers: usize| {
        let mut rig = Rig::new(RATE, 256);
        rig.engine.set_workers(workers, None);
        let stats = rig.engine.worker_stats();
        let mut a = track(1, 1, vec![note(0, BAR, 60)]);
        let mut b = track(2, 2, vec![note(0, BAR, 67)]);
        a.chain.push(effect(9));
        b.chain.push(effect(9));
        rig.insert(1, TestSine::new());
        rig.insert(2, TestSine::new());
        rig.insert(9, TestGain::with_latency(0.5, 3));
        rig.set_program(&program(120.0, vec![a, b]));
        rig.send(Command::Play);
        rig.process(48_000, 256);
        (rig.l, rig.r, stats.batches())
    };
    let (l0, r0, _) = run(0);
    let (l2, r2, batches) = run(2);
    assert_eq!(batches, 0);
    assert!(l0.iter().zip(&l2).all(|(a, b)| a.to_bits() == b.to_bits()));
    assert!(r0.iter().zip(&r2).all(|(a, b)| a.to_bits() == b.to_bits()));
}

#[test]
fn workers_can_be_changed_and_switched_off() {
    let mut rig = Rig::new(RATE, 256);
    assert_eq!(rig.engine.workers(), 0);
    let stats = rig.engine.worker_stats();
    let status = rig.engine.set_workers(2, None);
    assert_eq!((status.threads, status.priority), (2, None));
    assert_eq!(status.realtime_error, None);
    // The same setting keeps the pool.
    assert_eq!(rig.engine.set_workers(2, None), status);
    let (p, processors) = sines();
    for (slot, processor) in processors {
        rig.send(Command::InsertProcessor {
            slot: komponist_engine::SlotId(slot),
            processor,
        });
    }
    rig.set_program(&p);
    rig.send(Command::Play);
    rig.process(48_000, 256);
    let batches = stats.batches();
    assert!(batches >= 48_000 / 256, "{batches}");
    assert_eq!(rig.engine.set_workers(99, None).threads, 15);
    rig.process(4096, 256);
    assert_eq!(rig.engine.set_workers(0, None).threads, 0);
    assert_eq!(rig.engine.workers(), 0);
    let before = stats.batches();
    rig.process(4096, 256);
    assert_eq!(
        stats.batches(),
        before,
        "single-threaded: nothing dispatched"
    );
    // No assertion on `stats.late()`: whether a batch of 15 workers misses its period depends on
    // the machine's load (it did once during a gate next to two builds); the late path has its own
    // test in `workers.rs`.
}

/// Records how the engine drives a bridge-like instrument: `begin`, then `process` of the same
/// call; and the order in which tracks are processed.
struct Kickable {
    track: u32,
    begins: Arc<AtomicUsize>,
    /// `track` of every process call, in call order (packed: 4 bits per call, newest lowest).
    order: Arc<AtomicU32>,
    begun: Option<(u32, usize)>,
    inner: TestSine,
    kick: bool,
}

impl Processor for Kickable {
    fn activate(&mut self, sample_rate: f32, max_block: usize) -> Result<(), ProcessorError> {
        self.inner.activate(sample_rate, max_block)
    }

    fn begin(&mut self, ctx: &ProcessContext, events: &[Event], input: [&[f32]; 2]) -> bool {
        if !self.kick {
            return false;
        }
        assert!(self.begun.is_none(), "begin twice");
        assert_eq!(input[0].len(), ctx.frames as usize);
        assert!(input.iter().all(|c| c.iter().all(|&x| x == 0.0)));
        self.begins.fetch_add(1, Ordering::Relaxed);
        self.begun = Some((ctx.frames, events.len()));
        true
    }

    fn process(
        &mut self,
        ctx: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        if self.kick {
            assert_eq!(
                self.begun.take(),
                Some((ctx.frames, events.len())),
                "process without begin"
            );
        }
        let _ = self
            .order
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |o| {
                Some(o << 4 | self.track)
            });
        self.inner.process(ctx, events, io)
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {
        self.inner.reset();
    }
}

#[test]
fn bridge_instruments_start_early_and_run_last() {
    let mut rig = Rig::new(RATE, 256);
    let begins = Arc::new(AtomicUsize::new(0));
    let order = Arc::new(AtomicU32::new(0));
    // Tracks 1 and 3 kick (bridges), 2 does not; track 4's kickable sits behind an instrument.
    let tracks: Vec<_> = (1..=3)
        .map(|i| {
            rig.insert(
                i,
                Kickable {
                    track: i,
                    begins: Arc::clone(&begins),
                    order: Arc::clone(&order),
                    begun: None,
                    inner: TestSine::new(),
                    kick: i != 2,
                },
            );
            track(u64::from(i), i, vec![note(0, BAR, 60)])
        })
        .collect();
    rig.set_program(&program(120.0, tracks.clone()));
    rig.send(Command::Play);
    rig.process(256, 256);
    // One call: tracks 2, then 1 and 3 (kicked ones last, in track order).
    assert_eq!(order.load(Ordering::Relaxed), 0x213);
    assert_eq!(begins.load(Ordering::Relaxed), 2);
    // With loop wraps inside calls, every segment begins and completes again (a quarter-bar loop
    // is 24 000 frames at 120 bpm).
    let mut looped = program(120.0, tracks);
    looped.loop_on = true;
    looped.loop_range = TickRange::new(Tick::ZERO, Tick(BAR / 4));
    rig.set_program(&looped);
    let calls = 48_000 / 1000;
    let before = begins.load(Ordering::Relaxed);
    rig.process(48_000, 1000);
    assert!(begins.load(Ordering::Relaxed) - before >= 2 * (calls + 2));
    // With workers too.
    rig.engine.set_workers(2, None);
    let before = begins.load(Ordering::Relaxed);
    rig.process(48_000, 1000);
    assert!(begins.load(Ordering::Relaxed) - before >= 2 * (calls + 2));
}
