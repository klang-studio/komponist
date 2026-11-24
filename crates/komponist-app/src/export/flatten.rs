//! Transaction around a single-track offline render. The document is edited only after success.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use komponist_builtin::Internal;
use komponist_engine::export::{
    BitDepth, ExportError, ExportJob, ExportReport, TRACK_TAIL_SECONDS, TrackExport,
};
use komponist_model::{DeviceId, Project, TrackId, TrackKind, edits};
use komponist_plugins::{DeviceSnapshots, DeviceStatus};
use komponist_sampler::PadStatus;

use super::{Phase, Run};
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
        if let Some((_, problem)) = self.bad_sampler_pad(app) {
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
                        if host.sampler_is_loading(id)
                            || Self::sampler_pads(app, id).is_some_and(|(_, pads)| {
                                pads.iter().any(|pad| {
                                    pad.link.is_some()
                                        && matches!(
                                            pad.status,
                                            PadStatus::Loading | PadStatus::Empty
                                        )
                                })
                            }) =>
                    {
                        Some((id, DeviceStatus::Loading))
                    }
                    DeviceStatus::Ok => None,
                    status => Some((id, status)),
                }
            })
            .min_by_key(|(_, status)| *status == DeviceStatus::Loading)
    }

    pub(super) fn bad_sampler_pad(&self, app: &App) -> Option<(DeviceId, String)> {
        for id in self.scope.devices() {
            let host = app.engine.host();
            if host.sampler_sync_unavailable(id) {
                return Some((
                    id,
                    app.lang.flatten_sampler_sync_failed(app.device_name(id)),
                ));
            }
            let Some((general, views)) = Self::sampler_pads(app, id) else {
                continue;
            };
            for (pad, view) in views.into_iter().enumerate() {
                if view.link.is_some()
                    && matches!(view.status, PadStatus::Missing | PadStatus::Failed(_))
                {
                    let name = app.device_name(id);
                    let message = if general {
                        app.lang.flatten_sampler_failed(name, view.name)
                    } else {
                        app.lang.flatten_pad_failed(name, pad + 1, view.name)
                    };
                    return Some((id, message));
                }
            }
        }
        None
    }

    fn sampler_pads(app: &App, id: DeviceId) -> Option<(bool, Vec<komponist_sampler::PadView>)> {
        let host = app.engine.host();
        let device = app.project().find_device(id).map(|(_, _, device)| device);
        let builtin = device.and_then(|device| crate::device_host::internal_kind(device));
        let bundled =
            device.and_then(|device| komponist_plugins::bundled_sampler::kind(&device.plugin));
        let general = builtin.or(bundled) == Some(Internal::GeneralSampler);
        let pads = if let Some(handle) = host.sampler(id) {
            handle.views()
        } else if let Some((kind, device)) = bundled.zip(device) {
            crate::views::devices::bundled_sampler_view(
                kind,
                device
                    .state
                    .as_deref()
                    .and_then(komponist_plugins::bundled_sampler::assets)
                    .as_deref(),
                &device.asset_links,
                host.sampler_ui(id),
            )
            .pads
        } else {
            return None;
        };
        Some((general, pads))
    }

    pub(super) fn finish(self, app: &mut App, result: Result<ExportReport, ExportError>) {
        match result {
            Err(ExportError::DeviceFailed(id))
                if let Some((failed, problem)) = self.bad_sampler_pad(app)
                    && failed == id =>
            {
                app.flatten_failed(problem);
                return;
            }
            Err(error) => {
                app.flatten_error(error);
                return;
            }
            Ok(_) if let Some((_, problem)) = self.bad_sampler_pad(app) => {
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
            diagnostics: Vec::new(),
            offline_devices: Vec::new(),
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
            ExportError::WavTooLarge => self.flatten_failed(self.lang.export_wav_too_large()),
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
            ExportError::Stretch { clip, reason } => {
                let (name, why) = self.warp_error(clip, reason);
                self.flatten_failed(self.lang.flatten_warp_failed(name, why));
            }
            error => self.flatten_failed(error),
        }
    }
}
