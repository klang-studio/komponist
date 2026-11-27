//! CLAP preset sources and the preset a device last loaded.
//!
//! A source is what standard CLAP preset discovery reports and standard `clap.preset-load`
//! takes: a location kind with its location and an optional load key. The host records the
//! source of every load it requests and every load the plugin reports through the host side of
//! `clap.preset-load`, and stores it with the device ([`crate::Device::preset`]).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Where a preset lives (CLAP `location_kind` with its location).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ClapPresetLocation {
    /// The preset is embedded in the plug-in. CLAP passes a null location string.
    Plugin,
    /// An absolute UTF-8 path declared by the plug-in's provider.
    File(String),
}

/// The opaque CLAP source of one preset: its location and load key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClapPresetSource {
    pub location: ClapPresetLocation,
    /// The opaque bytes returned by the provider, excluding the trailing NUL.
    pub load_key: Option<Vec<u8>>,
}

/// The preset a device last loaded, recorded by the host through standard CLAP preset-load
/// (a load the host requested or one the plugin reported with `clap_host_preset_load.loaded`).
///
/// In the project file: `{"file": "/abs/path", "load_key": "<base64>", "modified": true}`, where
/// an absent `file` means a preset embedded in the plug-in and `modified` is only written when
/// set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DevicePreset {
    /// The loaded preset.
    pub source: ClapPresetSource,
    /// A parameter of the device was edited after the load.
    pub modified: bool,
}

impl DevicePreset {
    /// A freshly loaded preset.
    pub fn loaded(source: ClapPresetSource) -> Self {
        Self {
            source,
            modified: false,
        }
    }
}

#[derive(Serialize, Deserialize)]
struct DevicePresetFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    load_key: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    modified: bool,
}

impl Serialize for DevicePreset {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        DevicePresetFile {
            file: match &self.source.location {
                ClapPresetLocation::Plugin => None,
                ClapPresetLocation::File(path) => Some(path.clone()),
            },
            load_key: self
                .source
                .load_key
                .as_ref()
                .map(|key| STANDARD.encode(key)),
            modified: self.modified,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for DevicePreset {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let file = DevicePresetFile::deserialize(deserializer)?;
        let load_key = file
            .load_key
            .map(|key| STANDARD.decode(key).map_err(D::Error::custom))
            .transpose()?;
        Ok(Self {
            source: ClapPresetSource {
                location: file
                    .file
                    .map_or(ClapPresetLocation::Plugin, ClapPresetLocation::File),
                load_key,
            },
            modified: file.modified,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_presets_round_trip_through_the_project_form() {
        let embedded = DevicePreset::loaded(ClapPresetSource {
            location: ClapPresetLocation::Plugin,
            load_key: Some(vec![0xff, b'a', 0]),
        });
        let text = serde_json::to_string(&embedded).unwrap();
        assert_eq!(text, r#"{"load_key":"/2EA"}"#);
        assert_eq!(
            serde_json::from_str::<DevicePreset>(&text).unwrap(),
            embedded
        );

        let file = DevicePreset {
            source: ClapPresetSource {
                location: ClapPresetLocation::File("/presets/Warm.fxp".into()),
                load_key: None,
            },
            modified: true,
        };
        let text = serde_json::to_string(&file).unwrap();
        assert_eq!(text, r#"{"file":"/presets/Warm.fxp","modified":true}"#);
        assert_eq!(serde_json::from_str::<DevicePreset>(&text).unwrap(), file);

        assert!(serde_json::from_str::<DevicePreset>(r#"{"load_key":"not base64!"}"#).is_err());
    }
}
