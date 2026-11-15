//! Scanning bundles into [`PluginDescriptor`]s (ARCHITECTURE §8.3).
//!
//! [`crate::scan`] dispatches on the bundle format. Descriptors carry the plugin's features (CLAP
//! features or VST3 subcategories, lower-case) and a broad [`PluginKind`]; the browser categories
//! ("EQ und Filter", …) are derived from the features by `komponist-plugins`.
//!
//! Whether a plugin has a sidechain input ([`PluginDescriptor::sidechain`], #6) is not in the
//! factory's metadata: the scan instantiates each plugin once (CLAP `init`, VST3
//! `IComponent::initialize`), reads its audio input ports, and destroys it again, without
//! activating it. It does so with [`probe_context`], a host that offers nothing (timers and fds
//! are accepted and never fire). A plugin that fails to instantiate is still listed, without a
//! sidechain.

use std::os::fd::RawFd;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use komponist_ipc::{LogLevel, PluginDescriptor, PluginFormat, PluginKind};

use crate::host::{FdEvents, HostContext, HostLoop, HostNotification, HostNotify, TimerId};
use crate::plugin::HostingError;

/// The host context of a scan's short-lived instances: timers and fds are accepted and never
/// fire, notifications and log lines are dropped.
pub(crate) fn probe_context() -> HostContext {
    HostContext {
        host_loop: Rc::new(ProbeLoop::default()),
        notify: Arc::new(ProbeNotify),
        host_name: "komponist",
        host_version: env!("CARGO_PKG_VERSION"),
    }
}

/// [`HostLoop`] of [`probe_context`].
#[derive(Default)]
struct ProbeLoop {
    next_timer: std::cell::Cell<u32>,
}

impl HostLoop for ProbeLoop {
    fn register_timer(&self, _period: Duration) -> Result<TimerId, HostingError> {
        let id = self.next_timer.get();
        self.next_timer.set(id.wrapping_add(1));
        Ok(TimerId(id))
    }

    fn unregister_timer(&self, _timer: TimerId) -> Result<(), HostingError> {
        Ok(())
    }

    fn register_fd(&self, _fd: RawFd, _events: FdEvents) -> Result<(), HostingError> {
        Ok(())
    }

    fn modify_fd(&self, _fd: RawFd, _events: FdEvents) -> Result<(), HostingError> {
        Ok(())
    }

    fn unregister_fd(&self, _fd: RawFd) -> Result<(), HostingError> {
        Ok(())
    }
}

/// [`HostNotify`] of [`probe_context`].
struct ProbeNotify;

impl HostNotify for ProbeNotify {
    fn notify(&self, _notification: HostNotification) {}

    fn log(&self, _level: LogLevel, _message: &str) {}
}

/// Lists the plugins in `bundle` without activating them.
pub(crate) fn scan(bundle: &Path) -> Result<Vec<PluginDescriptor>, HostingError> {
    match crate::bundle_format(bundle) {
        Some(PluginFormat::Clap) => crate::clap::scan(bundle),
        Some(PluginFormat::Vst3) => crate::vst3::scan(bundle),
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
            Err(HostingError::Load(_))
        ));
        assert!(matches!(
            scan(Path::new("/nonexistent/X.clap")),
            Err(HostingError::Load(_))
        ));
    }
}
