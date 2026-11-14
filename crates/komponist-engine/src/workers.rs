//! Parallel track processing (WP-65, ARCHITECTURE §6.4).
//!
//! A fixed pool of worker threads, started outside the audio callback ([`crate::Engine::set_workers`],
//! the app does it when a stream starts) and kept until the pool is replaced or the engine is
//! dropped. Per segment the engine hands the pool one batch of jobs (one job = one track's chain,
//! fader, meter and delay compensation) and takes part itself:
//!
//! - **Dispatch:** the dispatching thread writes the batch descriptor, publishes it with one
//!   release store of the claim word (`epoch << 32 | count << 16 | next`) and wakes as many
//!   sleeping workers as there are jobs beyond the first (one `FUTEX_WAKE`). Every thread then
//!   claims jobs with a compare-and-swap on the claim word until none is left. No allocation, no
//!   lock; the descriptor is read only by a thread that holds a claimed job of its epoch, so the
//!   dispatcher never rewrites it while someone reads it.
//! - **Deadline fallback:** a worker that wakes late (or not at all) simply claims nothing: the
//!   dispatching thread takes every job no worker has started, in order, without waiting for
//!   anyone. It then waits (`FUTEX_WAIT` with a deadline of one callback period, repeated) only for
//!   jobs a worker is already running: such a job owns its track's processors and state until it
//!   returns, so it cannot be redone or abandoned. A wait past the deadline is counted in
//!   [`WorkerStats::late`].
//! - **Determinism:** a job touches only its own track (state, processors, output buffer) and its
//!   lane's scratch buffers; workers copy the dispatcher's floating-point mode (flush-to-zero,
//!   rounding) before a batch; the engine sums the tracks in track order after the batch. The
//!   output is therefore bit-exact whatever thread ran which track (tested against the
//!   single-threaded path).
//! - **Realtime:** workers run at `SCHED_FIFO` (the bridges' default priority, 80) if the system
//!   allows it, and run their jobs inside `assert_no_alloc`. Idle workers sleep in `FUTEX_WAIT`
//!   (with a one-second deadline, to look at the shutdown flag).

use std::cell::UnsafeCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sys;

/// Most worker threads a pool runs (next to the dispatching thread).
pub const MAX_WORKERS: usize = 15;

/// `SCHED_FIFO` priority of the workers: the same as the bridge RT threads (below PipeWire's data
/// loops, 88 for the server and 83 for clients on the owner's machine).
pub const DEFAULT_WORKER_PRIORITY: i32 = 80;

/// How long an idle worker sleeps before it looks at the shutdown flag again (shutdown also wakes
/// it at once).
const IDLE_WAIT: Duration = Duration::from_secs(1);

/// Workers for this machine: one per physical core (assuming two hardware threads per core),
/// minus two for the audio callback and the rest of the system (UI, bridges, audio server), at
/// most 8 and none below four hardware threads. 12 hardware threads (6 cores) give 4.
pub fn auto_workers() -> usize {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
    auto_workers_for(threads)
}

fn auto_workers_for(threads: usize) -> usize {
    if threads < 4 {
        0
    } else {
        (threads / 2).saturating_sub(2).clamp(1, 8)
    }
}

/// Outcome of [`crate::Engine::set_workers`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkerStatus {
    /// Worker threads running (0: the engine processes every track on the calling thread).
    pub threads: usize,
    /// The `SCHED_FIFO` priority asked for (`None`: normal scheduling).
    pub priority: Option<i32>,
    /// Why the workers run without `SCHED_FIFO` although it was asked for (e.g. "Operation not
    /// permitted": no `RLIMIT_RTPRIO`).
    pub realtime_error: Option<String>,
    /// Why fewer threads than asked for run (thread creation failed).
    pub spawn_error: Option<String>,
}

/// Counters of the pool, shared with the UI thread and tests (they survive pool changes).
#[derive(Debug, Default)]
pub struct WorkerStats {
    batches: AtomicU64,
    worker_jobs: AtomicU64,
    late: AtomicU64,
}

impl WorkerStats {
    /// Batches dispatched to the pool (segments processed in parallel).
    pub fn batches(&self) -> u64 {
        self.batches.load(Ordering::Relaxed)
    }

    /// Jobs (tracks) that ran on a worker thread rather than on the dispatching thread.
    pub fn worker_jobs(&self) -> u64 {
        self.worker_jobs.load(Ordering::Relaxed)
    }

    /// Times the dispatching thread still waited for a running job one callback period after it
    /// started waiting.
    pub fn late(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }
}

/// A word on its own cache line (the claim word is hammered by every thread; the futex words must
/// not share its line).
#[repr(align(128))]
struct Padded<T>(T);

/// What one batch runs: `run(ctx, job, lane)` for `job` in `0 .. count`.
#[derive(Clone, Copy)]
struct Batch {
    ctx: *const (),
    run: unsafe fn(*const (), usize, usize),
    count: u32,
    /// The dispatcher's floating-point control state ([`sys::fp_state`]).
    fp: u64,
}

/// State shared by the dispatcher and the workers.
struct Shared {
    /// Futex word the workers sleep on: the epoch of the latest batch.
    wake: Padded<AtomicU32>,
    /// `epoch << 32 | count << 16 | next`: the batch's job counter.
    claim: Padded<AtomicU64>,
    /// Futex word the dispatcher sleeps on: jobs of the batch finished.
    finished: Padded<AtomicU32>,
    shutdown: AtomicBool,
    /// A job panicked on a worker (the dispatcher re-raises it after the batch).
    panicked: AtomicBool,
    /// Written by the dispatcher before it publishes a batch, while no job is outstanding; read by
    /// a thread only after it claimed a job of that batch.
    batch: UnsafeCell<Batch>,
    stats: Arc<WorkerStats>,
}

// SAFETY: `batch` follows the protocol documented on the field (written only while no claimed job
// is outstanding, read only by holders of a claimed job of that batch; publication through the
// release store of `claim` and the acquire CAS of the claimer). Its `ctx` points to a `Sync`
// closure that outlives the batch (`Pool::run` waits for every job).
unsafe impl Sync for Shared {}
// SAFETY: as above; the raw pointer in `batch` is only dereferenced under that protocol.
unsafe impl Send for Shared {}

fn pack(epoch: u32, count: usize, next: usize) -> u64 {
    u64::from(epoch) << 32 | (count as u64) << 16 | next as u64
}

/// RT. Claims the next job of the batch with `epoch`, if one is left.
fn claim(word: &AtomicU64, epoch: u32) -> Option<usize> {
    let mut current = word.load(Ordering::Acquire);
    loop {
        let count = (current >> 16) & 0xffff;
        let next = current & 0xffff;
        if (current >> 32) as u32 != epoch || next >= count {
            return None;
        }
        match word.compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => return Some(next as usize),
            Err(seen) => current = seen,
        }
    }
}

/// The worker threads and the dispatching side of the batch protocol.
pub(crate) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    epoch: u32,
    status: WorkerStatus,
}

impl Pool {
    /// Non-RT. Starts `threads` workers (at most [`MAX_WORKERS`]) whose lanes are `1 ..= threads`,
    /// at `SCHED_FIFO` `priority` if given. `None` if not a single thread could start (the status
    /// says why).
    pub(crate) fn start(
        threads: usize,
        priority: Option<i32>,
        stats: Arc<WorkerStats>,
    ) -> (Option<Pool>, WorkerStatus) {
        let threads = threads.min(MAX_WORKERS);
        let shared = Arc::new(Shared {
            wake: Padded(AtomicU32::new(0)),
            claim: Padded(AtomicU64::new(0)),
            finished: Padded(AtomicU32::new(0)),
            shutdown: AtomicBool::new(false),
            panicked: AtomicBool::new(false),
            batch: UnsafeCell::new(Batch {
                ctx: core::ptr::null(),
                run: noop,
                count: 0,
                fp: 0,
            }),
            stats,
        });
        let mut status = WorkerStatus {
            priority,
            ..WorkerStatus::default()
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mut handles = Vec::with_capacity(threads);
        for lane in 1..=threads {
            let shared = Arc::clone(&shared);
            let ready = ready_tx.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("komponist-worker-{lane}"))
                .spawn(move || {
                    let realtime = priority.map(sys::set_current_thread_fifo);
                    let _ = ready.send(realtime);
                    work(&shared, lane);
                });
            match spawned {
                Ok(handle) => handles.push(handle),
                Err(e) => {
                    status.spawn_error = Some(e.to_string());
                    break;
                }
            }
        }
        drop(ready_tx);
        for _ in 0..handles.len() {
            if let Ok(Some(Err(e))) = ready_rx.recv() {
                status.realtime_error = Some(e.to_string());
            }
        }
        status.threads = handles.len();
        if handles.is_empty() {
            return (None, status);
        }
        let pool = Pool {
            shared,
            threads: handles,
            epoch: 0,
            status: status.clone(),
        };
        (Some(pool), status)
    }

    /// Worker threads (lanes `1 ..= threads()`).
    pub(crate) fn threads(&self) -> usize {
        self.threads.len()
    }

    /// The status the pool started with.
    pub(crate) fn status(&self) -> &WorkerStatus {
        &self.status
    }

    /// RT. Runs `job(j, lane)` for every `j` in `0 .. count` (≤ `u16::MAX`), on the calling thread
    /// (lane 0) and the workers, and returns when all are done. Jobs are claimed in order of `j`.
    /// `period` is the deadline of each wait for running jobs (see the module docs).
    pub(crate) fn run<F: Fn(usize, usize) + Sync>(
        &mut self,
        count: usize,
        job: &F,
        period: Duration,
    ) {
        /// Calls the closure behind `ctx`.
        ///
        /// # Safety
        /// `ctx` must point to a live `F`.
        unsafe fn trampoline<F: Fn(usize, usize)>(ctx: *const (), j: usize, lane: usize) {
            // SAFETY: guaranteed by the caller (`run` keeps `job` alive until every job is done).
            let f = unsafe { &*ctx.cast::<F>() };
            f(j, lane);
        }

        debug_assert!(count <= usize::from(u16::MAX));
        if count == 0 {
            return;
        }
        let s = &*self.shared;
        // SAFETY: the previous batch is finished (this method returned only after every job), so
        // no thread holds a claimed job and nobody reads the descriptor now.
        unsafe {
            *s.batch.get() = Batch {
                ctx: (job as *const F).cast(),
                run: trampoline::<F>,
                count: count as u32,
                fp: sys::fp_state(),
            };
        }
        s.finished.0.store(0, Ordering::Relaxed);
        self.epoch = self.epoch.wrapping_add(1);
        s.claim
            .0
            .store(pack(self.epoch, count, 0), Ordering::Release);
        s.wake.0.store(self.epoch, Ordering::Release);
        let helpers = self.threads.len().min(count - 1);
        if helpers > 0 {
            sys::wake(&s.wake.0, helpers);
        }
        s.stats.batches.fetch_add(1, Ordering::Relaxed);

        let mut mine = 0u32;
        while let Some(j) = claim(&s.claim.0, self.epoch) {
            job(j, 0);
            mine += 1;
        }
        let count = count as u32;
        let mut finished = s.finished.0.fetch_add(mine, Ordering::AcqRel) + mine;
        let mut deadline = Instant::now() + period;
        while finished < count {
            if !sys::wait_while(&s.finished.0, finished, deadline) {
                s.stats.late.fetch_add(1, Ordering::Relaxed);
                deadline = Instant::now() + period;
            }
            finished = s.finished.0.load(Ordering::Acquire);
        }
        // A bug, as on the calling thread; never leave it silent (the track's output is stale).
        assert!(
            !s.panicked.swap(false, Ordering::AcqRel),
            "a track job panicked on a worker thread"
        );
    }
}

impl Drop for Pool {
    /// Non-RT. Stops and joins the workers.
    fn drop(&mut self) {
        let s = &*self.shared;
        s.shutdown.store(true, Ordering::Release);
        s.wake.0.fetch_add(1, Ordering::AcqRel);
        sys::wake(&s.wake.0, usize::MAX);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

impl core::fmt::Debug for Pool {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pool")
            .field("threads", &self.threads.len())
            .finish_non_exhaustive()
    }
}

/// Placeholder job function of an empty descriptor (never called: a batch is published only with
/// its real function).
unsafe fn noop(_: *const (), _: usize, _: usize) {}

/// A worker's loop: sleep until a new epoch, claim and run jobs, report each finished job.
fn work(shared: &Shared, lane: usize) {
    let mut seen = shared.wake.0.load(Ordering::Acquire);
    let mut fp = sys::fp_state();
    assert_no_alloc::assert_no_alloc(|| {
        loop {
            let epoch = shared.wake.0.load(Ordering::Acquire);
            if epoch == seen {
                sys::wait_while(&shared.wake.0, seen, Instant::now() + IDLE_WAIT);
                continue;
            }
            seen = epoch;
            if shared.shutdown.load(Ordering::Acquire) {
                break;
            }
            while let Some(j) = claim(&shared.claim.0, epoch) {
                // SAFETY: this thread holds a claimed job of the batch, so the descriptor is the
                // one published with `epoch` and stays unchanged until the job is reported.
                let batch = unsafe { *shared.batch.get() };
                if batch.fp != fp {
                    sys::set_fp_state(batch.fp);
                    fp = batch.fp;
                }
                // SAFETY: `ctx` is the live closure of `Pool::run`, which waits for this job.
                let job = || unsafe { (batch.run)(batch.ctx, j, lane) };
                // A panic still counts the job as finished, so the dispatcher never waits for it
                // forever; it re-raises the panic.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    shared.panicked.store(true, Ordering::Release);
                }
                shared.stats.worker_jobs.fetch_add(1, Ordering::Relaxed);
                if shared.finished.0.fetch_add(1, Ordering::AcqRel) + 1 == batch.count {
                    sys::wake(&shared.finished.0, 1);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn auto_count_leaves_room_for_the_system() {
        assert_eq!(auto_workers_for(1), 0);
        assert_eq!(auto_workers_for(2), 0);
        assert_eq!(auto_workers_for(4), 1);
        assert_eq!(auto_workers_for(8), 2);
        assert_eq!(auto_workers_for(12), 4);
        assert_eq!(auto_workers_for(64), 8);
        assert!(auto_workers() <= 8);
    }

    #[test]
    fn claims_stop_at_the_count_and_at_a_new_epoch() {
        let word = AtomicU64::new(pack(7, 2, 0));
        assert_eq!(claim(&word, 7), Some(0));
        assert_eq!(claim(&word, 6), None);
        assert_eq!(claim(&word, 7), Some(1));
        assert_eq!(claim(&word, 7), None);
        word.store(pack(8, 1, 0), Ordering::Release);
        assert_eq!(claim(&word, 7), None);
        assert_eq!(claim(&word, 8), Some(0));
    }

    /// Every job runs exactly once per batch, on lanes `0 ..= threads`, over many batches.
    #[test]
    fn every_job_runs_once() {
        let stats = Arc::new(WorkerStats::default());
        let (pool, status) = Pool::start(3, None, Arc::clone(&stats));
        let mut pool = pool.expect("threads start");
        assert_eq!((status.threads, pool.threads()), (3, 3));
        let counts: Vec<AtomicUsize> = (0..40).map(|_| AtomicUsize::new(0)).collect();
        let lanes: Vec<AtomicUsize> = (0..4).map(|_| AtomicUsize::new(0)).collect();
        for batch in 1..=500 {
            let count = 1 + batch % 40;
            let job = |j: usize, lane: usize| {
                counts[j].fetch_add(1, Ordering::Relaxed);
                lanes[lane].fetch_add(1, Ordering::Relaxed);
                // Some work, so that workers get a share.
                std::hint::black_box((0..200).sum::<u64>());
            };
            assert_no_alloc::assert_no_alloc(|| pool.run(count, &job, Duration::from_secs(1)));
        }
        let total: usize = counts.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        let expected: usize = (1..=500).map(|b| 1 + b % 40).sum();
        assert_eq!(total, expected);
        let by_lanes: usize = lanes.iter().map(|c| c.load(Ordering::Relaxed)).sum();
        assert_eq!(by_lanes, expected);
        assert_eq!(stats.batches(), 500);
        assert_eq!(
            stats.worker_jobs() as usize,
            expected - lanes[0].load(Ordering::Relaxed)
        );
        drop(pool);
    }

    /// A job that outlasts the period is waited for (it owns its data) and counted as late.
    #[test]
    fn slow_jobs_are_waited_for_and_counted() {
        let stats = Arc::new(WorkerStats::default());
        let (pool, _) = Pool::start(1, None, Arc::clone(&stats));
        let mut pool = pool.unwrap();
        let done = AtomicUsize::new(0);
        let job = |_: usize, lane: usize| {
            if lane != 0 {
                std::thread::sleep(Duration::from_millis(30));
            } else {
                // Keep the caller busy so the worker can claim the other job even when the
                // machine is loaded (the caller took both jobs under parallel builds).
                std::thread::sleep(Duration::from_millis(5));
            }
            done.fetch_add(1, Ordering::Relaxed);
        };
        // Retry until the worker took one of the two jobs (it may wake after the caller took
        // both).
        for _ in 0..100 {
            pool.run(2, &job, Duration::from_millis(5));
            if stats.worker_jobs() > 0 {
                break;
            }
        }
        assert!(stats.worker_jobs() > 0);
        assert!(stats.late() > 0);
        assert_eq!(done.load(Ordering::Relaxed) % 2, 0);
    }

    /// Release builds only: in debug builds the panic's allocation inside `assert_no_alloc`
    /// aborts the process (which is also loud, but kills the test binary).
    #[cfg(not(debug_assertions))]
    #[test]
    fn a_panicking_job_reaches_the_dispatcher() {
        let (pool, _) = Pool::start(1, None, Arc::default());
        let mut pool = pool.unwrap();
        let job = |_: usize, lane: usize| {
            if lane != 0 {
                panic!("job panics");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for _ in 0..100 {
                pool.run(2, &job, Duration::from_secs(1));
            }
        }));
        assert!(caught.is_err());
    }

    #[test]
    fn workers_copy_the_dispatchers_float_mode() {
        let (pool, _) = Pool::start(2, None, Arc::default());
        let mut pool = pool.unwrap();
        let mine = sys::fp_state();
        let seen = AtomicU64::new(0);
        let job = |_: usize, lane: usize| {
            if lane != 0 {
                seen.store(sys::fp_state() | 1 << 63, Ordering::Relaxed);
            }
            // Every lane sleeps, so a loaded machine cannot let the caller take all jobs before a
            // worker wakes (it did under parallel builds).
            std::thread::sleep(Duration::from_millis(2));
        };
        for _ in 0..200 {
            pool.run(3, &job, Duration::from_secs(1));
            if seen.load(Ordering::Relaxed) != 0 {
                break;
            }
        }
        let seen = seen.load(Ordering::Relaxed);
        assert_ne!(seen, 0, "no job ran on a worker");
        assert_eq!(seen & !(1 << 63), mine);
    }
}
