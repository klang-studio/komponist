//! Direct engine timing on the shared sequencer workload.

use std::time::{Duration, Instant};

use komponist_engine::{Command, Processor};
use komponist_model::{Tick, TickRange};

use super::engine_common::{BAR, RATE, Rig, program, track};
use crate::material::material;

const TRACKS: u32 = 16;
pub const BLOCK: usize = 256;
/// 20 s of audio.
const BLOCKS: usize = 20 * 48_000 / BLOCK;

/// Mean and worst time of one 256-frame `process` call over 20 s of playback, with `workers`
/// worker threads.
pub fn run(workers: usize, make: impl Fn(u32) -> Box<dyn Processor>) -> (Duration, Duration) {
    let mut rig = Rig::new(RATE, BLOCK);
    let status = rig.engine.set_workers(
        workers,
        Some(komponist_engine::workers::WorkerRt::new(
            std::time::Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE)),
        )),
    );
    if let Some(e) = status.realtime_error {
        eprintln!("workers without realtime scheduling: {e}");
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

pub fn worker_counts() -> Vec<usize> {
    std::env::var("KOMPONIST_BENCH_WORKERS")
        .ok()
        .map(|s| s.split(',').filter_map(|w| w.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0, 4])
}
