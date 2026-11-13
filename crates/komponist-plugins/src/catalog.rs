//! The plugin catalog: built-in entries plus scanned CLAP and VST3 bundles (ARCHITECTURE §8.3,
//! §8.5).
//!
//! Scanning runs `komponist-bridge --scan <bundle>` per bundle as separate processes, in parallel,
//! with [`SCAN_TIMEOUT`] each, and caches results by path and modification time in
//! `~/.cache/komponist/plugins.json` ([`ScanCache`]). A bundle that crashes or times out is listed
//! as skipped ("1 Plugin übersprungen", S07).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use komponist_ipc::{PluginDescriptor, PluginFormat as BundleFormat, PluginKind};
use komponist_model::PluginRef;

/// Time one bundle scan may take before its process is killed and the bundle skipped.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// File name of the scan cache inside the cache directory (`~/.cache/komponist/`).
pub const SCAN_CACHE_FILE: &str = "plugins.json";

/// CLAP id of klopfer (`../klopfer/src/lib.rs`), loaded from the bundled `Klopfer.clap`.
pub const KLOPFER_CLAP_ID: &str = "com.klopfer.drum-sampler";

/// Which built-in a catalog entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinKind {
    /// pisynth, 6-operator FM, runs in process as `komponist_builtin::PisynthProcessor` (D6).
    Pisynth,
    /// klopfer, drum sampler, the bundled `Klopfer.clap` run through the bridge (D5).
    Klopfer,
}

/// Where a catalog entry's code comes from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PluginLocation {
    /// Built into the app; the app creates the processor itself (not the plugin manager).
    InProcess(BuiltinKind),
    /// A bundle hosted in a bridge process.
    Bundle {
        /// Bundle format.
        format: BundleFormat,
        /// Bundle path.
        path: PathBuf,
        /// Plugin id within the bundle.
        plugin_id: String,
    },
}

/// Browser category of an effect (S01 browser, ARCHITECTURE §8.3), mapped from CLAP features and
/// VST3 subcategories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectCategory {
    /// Browser section „EQ und Filter“ (CLAP `equalizer`, `filter`; VST3 `EQ`, `Filter`).
    EqFilter,
    /// „Dynamik“ (CLAP `compressor`, `limiter`, `gate`, `expander`, `transient-shaper`; VST3
    /// `Dynamics`).
    Dynamics,
    /// „Hall und Delay“ (CLAP `reverb`, `delay`; VST3 `Reverb`, `Delay`).
    ReverbDelay,
    /// „Modulation“ (CLAP `chorus`, `flanger`, `phaser`, `tremolo`; VST3 `Modulation`).
    Modulation,
    /// Everything else (listed under all effects only).
    Other,
}

impl EffectCategory {
    /// The category for a feature list (first matching rule in the order of the variants).
    pub fn from_features(features: &[String]) -> EffectCategory {
        todo!("WP-45")
    }
}

/// One plugin the browser can offer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogEntry {
    /// The reference a device stores when this entry is added (format `Builtin` for pisynth and
    /// klopfer, shown as "INTERN").
    pub plugin: PluginRef,
    /// Instrument or effect.
    pub kind: PluginKind,
    /// Browser category for effects, `None` for instruments.
    pub category: Option<EffectCategory>,
    /// CLAP features / VST3 subcategories (search and categories).
    pub features: Vec<String>,
    /// Where the code is.
    pub location: PluginLocation,
}

impl CatalogEntry {
    /// Whether this is a built-in ("INTERN").
    pub fn is_builtin(&self) -> bool {
        self.plugin.format == komponist_model::PluginFormat::Builtin
    }
}

/// All plugins known to the app. UI thread.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Catalog {
    entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// Only the built-ins: pisynth always; klopfer if `klopfer_bundle` is `Some` (dev path from
    /// `scripts/build-klopfer.sh`, or `<prefix>/lib/komponist/plugins/Klopfer.clap`; WP-54).
    pub fn builtin(klopfer_bundle: Option<PathBuf>) -> Catalog {
        todo!("WP-45, WP-54")
    }

    /// All entries: built-ins first, then scanned plugins sorted by name (case-insensitive).
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// The entry for a device's plugin reference (same format and id), if installed.
    pub fn resolve(&self, plugin: &PluginRef) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|e| e.plugin.format == plugin.format && e.plugin.id == plugin.id)
    }

    /// Replaces all scanned entries with `descriptors` (built-ins stay). Descriptors of kind
    /// `Other` are left out.
    pub fn set_scanned(&mut self, descriptors: &[PluginDescriptor]) {
        todo!("WP-45")
    }

    /// Entries whose name, vendor or features contain `query` (case-insensitive; empty query = all),
    /// in catalog order (browser search, Ctrl F).
    pub fn search(&self, query: &str) -> Vec<&CatalogEntry> {
        todo!("WP-45")
    }
}

/// Plugin folders per format (S07 "Plugin-Ordner"). Persisted in the settings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginDirs {
    /// CLAP folders.
    pub clap: Vec<PathBuf>,
    /// VST3 folders.
    pub vst3: Vec<PathBuf>,
}

impl PluginDirs {
    /// The default folders (ARCHITECTURE §8.3): CLAP `~/.clap`, `/usr/lib/clap`,
    /// `/usr/local/lib/clap` and the entries of `$CLAP_PATH`; VST3 `~/.vst3`, `/usr/lib/vst3`,
    /// `/usr/local/lib/vst3`.
    pub fn defaults() -> PluginDirs {
        todo!("WP-45")
    }
}

/// One cached scan result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedBundle {
    /// Bundle path.
    pub path: PathBuf,
    /// Modification time of the bundle in seconds since the Unix epoch, when it was scanned.
    pub mtime_secs: u64,
    /// What the scan found (empty if it failed).
    pub descriptors: Vec<PluginDescriptor>,
    /// Why the scan failed (crash, timeout, no plugins), `None` on success.
    pub error: Option<String>,
}

/// The scan cache file. A bundle is rescanned when its path is new or its mtime changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    /// Cached bundles.
    pub bundles: Vec<CachedBundle>,
}

impl ScanCache {
    /// Reads the cache; a missing or unreadable file gives an empty cache.
    pub fn load(path: &Path) -> ScanCache {
        todo!("WP-45")
    }

    /// Writes the cache (creating the directory if needed).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        todo!("WP-45")
    }
}

/// Plugins found in one folder (S07 shows the count per folder).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DirCount {
    /// Format of the folder.
    pub format: BundleFormat,
    /// The folder.
    pub path: PathBuf,
    /// Plugins found in it.
    pub plugins: usize,
}

/// A bundle the scan skipped.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SkippedBundle {
    /// Bundle path.
    pub path: PathBuf,
    /// Why ("lässt sich nicht laden" in the UI): crash, timeout or error text.
    pub reason: String,
}

/// Summary of a finished scan ("214 Plugins gefunden · letzter Scan heute 09:12 · 1 Plugin
/// übersprungen").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScanReport {
    /// Plugins found in total (all formats).
    pub found: usize,
    /// Count per folder, in the order of [`PluginDirs`].
    pub per_dir: Vec<DirCount>,
    /// Bundles that failed.
    pub skipped: Vec<SkippedBundle>,
    /// When the scan finished.
    pub finished: SystemTime,
}
