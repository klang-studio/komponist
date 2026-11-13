//! Scanning bundles into [`PluginDescriptor`]s (ARCHITECTURE §8.3).
//!
//! [`crate::scan`] dispatches on the bundle format. Descriptors carry the plugin's features (CLAP
//! features or VST3 subcategories, lower-case) and a broad [`PluginKind`]; the browser categories
//! ("EQ und Filter", …) are derived from the features by `komponist-plugins`.

use std::path::Path;

use komponist_ipc::{PluginDescriptor, PluginFormat, PluginKind};

use crate::plugin::HostingError;

/// Lists the plugins in `bundle` without activating them.
pub(crate) fn scan(bundle: &Path) -> Result<Vec<PluginDescriptor>, HostingError> {
    match crate::bundle_format(bundle) {
        Some(PluginFormat::Clap) => crate::clap::scan(bundle),
        Some(PluginFormat::Vst3) => {
            Err(HostingError::Unsupported("VST3 hosting arrives with WP-32"))
        }
        None => Err(HostingError::Load(format!(
            "{}: not a .clap or .vst3 bundle",
            bundle.display()
        ))),
    }
}

/// The broad kind of a plugin from its lower-case features: an instrument wins over an effect
/// (instruments with an audio input still belong on instrument tracks); note effects, analyzers
/// and anything unlabelled are [`PluginKind::Other`].
///
/// CLAP: `instrument`, `audio-effect`. VST3 subcategories: `instrument` (also `instrument|synth`
/// split into parts), `fx`.
pub(crate) fn kind_from_features(format: PluginFormat, features: &[String]) -> PluginKind {
    let has = |name: &str| features.iter().any(|f| f == name);
    let (instrument, effect) = match format {
        PluginFormat::Clap => ("instrument", "audio-effect"),
        PluginFormat::Vst3 => ("instrument", "fx"),
    };
    if has(instrument) {
        PluginKind::Instrument
    } else if has(effect) {
        PluginKind::Effect
    } else {
        PluginKind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn clap_kinds() {
        let k = |f: &[&str]| kind_from_features(PluginFormat::Clap, &features(f));
        assert_eq!(
            k(&["instrument", "synthesizer", "stereo"]),
            PluginKind::Instrument
        );
        assert_eq!(k(&["audio-effect", "utility"]), PluginKind::Effect);
        assert_eq!(k(&["audio-effect", "instrument"]), PluginKind::Instrument);
        assert_eq!(k(&["note-effect"]), PluginKind::Other);
        assert_eq!(k(&["analyzer"]), PluginKind::Other);
        assert_eq!(k(&[]), PluginKind::Other);
    }

    #[test]
    fn vst3_kinds() {
        let k = |f: &[&str]| kind_from_features(PluginFormat::Vst3, &features(f));
        assert_eq!(k(&["instrument", "synth"]), PluginKind::Instrument);
        assert_eq!(k(&["fx", "eq"]), PluginKind::Effect);
        assert_eq!(k(&["spatial"]), PluginKind::Other);
    }

    #[test]
    fn unknown_extensions_fail() {
        assert!(matches!(
            scan(Path::new("/nonexistent/readme.txt")),
            Err(HostingError::Load(_))
        ));
        assert!(matches!(
            scan(Path::new("/nonexistent/X.vst3")),
            Err(HostingError::Unsupported(_))
        ));
        assert!(matches!(
            scan(Path::new("/nonexistent/X.clap")),
            Err(HostingError::Load(_))
        ));
    }
}
