//! Non-finite and denormal audio (#217): a processor that emits NaN and Inf leaves the master
//! output finite and is counted; offline renders run with flush-to-zero like the audio callback.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use komponist_engine::{
    Command, Event, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};

use crate::common::*;

const BLOCK: usize = 256;

/// One bar at 120 bpm and 48 kHz, in frames.
const BAR_FRAMES: usize = 96_000;

/// Instrument: NaN, +Inf, −Inf and a finite value in turn, on both channels.
struct Poison;

impl Processor for Poison {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        for out in &mut io.output {
            for (i, x) in out.iter_mut().enumerate() {
                *x = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.25][i % 4];
            }
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

fn poisoned_rig() -> Rig {
    let mut rig = Rig::new(RATE, BLOCK);
    rig.insert(0, Poison);
    rig.set_program(&program(
        120.0,
        vec![track(1, 0, vec![note(0, 2 * BAR, 60)])],
    ));
    rig.send(Command::Play);
    rig
}

#[test]
fn a_processor_emitting_nan_and_inf_leaves_the_master_output_finite() {
    let mut rig = poisoned_rig();
    // Count-in plus one bar of the note; the rig runs every call inside `assert_no_alloc`.
    let frames = 2 * BAR_FRAMES;
    rig.process(frames, BLOCK);
    assert!(rig.l.iter().chain(&rig.r).all(|x| x.is_finite()));
    // The finite quarter of the samples got through.
    assert!(rig.l.iter().any(|&x| x == 0.25));
    let counted = rig.handle.telemetry().non_finite_samples() as usize;
    assert!(counted > 0, "occurrences are counted");
    assert_eq!(counted % 2, 0, "both channels are counted");
}

#[test]
fn clean_audio_counts_nothing() {
    let mut rig = Rig::new(RATE, BLOCK);
    rig.insert(0, komponist_engine::testing::TestSine::new());
    rig.set_program(&program(120.0, vec![track(1, 0, vec![note(0, BAR, 60)])]));
    rig.send(Command::Play);
    rig.process(3 * BAR_FRAMES, BLOCK);
    assert_eq!(rig.handle.telemetry().non_finite_samples(), 0);
}

/// Instrument that records whether a denormal times one half is flushed to zero.
struct FlushProbe {
    flushed: Arc<AtomicU32>,
    calls: Arc<AtomicU32>,
}

/// True when the calling thread flushes denormals to zero.
fn flushes_denormals() -> bool {
    black_box(f32::MIN_POSITIVE) * black_box(0.5) == 0.0
}

impl Processor for FlushProbe {
    fn activate(&mut self, _: f32, _: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(&mut self, _: &ProcessContext, _: &[Event], io: &mut StereoIo<'_>) -> ProcessStatus {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if flushes_denormals() {
            self.flushed.fetch_add(1, Ordering::Relaxed);
        }
        for out in &mut io.output {
            out.fill(0.0);
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

#[test]
fn render_runs_with_flush_to_zero_and_restores_the_callers_state() {
    assert!(!flushes_denormals(), "the test thread starts without FTZ");
    let flushed = Arc::new(AtomicU32::new(0));
    let calls = Arc::new(AtomicU32::new(0));
    let mut rig = Rig::new(RATE, BLOCK);
    rig.insert(
        0,
        FlushProbe {
            flushed: flushed.clone(),
            calls: calls.clone(),
        },
    );
    rig.set_program(&program(120.0, vec![track(1, 0, vec![note(0, BAR, 60)])]));
    rig.send(Command::Play);
    rig.render(2 * BAR_FRAMES, BLOCK);
    let calls = calls.load(Ordering::Relaxed);
    assert!(calls > 0, "the processor ran");
    assert_eq!(
        flushed.load(Ordering::Relaxed),
        calls,
        "every render call flushed denormals"
    );
    assert!(
        !flushes_denormals(),
        "the caller's floating-point state is restored"
    );
}
