//! Catalog and processor factory for the devices linked into komponist.

use std::sync::Arc;

use komponist_device_core::CatalogSpec;
use komponist_engine::{DeviceMeter, Processor};
use komponist_i18n::Lang;
use komponist_model::{PluginFormat, PluginRef};
use komponist_sampler::SamplerHandle;

#[cfg(test)]
mod i18n;
mod internal;

pub use internal::Internal;

/// Which built-in a catalog entry or project device refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinKind {
    /// Hexa, the six-operator FM instrument.
    Hexa,
    /// A device using the shared processor and parameter-map state format.
    Internal(Internal),
}

impl BuiltinKind {
    /// All devices in catalog order.
    pub fn all() -> impl Iterator<Item = Self> {
        std::iter::once(Self::Hexa).chain(Internal::ALL.into_iter().map(Self::Internal))
    }

    /// Resolves a project's built-in device reference.
    pub fn from_plugin(plugin: &PluginRef) -> Option<Self> {
        if plugin.format != PluginFormat::Builtin {
            return None;
        }
        if plugin.id == komponist_hexa::CATALOG.id {
            return Some(Self::Hexa);
        }
        Internal::from_id(&plugin.id).map(Self::Internal)
    }

    /// The catalog metadata supplied by the device.
    pub fn catalog(self) -> &'static CatalogSpec {
        match self {
            Self::Hexa => &komponist_hexa::CATALOG,
            Self::Internal(kind) => kind.catalog(),
        }
    }

    /// Why the saved `state` cannot be read, if it cannot (corrupted, or from a newer build). The
    /// processor then plays the default values; the host must keep the stored bytes and tell the
    /// user instead of saving over them.
    pub fn state_error(self, state: Option<&[u8]>) -> Option<String> {
        match self {
            Self::Hexa => komponist_hexa::decode_state(state)
                .err()
                .map(|e| e.to_string()),
            Self::Internal(kind) => kind.state_error(state).map(|e| e.to_string()),
        }
    }

    /// Creates the device's processor from its saved state. The host activates it and loads any
    /// linked samples using the project directory.
    pub fn processor_from_state(self, state: Option<&[u8]>) -> BuiltinProcessor {
        match self {
            Self::Hexa => BuiltinProcessor {
                processor: Box::new(komponist_hexa::processor_from_state(state)),
                meter: None,
                sampler: None,
            },
            Self::Internal(kind) => {
                let (processor, sampler) = kind.processor_from_state(state);
                BuiltinProcessor {
                    meter: Some(processor.meter()),
                    processor: Box::new(processor),
                    sampler,
                }
            }
        }
    }
}

/// A built-in processor and the handles the app uses outside the audio thread.
pub struct BuiltinProcessor {
    /// The processor to activate and insert into the engine.
    pub processor: Box<dyn Processor>,
    /// The meter fed by the processor, when provided by the device.
    pub meter: Option<Arc<DeviceMeter>>,
    /// The sample loading and visualization handle, when provided by the device.
    pub sampler: Option<SamplerHandle>,
}

/// The localized name for a built-in device reference.
pub fn device_name(lang: Lang, plugin: &PluginRef) -> Option<&'static str> {
    let kind = BuiltinKind::from_plugin(plugin)?;
    Some((kind.catalog().name_in)(lang))
}
