//! Project files and the recent list (ARCHITECTURE §5.5), and the project lifecycle of the app:
//! new (S10), open (with the S06 check), save, save as, the recent list of the start screen (S09).
//!
//! - `*.komponist` files are read and written with `komponist_model::persist`; writing goes
//!   through [`komponist_session::project_io`].
//! - The recent list lives in `recent.json` next to the settings file
//!   (`~/.config/komponist/recent.json`, on macOS in `~/Library/Application Support/komponist/`, on Windows in `%APPDATA%\komponist\`), newest first, at most [`RECENT_LIMIT`] entries.
//! - Opening or creating a project resets the engine ([`komponist_session::engine_sync::EngineSync::reset`]).
//!   A project whose plugins are not in the catalog opens the S06 dialog; while a plugin scan
//!   runs, the check waits for its end, so plugins the scan is about to find are not reported.
//! - A dirty project asks first (`dialogs::new_project`): save, discard or cancel; so does
//!   closing the window ([`App::close_requested`]). A running take with notes counts as unsaved
//!   even in a clean project (#85): "Speichern" ends it as Stop would and saves it, "Nicht
//!   speichern" throws it away, "Abbrechen" keeps recording. A failed save goes no further.
//! - Saving while recording writes the document plus the take as Stop would commit it at that
//!   moment, and capture goes on into the same take. No root of the undo history equals that
//!   file, so the project stays unsaved until it is saved again after Stop.
//! - Files the desktop asks to open (macOS: a double-click in Finder, files dropped on the Dock
//!   icon, a launch with files, #96; Linux: the command line of the desktop file's `%f`) take the
//!   same path as the recent list: [`App::open_files`] → `ProjectAction::OpenPath` (the unsaved
//!   prompt, the recent list, the reason a file cannot be read). One project is open at a time, so
//!   of several files the first is opened; the project that is open already is not loaded again.
//! - Saving under a new path renames the project after the file ("Unbenannt" saved as
//!   `Hafenlicht.komponist` becomes "Hafenlicht"), outside the undo history.
//!
//! Explicit opens and writes run on the UI thread. The start screen reads summaries on a worker.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use komponist_i18n::Lang;
use komponist_model::asset_collection::Collector;
use komponist_model::edits::ProjectProp;
use komponist_model::persist::{FILE_EXTENSION, FORMAT_VERSION, PersistError};
use komponist_model::{Document, Project};
use komponist_plugins::PluginManagerEvent;
use komponist_session::project_io::{
    ProjectIoError, load_project, load_project_repaired, name_from_path, resolve_project_parent,
    save_project_reporting, with_extension, write_atomic,
};

use crate::actions::{Action, ProjectAction, SettingsAction, UiAction};
use crate::app::App;
use crate::dialogs::file::{FileBrowser, Purpose};
use crate::dialogs::new_project::{self, Answer};
use crate::dialogs::{Overlay, Pending};
use crate::param_undo::replay_deferred_plugins;
use crate::platform::Platform;
use crate::ui_state::{Dialog, FocusArea, Screen, SettingsSection, StatusKind};

/// File name of the recent list inside the config directory.
pub const RECENT_FILE: &str = "recent.json";

/// Most entries kept in the recent list.
pub const RECENT_LIMIT: usize = 12;

/// Localizes a project file error for the status bar and recovery dialogs.
pub(crate) fn project_io_reason(error: &ProjectIoError, t: Lang) -> String {
    match error {
        ProjectIoError::Io(_, error) => file_io_reason(error, t),
        ProjectIoError::Format(_, PersistError::NotAProject) => {
            t.project_reason_not_project().to_owned()
        }
        ProjectIoError::Format(_, PersistError::UnsupportedVersion { found }) => {
            if *found > FORMAT_VERSION {
                t.project_reason_too_new(*found)
            } else {
                t.project_reason_unsupported_version(*found)
            }
        }
        ProjectIoError::Format(..) => t.project_reason_damaged().to_owned(),
    }
}

pub(crate) fn file_io_reason(error: &std::io::Error, t: Lang) -> String {
    match error.kind() {
        std::io::ErrorKind::NotFound => t.project_reason_not_found(),
        std::io::ErrorKind::PermissionDenied => t.project_reason_denied(),
        _ => t.project_reason_io(),
    }
    .to_owned()
}

/// The recent list file next to the settings file `settings`.
pub fn recent_path(settings: &Path) -> PathBuf {
    settings
        .parent()
        .map_or_else(|| PathBuf::from(RECENT_FILE), |d| d.join(RECENT_FILE))
}

/// The recently opened or saved projects, newest first (`recent.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecentList {
    /// Project files, newest first.
    pub projects: Vec<PathBuf>,
}

impl RecentList {
    /// Reads `path`; a missing file is an empty list.
    pub fn load(path: &Path) -> std::io::Result<RecentList> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RecentList::default()),
            Err(e) => Err(e),
        }
    }

    /// Writes the list to `path` (pretty JSON, atomic).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        if let Some(error) = write_atomic(path, &text, true)? {
            eprintln!(
                "komponist: {}: not flushed to disk: {error}",
                path.display()
            );
        }
        Ok(())
    }

    /// Puts `path` at the front (once), keeping [`RECENT_LIMIT`] entries.
    pub fn add(&mut self, path: PathBuf) {
        self.projects.retain(|p| *p != path);
        self.projects.insert(0, path);
        self.projects.truncate(RECENT_LIMIT);
    }

    /// Removes `path`; `false` if it was not listed.
    pub fn remove(&mut self, path: &Path) -> bool {
        let before = self.projects.len();
        self.projects.retain(|p| p != path);
        self.projects.len() != before
    }
}

/// What the start screen shows of a project file (S09 `recent`: name, "124 BPM · 6 Spuren",
/// the track colours, the date).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectSummary {
    /// Project name.
    pub name: String,
    /// Tempo in BPM.
    pub bpm: f64,
    /// Number of tracks.
    pub tracks: usize,
    /// Track hues in order (the colour bars).
    pub hues: Vec<u16>,
    /// When the file was last written.
    pub modified: Option<SystemTime>,
}

impl ProjectSummary {
    /// The summary of `project` saved at `modified`.
    pub fn of(project: &Project, modified: Option<SystemTime>) -> ProjectSummary {
        ProjectSummary {
            name: project.name.clone(),
            bpm: project.tempo.bpm,
            tracks: project.tracks.len(),
            hues: project.tracks.iter().map(|t| t.hue.0).collect(),
            modified,
        }
    }

    /// Reads the summary of the project file at `path`.
    pub fn read(path: &Path) -> Result<ProjectSummary, ProjectIoError> {
        let project = load_project(path)?;
        let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        Ok(ProjectSummary::of(&project, modified))
    }

    /// "124 BPM · 6 Spuren" ("124 BPM · 6 tracks").
    pub fn meta(&self, t: komponist_i18n::Lang) -> String {
        t.start_meta(fmt_bpm(self.bpm), self.tracks)
    }
}

/// "124", "98", "124.5": the tempo without trailing zeros.
pub fn fmt_bpm(bpm: f64) -> String {
    let text = format!("{bpm:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

// ------------------------------------------------------------------------------------------------
// The app's lifecycle
// ------------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum AfterParamText {
    Save(ProjectAction),
    Leave(Pending),
    SaveAnswer,
    SaveFile(PathBuf),
}

impl App {
    pub(crate) fn resume_after_param_text(&mut self) {
        self.expire_param_text();
        if self.param_undo.has_pending_text() {
            return;
        }
        match self.dialogs.after_param_text.take() {
            Some(AfterParamText::Save(action)) => self.lifecycle_action(action),
            Some(AfterParamText::Leave(next)) => self.guard(next),
            Some(AfterParamText::SaveAnswer) => self.answer_unsaved(Answer::Save),
            Some(AfterParamText::SaveFile(path)) => self.file_chosen(Purpose::Save, path),
            None => {}
        }
    }

    /// New, open, save, save as and the S06 answers (`ProjectAction`, module docs).
    pub(crate) fn lifecycle_action(&mut self, action: ProjectAction) {
        if matches!(action, ProjectAction::Save | ProjectAction::CollectAndSave)
            && self.param_undo.has_pending_text()
        {
            self.dialogs.after_param_text = Some(AfterParamText::Save(action));
            return;
        }
        match action {
            ProjectAction::New => self.guard(Pending::New),
            ProjectAction::Open => self.guard(Pending::Open),
            ProjectAction::Quit => self.guard(Pending::Quit),
            ProjectAction::OpenPath(path) => self.guard(Pending::OpenPath(path)),
            ProjectAction::Save => match self.project_path().map(Path::to_path_buf) {
                // A recovered copy whose file is gone asks for a place (crash recovery, variant d).
                Some(path) if !self.recovery_ui.save_asks => {
                    self.save_to(&path);
                }
                _ => self.open_file_browser(Purpose::Save),
            },
            ProjectAction::SaveAs => self.open_file_browser(Purpose::Save),
            ProjectAction::ReplaceAudio(clip) => {
                self.open_file_browser(Purpose::ReplaceAudio(clip))
            }
            ProjectAction::ReplaceAudioPath { clip, path } => self.replace_audio_path(clip, path),
            ProjectAction::CollectAndSave => self.collect_and_save(),
            ProjectAction::ShowStartScreen => self.guard(Pending::StartScreen),
            ProjectAction::ContinueWithMissing => {
                if matches!(self.ui.dialog, Some(Dialog::MissingPlugins(_))) {
                    self.dispatch(Action::Ui(UiAction::CloseDialog));
                }
            }
            // S06 → S07: the settings replace the dialog (the focus to return to stays), and the
            // folders are scanned again.
            ProjectAction::CheckPluginFolders => {
                self.open_dialog(Dialog::Settings(SettingsSection::Plugins));
                self.dialogs.settings.scroll_to = Some(SettingsSection::Plugins);
                self.scan_plugins();
            }
            _ => {}
        }
    }

    /// Files the desktop asked to open (module docs): the first one opens through
    /// `ProjectAction::OpenPath`, unless it is the open project's file.
    pub fn open_files(&mut self, paths: &[PathBuf]) {
        let Some(first) = paths.first() else {
            return;
        };
        if paths.len() > 1 {
            eprintln!(
                "komponist: {} files to open; opening {} only",
                paths.len(),
                first.display()
            );
        }
        let path = std::path::absolute(first).unwrap_or_else(|_| first.clone());
        let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => a == b,
        };
        if self.ui.screen == Screen::Project
            && self.project_path().is_some_and(|open| same(open, &path))
        {
            return;
        }
        // At launch the recovery prompt comes first; the file waits behind it.
        if self.defer_behind_recovery(&path) {
            return;
        }
        self.dispatch(Action::Project(ProjectAction::OpenPath(path)));
    }

    /// Whether leaving the project loses something: unsaved changes or a running take with notes.
    fn has_unsaved(&self) -> bool {
        self.session.document().is_dirty()
            || self.has_unsaved_take()
            || self.param_undo.has_pending_text()
            || self.session.engine().host().has_pending_state_edits()
            || !self.deferred_plugin_edits().is_empty()
    }

    /// `next` replaces the open project: asks first if it has unsaved changes.
    fn guard(&mut self, next: Pending) {
        if self.param_undo.has_pending_text() {
            self.dialogs.after_param_text = Some(AfterParamText::Leave(next));
            return;
        }
        // Native menu and desktop file-open requests can arrive above the render modal. Cancel
        // without blocking the UI and reconsider unsaved changes once the engine is back.
        if next == Pending::Quit {
            self.dialogs.after_flatten = None;
        } else if self.export.is_flattening() {
            self.dialogs.after_flatten = Some(next);
            self.cancel_export();
            self.resume_after_flatten();
            return;
        }
        if self.ui.screen == Screen::Project && self.has_unsaved() {
            self.dialogs.pending = Some(next);
            self.open_overlay(Overlay::Unsaved);
        } else {
            self.proceed(next);
        }
    }

    pub(crate) fn resume_after_flatten(&mut self) {
        if !self.export.is_flattening()
            && !self.session.engine().is_detached()
            && self.dialogs.overlay.is_none()
            && let Some(next) = self.dialogs.after_flatten.take()
        {
            self.guard(next);
        }
    }

    /// Runs a step that the guard let through.
    pub(crate) fn proceed(&mut self, next: Pending) {
        match next {
            Pending::New => {
                self.open_document(Document::new(new_project::empty_project(self.lang)), None);
                let created = Platform::current().pick(
                    self.lang.project_new_created(),
                    self.lang.project_new_created_mac(),
                );
                self.set_status(created, StatusKind::Info);
            }
            Pending::Template => {
                let Some((drums, hexa)) =
                    new_project::template_instruments(self.session.engine().host().catalog())
                else {
                    self.set_status(
                        "Bundled CLAP devices are unavailable".to_owned(),
                        StatusKind::Error,
                    );
                    return;
                };
                let project = new_project::template(self.lang, drums, hexa);
                self.open_document(Document::new(project), None);
                self.check_missing_plugins();
            }
            Pending::StartScreen => self.show_start_screen(),
            Pending::Open => self.open_file_browser(Purpose::Open),
            Pending::OpenPath(path) => self.open_path(path),
            Pending::Quit => {
                if self.export.is_flattening() {
                    self.cancel_export();
                }
                self.dialogs.quit_confirmed = true;
                self.dialogs.close_now = true;
            }
        }
    }

    /// The window is asked to close (its close button, Alt F4): `true` if it may close now. A
    /// project with unsaved changes or a running take asks first (save, discard, cancel); after
    /// "Speichern" or "Nicht speichern" the window closes by itself.
    pub fn close_requested(&mut self) -> bool {
        if self.dialogs.quit_confirmed || self.ui.screen != Screen::Project || !self.has_unsaved() {
            if self.export.is_flattening() {
                self.cancel_export();
            }
            return true;
        }
        if !matches!(self.dialogs.overlay, Some(Overlay::Unsaved)) {
            self.guard(Pending::Quit);
        }
        false
    }

    /// Whether a confirmed quit wants the window closed now (once).
    pub(crate) fn take_close(&mut self) -> bool {
        std::mem::take(&mut self.dialogs.close_now)
    }

    /// Opens the project file at `path`: asks first if a recovery copy of it waits (#227), then
    /// [`App::open_path_now`].
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        // An open that arrives while the recovery prompt is up (a file dropped on the start
        // screen) waits behind it.
        if self.defer_behind_recovery(&path) {
            return;
        }
        if !self.ask_before_opening(&path) {
            self.open_path_now(path);
        }
    }

    /// Opens the project file at `path` (no questions): resets the engine, remembers the file,
    /// checks the plugins (S06). A file that cannot be read leaves everything as it was and says
    /// why.
    pub(crate) fn open_path_now(&mut self, path: PathBuf) {
        let loaded = resolve_project_parent(path.clone())
            .and_then(|resolved| load_project_repaired(&resolved).map(|loaded| (resolved, loaded)));
        match loaded {
            Ok((path, (project, repairs))) => {
                let project_dir =
                    komponist_model::paths::absolute(path.parent().unwrap_or(Path::new("")));
                crate::recording::audio_record::reclaim_abandoned_takes(&project_dir);
                self.open_document(Document::new(project), Some(path.clone()));
                if !repairs.is_empty() {
                    eprintln!("komponist: {}: repaired on open: {repairs}", path.display());
                }
                self.dialogs.repaired = repairs.total();
                self.remember(path);
                self.check_missing_plugins();
            }
            Err(e) => {
                let file = file_name(&path);
                eprintln!("komponist: {e}");
                self.set_status(
                    self.lang
                        .project_open_failed(&file, project_io_reason(&e, self.lang)),
                    StatusKind::Error,
                );
            }
        }
    }

    /// S06: shows the missing plugins of the open project, or says it opened. While a scan runs
    /// the check waits for its end (`App::handle_device_event`).
    pub(crate) fn check_missing_plugins(&mut self) {
        let name = self.project().name.clone();
        let repaired =
            (self.dialogs.repaired > 0).then(|| self.lang.project_repaired(self.dialogs.repaired));
        if self.session.engine().host().is_scanning() {
            self.dialogs.missing_check = true;
            let text = match &repaired {
                Some(repaired) => self.lang.project_opened_repaired(&name, repaired),
                None => self.lang.project_opened(&name),
            };
            self.set_status(text, StatusKind::Info);
            return;
        }
        self.dialogs.missing_check = false;
        let missing = self.session.engine().host().missing_plugins(self.project());
        let audio = crate::dialogs::missing_plugins::missing_audio(
            self.project(),
            self.project_dir().as_deref(),
        );
        let assets = crate::dialogs::missing_plugins::missing_assets(
            self.lang,
            self.project(),
            self.project_dir().as_deref(),
            self.session.engine().host().catalog(),
        );
        if missing.is_empty() && audio.is_empty() && assets.is_empty() {
            let text = match &repaired {
                Some(repaired) => self.lang.project_opened_repaired(&name, repaired),
                None => self.lang.project_opened_ready(&name),
            };
            self.set_status(text, StatusKind::Info);
            return;
        }
        let n = if missing.is_empty() && !assets.is_empty() {
            self.lang.plugin_file_missing_title().to_owned()
        } else if audio.is_empty() && assets.is_empty() {
            crate::dialogs::missing_plugins::title(self.lang, missing.len())
        } else if missing.is_empty() {
            self.lang.missing_audio_files().to_owned()
        } else {
            self.lang.missing_plugins_and_files().to_owned()
        };
        let n = match &repaired {
            Some(repaired) => format!("{n} · {repaired}"),
            None => n,
        };
        self.set_status(
            self.lang.project_opened_problems(&name, n),
            StatusKind::Error,
        );
        // A dialog the user opened meanwhile (a late check after a scan) is not replaced; the
        // status bar says it. A running export keeps its dialog on top: S06 waits for it.
        let exporting = self.is_exporting() && matches!(self.ui.dialog, Some(Dialog::Export(_)));
        if self.ui.screen == Screen::Project
            && (self.ui.dialog.is_none() || exporting)
            && self.dialogs.overlay.is_none()
        {
            self.open_dialog(Dialog::MissingPlugins(missing));
        }
    }

    /// Saves the project to `path` (`.komponist` added): takes the bridged plugins' states first,
    /// renames the project after a new file, remembers the file. A running take goes into the
    /// file without ending (module docs). `false` if writing failed (the status bar says why).
    pub(crate) fn save_to(&mut self, path: &Path) -> bool {
        self.save_project_to(path, None)
    }

    pub(crate) fn save_to_collected(&mut self, path: &Path, collector: &mut Collector) -> bool {
        self.save_project_to(path, Some(collector))
    }

    fn save_project_to(&mut self, path: &Path, mut collector: Option<&mut Collector>) -> bool {
        let path = match resolve_project_parent(with_extension(path)) {
            Ok(path) => path,
            Err(error) => return self.report_project_save_error(error),
        };
        if !self.await_audio_take() {
            return false;
        }
        // Route queued GUI edits before requesting state, so a temporary parameter cannot be
        // mistaken for a persisted plugin value.
        self.drain_events();
        self.session.refresh_snapshot_automation();
        let edit_revision = self.param_undo.live_edit_revision();
        let mut outcome = self
            .session
            .engine_mut()
            .host_mut()
            .snapshot_for_save(crate::app::SNAPSHOT_TIMEOUT);
        let before_capture_events = self.device_events().len();
        self.drain_events();
        // Events can arrive between the host's last capture poll and this drain. Keep a
        // newer committed edit or preset report from being overwritten by older bytes.
        outcome
            .blocked
            .extend(self.param_undo.devices_edited_after(edit_revision));
        for (device, _) in &outcome.captured {
            let reported = self.device_events()[before_capture_events..].iter().any(|event| {
                matches!(event,
                    PluginManagerEvent::ClapPresetLoaded { device: loaded, .. }
                    | PluginManagerEvent::ClapPresetReported { device: loaded, startup: false, .. }
                    if loaded == device
                )
            });
            if reported {
                outcome.blocked.push(*device);
            }
        }
        outcome.blocked.sort_unstable();
        outcome.blocked.dedup();
        outcome
            .captured
            .retain(|(device, _)| !outcome.blocked.contains(device));
        let snapshots = outcome.captured;
        if !outcome.blocked.is_empty() {
            // Keep successful snapshots as ordinary device updates when no file will be written.
            for (device, state) in &snapshots {
                self.store_snapshot(*device, state);
            }
            let devices = outcome
                .blocked
                .into_iter()
                .map(|device| self.device_name(device))
                .collect::<Vec<_>>()
                .join(", ");
            let detail = self.lang.project_save_plugin_pending(devices);
            self.set_status(self.lang.project_save_failed(detail), StatusKind::Error);
            return false;
        }
        for (device, state) in &snapshots {
            self.store_snapshot_for_save(*device, state);
        }
        self.drain_events();
        // A recording interruption can start audio finalization while we drain engine events.
        // It owns the MIDI commit until the audio take has finished.
        if !self.await_audio_take() {
            return false;
        }
        let playhead = self.session.engine().playhead();
        let old_dir = self
            .project_dir()
            .map(|directory| komponist_model::paths::absolute(&directory));
        let new_dir =
            komponist_model::paths::absolute(path.parent().unwrap_or_else(|| Path::new("")));
        if let Some(collector) = &mut collector
            && let Err(error) = crate::project_assets::collect_project_files(
                self.project(),
                collector,
                old_dir.as_deref(),
                &self.dialogs.plugin_assets.approved_plugin_files,
            )
        {
            eprintln!("komponist: collecting samples failed: {error}");
            self.set_status(self.lang.status_collect_failed(error), StatusKind::Error);
            return false;
        }
        let mut recordings = match self.stage_recordings_for_save(&new_dir) {
            Ok(recordings) => recordings,
            Err(()) => return false,
        };
        let collected = |source: &Path| {
            collector
                .as_ref()
                .and_then(|collector| collector.placed(source))
        };
        let mut document = self.session.document().clone();
        document.rewrite_untracked(|project| {
            crate::project_assets::rebase_project_links(
                project,
                old_dir.as_deref(),
                &new_dir,
                |source| collected(&source).unwrap_or_else(|| recordings.destination(source)),
                |source| {
                    collector
                        .as_ref()
                        .and_then(|collector| collector.placed_asset(&source))
                        .unwrap_or_else(|| recordings.destination(source))
                },
            )
        });
        // A running take so far, with the notes still in flight, on top of all that (#85).
        let mut for_file =
            match self.project_with_take(document.project(), playhead, &mut recordings) {
                Ok(project) => project,
                Err(()) => return false,
            };
        let deferred = self.deferred_plugin_edits();
        for (device, state) in &snapshots {
            if deferred.iter().any(|edit| edit.device() == *device) {
                continue;
            }
            let project = for_file.as_ref().unwrap_or_else(|| document.project());
            let Some((_, _, model)) = project.find_device(*device) else {
                continue;
            };
            if !model.has_saved_state(state)
                && let Ok(updated) =
                    komponist_model::edits::set_saved_plugin_state(project, *device, state)
            {
                for_file = Some(updated);
            }
        }
        if let Some(updated) = replay_deferred_plugins(
            for_file.as_ref().unwrap_or_else(|| document.project()),
            deferred,
        ) {
            for_file = Some(updated);
        }
        // A new file renames the project after it; the rename happens only if writing works.
        let rename = name_from_path(&path).filter(|name| {
            self.project_path() != Some(path.as_path()) && *name != self.project().name
        });
        let file_ahead = for_file.is_some();
        let projected = for_file.as_ref().unwrap_or_else(|| document.project());
        let unavailable = outcome
            .unavailable
            .into_iter()
            .filter_map(|id| {
                let (_, _, device) = projected.find_device(id)?;
                Some((id, device.state.is_some()))
            })
            .collect::<Vec<_>>();
        let written = match (for_file, &rename) {
            (Some(mut project), rename) => {
                if let Some(name) = rename {
                    project.name = name.clone();
                }
                save_project_reporting(&path, &project)
            }
            (None, Some(name)) => {
                let mut renamed = Project::clone(document.project());
                renamed.name = name.clone();
                save_project_reporting(&path, &renamed)
            }
            (None, None) => save_project_reporting(&path, document.project()),
        };
        match written {
            Ok(not_durable) => {
                let rename_error = rename.and_then(|name| {
                    document
                        .apply_untracked(|project| {
                            komponist_model::edits::set_project_prop(
                                project,
                                ProjectProp::Name(name.clone()),
                            )
                        })
                        .err()
                });
                if file_ahead {
                    // The file holds a running take or plugin state still waiting on a gesture.
                    document.forget_saved();
                } else {
                    document.mark_saved();
                }
                self.session.accept_saved_project(document, path.clone());
                if let Some(error) = rename_error {
                    self.record_edit_error(error);
                }
                self.recovery_ui.save_asks = false;
                self.set_status(
                    self.lang
                        .project_saved(crate::dialogs::format::tilde(&path)),
                    StatusKind::Success,
                );
                self.remember(path);
                self.recordings_saved(recordings);
                self.report_missing_save_snapshots(&unavailable);
                if let Some(error) = not_durable {
                    self.report_save_not_durable(&error);
                }
                true
            }
            Err(error) => self.report_project_save_error(error),
        }
    }

    /// Reports whether a save retained the document's stored plug-in state or had no state.
    fn report_missing_save_snapshots(&mut self, devices: &[(komponist_model::DeviceId, bool)]) {
        let mut retained = Vec::new();
        let mut missing = Vec::new();
        for &(device, has_state) in devices {
            let names = if has_state {
                &mut retained
            } else {
                &mut missing
            };
            names.push(self.device_name(device));
        }
        let mut warnings = Vec::new();
        if !missing.is_empty() {
            warnings.push(
                self.lang
                    .project_save_plugin_missing_state(missing.join(", ")),
            );
        }
        if !retained.is_empty() {
            warnings.push(self.lang.project_save_plugin_cached(retained.join(", ")));
        }
        if !warnings.is_empty()
            && let Some(status) = &mut self.ui.status
        {
            // The status bar clips from the right; the project path can be arbitrarily long.
            warnings.push(std::mem::take(&mut status.text));
            status.text = warnings.join(" · ");
            status.kind = StatusKind::Error;
        }
    }

    /// The project file is published, but the folder entry was not flushed to disk: the save
    /// counts as done (the recordings it refers to stay), and the status says so.
    fn report_save_not_durable(&mut self, error: &std::io::Error) {
        eprintln!("komponist: project not flushed to disk: {error}");
        let warning = self.lang.project_save_not_durable();
        if let Some(status) = &mut self.ui.status {
            status.text = format!("{} · {warning}", status.text);
            status.kind = StatusKind::Error;
        }
    }

    fn report_project_save_error(&mut self, error: ProjectIoError) -> bool {
        eprintln!("komponist: {error}");
        self.set_status(
            self.lang
                .project_save_failed(project_io_reason(&error, self.lang)),
            StatusKind::Error,
        );
        false
    }

    /// Puts `path` at the front of the recent list and saves the list.
    pub(crate) fn remember(&mut self, path: PathBuf) {
        self.recent.add(path);
        self.dialogs.start.invalidate();
        self.save_recent();
    }

    /// Removes `path` from the recent list (start screen Entf) and saves the list.
    pub(crate) fn forget(&mut self, path: &Path) {
        if self.recent.remove(path) {
            self.save_recent();
        }
    }

    fn save_recent(&mut self) {
        let Some(file) = self.recent_path.clone() else {
            return;
        };
        if let Err(e) = self.recent.save(&file) {
            eprintln!(
                "komponist: recent projects not saved: {}: {e}",
                file.display()
            );
        }
    }

    /// Opens the file browser for `purpose` in the project's folder (else the last one, else the
    /// home folder).
    pub(crate) fn open_file_browser(&mut self, purpose: Purpose) {
        // A recovered project whose folder is gone starts elsewhere (crash recovery, variant d).
        let dir = self
            .project_path()
            .and_then(Path::parent)
            .filter(|dir| dir.is_dir())
            .map(Path::to_path_buf)
            .or_else(|| self.dialogs.last_dir.clone())
            .or_else(crate::dirs::home_dir)
            .unwrap_or_else(crate::dirs::filesystem_root);
        let name = match purpose {
            Purpose::Save => format!("{}.{FILE_EXTENSION}", self.project().name),
            _ => String::new(),
        };
        self.open_overlay(Overlay::File(FileBrowser::new(
            self.lang, purpose, dir, name,
        )));
    }

    /// Shows `overlay` above everything; the keyboard focus goes to it.
    pub(crate) fn open_overlay(&mut self, overlay: Overlay) {
        if self.dialogs.overlay.is_none() {
            self.dialogs.overlay_focus = Some(self.ui.focus);
        }
        self.dialogs.overlay = Some(overlay);
        self.ui.focus = FocusArea::Dialog;
    }

    /// Closes the overlay (if any) and gives the focus back; `true` if one was open. A pending
    /// step is dropped (the user cancelled).
    pub(crate) fn close_overlay(&mut self) -> bool {
        if matches!(
            self.dialogs.after_param_text,
            Some(AfterParamText::SaveAnswer | AfterParamText::SaveFile(_))
        ) {
            self.dialogs.after_param_text = None;
        }
        let Some(overlay) = self.dialogs.overlay.take() else {
            return false;
        };
        if let Overlay::File(b) = &overlay {
            self.dialogs.last_dir = Some(b.dir.clone());
        }
        self.dialogs.pending = None;
        if let Some(focus) = self.dialogs.overlay_focus.take() {
            self.ui.focus = focus;
        }
        self.open_queued_dialog();
        true
    }

    /// The unsaved-changes prompt was answered.
    pub(crate) fn answer_unsaved(&mut self, answer: Answer) {
        if answer == Answer::Save && self.param_undo.has_pending_text() {
            self.dialogs.after_param_text = Some(AfterParamText::SaveAnswer);
            return;
        }
        let pending = self.dialogs.pending.take();
        self.close_overlay();
        let Some(next) = pending else {
            return;
        };
        match answer {
            // A running take records on.
            Answer::Cancel => {}
            Answer::Discard => {
                if self.recording.active() {
                    self.discard_take();
                }
                self.proceed(next);
            }
            // A running take ends as Stop would and is saved with the project; if the save fails,
            // the take stays in the document and the project stays open.
            Answer::Save => {
                self.stop_take();
                match self.project_path().map(Path::to_path_buf) {
                    // As Ctrl S: a recovered copy whose file is gone asks for a place first.
                    Some(path) if !self.recovery_ui.save_asks => {
                        if self.save_to(&path) {
                            self.proceed(next);
                        }
                    }
                    _ => {
                        self.open_file_browser(Purpose::Save);
                        self.dialogs.pending = Some(next);
                    }
                }
            }
        }
    }

    /// The file browser chose `path`.
    pub(crate) fn file_chosen(&mut self, purpose: Purpose, path: PathBuf) {
        if purpose == Purpose::Save && self.param_undo.has_pending_text() {
            self.dialogs.after_param_text = Some(AfterParamText::SaveFile(path));
            return;
        }
        let pending = self.dialogs.pending.take();
        self.close_overlay();
        // The next file browser starts where this file is.
        let folder = path.parent();
        self.dialogs.last_dir = folder.map(Path::to_path_buf);
        match purpose {
            Purpose::Open => self.open_path(path),
            Purpose::Save => {
                let saved = self.save_to(&path);
                // "Sammeln und speichern" of a project that had no file yet: now it has one.
                if saved && std::mem::take(&mut self.dialogs.samples.collect_after_save) {
                    self.collect_and_save();
                }
                if saved && let Some(next) = pending {
                    self.proceed(next);
                }
            }
            Purpose::OpenPreset(device) => self.load_user_preset_file(device, path),
            Purpose::SavePreset(device) => self.save_user_preset_file(device, path),
            Purpose::PluginAsset { device, slot, .. } => {
                self.set_plugin_asset(device, slot, Some(&path));
            }
            Purpose::ReplaceAudio(clip) => self.replace_audio_path(clip, path),
            Purpose::ImportAudio => self.audio_import_chosen(vec![path]),
            Purpose::ExportFolder => self.export_folder_chosen(path),
            Purpose::Folder => {
                let (clap, vst3) = crate::dialogs::settings::folder_formats(&path);
                for (is_clap, add) in [(true, clap), (false, vst3)] {
                    if add {
                        self.dispatch(Action::Settings(SettingsAction::AddPluginDir {
                            clap: is_clap,
                            path: path.clone(),
                        }));
                    }
                }
            }
        }
    }
}

/// The file name of `path` for messages.
fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |f| f.to_string_lossy().into_owned(),
    )
}

impl App {
    /// Relink without copying a file. Failed decoding or a source offset beyond its end keeps
    /// both the document and undo history untouched.
    pub(crate) fn replace_audio_path(&mut self, clip: komponist_model::ClipId, path: PathBuf) {
        let Some((_, before)) = self.project().find_clip(clip) else {
            return;
        };
        let komponist_model::ClipContent::Audio(a) = &before.content else {
            return;
        };
        let old_length = before.length;
        let name = before.name.clone();
        let offset = a.offset;
        let Ok(source) = komponist_audiofile::FilePool::global().open(&path) else {
            self.set_status(
                self.lang
                    .audio_file_replace_failed(self.lang.audio_file_no_audio()),
                StatusKind::Error,
            );
            return;
        };
        let info = source.info();
        if offset >= info.frames {
            self.set_status(
                self.lang
                    .audio_file_replace_failed(self.lang.audio_file_too_short()),
                StatusKind::Error,
            );
            return;
        }
        let file = komponist_model::AudioFile::new(
            &path,
            self.project_dir().as_deref(),
            info.frames,
            info.sample_rate,
            info.channels as u16,
        );
        self.dispatch(Action::Edit(crate::EditAction::SetAudioClipFile {
            clip,
            file,
        }));
        if self
            .project()
            .find_clip(clip)
            .is_some_and(|(_, c)| c.length < old_length)
        {
            self.set_status(
                self.lang.audio_file_replace_shortened(name),
                StatusKind::Info,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;
    use komponist_session::project_io::{save_project, set_directory_sync_failure_for_test};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komponist-project-io-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn reads_a_localized_project_summary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Night Drive.komponist");
        let project = demo_project();
        save_project(&path, &project).unwrap();
        let summary = ProjectSummary::read(&path).unwrap();
        assert_eq!(summary.name, "Night Drive");
        assert_eq!(summary.tracks, project.tracks.len());
        assert_eq!(summary.hues[0], project.tracks[0].hue.0);
        assert_eq!(summary.meta(komponist_i18n::Lang::De), "124 BPM · 6 Spuren");
        assert!(summary.modified.is_some());
    }

    #[test]
    fn plugin_asset_links_rebase_after_success_and_collect_by_content() {
        use komponist_model::edits::{self, NewTrack};
        use komponist_model::plugin_assets::AssetLink;
        use komponist_model::{PluginFormat, PluginRef};

        let scratch = tempfile::tempdir().unwrap();
        let old_dir = scratch.path().join("original");
        let new_dir = scratch.path().join("moved");
        std::fs::create_dir(&old_dir).unwrap();
        std::fs::create_dir(&new_dir).unwrap();
        let source = old_dir.join("configuration.txt");
        let same = old_dir.join("same.txt");
        std::fs::write(&source, b"data to collect").unwrap();
        std::fs::copy(&source, &same).unwrap();
        std::fs::write(new_dir.join("configuration.txt"), b"unrelated").unwrap();
        let plugin = PluginRef {
            format: PluginFormat::Clap,
            id: "com.example.assets".into(),
            name: "Asset fixture".into(),
            vendor: String::new(),
            version: "1".into(),
            module: Box::default(),
        };
        let (project, track) = edits::add_track(
            &Project::new("Original"),
            NewTrack {
                name: None,
                hue: None,
                instrument: Some(plugin),
                index: None,
                arm_exclusive: false,
            },
        )
        .unwrap();
        let device = project
            .track(track)
            .unwrap()
            .channel
            .instrument()
            .unwrap()
            .1
            .id;
        let links = [
            (0, AssetLink::new(&source, Some(&old_dir))),
            (1, AssetLink::new(&same, Some(&old_dir))),
        ]
        .into_iter()
        .collect();
        let project = edits::set_device_asset_links(&project, device, links).unwrap();
        let mut app = App::headless(project);
        assert!(app.save_to(&old_dir.join("Original.komponist")));
        let destination = new_dir.join("Saved.komponist");
        std::fs::create_dir(&destination).unwrap();
        assert!(!app.save_to(&destination));
        assert_eq!(
            app.project().find_device(device).unwrap().2.asset_links[&0].rel,
            Path::new("configuration.txt")
        );
        std::fs::remove_dir(&destination).unwrap();
        assert!(app.save_to(&destination));
        let saved = load_project(&destination).unwrap();
        let links = &saved.find_device(device).unwrap().2.asset_links;
        assert_eq!(links[&0].resolve(Some(&new_dir)), Some(source.clone()));

        let mut collector = Collector::new(&destination);
        assert!(app.save_to_collected(&destination, &mut collector));
        assert_eq!(collector.unapproved_assets, [source.clone(), same.clone()]);
        assert!(!collector.samples_dir().exists());

        app.set_plugin_asset(device, 0, Some(&source));
        app.set_plugin_asset(device, 1, Some(&same));
        let mut collector = Collector::new(&destination);
        assert!(app.save_to_collected(&destination, &mut collector));
        assert!(collector.unapproved_assets.is_empty());
        let placed = collector.placed_asset(&source).unwrap();
        assert_eq!(collector.placed_asset(&same), Some(placed.clone()));
        assert_eq!(std::fs::read(&placed).unwrap(), b"data to collect");
        assert!(source.is_file() && same.is_file());
    }

    #[test]
    fn save_as_only_rebases_clip_and_plugin_file_links_after_the_write() {
        use komponist_model::edits::{self, NewAudioTrack, NewClip, NewClipContent};
        use komponist_model::plugin_assets::AssetLink;
        use komponist_model::{AudioFile, ClipContent, Tick};

        let dir = scratch("save-as-links");
        let old_dir = dir.join("old");
        let new_dir = old_dir.join("new");
        std::fs::create_dir_all(&new_dir).unwrap();
        // A link rebased as ../kick.wav would resolve to the wrong file from the old folder.
        let old_sample = old_dir.join("kick.wav");
        for (path, value) in [(&old_sample, 100_i16), (&dir.join("kick.wav"), 200)] {
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
            for _ in 0..480 {
                wav.write_sample(value).unwrap();
            }
            wav.finalize().unwrap();
        }
        let project = demo_project();
        let sampler = project.tracks[0].channel.devices[0].id;
        let state = komponist_model::project::SavedPluginState {
            bytes: b"opaque plugin state".as_slice().into(),
            parameter_bases: [(7, 0.25)].into(),
        };
        let project = edits::set_saved_plugin_state(&project, sampler, &state).unwrap();
        let link = AssetLink::new(&old_sample, Some(&old_dir));
        let project = edits::set_device_asset_links(&project, sampler, [(0, link)].into()).unwrap();
        let (project, track) = edits::add_audio_track(&project, NewAudioTrack::default()).unwrap();
        let (project, clip) = edits::add_clip(
            &project,
            track,
            NewClip {
                name: "Kick".into(),
                start: Tick::ZERO,
                length: Tick(1),
                content: NewClipContent::Audio {
                    file: AudioFile::new(&old_sample, Some(&old_dir), 480, 48_000, 1),
                    offset: 0,
                },
            },
        )
        .unwrap();
        let mut app = App::headless(project);
        let old_project = old_dir.join("Song.komponist");
        assert!(app.save_to(&old_project));
        // Give the document both an undo and a redo snapshot containing the links.
        for tempo in [110.0, 115.0] {
            app.session
                .document_mut()
                .apply("Tempo", |p| {
                    edits::set_project_prop(
                        p,
                        ProjectProp::Tempo(komponist_model::Tempo::new(tempo)),
                    )
                })
                .unwrap();
        }
        assert!(app.session.document_mut().undo());

        let check_links = |project: &Project, directory: &Path, relative: &Path| {
            let (_, _, device) = project.find_device(sampler).unwrap();
            assert_eq!(device.parameter_bases, state.parameter_bases);
            assert_eq!(device.state.as_deref(), Some(&*state.bytes));
            let sample = &device.asset_links[&0];
            assert_eq!(sample.rel, relative);
            assert_eq!(
                sample.resolve(Some(directory)).as_deref(),
                Some(old_sample.as_path())
            );
            let (_, clip) = project.find_clip(clip).unwrap();
            let ClipContent::Audio(audio) = &clip.content else {
                panic!("audio clip");
            };
            assert_eq!(audio.file.rel, relative);
            assert_eq!(
                audio.file.resolve(Some(directory)).as_deref(),
                Some(old_sample.as_path())
            );
        };

        let destination = new_dir.join("Copy.komponist");
        std::fs::create_dir(&destination).unwrap();
        let revision = app.session.document().revision();
        assert!(!app.save_to(&destination));
        assert_eq!(app.session.document().revision(), revision);
        assert_eq!(app.project_path(), Some(old_project.as_path()));
        check_links(app.project(), &old_dir, Path::new("kick.wav"));
        assert!(app.session.document_mut().undo());
        check_links(app.project(), &old_dir, Path::new("kick.wav"));
        assert!(app.session.document_mut().redo());
        assert!(app.session.document_mut().redo());
        check_links(app.project(), &old_dir, Path::new("kick.wav"));
        assert!(app.session.document_mut().undo());

        std::fs::remove_dir(&destination).unwrap();
        assert!(app.save_to(&destination));
        assert_eq!(app.project_path(), Some(destination.as_path()));
        let relative = Path::new("../kick.wav");
        check_links(&load_project(&destination).unwrap(), &new_dir, relative);
        check_links(app.project(), &new_dir, relative);
        assert!(!app.session.document().is_dirty());
        assert!(app.session.document_mut().undo());
        check_links(app.project(), &new_dir, relative);
        assert!(app.session.document_mut().redo());
        assert!(app.session.document_mut().redo());
        check_links(app.project(), &new_dir, relative);
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unreadable_files_say_why() {
        let dir = scratch("errors");
        std::fs::create_dir_all(&dir).unwrap();
        let missing = load_project(&dir.join("fehlt.komponist")).unwrap_err();
        assert_eq!(
            project_io_reason(&missing, Lang::De),
            "Datei nicht gefunden"
        );
        assert_eq!(project_io_reason(&missing, Lang::En), "file not found");
        let other = dir.join("notiz.komponist");
        std::fs::write(&other, r#"{"format":"anders"}"#).unwrap();
        assert_eq!(
            project_io_reason(&load_project(&other).unwrap_err(), Lang::De),
            "keine komponist-Projektdatei"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tempo_without_trailing_zeros() {
        assert_eq!(fmt_bpm(124.0), "124");
        assert_eq!(fmt_bpm(124.5), "124.5");
        assert_eq!(fmt_bpm(98.25), "98.25");
    }

    #[test]
    fn the_recent_list_is_next_to_settings() {
        assert_eq!(
            recent_path(Path::new("/c/komponist/settings.json")),
            Path::new("/c/komponist/recent.json")
        );
    }

    #[test]
    fn a_failed_directory_sync_keeps_the_recordings_the_project_refers_to() {
        let dir = scratch("sync-fail-recordings");
        std::fs::create_dir_all(dir.join("prov")).unwrap();
        let take = dir.join("prov/Take 1.wav");
        std::fs::write(&take, b"RIFF").unwrap();
        let mut app = App::headless(Project::new("Durable"));
        app.track_provisional_audio(take);
        let path = dir.join("Durable.komponist");
        set_directory_sync_failure_for_test(true);
        let saved = app.save_to(&path);
        set_directory_sync_failure_for_test(false);
        assert!(saved, "a published project counts as saved");
        assert!(path.exists());
        if cfg!(unix) {
            assert!(
                dir.join("Takes/Take 1.wav").exists(),
                "the take the saved project refers to must survive"
            );
            let status = app.ui.status.as_ref().unwrap();
            let warning = app.lang.project_save_not_durable();
            assert!(status.text.ends_with(warning), "{}", status.text);
        }
    }

    #[test]
    fn a_broken_recent_list_survives_remember() {
        let dir = scratch("recent-broken");
        std::fs::create_dir_all(&dir).unwrap();
        let settings = dir.join("settings.json");
        let recent = recent_path(&settings);
        std::fs::write(&recent, "{ kaputt").unwrap();
        let mut app = App::new(
            Document::new(Project::new("Recent")),
            komponist_session::engine_sync::EngineSync::null_with_midi(
                Box::new(komponist_session::device_host::PluginHost::headless_fixture()),
                komponist_audio::MidiReceiver::disconnected(),
            ),
            crate::settings::Settings::default(),
            Some(settings),
        );
        assert!(app.recent().is_empty());
        app.remember(PathBuf::from("/p/a.komponist"));
        // The session keeps working with an in-memory list; the file is untouched.
        assert_eq!(app.recent(), [PathBuf::from("/p/a.komponist")]);
        assert_eq!(std::fs::read_to_string(&recent).unwrap(), "{ kaputt");
    }

    #[test]
    fn the_recent_list_is_unique_bounded_and_persisted() {
        let mut list = RecentList::default();
        for i in 0..20 {
            list.add(PathBuf::from(format!("/p/{i}.komponist")));
        }
        list.add(PathBuf::from("/p/15.komponist"));
        assert_eq!(list.projects.len(), RECENT_LIMIT);
        assert_eq!(list.projects[0], Path::new("/p/15.komponist"));
        assert_eq!(
            list.projects
                .iter()
                .filter(|p| p.as_path() == Path::new("/p/15.komponist"))
                .count(),
            1
        );
        assert!(list.remove(Path::new("/p/15.komponist")));
        assert!(!list.remove(Path::new("/p/15.komponist")));

        let dir = scratch("recent");
        let path = dir.join(RECENT_FILE);
        assert_eq!(RecentList::load(&path).unwrap(), RecentList::default());
        list.save(&path).unwrap();
        assert_eq!(RecentList::load(&path).unwrap(), list);
        std::fs::write(&path, "{ kaputt").unwrap();
        assert_eq!(
            RecentList::load(&path).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData,
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
