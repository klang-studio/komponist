//! Engine CPU per 256-frame block, single-threaded and without a device (WP-61): 16 tracks of the
//! same material as `komponist-app`'s `engine_perf` (four-note chords per half bar, held, and a
//! 1/16 line, looped over 4 bars), once with instruments that only take their events and write a
//! constant (the engine's own cost: sequencer, event lists, chains, fader, balance, meters, PDC,
//! mixing) and once with pisynth (presets E-Piano, Strings, Supersaw, Init in turn).
//!
//! Each runs once per worker count (WP-65; `KOMPONIST_BENCH_WORKERS`, default `0,4`); workers ask
//! for `SCHED_FIFO` 80 like the app's. The time is the wall time of `process` on the calling
//! thread.
//!
//! ```text
//! cargo test --release -p komponist-engine --test engine bench -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use komponist_builtin::{PisynthProcessor, preset_patch, presets};
use komponist_engine::program::SeqNote;
use komponist_engine::{
    Command, Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};
use komponist_model::{Tick, TickRange};

use crate::common::*;

const TRACKS: u32 = 16;
const BLOCK: usize = 256;
/// 20 s of audio.
const BLOCKS: usize = 20 * 48_000 / BLOCK;

pub fn material(track: u32) -> Vec<SeqNote> {
    let mut notes = Vec::new();
    for half in 0..8i64 {
        let root = 48 + ((half as u32 * 5 + track) % 12) as u8;
        for (k, interval) in [0u8, 4, 7, 11].into_iter().enumerate() {
            notes.push(SeqNote {
                velocity: 90,
                ..note(
                    half * BAR / 2 + k as i64 * 10,
                    BAR / 2 - 30,
                    root + interval,
                )
            });
        }
    }
    for step in 0..64i64 {
        notes.push(SeqNote {
            velocity: 70,
            ..note(
                step * BAR / 16,
                BAR / 32,
                72 + ((step * 7 + i64::from(track)) % 12) as u8,
            )
        });
    }
    notes.sort_by_key(|n| (n.start, n.key));
    notes
}

/// An instrument that costs (almost) nothing: counts its events, writes a constant.
struct Idle(u64);

impl Processor for Idle {
    fn activate(&mut self, _sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        _: &ProcessContext,
        events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        self.0 += events.len() as u64;
        let level = if self.0.is_multiple_of(2) { 0.01 } else { 0.02 };
        io.output[0].fill(level);
        io.output[1].fill(level);
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

/// Mean and worst time of one 256-frame `process` call over 20 s of playback, with `workers`
/// worker threads.
fn run(workers: usize, make: impl Fn(u32) -> Box<dyn Processor>) -> (Duration, Duration) {
    let mut rig = Rig::new(RATE, BLOCK);
    let status = rig.engine.set_workers(
        workers,
        Some(komponist_engine::workers::DEFAULT_WORKER_PRIORITY),
    );
    if let Some(e) = status.realtime_error {
        eprintln!("workers without SCHED_FIFO: {e}");
    }
    let tracks = (0..TRACKS)
        .map(|i| track(u64::from(i) + 1, i, material(i)))
        .collect();
    let mut p = program(120.0, tracks);
    p.loop_range = TickRange::new(Tick::ZERO, Tick(4 * BAR));
    p.loop_on = true;
    for i in 0..TRACKS {
        rig.send(Command::InsertProcessor {
            slot: komponist_engine::SlotId(i),
            processor: make(i),
        });
    }
    rig.set_program(&p);
    rig.send(Command::Play);
    // Warm up one loop, then measure (the rig keeps its output: allocate it up front).
    rig.process(4 * 96_000, BLOCK);
    rig.l.reserve(BLOCKS * BLOCK);
    rig.r.reserve(BLOCKS * BLOCK);
    let (mut total, mut worst) = (Duration::ZERO, Duration::ZERO);
    for _ in 0..BLOCKS {
        let t0 = Instant::now();
        rig.process_with(BLOCK, &[], 0);
        let t = t0.elapsed();
        total += t;
        worst = worst.max(t);
    }
    (total / BLOCKS as u32, worst)
}

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn bench_engine_16_tracks() {
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    let share = |d: Duration| d.as_secs_f64() / period.as_secs_f64() * 100.0;
    let counts: Vec<usize> = std::env::var("KOMPONIST_BENCH_WORKERS")
        .ok()
        .map(|s| s.split(',').filter_map(|w| w.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0, 4]);
    for workers in counts {
        let (mean, worst) = run(workers, |_| Box::new(Idle(0)));
        eprintln!(
            "engine alone (16 idle instruments), {workers} workers: mean {mean:.1?} ({:.2} %), worst {worst:.1?} ({:.1} %) per {BLOCK} frames",
            share(mean),
            share(worst)
        );
        let names = ["E-Piano", "Strings", "Supersaw", "Init"];
        let (mean, worst) = run(workers, |i| {
            let index = presets()
                .find(|x| x.name == names[i as usize % 4])
                .unwrap()
                .index;
            let mut p = PisynthProcessor::new(preset_patch(index).unwrap());
            p.activate(RATE, BLOCK).unwrap();
            Box::new(p)
        });
        eprintln!(
            "engine + 16 pisynth, {workers} workers: mean {mean:.1?} ({:.1} %), worst {worst:.1?} ({:.1} %) per {BLOCK} frames",
            share(mean),
            share(worst)
        );
    }
}
