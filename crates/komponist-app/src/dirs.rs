//! The per-user folders the app needs: the home folder (shown as `~`, where the file browser and
//! the export start without a project folder), the Music folder, and on Windows the roaming
//! application data folder and the drive roots.
//!
//! Unix: the home folder is `$HOME`; the file browser and the export start in it, as before.
//! The Music folder ([`music_dir`]) is `~/Music` on macOS; on Linux it follows the XDG user
//! directories (`$XDG_MUSIC_DIR`, else `user-dirs.dirs`, else `~/Music`), so a German desktop gets
//! `~/Musik` and an English one `~/Music`. Windows: the folders come from the shell's known folders
//! (`SHGetKnownFolderPath`: `FOLDERID_Profile`, `FOLDERID_Music`, `FOLDERID_RoamingAppData`), which
//! also finds folders the user moved (a redirected Music or AppData). Only when the shell does not
//! answer, the environment variables `USERPROFILE` and `APPDATA` stand in. The known folder wins
//! over the variable on purpose: a variable can be stale or set by a launcher, the shell's answer is
//! what Explorer shows.

use std::path::{Path, PathBuf};

/// The folder name under the Music folder for recordings, language-neutral on purpose.
pub const RECORDINGS_FOLDER: &str = "komponist/Recordings";

/// The user's home folder (`~`): `$HOME`; on Windows the profile folder (`C:\Users\<name>`).
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        win::profile().or_else(|| from_env("USERPROFILE"))
    }
    #[cfg(not(windows))]
    {
        // As it always was: whatever `$HOME` says.
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// Where a file browser or an export without a project folder starts: the Music folder on Windows,
/// the home folder elsewhere.
pub fn start_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        win::music().or_else(|| home_dir().map(|h| h.join("Music")))
    }
    #[cfg(not(windows))]
    {
        home_dir()
    }
}

/// The user's Music folder. Windows: the known Music folder. macOS: `~/Music`. Linux:
/// `$XDG_MUSIC_DIR`, else the `XDG_MUSIC_DIR` line of `$XDG_CONFIG_HOME/user-dirs.dirs` (default
/// `~/.config`), else `~/Music`.
pub fn music_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        start_dir()
    }
    #[cfg(target_os = "macos")]
    {
        home_dir().map(|home| home.join("Music"))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        let home = home_dir()?;
        Some(xdg_music_dir(
            std::env::var_os("XDG_MUSIC_DIR").as_deref(),
            std::env::var_os("XDG_CONFIG_HOME")
                .as_deref()
                .map(Path::new),
            &home,
        ))
    }
}

/// Where unsaved audio recordings go: `<music>/komponist/Recordings`.
pub fn recordings_dir() -> Option<PathBuf> {
    music_dir().map(|music| music.join(RECORDINGS_FOLDER))
}

/// The XDG Music folder from explicit inputs (so tests need no process-wide environment):
/// `env_music` (the variable's value) wins if absolute, then `user-dirs.dirs` in `config_home`
/// (or `<home>/.config`), then `<home>/Music`. A `user-dirs.dirs` value equal to the home folder
/// means "disabled" in xdg-user-dirs and is skipped.
#[cfg(any(test, all(not(windows), not(target_os = "macos"))))]
fn xdg_music_dir(
    env_music: Option<&std::ffi::OsStr>,
    config_home: Option<&Path>,
    home: &Path,
) -> PathBuf {
    // A value equal to the home folder means "disabled" in xdg-user-dirs, for the variable too.
    if let Some(path) = env_music
        .map(PathBuf::from)
        .filter(|p| p.is_absolute() && p != home)
    {
        return path;
    }
    let config = config_home
        .filter(|p| p.is_absolute())
        .map_or_else(|| home.join(".config"), Path::to_path_buf);
    std::fs::read_to_string(config.join("user-dirs.dirs"))
        .ok()
        .and_then(|text| parse_user_dirs_music(&text, home))
        .filter(|path| path != home)
        .unwrap_or_else(|| home.join("Music"))
}

/// The `XDG_MUSIC_DIR="…"` value of a `user-dirs.dirs` file; the last such line wins (the file is
/// shell syntax). Parsed like xdg-user-dirs' own lookup, without running shell code: the value is
/// double-quoted, a leading `$HOME` is expanded, and a backslash takes the next character
/// literally (`\$`, `\"`, `\\`). Relative results are rejected.
#[cfg(any(test, all(not(windows), not(target_os = "macos"))))]
fn parse_user_dirs_music(text: &str, home: &Path) -> Option<PathBuf> {
    let quoted = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix("XDG_MUSIC_DIR="))
        .next_back()?
        .trim()
        .strip_prefix('"')?;
    let (mut path, rest) = match quoted.strip_prefix("$HOME") {
        Some(rest) => (home.to_str()?.to_owned(), rest),
        None => (String::new(), quoted),
    };
    let mut chars = rest.chars();
    loop {
        match chars.next()? {
            '"' => break,
            '\\' => path.push(chars.next()?),
            c => path.push(c),
        }
    }
    let path = PathBuf::from(path);
    path.is_absolute().then_some(path)
}

/// The roaming application data folder (`C:\Users\<name>\AppData\Roaming`), Windows only.
#[cfg(windows)]
pub fn roaming_app_data() -> Option<PathBuf> {
    win::roaming_app_data().or_else(|| from_env("APPDATA"))
}

/// The roots a file browser can start from above a folder: the drives (`C:\`, `D:\` …) on Windows,
/// empty elsewhere (there `/` is the one root).
pub fn roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        win::drives()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// The drive roots for a `GetLogicalDrives` bit mask (bit 0 = `A:`), `A:\` … `Z:\`.
#[cfg(any(windows, test))]
fn drives_from_mask(mask: u32) -> Vec<PathBuf> {
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", char::from(b'A' + bit))))
        .collect()
}

/// An absolute path from the environment variable `name` (empty or relative values do not count).
#[cfg(windows)]
fn from_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
}

/// Where a file browser starts when it knows nothing better: `/`; on Windows the list of drives
/// (an empty path, see `dialogs::file`).
pub fn filesystem_root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::new()
    } else {
        PathBuf::from("/")
    }
}

/// Whether `path` is a root (a drive root on Windows, `/` elsewhere): it has no parent.
pub fn is_root(path: &Path) -> bool {
    path.parent().is_none()
}

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::path::PathBuf;

    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
    use windows_sys::Win32::System::Com::CoTaskMemFree;
    use windows_sys::Win32::UI::Shell::{
        FOLDERID_Music, FOLDERID_Profile, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
    };
    use windows_sys::core::GUID;

    pub fn profile() -> Option<PathBuf> {
        known_folder(&FOLDERID_Profile)
    }

    pub fn music() -> Option<PathBuf> {
        known_folder(&FOLDERID_Music)
    }

    pub fn roaming_app_data() -> Option<PathBuf> {
        known_folder(&FOLDERID_RoamingAppData)
    }

    pub fn drives() -> Vec<PathBuf> {
        // SAFETY: `GetLogicalDrives` takes no arguments and returns a bit mask (0 on failure).
        super::drives_from_mask(unsafe { GetLogicalDrives() })
    }

    /// The path of known folder `id` for the current user; `None` if the shell has none.
    fn known_folder(id: &GUID) -> Option<PathBuf> {
        let mut raw: *mut u16 = std::ptr::null_mut();
        // SAFETY: `id` is a valid GUID, flags 0 and a null token mean the current user, and `raw`
        // receives a NUL-terminated string the shell allocates; it is read once and then freed
        // with `CoTaskMemFree` (also after a failure, as the documentation asks).
        let path = unsafe {
            let hr = SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut raw);
            let path = if hr >= 0 && !raw.is_null() {
                let len = (0..).take_while(|&i| *raw.add(i) != 0).count();
                Some(PathBuf::from(OsString::from_wide(
                    std::slice::from_raw_parts(raw, len),
                )))
            } else {
                None
            };
            CoTaskMemFree(raw.cast());
            path
        };
        path.filter(|p| p.is_absolute())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_masks_list_the_present_letters() {
        assert!(drives_from_mask(0).is_empty());
        assert_eq!(
            drives_from_mask(0b1_0100),
            vec![PathBuf::from("C:\\"), PathBuf::from("E:\\")]
        );
        assert_eq!(drives_from_mask(1 << 25), vec![PathBuf::from("Z:\\")]);
    }

    fn fake_config(content: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "komponist-dirs-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("user-dirs.dirs"), content).unwrap();
        dir
    }

    const HOME: &str = if cfg!(windows) {
        "C:\\home\\kim"
    } else {
        "/home/kim"
    };

    #[test]
    fn xdg_env_variable_wins() {
        let config = fake_config("XDG_MUSIC_DIR=\"$HOME/Musik\"\n");
        let env_dir = std::env::temp_dir().join("elsewhere");
        let got = xdg_music_dir(Some(env_dir.as_os_str()), Some(&config), Path::new(HOME));
        assert_eq!(got, env_dir);
        // A relative variable does not count.
        let got = xdg_music_dir(Some("rel".as_ref()), Some(&config), Path::new(HOME));
        assert_eq!(got, Path::new(HOME).join("Musik"));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn xdg_user_dirs_file_is_read_with_home_expanded() {
        let config = fake_config(
            "# comment\nXDG_DESKTOP_DIR=\"$HOME/Schreibtisch\"\nXDG_MUSIC_DIR=\"$HOME/Musik\"\n",
        );
        let got = xdg_music_dir(None, Some(&config), Path::new(HOME));
        assert_eq!(got, Path::new(HOME).join("Musik"));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn xdg_falls_back_to_music() {
        let home = Path::new(HOME);
        // No file at all (a config folder without user-dirs.dirs).
        let empty =
            std::env::temp_dir().join(format!("komponist-dirs-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert_eq!(xdg_music_dir(None, Some(&empty), home), home.join("Music"));
        std::fs::remove_dir_all(empty).unwrap();
        // File without a music line, or with the "disabled" value $HOME.
        let config = fake_config("XDG_DESKTOP_DIR=\"$HOME/Desktop\"\n");
        assert_eq!(xdg_music_dir(None, Some(&config), home), home.join("Music"));
        std::fs::write(config.join("user-dirs.dirs"), "XDG_MUSIC_DIR=\"$HOME/\"\n").unwrap();
        assert_eq!(xdg_music_dir(None, Some(&config), home), home.join("Music"));
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn user_dirs_values_are_parsed_strictly() {
        let home = Path::new(HOME);
        let abs = std::env::temp_dir().join("Tunes");
        // The file escapes backslashes (a Windows temp path has them).
        let quoted = abs.display().to_string().replace('\\', "\\\\");
        let line = format!("XDG_MUSIC_DIR=\"{quoted}\"");
        assert_eq!(parse_user_dirs_music(&line, home), Some(abs));
        assert_eq!(
            parse_user_dirs_music("XDG_MUSIC_DIR=\"relative\"", home),
            None
        );
        assert_eq!(parse_user_dirs_music("", home), None);
        assert_eq!(
            parse_user_dirs_music("XDG_MUSIC_DIR=\"$HOME/a\"\nXDG_MUSIC_DIR=\"$HOME/b\"", home),
            Some(home.join("b"))
        );
    }

    /// Escapes in the quoted value are decoded like xdg-user-dirs' own lookup does.
    #[test]
    fn user_dirs_escapes_are_decoded() {
        let home = Path::new(HOME);
        let line = r#"XDG_MUSIC_DIR="$HOME/Music \$Archive \"live\" \\x""#;
        assert_eq!(
            parse_user_dirs_music(line, home),
            Some(home.join(r#"Music $Archive "live" \x"#))
        );
        // Without a closing quote the line is malformed.
        assert_eq!(
            parse_user_dirs_music(r#"XDG_MUSIC_DIR="$HOME/Music"#, home),
            None
        );
    }

    /// The variable set to the home folder means "disabled", as in `user-dirs.dirs`.
    #[test]
    fn a_home_valued_variable_is_disabled() {
        let home = Path::new(HOME);
        let config = fake_config("XDG_MUSIC_DIR=\"$HOME/Klang\"\n");
        assert_eq!(
            xdg_music_dir(Some(home.as_os_str()), Some(&config), home),
            home.join("Klang")
        );
        std::fs::remove_dir_all(config).unwrap();
    }

    #[test]
    fn recordings_folder_name_is_language_neutral() {
        assert_eq!(RECORDINGS_FOLDER, "komponist/Recordings");
        if let Some(dir) = recordings_dir() {
            assert!(dir.ends_with("komponist/Recordings"));
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_knows_its_folders() {
        assert!(home_dir().is_some_and(|h| h.is_dir()));
        assert!(roaming_app_data().is_some_and(|h| h.is_dir()));
        assert!(start_dir().is_some());
        let roots = roots();
        assert!(roots.iter().any(|r| is_root(r) && r.is_dir()));
        assert!(!is_root(&home_dir().unwrap()));
    }
}
