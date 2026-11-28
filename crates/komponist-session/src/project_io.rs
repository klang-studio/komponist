//! Project file loading, path resolution and durable publication.
//!
//! The project model validates the file and reports repairs on load. File publication stages and
//! flushes the new contents before replacing the destination. A caller can distinguish a failed
//! write from a successful publication whose directory entry could not be flushed.

use std::io::Write;
use std::path::{Path, PathBuf};

use komponist_model::persist::{self, FILE_EXTENSION, PersistError};
use komponist_model::{Project, Repairs};

/// Why a project file could not be read or written.
#[derive(Debug)]
pub enum ProjectIoError {
    /// Reading or writing failed.
    Io(PathBuf, std::io::Error),
    /// Not a project file, or one this build cannot read.
    Format(PathBuf, PersistError),
}

impl core::fmt::Display for ProjectIoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ProjectIoError::Io(path, e) => write!(f, "{}: {e}", path.display()),
            ProjectIoError::Format(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for ProjectIoError {}

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
pub fn write_atomic(
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
pub fn create_dirs_durably(dir: &Path) -> std::io::Result<()> {
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

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static FAIL_DIRECTORY_SYNC: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes directory flushes fail on the current thread for fault-injection tests.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub fn set_directory_sync_failure_for_test(fail: bool) {
    FAIL_DIRECTORY_SYNC.with(|flag| flag.set(fail));
}

/// Flushes the entries of folder `dir`. Filesystems that reject fsync on a folder are fine.
pub fn sync_directory(dir: &Path) -> std::io::Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if FAIL_DIRECTORY_SYNC.with(std::cell::Cell::get) {
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

/// Resolves parent steps through the filesystem before using the file's folder as a link base.
pub fn resolve_project_parent(path: PathBuf) -> Result<PathBuf, ProjectIoError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_model::fixtures::demo_project;

    thread_local! {
        // The folders this thread has flushed.
        pub(super) static SYNCED: std::cell::RefCell<Vec<PathBuf>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    #[test]
    fn projects_round_trip_through_files() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
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
        let (loaded, repairs) = load_project_repaired(&path).unwrap();
        assert_eq!(loaded, project);
        assert!(repairs.is_empty());
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

        let path = link.join("../Song.komponist");
        assert_eq!(
            resolve_project_parent(path.clone()).unwrap(),
            actual.canonicalize().unwrap().join("Song.komponist"),
        );
        let project = Project::new("Song");
        save_project(&path, &project).unwrap();
        assert_eq!(
            load_project(&actual.join("Song.komponist")).unwrap(),
            project
        );
        assert_eq!(std::fs::read_dir(chosen).unwrap().count(), 1);
        assert_eq!(std::fs::read_dir(actual).unwrap().count(), 2);
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
    }

    #[test]
    fn write_atomic_replaces_the_target_and_leaves_no_staging_files() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
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
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let kept = dir.join("a");
        std::fs::write(&kept, "old").unwrap();
        let mut name = dir.join("a").into_os_string();
        name.push("\u{0}b");
        assert!(write_atomic(Path::new(&name), "new", false).is_err());
        assert_eq!(std::fs::read_to_string(&kept).unwrap(), "old");
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
    }

    #[test]
    fn a_failed_directory_sync_is_reported_after_the_file_is_published() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let path = dir.join("a/b/settings.json");
        set_directory_sync_failure_for_test(true);
        let result = write_atomic(&path, "one", true);
        set_directory_sync_failure_for_test(false);
        // Creating the folders is not published yet: that failure is an error.
        #[cfg(unix)]
        assert!(result.is_err());
        #[cfg(not(unix))]
        let _ = result;
        write_atomic(&path, "one", true).unwrap();
        set_directory_sync_failure_for_test(true);
        let result = write_atomic(&path, "two", true);
        set_directory_sync_failure_for_test(false);
        // The file is replaced either way; Unix reports the failed flush next to it.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        assert_eq!(result.unwrap().is_some(), cfg!(unix));
        assert_eq!(std::fs::read_dir(dir.join("a/b")).unwrap().count(), 1);
    }

    /// #221 (review of #257): a retry flushes the parents of folders that an earlier attempt
    /// created but failed to flush, although those folders exist by then.
    #[cfg(unix)]
    #[test]
    fn a_retry_flushes_the_parents_of_folders_a_failed_attempt_created() {
        let scratch = tempfile::tempdir().unwrap();
        let dir = scratch.path();
        let path = dir.join("a/b/settings.json");
        set_directory_sync_failure_for_test(true);
        assert!(write_atomic(&path, "one", true).is_err());
        set_directory_sync_failure_for_test(false);
        assert!(dir.join("a/b").is_dir(), "the folders stay");

        // Other tests' pending folders (the shared temp folder) may be flushed here too.
        let flushed = |text| {
            SYNCED.with_borrow_mut(Vec::clear);
            write_atomic(&path, text, true).unwrap();
            let mut synced = SYNCED.with_borrow_mut(std::mem::take);
            synced.retain(|folder| folder.starts_with(dir));
            synced
        };
        // The folders exist now, but their entries were never flushed: the retry does it,
        // outermost first, before the file's own folder.
        let retry = flushed("one");
        let position = |folder: &Path| retry.iter().position(|synced| synced == folder);
        let (outer, inner) = (position(dir).unwrap(), position(&dir.join("a")).unwrap());
        assert!(outer < inner, "{retry:?}");
        assert_eq!(
            retry.last().map(PathBuf::as_path),
            Some(dir.join("a/b").as_path())
        );
        // Once flushed, nothing is pending: the next write flushes only the file's folder.
        assert_eq!(flushed("two"), [dir.join("a/b")]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
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
