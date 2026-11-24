//! Engine CPU per 256-frame block (WP-61): 16 tracks of four-note chords and a 1/16 line,
//! looped over four bars. Instruments consume events and write a constant, isolating the
//! sequencer, event lists, chains, fader, balance, meters, PDC and mixer.
//!
//! Runs once per worker count (WP-65; `KOMPONIST_BENCH_WORKERS`, default `0,4`).
//!
//! ```text
//! cargo test --release -p komponist-engine --test engine bench_engine_16_tracks -- --ignored --nocapture
//! ```

use std::time::Duration;

use komponist_engine::{Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo};

use crate::common::RATE;
use crate::measure::{BLOCK, run, worker_counts};

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

#[test]
#[ignore = "timing: run in release with --ignored --nocapture"]
fn bench_engine_16_tracks() {
    let period = Duration::from_secs_f64(BLOCK as f64 / f64::from(RATE));
    let share = |d: Duration| d.as_secs_f64() / period.as_secs_f64() * 100.0;
    for workers in worker_counts() {
        let (mean, worst) = run(workers, |_| Box::new(Idle(0)));
        eprintln!(
            "engine alone (16 idle instruments), {workers} workers: mean {mean:.1?} ({:.2} %), worst {worst:.1?} ({:.1} %) per {BLOCK} frames",
            share(mean),
            share(worst)
        );
    }
}
