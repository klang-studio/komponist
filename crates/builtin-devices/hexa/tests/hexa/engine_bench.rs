//! Hexa's direct engine timing on the same sequencer workload as the idle engine benchmark.

use std::time::Duration;

use komponist_engine::Processor;
use komponist_hexa::{HexaProcessor, preset_patch, presets};

use crate::engine_common::RATE;
use crate::measure::{BLOCK, run, worker_counts};

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn bench_engine_16_hexa_tracks() {
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    let share = |d: Duration| d.as_secs_f64() / period.as_secs_f64() * 100.0;
    for workers in worker_counts() {
        let names = ["E-Piano", "Strings", "Supersaw", "Init"];
        let (mean, worst) = run(workers, |i| {
            let index = presets()
                .find(|x| x.name == names[i as usize % 4])
                .unwrap()
                .index;
            let mut p = HexaProcessor::new(preset_patch(index).unwrap());
            p.activate(RATE, BLOCK).unwrap();
            Box::new(p)
        });
        eprintln!(
            "engine + 16 hexa, {workers} workers: mean {mean:.1?} ({:.1} %), worst {worst:.1?} ({:.1} %) per {BLOCK} frames",
            share(mean),
            share(worst)
        );
    }
}
