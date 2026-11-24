//! Bounded worker queue for visible waveform file lookups.

use komponist_audiofile::PoolFile;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
};

pub(super) const LOOKUP_WORKERS: usize = 2;
pub(super) const MAX_LOOKUP_WORKERS: usize = 2 * LOOKUP_WORKERS;
pub(super) const LOOKUP_QUEUE: usize = 16;
// Leave at least half the queue for files whose first metadata has yet to arrive.
pub(super) const MAX_REFRESH_REQUESTS: usize = LOOKUP_QUEUE / 2;

struct LookupSlot(Arc<AtomicUsize>);

impl LookupSlot {
    fn acquire(counter: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        counter
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |used| {
                (used < limit).then_some(used + 1)
            })
            .ok()?;
        Some(Self(Arc::clone(counter)))
    }
}

impl Drop for LookupSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(super) struct PendingLookup {
    pub(super) result: Receiver<Option<Arc<PoolFile>>>,
    // Dropping the entry tells a worker it can skip a queued lookup for an invisible file.
    _interest: Arc<()>,
}

struct Lookup<T> {
    file: T,
    dir: Option<PathBuf>,
    result: SyncSender<Option<Arc<PoolFile>>>,
    interest: Weak<()>,
    ctx: egui::Context,
    refresh_slot: Option<LookupSlot>,
}

#[derive(Debug)]
pub(super) struct LookupWorker<T> {
    queue: SyncSender<Lookup<T>>,
    refresh_requests: Arc<AtomicUsize>,
}

impl<T: Clone + Send + 'static> LookupWorker<T> {
    pub(super) fn spawn(
        workers: &Arc<AtomicUsize>,
        resolve: impl Fn(&T, Option<&Path>) -> Option<Arc<PoolFile>> + Send + Sync + 'static,
    ) -> Option<Self> {
        let (queue, jobs) = mpsc::sync_channel::<Lookup<T>>(LOOKUP_QUEUE);
        let jobs = Arc::new(Mutex::new(jobs));
        let resolve = Arc::new(resolve);
        let mut started = false;
        for n in 0..LOOKUP_WORKERS {
            let Some(thread_slot) = LookupSlot::acquire(workers, MAX_LOOKUP_WORKERS) else {
                break;
            };
            let jobs = Arc::clone(&jobs);
            let resolve = Arc::clone(&resolve);
            // A slow mount can hold one lookup without holding the queue mutex or the UI.
            let worker = std::thread::Builder::new()
                .name(format!("komponist-waveform-file-{n}"))
                .spawn(move || {
                    let _thread_slot = thread_slot;
                    loop {
                        let next = jobs.lock().unwrap_or_else(|err| err.into_inner()).recv();
                        let Ok(job) = next else { break };
                        if job.interest.strong_count() == 0 {
                            continue;
                        }
                        let file = resolve(&job.file, job.dir.as_deref());
                        drop(job.refresh_slot);
                        if job.result.send(file).is_ok() {
                            job.ctx.request_repaint();
                        }
                    }
                });
            started |= worker.is_ok();
        }
        started.then(|| Self {
            queue,
            refresh_requests: Arc::default(),
        })
    }

    pub(super) fn refresh_slots(&self) -> usize {
        MAX_REFRESH_REQUESTS.saturating_sub(self.refresh_requests.load(Ordering::Relaxed))
    }

    pub(super) fn request(
        &self,
        file: &T,
        dir: Option<&Path>,
        ctx: &egui::Context,
        first_lookup: bool,
    ) -> Option<PendingLookup> {
        let refresh_slot = if first_lookup {
            None
        } else {
            Some(LookupSlot::acquire(
                &self.refresh_requests,
                MAX_REFRESH_REQUESTS,
            )?)
        };
        let (send, result) = mpsc::sync_channel(1);
        let interest = Arc::new(());
        self.queue
            .try_send(Lookup {
                file: file.clone(),
                dir: dir.map(Path::to_path_buf),
                result: send,
                interest: Arc::downgrade(&interest),
                ctx: ctx.clone(),
                refresh_slot,
            })
            .ok()?;
        Some(PendingLookup {
            result,
            _interest: interest,
        })
    }
}
