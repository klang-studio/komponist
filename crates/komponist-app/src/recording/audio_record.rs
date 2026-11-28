//! UI adapter for the session's audio capture and take finalization.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use komponist_engine::Command;
use komponist_engine::recording::RecordingTrack;
use komponist_model::edits;
use komponist_model::{AudioInput, Tick, TrackKind};
use komponist_session::recording::audio::{
    self, AppliedTakes, AudioRecording, StagedRecordings, TAKE_FINISH_TIMEOUT, TakeProgress,
};
use komponist_session::recording::automation::AutomationCapture;
use komponist_session::recording::midi::Commit;
use komponist_session::recording::writer::{RecordedTake, RecordingConfig, RecordingFormat};

use super::MidiTakeNames;
use crate::actions::{Action, UiAction};
use crate::app::App;
use crate::settings::CaptureFormat;
use crate::ui_state::StatusKind;

impl App {
    /// Overrides the provisional recording root for embedding/tests. No directory is created
    /// until recording starts. Headless construction always uses scratch space.
    pub fn with_recording_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.recording.audio_root = Some(directory.into());
        self
    }
    /// Current audio capture, including a count-in or a draining stop tail.
    pub fn audio_recording(&self) -> Option<&AudioRecording> {
        self.recording.audio.running()
    }

    /// Folder shown by the chip while an owned file still lives outside the project.
    pub fn provisional_recordings_folder(&self) -> Option<&Path> {
        self.recording.audio.folder()
    }

    /// A destination for generated audio, with the same first-save migration as a take.
    pub(crate) fn generated_audio_folder(&self) -> std::io::Result<(PathBuf, bool)> {
        if let Some(directory) = self.project_dir() {
            let target = directory.join("Takes");
            std::fs::create_dir_all(&target)?;
            return Ok((target, false));
        }
        let root = self
            .recording
            .audio_root
            .clone()
            .or_else(crate::dirs::recordings_dir)
            .ok_or_else(|| std::io::Error::other(self.lang.audio_record_folder_unavailable()))?;
        std::fs::create_dir_all(&root)?;
        Ok((root, true))
    }

    /// Owns an unsaved render until the project is saved, including across undo and redo.
    pub(crate) fn track_provisional_audio(&mut self, path: PathBuf) {
        self.recording.audio.track_provisional(path);
    }

    pub(crate) fn prepare_audio_take(&mut self, from: Tick) -> bool {
        if self.recording.audio.is_active() {
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
        let provisional = self.project_path().is_none();
        let directory = if let Some(directory) = self.project_dir() {
            directory.join("Takes")
        } else {
            let root = self
                .recording
                .audio_root
                .clone()
                .or_else(crate::dirs::recordings_dir);
            let Some(root) = root else {
                self.set_status(
                    self.lang.audio_record_folder_unavailable(),
                    StatusKind::Error,
                );
                return false;
            };
            let date = crate::dialogs::format::local(std::time::SystemTime::now()).map_or_else(
                || "recording".into(),
                |date| format!("{:04}-{:02}-{:02}", date.year, date.month, date.day),
            );
            match audio::provisional_directory(&root, &self.project().name, &date) {
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
        let rate = self.session.engine().sample_rate();
        let format = match self.settings.capture_format {
            CaptureFormat::Pcm24 => RecordingFormat::Pcm24,
            CaptureFormat::Float32 => RecordingFormat::Float32,
        };
        let base = edits::next_take_number(self.project());
        match self.recording.audio.prepare(
            RecordingConfig {
                directory,
                sample_rate: rate,
                format,
                tracks,
            },
            from,
            provisional,
            base,
        ) {
            Ok(recorder) => {
                self.session
                    .engine_mut()
                    .send(Command::SetRecorder(recorder));
                true
            }
            Err(error) => {
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
                false
            }
        }
    }

    pub(crate) fn update_audio_take(&mut self) {
        self.advance_audio_take(None);
    }

    fn advance_audio_take(&mut self, deadline: Option<Instant>) {
        let names = MidiTakeNames(self.lang);
        if deadline.is_none() {
            self.recording.audio.refresh_view(
                self.session.document().project().tempo,
                self.session.engine().playhead(),
                &names,
            );
        }
        let directory = self.project_dir();
        let progress = self.recording.audio.advance(
            self.session.document_mut(),
            directory.as_deref(),
            deadline,
        );
        match progress {
            TakeProgress::Pending => {}
            TakeProgress::Cancelled => self.sync_project(),
            TakeProgress::Failed {
                error,
                midi,
                automation,
            } => {
                // A writer failure may precede Stop, so freeze any still-running MIDI take.
                let mut commits = Vec::from_iter(midi);
                if let Some(midi) = self.recording.finish_midi(
                    self.session.document().project(),
                    self.session.engine().playhead(),
                ) {
                    commits.push(midi);
                }
                if !commits.is_empty() || automation.is_some() {
                    let label = commits
                        .first()
                        .map_or("undo-automation-record", Commit::label);
                    self.commit_recorded_take(label, automation.as_ref(), &commits, &[]);
                }
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
                self.session.engine_mut().send(Command::StopRecording);
            }
            TakeProgress::Ready(take) => {
                let label = take.midi.as_ref().map_or_else(
                    || {
                        self.lang
                            .record_take(edits::next_take_number(self.project()))
                    },
                    |commit| commit.label().to_owned(),
                );
                self.session.document_mut().end_gesture();
                let applied = self.commit_recorded_take(
                    &label,
                    take.automation.as_ref(),
                    take.midi.as_slice(),
                    &take.takes,
                );
                if let Some(applied) = applied {
                    if let Some(first) = applied.clips.first() {
                        self.dispatch(Action::Ui(UiAction::OpenClip(*first)));
                    }
                    if let Some(notice) = unlinked_notice(&applied, self.lang) {
                        self.set_status(notice, StatusKind::Info);
                    } else if take.midi.as_ref().is_some_and(Commit::incomplete) {
                        self.set_status(self.lang.record_notes_lost(), StatusKind::Error);
                    } else if applied.linked > 0 {
                        self.set_status(
                            self.lang.audio_record_complete(applied.linked),
                            StatusKind::Success,
                        );
                    }
                } else {
                    self.set_status(self.lang.audio_record_apply_failed(), StatusKind::Error);
                }
                if let Some(error) = take.move_failed {
                    self.set_status(self.lang.audio_record_move_failed(error), StatusKind::Error);
                }
            }
        }
    }

    /// Commits the headless edit, then updates the live devices and the app's error status.
    pub(crate) fn commit_recorded_take(
        &mut self,
        label: &str,
        automation: Option<&AutomationCapture>,
        midi: &[Commit],
        wavs: &[RecordedTake],
    ) -> Option<AppliedTakes> {
        let directory = self.project_dir();
        let before = Arc::clone(self.session.document().project());
        match audio::commit_take(
            self.session.document_mut(),
            label,
            automation,
            midi,
            wavs,
            &MidiTakeNames(self.lang),
            directory.as_deref(),
        ) {
            Ok(applied) => {
                self.follow_device_states(&before);
                Some(applied)
            }
            Err(error) => {
                self.record_edit_error(error);
                None
            }
        }
    }

    /// A stopped capture drains its latency tail before a Save-and-leave action can leave the
    /// project. The audio thread keeps running; a timeout leaves the project and files open.
    /// It waits on the writer's and then the staging worker's completion signal, at most
    /// [`TAKE_FINISH_TIMEOUT`] in all.
    pub(crate) fn await_audio_take(&mut self) -> bool {
        if !self.recording.audio.needs_finalization() {
            return true;
        }
        let deadline = Instant::now() + TAKE_FINISH_TIMEOUT;
        while self.recording.audio.is_active() && Instant::now() < deadline {
            self.advance_audio_take(Some(deadline));
        }
        if self.recording.audio.is_active() {
            self.set_status(self.lang.audio_record_finalizing(), StatusKind::Info);
            false
        } else {
            true
        }
    }

    pub(crate) fn stage_recordings_for_save(
        &mut self,
        project_dir: &Path,
    ) -> Result<StagedRecordings, ()> {
        self.recording
            .audio
            .stage_for_save(project_dir)
            .map_err(|error| {
                self.set_status(self.lang.audio_record_failed(error), StatusKind::Error);
            })
    }

    pub(crate) fn recordings_saved(&mut self, staged: StagedRecordings) {
        let warnings = self.recording.audio.saved(staged);
        if let Some(error) = warnings.remove_failed {
            self.set_status(self.lang.audio_record_move_failed(error), StatusKind::Error);
        } else if let Some(error) = warnings.relocate_failed {
            self.set_status(self.lang.audio_record_move_failed(error), StatusKind::Error);
        } else {
            let mut notice = warnings
                .move_failed
                .map(|error| (self.lang.audio_record_move_failed(error), StatusKind::Error));
            if let Some(unlinked) = warnings.unlinked
                && let Some(warning) = unlinked_notice(&unlinked, self.lang)
            {
                if let Some((text, _)) = &mut notice {
                    text.push_str(" · ");
                    text.push_str(&warning);
                } else {
                    notice = Some((warning, StatusKind::Info));
                }
            }
            if let Some((text, kind)) = notice {
                self.set_status(text, kind);
            }
        }
    }
}

fn unlinked_notice(applied: &AppliedTakes, lang: komponist_i18n::Lang) -> Option<String> {
    if applied.unlinked.is_empty() {
        return None;
    }
    let paths = applied
        .unlinked
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(" · ");
    Some(lang.audio_record_unlinked(applied.linked, paths))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{EditAction, TransportAction};
    use komponist_model::{ClipContent, Project, TrackId};
    use komponist_session::recording::audio::apply_takes;
    use std::sync::mpsc;
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
        app.session
            .document_mut()
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
        wait(&mut app, |app| !app.recording.audio.is_active());
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
        wait(&mut app, |app| !app.recording.audio.is_active());
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
            &MidiTakeNames(komponist_i18n::Lang::En),
            None,
        )
        .unwrap();
        let mut retained = hound::WavReader::open(&take).unwrap();
        assert!(retained.samples::<i16>().map(Result::unwrap).eq(0..32));
        assert_eq!(applied.clips.len(), 1);
        assert_eq!(applied.linked, 1);
        let notice = unlinked_notice(&applied, komponist_i18n::Lang::En).unwrap();
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
            &MidiTakeNames(komponist_i18n::Lang::En),
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
        let (project, applied) = apply_takes(
            &project,
            &[take],
            &MidiTakeNames(komponist_i18n::Lang::En),
            None,
        )
        .unwrap();
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
            app.recording.midi.is_none(),
            "audio must not create a MIDI overlay"
        );
        assert!(app.provisional_recordings_folder().is_some());
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
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
        assert!(!app.recording.audio.is_active());
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
        let saved = komponist_session::project_io::load_project(&project_file).unwrap();
        let ClipContent::Audio(audio) = &saved.track(track).unwrap().clips[0].content else {
            panic!("saved audio");
        };
        let snapshot = audio.file.abs.clone();
        let live = app.recording.audio.writer_progress_for_test()[0]
            .current
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
            app.recording.audio.writer_progress_for_test()[0]
                .current
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
        let saved = komponist_session::project_io::load_project(&blocked).unwrap();
        let ClipContent::Audio(audio) = &saved.track(track).unwrap().clips[0].content else {
            panic!("saved audio");
        };
        assert!(audio.file.abs.starts_with(destination.join("Takes")));
        assert_ne!(live_path(&app), audio.file.abs);
        assert!(hound::WavReader::open(&audio.file.abs).unwrap().duration() > 0);
    }

    #[test]
    fn saving_during_capture_keeps_live_sources_referenced_in_redo() {
        use komponist_model::plugin_assets::AssetLink;
        use komponist_model::{PluginFormat, PluginRef};

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
            .session
            .document_mut()
            .apply_with("Add a device with the live source", |project| {
                let (project, track) = edits::add_track(
                    project,
                    edits::NewTrack {
                        name: None,
                        hue: None,
                        instrument: Some(PluginRef {
                            format: PluginFormat::Clap,
                            id: "org.example.file-player".into(),
                            name: "File player".into(),
                            vendor: String::new(),
                            version: String::new(),
                            module: Box::default(),
                        }),
                        index: None,
                        arm_exclusive: false,
                    },
                )?;
                let device = project.track(track).unwrap().channel.devices[0].id;
                let link = AssetLink::new(&linked_source, None);
                let project = edits::set_device_asset_links(&project, device, [(0, link)].into())?;
                Ok((project, device))
            })
            .unwrap();
        assert!(app.session.document_mut().undo());
        let project_file = root.join("Song.komponist");
        assert!(app.save_to(&project_file));
        assert!(source.is_file());
        assert!(app.session.document_mut().redo());
        let check_sample = |app: &App| {
            let (_, _, player) = app.project().find_device(sampler).unwrap();
            player.asset_links[&0].clone()
        };
        assert_eq!(check_sample(&app).resolve(Some(&root)), Some(linked_source));

        // Once closed, the source can be copied and all history entries can adopt the new name.
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
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
        wait(&mut app, |app| !app.recording.audio.is_active());

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

        let saved = komponist_session::project_io::load_project(&expected_project).unwrap();
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
        wait(&mut app, |app| !app.recording.audio.is_active());
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
        wait(&mut app, |app| !app.recording.audio.is_active());
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
        wait(&mut app, |app| !app.recording.audio.is_active());
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
            .session
            .document_mut()
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
        app.session
            .document_mut()
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
            app.audio_recording().is_some_and(|view| {
                view.tracks().any(|track| {
                    view.progress(track)
                        .is_some_and(|progress| progress.frames > 0)
                })
            })
        });
    }
    #[test]
    fn mixed_audio_and_midi_stop_as_one_undo_step() {
        let (_root, mut app, audio) = rig();
        let midi = add_midi(&mut app);
        begin_mixed(&mut app);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
        assert!(app.recording.midi.is_none());
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
    fn save_drains_an_interrupted_mixed_take_before_staging() {
        use komponist_engine::{EVENT_CAPACITY, MAX_TRACKS, MidiInEvent, MidiPort, TransportState};
        use komponist_model::TickRange;

        let (root, mut app, audio) = rig();
        let midi = add_midi(&mut app);
        let range = TickRange::new(Tick::ZERO, Tick(1));
        app.session
            .document_mut()
            .apply("Set a short loop", |project| {
                let project =
                    edits::set_project_prop(project, edits::ProjectProp::LoopRange(range))?;
                edits::set_project_prop(&project, edits::ProjectProp::LoopOn(true))
            })
            .unwrap();
        app.step();
        let rate = f64::from(app.engine_mut().sample_rate());
        let lap = app.project().tempo_map().tick_to_sample(range.end, rate) as usize;
        assert!(lap > 0);
        // Playback fills the ordinary queue without creating audio take files.
        app.dispatch(Action::Transport(TransportAction::TogglePlay));
        assert!(app.engine_mut().run_held(lap * EVENT_CAPACITY, &[], 0));
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        let notes = [MidiInEvent {
            frame: 1,
            port: MidiPort(1),
            data: [0x90, 60, 100],
        }];
        assert!(
            app.engine_mut()
                .run_held(lap * (2 * MAX_TRACKS + 16), &notes, 0)
        );
        assert_eq!(app.transport(), TransportState::Playing);
        // The transport stopped recording, but the interruption is still queued for the app.
        assert!(app.recording.audio.is_active() && !app.recording.audio.needs_finalization());
        assert!(app.recording.midi.is_none());

        let path = root.join("Interrupted.komponist");
        // Every short lap can create a WAV. On a slow disk the writer may need more than
        // one save attempt's wait budget. Until it finishes, Save As must leave no project file.
        let deadline = Instant::now() + Duration::from_secs(10);
        let finalizing = app.lang.audio_record_finalizing();
        while !app.save_to(&path) {
            assert_eq!(
                app.ui.status.as_ref().map(|status| status.text.as_str()),
                Some(finalizing)
            );
            assert!(!path.exists());
            assert!(
                Instant::now() < deadline,
                "interrupted take did not finalize"
            );
        }
        let saved = komponist_session::project_io::load_project(&path).unwrap();
        let audio_clip = &saved.track(audio).unwrap().clips[0];
        let ClipContent::Audio(audio) = &audio_clip.content else {
            panic!("saved audio clip")
        };
        assert!(audio.file.abs.is_file());
        let midi_clip = &saved.track(midi).unwrap().clips[0];
        let ClipContent::Midi(midi) = &midi_clip.content else {
            panic!("saved MIDI clip")
        };
        assert!(
            midi.takes
                .iter()
                .any(|take| take.name == app.lang.record_take_incomplete(1))
        );
    }

    /// #210, owner 2026-11-23: the engine panics while a mixed take runs. The take ends at the
    /// crash like an interruption and keeps what was recorded so far, the audio and the MIDI
    /// (whose lap is marked incomplete), as one undo step; the new engine stays stopped there.
    #[test]
    fn a_mixed_take_is_kept_when_the_engine_panics() {
        let (_root, mut app, audio) = rig();
        // A hexa track, so a processor can panic; armed with the audio track.
        app.dispatch(Action::Edit(EditAction::NewMidiTrack));
        let midi = app.project().tracks.last().unwrap().id;
        app.session
            .document_mut()
            .apply("Arm both", |project| {
                edits::set_track_prop(project, &[audio, midi], edits::TrackProp::Arm(true))
            })
            .unwrap();
        app.step();
        // Single-threaded, so the panic is on this thread and outside `assert_no_alloc`.
        assert_eq!(
            app.engine_mut()
                .set_workers(Some(0))
                .unwrap()
                .unwrap()
                .threads,
            0
        );
        begin_mixed(&mut app);
        assert!(app.recording.midi.is_some());
        let instrument = app.project().track(midi).unwrap().channel.devices[0].id;
        let slot = app
            .session
            .engine()
            .program()
            .unwrap()
            .slots
            .slot(instrument)
            .unwrap();
        let at = app.session.engine().playhead();
        app.session.engine_mut().send(Command::InsertProcessor {
            slot,
            processor: Box::new(komponist_engine::testing::TestPanic),
        });
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            app.session.engine_mut().run_held(256, &[], 0);
        }));
        assert!(caught.is_err(), "the processor ran");
        app.session.engine_mut().release_engine();
        wait(&mut app, |app| !app.recording.audio.is_active());
        assert!(app.recording.midi.is_none());
        let midi_clip = &app.project().track(midi).unwrap().clips[0];
        let ClipContent::Midi(content) = &midi_clip.content else {
            panic!("a MIDI clip")
        };
        assert_eq!(content.takes[0].name, app.lang.record_take_incomplete(1));
        assert_eq!(content.takes[0].notes.len(), 1);
        let audio_clip = &app.project().track(audio).unwrap().clips[0];
        let ClipContent::Audio(content) = &audio_clip.content else {
            panic!("an audio clip")
        };
        assert!(content.file.abs.is_file());
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(app.project().track(midi).unwrap().clips.is_empty());
        assert!(app.project().track(audio).unwrap().clips.is_empty());
        wait(&mut app, |app| app.session.engine().playhead() == at);
        assert_eq!(app.transport(), komponist_engine::TransportState::Stopped);
    }

    #[test]
    fn audio_failure_freezes_and_preserves_the_active_midi_overlay_once() {
        let (_root, mut app, audio) = rig();
        let midi = add_midi(&mut app);
        begin_mixed(&mut app);
        app.engine_mut().send(Command::AbortAudioRecording);
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
        assert!(app.recording.midi.is_none());
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
        app.session
            .document_mut()
            .apply("Remove MIDI", |project| {
                edits::remove_tracks(project, &[midi])
            })
            .unwrap();
        app.step();
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
        assert!(app.project().track(midi).is_none());
        assert_eq!(app.project().track(audio).unwrap().clips.len(), 1);
        app.dispatch(Action::Edit(EditAction::Undo));
        assert!(app.project().track(midi).is_none());
        assert!(app.project().track(audio).unwrap().clips.is_empty());
        drop(app);
    }

    /// #221: a finished take is copied into the project's Takes folder on a worker. Frames go on
    /// meanwhile; Save waits for the copy on its completion signal, not in a sleep loop.
    #[test]
    fn a_finished_take_is_copied_into_the_project_on_a_worker() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        // The project got its folder while the take was running in the provisional folder.
        let project_file = root.join("Song.komponist");
        app.session
            .accept_saved_project(app.session.document().clone(), project_file.clone());
        let (release, go) = mpsc::channel();
        let (finished, _) = mpsc::channel();
        app.recording
            .audio
            .hold_staging_for_test(go, finished, None);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| {
            app.recording.audio.staging_paths_for_test().is_some()
        });
        let source = app
            .recording
            .audio
            .provisional_for_test()
            .iter()
            .next()
            .unwrap()
            .clone();
        // The worker is held with its copy made: the frames still run, the take is not in yet,
        // and the copy has no final name in Takes that anything could link.
        for _ in 0..5 {
            app.step();
        }
        assert!(app.recording.audio.is_active());
        assert!(app.recording.audio.has_material());
        assert!(app.project().track(track).unwrap().clips.is_empty());
        assert!(visible(&root.join("Takes")).is_empty());
        let deadline = Instant::now() + Duration::from_secs(5);
        while hidden(&root.join("Takes"))
            .first()
            .is_none_or(|private| visible(&root.join("Takes").join(private)).is_empty())
        {
            assert!(Instant::now() < deadline, "the worker made no private copy");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(visible(&root.join("Takes")).is_empty());

        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            release.send(()).unwrap();
        });
        assert!(app.save_to(&project_file));
        releaser.join().unwrap();
        assert!(!app.recording.audio.is_active());
        let clip = &app.project().track(track).unwrap().clips[0];
        let ClipContent::Audio(audio) = &clip.content else {
            panic!("audio clip");
        };
        assert!(audio.file.abs.starts_with(root.join("Takes")));
        assert!(audio.file.abs.is_file());
        assert!(!source.exists(), "the saved project took over the copy");
        assert!(app.provisional_recordings_folder().is_none());
        assert!(
            hidden(&root.join("Takes")).is_empty(),
            "the private folder is gone"
        );
    }

    /// Names in `folder`, without the hidden ones.
    fn visible(folder: &Path) -> Vec<String> {
        entries(folder)
            .into_iter()
            .filter(|name| !name.starts_with('.'))
            .collect()
    }

    /// The hidden names in `folder`.
    fn hidden(folder: &Path) -> Vec<String> {
        entries(folder)
            .into_iter()
            .filter(|name| name.starts_with('.'))
            .collect()
    }

    fn entries(folder: &Path) -> Vec<String> {
        std::fs::read_dir(folder)
            .map(|entries| {
                entries
                    .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A take discarded while its copies are made leaves no copies behind.
    #[test]
    fn a_take_discarded_during_its_copy_leaves_no_copies() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        app.session
            .accept_saved_project(app.session.document().clone(), root.join("Song.komponist"));
        let (release, go) = mpsc::channel::<()>();
        let (finished, done) = mpsc::channel();
        app.recording
            .audio
            .hold_staging_for_test(go, finished, None);
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| {
            app.recording.audio.staging_paths_for_test().is_some()
        });
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        assert!(!app.recording.audio.is_active());
        // Let the worker copy into a result nobody takes.
        drop(release);
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(std::fs::read_dir(root.join("Takes")).unwrap().count(), 0);
        assert!(app.project().track(track).unwrap().clips.is_empty());
    }

    /// Holds the next staging worker: stops the take begun on `track` in a saved project and
    /// steps until the worker has made its private copy and waits for `release`. `finished`
    /// reports the worker's end.
    fn staging_held(app: &mut App, root: &Path) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
        app.session
            .accept_saved_project(app.session.document().clone(), root.join("Song.komponist"));
        let (release, go) = mpsc::channel::<()>();
        let (finished, done) = mpsc::channel();
        let (ready, waiting) = mpsc::channel();
        app.recording
            .audio
            .hold_staging_for_test(go, finished, Some(ready));
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(app, |app| {
            app.recording.audio.staging_paths_for_test().is_some()
        });
        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
        (release, done)
    }

    /// Copies already made but not yet received when the take is discarded are deleted too.
    #[test]
    fn copies_made_before_a_discard_are_deleted() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let (release, done) = staging_held(&mut app, &root);
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        // The worker has ended; its private copy waits in the channel for the next frame.
        assert_eq!(hidden(&root.join("Takes")).len(), 1);
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        assert!(!app.recording.audio.is_active());
        assert_eq!(std::fs::read_dir(root.join("Takes")).unwrap().count(), 0);
    }

    /// Quitting waits (bounded) for the worker of a dropped take to delete its copies.
    #[test]
    fn shutdown_waits_until_a_dropped_take_has_no_copies() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let (release, _done) = staging_held(&mut app, &root);
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            release.send(()).unwrap();
        });
        app.recording.audio.shutdown(Duration::from_secs(5));
        releaser.join().unwrap();
        assert_eq!(std::fs::read_dir(root.join("Takes")).unwrap().count(), 0);
    }

    /// #221: published take names are flushed in Takes before any original can go. If that
    /// flush fails, the take keeps its originals and the status says the move failed.
    #[cfg(unix)]
    #[test]
    fn a_failed_flush_of_the_published_takes_keeps_the_originals() {
        use komponist_session::project_io::set_directory_sync_failure_for_test;
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let (release, done) = staging_held(&mut app, &root);
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        // The commit (and its flush) runs on this thread in the next frame.
        set_directory_sync_failure_for_test(true);
        app.step();
        set_directory_sync_failure_for_test(false);
        assert!(!app.recording.audio.is_active());
        let ClipContent::Audio(audio) = &app.project().track(track).unwrap().clips[0].content
        else {
            panic!("audio clip");
        };
        assert!(audio.file.abs.starts_with(root.join("Provisional")));
        assert!(audio.file.abs.is_file());
        assert!(
            app.recording
                .audio
                .provisional_for_test()
                .contains(&audio.file.abs)
        );
        assert!(app.recording.audio.pending_removals_for_test().is_empty());
        assert!(
            visible(&root.join("Takes")).is_empty(),
            "no unflushed copy stays"
        );
        let status = app.ui.status.as_ref().unwrap();
        assert_eq!(status.kind, StatusKind::Error, "{}", status.text);
    }

    /// A take whose copy fails links its original WAV, and the status keeps the reason.
    #[test]
    fn a_failed_copy_links_the_original_and_keeps_the_error_visible() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        std::fs::write(root.join("Takes"), b"not a folder").unwrap();
        app.session
            .accept_saved_project(app.session.document().clone(), root.join("Song.komponist"));
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
        let ClipContent::Audio(audio) = &app.project().track(track).unwrap().clips[0].content
        else {
            panic!("audio clip");
        };
        assert!(audio.file.abs.starts_with(root.join("Provisional")));
        assert!(audio.file.abs.is_file());
        assert!(
            app.recording
                .audio
                .provisional_for_test()
                .contains(&audio.file.abs)
        );
        let status = app.ui.status.as_ref().unwrap();
        assert_eq!(status.kind, StatusKind::Error, "{}", status.text);
        assert_ne!(status.text, app.lang.audio_record_complete(1));
    }

    /// #221: a staging worker held past the shutdown deadline leaves its private copies when the
    /// app ends; the next Save to that folder, or opening the project, deletes them.
    #[test]
    fn copies_left_by_a_worker_past_the_deadline_are_reclaimed() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let (release, _done) = staging_held(&mut app, &root);
        let takes = root.join("Takes");
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        let started = Instant::now();
        app.recording.audio.shutdown(Duration::from_millis(20));
        assert!(started.elapsed() < Duration::from_secs(1));
        // The app "ended" here: the worker still holds its private copy.
        assert_eq!(hidden(&takes).len(), 1);
        assert!(app.save_to(&root.join("Song.komponist")));
        assert!(
            hidden(&takes).is_empty(),
            "Save reclaims the abandoned copy"
        );
        drop(release);

        // The same after a crash, found when the project opens.
        let abandoned = takes.join(".komponist-staging-crashed");
        std::fs::create_dir_all(&abandoned).unwrap();
        std::fs::write(abandoned.join("Take.wav"), b"RIFF").unwrap();
        app.open_path(root.join("Song.komponist"));
        assert!(!abandoned.exists());
    }

    /// #221: when the background copy into a folder failed, the Save that follows keeps the
    /// original links without copying again on the UI thread; a later Save copies.
    #[test]
    fn a_save_after_a_failed_copy_keeps_the_originals_and_the_next_one_copies() {
        let (root, mut app, track) = rig();
        begin(&mut app, track);
        let takes = root.join("Takes");
        std::fs::write(&takes, b"not a folder").unwrap();
        app.session
            .accept_saved_project(app.session.document().clone(), root.join("Song.komponist"));
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        wait(&mut app, |app| !app.recording.audio.is_active());
        std::fs::remove_file(&takes).unwrap();
        let link = |app: &App| {
            let saved =
                komponist_session::project_io::load_project(&root.join("Song.komponist")).unwrap();
            let ClipContent::Audio(audio) = &saved.track(track).unwrap().clips[0].content else {
                panic!("audio clip");
            };
            let _ = app;
            audio.file.abs.clone()
        };
        assert!(app.save_to(&root.join("Song.komponist")));
        assert!(link(&app).starts_with(root.join("Provisional")));
        assert!(visible(&takes).is_empty(), "no copy in this Save");
        assert_eq!(app.ui.status.as_ref().unwrap().kind, StatusKind::Error);
        assert!(app.save_to(&root.join("Song.komponist")));
        assert!(link(&app).starts_with(&takes), "the next Save copies");
    }

    /// #221: a writer that fails after finishing a lap no frame saw, collected on the blocking
    /// path: that lap's WAV stays tracked for recovery.
    #[test]
    fn a_writer_failure_on_the_blocking_path_keeps_the_last_finished_lap() {
        let (_root, mut app, track) = rig();
        let (lo, hi) = (Tick(32), Tick(64));
        app.session
            .document_mut()
            .apply("Set loop", |project| {
                let project = edits::set_project_prop(
                    project,
                    edits::ProjectProp::LoopRange(komponist_model::TickRange::new(lo, hi)),
                )?;
                edits::set_project_prop(&project, edits::ProjectProp::LoopOn(true))
            })
            .unwrap();
        app.step();
        let map = app.project().tempo_map();
        let rate = f64::from(app.engine_mut().sample_rate());
        let lead = map.tick_to_sample(lo, rate) as usize;
        let end = map.tick_to_sample(hi, rate) as usize;
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(lead + 64, &[], 0));
        wait(&mut app, |app| {
            app.audio_recording()
                .and_then(|view| view.progress(track))
                .is_some_and(|progress| progress.frames > 0)
        });
        assert!(app.engine_mut().run_held(3 * (end - lead), &[], 0));
        // The writer publishes the laps on its own thread; no frame runs meanwhile. Wait until it
        // has drained what the engine queued.
        let deadline = Instant::now() + Duration::from_secs(5);
        let position = |app: &App| {
            let current = app
                .recording
                .audio
                .writer_progress_for_test()
                .remove(0)
                .current;
            (current.lap, current.frames)
        };
        let mut last = position(&app);
        let mut stable = 0;
        while stable < 5 || last.0 < 3 {
            assert!(Instant::now() < deadline, "the writer did not catch up");
            std::thread::sleep(Duration::from_millis(5));
            let now = position(&app);
            stable = if now == last { stable + 1 } else { 0 };
            last = now;
        }
        app.engine_mut().send(Command::AbortAudioRecording);
        assert!(app.engine_mut().run_held(256, &[], 0));
        let finished: Vec<PathBuf> = {
            app.recording
                .audio
                .writer_progress_for_test()
                .into_iter()
                .flat_map(|snapshot| snapshot.completed.into_iter().flatten())
                .filter(|progress| progress.frames > 0)
                .map(|progress| progress.path.clone())
                .filter(|path| !app.recording.audio.knows_path_for_test(path))
                .collect()
        };
        assert!(!finished.is_empty(), "a lap no frame saw");
        app.advance_audio_take(Some(Instant::now() + Duration::from_secs(5)));
        assert!(!app.recording.audio.is_active());
        for path in &finished {
            assert!(path.is_file());
            assert!(
                app.recording.audio.provisional_for_test().contains(path),
                "{path:?}"
            );
        }
    }

    /// Laps that finished between two frames never reach the live view; a take discarded while
    /// its copy is made still gives them up.
    #[test]
    fn laps_no_frame_saw_are_given_up_with_a_take_discarded_during_its_copy() {
        let (root, mut app, track) = rig();
        let (lo, hi) = (Tick(32), Tick(64));
        app.session
            .document_mut()
            .apply("Set loop", |project| {
                let project = edits::set_project_prop(
                    project,
                    edits::ProjectProp::LoopRange(komponist_model::TickRange::new(lo, hi)),
                )?;
                edits::set_project_prop(&project, edits::ProjectProp::LoopOn(true))
            })
            .unwrap();
        app.step();
        let map = app.project().tempo_map();
        let rate = f64::from(app.engine_mut().sample_rate());
        let lead = map.tick_to_sample(lo, rate) as usize;
        let end = map.tick_to_sample(hi, rate) as usize;
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(lead + 64, &[], 0));
        wait(&mut app, |app| {
            app.audio_recording()
                .and_then(|view| view.progress(track))
                .is_some_and(|progress| progress.frames > 0)
        });
        app.session
            .accept_saved_project(app.session.document().clone(), root.join("Song.komponist"));
        let (release, go) = mpsc::channel::<()>();
        let (finished, _done) = mpsc::channel();
        app.recording
            .audio
            .hold_staging_for_test(go, finished, None);
        // Four laps without a frame, then stop: the middle laps are never shown.
        assert!(app.engine_mut().run_held(4 * (end - lead), &[], 0));
        app.dispatch(Action::Transport(TransportAction::ToggleRecord));
        assert!(app.engine_mut().run_held(256, &[], 0));
        app.advance_audio_take(Some(Instant::now() + Duration::from_secs(5)));
        let paths: Vec<PathBuf> = app
            .recording
            .audio
            .staging_paths_for_test()
            .expect("the copy waits")
            .map(Path::to_path_buf)
            .collect();
        assert!(paths.len() >= 4, "{paths:?}");
        app.dispatch(Action::Transport(TransportAction::DiscardTake));
        for path in &paths {
            assert!(
                !app.recording.audio.provisional_for_test().contains(path),
                "{path:?}"
            );
        }
        drop(release);
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
