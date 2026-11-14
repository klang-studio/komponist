//! The plugin catalog: built-in entries plus scanned CLAP and VST3 bundles (ARCHITECTURE §8.3,
//! §8.5).
//!
//! Scanning runs `komponist-bridge --scan <bundle>` per bundle as separate processes, in parallel,
//! with [`SCAN_TIMEOUT`] each, and caches results by path and modification time in
//! `~/.cache/komponist/plugins.json` ([`ScanCache`]). A bundle that crashes or times out is listed
//! as skipped ("1 Plugin übersprungen", S07).

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use komponist_ipc::{PluginDescriptor, PluginFormat as BundleFormat, PluginKind};
use komponist_model::PluginRef;
use komponist_model::project::BUILTIN_KLOPFER;

pub use crate::builtin::KLOPFER_CLAP_ID;

/// Time one bundle scan may take before its process is killed and the bundle skipped.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// File name of the scan cache inside the cache directory (`~/.cache/komponist/`).
pub const SCAN_CACHE_FILE: &str = "plugins.json";

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
    /// Only the built-ins ([`crate::builtin`]): pisynth always; klopfer if `klopfer_bundle` is
    /// `Some` (from [`crate::builtin::find_klopfer_bundle`]).
    pub fn builtin(klopfer_bundle: Option<PathBuf>) -> Catalog {
        Catalog {
            entries: crate::builtin::entries(klopfer_bundle),
        }
    }

    /// All entries: built-ins first, then scanned plugins sorted by name (case-insensitive).
    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    /// The entry for a device's plugin reference (same format and id), if installed.
    ///
    /// A reference to klopfer as a CLAP plugin (a project made with a scanned `Klopfer.clap`)
    /// resolves to the built-in klopfer when that is installed: the scanned duplicate is not
    /// listed ([`Catalog::set_scanned`]), and both load the same CLAP plugin with the same state.
    pub fn resolve(&self, plugin: &PluginRef) -> Option<&CatalogEntry> {
        let same = |e: &&CatalogEntry| e.plugin.format == plugin.format && e.plugin.id == plugin.id;
        self.entries.iter().find(same).or_else(|| {
            (plugin.format == komponist_model::PluginFormat::Clap && plugin.id == KLOPFER_CLAP_ID)
                .then(|| {
                    self.entries
                        .iter()
                        .find(|e| e.plugin.id == BUILTIN_KLOPFER && e.is_builtin())
                })
                .flatten()
        })
    }

    /// Replaces all scanned entries with `descriptors` (built-ins stay). Descriptors of kind
    /// `Other` are left out.
    ///
    /// The same plugin in several bundles (one per folder) is listed once, from the first
    /// descriptor. The bundled klopfer is not listed again as a CLAP plugin while the built-in
    /// entry exists.
    pub fn set_scanned(&mut self, descriptors: &[PluginDescriptor]) {
        self.entries.retain(CatalogEntry::is_builtin);
        let has_klopfer = self.entries.iter().any(|e| e.plugin.id == BUILTIN_KLOPFER);
        let mut seen = HashSet::new();
        let mut scanned: Vec<CatalogEntry> = descriptors
            .iter()
            .filter(|d| d.kind != PluginKind::Other)
            .filter(|d| !(has_klopfer && d.format == BundleFormat::Clap && d.id == KLOPFER_CLAP_ID))
            .filter(|d| seen.insert((d.format, d.id.clone())))
            .map(scanned_entry)
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
fn scanned_entry(d: &PluginDescriptor) -> CatalogEntry {
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
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let clap_path = std::env::var_os("CLAP_PATH");
        PluginDirs::defaults_from(home.as_deref(), clap_path.as_deref())
    }

    /// [`PluginDirs::defaults`] for a given home folder and `$CLAP_PATH` value. Empty entries of
    /// `$CLAP_PATH` and folders listed twice are left out.
    fn defaults_from(home: Option<&Path>, clap_path: Option<&std::ffi::OsStr>) -> PluginDirs {
        let mut clap: Vec<PathBuf> = home.map(|h| h.join(".clap")).into_iter().collect();
        clap.push("/usr/lib/clap".into());
        clap.push("/usr/local/lib/clap".into());
        if let Some(paths) = clap_path {
            clap.extend(std::env::split_paths(paths).filter(|p| !p.as_os_str().is_empty()));
        }
        let mut seen = HashSet::new();
        clap.retain(|p| seen.insert(p.clone()));
        let mut vst3: Vec<PathBuf> = home.map(|h| h.join(".vst3")).into_iter().collect();
        vst3.push("/usr/lib/vst3".into());
        vst3.push("/usr/local/lib/vst3".into());
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

/// The scan cache file. A bundle is rescanned when its path is new or its mtime changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanCache {
    /// Cached bundles.
    pub bundles: Vec<CachedBundle>,
}

impl ScanCache {
    /// Reads the cache; a missing or unreadable file gives an empty cache.
    pub fn load(path: &Path) -> ScanCache {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
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
    use super::*;
    use komponist_model::project::BUILTIN_PISYNTH;

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
    fn builtins_come_first_and_klopfer_only_with_a_bundle() {
        let catalog = Catalog::builtin(None);
        assert_eq!(catalog.entries().len(), 1);
        let pisynth = &catalog.entries()[0];
        assert!(pisynth.is_builtin());
        assert_eq!(pisynth.plugin.id, BUILTIN_PISYNTH);
        assert_eq!(pisynth.plugin.name, "pisynth");
        assert_eq!(pisynth.kind, PluginKind::Instrument);
        assert_eq!(
            pisynth.location,
            PluginLocation::InProcess(BuiltinKind::Pisynth)
        );

        let catalog = Catalog::builtin(Some("/opt/Klopfer.clap".into()));
        let klopfer = &catalog.entries()[1];
        assert!(klopfer.is_builtin());
        assert_eq!(klopfer.plugin.id, BUILTIN_KLOPFER);
        assert_eq!(klopfer.plugin.vendor, "Klopfer");
        assert_eq!(
            klopfer.location,
            PluginLocation::Bundle {
                format: BundleFormat::Clap,
                path: "/opt/Klopfer.clap".into(),
                plugin_id: KLOPFER_CLAP_ID.into(),
            }
        );
        assert_eq!(catalog.resolve(&klopfer.plugin), Some(klopfer));
    }

    #[test]
    fn scanned_entries_are_sorted_filtered_and_resolved() {
        let mut catalog = Catalog::builtin(Some("/opt/Klopfer.clap".into()));
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
            // The same plugin from a later folder, and the bundled klopfer as CLAP.
            descriptor("org.surge", "Surge XT (old)", PluginKind::Instrument, &[]),
            descriptor(KLOPFER_CLAP_ID, "Klopfer", PluginKind::Instrument, &[]),
        ];
        catalog.set_scanned(&descriptors);
        let names: Vec<&str> = catalog
            .entries()
            .iter()
            .map(|e| e.plugin.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["pisynth", "klopfer", "Big Hall", "dexed", "Surge XT"]
        );
        let hall = &catalog.entries()[2];
        assert_eq!(hall.category, Some(EffectCategory::ReverbDelay));
        assert_eq!(hall.plugin.format, komponist_model::PluginFormat::Clap);
        assert_eq!(
            catalog.entries()[3].plugin.format,
            komponist_model::PluginFormat::Vst3
        );
        assert_eq!(catalog.entries()[4].category, None);

        let surge = PluginRef {
            format: komponist_model::PluginFormat::Clap,
            id: "org.surge".into(),
            name: "renamed".into(),
            vendor: String::new(),
            version: String::new(),
        };
        assert_eq!(catalog.resolve(&surge).unwrap().plugin.name, "Surge XT");
        let as_vst3 = PluginRef {
            format: komponist_model::PluginFormat::Vst3,
            ..surge
        };
        assert_eq!(catalog.resolve(&as_vst3), None);

        // A rescan replaces the scanned entries; without the built-in klopfer the CLAP one shows.
        let mut catalog = Catalog::builtin(None);
        catalog.set_scanned(&descriptors);
        catalog.set_scanned(&descriptors[5..]);
        let names: Vec<&str> = catalog
            .entries()
            .iter()
            .map(|e| e.plugin.name.as_str())
            .collect();
        assert_eq!(names, ["pisynth", "Klopfer"]);
    }

    #[test]
    fn a_clap_klopfer_reference_resolves_to_the_builtin() {
        let mut catalog = Catalog::builtin(Some("/opt/Klopfer.clap".into()));
        catalog.set_scanned(&[descriptor(
            KLOPFER_CLAP_ID,
            "Klopfer",
            PluginKind::Instrument,
            &["instrument"],
        )]);
        let as_clap = PluginRef {
            format: komponist_model::PluginFormat::Clap,
            id: KLOPFER_CLAP_ID.into(),
            name: "Klopfer".into(),
            vendor: "Klopfer".into(),
            version: "0.1.0".into(),
        };
        // Listed once (as the built-in), still found for a project that names the CLAP plugin.
        assert_eq!(catalog.entries().len(), 2);
        let entry = catalog.resolve(&as_clap).unwrap();
        assert!(entry.is_builtin());
        assert_eq!(entry.plugin.id, BUILTIN_KLOPFER);
        // Without the built-in, the scanned CLAP entry itself.
        let mut catalog = Catalog::builtin(None);
        catalog.set_scanned(&[descriptor(
            KLOPFER_CLAP_ID,
            "Klopfer",
            PluginKind::Instrument,
            &[],
        )]);
        let entry = catalog.resolve(&as_clap).unwrap();
        assert!(!entry.is_builtin());
        // Other CLAP plugins do not fall back.
        let other = PluginRef {
            id: "x.other".into(),
            ..as_clap
        };
        assert_eq!(catalog.resolve(&other), None);
    }

    #[test]
    fn search_matches_name_vendor_and_features() {
        let mut catalog = Catalog::builtin(None);
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
        assert_eq!(names(""), ["pisynth", "Big Hall", "Squash"]);
        assert_eq!(names("HALL"), ["Big Hall"]);
        assert_eq!(names("vendor"), ["Big Hall", "Squash"]);
        assert_eq!(names("compress"), ["Squash"]);
        assert_eq!(names("synth"), ["pisynth"]);
        assert!(names("nothing like this").is_empty());
    }

    #[test]
    fn default_folders() {
        let dirs = PluginDirs::defaults_from(
            Some(Path::new("/home/x")),
            Some(std::ffi::OsStr::new(
                "/opt/clap::/usr/lib/clap:/home/x/more",
            )),
        );
        let clap: Vec<&str> = dirs.clap.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            clap,
            [
                "/home/x/.clap",
                "/usr/lib/clap",
                "/usr/local/lib/clap",
                "/opt/clap",
                "/home/x/more"
            ]
        );
        let vst3: Vec<&str> = dirs.vst3.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(
            vst3,
            ["/home/x/.vst3", "/usr/lib/vst3", "/usr/local/lib/vst3"]
        );
        let bare = PluginDirs::defaults_from(None, None);
        assert_eq!(bare.clap.len(), 2);
        assert_eq!(bare.vst3.len(), 2);
        // The real environment gives at least the system folders.
        assert!(
            PluginDirs::defaults()
                .clap
                .contains(&PathBuf::from("/usr/lib/clap"))
        );
    }

    #[test]
    fn scan_cache_round_trips_and_tolerates_bad_files() {
        let dir = std::env::temp_dir().join(format!("kp-cache-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("nested/plugins.json");
        assert_eq!(ScanCache::load(&file), ScanCache::default());
        let cache = ScanCache {
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
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
