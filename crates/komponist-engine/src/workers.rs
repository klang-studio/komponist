//! Parallel track processing (WP-65, ARCHITECTURE §6.4.1), as a scheduler of the routing graph's
//! jobs (WP-102, §6.8.4).
//!
//! A fixed pool of worker threads, started outside the audio callback ([`crate::Engine::set_workers`],
//! the app does it when a stream starts) and kept until the pool is replaced or the engine is
//! dropped. Per segment the engine hands the pool one batch of jobs (one job = one track's or bus's
//! chain, fader, meter and delay compensation) and takes part itself:
//!
//! - **Dispatch:** the dispatching thread writes the batch descriptor, publishes the jobs that are
//!   ready at once (every job without inputs; in a project without buses all of them) with one
//!   release store of the **ready set** (a bit per job, at most [`MAX_JOBS`]) and wakes as many
//!   sleeping workers as there are ready jobs beyond the first (one `FUTEX_WAKE`). Every thread
//!   then claims ready jobs, the lowest first, by clearing their bit atomically. No allocation, no
//!   lock; the descriptor is read only by a thread that holds a claimed job, so the dispatcher
//!   never rewrites it while someone reads it.
//! - **Dependencies:** a job that others read (a track feeding a bus) publishes the jobs it makes
//!   ready when it is done ([`Publisher`]; the engine counts each job's missing inputs and
//!   publishes a job when its count reaches zero). The thread that publishes goes on claiming, so
//!   it usually runs the bus itself; the dispatcher is woken to claim the rest.
//! - **Deadline fallback:** a worker that wakes late (or not at all) simply claims nothing: the
//!   dispatching thread takes every ready job no worker has started, without waiting for anyone.
//!   It waits (`FUTEX_WAIT` with a deadline of one callback period, repeated) only while jobs a
//!   worker is running are outstanding (their dependents may be all that is left): such a job owns
//!   its track's processors and state until it returns, so it cannot be redone or abandoned. A
//!   wait past the deadline is counted in [`WorkerStats::late`].
//! - **Determinism:** a job touches only its own track (state, processors, output buffers), the
//!   finished outputs of the jobs it depends on (read only) and its lane's scratch buffers; workers
//!   copy the dispatcher's floating-point mode (flush-to-zero, rounding) before a batch; buses and
//!   the master sum their inputs in a fixed order. The output is therefore bit-exact whatever
//!   thread ran which job (tested against the single-threaded path).
//! - **Realtime ([`WorkerRt`]):** on Linux workers run at `SCHED_FIFO` (the bridges' default
//!   priority, 80) if the system allows it. On macOS (#45) they set the time-constraint policy for
//!   the callback period and join the device's audio IO workgroup when the stream has one (without
//!   it, a lightly loaded Apple silicon Mac runs them on slow cores; spike #43), and leave it
//!   before they end. Jobs run inside `assert_no_alloc`. Idle workers sleep in the kernel wait
//!   (`FUTEX_WAIT`, `os_sync_wait_on_address`) with a one-second deadline, to look at the shutdown
//!   flag.

use std::cell::{Cell, UnsafeCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::sys;

/// Most worker threads a pool runs (next to the dispatching thread).
pub const MAX_WORKERS: usize = 15;

/// Most jobs in one batch (the bits of the ready set): one per track, [`crate::MAX_TRACKS`].
pub const MAX_JOBS: usize = 128;

/// `SCHED_FIFO` priority of the workers on Linux: the same as the bridge RT threads (below
/// PipeWire's data loops, 88 for the server and 83 for clients on the owner's machine).
pub const DEFAULT_WORKER_PRIORITY: i32 = 80;

/// How the workers run in realtime ([`crate::Engine::set_workers`]); the app builds it for every
/// stream from its period and the device's workgroup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerRt {
    /// The callback period (buffer frames / rate): the period of the macOS time-constraint policy.
    pub period: Duration,
    /// `SCHED_FIFO` priority (1 ..= 99) on Linux.
    pub fifo_priority: i32,
    /// The audio device's IO workgroup the workers join (macOS; `None` elsewhere, and for streams
    /// without one such as the null backend).
    pub workgroup: Option<komponist_rt::Workgroup>,
}

impl WorkerRt {
    /// Realtime for a callback `period`, at [`DEFAULT_WORKER_PRIORITY`] and without a workgroup.
    pub fn new(period: Duration) -> WorkerRt {
        WorkerRt {
            period,
            fifo_priority: DEFAULT_WORKER_PRIORITY,
            workgroup: None,
        }
    }

    /// Whether a pool started with `self` already runs as `other` asks: on Linux the priority
    /// counts, on macOS the period and the workgroup (the same object).
    pub(crate) fn same_as(&self, other: &WorkerRt) -> bool {
        if cfg!(target_os = "macos") {
            self.period == other.period && self.workgroup == other.workgroup
        } else {
            self.fifo_priority == other.fifo_priority
        }
    }
}

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
    /// The `SCHED_FIFO` priority asked for (`None`: normal scheduling). On macOS the workers use
    /// the time-constraint policy instead whenever this is `Some`.
    pub priority: Option<i32>,
    /// Why the workers run without realtime scheduling although it was asked for (Linux:
    /// "Operation not permitted", no `RLIMIT_RTPRIO`; macOS: the time-constraint policy refused).
    pub realtime_error: Option<String>,
    /// The workers joined the audio device's workgroup (macOS).
    pub joined_workgroup: bool,
    /// Why a worker could not join the workgroup it was given (an `errno`, e.g. 37 `EALREADY`).
    pub workgroup_error: Option<String>,
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

/// The jobs of a batch that may be claimed: bit `j` = job `j` is ready and nobody runs it yet.
pub(crate) struct Ready {
    words: [AtomicU64; MAX_JOBS / 64],
}

impl Ready {
    pub(crate) const fn new() -> Ready {
        Ready {
            words: [AtomicU64::new(0), AtomicU64::new(0)],
        }
    }

    /// RT. Replaces the set (a new batch; no job of the previous one is outstanding).
    fn reset(&self, bits: u128) {
        self.words[0].store(bits as u64, Ordering::Release);
        self.words[1].store((bits >> 64) as u64, Ordering::Release);
    }

    /// RT. Adds `bits` to the set.
    fn publish(&self, bits: u128) {
        for (word, bits) in self.words.iter().zip([bits as u64, (bits >> 64) as u64]) {
            if bits != 0 {
                word.fetch_or(bits, Ordering::AcqRel);
            }
        }
    }

    /// RT. Whether any job is ready.
    fn any(&self) -> bool {
        self.words.iter().any(|w| w.load(Ordering::Acquire) != 0)
    }

    /// RT. Claims the lowest ready job, if there is one.
    fn claim(&self) -> Option<usize> {
        for (w, word) in self.words.iter().enumerate() {
            let mut bits = word.load(Ordering::Acquire);
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                let mask = 1u64 << bit;
                let before = word.fetch_and(!mask, Ordering::AcqRel);
                if before & mask != 0 {
                    return Some(w * 64 + bit);
                }
                bits = before & !mask;
            }
        }
        None
    }
}

/// Handed to every job of a batch: [`Publisher::publish`] makes jobs claimable. Lives on the
/// stack of the thread that runs the job.
pub(crate) struct Publisher<'a> {
    ready: &'a Ready,
    published: Cell<bool>,
}

impl<'a> Publisher<'a> {
    fn new(ready: &'a Ready) -> Publisher<'a> {
        Publisher {
            ready,
            published: Cell::new(false),
        }
    }

    /// RT. The jobs in `bits` (bit `j` = job `j`) may run now: all their inputs are done.
    pub(crate) fn publish(&self, bits: u128) {
        if bits != 0 {
            self.ready.publish(bits);
            self.published.set(true);
        }
    }
}

/// RT. Runs a batch on the calling thread alone: `job(j, 0, …)` for every job, the lowest ready
/// one first, until `count` jobs ran. `ready` are the jobs ready at the start; the rest must be
/// published by the jobs (else this panics).
pub(crate) fn run_serial<F: Fn(usize, usize, &Publisher<'_>)>(count: usize, ready: u128, job: &F) {
    let set = Ready::new();
    set.reset(ready);
    for _ in 0..count {
        let j = set.claim().expect("a job of the batch never became ready");
        job(j, 0, &Publisher::new(&set));
    }
}

/// The ready set of all jobs `0 .. count`.
#[cfg(test)]
fn all_jobs(count: usize) -> u128 {
    if count >= 128 {
        u128::MAX
    } else {
        (1u128 << count) - 1
    }
}

/// What one batch runs: `run(ctx, job, lane, publisher)` for `job` in `0 .. count`.
#[derive(Clone, Copy)]
struct Batch {
    ctx: *const (),
    run: unsafe fn(*const (), usize, usize, &Publisher<'_>),
    count: u32,
    /// The dispatcher's floating-point control state ([`sys::fp_state`]).
    fp: u64,
}

/// State shared by the dispatcher and the workers.
struct Shared {
    /// Futex word the workers sleep on: the epoch of the latest batch.
    wake: Padded<AtomicU32>,
    /// The batch's ready jobs.
    ready: Padded<Ready>,
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

/// The worker threads and the dispatching side of the batch protocol.
pub(crate) struct Pool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    epoch: u32,
    status: WorkerStatus,
    /// What the pool was started with.
    rt: Option<WorkerRt>,
}

impl Shared {
    /// Non-RT. The state of a pool without any batch, at epoch 0.
    fn new(stats: Arc<WorkerStats>) -> Shared {
        Shared {
            wake: Padded(AtomicU32::new(0)),
            ready: Padded(Ready::new()),
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
        }
    }
}

impl Pool {
    /// Non-RT. Starts `threads` workers (at most [`MAX_WORKERS`]) whose lanes are `1 ..= threads`,
    /// in realtime per `rt` if given. `None` if not a single thread could start (the status says
    /// why).
    pub(crate) fn start(
        threads: usize,
        rt: Option<WorkerRt>,
        stats: Arc<WorkerStats>,
    ) -> (Option<Pool>, WorkerStatus) {
        let threads = threads.min(MAX_WORKERS);
        let shared = Arc::new(Shared::new(stats));
        let mut status = WorkerStatus {
            priority: rt.as_ref().map(|rt| rt.fifo_priority),
            joined_workgroup: rt.as_ref().is_some_and(|rt| rt.workgroup.is_some()),
            ..WorkerStatus::default()
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let mut handles = Vec::with_capacity(threads);
        for lane in 1..=threads {
            let shared = Arc::clone(&shared);
            let ready = ready_tx.clone();
            // The epoch the worker starts from is read here, before it runs: a worker that reads
            // it itself can come up after `Drop` bumped it for the shutdown, take that bump as its
            // start and sleep through it (a pool dropped right after it started hung in `join` on
            // slow CI runners, 2026-11-15).
            let seen = shared.wake.0.load(Ordering::Acquire);
            let rt = rt.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("komponist-worker-{lane}"))
                .spawn(move || {
                    let realtime = rt.as_ref().map(sys::set_current_thread_realtime);
                    let joined = rt.as_ref().and_then(sys::join_workgroup);
                    let refused = joined.as_ref().and_then(|j| j.as_ref().err().copied());
                    let _ = ready.send((realtime, refused));
                    drop(rt);
                    work(&shared, lane, seen);
                    // Leave before the thread ends (the pool is replaced or dropped, e.g. when
                    // the stream stops); the last reference to the workgroup goes here too.
                    if let Some(Ok((workgroup, token))) = joined {
                        workgroup.leave(token);
                    }
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
            let Ok((realtime, refused)) = ready_rx.recv() else {
                continue;
            };
            if let Some(Err(e)) = realtime {
                status.realtime_error = Some(e.to_string());
            }
            if let Some(errno) = refused {
                status.joined_workgroup = false;
                status.workgroup_error = Some(std::io::Error::from_raw_os_error(errno).to_string());
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
            rt,
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

    /// Whether the pool runs as `rt` asks (see [`WorkerRt::same_as`]).
    pub(crate) fn runs_as(&self, rt: Option<&WorkerRt>) -> bool {
        match (&self.rt, rt) {
            (None, None) => true,
            (Some(a), Some(b)) => a.same_as(b),
            _ => false,
        }
    }

    /// RT. Runs `job(j, lane)` for every `j` in `0 .. count` (≤ [`MAX_JOBS`], all independent),
    /// on the calling thread (lane 0) and the workers, and returns when all are done. `period`
    /// is the deadline of each wait for running jobs (see the module docs).
    #[cfg(test)]
    pub(crate) fn run<F: Fn(usize, usize) + Sync>(
        &mut self,
        count: usize,
        job: &F,
        period: Duration,
    ) {
        self.run_graph(
            count,
            all_jobs(count),
            &|j, lane, _: &Publisher<'_>| job(j, lane),
            period,
        );
    }

    /// RT. Runs a batch of `count` jobs (≤ [`MAX_JOBS`]) with dependencies: `ready` are the jobs
    /// that may start at once, every other job must be published by the job that completes its
    /// inputs (`job(j, lane, publisher)`), or this never returns. Runs on the calling thread
    /// (lane 0) and the workers and returns when all `count` jobs are done. Ready jobs are claimed
    /// lowest first. `period` is the deadline of each wait for running jobs (see the module
    /// docs).
    pub(crate) fn run_graph<F: Fn(usize, usize, &Publisher<'_>) + Sync>(
        &mut self,
        count: usize,
        ready: u128,
        job: &F,
        period: Duration,
    ) {
        /// Calls the closure behind `ctx`.
        ///
        /// # Safety
        /// `ctx` must point to a live `F`.
        unsafe fn trampoline<F: Fn(usize, usize, &Publisher<'_>)>(
            ctx: *const (),
            j: usize,
            lane: usize,
            publisher: &Publisher<'_>,
        ) {
            // SAFETY: guaranteed by the caller (`run_graph` keeps `job` alive until every job is
            // done).
            let f = unsafe { &*ctx.cast::<F>() };
            f(j, lane, publisher);
        }

        debug_assert!(count <= MAX_JOBS);
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
        s.ready.0.reset(ready);
        self.epoch = self.epoch.wrapping_add(1);
        s.wake.0.store(self.epoch, Ordering::Release);
        let helpers = self
            .threads
            .len()
            .min((ready.count_ones() as usize).saturating_sub(1));
        if helpers > 0 {
            sys::wake(&s.wake.0, helpers);
        }
        s.stats.batches.fetch_add(1, Ordering::Relaxed);

        let count = count as u32;
        let mut deadline = None;
        loop {
            let mut mine = 0u32;
            while let Some(j) = s.ready.0.claim() {
                job(j, 0, &Publisher::new(&s.ready.0));
                mine += 1;
            }
            let finished = s.finished.0.fetch_add(mine, Ordering::AcqRel) + mine;
            if finished >= count {
                break;
            }
            // A worker published jobs before it counted its own as finished.
            if s.ready.0.any() {
                continue;
            }
            let until = *deadline.get_or_insert_with(|| Instant::now() + period);
            if !sys::wait_while(&s.finished.0, finished, until) {
                s.stats.late.fetch_add(1, Ordering::Relaxed);
                deadline = Some(Instant::now() + period);
            }
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
unsafe fn noop(_: *const (), _: usize, _: usize, _: &Publisher<'_>) {}

/// A worker's loop: sleep until a new epoch after `seen` (the epoch when it was spawned), claim
/// and run jobs, report each finished job. The shutdown flag is looked at on every round, so even
/// a missed wake-up ends the worker within [`IDLE_WAIT`].
fn work(shared: &Shared, lane: usize, mut seen: u32) {
    let mut fp = sys::fp_state();
    assert_no_alloc::assert_no_alloc(|| {
        loop {
            if shared.shutdown.load(Ordering::Acquire) {
                break;
            }
            let epoch = shared.wake.0.load(Ordering::Acquire);
            if epoch == seen {
                sys::wait_while(&shared.wake.0, seen, Instant::now() + IDLE_WAIT);
                continue;
            }
            seen = epoch;
            while let Some(j) = shared.ready.0.claim() {
                // SAFETY: this thread holds a claimed job of the batch, so the descriptor is the
                // one published with it and stays unchanged until the job is reported.
                let batch = unsafe { *shared.batch.get() };
                if batch.fp != fp {
                    sys::set_fp_state(batch.fp);
                    fp = batch.fp;
                }
                let publisher = Publisher::new(&shared.ready.0);
                // SAFETY: `ctx` is the live closure of `Pool::run_graph`, which waits for this
                // job.
                let job = || unsafe { (batch.run)(batch.ctx, j, lane, &publisher) };
                // A panic still counts the job as finished, so the dispatcher never waits for it
                // forever (the engine's jobs publish their dependents on unwind too); it re-raises
                // the panic.
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                    shared.panicked.store(true, Ordering::Release);
                }
                shared.stats.worker_jobs.fetch_add(1, Ordering::Relaxed);
                let finished = shared.finished.0.fetch_add(1, Ordering::AcqRel) + 1;
                if finished == batch.count || publisher.published.get() {
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

    /// A worker that runs only after the pool was dropped (its shutdown wake-up already
    /// happened) still ends: it starts from the epoch of its spawn, not from the one it finds.
    #[test]
    fn a_worker_that_starts_after_the_shutdown_ends() {
        let shared = Arc::new(Shared::new(Arc::default()));
        let seen = shared.wake.0.load(Ordering::Acquire);
        // `Pool::drop`, before the worker ran.
        shared.shutdown.store(true, Ordering::Release);
        shared.wake.0.fetch_add(1, Ordering::AcqRel);
        sys::wake(&shared.wake.0, usize::MAX);
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::spawn(move || work(&shared, 1, seen))
        };
        let deadline = Instant::now() + Duration::from_millis(500);
        while !worker.is_finished() {
            assert!(Instant::now() < deadline, "the worker missed the shutdown");
            std::thread::sleep(Duration::from_millis(1));
        }
        worker.join().unwrap();
    }

    /// Many pools dropped right after they started: none hangs in `join`.
    #[test]
    fn pools_dropped_at_once_join() {
        for _ in 0..200 {
            let (pool, _) = Pool::start(2, None, Arc::default());
            drop(pool);
        }
    }

    /// Realtime without a workgroup: the priority is reported, no workgroup is joined, and the
    /// pool is kept for the same setting (Linux: same priority; macOS: same period).
    #[test]
    fn realtime_workers_report_their_scheduling() {
        let rt = WorkerRt::new(Duration::from_micros(5333));
        let (pool, status) = Pool::start(2, Some(rt.clone()), Arc::default());
        let pool = pool.unwrap();
        assert_eq!(status.priority, Some(DEFAULT_WORKER_PRIORITY));
        if let Some(e) = &status.realtime_error {
            // Linux machines without an RT rlimit refuse SCHED_FIFO; macOS needs no privilege.
            assert!(cfg!(target_os = "linux"), "{e}");
        }
        assert!(!status.joined_workgroup);
        assert_eq!(status.workgroup_error, None);
        assert!(pool.runs_as(Some(&rt)));
        assert!(!pool.runs_as(None));
        let other_period = WorkerRt::new(Duration::from_micros(2667));
        let other_priority = WorkerRt {
            fifo_priority: 70,
            ..rt.clone()
        };
        if cfg!(target_os = "macos") {
            assert!(!pool.runs_as(Some(&other_period)));
            assert!(pool.runs_as(Some(&other_priority)));
        } else {
            assert!(pool.runs_as(Some(&other_period)));
            assert!(!pool.runs_as(Some(&other_priority)));
        }
    }

    /// macOS: workers join the workgroup they are given (a work interval of the test stands in
    /// for the device's IO workgroup): on a worker, a second join is refused with `EALREADY`.
    #[cfg(target_os = "macos")]
    #[test]
    fn workers_join_the_workgroup() {
        const EALREADY: i32 = 37;
        let interval = komponist_rt::WorkInterval::new(c"komponist-engine-test").unwrap();
        let workgroup = interval.workgroup();
        let rt = WorkerRt {
            workgroup: Some(workgroup.clone()),
            ..WorkerRt::new(Duration::from_micros(2667))
        };
        let (pool, status) = Pool::start(2, Some(rt.clone()), Arc::default());
        let mut pool = pool.unwrap();
        assert_eq!(status.realtime_error, None);
        assert!(status.joined_workgroup);
        assert_eq!(status.workgroup_error, None);
        assert!(pool.runs_as(Some(&rt)));
        // Another lookup of a workgroup is another object: the pool is rebuilt.
        let other = WorkerRt {
            workgroup: Some(
                komponist_rt::WorkInterval::new(c"other")
                    .unwrap()
                    .workgroup(),
            ),
            ..rt.clone()
        };
        assert!(!pool.runs_as(Some(&other)));
        let members = AtomicUsize::new(0);
        let outsiders = AtomicUsize::new(0);
        let job = |_: usize, lane: usize| {
            if lane != 0 {
                match workgroup.join() {
                    Err(EALREADY) => members.fetch_add(1, Ordering::Relaxed),
                    Ok(token) => {
                        workgroup.leave(token);
                        outsiders.fetch_add(1, Ordering::Relaxed)
                    }
                    Err(_) => outsiders.fetch_add(1, Ordering::Relaxed),
                };
            }
            // Every lane sleeps, so the caller cannot take all jobs before a worker wakes.
            std::thread::sleep(Duration::from_millis(2));
        };
        for _ in 0..200 {
            assert_no_alloc::assert_no_alloc(|| pool.run(3, &job, Duration::from_secs(1)));
            if members.load(Ordering::Relaxed) > 0 {
                break;
            }
        }
        assert!(
            members.load(Ordering::Relaxed) > 0,
            "no job ran on a worker"
        );
        assert_eq!(outsiders.load(Ordering::Relaxed), 0);
        // The workgroup outlives the pool, whose workers left it and dropped their references.
        drop(pool);
        let token = workgroup.join().expect("this thread joins after the pool");
        workgroup.leave(token);
    }

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
    fn ready_jobs_are_claimed_lowest_first_and_once() {
        let ready = Ready::new();
        ready.reset(1 << 3 | 1 << 70 | 1);
        assert!(ready.any());
        assert_eq!(ready.claim(), Some(0));
        assert_eq!(ready.claim(), Some(3));
        ready.publish(1 << 1 | 1 << 127);
        assert_eq!(ready.claim(), Some(1));
        assert_eq!(ready.claim(), Some(70));
        assert_eq!(ready.claim(), Some(127));
        assert_eq!(ready.claim(), None);
        assert!(!ready.any());
        assert_eq!(all_jobs(3), 0b111);
        assert_eq!(all_jobs(128), u128::MAX);
    }

    /// A chain of jobs, each published by its predecessor, plus independent ones: every job runs
    /// once and after its predecessor, serially and on the pool.
    #[test]
    fn dependent_jobs_run_after_their_inputs() {
        use std::sync::atomic::AtomicU32;
        let (pool, _) = Pool::start(3, None, Arc::default());
        let mut pool = pool.unwrap();
        for parallel in [false, true] {
            for _ in 0..200 {
                // Jobs 0..4 are independent; 4 needs 0..4 (counter), 5 needs 4.
                let missing = AtomicU32::new(4);
                let done: Vec<AtomicU32> = (0..6).map(|_| AtomicU32::new(0)).collect();
                let job = |j: usize, _lane: usize, publisher: &Publisher<'_>| {
                    match j {
                        4 => assert!((0..4).all(|i| done[i].load(Ordering::Acquire) == 1)),
                        5 => assert_eq!(done[4].load(Ordering::Acquire), 1),
                        _ => {}
                    }
                    std::hint::black_box((0..100).sum::<u64>());
                    done[j].fetch_add(1, Ordering::Release);
                    if j < 4 && missing.fetch_sub(1, Ordering::AcqRel) == 1 {
                        publisher.publish(1 << 4);
                    }
                    if j == 4 {
                        publisher.publish(1 << 5);
                    }
                };
                if parallel {
                    assert_no_alloc::assert_no_alloc(|| {
                        pool.run_graph(6, 0b1111, &job, Duration::from_secs(1))
                    });
                } else {
                    run_serial(6, 0b1111, &job);
                }
                assert!(done.iter().all(|d| d.load(Ordering::Relaxed) == 1));
            }
        }
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
