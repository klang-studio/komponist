//! Preset catalog data collected through standard CLAP preset discovery.

use std::collections::BTreeMap;
use std::ffi::CStr;

use serde::{Deserialize, Serialize};

/// Optional namespaced metadata keys carried by CLAP's `add_extra_info` callback.
pub const ALIAS_KEY: &CStr = c"org.klang-studio.komponist.preset-alias";
pub const LOCALIZED_NAME_PREFIX: &str = "org.klang-studio.komponist.preset-name.";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ClapPresetLocation {
    /// The preset is embedded in the plug-in. CLAP passes a null location string.
    Plugin,
    /// An absolute UTF-8 path declared by the plug-in's provider.
    File(String),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClapPresetSource {
    pub location: ClapPresetLocation,
    /// The opaque bytes returned by the provider, excluding the trailing NUL.
    pub load_key: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClapPreset {
    pub plugin_id: String,
    pub source: ClapPresetSource,
    pub name: String,
    pub localized_names: BTreeMap<String, String>,
    pub aliases: Vec<String>,
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

    /// Names are for adapting older references. Use the opaque source for new selections.
    pub fn matches_name(&self, saved_name: &str) -> bool {
        self.name == saved_name || self.aliases.iter().any(|alias| alias == saved_name)
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
