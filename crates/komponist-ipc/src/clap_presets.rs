//! Preset catalog data collected through standard CLAP preset discovery.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Optional namespaced metadata key prefix carried by CLAP's `add_extra_info` callback.
pub const LOCALIZED_NAME_PREFIX: &str = "org.klang-studio.komponist.preset-name.";

pub use komponist_model::clap_preset::{ClapPresetLocation, ClapPresetSource, DevicePreset};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClapPreset {
    pub plugin_id: String,
    pub source: ClapPresetSource,
    pub name: String,
    pub localized_names: BTreeMap<String, String>,
    pub features: Vec<String>,
    pub factory: bool,
}

impl ClapPreset {
    /// Preset group advertised by the provider's first category feature.
    pub fn category(&self) -> Option<&str> {
        self.features.first().map(String::as_str)
    }

    /// Uses the provider's default display name if the requested language is unavailable.
    pub fn display_name(&self, language: &str) -> &str {
        self.localized_names
            .iter()
            .find(|(tag, _)| tag.eq_ignore_ascii_case(language))
            .map_or(&self.name, |(_, name)| name.as_str())
    }
}

/// Main-thread operations that apply a preset to a running instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetLoad {
    /// Source advertised by standard CLAP preset discovery.
    Discovered(ClapPreset),
    /// Opaque state produced with the CLAP preset context.
    UserState(Vec<u8>),
}

/// A preset change the plug-in reported through the host side of standard `clap.preset-load`
/// outside a load the host requested (its own browser, a program change).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetReport {
    /// `clap_host_preset_load.loaded`: the plug-in loaded this preset. `startup`: reported while
    /// the instance was being activated, so it names the preset the instance starts in.
    Loaded {
        source: ClapPresetSource,
        startup: bool,
    },
    /// `clap_host_preset_load.on_error`: loading this preset failed.
    Failed {
        source: ClapPresetSource,
        /// The plug-in's message and OS error, for the log.
        message: String,
    },
}

/// The index of the device's recorded preset among a running instance's discovered presets,
/// and whether a parameter was edited since it was loaded.
pub fn selected_preset(
    selection: Option<&DevicePreset>,
    presets: &[ClapPreset],
) -> Option<(usize, bool)> {
    let selection = selection?;
    let index = presets
        .iter()
        .position(|preset| preset.source == selection.source)?;
    Some((index, selection.modified))
}
