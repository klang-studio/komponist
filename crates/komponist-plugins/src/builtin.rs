//! The built-in entries of the catalog (ARCHITECTURE §8.5): hexa and the internal devices of
//! `komponist-builtin`, all in process, shown as "INTERN".
//!
//! The drum sampler of WP-87 is one of the internal devices ([`Internal::Sampler`]); it replaced
//! the bundled "klopfer · INTERN". klopfer is an ordinary CLAP plugin now, found by the scan;
//! projects saved with the old built-in entry (`BUILTIN_KLOPFER`) resolve to the scanned plugin
//! ([`crate::Catalog::resolve`]).

use komponist_ipc::PluginKind;
use komponist_model::project::BUILTIN_HEXA;
use komponist_model::{PluginFormat, PluginRef};

use komponist_builtin::Internal;

use crate::catalog::{BuiltinKind, CatalogEntry, EffectCategory, PluginLocation};

/// CLAP id of klopfer (klopfer's `src/lib.rs`), the scanned plugin that projects with the old
/// built-in klopfer entry resolve to.
pub const KLOPFER_CLAP_ID: &str = "com.klopfer.drum-sampler";

/// The built-in entries: hexa, then the internal devices (the Sampler, effects and MIDI
/// effects) in the order of [`Internal::ALL`].
pub(crate) fn entries() -> Vec<CatalogEntry> {
    let mut entries = vec![CatalogEntry {
        plugin: builtin_ref(BUILTIN_HEXA, "hexa", "klang-studio"),
        kind: PluginKind::Instrument,
        category: None,
        features: vec!["instrument".into(), "synthesizer".into()],
        location: PluginLocation::InProcess(BuiltinKind::Hexa),
        sidechain: false,
    }];
    entries.extend(Internal::ALL.into_iter().map(internal_entry));
    entries
}

/// The catalog entry of an internal effect or MIDI effect: name, reference and features come from
/// `komponist_builtin::Internal` (the one place a device is defined).
fn internal_entry(kind: Internal) -> CatalogEntry {
    let features: Vec<String> = kind.features().iter().map(|f| (*f).to_owned()).collect();
    CatalogEntry {
        plugin: kind.plugin_ref(),
        kind: match kind.role() {
            komponist_model::DeviceRole::MidiEffect => PluginKind::MidiEffect,
            komponist_model::DeviceRole::Instrument => PluginKind::Instrument,
            komponist_model::DeviceRole::Effect => PluginKind::Effect,
        },
        category: (kind.role() == komponist_model::DeviceRole::Effect)
            .then(|| EffectCategory::from_features(&features)),
        features,
        location: PluginLocation::InProcess(BuiltinKind::Internal(kind)),
        sidechain: kind.has_sidechain(),
    }
}

/// The reference of a built-in ("INTERN").
fn builtin_ref(id: &str, name: &str, vendor: &str) -> PluginRef {
    PluginRef {
        format: PluginFormat::Builtin,
        id: id.into(),
        name: name.into(),
        vendor: vendor.into(),
        version: env!("CARGO_PKG_VERSION").into(),
        module: Box::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_builtin_entries_are_hexa_then_the_internal_devices() {
        let names: Vec<String> = entries().into_iter().map(|e| e.plugin.name).collect();
        let mut expected = vec!["hexa".to_owned()];
        expected.extend(Internal::ALL.iter().map(|k| k.name().to_owned()));
        assert_eq!(names, expected);
        assert!(
            entries()
                .iter()
                .all(|e| e.plugin.format == PluginFormat::Builtin)
        );
    }

    #[test]
    fn the_sampler_and_volt_are_instruments_the_effects_are_not() {
        for e in entries() {
            let instrument = e.plugin.name == "hexa"
                || e.plugin.name == komponist_builtin::sampler::NAME
                || e.plugin.name == komponist_builtin::internal::volt::NAME;
            assert_eq!(
                e.kind == PluginKind::Instrument,
                instrument,
                "{}",
                e.plugin.name
            );
            assert_eq!(
                e.category.is_none(),
                e.kind != PluginKind::Effect,
                "{}",
                e.plugin.name
            );
        }
    }
}
