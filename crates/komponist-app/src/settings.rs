//! App settings (`~/.config/komponist/settings.json`, on macOS
//! `~/Library/Application Support/komponist/settings.json`, on Windows `%APPDATA%\komponist\settings.json`): audio stream, MIDI inputs, plugin folders,
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
use komponist_i18n::Lang;
use komponist_model::project::DRUM_SAMPLER_CLAP_ID;
use komponist_model::{PluginFormat, PluginRef};
use komponist_plugins::PluginDirs;
use komponist_tokens::size;

use crate::actions::{BitDepth, ExportRange, ExportSettings};
use crate::master_scope::MasterDisplay;
use crate::ui_state::PanelLayout;

/// File name inside the config directory.
pub const FILE_NAME: &str = "settings.json";

/// Everything komponist remembers between sessions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Audio driver, device, rate and buffer (S07).
    pub audio: StreamConfig,
    /// Explicit capture selection; absent in old settings and off by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input: Option<komponist_audio::InputConfig>,
    /// File encoding for new audio recordings. Existing takes remain untouched.
    #[serde(default, skip_serializing_if = "CaptureFormat::is_default")]
    pub capture_format: CaptureFormat,
    /// MIDI inputs switched off in S07, by port name (new ports start enabled).
    pub midi_disabled: Vec<String>,
    /// MIDI inputs the user switched on in S07, by port name. Matters for ports that start
    /// disabled ("Midi Through", the IAC Driver buses): the stored choice wins at the next start.
    pub midi_enabled: Vec<String>,
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
    /// "Oben halten" and the position of each plugin's frame, by plugin.
    pub plugin_frames: Vec<FrameMemory>,
    /// Worker threads that process tracks in parallel with the audio callback (WP-65; settings
    /// file only, no S07 control yet): `null` = automatic (one per core minus two, 4 on a
    /// 6-core machine), `0` = off (single-threaded), `n` = `n` threads (at most 15).
    pub worker_threads: Option<u32>,
    /// The UI language (S07 "Darstellung", applied at once; `--lang` overrides it for one run).
    pub language: Language,
    /// What the master display of the transport bar shows (#34; a click on it switches).
    pub master_display: MasterDisplay,
}

/// Encoding of newly recorded audio files (audio proposal §17).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureFormat {
    /// Signed 24-bit WAV (default).
    #[default]
    Pcm24,
    /// 32-bit floating-point WAV.
    Float32,
}

impl CaptureFormat {
    fn is_default(&self) -> bool {
        *self == Self::Pcm24
    }
}

/// The language setting: follow the system, or one language explicitly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    /// The language of the desktop; English unless it is German (owner, 2026-11-15).
    #[default]
    System,
    /// Deutsch.
    De,
    /// English.
    En,
}

/// An unknown value (a language that was removed, a typo, a wrong type) means [`Language::System`];
/// it must not fail the whole settings file.
impl<'de> Deserialize<'de> for Language {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Language, D::Error> {
        let value = serde_json::Value::deserialize(d)?;
        Ok(match value.as_str() {
            Some("de") => Language::De,
            Some("en") => Language::En,
            _ => Language::System,
        })
    }
}

impl Language {
    /// The language to show. `system` is the desktop's locale (`de-DE`, `en_US`), read by
    /// `main()` only: [`Language::System`] gives German for a German desktop and English for
    /// anything else, the explicit values give themselves.
    pub fn resolve(self, system: Option<&str>) -> Lang {
        match self {
            Language::System => system.and_then(Lang::from_code).unwrap_or(Lang::En),
            Language::De => Lang::De,
            Language::En => Lang::En,
        }
    }
}

/// The language for this run: `--lang` wins over the setting, the setting over the system.
pub fn startup_lang(cli: Option<Lang>, setting: Language, system: Option<&str>) -> Lang {
    cli.unwrap_or_else(|| setting.resolve(system))
}

/// What a plugin's frame window remembers between sessions (WP-50): "Oben halten" and where it
/// stood when it closed. Keyed by plugin (format and id), because device ids restart in every
/// project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameMemory {
    /// Plugin format.
    pub format: PluginFormat,
    /// Format-specific id.
    pub id: String,
    /// "Oben halten".
    pub keep_on_top: bool,
    /// Outer position in points.
    pub pos: Option<(i32, i32)>,
}

impl Settings {
    /// The frame memory of `plugin`.
    pub fn frame_memory(&self, plugin: &PluginRef) -> Option<&FrameMemory> {
        self.plugin_frames
            .iter()
            .find(|m| m.format == plugin.format && m.id == plugin.id)
    }

    /// Changes the frame memory of `plugin` with `change`, creating it if needed; `true` if it
    /// differs afterwards.
    pub fn update_frame_memory(
        &mut self,
        plugin: &PluginRef,
        change: impl FnOnce(&mut FrameMemory),
    ) -> bool {
        let at = match self
            .plugin_frames
            .iter()
            .position(|m| m.format == plugin.format && m.id == plugin.id)
        {
            Some(i) => i,
            None => {
                self.plugin_frames.push(FrameMemory {
                    format: plugin.format,
                    id: plugin.id.clone(),
                    keep_on_top: false,
                    pos: None,
                });
                self.plugin_frames.len() - 1
            }
        };
        let before = self.plugin_frames[at].clone();
        change(&mut self.plugin_frames[at]);
        self.plugin_frames[at] != before
    }
}

/// A browser favourite keyed by format, ID and whether the plugin is shipped with the app.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Favorite {
    /// Plugin format.
    pub format: PluginFormat,
    /// Format-specific id.
    pub id: String,
    /// Whether this marks the packaged CLAP rather than an external copy of the same ID.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bundled: bool,
}

impl Favorite {
    /// The favourite for `plugin`.
    pub fn of(plugin: &PluginRef) -> Favorite {
        Favorite {
            format: plugin.format,
            id: plugin.id.clone(),
            bundled: komponist_plugins::requires_bundled(plugin),
        }
    }

    /// Whether `plugin` is this favourite.
    pub fn matches(&self, plugin: &PluginRef) -> bool {
        self.format == plugin.format
            && self.id == plugin.id
            && self.bundled == komponist_plugins::requires_bundled(plugin)
    }
}

/// The favourites in the order they were starred. A fresh install starts with two bundled
/// instruments, hexa and the sampler (S01 "Favoriten", the prototype's `favs`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Favorites(pub Vec<Favorite>);

impl Default for Favorites {
    fn default() -> Favorites {
        let packaged = |id: &str| Favorite {
            format: PluginFormat::Clap,
            id: id.to_owned(),
            bundled: true,
        };
        Favorites(vec![
            packaged("org.klang-studio.komponist.hexa"),
            packaged(DRUM_SAMPLER_CLAP_ID),
        ])
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

/// The settings folder. Linux: `$XDG_CONFIG_HOME/komponist`, else `$HOME/.config/komponist`.
/// macOS: `$HOME/Library/Application Support/komponist` (the XDG variables are not honoured there).
/// Windows: `<roaming application data>\komponist`, i.e. `%APPDATA%\komponist` (the shell's known
/// folder, the variable only as a fallback; [`crate::dirs`]).
/// `None` without a usable `$HOME` (on Linux, without `$XDG_CONFIG_HOME` either).
pub fn config_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        crate::dirs::roaming_app_data().map(|d| d.join("komponist"))
    }
    #[cfg(not(windows))]
    {
        let xdg = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from);
        let home = std::env::var_os("HOME").map(PathBuf::from);
        config_dir_from(cfg!(target_os = "macos"), xdg.as_deref(), home.as_deref())
    }
}

/// [`config_dir`] for given inputs (Linux and macOS); `macos` picks the platform's convention.
#[cfg(not(windows))]
fn config_dir_from(macos: bool, xdg: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let base = if macos {
        home?.join("Library/Application Support")
    } else {
        xdg.filter(|p| p.is_absolute())
            .map(Path::to_path_buf)
            .or_else(|| home.map(|h| h.join(".config")))?
    };
    Some(base.join("komponist"))
}

/// The audio cache folder (created lazily by the file pool). Linux follows
/// `$XDG_CACHE_HOME`, macOS uses `~/Library/Caches`, Windows `%LOCALAPPDATA%`.
/// No directory is created by this function or by headless app construction.
pub fn cache_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .map(|p| p.join("komponist"))
    }
    #[cfg(not(windows))]
    {
        let xdg = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
        let home = crate::dirs::home_dir();
        cache_dir_from(cfg!(target_os = "macos"), xdg.as_deref(), home.as_deref())
    }
}

/// [`cache_dir`], else `<temp folder>/komponist`: where the plugin manager keeps its scan cache and the bundled
/// CLAP snapshots (it needs a folder even without a usable home).
pub fn cache_dir_or_temp() -> PathBuf {
    cache_dir().unwrap_or_else(|| std::env::temp_dir().join("komponist"))
}

#[cfg(not(windows))]
fn cache_dir_from(macos: bool, xdg: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let home = home.filter(|p| p.is_absolute());
    let base = if macos {
        home?.join("Library/Caches")
    } else {
        xdg.filter(|p| p.is_absolute())
            .map(Path::to_path_buf)
            .or_else(|| home.map(|h| h.join(".cache")))?
    };
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
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| SettingsError::Parse(path.to_owned(), e))?;
        komponist_session::project_io::write_atomic(path, &text, true)
            .map(|not_durable| {
                if let Some(error) = not_durable {
                    eprintln!(
                        "komponist: {}: not flushed to disk: {error}",
                        path.display()
                    );
                }
            })
            .map_err(io)
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

    #[test]
    fn old_settings_keep_capture_off_and_new_capture_settings_round_trip() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert!(old.audio_input.is_none());
        assert_eq!(old.capture_format, CaptureFormat::Pcm24);
        let mut settings = old;
        settings.audio_input = Some(komponist_audio::InputConfig {
            device: Some("Test input".into()),
            device_id: Some(komponist_audio::DeviceId("test:input".into())),
        });
        settings.capture_format = CaptureFormat::Float32;
        let stored = serde_json::to_string(&settings).unwrap();
        let loaded: Settings = serde_json::from_str(&stored).unwrap();
        assert_eq!(loaded.audio_input, settings.audio_input);
        assert_eq!(loaded.capture_format, CaptureFormat::Float32);
    }

    #[test]
    #[cfg(not(windows))]
    fn cache_dir_follows_the_platform() {
        let home = Path::new("/users/test");
        let xdg = Path::new("/cache");
        assert_eq!(
            cache_dir_from(true, Some(xdg), Some(home)),
            Some(home.join("Library/Caches/komponist"))
        );
        assert_eq!(
            cache_dir_from(false, Some(xdg), Some(home)),
            Some(xdg.join("komponist"))
        );
        assert_eq!(
            cache_dir_from(false, Some(Path::new("relative")), Some(home)),
            Some(home.join(".cache/komponist"))
        );
        assert_eq!(cache_dir_from(false, None, None), None);
        assert_eq!(cache_dir_from(true, Some(xdg), None), None);
        assert_eq!(
            cache_dir_from(false, None, Some(Path::new("relative"))),
            None
        );
    }

    #[test]
    fn the_plugin_cache_folder_is_the_app_cache_or_a_temp_folder() {
        let dir = cache_dir_or_temp();
        assert!(dir.is_absolute());
        assert!(dir.ends_with("komponist"));
        match cache_dir() {
            Some(cache) => assert_eq!(dir, cache),
            None => assert_eq!(dir, std::env::temp_dir().join("komponist")),
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn config_dir_follows_the_platform() {
        let home = Path::new("/Users/u");
        let xdg = Path::new("/x/cfg");
        assert_eq!(
            config_dir_from(true, Some(xdg), Some(home)),
            Some("/Users/u/Library/Application Support/komponist".into())
        );
        assert_eq!(config_dir_from(true, None, None), None);
        assert_eq!(
            config_dir_from(false, Some(xdg), Some(home)),
            Some("/x/cfg/komponist".into())
        );
        assert_eq!(
            config_dir_from(false, Some(Path::new("rel")), Some(home)),
            Some("/Users/u/.config/komponist".into())
        );
        assert_eq!(
            config_dir_from(false, None, Some(home)),
            Some("/Users/u/.config/komponist".into())
        );
        assert_eq!(config_dir_from(false, None, None), None);
    }

    use komponist_audio::HostKind;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("komponist-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn frame_memory_is_kept_per_plugin_and_round_trips() {
        let a = PluginRef {
            format: PluginFormat::Clap,
            id: "surge".into(),
            name: "Surge XT".into(),
            vendor: String::new(),
            version: String::new(),
            module: Box::default(),
        };
        let b = PluginRef {
            id: "vital".into(),
            ..a.clone()
        };
        let mut s = Settings::default();
        assert!(s.update_frame_memory(&a, |m| m.keep_on_top = true));
        assert!(s.update_frame_memory(&a, |m| m.pos = Some((10, 20))));
        assert!(
            !s.update_frame_memory(&a, |m| m.pos = Some((10, 20))),
            "no change"
        );
        let json = serde_json::to_string(&s).unwrap();
        let back: Settings = serde_json::from_str(&json).unwrap();
        let m = back.frame_memory(&a).unwrap();
        assert!(m.keep_on_top);
        assert_eq!(m.pos, Some((10, 20)));
        assert!(back.frame_memory(&b).is_none());
    }

    #[test]
    fn worker_threads_default_to_automatic() {
        assert_eq!(Settings::default().worker_threads, None);
        let s: Settings = serde_json::from_str(r#"{"worker_threads": 0}"#).unwrap();
        assert_eq!(s.worker_threads, Some(0));
        let s: Settings = serde_json::from_str(r#"{"worker_threads": null}"#).unwrap();
        assert_eq!(s.worker_threads, None);
    }

    #[test]
    fn language_defaults_to_the_system_and_resolves() {
        assert_eq!(Settings::default().language, Language::System);
        let s: Settings = serde_json::from_str(r#"{"language": "en"}"#).unwrap();
        assert_eq!(s.language, Language::En);
        let s: Settings = serde_json::from_str(r#"{"language": "system"}"#).unwrap();
        assert_eq!(s.language, Language::System);
        assert_eq!(serde_json::to_string(&Language::De).unwrap(), "\"de\"");
        // Unknown values fall back to the system; the rest of the file still loads.
        for bad in [r#""fr""#, "7", "null", r#"{"a":1}"#] {
            let s: Settings =
                serde_json::from_str(&format!(r#"{{"language": {bad}, "ui_scale": 2.0}}"#))
                    .unwrap();
            assert_eq!(s.language, Language::System, "{bad}");
            assert_eq!(s.ui_scale, Some(2.0));
        }
        // The system: German gives German, everything else (and nothing) English.
        assert_eq!(Language::System.resolve(Some("de_DE.UTF-8")), Lang::De);
        assert_eq!(Language::System.resolve(Some("de-AT")), Lang::De);
        assert_eq!(Language::System.resolve(Some("en-US")), Lang::En);
        assert_eq!(Language::System.resolve(Some("fr-FR")), Lang::En);
        assert_eq!(Language::System.resolve(Some("C")), Lang::En);
        assert_eq!(Language::System.resolve(None), Lang::En);
        // Explicit values ignore the system.
        assert_eq!(Language::De.resolve(Some("en-US")), Lang::De);
        assert_eq!(Language::En.resolve(Some("de-DE")), Lang::En);
        // `--lang` beats the setting, the setting beats the system.
        let (de, en) = (Some("de-DE"), Some("en-US"));
        assert_eq!(startup_lang(Some(Lang::En), Language::De, de), Lang::En);
        assert_eq!(startup_lang(Some(Lang::De), Language::En, en), Lang::De);
        assert_eq!(startup_lang(None, Language::En, de), Lang::En);
        assert_eq!(startup_lang(None, Language::System, de), Lang::De);
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
        s.midi_enabled.push("IAC Driver Bus 1".into());
        s.ui_scale = Some(2.0);
        s.panels.browser_open = false;
        s.panels.dock_height = 400.0;
        s.export = Some(default_export("Night Drive", dir.clone()));
        s.favorites.toggle(&PluginRef {
            format: PluginFormat::Clap,
            id: "com.example.reverb".into(),
            name: "Reverb".into(),
            vendor: "Example".into(),
            version: "1.0".into(),
            module: Box::default(),
        });
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrent_settings_writers_each_publish_a_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let legacy_tmp = path.with_extension("json.tmp");
        std::fs::create_dir(&legacy_tmp).unwrap();
        let settings = [1.5, 2.0].map(|scale| Settings {
            ui_scale: Some(scale),
            ..Default::default()
        });
        let ready = std::sync::Barrier::new(settings.len());
        std::thread::scope(|scope| {
            for settings in &settings {
                let path = &path;
                let ready = &ready;
                scope.spawn(move || {
                    ready.wait();
                    settings.save(path).unwrap();
                });
            }
        });
        assert!(settings.contains(&Settings::load(&path).unwrap()));
        assert!(legacy_tmp.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
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
    fn favourites_start_with_the_shipped_instruments_and_toggle() {
        let s = Settings::default();
        let ids: Vec<&str> = s.favorites.0.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            ["org.klang-studio.komponist.hexa", DRUM_SAMPLER_CLAP_ID]
        );
        let mut f = s.favorites.clone();
        let hexa = PluginRef {
            format: PluginFormat::Clap,
            id: "org.klang-studio.komponist.hexa".into(),
            name: "Hexa".into(),
            vendor: String::new(),
            version: "9".into(),
            module: "bundled-clap/hexa.clap".into(),
        };
        assert!(f.contains(&hexa));
        f.toggle(&hexa);
        assert!(!f.contains(&hexa));
        f.toggle(&hexa);
        assert_eq!(f.0.last(), Some(&Favorite::of(&hexa)));
        // An empty list stays empty after a save (no defaults come back).
        let json = serde_json::to_string(&Favorites(Vec::new())).unwrap();
        assert_eq!(json, "[]");
        let back: Settings = serde_json::from_str(r#"{"favorites": []}"#).unwrap();
        assert!(back.favorites.0.is_empty());
    }
}
