//! App settings (`~/.config/komponist/settings.json`): audio stream, MIDI inputs, plugin folders,
//! UI scale, panel layout, the last export settings and the browser's favourites. The recent
//! projects have their own file next to it (`recent.json`, [`crate::project_io::RecentList`]).
//!
//! Plugin folders (S07 "Plugin-Ordner"): a scan always covers the default folders (ARCHITECTURE
//! §8.3, `$CLAP_PATH` included) and the folders added in S07. Default folders cannot be removed
//! (they are where packages install plugins, and a folder that does not exist costs nothing);
//! added folders can. S07 lists the default folders that exist and every added folder.
//!
//! Settings are applied at once and saved when they change (S07 "Änderungen gelten sofort"). A
//! missing file gives the defaults; an unreadable one is reported and replaced by the defaults
//! on the next save, never deleted. Unknown fields are ignored and missing fields take their
//! defaults, so older and newer files load.
//!
//! Thread: UI thread.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use komponist_audio::StreamConfig;
use komponist_model::project::{BUILTIN_KLOPFER, BUILTIN_PISYNTH};
use komponist_model::{PluginFormat, PluginRef};
use komponist_plugins::PluginDirs;
use komponist_tokens::size;

use crate::actions::{BitDepth, ExportRange, ExportSettings};
use crate::ui_state::PanelLayout;

/// File name inside the config directory.
pub const FILE_NAME: &str = "settings.json";

/// Everything komponist remembers between sessions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Audio driver, device, rate and buffer (S07).
    pub audio: StreamConfig,
    /// MIDI inputs switched off in S07, by port name (new ports start enabled).
    pub midi_disabled: Vec<String>,
    /// CLAP plugin folders added in S07 ("Plugin-Ordner"), scanned after the default folders
    /// ([`Settings::plugin_dirs`]).
    pub clap_dirs: Vec<PathBuf>,
    /// VST3 plugin folders added in S07.
    pub vst3_dirs: Vec<PathBuf>,
    /// UI scale override (`pixels_per_point`); `None` follows the display.
    pub ui_scale: Option<f32>,
    /// Panel sizes and visibility of layout 2a.
    pub panels: PanelSettings,
    /// The export dialog's last settings (S08 opens with them).
    pub export: Option<ExportSettings>,
    /// The browser's favourites (the star of a browser row).
    pub favorites: Favorites,
}

/// A browser favourite: a plugin by format and id, the way `Catalog::resolve` matches devices
/// (name and version may change with an update).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Favorite {
    /// Plugin format.
    pub format: PluginFormat,
    /// Format-specific id.
    pub id: String,
}

impl Favorite {
    /// The favourite for `plugin`.
    pub fn of(plugin: &PluginRef) -> Favorite {
        Favorite {
            format: plugin.format,
            id: plugin.id.clone(),
        }
    }

    /// Whether `plugin` is this favourite.
    pub fn matches(&self, plugin: &PluginRef) -> bool {
        self.format == plugin.format && self.id == plugin.id
    }
}

/// The favourites in the order they were starred. A fresh install starts with the two built-in
/// instruments, pisynth and klopfer (S01 "Favoriten", the prototype's `favs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Favorites(pub Vec<Favorite>);

impl Default for Favorites {
    fn default() -> Favorites {
        let builtin = |id: &str| Favorite {
            format: PluginFormat::Builtin,
            id: id.to_owned(),
        };
        Favorites(vec![builtin(BUILTIN_PISYNTH), builtin(BUILTIN_KLOPFER)])
    }
}

impl Favorites {
    /// Whether `plugin` is a favourite.
    pub fn contains(&self, plugin: &PluginRef) -> bool {
        self.0.iter().any(|f| f.matches(plugin))
    }

    /// Stars `plugin`, or removes its star.
    pub fn toggle(&mut self, plugin: &PluginRef) {
        if self.contains(plugin) {
            self.0.retain(|f| !f.matches(plugin));
        } else {
            self.0.push(Favorite::of(plugin));
        }
    }
}

/// The persisted part of [`PanelLayout`]: sizes and which panels are open. Maximise and the
/// active dock tab are per session.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelSettings {
    /// Browser open.
    pub browser_open: bool,
    /// Browser width.
    pub browser_width: f32,
    /// Inspector open.
    pub inspector_open: bool,
    /// Inspector width.
    pub inspector_width: f32,
    /// Dock open.
    pub dock_open: bool,
    /// Dock height.
    pub dock_height: f32,
}

impl Default for PanelSettings {
    fn default() -> PanelSettings {
        PanelSettings::from_layout(&PanelLayout::default())
    }
}

impl PanelSettings {
    /// The persisted part of `layout`.
    pub fn from_layout(layout: &PanelLayout) -> PanelSettings {
        PanelSettings {
            browser_open: layout.browser_open,
            browser_width: layout.browser_width,
            inspector_open: layout.inspector_open,
            inspector_width: layout.inspector_width,
            dock_open: layout.dock_open,
            dock_height: layout.dock_height,
        }
    }

    /// Writes these values into `layout`, clamping sizes to their token ranges (a hand-edited
    /// file cannot break the layout). The dock height is clamped to its minimum here; the window
    /// height limit applies at layout time.
    pub fn apply_to(&self, layout: &mut PanelLayout) {
        let clamp = |v: f32, min: f32, max: f32, default: f32| {
            if v.is_finite() {
                v.clamp(min, max)
            } else {
                default
            }
        };
        layout.browser_open = self.browser_open;
        layout.browser_width = clamp(
            self.browser_width,
            size::browser::MIN,
            size::browser::MAX,
            size::browser::DEFAULT,
        );
        layout.inspector_open = self.inspector_open;
        layout.inspector_width = clamp(
            self.inspector_width,
            size::inspector::MIN,
            size::inspector::MAX,
            size::inspector::DEFAULT,
        );
        layout.dock_open = self.dock_open;
        layout.dock_height = clamp(
            self.dock_height,
            size::dock::MIN,
            f32::MAX,
            size::dock::DEFAULT,
        );
    }
}

/// The default export settings (S08): whole song, 2 s tail, 48 kHz, 24 bit, normalise on, dither
/// off (as S08 shows it), into `folder` as `<project>.wav`.
pub fn default_export(project_name: &str, folder: PathBuf) -> ExportSettings {
    ExportSettings {
        range: ExportRange::Song,
        tail_seconds: 2.0,
        sample_rate: 48_000,
        bit_depth: BitDepth::Int24,
        normalize: true,
        dither: false,
        folder,
        file_name: format!("{project_name}.wav"),
    }
}

/// Why settings could not be loaded or saved.
#[derive(Debug)]
pub enum SettingsError {
    /// Reading or writing the file failed.
    Io(PathBuf, std::io::Error),
    /// The file is not valid settings JSON.
    Parse(PathBuf, serde_json::Error),
}

impl core::fmt::Display for SettingsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SettingsError::Io(path, e) => write!(f, "{}: {e}", path.display()),
            SettingsError::Parse(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for SettingsError {}

/// `$XDG_CONFIG_HOME/komponist`, else `$HOME/.config/komponist`; `None` without either.
pub fn config_dir() -> Option<PathBuf> {
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let base = xdg.or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".config"))
    })?;
    Some(base.join("komponist"))
}

/// The default settings file, `<config_dir>/settings.json`.
pub fn default_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join(FILE_NAME))
}

impl Settings {
    /// Reads `path`. A missing file gives `Ok(Settings::default())`.
    pub fn load(path: &Path) -> Result<Settings, SettingsError> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).map_err(|e| SettingsError::Parse(path.to_owned(), e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(SettingsError::Io(path.to_owned(), e)),
        }
    }

    /// Writes the settings to `path` (pretty JSON), creating the folder. Writes a temporary file
    /// next to it and renames it, so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let io = |e| SettingsError::Io(path.to_owned(), e);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| SettingsError::Parse(path.to_owned(), e))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }

    /// The folders a plugin scan covers: the default folders (ARCHITECTURE §8.3, `$CLAP_PATH`
    /// included), then the folders added in S07, each once.
    pub fn plugin_dirs(&self) -> PluginDirs {
        let mut dirs = PluginDirs::defaults();
        for (list, added) in [
            (&mut dirs.clap, &self.clap_dirs),
            (&mut dirs.vst3, &self.vst3_dirs),
        ] {
            for dir in added {
                if !list.contains(dir) {
                    list.push(dir.clone());
                }
            }
        }
        dirs
    }

    /// Whether `path` is one of the default plugin folders (S07 offers no "remove" for them).
    pub fn is_default_plugin_dir(path: &Path) -> bool {
        let defaults = PluginDirs::defaults();
        defaults
            .clap
            .iter()
            .chain(&defaults.vst3)
            .any(|d| d == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_audio::HostKind;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("komponist-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let dir = temp_dir("missing");
        let s = Settings::load(&dir.join(FILE_NAME)).unwrap();
        assert_eq!(s, Settings::default());
        assert_eq!(s.panels.browser_width, size::browser::DEFAULT);
        assert_eq!(s.panels.dock_height, size::dock::DEFAULT);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = temp_dir("round-trip");
        let path = dir.join("sub").join(FILE_NAME);
        let mut s = Settings::default();
        s.audio.host = HostKind::Null;
        s.audio.buffer_frames = 512;
        s.midi_disabled.push("Midi Through Port-0".into());
        s.ui_scale = Some(2.0);
        s.panels.browser_open = false;
        s.panels.dock_height = 400.0;
        s.export = Some(default_export("Nachtfahrt", dir.clone()));
        s.favorites.toggle(&PluginRef {
            format: PluginFormat::Clap,
            id: "com.example.reverb".into(),
            name: "Reverb".into(),
            vendor: "Example".into(),
            version: "1.0".into(),
        });
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_and_missing_fields_load() {
        let dir = temp_dir("partial");
        let path = dir.join(FILE_NAME);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"ui_scale": 1.5, "future": true}"#).unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.ui_scale, Some(1.5));
        assert_eq!(s.audio, StreamConfig::default());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(matches!(
            Settings::load(&path),
            Err(SettingsError::Parse(..))
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn panel_sizes_are_clamped_to_the_tokens() {
        let mut layout = PanelLayout::default();
        PanelSettings {
            browser_open: true,
            browser_width: 9000.0,
            inspector_open: false,
            inspector_width: 10.0,
            dock_open: true,
            dock_height: f32::NAN,
        }
        .apply_to(&mut layout);
        assert_eq!(layout.browser_width, size::browser::MAX);
        assert_eq!(layout.inspector_width, size::inspector::MIN);
        assert!(!layout.inspector_open);
        assert_eq!(layout.dock_height, size::dock::DEFAULT);
    }

    #[test]
    fn plugin_dirs_are_the_defaults_plus_the_added_folders() {
        let defaults = PluginDirs::defaults();
        let mut s = Settings::default();
        assert_eq!(s.plugin_dirs(), defaults);
        s.clap_dirs = vec!["/opt/clap".into(), defaults.clap[0].clone()];
        s.vst3_dirs = vec!["/opt/vst3".into()];
        let dirs = s.plugin_dirs();
        assert_eq!(dirs.clap.len(), defaults.clap.len() + 1);
        assert_eq!(dirs.clap.last(), Some(&PathBuf::from("/opt/clap")));
        assert_eq!(dirs.vst3.last(), Some(&PathBuf::from("/opt/vst3")));
        assert_eq!(dirs.vst3[..defaults.vst3.len()], defaults.vst3[..]);
    }

    #[test]
    fn favourites_start_with_the_builtins_and_toggle() {
        let s = Settings::default();
        let ids: Vec<&str> = s.favorites.0.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, [BUILTIN_PISYNTH, BUILTIN_KLOPFER]);
        let mut f = s.favorites.clone();
        let pisynth = PluginRef {
            format: PluginFormat::Builtin,
            id: BUILTIN_PISYNTH.into(),
            name: "pisynth".into(),
            vendor: String::new(),
            version: "9".into(),
        };
        assert!(f.contains(&pisynth));
        f.toggle(&pisynth);
        assert!(!f.contains(&pisynth));
        f.toggle(&pisynth);
        assert_eq!(f.0.last(), Some(&Favorite::of(&pisynth)));
        // An empty list stays empty after a save (no defaults come back).
        let json = serde_json::to_string(&Favorites(Vec::new())).unwrap();
        assert_eq!(json, "[]");
        let back: Settings = serde_json::from_str(r#"{"favorites": []}"#).unwrap();
        assert!(back.favorites.0.is_empty());
    }
}
