//! Parallel track processing (WP-65): the output with worker threads is bit-exact with the
//! single-threaded engine (realtime and offline), chains that share
//! a slot run serially, bridge-style processors are started early (`Processor::begin`) and their
//! tracks run last, and the pool can be changed and switched off.
//!
//! Every `process` / `render` call runs inside `assert_no_alloc` (the rig); the workers run their
//! jobs inside `assert_no_alloc` themselves, so an allocation on a worker aborts too.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use komponist_engine::Command;
use komponist_engine::testing::{TestGain, TestSine};
use komponist_model::{Tick, TickRange};
use komponist_processor::{
    Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};

use crate::common::*;
use crate::material::material;

use crate::parallel_support::{Setup, assert_bit_exact, play};
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

#[test]
fn test_processors_are_bit_exact_with_workers() {
    let frames = 3 * 48_000;
    let single = play(0, frames, sines);
    for workers in [1, 2, 4] {
        let parallel = play(workers, frames, sines);
        assert_bit_exact(&single, &parallel, &format!("{workers} workers"));
    }
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
