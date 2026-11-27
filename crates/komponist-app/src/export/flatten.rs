//! Transaction around a single-track offline render. The document is edited only after success.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use komponist_engine::export::{
    BitDepth, ExportError, ExportJob, ExportReport, TRACK_TAIL_SECONDS, TrackExport,
};
use komponist_model::{DeviceId, Project, TrackId, TrackKind, edits};
use komponist_plugins::{DeviceSnapshots, DeviceStatus};

use crate::actions::{Action, SelectMode, SelectionChange, UiAction};
use crate::app::{App, SNAPSHOT_TIMEOUT};
use crate::ui_state::{Dialog, StatusKind};

pub(super) struct Flatten {
    pub(super) id: TrackId,
    pub(super) name: String,
    pub(super) scope: Arc<TrackExport>,
    pub(super) requested: Instant,
    snapshots: DeviceSnapshots,
    original: Option<Arc<Project>>,
    asset: Asset,
}

/// A private project audio directory owns the WAV until the document references it.
struct Asset {
    path: PathBuf,
    provisional: bool,
    directory: tempfile::TempDir,
}

impl Asset {
    fn reserve(app: &App, id: TrackId) -> io::Result<Self> {
        let (folder, provisional) = app.generated_audio_folder()?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("flatten-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let directory = builder.tempdir_in(folder)?;
        let path = directory.path().join(format!("flatten-{}.wav", id.0));
        Ok(Self {
            path,
            provisional,
            directory,
        })
    }
}

impl Flatten {
    pub(super) fn prepare(&mut self, app: &mut App) -> Result<(), String> {
        let changed = || app.lang.flatten_track_changed().to_owned();
        let project = Arc::clone(app.doc.project());
        let track = project.track(self.id).ok_or_else(changed)?;
        edits::validate_flatten_automation(track)
            .map_err(|_| app.lang.flatten_automation_too_large().to_owned())?;
        let scope = TrackExport::new(&project, self.id).map_err(|_| changed())?;
        if scope.range() != self.scope.range() || project.tempo != self.scope.project().tempo {
            return Err(changed());
        }
        self.scope = Arc::new(scope);
        if let Some((_, problem)) = self.unavailable_audio_asset(app) {
            return Err(problem);
        }
        let ids: Vec<_> = track
            .channel
            .devices
            .iter()
            .map(|device| device.id)
            .collect();
        self.snapshots = app
            .engine
            .host_mut()
            .snapshot_selected_states(&ids, SNAPSHOT_TIMEOUT)
            .map_err(|id| app.lang.flatten_state_failed(app.device_name(id)))?;
        self.original = Some(project);
        Ok(())
    }

    pub(super) fn unavailable(&self, app: &App) -> Option<(DeviceId, DeviceStatus)> {
        self.scope
            .devices()
            .filter_map(|id| {
                let host = app.engine.host();
                match host.status(id) {
                    DeviceStatus::Ok
                        if app
                            .project()
                            .find_device(id)
                            .is_some_and(|(_, _, device)| host.render_assets_pending(device)) =>
                    {
                        Some((id, DeviceStatus::Loading))
                    }
                    DeviceStatus::Ok => None,
                    status => Some((id, status)),
                }
            })
            .min_by_key(|(_, status)| *status == DeviceStatus::Loading)
    }

    pub(super) fn unavailable_audio_asset(&self, app: &App) -> Option<(DeviceId, String)> {
        self.scope.devices().find_map(|id| {
            app.unavailable_render_audio_asset(id)
                .map(|problem| (id, problem))
        })
    }

    pub(super) fn finish(self, app: &mut App, result: Result<ExportReport, ExportError>) {
        match result {
            Err(ExportError::DeviceFailed(id))
                if let Some((failed, problem)) = self.unavailable_audio_asset(app)
                    && failed == id =>
            {
                app.flatten_failed(problem);
                return;
            }
            Err(error) => {
                app.flatten_error(error);
                return;
            }
            Ok(_) if let Some((_, problem)) = self.unavailable_audio_asset(app) => {
                app.flatten_failed(problem);
                return;
            }
            Ok(_) => {}
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
                .is_some_and(|(_, _, d)| d.has_saved_state(state))
                && let Some((_, _, original)) = original.find_device(*id)
            {
                let restored = match original.saved_state() {
                    Some(original) => edits::set_saved_plugin_state(&comparable, *id, &original),
                    None => edits::set_device_state(&comparable, *id, None),
                };
                comparable = restored.unwrap_or(comparable);
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
            match edits::set_saved_plugin_state(&restored, device, &state) {
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
        let _ = self.asset.directory.keep();
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
        let Some(track) = self
            .project()
            .track(id)
            .filter(|track| track.kind == TrackKind::Midi && !track.clips.is_empty())
        else {
            self.flatten_error(ExportError::InvalidTrack);
            return;
        };
        if edits::validate_flatten_automation(track).is_err() {
            self.flatten_failed(self.lang.flatten_automation_too_large());
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
        self.export.begin(
            job,
            Some(Flatten {
                id,
                name,
                scope,
                requested: Instant::now(),
                snapshots: Vec::new(),
                original: None,
                asset,
            }),
        );
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
            ExportError::WavTooLarge => self.flatten_failed(self.lang.export_wav_too_large()),
            ExportError::DeviceFailed(id) => {
                let device = self.device_name(id);
                self.flatten_failed(self.lang.flatten_device_failed(device));
            }
            ExportError::MissingAudio(path) => {
                self.flatten_failed(self.lang.export_missing_audio(path.display()))
            }
            ExportError::Stretch { clip, reason } => {
                let (name, why) = self.warp_error(clip, reason);
                self.flatten_failed(self.lang.flatten_warp_failed(name, why));
            }
            error => self.flatten_failed(error),
        }
    }
}
