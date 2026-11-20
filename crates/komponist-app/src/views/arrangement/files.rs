//! Background file lookups and cached waveform meshes for visible audio clips.
use komponist_audiofile::{AudioError, FilePool, PeakData, PeakState, PoolFile};
use komponist_model::{AudioFile, Clip, ClipContent, Tempo};
use komponist_ui::timeline::{AudioFileState, WaveMesh, WaveSpec};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    time::{Duration, Instant},
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const LOOKUP_RETRY: Duration = Duration::from_millis(50);
const LOOKUP_WORKERS: usize = 2;
const MAX_LOOKUP_WORKERS: usize = 2 * LOOKUP_WORKERS;
const LOOKUP_QUEUE: usize = 16;
// Leave at least half the queue for files whose first metadata has yet to arrive.
const MAX_REFRESH_REQUESTS: usize = LOOKUP_QUEUE / 2;

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
struct PendingLookup {
    result: Receiver<Option<Arc<PoolFile>>>,
    // Dropping the entry tells a worker it can skip a queued lookup for an invisible file.
    _interest: Arc<()>,
}

struct Lookup {
    file: AudioFile,
    dir: Option<PathBuf>,
    result: SyncSender<Option<Arc<PoolFile>>>,
    interest: Weak<()>,
    ctx: egui::Context,
    refresh_slot: Option<LookupSlot>,
}

#[derive(Debug)]
struct LookupWorker {
    queue: SyncSender<Lookup>,
    refresh_requests: Arc<AtomicUsize>,
}

impl LookupWorker {
    fn spawn(
        workers: &Arc<AtomicUsize>,
        resolve: impl Fn(&AudioFile, Option<&Path>) -> Option<Arc<PoolFile>> + Send + Sync + 'static,
    ) -> Option<Self> {
        let (queue, jobs) = mpsc::sync_channel::<Lookup>(LOOKUP_QUEUE);
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

    fn request(
        &self,
        file: &AudioFile,
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

fn resolve(file: &AudioFile, dir: Option<&Path>) -> Option<Arc<PoolFile>> {
    file.resolve(dir)
        .and_then(|path| FilePool::global().open(&path).ok())
}

#[derive(Debug, Default)]
struct FileEntry {
    checked: Option<Instant>,
    file: Option<Arc<PoolFile>>,
    peaks: Option<Arc<PeakData>>,
    pending: Option<PendingLookup>,
    used: u64,
}

impl FileEntry {
    fn poll(&mut self) {
        let Some(pending) = &self.pending else { return };
        match pending.result.try_recv() {
            Ok(file) => {
                let reappeared = self.checked.is_some() && self.file.is_none();
                if let Some(file) = &file
                    && (reappeared
                        || matches!(file.peaks_state(), PeakState::Failed(AudioError::Io(_))))
                {
                    file.allow_peak_retry();
                }
                self.file = file;
            }
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {}
        }
        self.checked = Some(Instant::now());
        self.pending = None;
    }

    fn state(&mut self, ctx: &egui::Context) -> AudioFileState {
        let Some(file) = &self.file else {
            return if self.checked.is_some() {
                AudioFileState::Missing
            } else {
                AudioFileState::Pending
            };
        };
        let state = file.peaks_state();
        // Register once per peak computation; its callback lives until completion.
        let state = if matches!(state, PeakState::Idle) {
            let ctx = ctx.clone();
            file.request_peaks(move || ctx.request_repaint())
        } else {
            state
        };
        match state {
            PeakState::Ready(peaks) => {
                self.peaks = Some(peaks);
                AudioFileState::Ready
            }
            PeakState::Failed(_) => AudioFileState::Missing,
            _ => AudioFileState::Pending,
        }
    }
}
#[derive(Debug)]
struct WaveEntry {
    // Keep the pointer used as the map key alive until this mesh leaves the cache.
    _clip: Arc<Clip>,
    peaks: Option<Arc<PeakData>>,
    spec: WaveSpec,
    mesh: WaveMesh,
    used: u64,
}
/// Files are rechecked once a second, allowing missing links to come back online.
#[derive(Debug, Default)]
pub struct AudioCache {
    files: HashMap<(AudioFile, Option<PathBuf>), FileEntry>,
    waves: HashMap<usize, WaveEntry>,
    worker: Option<LookupWorker>,
    // Still-running workers from abandoned documents count against the same limit.
    workers: Arc<AtomicUsize>,
    worker_retry: Option<Instant>,
    refresh_context: Option<egui::Context>,
    frame: u64,
}
impl AudioCache {
    /// Forget the previous document's waveforms and disconnect any lookups it queued.
    pub(super) fn clear(&mut self) {
        self.files.clear();
        self.waves.clear();
        // A blocked filesystem call cannot be cancelled. A fresh queue lets the new document
        // start work on different threads while the previous generation winds down.
        self.worker = None;
        self.worker_retry = None;
        self.refresh_context = None;
    }

    pub fn begin_frame(&mut self) {
        self.frame += 1;
    }
    pub fn end_frame(&mut self) {
        self.waves.retain(|_, e| e.used == self.frame);
        // Bound file handles to visible clips; the global pool shares active engine handles.
        self.files.retain(|_, file| file.used == self.frame);
        if let Some(ctx) = self.refresh_context.take()
            && let Some(worker) = &self.worker
        {
            Self::refresh_files(&mut self.files, worker, &ctx);
        }
    }

    fn refresh_files(
        files: &mut HashMap<(AudioFile, Option<PathBuf>), FileEntry>,
        worker: &LookupWorker,
        ctx: &egui::Context,
    ) {
        let slots =
            MAX_REFRESH_REQUESTS.saturating_sub(worker.refresh_requests.load(Ordering::Relaxed));
        // The frame has already queued initial lookups, including for clips drawn later.
        // Select just the oldest visible refreshes that can fit, independent of paint order.
        let mut due = Vec::with_capacity(slots);
        if slots == 0 {
            return;
        }
        for ((source, dir), file) in files {
            let Some(checked) = file.checked else {
                continue;
            };
            if file.pending.is_some() || checked.elapsed() < REFRESH_INTERVAL {
                continue;
            }
            let index = due.partition_point(|(time, _, _, _)| *time <= checked);
            if index < slots {
                if due.len() == slots {
                    due.pop();
                }
                due.insert(index, (checked, source, dir, file));
            }
        }
        for (_, source, dir, file) in due {
            file.pending = worker.request(source, dir.as_deref(), ctx, false);
            if file.pending.is_none() {
                ctx.request_repaint_after(LOOKUP_RETRY);
                break;
            }
        }
    }

    pub fn get(
        &mut self,
        clip: &Arc<Clip>,
        tempo: Tempo,
        dir: Option<&Path>,
        ppt: f32,
        height: f32,
        ctx: &egui::Context,
    ) -> Option<(AudioFileState, &WaveMesh)> {
        let ClipContent::Audio(a) = &clip.content else {
            return None;
        };
        let file = self
            .files
            .entry((a.file.clone(), dir.map(Path::to_path_buf)))
            .or_default();
        file.used = self.frame;
        file.poll();
        if file.pending.is_none()
            && file
                .checked
                .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL)
        {
            if self.worker.is_none()
                && self
                    .worker_retry
                    .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL)
            {
                self.worker = LookupWorker::spawn(&self.workers, resolve);
                self.worker_retry = self.worker.is_none().then(Instant::now);
            }
            if let Some(worker) = self.worker.as_ref() {
                if file.checked.is_none() {
                    file.pending = worker.request(&a.file, dir, ctx, true);
                    if file.pending.is_none() {
                        ctx.request_repaint_after(LOOKUP_RETRY);
                    }
                } else {
                    self.refresh_context.get_or_insert_with(|| ctx.clone());
                }
            }
        }
        // Completion wakes the UI immediately. Periodic checks also detect files that reappear.
        ctx.request_repaint_after(REFRESH_INTERVAL);
        let state = file.state(ctx);
        let peaks = file.peaks.as_ref();
        let spec = WaveSpec {
            width: (clip.length.0 as f32 * ppt
                - 2.0 * komponist_ui::widgets::metrics::timeline::CLIP_INSET_X)
                .max(komponist_ui::widgets::metrics::timeline::CLIP_MIN_WIDTH),
            px_per_tick: ppt,
            body_height: height,
            offset: a.offset,
            frames: a.source_frames(clip.length, tempo),
            gain: 10.0f32.powf(a.gain_db / 20.0),
            fade_in: a.fade_in.0 as f32 * ppt,
            fade_out: a.fade_out.0 as f32 * ppt,
        };
        let entry = self
            .waves
            .entry(Arc::as_ptr(clip) as usize)
            .or_insert_with(|| WaveEntry {
                _clip: clip.clone(),
                peaks: None,
                spec,
                mesh: WaveMesh::flat(spec.width, ppt, height),
                used: 0,
            });
        let changed = match (&entry.peaks, peaks) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if entry.used == 0 || entry.spec != spec || changed {
            entry.mesh = peaks.map_or_else(
                || WaveMesh::flat(spec.width, ppt, height),
                |p| WaveMesh::from_peaks(p, &spec),
            );
            entry.peaks = peaks.cloned();
            entry.spec = spec;
        }
        entry.used = self.frame;
        Some((state, &entry.mesh))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::edits::{self, NewAudioTrack, NewClip, NewClipContent};
    use komponist_model::{Project, Tick};
    fn clip(path: &Path) -> Arc<Clip> {
        let (p, t) =
            edits::add_audio_track(&Project::new("cache"), NewAudioTrack::default()).unwrap();
        let (p, c) = edits::add_clip(
            &p,
            t,
            NewClip {
                start: Tick::ZERO,
                length: Tick(3840),
                name: "wave".into(),
                content: NewClipContent::Audio {
                    file: AudioFile::new(path, None, 96000, 48000, 1),
                    offset: 0,
                },
            },
        )
        .unwrap();
        p.find_clip(c).unwrap().1.clone()
    }
    #[test]
    fn fixed_tempo_changes_rebuild_the_same_clip_window() {
        let c = clip(Path::new("/missing/cache-test.wav"));
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.begin_frame();
        cache
            .get(&c, Tempo::new(120.0), None, 0.02, 36.0, &ctx)
            .unwrap();
        assert_eq!(cache.waves.values().next().unwrap().spec.frames, 96000);
        cache
            .get(&c, Tempo::new(240.0), None, 0.02, 36.0, &ctx)
            .unwrap();
        assert_eq!(cache.waves.values().next().unwrap().spec.frames, 48000);
        cache.end_frame();
        assert_eq!(cache.waves.len(), 1);
        cache.begin_frame();
        cache.end_frame();
        assert!(cache.waves.is_empty());
        assert!(cache.files.is_empty());
    }
    fn write_wav(path: &Path, frames: usize, magnitude: i16) {
        let mut wav = hound::WavWriter::create(
            path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 48_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for i in 0..frames {
            wav.write_sample(if i % 2 == 0 { magnitude } else { -magnitude })
                .unwrap();
        }
        wav.finalize().unwrap();
    }

    fn read_wave<'a>(
        cache: &'a mut AudioCache,
        clip: &Arc<Clip>,
        dir: Option<&Path>,
        ctx: &egui::Context,
    ) -> (AudioFileState, &'a WaveMesh) {
        cache
            .get(clip, Tempo::new(120.0), dir, 0.02, 36.0, ctx)
            .unwrap()
    }

    fn wait_for_wave(
        cache: &mut AudioCache,
        clip: &Arc<Clip>,
        dir: Option<&Path>,
        ctx: &egui::Context,
        ready: impl Fn(AudioFileState, &WaveMesh) -> bool,
    ) -> WaveMesh {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let (state, mesh) = read_wave(cache, clip, dir, ctx);
            if ready(state, mesh) {
                return mesh.clone();
            }
            assert!(
                Instant::now() < deadline,
                "waveform did not reach expected state: {state:?}"
            );
            cache.end_frame();
            cache.begin_frame();
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn expire(cache: &mut AudioCache) {
        for file in cache.files.values_mut() {
            file.checked = Some(Instant::now() - REFRESH_INTERVAL);
        }
    }

    #[test]
    fn missing_files_recover_and_keep_the_last_waveform_during_a_disconnection() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("source.wav");
        let clip = clip(&path);
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.begin_frame();
        assert_eq!(
            read_wave(&mut cache, &clip, None, &ctx).0,
            AudioFileState::Pending
        );
        let missing = wait_for_wave(&mut cache, &clip, None, &ctx, |state, _| {
            state == AudioFileState::Missing
        });
        assert!(missing.is_empty());

        write_wav(&path, 96_000, 16_000);
        expire(&mut cache);
        let first = wait_for_wave(&mut cache, &clip, None, &ctx, |state, _| {
            state == AudioFileState::Ready
        });
        assert!(!first.is_empty());
        std::fs::remove_file(&path).unwrap();
        expire(&mut cache);

        let (_, checking) = read_wave(&mut cache, &clip, None, &ctx);
        assert_eq!(checking, &first);
        let disconnected = wait_for_wave(&mut cache, &clip, None, &ctx, |state, _| {
            state == AudioFileState::Missing
        });
        assert_eq!(disconnected, first);

        // Changed metadata at the same path must replace the old waveform when available.
        write_wav(&path, 48_000, 2_000);
        expire(&mut cache);
        let restored = wait_for_wave(&mut cache, &clip, None, &ctx, |state, mesh| {
            state == AudioFileState::Ready && mesh != &first
        });
        assert!(!restored.is_empty());
    }

    #[test]
    fn a_returning_file_can_retry_peaks_on_the_same_pool_entry() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("source.wav");
        let moved = scratch.path().join("disconnected.wav");
        write_wav(&path, 48_000, 10_000);
        let pool = FilePool::new();
        let held_file = pool.open(&path).unwrap();
        std::fs::rename(&path, &moved).unwrap();
        assert!(matches!(held_file.peaks(), Err(AudioError::Io(_))));

        let lookup_pool = pool.clone();
        let mut cache = AudioCache::default();
        cache.worker = LookupWorker::spawn(&cache.workers, move |file, dir| {
            file.resolve(dir)
                .and_then(|path| lookup_pool.open(&path).ok())
        });
        assert!(cache.worker.is_some());
        let clip = clip(&path);
        let ctx = egui::Context::default();
        cache.begin_frame();
        let missing = wait_for_wave(&mut cache, &clip, None, &ctx, |state, _| {
            state == AudioFileState::Missing
        });
        assert!(missing.is_empty());

        std::fs::rename(&moved, &path).unwrap();
        assert!(Arc::ptr_eq(&held_file, &pool.open(&path).unwrap()));
        expire(&mut cache);
        let restored = wait_for_wave(&mut cache, &clip, None, &ctx, |state, _| {
            state == AudioFileState::Ready
        });
        assert!(!restored.is_empty());
    }

    fn block_lookup_workers(cache: &mut AudioCache) -> (Receiver<()>, mpsc::Sender<()>) {
        let (started, calls) = mpsc::channel();
        let (resume, held) = mpsc::channel();
        let held = Mutex::new(held);
        cache.worker = LookupWorker::spawn(&cache.workers, move |_, _| {
            let _ = started.send(());
            let _ = held.lock().unwrap().recv();
            None
        });
        assert!(cache.worker.is_some());
        (calls, resume)
    }

    fn receive_metadata(
        cache: &mut AudioCache,
        clip: &Arc<Clip>,
        ctx: &egui::Context,
    ) -> Arc<PoolFile> {
        let ClipContent::Audio(source) = &clip.content else {
            panic!("audio clip");
        };
        assert_eq!(read_wave(cache, clip, None, ctx).0, AudioFileState::Pending);
        // Wait for this file's metadata job, independently of the shared peak decoder.
        cache.files[&(source.file.clone(), None)]
            .pending
            .as_ref()
            .expect("local metadata job scheduled")
            .result
            .recv_timeout(Duration::from_secs(5))
            .expect("local metadata worker replied")
            .expect("local WAV was found")
    }

    fn wait_for_worker_count(cache: &AudioCache, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while cache.workers.load(Ordering::Relaxed) != count {
            assert!(
                Instant::now() < deadline,
                "abandoned metadata workers did not exit"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_new_document_can_load_with_both_old_workers_blocked_and_threads_stay_bounded() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("local.wav");
        write_wav(&path, 48_000, 9_000);
        let local = clip(&path);
        let old = [
            clip(&scratch.path().join("blocked-a.wav")),
            clip(&scratch.path().join("blocked-b.wav")),
        ];
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.begin_frame();
        let (old_calls, resume_old) = block_lookup_workers(&mut cache);
        for clip in &old {
            assert_eq!(
                read_wave(&mut cache, clip, None, &ctx).0,
                AudioFileState::Pending
            );
        }
        for _ in &old {
            old_calls.recv_timeout(Duration::from_secs(5)).unwrap();
        }

        cache.clear();
        let local_file = receive_metadata(&mut cache, &local, &ctx);
        assert_eq!(local_file.path(), path.canonicalize().unwrap());
        assert_eq!(cache.workers.load(Ordering::Relaxed), MAX_LOOKUP_WORKERS);

        cache.clear();
        wait_for_worker_count(&cache, LOOKUP_WORKERS);
        let (new_calls, resume_new) = block_lookup_workers(&mut cache);
        for clip in &old {
            read_wave(&mut cache, clip, None, &ctx);
        }
        for _ in &old {
            new_calls.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        for _ in 0..3 {
            cache.clear();
            assert_eq!(
                read_wave(&mut cache, &local, None, &ctx).0,
                AudioFileState::Pending
            );
            assert!(cache.worker.is_none());
            assert_eq!(cache.workers.load(Ordering::Relaxed), MAX_LOOKUP_WORKERS);
        }
        for _ in &old {
            resume_old.send(()).unwrap();
            resume_new.send(()).unwrap();
        }
        wait_for_worker_count(&cache, 0);
        assert!(cache.worker_retry.is_some());
        cache.worker_retry = Some(Instant::now() - REFRESH_INTERVAL);
        let retried = receive_metadata(&mut cache, &local, &ctx);
        assert_eq!(retried.id(), local_file.id());
    }

    #[test]
    fn periodic_refreshes_cannot_fill_the_slots_for_a_new_visible_file() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("first-read.wav");
        write_wav(&path, 48_000, 9_000);
        let new_clip = clip(&path);
        let (started, calls) = mpsc::channel();
        let (resume, held) = mpsc::channel();
        let held = Mutex::new(held);
        let started_reads = AtomicUsize::new(0);
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.worker = LookupWorker::spawn(&cache.workers, move |file, dir| {
            if started_reads.fetch_add(1, Ordering::Relaxed) < LOOKUP_WORKERS {
                let _ = started.send(());
                let _ = held.lock().unwrap().recv();
            }
            resolve(file, dir)
        });
        cache.begin_frame();
        for index in 0..LOOKUP_QUEUE + MAX_REFRESH_REQUESTS {
            let repeated = clip(&scratch.path().join(format!("periodic-{index}.wav")));
            let ClipContent::Audio(link) = &repeated.content else {
                panic!("audio clip")
            };
            cache.files.insert(
                (link.file.clone(), None),
                FileEntry {
                    checked: Some(Instant::now() - REFRESH_INTERVAL),
                    ..FileEntry::default()
                },
            );
            read_wave(&mut cache, &repeated, None, &ctx);
        }
        cache.end_frame();
        for _ in 0..LOOKUP_WORKERS {
            calls.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(
            cache
                .files
                .values()
                .filter(|entry| entry.pending.is_some())
                .count(),
            MAX_REFRESH_REQUESTS
        );

        cache.begin_frame();
        assert_eq!(
            read_wave(&mut cache, &new_clip, None, &ctx).0,
            AudioFileState::Pending
        );
        let ClipContent::Audio(new_link) = &new_clip.content else {
            panic!("audio clip")
        };
        assert!(
            cache.files[&(new_link.file.clone(), None)]
                .pending
                .is_some()
        );
        for _ in 0..LOOKUP_WORKERS {
            resume.send(()).unwrap();
        }
        let ready = wait_for_wave(&mut cache, &new_clip, None, &ctx, |state, _| {
            state == AudioFileState::Ready
        });
        assert!(!ready.is_empty());
    }

    #[test]
    fn the_oldest_visible_periodic_checks_get_the_first_refresh_slots() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("returning.wav");
        let returning = clip(&path);
        let ctx = egui::Context::default();
        let mut cache = AudioCache::default();
        cache.begin_frame();
        let ClipContent::Audio(link) = &returning.content else {
            panic!("audio clip")
        };
        let key = (link.file.clone(), None);
        cache.files.insert(
            key.clone(),
            FileEntry {
                checked: Some(Instant::now() - 2 * REFRESH_INTERVAL),
                ..FileEntry::default()
            },
        );
        write_wav(&path, 48_000, 9_000);

        for index in 0..LOOKUP_QUEUE {
            let clip = clip(&scratch.path().join(format!("early-row-{index}.wav")));
            let ClipContent::Audio(link) = &clip.content else {
                panic!("audio clip")
            };
            cache.files.insert(
                (link.file.clone(), None),
                FileEntry {
                    checked: Some(Instant::now() - REFRESH_INTERVAL),
                    ..FileEntry::default()
                },
            );
            read_wave(&mut cache, &clip, None, &ctx);
        }
        // This source is encountered only after every much younger periodic candidate.
        assert_eq!(
            read_wave(&mut cache, &returning, None, &ctx).0,
            AudioFileState::Missing
        );
        cache.end_frame();
        assert!(cache.files[&key].pending.is_some());
        cache.begin_frame();
        let ready = wait_for_wave(&mut cache, &returning, None, &ctx, |state, _| {
            state == AudioFileState::Ready
        });
        assert!(!ready.is_empty());
    }

    #[test]
    fn a_blocked_lookup_deduplicates_and_cannot_replace_a_new_project_waveform() {
        let scratch = tempfile::tempdir().unwrap();
        let old_dir = scratch.path().join("old");
        let new_dir = scratch.path().join("new");
        std::fs::create_dir(&old_dir).unwrap();
        std::fs::create_dir(&new_dir).unwrap();
        write_wav(&old_dir.join("source.wav"), 48_000, 12_000);
        write_wav(&new_dir.join("source.wav"), 48_000, 4_000);
        let clip = clip(&old_dir.join("source.wav"));
        let ctx = egui::Context::default();

        let (started, first_request) = mpsc::channel();
        let (resume, blocked) = mpsc::channel();
        let blocked = Mutex::new(blocked);
        let (returned, old_lookup_done) = mpsc::channel();
        let mut cache = AudioCache::default();
        cache.worker = LookupWorker::spawn(&cache.workers, move |_, dir| {
            let dir = dir.unwrap();
            let old = dir.ends_with("old");
            if old {
                started.send(()).unwrap();
                blocked
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
            let file = FilePool::global().open(&dir.join("source.wav")).ok();
            if old {
                returned.send(()).unwrap();
            }
            file
        });
        assert!(cache.worker.is_some());
        cache.begin_frame();
        let (state, _) = read_wave(&mut cache, &clip, Some(&old_dir), &ctx);
        assert_eq!(state, AudioFileState::Pending);
        first_request.recv_timeout(Duration::from_secs(5)).unwrap();
        for _ in 0..LOOKUP_QUEUE {
            let (state, _) = read_wave(&mut cache, &clip, Some(&old_dir), &ctx);
            assert_eq!(state, AudioFileState::Pending);
        }
        assert!(
            first_request.try_recv().is_err(),
            "one request per file and directory"
        );
        cache.end_frame();

        cache.begin_frame();
        let fresh = wait_for_wave(&mut cache, &clip, Some(&new_dir), &ctx, |state, _| {
            state == AudioFileState::Ready
        });
        cache.end_frame();
        assert_eq!(cache.files.len(), 1);
        resume.send(()).unwrap();
        old_lookup_done
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let (state, mesh) = read_wave(&mut cache, &clip, Some(&new_dir), &ctx);
        assert_eq!(state, AudioFileState::Ready);
        assert_eq!(mesh, &fresh);

        // A new document can reuse the same directory and link; resolve it again rather
        // than inheriting a result owned by the previous document.
        cache.clear();
        let (state, _) = read_wave(&mut cache, &clip, Some(&new_dir), &ctx);
        assert_eq!(state, AudioFileState::Pending);
    }
}
