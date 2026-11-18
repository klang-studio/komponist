//! Transaction around a single-track offline render. The document is edited only after success.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use komponist_engine::export::{
    BitDepth, ExportError, ExportJob, ExportReport, TRACK_TAIL_SECONDS, TrackExport,
};
use komponist_model::{DeviceId, Project, TrackId, TrackKind, edits};
use komponist_plugins::DeviceStatus;

use super::{Phase, Run};
use crate::actions::{Action, SelectMode, SelectionChange, UiAction};
use crate::app::{App, SNAPSHOT_TIMEOUT};
use crate::ui_state::{Dialog, StatusKind};

pub(super) struct Flatten {
    pub(super) id: TrackId,
    pub(super) name: String,
    pub(super) scope: Arc<TrackExport>,
    pub(super) requested: Instant,
    snapshots: Vec<(DeviceId, Arc<[u8]>)>,
    original: Option<Arc<Project>>,
    asset: Asset,
}

/// The empty reservation and the rendered file have the same owner. Cancellation and every
/// failure remove the reservation, or the WAV if the worker had already written it.
struct Asset {
    path: PathBuf,
    provisional: bool,
    keep: bool,
}

impl Asset {
    fn reserve(app: &App, id: TrackId) -> io::Result<Self> {
        let (folder, provisional) = app.generated_audio_folder()?;
        for index in 0..10_000 {
            let path = folder.join(format!("flatten-{}-{index}.wav", id.0));
            match OpenOptions::new().create_new(true).write(true).open(&path) {
                Ok(_) => {
                    return Ok(Self {
                        path,
                        provisional,
                        keep: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "render file names exhausted",
        ))
    }
}

impl Drop for Asset {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_file(&self.path);
            if self.provisional
                && let Some(folder) = self.path.parent()
            {
                let _ = fs::remove_dir(folder);
            }
        }
    }
}

impl Flatten {
    pub(super) fn prepare(&mut self, app: &mut App) -> Result<(), ExportError> {
        let project = Arc::clone(app.doc.project());
        let scope = TrackExport::new(&project, self.id)?;
        if scope.range() != self.scope.range() || project.tempo != self.scope.project().tempo {
            return Err(ExportError::InvalidTrack);
        }
        self.scope = Arc::new(scope);
        let track = project.track(self.id).ok_or(ExportError::InvalidTrack)?;
        let ids: Vec<_> = track
            .channel
            .devices
            .iter()
            .map(|device| device.id)
            .collect();
        self.snapshots = app
            .engine
            .host_mut()
            .snapshot_selected_states(&ids, SNAPSHOT_TIMEOUT);
        self.original = Some(project);
        Ok(())
    }

    pub(super) fn unavailable(&self, app: &App) -> Option<(DeviceId, DeviceStatus)> {
        self.scope
            .devices()
            .filter_map(|id| {
                let status = app.engine.device_status(id);
                (status != DeviceStatus::Ok).then_some((id, status))
            })
            .min_by_key(|(_, status)| *status == DeviceStatus::Loading)
    }

    pub(super) fn finish(mut self, app: &mut App, result: Result<ExportReport, ExportError>) {
        if let Err(error) = result {
            app.flatten_error(error);
            return;
        }
        let Some(original) = self.original.as_ref() else {
            app.flatten_failed(app.lang.flatten_worker_failed());
            return;
        };
        let mut comparable = Project::clone(app.project());
        // A bridge can publish the same snapshot independently when it becomes ready.
        for (id, state) in &self.snapshots {
            if comparable
                .find_device(*id)
                .is_some_and(|(_, _, d)| d.state.as_ref() == Some(state))
                && let Some((_, _, original)) = original.find_device(*id)
            {
                comparable = edits::set_device_state(&comparable, *id, original.state.clone())
                    .unwrap_or(comparable);
            }
        }
        let unchanged = comparable.track(self.id) == original.track(self.id)
            && comparable.tempo_map() == original.tempo_map()
            && TrackExport::new(&comparable, self.id)
                .is_ok_and(|scope| scope.project().tracks == self.scope.project().tracks);
        if !unchanged {
            app.flatten_failed(app.lang.flatten_track_changed());
            return;
        }
        let folder = app.project_dir();
        let file = match crate::audio_import::linked_file(&self.asset.path, folder.as_deref()) {
            Ok(file) => file,
            Err(error) => {
                app.flatten_failed(error);
                return;
            }
        };
        let label = "undo-flatten-track";
        let start = self.scope.range().start;
        let before = Arc::clone(app.doc.project());
        let mut restored = Project::clone(&before);
        for (device, state) in self.snapshots {
            match edits::set_device_state(&restored, device, Some(state)) {
                Ok(project) => restored = project,
                Err(error) => {
                    app.flatten_failed(error);
                    return;
                }
            }
        }
        // An in-flight gesture belongs to its own edit. Install the flattened track in one step.
        app.doc.end_gesture();
        let Some(clip) = app.apply_with(label, |project| {
            edits::flatten_midi_track(project, self.id, start, file)
        }) else {
            app.flatten_failed(app.lang.flatten_worker_failed());
            return;
        };
        if restored != *before {
            // Enrich only the state this undo step restores. Earlier parameter edits keep their
            // own snapshots, and a render failure never changes the document or its history.
            app.doc.rewrite_untracked(|project| {
                std::ptr::eq(project, before.as_ref()).then(|| restored.clone())
            });
        }
        if self.asset.provisional {
            app.track_provisional_audio(self.asset.path.clone());
        }
        self.asset.keep = true;
        app.dispatch(Action::Ui(UiAction::Select(SelectionChange::Clips {
            clips: vec![clip],
            mode: SelectMode::Replace,
        })));
        app.set_status(app.lang.flatten_done(&self.name), StatusKind::Success);
    }
}

impl App {
    pub(crate) fn start_flatten(&mut self, id: TrackId) {
        if self.export.is_active() || self.ui.dialog.is_some() || self.dialogs.overlay.is_some() {
            return;
        }
        if self
            .project()
            .track(id)
            .is_none_or(|t| t.kind != TrackKind::Midi || t.clips.is_empty())
        {
            self.flatten_error(ExportError::InvalidTrack);
            return;
        }
        if !self.stop_for_export() {
            return;
        }
        self.engine.sync(&self.doc);
        let scope = match TrackExport::new(self.project(), id) {
            Ok(scope) => Arc::new(scope),
            Err(error) => {
                self.flatten_error(error);
                return;
            }
        };
        if let Some(sample) = crate::dialogs::missing_plugins::missing_samples(
            scope.project(),
            self.project_dir().as_deref(),
        )
        .into_iter()
        .find(|sample| scope.devices().any(|id| id == sample.device))
        {
            self.flatten_failed(self.lang.flatten_missing_sample(sample.file));
            return;
        }
        let Some(track) = self.project().track(id) else {
            return;
        };
        let name = track.name.clone();
        let rate = self.engine.sample_rate();
        let range = ExportJob::frames_of(&self.project().tempo_map(), scope.range(), rate);
        if range.is_empty() {
            self.flatten_error(ExportError::InvalidTrack);
            return;
        }
        let asset = match Asset::reserve(self, id) {
            Ok(asset) => asset,
            Err(error) => {
                self.flatten_failed(error);
                return;
            }
        };
        let job = ExportJob {
            path: asset.path.clone(),
            sample_rate: rate,
            range,
            tail: ExportJob::tail_frames(TRACK_TAIL_SECONDS, rate),
            bit_depth: BitDepth::Float32,
            normalize: None,
            dither: false,
        };
        self.open_dialog(Dialog::Flatten);
        self.export.run = Some(Run {
            seconds: job.frames() as f64 / f64::from(rate),
            job,
            phase: Phase::Devices,
            flatten: Some(Flatten {
                id,
                name,
                scope,
                requested: Instant::now(),
                snapshots: Vec::new(),
                original: None,
                asset,
            }),
        });
        self.poll_export();
    }

    pub(super) fn flatten_failed(&mut self, reason: impl std::fmt::Display) {
        self.set_status(self.lang.flatten_failed(reason), StatusKind::Error);
    }

    pub(super) fn flatten_error(&mut self, error: ExportError) {
        match error {
            ExportError::Cancelled => {
                self.set_status(self.lang.flatten_cancelled(), StatusKind::Info)
            }
            ExportError::InvalidTrack => self.flatten_failed(self.lang.flatten_no_clips()),
            ExportError::DeviceFailed(id) => {
                let device = self
                    .project()
                    .find_device(id)
                    .map(|(_, _, device)| device.plugin.name.as_str())
                    .unwrap_or_default();
                self.flatten_failed(self.lang.flatten_device_failed(device));
            }
            ExportError::MissingAudio(path) => {
                self.flatten_failed(self.lang.export_missing_audio(path.display()))
            }
            error => self.flatten_failed(error),
        }
    }
}
