//! Standard CLAP preset-discovery indexing and preset-load argument conversion.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{CStr, CString};
use std::fs;
use std::path::{Path, PathBuf};

use clack_extensions::preset_discovery::prelude::*;
use clack_host::prelude::{HostError, HostInfo, PluginEntry};
use komponist_ipc::clap_presets::{
    ALIAS_KEY, ClapPreset, ClapPresetLocation, ClapPresetSource, LOCALIZED_NAME_PREFIX,
};

use crate::HostingError;

const MAX_PROVIDERS: u32 = 128;
const MAX_LOCATIONS: usize = 512;
const MAX_ENTRIES: usize = 8192;
const MAX_PRESETS: usize = 4096;
const MAX_METADATA_BYTES: usize = 4096;
const MAX_METADATA_ITEMS: usize = 32;
const INVALID_DECLARATION: HostError = HostError::Message("invalid CLAP preset declaration");
const INVALID_PRESET: HostError = HostError::Message("invalid CLAP preset metadata");

/// Reads a CLAP bundle's providers in the disposable scan process. A plugin without a
/// discovery factory has an empty list.
pub(super) fn discover(bundle: &Path) -> Result<Vec<ClapPreset>, HostingError> {
    let entry = super::load_entry(bundle)?;
    let host = HostInfo::new(
        "komponist",
        super::HOST_VENDOR,
        "",
        env!("CARGO_PKG_VERSION"),
    )
    .map_err(|error| HostingError::Load(error.to_string()))?;
    Ok(discover_entry(&entry, &host))
}

fn discover_entry(entry: &PluginEntry, host: &HostInfo) -> Vec<ClapPreset> {
    let Some(factory) = entry.get_factory::<PresetDiscoveryFactory>() else {
        return Vec::new();
    };
    let mut presets = Vec::new();
    let mut seen = BTreeSet::new();
    let mut entries = MAX_ENTRIES;
    for index in 0..factory.provider_count().min(MAX_PROVIDERS) {
        let Some(provider_id) = factory.get_provider_descriptor(index).and_then(|d| d.id()) else {
            continue;
        };
        let Ok(mut provider) = Provider::instantiate(Indexer::default(), entry, provider_id, host)
        else {
            continue;
        };
        for (location, flags) in provider.indexer().locations.clone() {
            let sources = match location {
                ClapPresetLocation::Plugin => vec![ClapPresetLocation::Plugin],
                ClapPresetLocation::File(path) => preset_files(
                    Path::new(&path),
                    &provider.indexer().extensions,
                    &mut entries,
                ),
            };
            for location in sources {
                if presets.len() >= MAX_PRESETS {
                    return presets;
                }
                let mut receiver = Receiver {
                    location: &location,
                    default_flags: flags,
                    limit: MAX_PRESETS - presets.len(),
                    current: None,
                    found: Vec::new(),
                    failed: false,
                };
                let path = match &location {
                    ClapPresetLocation::Plugin => None,
                    ClapPresetLocation::File(path) => CString::new(path.as_str()).ok(),
                };
                provider.get_metadata(location_arg(path.as_deref()), &mut receiver);
                receiver.finish_preset();
                if !receiver.failed {
                    for preset in receiver.found {
                        let key = (preset.plugin_id.clone(), preset.source.clone());
                        if seen.insert(key) {
                            presets.push(preset);
                        }
                    }
                }
            }
        }
    }
    presets
}

fn preset_files(
    root: &Path,
    extensions: &BTreeSet<String>,
    budget: &mut usize,
) -> Vec<ClapPresetLocation> {
    let mut files = Vec::new();
    let Ok(info) = fs::metadata(root) else {
        return files;
    };
    if info.is_file() {
        if *budget != 0 {
            *budget -= 1;
            if let Some(path) = root.to_str() {
                files.push(ClapPresetLocation::File(path.to_owned()));
            }
        }
        return files;
    }
    if !info.is_dir() {
        return files;
    }
    let mut folders = vec![root.to_path_buf()];
    while *budget != 0 {
        let Some(folder) = folders.pop() else { break };
        // Only the explicitly declared root follows links. The walk never follows child links.
        if folder != root && !fs::symlink_metadata(&folder).is_ok_and(|info| info.is_dir()) {
            continue;
        }
        let Ok(children) = fs::read_dir(folder) else {
            continue;
        };
        for item in children.take(*budget).flatten() {
            *budget -= 1;
            let Ok(file_type) = item.file_type() else {
                continue;
            };
            let path = item.path();
            if file_type.is_dir() {
                folders.push(path);
            } else if file_type.is_file()
                && matches_extension(&path, extensions)
                && let Some(path) = path.to_str()
            {
                files.push(ClapPresetLocation::File(path.to_owned()));
            }
        }
    }
    files.sort_unstable();
    files
}

fn matches_extension(path: &Path, extensions: &BTreeSet<String>) -> bool {
    extensions.is_empty()
        || extensions.contains("")
        || path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|ext| extensions.contains(&ext.to_ascii_lowercase()))
}

#[derive(Default)]
struct Indexer {
    locations: Vec<(ClapPresetLocation, Flags)>,
    extensions: BTreeSet<String>,
}

impl IndexerImpl for Indexer {
    fn declare_filetype(&mut self, file_type: FileType) -> Result<(), HostError> {
        if self.extensions.len() < MAX_LOCATIONS {
            let extension = file_type.file_extension.unwrap_or(c"");
            if let Some(extension) = text(extension) {
                self.extensions.insert(extension.to_ascii_lowercase());
            }
        }
        Ok(())
    }

    fn declare_location(&mut self, location: LocationInfo) -> Result<(), HostError> {
        let location_value = match location.location {
            Location::Plugin => ClapPresetLocation::Plugin,
            Location::File { path } => {
                let path = text(path).ok_or(INVALID_DECLARATION)?;
                if !PathBuf::from(&path).is_absolute() {
                    return Err(INVALID_DECLARATION);
                }
                ClapPresetLocation::File(path)
            }
        };
        if self.locations.len() >= MAX_LOCATIONS {
            return Err(INVALID_DECLARATION);
        }
        let row = (location_value, location.flags);
        if !self.locations.contains(&row) {
            self.locations.push(row);
        }
        Ok(())
    }

    fn declare_soundpack(&mut self, _: Soundpack) -> Result<(), HostError> {
        Ok(())
    }
}

struct Receiver<'a> {
    location: &'a ClapPresetLocation,
    default_flags: Flags,
    limit: usize,
    current: Option<PendingPreset>,
    found: Vec<ClapPreset>,
    failed: bool,
}

struct PendingPreset {
    source: ClapPresetSource,
    name: String,
    localized_names: BTreeMap<String, String>,
    aliases: Vec<String>,
    features: Vec<String>,
    flags: Flags,
    plugin_ids: BTreeSet<String>,
}

impl Receiver<'_> {
    fn finish_preset(&mut self) {
        let Some(preset) = self.current.take() else {
            return;
        };
        for id in preset
            .plugin_ids
            .into_iter()
            .take(self.limit.saturating_sub(self.found.len()))
        {
            self.found.push(ClapPreset {
                plugin_id: id,
                source: preset.source.clone(),
                name: preset.name.clone(),
                localized_names: preset.localized_names.clone(),
                aliases: preset.aliases.clone(),
                features: preset.features.clone(),
                factory: preset.flags.contains(Flags::IS_FACTORY_CONTENT),
            });
        }
    }
}

impl MetadataReceiverImpl for Receiver<'_> {
    fn on_error(&mut self, _: i32, _: Option<&CStr>) {
        self.failed = true;
    }

    fn begin_preset(
        &mut self,
        name: Option<&CStr>,
        load_key: Option<&CStr>,
    ) -> Result<(), HostError> {
        self.finish_preset();
        if self.found.len() >= self.limit || self.failed {
            return Err(INVALID_PRESET);
        }
        let load_key = load_key.map(CStr::to_bytes);
        if load_key.is_some_and(|key| key.len() > MAX_METADATA_BYTES) {
            return Err(INVALID_PRESET);
        }
        let fallback = match self.location {
            ClapPresetLocation::Plugin if load_key.is_none_or(<[u8]>::is_empty) => {
                return Err(INVALID_PRESET);
            }
            ClapPresetLocation::Plugin => None,
            ClapPresetLocation::File(path) => {
                Path::new(path).file_stem().and_then(|stem| stem.to_str())
            }
        };
        let name = name
            .and_then(text)
            .filter(|name| !name.is_empty())
            .or_else(|| fallback.map(str::to_owned))
            .ok_or(INVALID_PRESET)?;
        self.current = Some(PendingPreset {
            source: ClapPresetSource {
                location: self.location.clone(),
                load_key: load_key.map(<[u8]>::to_vec),
            },
            name,
            localized_names: BTreeMap::new(),
            aliases: Vec::new(),
            features: Vec::new(),
            flags: self.default_flags,
            plugin_ids: BTreeSet::new(),
        });
        Ok(())
    }

    fn add_plugin_id(&mut self, id: UniversalPluginId) {
        if id.abi == c"clap"
            && let Some(current) = &mut self.current
            && current.plugin_ids.len() < MAX_METADATA_ITEMS
            && let Some(id) = text(id.id).filter(|value| !value.is_empty())
        {
            current.plugin_ids.insert(id);
        }
    }

    fn set_flags(&mut self, flags: Flags) {
        if let Some(current) = &mut self.current {
            current.flags = flags;
        }
    }

    fn add_feature(&mut self, feature: &CStr) {
        if let Some(current) = &mut self.current {
            add_text(&mut current.features, feature);
        }
    }

    fn add_extra_info(&mut self, key: &CStr, value: &CStr) {
        let Some(current) = &mut self.current else {
            return;
        };
        if key == ALIAS_KEY {
            add_text(&mut current.aliases, value);
        } else if let Ok(key) = key.to_str()
            && let Some(language) = key.strip_prefix(LOCALIZED_NAME_PREFIX)
            && !language.is_empty()
            && language.len() <= 32
            && language
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && current.localized_names.len() < MAX_METADATA_ITEMS
            && let Some(value) = text(value).filter(|value| !value.is_empty())
        {
            current
                .localized_names
                .entry(language.to_owned())
                .or_insert(value);
        }
    }

    fn set_soundpack_id(&mut self, _: &CStr) {}
    fn add_creator(&mut self, _: &CStr) {}
    fn set_description(&mut self, _: &CStr) {}
    fn set_timestamps(&mut self, _: Option<Timestamp>, _: Option<Timestamp>) {}
}

fn text(value: &CStr) -> Option<String> {
    (value.to_bytes().len() <= MAX_METADATA_BYTES)
        .then(|| value.to_str().ok().map(str::to_owned))
        .flatten()
}

fn add_text(values: &mut Vec<String>, value: &CStr) {
    if values.len() < MAX_METADATA_ITEMS
        && let Some(value) = text(value).filter(|value| !value.is_empty())
        && !values.contains(&value)
    {
        values.push(value);
    }
}

pub(super) fn location_arg(path: Option<&CStr>) -> Location<'_> {
    path.map_or(Location::Plugin, |path| Location::File { path })
}

#[cfg(test)]
mod tests;
