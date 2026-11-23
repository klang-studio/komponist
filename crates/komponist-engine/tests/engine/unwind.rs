//! A panic inside `Engine::process` (#210): with worker threads, the segment's jobs still running
//! on the workers finish before the panic leaves the engine (they read the segment's context on
//! the engine's stack), and the engine that unwound is poisoned: it outputs silence and runs no
//! processor any more.
//!
//! The panicking calls run outside `assert_no_alloc` (a panic allocates its payload), on this
//! thread; the workers' jobs (which run inside `assert_no_alloc`) never panic here.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use komponist_engine::{
    Event, LiveInput, ProcessContext, ProcessStatus, Processor, ProcessorError, StereoIo,
};

use crate::common::*;

/// What the two [`Meeting`] processors share.
struct Shared {
    /// The thread that calls `Engine::process` (the dispatcher).
    dispatcher: ThreadId,
    /// The processors meet (and panic) only while armed, outside `assert_no_alloc`.
    armed: AtomicBool,
    /// A job started on a worker in this call.
    worker_started: AtomicBool,
    /// That job returned.
    worker_done: AtomicBool,
    /// `process` calls of both processors.
    calls: AtomicU32,
}

/// An instrument that, on a worker, works for a while; on the dispatcher, waits for a worker job
/// to start and then panics (so the panic happens while that job runs). Writes silence.
struct Meeting(Arc<Shared>);

impl Processor for Meeting {
    fn activate(&mut self, _sample_rate: f32, _max_block: usize) -> Result<(), ProcessorError> {
        Ok(())
    }

    fn process(
        &mut self,
        _ctx: &ProcessContext,
        _events: &[Event],
        io: &mut StereoIo<'_>,
    ) -> ProcessStatus {
        let shared = &*self.0;
        shared.calls.fetch_add(1, Ordering::SeqCst);
        io.output[0].fill(0.0);
        io.output[1].fill(0.0);
        if !shared.armed.load(Ordering::SeqCst) {
            return ProcessStatus::Continue;
        }
        if std::thread::current().id() != shared.dispatcher {
            shared.worker_started.store(true, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            shared.worker_done.store(true, Ordering::SeqCst);
            return ProcessStatus::Continue;
        }
        let timeout = Instant::now() + Duration::from_secs(2);
        while !shared.worker_started.load(Ordering::SeqCst) && Instant::now() < timeout {
            std::thread::yield_now();
        }
        if shared.worker_started.load(Ordering::SeqCst) {
            panic!("a processor panics on the dispatcher");
        }
        ProcessStatus::Continue
    }

    fn latency(&self) -> u32 {
        0
    }

    fn reset(&mut self) {}
}

#[test]
fn a_panic_on_the_dispatcher_waits_for_the_workers_and_poisons_the_engine() {
    let mut rig = Rig::new(RATE, 256);
    assert_eq!(rig.engine.set_workers(1, None).threads, 1);
    let shared = Arc::new(Shared {
        dispatcher: std::thread::current().id(),
        armed: AtomicBool::new(false),
        worker_started: AtomicBool::new(false),
        worker_done: AtomicBool::new(false),
        calls: AtomicU32::new(0),
    });
    rig.insert(0, Meeting(Arc::clone(&shared)));
    rig.insert(1, Meeting(Arc::clone(&shared)));
    rig.set_program(&program(
        120.0,
        vec![track(1, 0, vec![]), track(2, 1, vec![])],
    ));
    // The first call takes the commands (the program comes after the processors).
    rig.process(256, 256);
    assert!(!rig.engine.is_poisoned());

    let input = LiveInput {
        midi: &[],
        output_latency: 0,
    };
    let (mut l, mut r) = (vec![0.0f32; 256], vec![0.0f32; 256]);
    let mut panicked = false;
    shared.armed.store(true, Ordering::SeqCst);
    for _ in 0..50 {
        shared.worker_started.store(false, Ordering::SeqCst);
        shared.worker_done.store(false, Ordering::SeqCst);
        let engine = &mut rig.engine;
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.process(&mut l, &mut r, &input);
        }));
        if caught.is_err() {
            assert!(
                shared.worker_done.load(Ordering::SeqCst),
                "the panic left the engine while a worker still ran a job"
            );
            panicked = true;
            break;
        }
        // The worker woke too late for this call (the dispatcher ran both jobs): again.
        assert!(!rig.engine.is_poisoned());
    }
    assert!(panicked, "no call had a job on each thread");
    shared.armed.store(false, Ordering::SeqCst);

    // Poisoned: silence from now on, no processor runs, also offline.
    assert!(rig.engine.is_poisoned());
    let calls = shared.calls.load(Ordering::SeqCst);
    let range = rig.process(1024, 256);
    assert!(rig.l[range.clone()].iter().all(|&s| s == 0.0));
    assert!(rig.r[range].iter().all(|&s| s == 0.0));
    assert!(rig.engine.render(256).iter().all(|&s| s == 0.0));
    assert_eq!(shared.calls.load(Ordering::SeqCst), calls);
    assert!(rig.engine.is_poisoned());
    // Dropping it joins the workers (nothing is outstanding).
    drop(rig);
}
