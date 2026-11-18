//! Project files and the recent list (ARCHITECTURE §5.5), and the project lifecycle of the app:
//! new (S10), open (with the S06 check), save, save as, the recent list of the start screen (S09).
//!
//! - `*.komponist` files are read and written with `komponist_model::persist`; writing goes
//!   through a temporary file next to the target and a rename, so a crash never leaves half a
//!   project.
//! - The recent list lives in `recent.json` next to the settings file
//!   (`~/.config/komponist/recent.json`, on macOS in `~/Library/Application Support/komponist/`, on Windows in `%APPDATA%\komponist\`), newest first, at most [`RECENT_LIMIT`] entries.
//! - Opening or creating a project resets the engine ([`crate::engine_sync::EngineSync::reset`]).
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
//! Thread: UI thread (files are small; reading the recent projects' summaries happens once when
//! the start screen shows).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use komponist_i18n::Lang;
use komponist_model::edits::ProjectProp;
use komponist_model::persist::{self, FILE_EXTENSION, PersistError};
use komponist_model::{Document, Project};

use crate::actions::{Action, ProjectAction, SettingsAction, UiAction};
use crate::app::App;
use crate::dialogs::file::{FileBrowser, Purpose};
use crate::dialogs::new_project::{self, Answer};
use crate::dialogs::{Overlay, Pending};
use crate::platform::Platform;
use crate::ui_state::{Dialog, FocusArea, Screen, SettingsSection, StatusKind};

/// File name of the recent list inside the config directory.
pub const RECENT_FILE: &str = "recent.json";

/// Most entries kept in the recent list.
pub const RECENT_LIMIT: usize = 12;

/// Why a project file or the recent list could not be read or written.
#[derive(Debug)]
pub enum ProjectIoError {
    /// Reading or writing failed.
    Io(PathBuf, std::io::Error),
    /// Not a project file, or one this build cannot read.
    Format(PathBuf, PersistError),
    /// The recent list is not valid JSON.
    Recent(PathBuf, serde_json::Error),
}

impl core::fmt::Display for ProjectIoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProjectIoError::Io(path, e) => write!(f, "{}: {e}", path.display()),
            ProjectIoError::Format(path, e) => write!(f, "{}: {e}", path.display()),
            ProjectIoError::Recent(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for ProjectIoError {}

impl ProjectIoError {
    /// The reason for the status bar in `t`'s language ("Datei nicht gefunden"). The system's own
    /// text is not shown (it is English); [`App`] logs the error itself.
    pub fn reason(&self, t: Lang) -> String {
        match self {
            ProjectIoError::Io(_, e) if e.kind() == std::io::ErrorKind::NotFound => {
                t.project_reason_not_found().to_owned()
            }
            ProjectIoError::Io(_, e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                t.project_reason_denied().to_owned()
            }
            ProjectIoError::Io(..) => t.project_reason_io().to_owned(),
            ProjectIoError::Format(_, PersistError::NotAProject) => {
                t.project_reason_not_project().to_owned()
            }
            ProjectIoError::Format(_, PersistError::TooNew { found }) => {
                t.project_reason_too_new(found)
            }
            ProjectIoError::Format(_, PersistError::Migration { .. }) => {
                t.project_reason_migration().to_owned()
            }
            ProjectIoError::Format(..) | ProjectIoError::Recent(..) => {
                t.project_reason_damaged().to_owned()
            }
        }
    }
}

/// Reads a project file.
pub fn load_project(path: &Path) -> Result<Project, ProjectIoError> {
    let text = std::fs::read_to_string(path).map_err(|e| ProjectIoError::Io(path.to_owned(), e))?;
    persist::from_json(&text).map_err(|e| ProjectIoError::Format(path.to_owned(), e))
}

/// Writes `project` to `path` through a temporary file next to it and a rename. The folder must
/// exist: saving never creates folders (a mistyped name must not leave folders behind).
pub fn save_project(path: &Path, project: &Project) -> Result<(), ProjectIoError> {
    let json = persist::to_json(project).map_err(|e| ProjectIoError::Format(path.to_owned(), e))?;
    write_atomic(path, &json, false)
}

/// Writes `text` to `path` via `<path>.tmp` and a rename; `create_dir` creates the folder first
/// (the config folder of the recent list).
fn write_atomic(path: &Path, text: &str, create_dir: bool) -> Result<(), ProjectIoError> {
    let io = |e| ProjectIoError::Io(path.to_owned(), e);
    if create_dir && let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text).map_err(io)?;
    std::fs::rename(&tmp, path).map_err(io)
}

/// `path` with the `.komponist` extension (added if it has another one or none).
pub fn with_extension(path: &Path) -> PathBuf {
    if path.extension().is_some_and(|e| e == FILE_EXTENSION) {
        path.to_owned()
    } else {
        let mut p = path.as_os_str().to_owned();
        p.push(".");
        p.push(FILE_EXTENSION);
        PathBuf::from(p)
    }
}

/// Whether `path` names a project file (by its extension).
pub fn is_project_file(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == FILE_EXTENSION)
}

/// The project name a file gives: its stem ("Hafenlicht" for `Hafenlicht.komponist`).
pub fn name_from_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_string_lossy().trim().to_owned();
    (!stem.is_empty()).then_some(stem)
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
    pub fn load(path: &Path) -> Result<RecentList, ProjectIoError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| ProjectIoError::Recent(path.to_owned(), e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(RecentList::default()),
            Err(e) => Err(ProjectIoError::Io(path.to_owned(), e)),
        }
    }

    /// Writes the list to `path` (pretty JSON, atomic).
    pub fn save(&self, path: &Path) -> Result<(), ProjectIoError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| ProjectIoError::Recent(path.to_owned(), e))?;
        write_atomic(path, &text, true)
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

impl App {
    /// New, open, save, save as and the S06 answers (`ProjectAction`, module docs).
    pub(crate) fn lifecycle_action(&mut self, action: ProjectAction) {
        match action {
            ProjectAction::New => self.guard(Pending::New),
            ProjectAction::Open => self.guard(Pending::Open),
            ProjectAction::Quit => self.guard(Pending::Quit),
            ProjectAction::OpenPath(path) => self.guard(Pending::OpenPath(path)),
            ProjectAction::Save => match self.project_path.clone() {
                Some(path) => {
                    self.save_to(&path);
                }
                None => self.open_file_browser(Purpose::Save),
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
            && self
                .project_path
                .as_deref()
                .is_some_and(|open| same(open, &path))
        {
            return;
        }
        self.dispatch(Action::Project(ProjectAction::OpenPath(path)));
    }

    /// Whether leaving the project loses something: unsaved changes or a running take with notes.
    fn has_unsaved(&self) -> bool {
        self.doc.is_dirty() || self.has_unsaved_take()
    }

    /// `next` replaces the open project: asks first if it has unsaved changes.
    fn guard(&mut self, next: Pending) {
        if self.ui.screen == Screen::Project && self.has_unsaved() {
            self.dialogs.pending = Some(next);
            self.open_overlay(Overlay::Unsaved);
        } else {
            self.proceed(next);
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
                let project = new_project::template(self.lang, crate::app::hexa_ref());
                self.open_document(Document::new(project), None);
                self.check_missing_plugins();
            }
            Pending::StartScreen => self.show_start_screen(),
            Pending::Open => self.open_file_browser(Purpose::Open),
            Pending::OpenPath(path) => self.open_path(path),
            Pending::Quit => {
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

    /// Opens the project file at `path` (no questions): resets the engine, remembers the file,
    /// checks the plugins (S06). A file that cannot be read leaves everything as it was and says
    /// why.
    pub(crate) fn open_path(&mut self, path: PathBuf) {
        match load_project(&path) {
            Ok(project) => {
                self.open_document(Document::new(project), Some(path.clone()));
                self.remember(path);
                self.check_missing_plugins();
            }
            Err(e) => {
                let file = file_name(&path);
                eprintln!("komponist: {e}");
                self.set_status(
                    self.lang.project_open_failed(&file, e.reason(self.lang)),
                    StatusKind::Error,
                );
            }
        }
    }

    /// S06: shows the missing plugins of the open project, or says it opened. While a scan runs
    /// the check waits for its end (`App::handle_device_event`).
    pub(crate) fn check_missing_plugins(&mut self) {
        let name = self.project().name.clone();
        if self.engine.host().is_scanning() {
            self.dialogs.missing_check = true;
            self.set_status(self.lang.project_opened(&name), StatusKind::Info);
            return;
        }
        self.dialogs.missing_check = false;
        let missing = self.engine.host().missing_plugins(self.project());
        let samples = crate::dialogs::missing_plugins::missing_samples(
            self.project(),
            self.project_dir().as_deref(),
        )
        .len();
        let audio = crate::dialogs::missing_plugins::missing_audio(
            self.project(),
            self.project_dir().as_deref(),
        );
        if missing.is_empty() && samples == 0 && audio.is_empty() {
            self.set_status(self.lang.project_opened_ready(&name), StatusKind::Info);
            return;
        }
        let n = if audio.is_empty() {
            crate::dialogs::missing_plugins::title_for(self.lang, missing.len(), samples)
        } else if missing.is_empty() {
            self.lang.missing_audio_files().to_owned()
        } else {
            self.lang.missing_plugins_and_files().to_owned()
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
        let path = with_extension(path);
        if !self.await_audio_take() {
            return false;
        }
        if self.migrate_recordings_for_save(&path).is_err() {
            return false;
        }
        // The bridged plugins' current states go into the file (blocking, bounded).
        for (device, state) in self
            .engine
            .host_mut()
            .snapshot_states(crate::app::SNAPSHOT_TIMEOUT)
        {
            self.store_snapshot_for_save(device, &state);
        }
        // The sample links get the paths for this file's folder.
        self.rebase_samples(&path);
        // A running take so far, with the notes still in flight, on top of all that (#85).
        let with_take = match self.project_with_take() {
            Ok(project) => project,
            Err(()) => return false,
        };
        // A new file renames the project after it; the rename happens only if writing works.
        let rename = name_from_path(&path).filter(|name| {
            self.project_path.as_deref() != Some(path.as_path()) && *name != self.project().name
        });
        let take_saved = with_take.is_some();
        let written = match (with_take, &rename) {
            (Some(mut project), rename) => {
                if let Some(name) = rename {
                    project.name = name.clone();
                }
                save_project(&path, &project)
            }
            (None, Some(name)) => {
                let mut renamed = Project::clone(self.project());
                renamed.name = name.clone();
                save_project(&path, &renamed)
            }
            (None, None) => save_project(&path, self.project()),
        };
        match written {
            Ok(()) => {
                if let Some(name) = rename {
                    self.untracked(ProjectProp::Name(name));
                }
                if take_saved {
                    // The file holds the take, the document does not yet.
                    self.doc.forget_saved();
                } else {
                    self.doc.mark_saved();
                }
                self.project_path = Some(path.clone());
                let dir = self.project_dir();
                self.engine.host_mut().set_project_dir(dir);
                self.set_status(
                    self.lang
                        .project_saved(crate::dialogs::format::tilde(&path)),
                    StatusKind::Success,
                );
                self.remember(path);
                self.recordings_saved();
                true
            }
            Err(e) => {
                eprintln!("komponist: {e}");
                self.set_status(
                    self.lang.project_save_failed(e.reason(self.lang)),
                    StatusKind::Error,
                );
                false
            }
        }
    }

    /// Puts `path` at the front of the recent list and saves the list.
    fn remember(&mut self, path: PathBuf) {
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
            eprintln!("komponist: recent projects not saved: {e}");
        }
    }

    /// Opens the file browser for `purpose` in the project's folder (else the last one, else the
    /// home folder).
    pub(crate) fn open_file_browser(&mut self, purpose: Purpose) {
        let dir = self
            .project_path
            .as_deref()
            .and_then(Path::parent)
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
        true
    }

    /// The unsaved-changes prompt was answered.
    pub(crate) fn answer_unsaved(&mut self, answer: Answer) {
        let pending = self.dialogs.pending.take();
        self.close_overlay();
        let Some(next) = pending else {
            return;
        };
        match answer {
            // A running take records on.
            Answer::Cancel => {}
            Answer::Discard => {
                if self.recording.is_some() || self.audio_recording.is_active() {
                    self.discard_take();
                }
                self.proceed(next);
            }
            // A running take ends as Stop would and is saved with the project; if the save fails,
            // the take stays in the document and the project stays open.
            Answer::Save => {
                self.stop_take();
                match self.project_path.clone() {
                    Some(path) => {
                        if self.save_to(&path) {
                            self.proceed(next);
                        }
                    }
                    None => {
                        self.open_file_browser(Purpose::Save);
                        self.dialogs.pending = Some(next);
                    }
                }
            }
        }
    }

    /// The file browser chose `path`.
    pub(crate) fn file_chosen(&mut self, purpose: Purpose, path: PathBuf) {
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
            Purpose::Sample => self.sample_chosen(path),
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "komponist-project-io-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn projects_round_trip_through_files() {
        let dir = scratch("round-trip");
        let path = dir.join("sub/Night Drive.komponist");
        let project = demo_project();
        // Saving never creates folders.
        assert!(matches!(
            save_project(&path, &project),
            Err(ProjectIoError::Io(..))
        ));
        assert!(!dir.join("sub").exists());
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        save_project(&path, &project).unwrap();
        assert!(!dir.join("sub/Night Drive.komponist.tmp").exists());
        assert_eq!(load_project(&path).unwrap(), project);
        let summary = ProjectSummary::read(&path).unwrap();
        assert_eq!(summary.name, "Night Drive");
        assert_eq!(summary.tracks, project.tracks.len());
        assert_eq!(summary.hues[0], project.tracks[0].hue.0);
        assert_eq!(summary.meta(komponist_i18n::Lang::De), "124 BPM · 6 Spuren");
        assert!(summary.modified.is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_files_say_why() {
        let dir = scratch("errors");
        std::fs::create_dir_all(&dir).unwrap();
        let missing = load_project(&dir.join("fehlt.komponist")).unwrap_err();
        assert_eq!(missing.reason(Lang::De), "Datei nicht gefunden");
        assert_eq!(missing.reason(Lang::En), "file not found");
        let other = dir.join("notiz.komponist");
        std::fs::write(&other, r#"{"format":"anders"}"#).unwrap();
        assert_eq!(
            load_project(&other).unwrap_err().reason(Lang::De),
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
    fn extensions_and_names() {
        assert_eq!(
            with_extension(Path::new("/a/Hafenlicht")),
            Path::new("/a/Hafenlicht.komponist")
        );
        assert_eq!(
            with_extension(Path::new("/a/b.komponist")),
            Path::new("/a/b.komponist")
        );
        assert_eq!(
            with_extension(Path::new("/a/v1.2")),
            Path::new("/a/v1.2.komponist")
        );
        assert!(is_project_file(Path::new("x.komponist")));
        assert!(!is_project_file(Path::new("x.json")));
        assert_eq!(
            name_from_path(Path::new("/a/Hafenlicht.komponist")).as_deref(),
            Some("Hafenlicht")
        );
        assert_eq!(
            recent_path(Path::new("/c/komponist/settings.json")),
            Path::new("/c/komponist/recent.json")
        );
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
        assert!(matches!(
            RecentList::load(&path),
            Err(ProjectIoError::Recent(..))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
