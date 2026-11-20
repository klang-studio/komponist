//! Audio recording lifecycle on the UI thread. The engine owns bounded capture queues, the
//! session owns file IO; growing clips are overlays until finalized takes form one undo step.

mod files;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use komponist_engine::Command;
use komponist_engine::recording::{
    self, RecordedTake, RecordingConfig, RecordingFormat, RecordingProgress, RecordingSession,
    RecordingTrack,
};
use komponist_model::edits::{self, EditError, NewClip, NewClipContent};
use komponist_model::{
    AudioClip, AudioFile, AudioInput, AudioTake, Clip, ClipContent, ClipId, Project, Tempo, Tick,
    TrackId, TrackKind,
};

use crate::actions::{Action, UiAction};
use crate::app::App;
use crate::record;
use crate::settings::CaptureFormat;
use crate::ui_state::StatusKind;

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
    #[cfg(test)]
    pub(crate) fn preview(
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

/// Session-owned files survive undo. Only an explicit discard deletes an active session.
#[derive(Default)]
pub(crate) struct State {
    capture: Option<Capture>,
    provisional: BTreeSet<PathBuf>,
    root_override: Option<PathBuf>,
    pending_midi: Option<record::Commit>,
    pending_automation: Option<crate::automation_capture::AutomationCapture>,
    pending_removals: BTreeSet<PathBuf>,
}

impl State {
    pub(crate) fn running(&self) -> Option<&AudioRecording> {
        self.capture.as_ref().map(|capture| &capture.view)
    }
    pub(crate) fn is_active(&self) -> bool {
        self.capture.is_some()
    }
    pub(crate) fn has_material(&self) -> bool {
        self.capture
            .as_ref()
            .is_some_and(|capture| capture.material || capture.stopping)
            || self.pending_midi.is_some()
            || self.pending_automation.is_some()
    }
    pub(crate) fn defer_midi(&mut self, commit: Option<record::Commit>) {
        if commit.is_some() {
            self.pending_midi = commit;
        }
    }
    pub(crate) fn defer_automation(&mut self, pass: crate::automation_capture::AutomationCapture) {
        if pass.has_points() {
            if let Some(previous) = &mut self.pending_automation {
                previous.merge(pass);
            } else {
                self.pending_automation = Some(pass);
            }
        }
    }

    pub(crate) fn pending_automation_published(&self) -> bool {
        self.pending_automation
            .as_ref()
            .is_some_and(|pass| pass.was_published())
    }

    pub(crate) fn references_automation_lane(
        &self,
        owner: komponist_model::automation::AutomationOwner,
        lane: komponist_model::automation::LaneId,
    ) -> bool {
        self.pending_automation
            .as_ref()
            .is_some_and(|pass| pass.references_lane(owner, lane))
    }
    pub(crate) fn folder(&self) -> Option<&Path> {
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
    pub(crate) fn stop(&mut self) {
        if let Some(capture) = &mut self.capture {
            capture.stopping = true;
        }
    }
    pub(crate) fn reset(&mut self) {
        self.cancel();
        self.provisional.clear();
        self.pending_removals.clear();
    }
    pub(crate) fn cancel(&mut self) {
        if let Some(capture) = self.capture.take() {
            capture.session.cancel();
            for path in &capture.owned {
                self.provisional.remove(path);
            }
        }
        self.pending_midi = None;
        self.pending_automation = None;
    }
}

impl App {
    /// Overrides the provisional recording root for embedding/tests. No directory is created
    /// until recording starts. Headless construction always uses scratch space.
    pub fn with_recording_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.audio_recording.root_override = Some(directory.into());
        self
    }
    /// Folder shown by the persistent provisional-recordings chip, while any owned file still
    /// lives outside the saved project's Takes folder.
    /// Current audio capture, including a count-in or a draining stop tail.
    pub fn audio_recording(&self) -> Option<&AudioRecording> {
        self.audio_recording.running()
    }

    pub fn provisional_recordings_folder(&self) -> Option<&Path> {
        self.audio_recording.folder()
    }

    /// A destination for generated audio, with the same first-save migration as a take.
    pub(crate) fn generated_audio_folder(&self) -> std::io::Result<(PathBuf, bool)> {
        if let Some(directory) = self.project_dir() {
            let target = directory.join("Takes");
            std::fs::create_dir_all(&target)?;
            return Ok((target, false));
        }
        let root = self
            .audio_recording
            .root_override
            .clone()
            .or_else(files::provisional_root)
            .ok_or_else(|| std::io::Error::other(self.lang.audio_record_folder_unavailable()))?;
        std::fs::create_dir_all(&root)?;
        Ok((root, true))
    }

    /// Owns an unsaved render until the project is saved, including across undo and redo.
    pub(crate) fn track_provisional_audio(&mut self, path: PathBuf) {
        self.audio_recording.provisional.insert(path);
    }

    pub(crate) fn prepare_audio_take(&mut self, from: Tick) -> bool {
        if self.audio_recording.capture.is_some() {
            self.set_status(self.lang.audio_record_finalizing(), StatusKind::Info);
            return false;
        }
        let armed: Vec<_> = self
            .project()
            .tracks
            .iter()
            .filter(|track| track.arm && track.kind == TrackKind::Audio)
            .map(|track| track.id)
            .collect();
        if armed.is_empty() {
            return true;
        }
        if armed.iter().any(|id| !self.audio_arm_allowed(*id)) {
            return false;
        }
        let provisional = self.project_path.is_none();
        let directory = if let Some(directory) = self.project_dir() {
            directory.join("Takes")
        } else {
            let root = self
                .audio_recording
                .root_override
                .clone()
                .or_else(files::provisional_root);
            let Some(root) = root else {
                self.set_status(
                    self.lang.audio_record_folder_unavailable(),
                    StatusKind::Error,
                );
                return false;
            };
            match files::directory(&root, &self.project().name) {
                Ok(directory) => directory,
                Err(error) => {
                    self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
                    return false;
                }
            }
        };
        let directory = komponist_model::paths::absolute(&directory);
        let tracks = armed
            .into_iter()
            .filter_map(|id| self.project().track(id))
            .map(|track| RecordingTrack {
                track: track.id,
                name: track.name.clone(),
                input: track.audio.input,
                channels: match track.audio.input {
                    AudioInput::Hardware(input) => input.channel_count(),
                    _ => 2,
                },
            })
            .collect();
        let rate = self.engine.sample_rate();
        let format = match self.settings.capture_format {
            CaptureFormat::Pcm24 => RecordingFormat::Pcm24,
            CaptureFormat::Float32 => RecordingFormat::Float32,
        };
        match recording::prepare(RecordingConfig {
            directory: directory.clone(),
            sample_rate: rate,
            format,
            tracks,
        }) {
            Ok((recorder, session)) => {
                self.audio_recording.capture = Some(Capture {
                    session,
                    directory,
                    provisional,
                    view: AudioRecording {
                        start: from,
                        head: from,
                        ..Default::default()
                    },
                    stopping: false,
                    base: edits::next_take_number(self.project()),
                    rate,
                    owned: BTreeSet::new(),
                    material: false,
                });
                self.engine.send(Command::SetRecorder(recorder));
                true
            }
            Err(error) => {
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
                false
            }
        }
    }

    pub(crate) fn update_audio_take(&mut self) {
        let project = self.project().clone();
        let head = self.engine.playhead();
        let Some(capture) = &mut self.audio_recording.capture else {
            return;
        };
        capture.view.head = head;
        let visible = capture.session.visible_progress();
        let track_count = visible.len().max(1) as u32;
        capture.view.clips.clear();
        capture.view.progress.clear();
        capture.view.completed.clear();
        let provisional = &mut self.audio_recording.provisional;
        let label = |base: u32, index: usize, lap: u32| {
            let take = self.lang.record_take(
                base.saturating_add(index as u32)
                    .saturating_add(lap.saturating_mul(track_count)),
            );
            self.lang.record_clip(take)
        };
        for (index, snapshot) in visible.into_iter().enumerate() {
            for (slot, progress) in snapshot.completed.into_iter().enumerate() {
                if let Some(progress) = progress
                    && capture.remember_progress(&progress, provisional)
                {
                    let clip = recording_preview(
                        &progress,
                        capture.rate,
                        project.tempo,
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
                    project.tempo,
                    ClipId(u64::MAX - 1024 - index as u64),
                    label(capture.base, index, progress.lap),
                );
                capture.view.clips.push((progress.track, clip));
            }
            capture.view.progress.push(progress);
        }
        let Some(result) = capture.session.poll() else {
            return;
        };
        let capture = self
            .audio_recording
            .capture
            .take()
            .expect("polled capture exists");
        match result {
            Ok(mut result) => {
                if capture.provisional {
                    self.audio_recording
                        .provisional
                        .extend(result.takes.iter().map(|take| take.path.clone()));
                }
                if let Some(directory) = self.project_dir() {
                    let directory = komponist_model::paths::absolute(&directory);
                    let paths: Vec<_> = result
                        .takes
                        .iter()
                        .filter(|take| self.audio_recording.provisional.contains(&take.path))
                        .map(|take| take.path.clone())
                        .collect();
                    match files::stage(&paths, &directory.join("Takes")) {
                        Ok(mapping) => {
                            for take in &mut result.takes {
                                if let Some(path) = mapping.get(&take.path) {
                                    self.audio_recording.provisional.remove(&take.path);
                                    self.audio_recording.provisional.insert(path.clone());
                                    take.path = path.clone();
                                }
                            }
                            // Keep source files until the result is in the document; old project
                            // checkpoints and undo states may still link to those files.
                            self.audio_recording
                                .pending_removals
                                .extend(mapping.keys().cloned());
                            if !mapping.is_empty() {
                                let canonical = files::canonical_copies(&mapping);
                                self.doc.rewrite_untracked(|project| {
                                    crate::samples::rebase_project_links(
                                        project,
                                        Some(&directory),
                                        &directory,
                                        |source| {
                                            mapping
                                                .get(&source)
                                                .or_else(|| {
                                                    canonical
                                                        .get(&std::fs::canonicalize(&source).ok()?)
                                                })
                                                .cloned()
                                                .unwrap_or(source)
                                        },
                                    )
                                });
                            }
                        }
                        Err(error) => self.set_status(
                            self.lang.audio_record_move_failed(error),
                            StatusKind::Error,
                        ),
                    }
                }
                let midi = self.audio_recording.pending_midi.take();
                let automation = self.audio_recording.pending_automation.take();
                let label = midi
                    .as_ref()
                    .map(|commit| commit.label().to_owned())
                    .unwrap_or_else(|| {
                        self.lang
                            .record_take(edits::next_take_number(self.project()))
                    });
                let lang = self.lang;
                let directory = self.project_dir();
                self.doc.end_gesture();
                let applied = self.apply_with(&label, |project| {
                    let automated;
                    let project = if let Some(pass) = &automation {
                        automated = pass.apply(project)?;
                        &automated
                    } else {
                        project
                    };
                    let (project, mut clips) = match &midi {
                        Some(commit) => commit.apply(project)?,
                        None => (project.clone(), Vec::new()),
                    };
                    let (project, mut audio) =
                        apply_takes(&project, &result.takes, lang, directory.as_deref())?;
                    clips.append(&mut audio.clips);
                    audio.clips = clips;
                    Ok((project, audio))
                });
                if let Some(applied) = applied {
                    if let Some(first) = applied.clips.first() {
                        self.dispatch(Action::Ui(UiAction::OpenClip(*first)));
                    }
                    if let Some(notice) = applied.unlinked_notice(self.lang) {
                        self.set_status(notice, StatusKind::Info);
                    } else if applied.linked > 0 {
                        self.set_status(
                            self.lang.audio_record_complete(applied.linked),
                            StatusKind::Success,
                        );
                    }
                } else {
                    self.set_status(self.lang.audio_record_apply_failed(), StatusKind::Error);
                }
            }
            Err(recording::RecordingError::Cancelled) => {
                self.audio_recording.pending_midi = None;
                if self.audio_recording.pending_automation_published() {
                    self.engine.send_project(self.doc.project());
                }
                self.audio_recording.pending_automation = None;
            }
            Err(error) => {
                // Freeze an in-progress MIDI overlay too: writer failure can happen before
                // Stop has created pending_midi. Both paths preserve captured notes once.
                if capture.provisional {
                    self.audio_recording
                        .provisional
                        .extend(capture.owned.iter().filter(|path| path.is_file()).cloned());
                }
                let mut commits = Vec::new();
                if let Some(midi) = self.audio_recording.pending_midi.take() {
                    commits.push(midi);
                }
                if let Some(midi) = self.recording.take().and_then(|recording| {
                    recording.commit(self.doc.project(), self.engine.playhead())
                }) {
                    commits.push(midi);
                }
                let automation = self.audio_recording.pending_automation.take();
                if !commits.is_empty() || automation.is_some() {
                    let label = commits
                        .first()
                        .map_or("undo-automation-record", |first| first.label())
                        .to_owned();
                    self.apply_with(&label, |project| {
                        let mut project = match &automation {
                            Some(pass) => pass.apply(project)?,
                            None => project.clone(),
                        };
                        let mut clips = Vec::new();
                        for commit in &commits {
                            let (next, added) = commit.apply(&project)?;
                            project = next;
                            clips.extend(added);
                        }
                        Ok((project, clips))
                    });
                }
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
                self.engine.send(Command::StopRecording);
            }
        }
        drop(capture); // joins/IO only on this non-RT thread
    }

    /// A stopped capture drains its latency tail before a Save-and-leave action can leave the
    /// project. The audio thread keeps running; a timeout leaves the project and files open.
    pub(crate) fn await_audio_take(&mut self) -> bool {
        if !self
            .audio_recording
            .capture
            .as_ref()
            .is_some_and(|capture| capture.stopping)
        {
            return true;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while self.audio_recording.capture.is_some() && std::time::Instant::now() < deadline {
            self.update_audio_take();
            if self.audio_recording.capture.is_some() {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        if self.audio_recording.capture.is_some() {
            self.set_status(self.lang.audio_record_finalizing(), StatusKind::Info);
            false
        } else {
            true
        }
    }

    /// Copies owned WAVs and takes an independent checkpoint while the writer stays in place.
    /// Only a successful project write may adopt these paths and relocate the live writer.
    pub(crate) fn stage_recordings_for_save(
        &mut self,
        project_dir: &Path,
    ) -> Result<StagedRecordings, ()> {
        let target = project_dir.join("Takes");
        let mut staged = StagedRecordings {
            target,
            mapping: BTreeMap::new(),
            copied_files: BTreeMap::new(),
            live: BTreeSet::new(),
            live_files: BTreeSet::new(),
            keep_live_paths: Cell::new(false),
            checkpoints: Vec::new(),
            notice: None,
        };
        if let Some(capture) = &self.audio_recording.capture {
            let takes = capture.session.checkpoint().map_err(|error| {
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
            })?;
            if !takes.is_empty() {
                staged.checkpoints =
                    files::checkpoint(&takes, &staged.target).map_err(|error| {
                        self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
                    })?;
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
            .audio_recording
            .provisional
            .iter()
            .filter(|path| !staged.live.contains(*path))
            .cloned()
            .collect();
        if !paths.is_empty() {
            match files::stage(&paths, &staged.target) {
                Ok(mapping) => {
                    staged.copied_files = files::canonical_copies(&mapping);
                    staged.mapping = mapping;
                }
                Err(error) => {
                    staged.notice =
                        Some((self.lang.audio_record_move_failed(error), StatusKind::Error));
                }
            }
        }
        Ok(staged)
    }

    /// Publishes the staged paths after the new project file is written. Older on-disk
    /// checkpoints and live writer paths remain usable until then.
    pub(crate) fn recordings_saved(&mut self, mut staged: StagedRecordings) {
        staged.checkpoints.clear();
        let mapping = std::mem::take(&mut staged.mapping);
        let mut notice = staged.notice.take();
        for source in mapping.into_keys() {
            self.audio_recording.provisional.remove(&source);
            self.audio_recording.pending_removals.insert(source);
        }
        if let Some(capture) = &mut self.audio_recording.capture
            && capture.provisional
            && !staged.keep_live_paths.get()
            && !capture.directory.starts_with(&staged.target)
        {
            match capture.session.relocate(&staged.target) {
                Ok(after) => {
                    for path in &staged.live {
                        self.audio_recording.provisional.remove(path);
                        capture.owned.remove(path);
                    }
                    // The worker can finish more laps while the project is being written.
                    capture
                        .owned
                        .extend(after.into_iter().map(|take| take.path));
                    capture.directory = staged.target.clone();
                }
                Err(error) => {
                    notice = Some((self.lang.audio_record_move_failed(error), StatusKind::Error));
                }
            }
        }
        self.audio_recording
            .provisional
            .retain(|path| !path.starts_with(&staged.target));
        if let Some(capture) = &mut self.audio_recording.capture
            && capture.directory.starts_with(&staged.target)
        {
            capture.provisional = false;
        }
        if let Some((notice, kind)) = notice {
            self.set_status(notice, kind);
        }
        let paths = std::mem::take(&mut self.audio_recording.pending_removals);
        for path in paths {
            if let Err(error) = std::fs::remove_file(&path)
                && error.kind() != std::io::ErrorKind::NotFound
            {
                self.audio_recording.pending_removals.insert(path);
                self.set_status(self.lang.audio_record_move_failed(error), StatusKind::Error);
            }
        }
    }
}

/// Assets created by this save attempt. A failed write leaves the live capture, document and
/// original recordings in place, and drops only the unreferenced copies made by this attempt.
pub(crate) struct StagedRecordings {
    target: PathBuf,
    mapping: BTreeMap<PathBuf, PathBuf>,
    copied_files: BTreeMap<PathBuf, PathBuf>,
    live: BTreeSet<PathBuf>,
    live_files: BTreeSet<PathBuf>,
    keep_live_paths: Cell<bool>,
    checkpoints: Vec<RecordedTake>,
    notice: Option<(String, StatusKind)>,
}

impl StagedRecordings {
    /// Selects the saved path for a resolved link. A live WAV already linked in the document
    /// or its history must keep its name until the capture stops and its writer closes.
    pub(crate) fn destination(&self, source: PathBuf) -> PathBuf {
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

    pub(crate) fn project_with_audio_take(
        &mut self,
        project: &Project,
        lang: komponist_i18n::Lang,
    ) -> Result<Option<Project>, EditError> {
        if self.checkpoints.is_empty() {
            return Ok(None);
        }
        let (project, applied) =
            apply_takes(project, &self.checkpoints, lang, self.target.parent())?;
        if let Some(notice) = applied.unlinked_notice(lang) {
            if let Some((existing, _)) = &mut self.notice {
                existing.push_str(" · ");
                existing.push_str(&notice);
            } else {
                self.notice = Some((notice, StatusKind::Info));
            }
        }
        Ok((!applied.clips.is_empty()).then_some(project))
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

#[derive(Default)]
struct AppliedTakes {
    clips: Vec<ClipId>,
    linked: usize,
    unlinked: Vec<PathBuf>,
}

impl AppliedTakes {
    fn unlinked_notice(&self, lang: komponist_i18n::Lang) -> Option<String> {
        if self.unlinked.is_empty() {
            return None;
        }
        let paths = self
            .unlinked
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(" · ");
        Some(lang.audio_record_unlinked(self.linked, paths))
    }
}

fn apply_takes(
    project: &Project,
    takes: &[RecordedTake],
    lang: komponist_i18n::Lang,
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
        let name = lang.record_take(edits::next_take_number(&project));
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
            let name = lang.record_take(edits::next_take_number(&project));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{EditAction, TransportAction};
    use std::time::{Duration, Instant};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            loop {
                let path = std::env::temp_dir().join(format!(
                    "komponist-audio-lifecycle-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self(path),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(error) => panic!("could not create recording fixture: {error}"),
                }
            }
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = Path;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn rig() -> (Scratch, App, TrackId) {
        let root = Scratch::new();
        let (app, track) = rig_at(&root.0);
        (root, app, track)
    }

    fn rig_at(root: &Path) -> (App, TrackId) {
        let mut project = Project::new("Captured audio");
        project.count_in_bars = 0;
        project.loop_on = false;
        let (project, bus) = edits::add_bus(&project, edits::NewBus::default()).unwrap();
        let (mut project, track) = edits::add_audio_track(
            &project,
            edits::NewAudioTrack {
                input: AudioInput::Bus(bus),
                ..Default::default()
            },
        )
        .unwrap();
        Arc::make_mut(
            project
                .tracks
                .iter_mut()
                .find(|candidate| candidate.id == track)
                .unwrap(),
        )
        .arm = true;
        let mut app = App::headless(project).with_recording_directory(root.join("Provisional"));
        app.engine_mut().hold_engine().unwrap();
        app.step();
        (app, track)
    }
    fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.step();
            if done(app) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "audio lifecycle did not advance; status={:?}",
                app.ui.status
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn begin(app: &mut App, track: TrackId) {
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(1024, &[], 0));
        wait(app, |app| {
            app.audio_recording()
                .and_then(|view| view.progress(track))
                .is_some_and(|progress| progress.frames > 0)
        });
    }
    #[test]
    fn arrangement_keeps_the_lead_in_and_last_loop_overlay_while_the_next_take_grows() {
        let scratch = Scratch::new();
        let (mut app, track) = rig_at(&scratch.0);
        let lo = Tick(32);
        let hi = Tick(64);
        let range = komponist_model::TickRange::new(lo, hi);
        app.doc
            .apply("Set loop", |project| {
                let project =
                    edits::set_project_prop(project, edits::ProjectProp::LoopRange(range))?;
                edits::set_project_prop(&project, edits::ProjectProp::LoopOn(true))
            })
            .unwrap();
        app.step();

        let map = app.project().tempo_map();
        let rate = f64::from(app.engine_mut().sample_rate());
        let lead = map.tick_to_sample(lo, rate) as usize;
        let end = map.tick_to_sample(hi, rate) as usize;
        let period = 64;
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(lead + period, &[], 0));
        wait(&mut app, |app| {
            app.audio_recording()
                .and_then(|view| view.progress(track))
                .is_some_and(|progress| progress.lap == 1)
        });
        let view = app.audio_recording().unwrap();
        let starts: Vec<_> = view
            .overlays(track)
            .map(|(clip, progress)| (clip.start, progress.lap))
            .collect();
        assert_eq!(starts, [(Tick::ZERO, 0), (lo, 1)]);

        assert!(app.engine_mut().run_held(end - lead, &[], 0));
        wait(&mut app, |app| {
            app.audio_recording()
                .and_then(|view| view.progress(track))
                .is_some_and(|progress| progress.lap == 2)
        });
        let view = app.audio_recording().unwrap();
        let overlays: Vec<_> = view.overlays(track).collect();
        assert_eq!(overlays.len(), 3);
        assert_eq!(
            (overlays[0].0.start, overlays[0].0.end(), overlays[0].1.lap),
            (Tick::ZERO, lo, 0)
        );
        assert_eq!(
            (overlays[1].0.start, overlays[1].0.end(), overlays[1].1.lap),
            (lo, hi, 1)
        );
        assert_eq!((overlays[2].0.start, overlays[2].1.lap), (lo, 2));
        assert_eq!(view.clip(track), Some(overlays[2].0));
        assert_eq!(view.first(), Some((track, overlays[2].0)));

        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(period, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        let clips = &app.project().track(track).unwrap().clips;
        assert_eq!(clips.len(), 2);
        let ClipContent::Audio(cycle) = &clips[1].content else {
            panic!("loop audio clip");
        };
        assert_eq!(cycle.takes.len(), 2);
    }

    #[test]
    fn an_audio_driver_change_cannot_interrupt_a_take() {
        let (_scratch, mut app, track) = rig();
        begin(&mut app, track);
        let previous = app.settings.audio.clone();
        let mut config = previous.clone();
        config.host = komponist_audio::HostKind::Jack;
        app.dispatch(Action::Settings(crate::actions::SettingsAction::SetAudio(
            config,
        )));

        assert_eq!(app.settings.audio, previous);
        assert!(app.audio_recording().is_some());
        assert_eq!(
            app.ui.status.as_ref().unwrap().text,
            app.lang.settings_audio_driver_change_recording()
        );
    }

    #[test]
    fn a_jack_playback_route_change_does_not_restart_an_active_take() {
        let scratch = Scratch::new();
        let (mut app, track) = rig_at(&scratch.0);
        begin(&mut app, track);
        let previous = app.settings.audio.clone();
        let mut config = previous.clone();
        config.jack_routes.playback[0] = Some("system:playback_1".into());
        app.dispatch(Action::Settings(crate::actions::SettingsAction::SetAudio(
            config.clone(),
        )));

        assert_eq!(app.settings.audio, previous);
        assert!(app.audio_recording().is_some());
        assert_eq!(
            app.ui.status.as_ref().unwrap().text,
            app.lang.settings_jack_playback_change_recording()
        );

        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        app.dispatch(Action::Settings(crate::actions::SettingsAction::SetAudio(
            config.clone(),
        )));
        assert_eq!(app.settings.audio, config);
    }

    #[test]
    fn a_sub_tick_lead_in_keeps_its_file_and_does_not_block_loop_takes() {
        let (project, track) = edits::add_audio_track(
            &Project::new("Recorded loop"),
            edits::NewAudioTrack::default(),
        )
        .unwrap();
        let scratch = Scratch::new();
        let take = scratch.0.join("lead-in.wav");
        let cycle = scratch.0.join("loop.wav");
        let make_take = |lap, start, frames, window_frames, path| RecordedTake {
            track,
            path,
            frames,
            window_frames,
            window_end: None,
            sample_rate: 48_000,
            channels: 2,
            start,
            lap,
        };
        let mut writer = hound::WavWriter::create(
            &take,
            hound::WavSpec {
                sample_rate: 48_000,
                channels: 2,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for sample in 0..32_i16 {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();
        let lead_in = make_take(0, Tick(719), 16, None, take.clone());
        let checkpoint = make_take(1, Tick(720), 32, Some(2400), cycle.clone());
        let (project, applied) = apply_takes(
            &project,
            &[lead_in, checkpoint],
            komponist_i18n::Lang::En,
            None,
        )
        .unwrap();
        let mut retained = hound::WavReader::open(&take).unwrap();
        assert!(retained.samples::<i16>().map(Result::unwrap).eq(0..32));
        assert_eq!(applied.clips.len(), 1);
        assert_eq!(applied.linked, 1);
        let notice = applied.unlinked_notice(komponist_i18n::Lang::En).unwrap();
        assert!(notice.contains(take.to_str().unwrap()));
        assert_eq!(applied.unlinked, [take]);
        let clip = &project.track(track).unwrap().clips[0];
        assert_eq!(clip.start, Tick(720));
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("loop audio clip");
        };
        assert_eq!(audio.file.frames, 2400);
        assert_eq!(audio.file.abs, cycle);
    }

    #[test]
    fn recording_lead_in_and_cycle_takes_make_separate_clips() {
        let project = Project::new("Recorded loop");
        let (project, track) =
            edits::add_audio_track(&project, edits::NewAudioTrack::default()).unwrap();
        let loop_start = Tick::from_steps(8);
        let make = |lap, start, frames, window_frames| RecordedTake {
            track,
            path: std::env::temp_dir().join(format!("komponist-loop-{lap}.wav")),
            frames,
            window_frames,
            window_end: None,
            sample_rate: 48_000,
            channels: 2,
            start,
            lap,
        };
        let first = make(0, Tick::ZERO, 48_000, None);
        let loop_pass = make(1, loop_start, 48_000, Some(48_000));
        let loop_checkpoint = make(2, loop_start, 24_000, Some(48_000));
        let expected_paths = [
            first.path.clone(),
            loop_pass.path.clone(),
            loop_checkpoint.path.clone(),
        ];
        let (project, applied) = apply_takes(
            &project,
            &[loop_checkpoint, first, loop_pass],
            komponist_i18n::Lang::En,
            None,
        )
        .unwrap();
        assert_eq!(applied.clips.len(), 2);
        let clips = &project.track(track).unwrap().clips;
        assert_eq!(clips.len(), 2);
        assert_eq!((clips[0].start, clips[0].length), (Tick::ZERO, loop_start));
        assert_eq!(clips[1].start, loop_start);
        assert_eq!(clips[1].length, loop_start);
        let files: Vec<_> = clips
            .iter()
            .flat_map(|clip| {
                let ClipContent::Audio(audio) = &clip.content else {
                    panic!("audio clip")
                };
                audio
                    .takes
                    .iter()
                    .map(|take| (&take.file.abs, take.file.frames))
            })
            .collect();
        assert_eq!(
            files,
            expected_paths
                .iter()
                .map(|path| (path, 48_000))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_rounded_cycle_take_links_to_the_exact_loop_end() {
        let mut project = Project::new("Rounded cycle");
        project.tempo = komponist_model::Tempo::new(122.0);
        let (project, track) =
            edits::add_audio_track(&project, edits::NewAudioTrack::default()).unwrap();
        let scratch = Scratch::new();
        let take = RecordedTake {
            track,
            path: scratch.0.join("loop.wav"),
            frames: 94_427,
            window_frames: Some(94_427),
            window_end: Some(Tick(7680)),
            sample_rate: 48_000,
            channels: 1,
            start: Tick(3840),
            lap: 0,
        };
        let (project, applied) =
            apply_takes(&project, &[take], komponist_i18n::Lang::En, None).unwrap();
        assert_eq!(applied.linked, 1);
        let clip = &project.track(track).unwrap().clips[0];
        assert_eq!(clip.start, Tick(3840));
        assert_eq!(clip.end(), Tick(7680));
    }

    #[test]
    fn stopped_audio_is_one_undo_step_and_undo_keeps_its_file_for_redo() {
        let (_root, mut app, track) = rig();
        begin(&mut app, track);
        assert!(
            app.recording.is_none(),
            "audio must not create a MIDI overlay"
        );
        assert!(app.provisional_recordings_folder().is_some());
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        let clip = app.project().track(track).unwrap().clips[0].clone();
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("audio clip");
        };
        let path = audio.file.abs.clone();
        assert!(path.is_file());
        assert_eq!(
            hound::WavReader::open(&path)
                .unwrap()
                .spec()
                .bits_per_sample,
            24
        );
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(app.project().track(track).unwrap().clips.is_empty());
        assert!(path.is_file());
        app.dispatch(Action::Edit(EditAction::Redo));
        assert_eq!(app.project().track(track).unwrap().clips[0], clip);
        drop(app);
    }
    #[test]
    fn opening_export_waits_for_the_stopped_take_before_computing_its_song_range() {
        let (_root, mut app, audio) = rig();
        begin(&mut app, audio);
        app.dispatch(Action::Transport(TransportAction::Stop));
        assert!(app.engine_mut().run_held(256, &[], 0));
        // The engine has ended capture, but the UI has not polled its final writer result yet.
        app.open_export_dialog();
        assert!(!app.audio_recording.is_active());
        assert_eq!(app.project().track(audio).unwrap().clips.len(), 1);
        assert!(matches!(
            app.ui.dialog,
            Some(crate::ui_state::Dialog::Export(_))
        ));
        drop(app);
    }
    #[test]
    fn save_during_capture_has_independent_assets_that_survive_esc() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let project_file = root.join("Song.komponist");
        assert!(app.save_to(&project_file));
        let saved = crate::project_io::load_project(&project_file).unwrap();
        let ClipContent::Audio(audio) = &saved.track(track).unwrap().clips[0].content else {
            panic!("saved audio");
        };
        let snapshot = audio.file.abs.clone();
        let live = app
            .audio_recording
            .capture
            .as_ref()
            .unwrap()
            .session
            .progress()[0]
            .path
            .clone();
        assert_ne!(snapshot, live);
        assert!(snapshot.starts_with(root.join("Takes")));
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        assert!(app.engine_mut().run_held(256, &[], 0));
        app.step();
        assert!(
            snapshot.is_file(),
            "Esc must preserve saved checkpoint audio"
        );
        assert!(
            !live.is_file(),
            "only the discarded worker's own live file is deleted"
        );
        assert!(app.provisional_recordings_folder().is_none());
        assert!(app.project().track(track).unwrap().clips.is_empty());
        assert!(hound::WavReader::open(snapshot).unwrap().duration() > 0);
        drop(app);
    }
    #[test]
    fn failed_save_discards_checkpoint_copies_without_moving_the_live_writer() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let live_path = |app: &App| {
            app.audio_recording
                .capture
                .as_ref()
                .unwrap()
                .session
                .progress()[0]
                .path
                .clone()
        };
        let original = live_path(&app);
        let missing = root.join("Missing");
        assert!(!app.save_to(&missing.join("Song.komponist")));
        assert!(!missing.exists());
        assert_eq!(live_path(&app), original);

        let destination = root.join("Save As");
        let blocked = destination.join("Song.komponist");
        std::fs::create_dir_all(&blocked).unwrap();
        assert!(!app.save_to(&blocked));

        assert_eq!(app.project_path(), None);
        assert_eq!(app.provisional_recordings_folder(), original.parent());
        assert_eq!(live_path(&app), original);
        assert!(original.is_file());
        assert_eq!(
            std::fs::read_dir(destination.join("Takes"))
                .unwrap()
                .count(),
            0
        );

        std::fs::remove_dir(&blocked).unwrap();
        assert!(app.save_to(&blocked));
        let saved = crate::project_io::load_project(&blocked).unwrap();
        let ClipContent::Audio(audio) = &saved.track(track).unwrap().clips[0].content else {
            panic!("saved audio");
        };
        assert!(audio.file.abs.starts_with(destination.join("Takes")));
        assert_ne!(live_path(&app), audio.file.abs);
        assert!(hound::WavReader::open(&audio.file.abs).unwrap().duration() > 0);
    }

    #[test]
    fn saving_during_capture_keeps_live_sources_referenced_in_redo() {
        use komponist_builtin::Internal;
        use komponist_builtin::sampler::links::{self, SampleLink};

        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let source = app
            .audio_recording()
            .unwrap()
            .progress(track)
            .unwrap()
            .path
            .clone();
        #[cfg(unix)]
        let linked_source = {
            let alias = root.join("Alias.wav");
            std::os::unix::fs::symlink(&source, &alias).unwrap();
            alias
        };
        #[cfg(not(unix))]
        let linked_source = source.clone();
        let sampler = app
            .doc
            .apply_with("Add Sampler with the live source", |project| {
                let (project, track) = edits::add_track(
                    project,
                    edits::NewTrack {
                        name: None,
                        hue: None,
                        instrument: Some(Internal::Sampler.plugin_ref()),
                        index: None,
                        arm_exclusive: false,
                    },
                )?;
                let device = project.track(track).unwrap().channel.devices[0].id;
                let state = links::write(None, 0, Some(&SampleLink::new(&linked_source, None)));
                let project = edits::set_device_state(&project, device, Some(state.into()))?;
                Ok((project, device))
            })
            .unwrap();
        assert!(app.doc.undo());
        let project_file = root.join("Song.komponist");
        assert!(app.save_to(&project_file));
        assert!(source.is_file());
        assert!(app.doc.redo());
        let check_sample = |app: &App| {
            let (_, _, sampler) = app.project().find_device(sampler).unwrap();
            links::read(sampler.state.as_deref()).remove(0).1
        };
        assert_eq!(check_sample(&app).resolve(Some(&root)), Some(linked_source));

        // Once closed, the source can be copied and all history entries can adopt the new name.
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        assert!(app.save_to(&project_file));
        assert!(!source.exists());
        let sample = check_sample(&app).resolve(Some(&root)).unwrap();
        assert!(sample.starts_with(root.join("Takes")));
        assert!(sample.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn save_as_uses_the_physical_parent_of_a_symlink_followed_by_dot_dot() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());

        let chosen_dir = root.join("selected");
        let actual_dir = root.join("actual");
        let symlink = chosen_dir.join("link");
        let target = actual_dir.join("child");
        std::fs::create_dir(&chosen_dir).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        std::os::unix::fs::symlink(&target, &symlink).unwrap();

        assert!(app.save_to(&symlink.join("../Song.komponist")));
        let expected_dir = std::fs::canonicalize(&actual_dir).unwrap();
        let expected_project = expected_dir.join("Song.komponist");
        assert_eq!(app.project_path(), Some(expected_project.as_path()));
        assert!(!chosen_dir.join("Song.komponist").exists());
        assert!(!chosen_dir.join("Takes").exists());

        let saved = crate::project_io::load_project(&expected_project).unwrap();
        let ClipContent::Audio(audio) = &saved.track(track).unwrap().clips[0].content else {
            panic!("saved audio");
        };
        assert!(audio.file.rel.starts_with("Takes"));
        assert_eq!(expected_dir.join(&audio.file.rel), audio.file.abs);
        assert!(audio.file.abs.is_file());
    }

    #[test]
    fn first_save_migrates_every_undo_state_and_keeps_redo_playable() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        let original = match &app.project().track(track).unwrap().clips[0].content {
            ClipContent::Audio(audio) => audio.file.abs.clone(),
            _ => unreachable!(),
        };
        let project_file = root.join("Song.komponist");
        assert!(app.save_to(&project_file));
        assert!(app.provisional_recordings_folder().is_none());
        assert!(!original.is_file());
        app.dispatch(Action::Edit(EditAction::Undo));
        app.dispatch(Action::Edit(EditAction::Redo));
        let ClipContent::Audio(audio) = &app.project().track(track).unwrap().clips[0].content
        else {
            unreachable!();
        };
        assert!(audio.file.abs.starts_with(root.join("Takes")));
        assert!(audio.file.abs.is_file());
        drop(app);
    }
    #[test]
    fn failed_move_keeps_originals_provisional_and_reports_the_reason_after_saving() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        let original = match &app.project().track(track).unwrap().clips[0].content {
            ClipContent::Audio(audio) => audio.file.abs.clone(),
            _ => unreachable!(),
        };
        std::fs::write(root.join("Takes"), b"not a directory").unwrap();
        assert!(app.save_to(&root.join("Song.komponist")));
        assert!(original.is_file());
        assert!(app.provisional_recordings_folder().is_some());
        assert_eq!(app.ui.status.as_ref().unwrap().kind, StatusKind::Error);
        std::fs::remove_file(root.join("Takes")).unwrap();
        assert!(app.save_to(&root.join("Song.komponist")));
        assert!(!original.is_file());
        assert!(app.provisional_recordings_folder().is_none());
        drop(app);
    }
    #[test]
    fn failed_project_write_keeps_originals_and_retry_to_another_folder_updates_links() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        let original = match &app.project().track(track).unwrap().clips[0].content {
            ClipContent::Audio(audio) => audio.file.abs.clone(),
            _ => unreachable!(),
        };
        let blocked = root.join("blocked.komponist");
        std::fs::create_dir(&blocked).unwrap();
        assert!(!app.save_to(&blocked));
        assert!(original.is_file());
        assert!(app.provisional_recordings_folder().is_some());
        let track_file = |app: &App| {
            let ClipContent::Audio(audio) = &app.project().track(track).unwrap().clips[0].content
            else {
                panic!("audio clip");
            };
            audio.file.abs.clone()
        };
        assert_eq!(track_file(&app), original);
        app.dispatch(Action::Edit(EditAction::Undo));
        app.dispatch(Action::Edit(EditAction::Redo));
        assert_eq!(track_file(&app), original);
        assert_eq!(std::fs::read_dir(root.join("Takes")).unwrap().count(), 0);
        let retry = root.join("Retry");
        std::fs::create_dir(&retry).unwrap();
        assert!(app.save_to(&retry.join("Song.komponist")));
        let ClipContent::Audio(audio) = &app.project().track(track).unwrap().clips[0].content
        else {
            unreachable!();
        };
        assert!(audio.file.abs.starts_with(retry.join("Takes")));
        assert!(audio.file.abs.is_file());
        assert!(!original.is_file());
        assert!(app.provisional_recordings_folder().is_none());
        drop(app);
    }

    fn add_midi(app: &mut App) -> TrackId {
        let midi = app
            .doc
            .apply_with("Add MIDI", |project| {
                edits::add_track(
                    project,
                    edits::NewTrack {
                        name: Some("MIDI input".into()),
                        hue: None,
                        instrument: None,
                        index: None,
                        arm_exclusive: false,
                    },
                )
            })
            .unwrap();
        app.doc
            .apply("Arm MIDI", |project| {
                edits::set_track_prop(project, &[midi], edits::TrackProp::Arm(true))
            })
            .unwrap();
        app.step();
        midi
    }
    fn begin_mixed(app: &mut App) {
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        let notes = [
            komponist_engine::MidiInEvent {
                frame: 0,
                port: komponist_engine::MidiPort(1),
                data: [0x90, 60, 100],
            },
            komponist_engine::MidiInEvent {
                frame: 512,
                port: komponist_engine::MidiPort(1),
                data: [0x80, 60, 0],
            },
        ];
        assert!(app.engine_mut().run_held(1024, &notes, 0));
        wait(app, |app| {
            app.audio_recording()
                .is_some_and(|view| view.progress.iter().any(|progress| progress.frames > 0))
        });
    }
    #[test]
    fn mixed_audio_and_midi_stop_as_one_undo_step() {
        let (_root, mut app, audio) = rig();
        let midi = add_midi(&mut app);
        begin_mixed(&mut app);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        assert!(app.recording.is_none());
        assert_eq!(app.project().track(midi).unwrap().clips.len(), 1);
        assert_eq!(app.project().track(audio).unwrap().clips.len(), 1);
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(app.project().track(midi).unwrap().clips.is_empty());
        assert!(app.project().track(audio).unwrap().clips.is_empty());
        app.dispatch(Action::Edit(EditAction::Redo));
        assert_eq!(app.project().track(midi).unwrap().clips.len(), 1);
        assert_eq!(app.project().track(audio).unwrap().clips.len(), 1);
        drop(app);
    }
    #[test]
    fn audio_failure_freezes_and_preserves_the_active_midi_overlay_once() {
        let (_root, mut app, audio) = rig();
        let midi = add_midi(&mut app);
        begin_mixed(&mut app);
        app.engine_mut().send(Command::AbortAudioRecording);
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        assert!(app.recording.is_none());
        assert_eq!(app.project().track(midi).unwrap().clips.len(), 1);
        assert!(app.project().track(audio).unwrap().clips.is_empty());
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(app.project().track(midi).unwrap().clips.is_empty());
        drop(app);
    }
    #[test]
    fn deleting_a_midi_track_during_audio_finalization_does_not_drop_audio() {
        let (_root, mut app, audio) = rig();
        let midi = add_midi(&mut app);
        begin_mixed(&mut app);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        app.doc
            .apply("Remove MIDI", |project| {
                edits::remove_tracks(project, &[midi])
            })
            .unwrap();
        app.step();
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.audio_recording.is_active());
        assert!(app.project().track(midi).is_none());
        assert_eq!(app.project().track(audio).unwrap().clips.len(), 1);
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(app.project().track(midi).is_none());
        assert!(app.project().track(audio).unwrap().clips.is_empty());
        drop(app);
    }
    #[test]
    fn save_during_count_in_does_not_leave_a_stale_provisional_path() {
        let (root, mut app, audio) = rig();
        app.dispatch(Action::Transport(TransportAction::ToggleCountIn));
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        app.step();
        assert_eq!(app.transport(), komponist_engine::TransportState::CountIn);
        assert!(app.audio_recording().unwrap().clip(audio).is_none());
        assert!(app.save_to(&root.join("Count-in.komponist")));
        assert!(app.provisional_recordings_folder().is_none());
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        assert!(app.engine_mut().run_held(256, &[], 0));
        app.step();
        assert!(app.provisional_recordings_folder().is_none());
        drop(app);
    }
    #[test]
    fn unsaved_guard_sees_recording_before_the_ui_has_consumed_worker_progress() {
        let (_root, mut app, _) = rig();
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(1024, &[], 0));
        assert!(app.has_unsaved_take());
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        drop(app);
    }
}
