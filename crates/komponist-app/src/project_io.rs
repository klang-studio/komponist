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
//! Explicit opens and writes run on the UI thread. The start screen reads summaries on a worker.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use komponist_i18n::Lang;
use komponist_model::asset_collection::Collector;
use komponist_model::edits::ProjectProp;
use komponist_model::persist::{self, FILE_EXTENSION, FORMAT_VERSION, PersistError};
use komponist_model::{Document, Project, Repairs};

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
            ProjectIoError::Format(_, PersistError::UnsupportedVersion { found }) => {
                if *found > FORMAT_VERSION {
                    t.project_reason_too_new(*found)
                } else {
                    t.project_reason_unsupported_version(*found)
                }
            }
            ProjectIoError::Format(..) | ProjectIoError::Recent(..) => {
                t.project_reason_damaged().to_owned()
            }
        }
    }
}

/// Reads a project file.
pub fn load_project(path: &Path) -> Result<Project, ProjectIoError> {
    load_project_repaired(path).map(|(project, _)| project)
}

/// Reads a project file and what loading had to repair in it (#209), see
/// [`komponist_model::validate`].
pub fn load_project_repaired(path: &Path) -> Result<(Project, Repairs), ProjectIoError> {
    let text = std::fs::read_to_string(path).map_err(|e| ProjectIoError::Io(path.to_owned(), e))?;
    persist::load(&text)
        .map(|loaded| (loaded.project, loaded.repairs))
        .map_err(|e| ProjectIoError::Format(path.to_owned(), e))
}

/// Writes `project` to `path` through a temporary file next to it and a rename. The folder must
/// exist: saving never creates folders (a mistyped name must not leave folders behind). A
/// failed durability flush after a successful publication is logged, not an error; use
/// [`save_project_reporting`] to see it.
pub fn save_project(path: &Path, project: &Project) -> Result<(), ProjectIoError> {
    save_project_reporting(path, project).map(|not_durable| {
        if let Some(error) = not_durable {
            eprintln!(
                "komponist: {}: not flushed to disk: {error}",
                path.display()
            );
        }
    })
}

/// [`save_project`] that returns the durability failure of a file that *was* published: `Ok(Some(_))`
/// means the new file is in place but the folder entry may not survive a power loss.
pub fn save_project_reporting(
    path: &Path,
    project: &Project,
) -> Result<Option<std::io::Error>, ProjectIoError> {
    let json = persist::to_json(project).map_err(|e| ProjectIoError::Format(path.to_owned(), e))?;
    write_atomic(path, &json, false).map_err(|error| ProjectIoError::Io(path.to_owned(), error))
}

/// Writes and closes a file in an exclusive staging directory on the destination filesystem.
/// Renaming publishes the whole file at once, and dropping the temporary directory cleans up
/// staging after either a success or an error. Only config writers create a missing parent.
///
/// `Err` means nothing was published. `Ok(Some(error))` means the file *was* published but making
/// the rename durable (the parent folder's fsync, Unix) failed: callers keep what the file refers
/// to and may warn.
pub(crate) fn write_atomic(
    path: &Path,
    text: &str,
    create_dir: bool,
) -> std::io::Result<Option<std::io::Error>> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if create_dir {
        create_dirs_durably(parent)?;
    }

    let mut builder = tempfile::Builder::new();
    builder.prefix(".komponist-save-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let staging = builder.tempdir_in(parent)?;
    let staged_file = staging.path().join("document");
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&staged_file)?;
    file.write_all(text.as_bytes())?;
    // The data must be on disk before the rename publishes it: on XFS and btrfs a power loss
    // after a rename without this can leave a zero-length file in place of the good one.
    file.sync_all()?;
    drop(file);
    durable_rename(&staged_file, path, parent)
}

/// Folders whose entry lists hold a new folder but were not flushed yet: an earlier
/// [`create_dirs_durably`] created the folder and then failed to flush its parent. A later call
/// for a path below such a folder flushes it again, although the new folder exists by then (#221,
/// review of #257). Only failures add entries, and a successful flush removes them.
static UNFLUSHED: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// `create_dir_all(dir)`, then (Unix) flushes the parent folder of every folder it had to
/// create, up to the first existing ancestor, so the new folders themselves survive a power loss.
/// Flushes that failed on an earlier call for an ancestor of `dir` are retried here.
pub(crate) fn create_dirs_durably(dir: &Path) -> std::io::Result<()> {
    let missing: Vec<PathBuf> = dir
        .ancestors()
        .take_while(|path| !path.exists())
        .map(Path::to_path_buf)
        .collect();
    let created = std::fs::create_dir_all(dir);
    let mut unflushed = UNFLUSHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Also after a failed `create_dir_all`: the folders it did create stay.
    for new in missing.iter().filter(|path| path.is_dir()) {
        if let Some(parent) = containing_dir(new)
            && !unflushed.iter().any(|folder| folder == parent)
        {
            unflushed.push(parent.to_path_buf());
        }
    }
    created?;
    let mut due: Vec<PathBuf> = unflushed
        .iter()
        .filter(|folder| {
            dir.starts_with(folder) || (folder.as_path() == Path::new(".") && dir.is_relative())
        })
        .cloned()
        .collect();
    // Outermost first; each new folder's entry lives in its parent.
    due.sort_by_key(|folder| folder.components().count());
    for folder in due {
        sync_directory(&folder)?;
        unflushed.retain(|pending| *pending != folder);
    }
    Ok(())
}

/// The folder whose entry list holds `path`: its parent, or `.` for a single relative component.
fn containing_dir(path: &Path) -> Option<&Path> {
    path.parent().map(|parent| {
        if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        }
    })
}

/// Flushes the entries of folder `dir`. Filesystems that reject fsync on a folder are fine.
pub(crate) fn sync_directory(dir: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if tests::FAIL_DIRECTORY_SYNC.with(std::cell::Cell::get) {
        return Err(std::io::Error::other("injected directory sync failure"));
    }
    #[cfg(test)]
    tests::SYNCED.with_borrow_mut(|synced| synced.push(dir.to_path_buf()));
    #[cfg(unix)]
    {
        match std::fs::File::open(dir).and_then(|dir| dir.sync_all()) {
            Err(error) if !directory_sync_unsupported(&error) => Err(error),
            _ => Ok(()),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// Renames `from` over `to` (both in `parent`) and makes the rename itself durable. The rename
/// publishes the file; a flush failure afterwards comes back as `Ok(Some(error))`.
#[cfg(unix)]
fn durable_rename(
    from: &Path,
    to: &Path,
    parent: &Path,
) -> std::io::Result<Option<std::io::Error>> {
    std::fs::rename(from, to)?;
    Ok(sync_directory(parent).err())
}

#[cfg(unix)]
fn directory_sync_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(code) if code == libc::EINVAL || code == libc::ENOTSUP || code == libc::EOPNOTSUPP
    )
}

/// Renames `from` over `to` and returns only once the move is flushed to disk
/// (`MOVEFILE_WRITE_THROUGH`); `std::fs::rename` replaces the target but does not wait.
#[cfg(windows)]
fn durable_rename(
    from: &Path,
    to: &Path,
    _parent: &Path,
) -> std::io::Result<Option<std::io::Error>> {
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let (from, to) = (windows_wide(from)?, windows_wide(to)?);
    // SAFETY: both buffers are NUL-terminated and outlive the call.
    let moved = unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(None)
    }
}

/// `path` as a NUL-terminated UTF-16 string for a Win32 call. Like `std::fs`, a path that does not
/// fit `MAX_PATH` becomes absolute and verbatim (`\\?\` or `\\?\UNC\`), which the raw API needs
/// without the long-path opt-in.
#[cfg(windows)]
fn windows_wide(path: &Path) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    const MAX_PATH: usize = 260;
    let units = |path: &Path| path.as_os_str().encode_wide().collect::<Vec<u16>>();
    let slash = u16::from(b'\\');
    if units(path).contains(&0) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains a NUL",
        ));
    }
    let mut wide = units(path);
    if !wide.starts_with(&[slash, slash, u16::from(b'?'), slash]) {
        // The limit applies to the resolved path: a short relative path from a long working
        // directory needs the prefix too.
        let absolute = units(&std::path::absolute(path)?);
        wide = if absolute.len() < MAX_PATH {
            absolute
        } else if absolute.starts_with(&[slash, slash]) {
            r"\\?\UNC\"
                .encode_utf16()
                .chain(absolute[2..].iter().copied())
                .collect()
        } else {
            r"\\?\".encode_utf16().chain(absolute).collect()
        };
    }
    wide.push(0);
    Ok(wide)
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

/// Resolve parent steps through the filesystem before using this file's folder as a link base.
fn resolve_project_parent(path: PathBuf) -> Result<PathBuf, ProjectIoError> {
    let Some(parent) = path.parent().filter(|parent| {
        parent
            .components()
            .any(|component| component == std::path::Component::ParentDir)
    }) else {
        return Ok(path);
    };
    let Some(name) = path.file_name() else {
        return Ok(path);
    };
    let directory =
        std::fs::canonicalize(parent).map_err(|error| ProjectIoError::Io(path.clone(), error))?;
    #[cfg(windows)]
    let directory = project_directory_without_verbatim(directory);
    Ok(directory.join(name))
}

#[cfg(windows)]
fn project_directory_without_verbatim(directory: PathBuf) -> PathBuf {
    let Some(text) = directory.to_str() else {
        return directory;
    };
    let ordinary = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(drive) = text
        .strip_prefix(r"\\?\")
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
    {
        PathBuf::from(drive)
    } else {
        return directory;
    };
    // Some names can only be opened with a verbatim prefix; keep it when required.
    if std::fs::canonicalize(&ordinary)
        .as_ref()
        .is_ok_and(|path| *path == directory)
    {
        ordinary
    } else {
        directory
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
            .map(|not_durable| {
                if let Some(error) = not_durable {
                    eprintln!(
                        "komponist: {}: not flushed to disk: {error}",
                        path.display()
                    );
                }
            })
            .map_err(|error| ProjectIoError::Io(path.to_owned(), error))
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
            ProjectAction::Save => match self.project_path.clone() {
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
            && self
                .project_path
                .as_deref()
                .is_some_and(|open| same(open, &path))
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
        self.doc.is_dirty()
            || self.has_unsaved_take()
            || self.param_undo.has_pending_text()
            || self.engine.host().has_pending_state_edits()
            || self.deferred_plugin_states().len() != 0
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
            && !self.engine.is_detached()
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
                    new_project::template_instruments(self.engine.host().catalog())
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
                crate::audio_record::reclaim_abandoned_takes(&project_dir);
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
        let repaired =
            (self.dialogs.repaired > 0).then(|| self.lang.project_repaired(self.dialogs.repaired));
        if self.engine.host().is_scanning() {
            self.dialogs.missing_check = true;
            let text = match &repaired {
                Some(repaired) => self.lang.project_opened_repaired(&name, repaired),
                None => self.lang.project_opened(&name),
            };
            self.set_status(text, StatusKind::Info);
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
        let assets = crate::dialogs::missing_plugins::missing_assets(
            self.lang,
            self.project(),
            self.project_dir().as_deref(),
            self.engine.host().catalog(),
        );
        if missing.is_empty() && samples == 0 && audio.is_empty() && assets.is_empty() {
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
            crate::dialogs::missing_plugins::title_for(self.lang, missing.len(), samples)
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
        self.engine.refresh_snapshot_automation(self.doc.project());
        let outcome = self
            .engine
            .host_mut()
            .snapshot_for_save(crate::app::SNAPSHOT_TIMEOUT);
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
        let playhead = self.engine.playhead();
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
        let mut document = self.doc.clone();
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
        let deferred = self
            .deferred_plugin_states()
            .filter(|(device, _)| !snapshots.iter().any(|(captured, _)| captured == device));
        for (device, state) in deferred.chain(&snapshots) {
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
        // A new file renames the project after it; the rename happens only if writing works.
        let rename = name_from_path(&path).filter(|name| {
            self.project_path.as_deref() != Some(path.as_path()) && *name != self.project().name
        });
        let file_ahead = for_file.is_some();
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
                let previous = std::sync::Arc::clone(self.doc.project());
                self.doc = document;
                if let Some(name) = rename {
                    self.untracked(ProjectProp::Name(name));
                }
                if file_ahead {
                    // The file holds a running take or plugin state still waiting on a gesture.
                    self.doc.forget_saved();
                } else {
                    self.doc.mark_saved();
                }
                self.project_path = Some(path.clone());
                self.recovery_ui.save_asks = false;
                let dir = self.project_dir();
                let project = std::sync::Arc::clone(self.doc.project());
                let host = self.engine.host_mut();
                host.set_project_dir(dir);
                // Future snapshots and renders resolve links against the new folder. Hand the
                // changed links to the devices only after the file write and directory change
                // succeeded.
                for device in project
                    .tracks
                    .iter()
                    .flat_map(|track| &track.channel.devices)
                    .chain(&project.master.devices)
                {
                    if let Some((_, _, old)) = previous.find_device(device.id)
                        && old.asset_links != device.asset_links
                    {
                        host.sync_asset_links(device.id, &old.asset_links, &device.asset_links);
                    }
                }
                self.set_status(
                    self.lang
                        .project_saved(crate::dialogs::format::tilde(&path)),
                    StatusKind::Success,
                );
                self.remember(path);
                self.recordings_saved(recordings);
                self.report_missing_save_snapshots(&outcome.unavailable);
                if let Some(error) = not_durable {
                    self.report_save_not_durable(&error);
                }
                true
            }
            Err(error) => self.report_project_save_error(error),
        }
    }

    /// Reports whether a save retained the document's stored plug-in state or had no state.
    fn report_missing_save_snapshots(&mut self, devices: &[komponist_model::DeviceId]) {
        let mut retained = Vec::new();
        let mut missing = Vec::new();
        for &device in devices {
            let Some((_, _, model)) = self.project().find_device(device) else {
                continue;
            };
            let has_state =
                model.state.is_some() || self.deferred_plugin_states().any(|(id, _)| *id == device);
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
            self.lang.project_save_failed(error.reason(self.lang)),
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
            eprintln!("komponist: recent projects not saved: {e}");
        }
    }

    /// Opens the file browser for `purpose` in the project's folder (else the last one, else the
    /// home folder).
    pub(crate) fn open_file_browser(&mut self, purpose: Purpose) {
        // A recovered project whose folder is gone starts elsewhere (crash recovery, variant d).
        let dir = self
            .project_path
            .as_deref()
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
                if self.recording.is_some()
                    || self.audio_recording.is_active()
                    || self.automation_capture.active()
                {
                    self.discard_take();
                }
                self.proceed(next);
            }
            // A running take ends as Stop would and is saved with the project; if the save fails,
            // the take stays in the document and the project stays open.
            Answer::Save => {
                self.stop_take();
                match self.project_path.clone() {
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
pub(crate) mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    thread_local! {
        /// Makes [`sync_directory`] fail on this thread, as a filesystem with a broken fsync would.
        pub(crate) static FAIL_DIRECTORY_SYNC: std::cell::Cell<bool> =
            const { std::cell::Cell::new(false) };
        /// The folders [`sync_directory`] flushed on this thread.
        pub(super) static SYNCED: std::cell::RefCell<Vec<PathBuf>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

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
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
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
    fn concurrent_project_writers_each_publish_a_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Song.komponist");
        let projects = [
            Project::new("First project"),
            Project::new("Second project"),
        ];
        let legacy_tmp = path.with_extension("komponist.tmp");
        std::fs::create_dir(&legacy_tmp).unwrap();
        let ready = std::sync::Barrier::new(projects.len());
        std::thread::scope(|scope| {
            for project in &projects {
                let path = &path;
                let ready = &ready;
                scope.spawn(move || {
                    ready.wait();
                    save_project(path, project).unwrap();
                });
            }
        });
        assert!(projects.contains(&load_project(&path).unwrap()));
        assert!(legacy_tmp.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn a_failed_promotion_keeps_the_destination_and_removes_staging() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Blocked.komponist");
        std::fs::create_dir(&path).unwrap();

        let error = save_project(&path, &Project::new("Song")).unwrap_err();
        assert!(matches!(error, ProjectIoError::Io(destination, _) if destination == path));
        assert!(path.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn stage_and_promotion_use_the_same_physical_parent() {
        let dir = tempfile::tempdir().unwrap();
        let chosen = dir.path().join("chosen");
        let actual = dir.path().join("actual");
        let child = actual.join("child");
        std::fs::create_dir(&chosen).unwrap();
        std::fs::create_dir_all(&child).unwrap();
        let link = chosen.join("link");
        std::os::unix::fs::symlink(&child, &link).unwrap();

        let project = Project::new("Song");
        save_project(&link.join("../Song.komponist"), &project).unwrap();
        assert_eq!(
            load_project(&actual.join("Song.komponist")).unwrap(),
            project
        );
        assert_eq!(std::fs::read_dir(chosen).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(actual).unwrap().count(), 2);
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
            app.doc
                .apply("Tempo", |p| {
                    edits::set_project_prop(
                        p,
                        ProjectProp::Tempo(komponist_model::Tempo::new(tempo)),
                    )
                })
                .unwrap();
        }
        assert!(app.doc.undo());

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
        let revision = app.doc.revision();
        assert!(!app.save_to(&destination));
        assert_eq!(app.doc.revision(), revision);
        assert_eq!(app.project_path(), Some(old_project.as_path()));
        check_links(app.project(), &old_dir, Path::new("kick.wav"));
        assert!(app.doc.undo());
        check_links(app.project(), &old_dir, Path::new("kick.wav"));
        assert!(app.doc.redo());
        assert!(app.doc.redo());
        check_links(app.project(), &old_dir, Path::new("kick.wav"));
        assert!(app.doc.undo());

        std::fs::remove_dir(&destination).unwrap();
        assert!(app.save_to(&destination));
        assert_eq!(app.project_path(), Some(destination.as_path()));
        let relative = Path::new("../kick.wav");
        check_links(&load_project(&destination).unwrap(), &new_dir, relative);
        check_links(app.project(), &new_dir, relative);
        assert!(!app.doc.is_dirty());
        assert!(app.doc.undo());
        check_links(app.project(), &new_dir, relative);
        assert!(app.doc.redo());
        assert!(app.doc.redo());
        check_links(app.project(), &new_dir, relative);
        drop(app);
        std::fs::remove_dir_all(dir).unwrap();
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
    fn write_atomic_replaces_the_target_and_leaves_no_staging_files() {
        let dir = scratch("atomic");
        let path = dir.join("nested/settings.json");
        // Creates the folder on request, replaces on the second write.
        write_atomic(&path, "one", true).unwrap();
        write_atomic(&path, "two", true).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(std::fs::read_dir(dir.join("nested")).unwrap().count(), 1);
        // A target that cannot be replaced (a folder) is an error: it stays and staging is gone.
        let blocked = dir.join("nested/blocked");
        std::fs::create_dir(&blocked).unwrap();
        std::fs::write(blocked.join("keep"), "x").unwrap();
        assert!(write_atomic(&blocked, "three", true).is_err());
        assert!(blocked.join("keep").exists());
        let names: Vec<_> = std::fs::read_dir(dir.join("nested"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    /// A destination with a NUL is refused before anything is published: the file whose name is
    /// the part before the NUL stays as it was, and no staging folder is left.
    #[test]
    fn a_destination_with_a_nul_changes_nothing() {
        let dir = scratch("nul");
        std::fs::create_dir_all(&dir).unwrap();
        let kept = dir.join("a");
        std::fs::write(&kept, "old").unwrap();
        let mut name = dir.join("a").into_os_string();
        name.push("\u{0}b");
        assert!(write_atomic(Path::new(&name), "new", false).is_err());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "old");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_directory_sync_is_reported_after_the_file_is_published() {
        let dir = scratch("sync-fail");
        let path = dir.join("a/b/settings.json");
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let result = write_atomic(&path, "one", true);
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(false));
        // Creating the folders is not published yet: that failure is an error.
        #[cfg(unix)]
        assert!(result.is_err());
        #[cfg(not(unix))]
        let _ = result;
        write_atomic(&path, "one", true).unwrap();
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let result = write_atomic(&path, "two", true);
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(false));
        // The file is replaced either way; Unix reports the failed flush next to it.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(result.unwrap().is_some(), cfg!(unix));
        assert_eq!(std::fs::read_dir(dir.join("a/b")).unwrap().count(), 1);
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
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        let saved = app.save_to(&path);
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(false));
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
            crate::engine_sync::EngineSync::null_with_midi(
                Box::new(App::headless_plugin_host()),
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
        assert!(matches!(
            RecentList::load(&path),
            Err(ProjectIoError::Recent(..))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// #221 (review of #257): a retry flushes the parents of folders that an earlier attempt
    /// created but failed to flush, although those folders exist by then.
    #[cfg(unix)]
    #[test]
    fn a_retry_flushes_the_parents_of_folders_a_failed_attempt_created() {
        let dir = scratch("sync-retry");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a/b/settings.json");
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(true));
        assert!(write_atomic(&path, "one", true).is_err());
        FAIL_DIRECTORY_SYNC.with(|flag| flag.set(false));
        assert!(dir.join("a/b").is_dir(), "the folders stay");

        // Other tests' pending folders (the shared temp folder) may be flushed here too.
        let flushed = |text| {
            SYNCED.with_borrow_mut(Vec::clear);
            write_atomic(&path, text, true).unwrap();
            let mut synced = SYNCED.with_borrow_mut(std::mem::take);
            synced.retain(|folder| folder.starts_with(&dir));
            synced
        };
        // The folders exist now, but their entries were never flushed: the retry does it,
        // outermost first, before the file's own folder.
        let retry = flushed("one");
        let position = |folder: &Path| retry.iter().position(|synced| synced == folder);
        let (outer, inner) = (position(&dir).unwrap(), position(&dir.join("a")).unwrap());
        assert!(outer < inner, "{retry:?}");
        assert_eq!(
            retry.last().map(PathBuf::as_path),
            Some(dir.join("a/b").as_path())
        );
        // Once flushed, nothing is pending: the next write flushes only the file's folder.
        assert_eq!(flushed("two"), [dir.join("a/b")]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A new folder's entry lives in its parent; a single relative component lives in `.`.
    #[test]
    fn a_new_folder_is_flushed_in_its_containing_folder() {
        assert_eq!(
            super::containing_dir(Path::new("nested")),
            Some(Path::new("."))
        );
        assert_eq!(
            super::containing_dir(Path::new("a/b")),
            Some(Path::new("a"))
        );
        assert_eq!(super::containing_dir(Path::new("/x")), Some(Path::new("/")));
        assert_eq!(super::containing_dir(Path::new("/")), None);
    }

    /// `MoveFileExW` gets NUL-free, absolute paths; resolved paths of `MAX_PATH` or more get the
    /// verbatim prefix, also when the input was a short relative path.
    #[cfg(windows)]
    #[test]
    fn windows_paths_are_resolved_before_the_length_check() {
        use std::os::windows::ffi::OsStrExt;
        let text = |wide: Vec<u16>| String::from_utf16(&wide[..wide.len() - 1]).unwrap();
        assert_eq!(
            super::windows_wide(Path::new("a\u{0}b"))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
        // A short relative name resolves against the working directory; it gets the prefix only
        // when the resolved path reaches the limit (a deeply nested checkout).
        let short = text(super::windows_wide(Path::new("short.komponist")).unwrap());
        let resolved = std::path::absolute("short.komponist").unwrap();
        let resolved_len = resolved.as_os_str().encode_wide().count();
        assert_eq!(short.starts_with(r"\\?\"), resolved_len >= 260, "{short}");
        assert!(short.ends_with(r"short.komponist"));
        let long = format!(r"C:\{}\x.komponist", "d".repeat(300));
        assert!(text(super::windows_wide(Path::new(&long)).unwrap()).starts_with(r"\\?\C:\"));
        let unc = format!(r"\\server\share\{}\x.komponist", "d".repeat(300));
        assert!(
            text(super::windows_wide(Path::new(&unc)).unwrap()).starts_with(r"\\?\UNC\server\")
        );
    }
}
