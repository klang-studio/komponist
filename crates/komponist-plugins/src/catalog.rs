//! The plugin catalog: built-in entries plus scanned CLAP and VST3 bundles (ARCHITECTURE §8.3,
//! §8.5).
//!
//! Scanning runs `komponist-bridge --scan <bundle>` per bundle as separate processes, in parallel,
//! with [`SCAN_TIMEOUT`] each, and caches results by path and modification time in
//! `~/.cache/komponist/plugins.json` (macOS `~/Library/Caches/komponist/`; [`ScanCache`]). A bundle that crashes or times out is listed
//! as skipped ("1 Plugin übersprungen", S07).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use komponist_ipc::{PluginDescriptor, PluginFormat as BundleFormat, PluginKind};
use komponist_model::PluginRef;
use komponist_model::project::BUILTIN_KLOPFER;

pub use komponist_builtin::BuiltinKind;

pub use crate::builtin::KLOPFER_CLAP_ID;

/// Time one bundle scan may take before its process is killed and the bundle skipped.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// File name of the scan cache inside the cache directory (`~/.cache/komponist/`).
pub const SCAN_CACHE_FILE: &str = "plugins.json";

const BUNDLED_MODULE_PREFIX: &str = "bundled-clap/";

pub(crate) fn bundled_module(file_name: &str) -> Box<str> {
    format!("{BUNDLED_MODULE_PREFIX}{file_name}").into()
}

/// A bundled reference may resolve only through the packaged catalog. The module
/// suffix is a file name for diagnostics; it is never interpreted as a load path.
pub(crate) fn requires_bundled(plugin: &PluginRef) -> bool {
    plugin.format == komponist_model::PluginFormat::Clap
        && plugin.module.starts_with(BUNDLED_MODULE_PREFIX)
}

/// The built-in kind requested by a packaged CLAP reference. This identifies the stored
/// catalog address only; the running instance must still be authenticated by the manager.
pub fn packaged_kind(plugin: &PluginRef) -> Option<BuiltinKind> {
    if !requires_bundled(plugin) {
        return None;
    }
    crate::bundled_state::kind_for_id(&plugin.id)
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
        const RULES: [(EffectCategory, &[&str]); 4] = [
            (EffectCategory::EqFilter, &["equalizer", "eq", "filter"]),
            (
                EffectCategory::Dynamics,
                &[
                    "compressor",
                    "limiter",
                    "gate",
                    "expander",
                    "transient-shaper",
                    "dynamics",
                ],
            ),
            (EffectCategory::ReverbDelay, &["reverb", "delay"]),
            (
                EffectCategory::Modulation,
                &["chorus", "flanger", "phaser", "tremolo", "modulation"],
            ),
        ];
        RULES
            .iter()
            .find(|(_, names)| {
                features
                    .iter()
                    .any(|f| names.iter().any(|n| f.eq_ignore_ascii_case(n)))
            })
            .map_or(EffectCategory::Other, |(category, _)| *category)
    }
}

/// One plugin the browser can offer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CatalogEntry {
    /// The reference a device stores when this entry is added (format `Builtin` for hexa and
    /// the internal devices, shown as "INTERN").
    pub plugin: PluginRef,
    /// Instrument or effect.
    pub kind: PluginKind,
    /// Browser category for effects, `None` for instruments.
    pub category: Option<EffectCategory>,
    /// CLAP features / VST3 subcategories (search and categories).
    pub features: Vec<String>,
    /// Where the code is.
    pub location: PluginLocation,
    /// The plugin has a sidechain input (#6): the device head shows the „SC“ chip. Scanned plugins
    /// from their audio ports ([`PluginDescriptor::sidechain`]), internal devices from their spec.
    pub sidechain: bool,
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
    /// Only the built-ins ([`crate::builtin`]): hexa and the internal devices.
    pub fn builtin() -> Catalog {
        Catalog {
            entries: crate::builtin::entries(),
        }
    }

    /// Find the packaged CLAP entry of a built-in, if it was included in the scan.
    pub fn packaged(&self, kind: BuiltinKind) -> Option<&CatalogEntry> {
        self.entries
            .iter()
            .find(|entry| packaged_kind(&entry.plugin) == Some(kind))
    }

    /// All entries: built-ins first, then scanned plugins sorted by name (case-insensitive).
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// The entry for a device's plugin reference, if installed. Bundled and external CLAP
    /// references resolve separately even when they use the same plugin ID. A bundled reference
    /// also requires the exact packaged module.
    ///
    /// A project saved while klopfer was a built-in (`Builtin` format, id [`BUILTIN_KLOPFER`])
    /// resolves to the scanned klopfer CLAP plugin: the device keeps its state, which is the
    /// plugin's own.
    pub fn resolve(&self, plugin: &PluginRef) -> Option<&CatalogEntry> {
        let bundled = requires_bundled(plugin);
        let same = |e: &&CatalogEntry| {
            e.plugin.format == plugin.format
                && e.plugin.id == plugin.id
                && requires_bundled(&e.plugin) == bundled
                && (!bundled || e.plugin.module == plugin.module)
        };
        self.entries.iter().find(same).or_else(|| {
            (plugin.format == komponist_model::PluginFormat::Builtin
                && plugin.id == BUILTIN_KLOPFER)
                .then(|| {
                    self.entries.iter().find(|e| {
                        e.plugin.format == komponist_model::PluginFormat::Clap
                            && e.plugin.id == KLOPFER_CLAP_ID
                    })
                })
                .flatten()
        })
    }

    /// Replaces all scanned entries with `descriptors` (built-ins stay). Descriptors of kind
    /// `Other` are left out.
    ///
    /// The same plugin in several bundles (one per folder) is listed once, from the first
    /// descriptor.
    pub fn set_scanned(&mut self, descriptors: &[PluginDescriptor]) {
        self.set_scanned_with_bundled(descriptors, |_| false);
    }

    pub(crate) fn set_scanned_with_bundled(
        &mut self,
        descriptors: &[PluginDescriptor],
        shipped: impl Fn(&PluginDescriptor) -> bool,
    ) {
        self.entries.retain(CatalogEntry::is_builtin);
        let mut seen = HashSet::new();
        let mut scanned: Vec<CatalogEntry> = descriptors
            .iter()
            .filter(|d| d.kind != PluginKind::Other)
            .filter_map(|d| {
                let bundled = shipped(d);
                seen.insert((d.format, d.id.clone(), bundled))
                    .then(|| scanned_entry(d, bundled))
            })
            .collect();
        scanned.sort_by_cached_key(|e| e.plugin.name.to_lowercase());
        self.entries.extend(scanned);
    }

    /// Entries whose name, vendor or features contain `query` (case-insensitive; empty query = all),
    /// in catalog order (browser search, Ctrl F).
    pub fn search(&self, query: &str) -> Vec<&CatalogEntry> {
        let query = query.to_lowercase();
        let hit = |text: &str| text.to_lowercase().contains(&query);
        self.entries
            .iter()
            .filter(|e| {
                query.is_empty()
                    || hit(&e.plugin.name)
                    || hit(&e.plugin.vendor)
                    || e.features.iter().any(|f| hit(f))
            })
            .collect()
    }
}

/// The catalog entry of a scanned plugin.
fn scanned_entry(d: &PluginDescriptor, bundled: bool) -> CatalogEntry {
    let format = match d.format {
        BundleFormat::Clap => komponist_model::PluginFormat::Clap,
        BundleFormat::Vst3 => komponist_model::PluginFormat::Vst3,
    };
    CatalogEntry {
        plugin: PluginRef {
            format,
            id: d.id.clone(),
            name: d.name.clone(),
            vendor: d.vendor.clone(),
            version: d.version.clone(),
            module: d.bundle.file_name().map_or_else(Box::default, |name| {
                let name = name.to_string_lossy();
                if bundled {
                    bundled_module(&name)
                } else {
                    name.into()
                }
            }),
        },
        kind: d.kind,
        category: (d.kind == PluginKind::Effect)
            .then(|| EffectCategory::from_features(&d.features)),
        features: d.features.clone(),
        location: PluginLocation::Bundle {
            format: d.format,
            path: d.bundle.clone(),
            plugin_id: d.id.clone(),
        },
        sidechain: d.sidechain,
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

/// The OS whose conventions the default folders follow (plugin folders, the scan cache).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    /// Linux, and the fallback for any other Unix.
    Linux,
    Macos,
    Windows,
}

impl Platform {
    /// The OS this build runs on.
    pub(crate) const CURRENT: Platform = if cfg!(windows) {
        Platform::Windows
    } else if cfg!(target_os = "macos") {
        Platform::Macos
    } else {
        Platform::Linux
    };
}

/// An environment variable as a path, if set and not empty.
pub(crate) fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

impl PluginDirs {
    /// The default folders (ARCHITECTURE §8.3). Linux: CLAP `~/.clap`, `/usr/lib/clap`,
    /// `/usr/local/lib/clap` and the entries of `$CLAP_PATH`; VST3 `~/.vst3`, `/usr/lib/vst3`,
    /// `/usr/local/lib/vst3`. macOS: CLAP `~/Library/Audio/Plug-Ins/CLAP`,
    /// `/Library/Audio/Plug-Ins/CLAP` and `$CLAP_PATH`; VST3 `~/Library/Audio/Plug-Ins/VST3`,
    /// `/Library/Audio/Plug-Ins/VST3`. Windows: CLAP `%COMMONPROGRAMFILES%\CLAP`,
    /// `%LOCALAPPDATA%\Programs\Common\CLAP` and the entries of `%CLAP_PATH%` (`;`-separated);
    /// VST3 `%COMMONPROGRAMFILES%\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3`.
    pub fn defaults() -> PluginDirs {
        PluginDirs::defaults_from(Platform::CURRENT, &|name: &str| env_path(name))
    }

    /// [`PluginDirs::defaults`] for a platform and the environment `var` (a variable by name).
    /// Empty entries of `CLAP_PATH` and folders listed twice are left out.
    pub(crate) fn defaults_from(
        platform: Platform,
        var: &dyn Fn(&str) -> Option<PathBuf>,
    ) -> PluginDirs {
        let home = var("HOME");
        let under = |base: Option<PathBuf>, rel: &str| base.map(|b| b.join(rel));
        let (mut clap, vst3): (Vec<Option<PathBuf>>, Vec<Option<PathBuf>>) = match platform {
            Platform::Linux => (
                vec![
                    under(home.clone(), ".clap"),
                    Some("/usr/lib/clap".into()),
                    Some("/usr/local/lib/clap".into()),
                ],
                vec![
                    under(home, ".vst3"),
                    Some("/usr/lib/vst3".into()),
                    Some("/usr/local/lib/vst3".into()),
                ],
            ),
            Platform::Macos => (
                vec![
                    under(home.clone(), "Library/Audio/Plug-Ins/CLAP"),
                    Some("/Library/Audio/Plug-Ins/CLAP".into()),
                ],
                vec![
                    under(home, "Library/Audio/Plug-Ins/VST3"),
                    Some("/Library/Audio/Plug-Ins/VST3".into()),
                ],
            ),
            Platform::Windows => {
                let common = var("COMMONPROGRAMFILES");
                let local = var("LOCALAPPDATA").map(|l| l.join("Programs").join("Common"));
                (
                    vec![under(common.clone(), "CLAP"), under(local.clone(), "CLAP")],
                    vec![under(common, "VST3"), under(local, "VST3")],
                )
            }
        };
        if let Some(paths) = var("CLAP_PATH") {
            let separator = if platform == Platform::Windows {
                ';'
            } else {
                ':'
            };
            let entries: Vec<PathBuf> = match paths.to_str() {
                Some(text) => text.split(separator).map(PathBuf::from).collect(),
                // Not UTF-8: only the running OS can split it.
                None => std::env::split_paths(paths.as_os_str()).collect(),
            };
            clap.extend(
                entries
                    .into_iter()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(Some),
            );
        }
        let mut seen = HashSet::new();
        let clap: Vec<PathBuf> = clap
            .into_iter()
            .flatten()
            .filter(|p| seen.insert(p.clone()))
            .collect();
        let vst3 = vst3.into_iter().flatten().collect();
        PluginDirs { clap, vst3 }
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

/// Format version of the scan cache file. 1 (#6): descriptors say whether the plugin has a
/// sidechain input, which a file without a version (from before #6) does not know.
pub const SCAN_CACHE_VERSION: u32 = 1;

/// The scan cache file. A bundle is rescanned when its path is new or its mtime changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    /// [`SCAN_CACHE_VERSION`] of the writer; absent (0) in files from before #6.
    #[serde(default)]
    pub version: u32,
    /// Cached bundles.
    pub bundles: Vec<CachedBundle>,
}

impl Default for ScanCache {
    fn default() -> ScanCache {
        ScanCache {
            version: SCAN_CACHE_VERSION,
            bundles: Vec::new(),
        }
    }
}

impl ScanCache {
    /// Reads the cache; a missing or unreadable file, or one of another
    /// [`SCAN_CACHE_VERSION`], gives an empty cache, so the next scan scans every bundle again
    /// (the catalog starts with the built-ins only until then).
    pub fn load(path: &Path) -> ScanCache {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<ScanCache>(&bytes).ok())
            .filter(|cache| cache.version == SCAN_CACHE_VERSION)
            .unwrap_or_default()
    }

    /// Writes the cache (creating the directory if needed).
    ///
    /// Written to a temporary file next to `path` and renamed over it, so a crash never leaves a
    /// half-written cache.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}.tmp", std::process::id()));
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// The cached result for `path` scanned at `mtime_secs`, if any.
    pub(crate) fn lookup(&self, path: &Path, mtime_secs: u64) -> Option<&CachedBundle> {
        self.bundles
            .iter()
            .find(|b| b.path == path && b.mtime_secs == mtime_secs)
    }

    /// All descriptors in cache order.
    pub(crate) fn descriptors(&self) -> Vec<PluginDescriptor> {
        self.bundles
            .iter()
            .flat_map(|b| b.descriptors.iter().cloned())
            .collect()
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

#[cfg(test)]
mod tests {
    /// `hexa`, then the internal devices, by name.
    fn builtin_names() -> Vec<&'static str> {
        let mut names = vec!["Hexa"];
        names.extend(komponist_builtin::Internal::ALL.iter().map(|k| k.name()));
        names
    }

    use super::*;
    use komponist_model::project::BUILTIN_HEXA;

    fn descriptor(id: &str, name: &str, kind: PluginKind, features: &[&str]) -> PluginDescriptor {
        PluginDescriptor {
            format: BundleFormat::Clap,
            id: id.into(),
            name: name.into(),
            vendor: "Vendor".into(),
            version: "1.0".into(),
            kind,
            features: features.iter().map(|f| (*f).into()).collect(),
            bundle: format!("/usr/lib/clap/{name}.clap").into(),
            sidechain: false,
        }
    }

    fn features(list: &[&str]) -> Vec<String> {
        list.iter().map(|f| (*f).into()).collect()
    }

    #[test]
    fn effect_categories_follow_the_mapping_table() {
        let cases: [(&[&str], EffectCategory); 12] = [
            (&["audio-effect", "equalizer"], EffectCategory::EqFilter),
            (&["audio-effect", "filter"], EffectCategory::EqFilter),
            (&["fx", "eq"], EffectCategory::EqFilter),
            (&["audio-effect", "compressor"], EffectCategory::Dynamics),
            (
                &["audio-effect", "transient-shaper"],
                EffectCategory::Dynamics,
            ),
            (&["fx", "dynamics"], EffectCategory::Dynamics),
            (&["audio-effect", "reverb"], EffectCategory::ReverbDelay),
            (&["fx", "Delay"], EffectCategory::ReverbDelay),
            (&["audio-effect", "phaser"], EffectCategory::Modulation),
            (&["fx", "modulation"], EffectCategory::Modulation),
            (&["audio-effect", "distortion"], EffectCategory::Other),
            // The first rule in variant order wins.
            (&["delay", "chorus", "filter"], EffectCategory::EqFilter),
        ];
        for (list, expected) in cases {
            assert_eq!(
                EffectCategory::from_features(&features(list)),
                expected,
                "{list:?}"
            );
        }
        assert_eq!(EffectCategory::from_features(&[]), EffectCategory::Other);
    }

    #[test]
    fn builtins_come_first() {
        let catalog = Catalog::builtin();
        let names: Vec<&str> = catalog
            .entries()
            .iter()
            .map(|e| e.plugin.name.as_str())
            .collect();
        assert_eq!(names, builtin_names());
        let hexa = &catalog.entries()[0];
        assert!(hexa.is_builtin());
        assert_eq!(hexa.plugin.id, BUILTIN_HEXA);
        assert_eq!(hexa.plugin.name, "Hexa");
        assert_eq!(hexa.kind, PluginKind::Instrument);
        assert_eq!(hexa.location, PluginLocation::InProcess(BuiltinKind::Hexa));

        // The Sampler is an in-process instrument without a browser category.
        let sampler = &catalog.entries()[1];
        assert!(sampler.is_builtin());
        assert_eq!(sampler.kind, PluginKind::Instrument);
        assert_eq!(sampler.category, None);
        assert_eq!(
            sampler.plugin.name,
            komponist_model::project::DRUM_SAMPLER_NAME
        );
        assert_eq!(catalog.resolve(&sampler.plugin), Some(sampler));

        // The internal effects: in process, effects with a browser category, resolvable.
        let eq = &catalog.entries()[2];
        assert!(eq.is_builtin());
        assert_eq!(eq.kind, PluginKind::Effect);
        assert_eq!(eq.category, Some(EffectCategory::EqFilter));
        assert_eq!(
            eq.location,
            PluginLocation::InProcess(BuiltinKind::Internal(komponist_builtin::Internal::Eq))
        );
        let categories: Vec<_> = catalog.entries()[2..6].iter().map(|e| e.category).collect();
        assert_eq!(
            categories,
            [
                Some(EffectCategory::EqFilter),
                Some(EffectCategory::ReverbDelay),
                Some(EffectCategory::ReverbDelay),
                Some(EffectCategory::Dynamics)
            ]
        );
        assert_eq!(catalog.resolve(&eq.plugin), Some(eq));
    }

    #[test]
    fn scanned_entries_carry_the_module_file_name() {
        let mut catalog = Catalog::builtin();
        let mut vst3 = descriptor("0123ABCD", "Vital", PluginKind::Instrument, &[]);
        vst3.format = BundleFormat::Vst3;
        vst3.bundle = "/home/u/.vst3/Vital.vst3".into();
        catalog.set_scanned(&[
            descriptor("org.surge", "Surge XT", PluginKind::Instrument, &[]),
            vst3,
        ]);
        let module = |name: &str| {
            catalog
                .entries()
                .iter()
                .find(|e| e.plugin.name == name)
                .map(|e| e.plugin.module.clone())
        };
        assert_eq!(module("Surge XT").as_deref(), Some("Surge XT.clap"));
        assert_eq!(module("Vital").as_deref(), Some("Vital.vst3"));
        assert_eq!(module("Hexa").as_deref(), Some(""));
    }

    #[test]
    fn bundled_and_external_references_with_the_same_id_resolve_separately() {
        let mut external = descriptor(
            "org.klang-studio.komponist.volt",
            "Volt",
            PluginKind::Instrument,
            &[],
        );
        external.bundle = "/external/komponist_volt.clap".into();
        let mut bundled = external.clone();
        bundled.bundle = "/app/bundled-clap/komponist_volt.clap".into();
        let mut catalog = Catalog::builtin();
        catalog.set_scanned(std::slice::from_ref(&external));
        let external_ref = scanned_entry(&external, false).plugin;
        let bundled_ref = scanned_entry(&bundled, true).plugin;
        assert!(catalog.resolve(&bundled_ref).is_none());
        assert!(catalog.resolve(&external_ref).is_some());

        let shipped_path = &bundled.bundle;
        let shipped = |d: &PluginDescriptor| &d.bundle == shipped_path;
        catalog.set_scanned_with_bundled(&[external.clone(), bundled.clone()], shipped);
        let location = |plugin| &catalog.resolve(plugin).unwrap().location;
        assert!(
            matches!(location(&bundled_ref), PluginLocation::Bundle { path, .. } if path == shipped_path)
        );
        assert!(
            matches!(location(&external_ref), PluginLocation::Bundle { path, .. } if *path == external.bundle)
        );

        catalog.set_scanned_with_bundled(std::slice::from_ref(&bundled), shipped);
        assert!(catalog.resolve(&external_ref).is_none());
        assert!(catalog.resolve(&bundled_ref).is_some());
    }

    #[test]
    fn macos_bundles_are_named_by_their_bundle_folder() {
        // The module is the bundle folder (`.clap` / `.vst3`), not the binary in `Contents/MacOS`.
        let mut catalog = Catalog::builtin();
        let mut clap = descriptor("org.surge", "Surge XT", PluginKind::Instrument, &[]);
        clap.bundle = "/Library/Audio/Plug-Ins/CLAP/Surge XT.clap".into();
        let mut vst3 = descriptor("0123ABCD", "Vital", PluginKind::Instrument, &[]);
        vst3.format = BundleFormat::Vst3;
        vst3.bundle = "/Users/u/Library/Audio/Plug-Ins/VST3/Vital.vst3".into();
        catalog.set_scanned(&[clap, vst3]);
        let modules: Vec<&str> = catalog
            .entries()
            .iter()
            .filter(|e| !e.is_builtin())
            .map(|e| &*e.plugin.module)
            .collect();
        assert_eq!(modules, ["Surge XT.clap", "Vital.vst3"]);
    }

    #[test]
    fn scanned_entries_are_sorted_filtered_and_resolved() {
        let mut catalog = Catalog::builtin();
        let mut vst3 = descriptor("0123ABCD", "dexed", PluginKind::Instrument, &["instrument"]);
        vst3.format = BundleFormat::Vst3;
        let descriptors = [
            descriptor(
                "org.surge",
                "Surge XT",
                PluginKind::Instrument,
                &["instrument"],
            ),
            descriptor(
                "x.reverb",
                "Big Hall",
                PluginKind::Effect,
                &["audio-effect", "reverb"],
            ),
            descriptor("x.analyzer", "Scope", PluginKind::Other, &["analyzer"]),
            vst3,
            // The same plugin from a later folder.
            descriptor("org.surge", "Surge XT (old)", PluginKind::Instrument, &[]),
        ];
        catalog.set_scanned(&descriptors);
        let names: Vec<&str> = catalog
            .entries()
            .iter()
            .map(|e| e.plugin.name.as_str())
            .collect();
        let mut expected = builtin_names();
        let first = expected.len();
        expected.extend(["Big Hall", "dexed", "Surge XT"]);
        assert_eq!(names, expected);
        let hall = &catalog.entries()[first];
        assert_eq!(hall.category, Some(EffectCategory::ReverbDelay));
        assert_eq!(hall.plugin.format, komponist_model::PluginFormat::Clap);
        assert_eq!(
            catalog.entries()[first + 1].plugin.format,
            komponist_model::PluginFormat::Vst3
        );
        assert_eq!(catalog.entries()[first + 2].category, None);

        let surge = PluginRef {
            format: komponist_model::PluginFormat::Clap,
            id: "org.surge".into(),
            name: "renamed".into(),
            vendor: String::new(),
            version: String::new(),
            module: Box::default(),
        };
        assert_eq!(catalog.resolve(&surge).unwrap().plugin.name, "Surge XT");
        let as_vst3 = PluginRef {
            format: komponist_model::PluginFormat::Vst3,
            ..surge
        };
        assert_eq!(catalog.resolve(&as_vst3), None);

        // A rescan replaces the scanned entries.
        let mut catalog = Catalog::builtin();
        catalog.set_scanned(&descriptors);
        catalog.set_scanned(&descriptors[4..]);
        let names: Vec<&str> = catalog
            .entries()
            .iter()
            .map(|e| e.plugin.name.as_str())
            .collect();
        let mut expected = builtin_names();
        expected.push("Surge XT (old)");
        assert_eq!(names, expected);
    }

    #[test]
    fn klopfer_is_an_ordinary_scanned_plugin_and_old_projects_find_it() {
        let mut catalog = Catalog::builtin();
        assert!(catalog.entries().iter().all(|e| e.plugin.name != "klopfer"));
        catalog.set_scanned(&[descriptor(
            KLOPFER_CLAP_ID,
            "Klopfer",
            PluginKind::Instrument,
            &["instrument"],
        )]);
        let scanned = catalog.entries().last().unwrap();
        assert!(!scanned.is_builtin());
        assert_eq!(scanned.plugin.id, KLOPFER_CLAP_ID);
        // A project from before the sampler names klopfer as a built-in ("INTERN"): it resolves to
        // the scanned plugin, which loads the same state.
        let legacy = PluginRef {
            format: komponist_model::PluginFormat::Builtin,
            id: BUILTIN_KLOPFER.into(),
            name: "klopfer".into(),
            vendor: "Klopfer".into(),
            version: "0.1.0".into(),
            module: Box::default(),
        };
        assert_eq!(catalog.resolve(&legacy), Some(scanned));
        // Without the scanned plugin it is missing (the S06 dialog lists it).
        assert_eq!(Catalog::builtin().resolve(&legacy), None);
        // Other built-in ids do not fall back.
        let other = PluginRef {
            id: "x.other".into(),
            ..legacy
        };
        assert_eq!(catalog.resolve(&other), None);
    }

    #[test]
    fn search_matches_name_vendor_and_features() {
        let mut catalog = Catalog::builtin();
        catalog.set_scanned(&[
            descriptor(
                "x.reverb",
                "Big Hall",
                PluginKind::Effect,
                &["audio-effect", "reverb"],
            ),
            descriptor(
                "x.comp",
                "Squash",
                PluginKind::Effect,
                &["audio-effect", "compressor"],
            ),
        ]);
        let names = |q: &str| -> Vec<String> {
            catalog
                .search(q)
                .iter()
                .map(|e| e.plugin.name.clone())
                .collect()
        };
        let mut all = builtin_names();
        all.extend(["Big Hall", "Squash"]);
        assert_eq!(names(""), all);
        assert_eq!(names("HALL"), ["Hall", "Big Hall"]);
        assert_eq!(names("vendor"), ["Big Hall", "Squash"]);
        // The built-in compressor matches by its feature (WP-93).
        assert_eq!(names("compress"), ["Presse", "Squash"]);
        assert_eq!(names("synth"), ["Hexa", "Volt"]);
        assert!(names("nothing like this").is_empty());
    }

    /// An environment of `(name, value)` pairs for `defaults_from`.
    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<PathBuf> + 'a {
        move |name| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| PathBuf::from(v))
        }
    }

    fn strings(paths: &[PathBuf]) -> Vec<&str> {
        paths.iter().map(|p| p.to_str().unwrap()).collect()
    }

    #[test]
    fn default_folders() {
        let dirs = PluginDirs::defaults_from(
            Platform::Linux,
            &env(&[
                ("HOME", "/home/x"),
                ("CLAP_PATH", "/opt/clap::/usr/lib/clap:/home/x/more"),
            ]),
        );
        let home = Path::new("/home/x");
        assert_eq!(
            dirs.clap,
            [
                home.join(".clap"),
                "/usr/lib/clap".into(),
                "/usr/local/lib/clap".into(),
                "/opt/clap".into(),
                "/home/x/more".into()
            ]
        );
        assert_eq!(
            dirs.vst3,
            [
                home.join(".vst3"),
                "/usr/lib/vst3".into(),
                "/usr/local/lib/vst3".into()
            ]
        );
        let bare = PluginDirs::defaults_from(Platform::Linux, &env(&[]));
        assert_eq!(bare.clap.len(), 2);
        assert_eq!(bare.vst3.len(), 2);
        // The real environment gives at least the system folders.
        let system_clap = match Platform::CURRENT {
            Platform::Macos => PathBuf::from("/Library/Audio/Plug-Ins/CLAP"),
            Platform::Linux => PathBuf::from("/usr/lib/clap"),
            Platform::Windows => env_path("COMMONPROGRAMFILES").unwrap().join("CLAP"),
        };
        assert!(PluginDirs::defaults().clap.contains(&system_clap));
    }

    #[test]
    fn default_folders_on_macos() {
        let dirs = PluginDirs::defaults_from(
            Platform::Macos,
            &env(&[
                ("HOME", "/Users/x"),
                (
                    "CLAP_PATH",
                    "/opt/clap::/Library/Audio/Plug-Ins/CLAP:/Users/x/more",
                ),
            ]),
        );
        let home = Path::new("/Users/x");
        assert_eq!(
            dirs.clap,
            [
                home.join("Library/Audio/Plug-Ins/CLAP"),
                "/Library/Audio/Plug-Ins/CLAP".into(),
                "/opt/clap".into(),
                "/Users/x/more".into()
            ]
        );
        assert_eq!(
            dirs.vst3,
            [
                home.join("Library/Audio/Plug-Ins/VST3"),
                "/Library/Audio/Plug-Ins/VST3".into()
            ]
        );
        let bare = PluginDirs::defaults_from(Platform::Macos, &env(&[]));
        assert_eq!(bare.clap, [PathBuf::from("/Library/Audio/Plug-Ins/CLAP")]);
        assert_eq!(bare.vst3, [PathBuf::from("/Library/Audio/Plug-Ins/VST3")]);
    }

    /// Windows: the folders of the CLAP and VST3 specs, `CLAP_PATH` split at `;` (whatever OS
    /// runs the test), no home-relative folders.
    #[test]
    fn default_folders_on_windows() {
        let common = r"C:\Program Files\Common Files";
        let local = r"C:\Users\x\AppData\Local";
        // The default folder repeated in CLAP_PATH, joined as the code joins it: with `\` on a
        // Windows host, with `/` where the test runs on Linux or macOS (CI), so the duplicate is
        // dropped on every host.
        let repeated = Path::new(common).join("CLAP");
        let clap_path = format!(r"D:\clap;;{};E:\more", repeated.display());
        let dirs = PluginDirs::defaults_from(
            Platform::Windows,
            &env(&[
                ("COMMONPROGRAMFILES", common),
                ("LOCALAPPDATA", local),
                ("HOME", "/home/ignored"),
                ("CLAP_PATH", &clap_path),
            ]),
        );
        let local_common = Path::new(local).join("Programs").join("Common");
        assert_eq!(
            dirs.clap,
            [
                Path::new(common).join("CLAP"),
                local_common.join("CLAP"),
                r"D:\clap".into(),
                r"E:\more".into()
            ],
            "{:?}",
            strings(&dirs.clap)
        );
        assert_eq!(
            dirs.vst3,
            [Path::new(common).join("VST3"), local_common.join("VST3")]
        );
        let bare = PluginDirs::defaults_from(Platform::Windows, &env(&[]));
        assert!(bare.clap.is_empty() && bare.vst3.is_empty());
    }

    #[test]
    fn scan_cache_round_trips_and_tolerates_bad_files() {
        let dir = std::env::temp_dir().join(format!("kp-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested/plugins.json");
        assert_eq!(ScanCache::load(&file), ScanCache::default());
        let cache = ScanCache {
            version: SCAN_CACHE_VERSION,
            bundles: vec![
                CachedBundle {
                    path: "/usr/lib/clap/A.clap".into(),
                    mtime_secs: 1_700_000_000,
                    descriptors: vec![descriptor("a", "A", PluginKind::Effect, &["eq"])],
                    error: None,
                },
                CachedBundle {
                    path: "/usr/lib/clap/B.clap".into(),
                    mtime_secs: 3,
                    descriptors: Vec::new(),
                    error: Some("crashed (signal 11)".into()),
                },
            ],
        };
        cache.save(&file).unwrap();
        assert_eq!(ScanCache::load(&file), cache);
        assert!(
            cache
                .lookup(Path::new("/usr/lib/clap/A.clap"), 1_700_000_000)
                .is_some()
        );
        assert!(cache.lookup(Path::new("/usr/lib/clap/A.clap"), 1).is_none());
        assert_eq!(cache.descriptors().len(), 1);
        std::fs::write(&file, b"{ not json").unwrap();
        assert_eq!(ScanCache::load(&file), ScanCache::default());
        // A cache from before #6 (no version, descriptors without `sidechain`) is dropped, so
        // the next scan finds the sidechain ports.
        std::fs::write(
            &file,
            br#"{"bundles":[{"path":"/usr/lib/clap/A.clap","mtime_secs":3,"descriptors":[
                {"format":"clap","id":"a","name":"A","vendor":"V","version":"1","kind":"effect",
                 "features":[],"bundle":"/usr/lib/clap/A.clap"}],"error":null}]}"#,
        )
        .unwrap();
        assert_eq!(ScanCache::load(&file), ScanCache::default());
        // The current version keeps it.
        let current = String::from_utf8(std::fs::read(&file).unwrap())
            .unwrap()
            .replacen('{', &format!("{{\"version\":{SCAN_CACHE_VERSION},"), 1);
        std::fs::write(&file, current).unwrap();
        let loaded = ScanCache::load(&file);
        assert_eq!(loaded.bundles.len(), 1);
        assert!(!loaded.bundles[0].descriptors[0].sidechain);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
