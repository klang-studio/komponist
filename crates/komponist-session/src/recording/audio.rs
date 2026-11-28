//! Audio take preparation, file finalization, and document commit on the control thread.

mod files;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use komponist_engine::recording::Recorder;
use komponist_model::edits::{self, EditError, NewClip, NewClipContent};
use komponist_model::{
    AudioClip, AudioFile, AudioTake, Clip, ClipContent, ClipId, Document, Project, Tempo, Tick,
    TrackId, TrackKind,
};

use super::automation::AutomationCapture;
use super::midi::{Commit, TakeNames};
use super::writer::{
    self, RecordedTake, RecordingConfig, RecordingError, RecordingProgress, RecordingResult,
    RecordingSession,
};

/// How long Save (and opening the export) waits for a stopped take to drain and be copied.
pub const TAKE_FINISH_TIMEOUT: Duration = Duration::from_secs(2);

/// The live view of an audio session, read by the arrangement, dock and inspector.
#[derive(Default)]
pub struct AudioRecording {
    /// Song position where this recording started (before any loop wrap).
    pub start: Tick,
    /// Current transport head.
    pub head: Tick,
    progress: Vec<RecordingProgress>,
    clips: Vec<(TrackId, Arc<Clip>)>,
    completed: Vec<(Arc<Clip>, Arc<RecordingProgress>)>,
}

impl AudioRecording {
    /// Deterministic view-only fixture; it cannot start capture or own recording files.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn preview(
        start: Tick,
        head: Tick,
        progress: Vec<RecordingProgress>,
        clips: Vec<(TrackId, Arc<Clip>)>,
    ) -> Self {
        Self {
            start,
            head,
            progress,
            clips,
            completed: Vec::new(),
        }
    }

    /// Current drawn clip on one track (not yet in the document).
    pub fn clip(&self, track: TrackId) -> Option<&Arc<Clip>> {
        self.clips
            .iter()
            .find(|(id, _)| *id == track)
            .map(|(_, clip)| clip)
    }
    /// First live audio track/clip, for the editor.
    pub fn first(&self) -> Option<(TrackId, &Arc<Clip>)> {
        self.clips.first().map(|(track, clip)| (*track, clip))
    }
    /// Bounded, measured waveform/peak and file telemetry for a live track.
    pub fn progress(&self, track: TrackId) -> Option<&RecordingProgress> {
        self.progress
            .iter()
            .find(|progress| progress.track == track)
    }
    /// Completed overlays first, followed by the current pass.
    pub fn overlays(
        &self,
        track: TrackId,
    ) -> impl Iterator<Item = (&Arc<Clip>, &RecordingProgress)> {
        self.completed
            .iter()
            .filter(move |(_, progress)| progress.track == track)
            .map(|(clip, progress)| (clip, progress.as_ref()))
            .chain(self.clip(track).zip(self.progress(track)))
    }

    /// Tracks represented in the live view.
    pub fn tracks(&self) -> impl Iterator<Item = TrackId> + '_ {
        self.progress.iter().map(|progress| progress.track)
    }
}

struct Capture {
    session: RecordingSession,
    view: AudioRecording,
    directory: PathBuf,
    provisional: bool,
    stopping: bool,
    base: u32,
    rate: u32,
    owned: BTreeSet<PathBuf>,
    material: bool,
    /// The writer has finished; its takes are being copied into the project's Takes folder.
    staging: Option<Staging>,
}

/// Copies of finished takes, made on a worker so that a long copy and its `fsync` never hold up a
/// frame (#221). The take goes into the document once the copies are done; until then the
/// capture stays active, so no new take starts and Save waits for it.
struct Staging {
    result: RecordingResult,
    /// The project folder the copies go to (its `Takes`).
    directory: PathBuf,
    done: mpsc::Receiver<StageResult>,
    /// Hangs up once the worker has ended, its clean-up included.
    alive: mpsc::Receiver<()>,
}

/// Copies a staging worker made. Until the take that uses them is committed, this value is
/// their only owner: dropping it (a discarded take, a result nobody received) deletes them.
/// They live in a private folder in `Takes` under no final name, so nothing (an audio import, a
/// file browser) can link them before the take does.
struct StagedCopies {
    private: PathBuf,
    copies: BTreeMap<PathBuf, PathBuf>,
}

impl StagedCopies {
    /// Gives the copies their final names in `target`; the caller owns them then (the take's
    /// document links refer to them). Returns `source → final path`. The new entries are flushed
    /// before this returns, since the caller deletes the originals later: a power loss must not
    /// keep the deletion and lose the copy. If the flush fails, the published files go again.
    fn publish(self, target: &Path) -> std::io::Result<BTreeMap<PathBuf, PathBuf>> {
        let published = files::publish(&self.copies, target)?;
        if let Err(error) = crate::project_io::sync_directory(target) {
            for path in published.values() {
                let _ = std::fs::remove_file(path);
            }
            return Err(error);
        }
        Ok(published)
    }
}

impl Drop for StagedCopies {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.private);
    }
}

/// A background copy into `target` that failed: the same Save does not copy these sources again
/// on the UI thread, a later one does (#221).
struct FailedStage {
    target: PathBuf,
    sources: BTreeSet<PathBuf>,
    error: Arc<io::Error>,
}

type StageResult = std::io::Result<StagedCopies>;

/// Tests: holds the next staging worker after its private copies are made until `go` gets a
/// message (or hangs up), and reports on `finished` when it is done, its clean-up included.
/// `ready` hears when it waits there.
#[cfg(any(test, feature = "test-support"))]
struct StageGate {
    go: mpsc::Receiver<()>,
    finished: mpsc::Sender<()>,
    ready: Option<mpsc::Sender<()>>,
}

/// Runs [`files::stage`] on a worker; returns the receivers of its result and of its end. If
/// nobody takes the result (the take was discarded, the app closes), the [`StagedCopies`] in it
/// delete themselves; the sources stay as they are.
fn stage_in_background(
    paths: Vec<PathBuf>,
    target: PathBuf,
    #[cfg(any(test, feature = "test-support"))] gate: Option<StageGate>,
) -> (mpsc::Receiver<StageResult>, mpsc::Receiver<()>) {
    let (sender, done) = mpsc::sync_channel(1);
    let (alive, ended) = mpsc::channel::<()>();
    let worker = sender.clone();
    let spawned = std::thread::Builder::new()
        .name("komponist-take-staging".into())
        .spawn(move || {
            let staged = files::stage_private(&paths, &target)
                .map(|(private, copies)| StagedCopies { private, copies });
            #[cfg(any(test, feature = "test-support"))]
            if let Some(gate) = &gate {
                if let Some(ready) = &gate.ready {
                    let _ = ready.send(());
                }
                let _ = gate.go.recv();
            }
            // A failed send drops the copies here, before `alive` reports the end.
            let _ = worker.send(staged);
            drop(alive);
            #[cfg(any(test, feature = "test-support"))]
            if let Some(gate) = gate {
                let _ = gate.finished.send(());
            }
        });
    if let Err(error) = spawned {
        let _ = sender.send(Err(error));
    }
    (done, ended)
}

impl Capture {
    fn remember_progress(
        &mut self,
        progress: &RecordingProgress,
        provisional: &mut BTreeSet<PathBuf>,
    ) -> bool {
        if progress.path.as_os_str().is_empty() {
            return false;
        }
        self.owned.insert(progress.path.clone());
        if progress.frames == 0 {
            return false;
        }
        self.material = true;
        if self.provisional {
            provisional.insert(progress.path.clone());
        }
        true
    }
}

fn recording_preview(
    progress: &RecordingProgress,
    rate: u32,
    tempo: Tempo,
    id: ClipId,
    name: String,
) -> Arc<Clip> {
    let file = AudioFile::new(
        &progress.path,
        None,
        progress.frames,
        rate,
        progress.channels,
    );
    let source = AudioClip::new(file, 0, tempo);
    let length = source.frames_to_ticks(progress.frames, tempo).max(Tick(1));
    Arc::new(Clip {
        id,
        name,
        start: progress.start,
        length,
        loop_length: None,
        mute: false,
        transpose: 0,
        velocity_pct: 0,
        automation: Vec::new(),
        content: ClipContent::Audio(source),
    })
}

/// The audio take and its files, pending commits and finalization worker.
/// Session-owned files survive undo. Only an explicit discard deletes an active session.
#[derive(Default)]
pub struct AudioCapture {
    capture: Option<Capture>,
    provisional: BTreeSet<PathBuf>,
    pending_midi: Option<Commit>,
    pending_automation: Option<AutomationCapture>,
    pending_removals: BTreeSet<PathBuf>,
    /// The ends of staging workers whose take was dropped; shutdown waits for their clean-up.
    cleanup: Vec<mpsc::Receiver<()>>,
    /// The last background copy that failed, until the next Save.
    failed_stage: Option<FailedStage>,
    #[cfg(any(test, feature = "test-support"))]
    stage_gate: Option<StageGate>,
}

impl AudioCapture {
    pub fn running(&self) -> Option<&AudioRecording> {
        self.capture.as_ref().map(|capture| &capture.view)
    }
    pub fn is_active(&self) -> bool {
        self.capture.is_some()
    }
    pub fn has_material(&self) -> bool {
        self.capture.as_ref().is_some_and(|capture| {
            capture.material || capture.stopping || capture.staging.is_some()
        }) || self.pending_midi.is_some()
            || self.pending_automation.is_some()
    }
    pub fn defer_midi(&mut self, commit: Option<Commit>) {
        if commit.is_some() {
            self.pending_midi = commit;
        }
    }
    pub fn defer_automation(&mut self, pass: AutomationCapture) {
        if pass.has_points() {
            if let Some(previous) = &mut self.pending_automation {
                previous.merge(pass);
            } else {
                self.pending_automation = Some(pass);
            }
        }
    }

    pub fn pending_automation(&self) -> Option<&AutomationCapture> {
        self.pending_automation.as_ref()
    }

    pub fn references_automation_lane(
        &self,
        owner: komponist_model::automation::AutomationOwner,
        lane: komponist_model::automation::LaneId,
    ) -> bool {
        self.pending_automation
            .as_ref()
            .is_some_and(|pass| pass.references_lane(owner, lane))
    }
    pub fn folder(&self) -> Option<&Path> {
        self.capture
            .as_ref()
            .filter(|capture| capture.provisional)
            .map(|capture| capture.directory.as_path())
            .or_else(|| {
                self.provisional
                    .iter()
                    .next()
                    .and_then(|path| path.parent())
            })
    }
    pub fn stop(&mut self) {
        if let Some(capture) = &mut self.capture {
            capture.stopping = true;
        }
    }
    pub fn reset(&mut self) {
        self.cancel();
        self.provisional.clear();
        self.pending_removals.clear();
    }
    /// Drops the running take. A take whose copies are still being made is dropped as well, like
    /// one still draining its tail: its copies are deleted (by the worker, or here if they are
    /// already made), the finished WAVs stay.
    pub fn cancel(&mut self) {
        if let Some(mut capture) = self.capture.take() {
            if let Some(staging) = capture.staging.take() {
                self.cleanup
                    .retain(|ended| ended.try_recv() == Err(mpsc::TryRecvError::Empty));
                self.cleanup.push(staging.alive);
            }
            capture.session.cancel();
            for path in &capture.owned {
                self.provisional.remove(path);
            }
        }
        self.pending_midi = None;
        self.pending_automation = None;
    }
}

impl AudioCapture {
    /// Quitting: drops the running take and waits, at most `timeout`, until the staging workers
    /// of dropped takes have deleted their copies, so none stay behind in the project folder.
    pub fn shutdown(&mut self, timeout: Duration) {
        self.cancel();
        let deadline = Instant::now() + timeout;
        for ended in self.cleanup.drain(..) {
            let _ = ended.recv_timeout(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

/// Opening a project: deletes private copies that a staging worker left in its `Takes` when the
/// app ended before the worker could clean up.
pub fn reclaim_abandoned_takes(project_dir: &Path) {
    files::reclaim_abandoned(&project_dir.join("Takes"));
}

/// One non-realtime update of the take's lifecycle.
pub enum TakeProgress {
    /// No result is available yet.
    Pending,
    /// The writer acknowledged discard.
    Cancelled,
    /// The writer failed. Pending MIDI and automation can still be committed.
    Failed {
        /// Writer error.
        error: RecordingError,
        /// MIDI captured before Stop.
        midi: Option<Commit>,
        /// Automation captured before Stop.
        automation: Option<AutomationCapture>,
    },
    /// All recorded files are ready for a document edit.
    Ready(FinalizedTake),
}

/// Files and edits ready to form one undo step.
pub struct FinalizedTake {
    /// Final WAVs, using their published paths when available.
    pub takes: Vec<RecordedTake>,
    /// MIDI captured in the same take.
    pub midi: Option<Commit>,
    /// Completed automation from the same take.
    pub automation: Option<AutomationCapture>,
    /// A failed move leaves the original WAVs available.
    pub move_failed: Option<Arc<io::Error>>,
}

/// Nonfatal audio file issues to present after the save.
#[derive(Default)]
pub struct SaveWarnings {
    /// A failed copy leaves the original path in the document.
    pub move_failed: Option<Arc<io::Error>>,
    /// Checkpoint WAVs for missing tracks or unrepresentable clips.
    pub unlinked: Option<AppliedTakes>,
    /// A failed relocation leaves the live writer in its original folder.
    pub relocate_failed: Option<RecordingError>,
    /// The last failed removal, retried on the next save.
    pub remove_failed: Option<io::Error>,
}

/// Clips added and the disposition of the captured files.
#[derive(Default)]
pub struct AppliedTakes {
    /// Clip IDs in commit order, MIDI before audio.
    pub clips: Vec<ClipId>,
    /// Number of linked audio takes.
    pub linked: usize,
    /// WAVs kept for recovery without a document link.
    pub unlinked: Vec<PathBuf>,
}

/// Applies automation, MIDI and audio to the document together in one undo step.
pub fn commit_take(
    document: &mut Document,
    label: &str,
    automation: Option<&AutomationCapture>,
    midi: &[Commit],
    wavs: &[RecordedTake],
    names: &dyn TakeNames,
    directory: Option<&Path>,
) -> Result<AppliedTakes, EditError> {
    document.apply_with(label, |project| {
        let mut project = match automation {
            Some(pass) => pass.apply(project)?,
            None => project.clone(),
        };
        let mut clips = Vec::new();
        for commit in midi {
            let (next, added) = commit.apply(&project)?;
            project = next;
            clips.extend(added);
        }
        if wavs.is_empty() {
            return Ok((
                project,
                AppliedTakes {
                    clips,
                    ..AppliedTakes::default()
                },
            ));
        }
        let (project, mut audio) = apply_takes(&project, wavs, names, directory)?;
        clips.append(&mut audio.clips);
        audio.clips = clips;
        Ok((project, audio))
    })
}

/// Creates a unique folder for a provisional capture.
pub fn provisional_directory(root: &Path, name: &str, date: &str) -> io::Result<PathBuf> {
    files::directory(root, name, date)
}

impl AudioCapture {
    /// Opens the WAV writer and returns the producer to install in the engine.
    pub fn prepare(
        &mut self,
        config: RecordingConfig,
        from: Tick,
        provisional: bool,
        base: u32,
    ) -> io::Result<Box<Recorder>> {
        if self.is_active() {
            return Err(io::Error::other("an audio take is still active"));
        }
        let directory = config.directory.clone();
        let rate = config.sample_rate;
        let (recorder, session) = writer::prepare(config)?;
        self.capture = Some(Capture {
            session,
            directory,
            provisional,
            view: AudioRecording {
                start: from,
                head: from,
                ..AudioRecording::default()
            },
            stopping: false,
            base,
            rate,
            owned: BTreeSet::new(),
            material: false,
            staging: None,
        });
        Ok(recorder)
    }

    /// Whether a stopped capture still needs writer or staging finalization.
    pub fn needs_finalization(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.stopping || capture.staging.is_some())
    }

    /// Retains an unsaved render until its project is saved, including across undo and redo.
    pub fn track_provisional(&mut self, path: PathBuf) {
        self.provisional.insert(path);
    }

    /// Polls the writer or staging worker, or waits on its result until the supplied deadline.
    /// This handles file ownership and rebases existing links before returning a ready take.
    pub fn advance(
        &mut self,
        document: &mut Document,
        project_dir: Option<&Path>,
        deadline: Option<Instant>,
    ) -> TakeProgress {
        if self
            .capture
            .as_ref()
            .is_some_and(|capture| capture.staging.is_some())
        {
            return self.poll_take_staging(document, deadline);
        }
        let Some(capture) = &mut self.capture else {
            return TakeProgress::Pending;
        };
        let result = match deadline {
            Some(deadline) => capture
                .session
                .poll_timeout(deadline.saturating_duration_since(Instant::now())),
            None => capture.session.poll(),
        };
        match result {
            Some(result) => self.take_finished(document, project_dir, result),
            None => TakeProgress::Pending,
        }
    }

    /// The live view of the running take: its growing clips and finished laps.
    pub fn refresh_view(&mut self, tempo: Tempo, head: Tick, names: &dyn TakeNames) {
        let Some(capture) = self
            .capture
            .as_mut()
            .filter(|capture| capture.staging.is_none())
        else {
            return;
        };
        capture.view.head = head;
        let visible = capture.session.visible_progress();
        let track_count = visible.len().max(1) as u32;
        capture.view.clips.clear();
        capture.view.progress.clear();
        capture.view.completed.clear();
        let provisional = &mut self.provisional;
        let label = |base: u32, index: usize, lap: u32| {
            let take = names.take(
                base.saturating_add(index as u32)
                    .saturating_add(lap.saturating_mul(track_count)),
                false,
            );
            names.live_clip(&take)
        };
        for (index, snapshot) in visible.into_iter().enumerate() {
            for (slot, progress) in snapshot.completed.into_iter().enumerate() {
                if let Some(progress) = progress
                    && capture.remember_progress(&progress, provisional)
                {
                    let clip = recording_preview(
                        &progress,
                        capture.rate,
                        tempo,
                        ClipId(u64::MAX - 4096 - (2 * index + slot) as u64),
                        label(capture.base, index, progress.lap),
                    );
                    capture.view.completed.push((clip, progress));
                }
            }
            let progress = snapshot.current;
            if capture.remember_progress(&progress, provisional) {
                let clip = recording_preview(
                    &progress,
                    capture.rate,
                    tempo,
                    ClipId(u64::MAX - 1024 - index as u64),
                    label(capture.base, index, progress.lap),
                );
                capture.view.clips.push((progress.track, clip));
            }
            capture.view.progress.push(progress);
        }
    }

    /// The writer's end: commits the take, or hands its copies to a staging worker first.
    fn take_finished(
        &mut self,
        document: &mut Document,
        project_dir: Option<&Path>,
        result: Result<RecordingResult, RecordingError>,
    ) -> TakeProgress {
        let mut capture = self.capture.take().expect("polled capture exists");
        match result {
            Ok(result) => {
                // A lap that ended between two frames was never in the live view: Esc while its
                // copy is made must still treat it as this take's file.
                capture
                    .owned
                    .extend(result.takes.iter().map(|take| take.path.clone()));
                if capture.provisional {
                    self.provisional
                        .extend(result.takes.iter().map(|take| take.path.clone()));
                }
                if let Some(directory) = project_dir {
                    let directory = komponist_model::paths::absolute(directory);
                    let paths: Vec<_> = result
                        .takes
                        .iter()
                        .filter(|take| self.provisional.contains(&take.path))
                        .map(|take| take.path.clone())
                        .collect();
                    if !paths.is_empty() {
                        // Copying can take seconds: the take is committed once the worker is done.
                        let (done, alive) = stage_in_background(
                            paths,
                            directory.join("Takes"),
                            #[cfg(any(test, feature = "test-support"))]
                            self.stage_gate.take(),
                        );
                        capture.staging = Some(Staging {
                            result,
                            directory,
                            done,
                            alive,
                        });
                        self.capture = Some(capture);
                        return TakeProgress::Pending;
                    }
                }
                self.finalize_audio_take(document, result, None)
            }
            Err(RecordingError::Cancelled) => {
                self.pending_midi = None;
                self.pending_automation = None;
                TakeProgress::Cancelled
            }
            Err(error) => {
                // The writer may have finished a lap no frame saw (the blocking path draws no
                // live view): keep every file it published, without drawing waveforms.
                let provisional = &mut self.provisional;
                for snapshot in capture.session.visible_progress() {
                    for progress in snapshot.completed.iter().flatten() {
                        capture.remember_progress(progress, provisional);
                    }
                    capture.remember_progress(&snapshot.current, provisional);
                }
                // Freeze an in-progress MIDI overlay too: writer failure can happen before
                // Stop has created pending_midi. Both paths preserve captured notes once.
                if capture.provisional {
                    self.provisional
                        .extend(capture.owned.iter().filter(|path| path.is_file()).cloned());
                }
                TakeProgress::Failed {
                    error,
                    midi: self.pending_midi.take(),
                    automation: self.pending_automation.take(),
                }
            }
        }
    }

    fn poll_take_staging(
        &mut self,
        document: &mut Document,
        deadline: Option<Instant>,
    ) -> TakeProgress {
        let Some(staging) = self
            .capture
            .as_ref()
            .and_then(|capture| capture.staging.as_ref())
        else {
            return TakeProgress::Pending;
        };
        // `Err(true)`: the worker hung up without a result.
        let received = match deadline {
            Some(deadline) => staging
                .done
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .map_err(|error| error == mpsc::RecvTimeoutError::Disconnected),
            None => staging
                .done
                .try_recv()
                .map_err(|error| error == mpsc::TryRecvError::Disconnected),
        };
        let staged = match received {
            Ok(staged) => staged,
            Err(false) => return TakeProgress::Pending,
            Err(true) => Err(std::io::Error::other("recording copy worker stopped")),
        };
        let mut capture = self.capture.take().expect("staging capture exists");
        let staging = capture.staging.take().expect("staging exists");
        self.finalize_audio_take(document, staging.result, Some((staging.directory, staged)))
    }

    fn finalize_audio_take(
        &mut self,
        document: &mut Document,
        mut result: RecordingResult,
        staged: Option<(PathBuf, StageResult)>,
    ) -> TakeProgress {
        let mut move_failed = None;
        if let Some((directory, staged)) = staged {
            let takes = directory.join("Takes");
            match staged.and_then(|copies| copies.publish(&takes)) {
                Ok(mapping) => {
                    for take in &mut result.takes {
                        if let Some(path) = mapping.get(&take.path) {
                            self.provisional.remove(&take.path);
                            self.provisional.insert(path.clone());
                            take.path = path.clone();
                        }
                    }
                    // Keep source files until the result is in the document; old project
                    // checkpoints and undo states may still link to those files.
                    self.pending_removals.extend(mapping.keys().cloned());
                    if !mapping.is_empty() {
                        let canonical = files::canonical_copies(&mapping);
                        let destination = |source: PathBuf| {
                            mapping
                                .get(&source)
                                .or_else(|| canonical.get(&std::fs::canonicalize(&source).ok()?))
                                .cloned()
                                .unwrap_or(source)
                        };
                        document.rewrite_untracked(|project| {
                            crate::project_assets::rebase_project_links(
                                project,
                                Some(&directory),
                                &directory,
                                destination,
                                destination,
                            )
                        });
                    }
                }
                // The take links its original WAVs; the error stays the last word below.
                Err(error) => {
                    let error = Arc::new(error);
                    let sources = result
                        .takes
                        .iter()
                        .map(|take| take.path.clone())
                        .filter(|path| self.provisional.contains(path))
                        .collect();
                    self.failed_stage = Some(FailedStage {
                        target: takes,
                        sources,
                        error: Arc::clone(&error),
                    });
                    move_failed = Some(error);
                }
            }
        }
        TakeProgress::Ready(FinalizedTake {
            takes: result.takes,
            midi: self.pending_midi.take(),
            automation: self.pending_automation.take(),
            move_failed,
        })
    }

    /// Copies owned WAVs and takes an independent checkpoint while the writer stays in place.
    /// Only a successful project write may adopt these paths and relocate the live writer.
    pub fn stage_for_save(
        &mut self,
        project_dir: &Path,
    ) -> Result<StagedRecordings, RecordingError> {
        let target = project_dir.join("Takes");
        // Private copies of an app that ended during a background copy.
        files::reclaim_abandoned(&target);
        // Sources whose background copy into this folder just failed keep their original links in
        // this Save; the next Save tries again.
        let failed = self
            .failed_stage
            .take()
            .filter(|failed| failed.target == target);
        let mut staged = StagedRecordings {
            target,
            mapping: BTreeMap::new(),
            copied_files: BTreeMap::new(),
            live: BTreeSet::new(),
            live_files: BTreeSet::new(),
            keep_live_paths: Cell::new(false),
            checkpoints: Vec::new(),
            warnings: SaveWarnings::default(),
        };
        if let Some(capture) = &self.capture {
            let takes = capture.session.checkpoint()?;
            if !takes.is_empty() {
                staged.checkpoints = files::checkpoint(&takes, &staged.target)
                    .map_err(|error| RecordingError::Io(error.to_string()))?;
                staged.live.extend(takes.into_iter().map(|take| take.path));
                staged.live_files.extend(
                    staged
                        .live
                        .iter()
                        .filter_map(|path| std::fs::canonicalize(path).ok()),
                );
            }
        }
        let paths: Vec<_> = self
            .provisional
            .iter()
            .filter(|path| !staged.live.contains(*path))
            .filter(|path| !failed.as_ref().is_some_and(|f| f.sources.contains(*path)))
            .cloned()
            .collect();
        if let Some(failed) = failed {
            staged.warnings.move_failed = Some(failed.error);
        }
        if !paths.is_empty() {
            match files::stage(&paths, &staged.target) {
                Ok(mapping) => {
                    staged.copied_files = files::canonical_copies(&mapping);
                    staged.mapping = mapping;
                }
                Err(error) => {
                    staged.warnings.move_failed = Some(Arc::new(error));
                }
            }
        }
        Ok(staged)
    }

    /// Publishes the staged paths after the new project file is written. Older on-disk
    /// checkpoints and live writer paths remain usable until then.
    pub fn saved(&mut self, mut staged: StagedRecordings) -> SaveWarnings {
        staged.checkpoints.clear();
        let mapping = std::mem::take(&mut staged.mapping);
        let mut warnings = std::mem::take(&mut staged.warnings);
        for source in mapping.into_keys() {
            self.provisional.remove(&source);
            self.pending_removals.insert(source);
        }
        if let Some(capture) = &mut self.capture
            && capture.provisional
            && !staged.keep_live_paths.get()
            && !capture.directory.starts_with(&staged.target)
        {
            match capture.session.relocate(&staged.target) {
                Ok(after) => {
                    for path in &staged.live {
                        self.provisional.remove(path);
                        capture.owned.remove(path);
                    }
                    // The worker can finish more laps while the project is being written.
                    capture
                        .owned
                        .extend(after.into_iter().map(|take| take.path));
                    capture.directory = staged.target.clone();
                }
                Err(error) => {
                    warnings.relocate_failed = Some(error);
                }
            }
        }
        self.provisional
            .retain(|path| !path.starts_with(&staged.target));
        if let Some(capture) = &mut self.capture
            && capture.directory.starts_with(&staged.target)
        {
            capture.provisional = false;
        }
        let paths = std::mem::take(&mut self.pending_removals);
        for path in paths {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                self.pending_removals.insert(path);
                warnings.remove_failed = Some(error);
            }
        }
        warnings
    }
}

/// Assets created by this save attempt. A failed write leaves the live capture, document and
/// original recordings in place, and drops only the unreferenced copies made by this attempt.
pub struct StagedRecordings {
    target: PathBuf,
    mapping: BTreeMap<PathBuf, PathBuf>,
    copied_files: BTreeMap<PathBuf, PathBuf>,
    live: BTreeSet<PathBuf>,
    live_files: BTreeSet<PathBuf>,
    keep_live_paths: Cell<bool>,
    checkpoints: Vec<RecordedTake>,
    warnings: SaveWarnings,
}

impl StagedRecordings {
    /// Selects the saved path for a resolved link. A live WAV already linked in the document
    /// or its history must keep its name until the capture stops and its writer closes.
    pub fn destination(&self, source: PathBuf) -> PathBuf {
        if let Some(copy) = self.mapping.get(&source) {
            return copy.clone();
        }
        if self.live.contains(&source) {
            self.keep_live_paths.set(true);
        } else if !(self.live_files.is_empty() && self.copied_files.is_empty())
            && let Ok(canonical) = std::fs::canonicalize(&source)
        {
            if self.live_files.contains(&canonical) {
                self.keep_live_paths.set(true);
            }
            if let Some(copy) = self.copied_files.get(&canonical) {
                return copy.clone();
            }
        }
        source
    }

    /// Applies the checkpoint WAVs to an independent project snapshot.
    pub fn project_with_audio_take(
        &mut self,
        project: &Project,
        names: &dyn TakeNames,
    ) -> Result<Option<Project>, EditError> {
        if self.checkpoints.is_empty() {
            return Ok(None);
        }
        let (project, applied) =
            apply_takes(project, &self.checkpoints, names, self.target.parent())?;
        let has_clips = !applied.clips.is_empty();
        if !applied.unlinked.is_empty() {
            self.warnings.unlinked = Some(applied);
        }
        Ok(has_clips.then_some(project))
    }
}

impl Drop for StagedRecordings {
    fn drop(&mut self) {
        for path in self
            .mapping
            .values()
            .chain(self.checkpoints.iter().map(|take| &take.path))
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Projects finalized WAVs into clips and reports any files that could not be linked.
pub fn apply_takes(
    project: &Project,
    takes: &[RecordedTake],
    names: &dyn TakeNames,
    directory: Option<&Path>,
) -> Result<(Project, AppliedTakes), EditError> {
    // The linear lead-in and the cycle have different clip geometry.
    let mut grouped = BTreeMap::<_, Vec<&RecordedTake>>::new();
    for take in takes.iter().filter(|take| take.frames > 0) {
        grouped
            .entry((take.track, take.start, take.window_frames, take.window_end))
            .or_default()
            .push(take);
    }
    let mut project = project.clone();
    let mut applied = AppliedTakes::default();
    for ((track, _, _, window_end), mut takes) in grouped {
        if project
            .track(track)
            .is_none_or(|track| track.kind != TrackKind::Audio)
        {
            applied
                .unlinked
                .extend(takes.iter().map(|take| take.path.clone()));
            continue;
        }
        takes.sort_by_key(|take| take.lap);
        let first = takes[0];
        let source = |take: &RecordedTake| {
            AudioFile::new(
                &take.path,
                directory,
                take.window_frames.unwrap_or(take.frames),
                take.sample_rate,
                take.channels,
            )
        };
        let audio = AudioClip::new(source(first), 0, project.tempo);
        let max_length = audio.max_length(project.tempo);
        let length = window_end.map_or(max_length, |end| (end - first.start).min(max_length));
        if length == Tick::ZERO {
            // A sub-tick span has no clip geometry. Its WAV is already saved for recovery,
            // so continue committing the representable passes on this and the other tracks.
            applied
                .unlinked
                .extend(takes.iter().map(|take| take.path.clone()));
            continue;
        }
        let name = names.take(edits::next_take_number(&project), false);
        let (next, id) = edits::add_clip(
            &project,
            track,
            NewClip {
                start: first.start,
                length,
                name: name.clone(),
                content: NewClipContent::Audio {
                    file: source(first),
                    offset: 0,
                },
            },
        )?;
        project = next;
        let track_index = project
            .tracks
            .iter()
            .position(|candidate| candidate.id == track)
            .expect("new clip track");
        let clip_index = project.tracks[track_index]
            .clips
            .iter()
            .position(|clip| clip.id == id)
            .expect("new clip");
        if let ClipContent::Audio(audio) = &mut Arc::make_mut(
            &mut Arc::make_mut(&mut project.tracks[track_index]).clips[clip_index],
        )
        .content
        {
            audio.takes[0].name = name;
        }
        applied.linked += takes.len();
        for take in takes.into_iter().skip(1) {
            let name = names.take(edits::next_take_number(&project), false);
            project = edits::add_audio_take(
                &project,
                id,
                AudioTake {
                    name: name.clone(),
                    file: source(take),
                    offset: 0,
                },
            )?
            .0;
            project = edits::set_clip_prop(&project, id, edits::ClipProp::Name(name))?;
        }
        applied.clips.push(id);
    }
    Ok((project, applied))
}

#[cfg(feature = "test-support")]
#[doc(hidden)]
impl AudioCapture {
    pub fn hold_staging_for_test(
        &mut self,
        go: mpsc::Receiver<()>,
        finished: mpsc::Sender<()>,
        ready: Option<mpsc::Sender<()>>,
    ) {
        self.stage_gate = Some(StageGate {
            go,
            finished,
            ready,
        });
    }

    pub fn staging_paths_for_test(&self) -> Option<impl Iterator<Item = &Path>> {
        let staging = self.capture.as_ref()?.staging.as_ref()?;
        Some(staging.result.takes.iter().map(|take| take.path.as_path()))
    }

    pub fn provisional_for_test(&self) -> &BTreeSet<PathBuf> {
        &self.provisional
    }

    pub fn pending_removals_for_test(&self) -> &BTreeSet<PathBuf> {
        &self.pending_removals
    }

    pub fn writer_progress_for_test(&self) -> Vec<writer::TrackRecordingProgress> {
        self.capture.as_ref().unwrap().session.visible_progress()
    }

    pub fn knows_path_for_test(&self, path: &Path) -> bool {
        self.capture.as_ref().unwrap().owned.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_engine::EngineEvent;
    use komponist_model::automation::{AutomationMode, AutomationOwner, AutomationTarget};

    #[derive(Debug)]
    struct Names;

    impl TakeNames for Names {
        fn take(&self, number: u32, _: bool) -> String {
            format!("Take {number}")
        }

        fn live_clip(&self, take: &str) -> String {
            take.to_owned()
        }
    }

    #[test]
    fn a_combined_take_inserts_midi_audio_and_automation_in_one_undo_step() {
        let (project, midi_track) = edits::add_track(
            &Project::new("Capture"),
            edits::NewTrack {
                name: None,
                hue: None,
                instrument: None,
                index: None,
                arm_exclusive: true,
            },
        )
        .unwrap();
        let (project, audio_track) =
            edits::add_audio_track(&project, edits::NewAudioTrack::default()).unwrap();
        let owner = AutomationOwner::Master;
        let target = AutomationTarget::Pan;
        let (project, lane) =
            edits::automation::add_lane(&project, owner, target.clone(), "Pan".into()).unwrap();
        let project =
            edits::automation::set_lane_mode(&project, owner, lane, AutomationMode::Latch).unwrap();
        let mut document = Document::new(project);
        let before = document.project().clone();

        let mut midi = super::super::midi::Recording::new(&before, Tick::ZERO, Names);
        midi.handle(
            &EngineEvent::RecordedNoteOn {
                track: midi_track,
                tick: Tick::ZERO,
                lap: 0,
                key: 60,
                velocity: 100,
            },
            &before,
        );
        let midi = midi.commit(&before, Tick(960)).unwrap();
        let mut automation = AutomationCapture::default();
        automation.open(&before, owner, target.clone(), false, 0.5);
        automation.value(&before, owner, &target, Tick(10), 0.75);
        automation.end_segments(&before, Tick(960));
        let temp = tempfile::tempdir().unwrap();
        let wav = RecordedTake {
            track: audio_track,
            path: temp.path().join("take.wav"),
            frames: 24_000,
            window_frames: None,
            window_end: None,
            sample_rate: 48_000,
            channels: 1,
            start: Tick::ZERO,
            lap: 0,
        };
        let applied = commit_take(
            &mut document,
            "Take 1",
            Some(&automation),
            &[midi],
            &[wav],
            &Names,
            None,
        )
        .unwrap();
        assert_eq!(applied.clips.len(), 2);
        assert_eq!(applied.linked, 1);
        let points = &edits::automation::lanes(document.project(), owner)
            .unwrap()
            .lane(&target)
            .unwrap()
            .points;
        assert_eq!(
            komponist_model::automation::value_at(points.as_slice(), Tick(10)),
            Some(0.75)
        );
        assert!(document.undo());
        assert_eq!(document.project(), &before);
        assert!(!document.undo());
        assert!(document.redo());
        assert_eq!(
            document.project().track(midi_track).unwrap().clips[0].id,
            applied.clips[0]
        );
        assert_eq!(
            document.project().track(audio_track).unwrap().clips[0].id,
            applied.clips[1]
        );
    }
}
