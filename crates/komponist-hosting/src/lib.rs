//! The plugin format layer of komponist: CLAP and VST3 hosting and scanning.
//!
//! Everything behind the [`HostedPlugin`] / [`HostedProcessor`] traits. Only `komponist-bridge`
//! links this crate, so plugin code never runs inside the app process. Which implementation sits
//! behind the traits (an adapter onto truce-rack, or an own layer on clack-host 0.2 + vst3 0.3) is
//! decided by spike WP-15 (decision D3); the traits are shaped so either stays thin.
//!
//! Module map (ARCHITECTURE §8.2–8.3):
//! - [`plugin`]: [`HostedPlugin`] (main thread), [`HostedProcessor`] (RT thread), [`ProcessBlock`].
//! - [`host`]: [`HostLoop`] (timers, fds), [`HostNotify`] (plugin → host notifications),
//!   [`HostContext`].
//! - [`load`] and [`scan`]: entry points, dispatching on the bundle format.
//! - [`host_loop`]: [`host_loop::PollLoop`], a reference `poll()` implementation of [`HostLoop`]
//!   with the write-interest policy for plugin fds (used by the tests; the bridge owns its loop).
//!
//! Format checklist for either implementation (ARCHITECTURE §8.2): CLAP audio-ports, note-ports,
//! params, state, latency, gui (x11, embedded), timer-support, posix-fd-support, thread-check, log,
//! render, note-name; VST3 IComponent / IAudioProcessor / IEditController (+ IConnectionPoint),
//! IHostApplication, IComponentHandler, IParameterChanges / IEventList, IBStream, IPlugView (X11
//! embed), IPlugFrame, Linux IRunLoop, IPlugViewContentScaleSupport.
//!
//! Shared vocabulary comes from `komponist-ipc` and is re-exported here: [`PluginDescriptor`],
//! [`ParamInfo`], [`NoteName`], [`RenderMode`], [`GuiSize`].

// Contract stage (WP-01): bodies are `todo!()` until WP-26 / WP-32 implement them, so parameters
// are unused for now. Remove this allow when the implementation lands.
#![allow(unused_variables)]

use std::path::Path;

/// Unit tests check RT paths with `assert_no_alloc`, which needs its allocator.
#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

pub mod host;
pub mod host_loop;
pub mod plugin;

pub use host::{FdEvents, HostContext, HostLoop, HostNotification, HostNotify, TimerId};
pub use komponist_ipc::{
    GuiSize, NoteName, ParamInfo, PluginDescriptor, PluginFormat, PluginKind, RenderMode,
};
pub use plugin::{
    ActivateConfig, EventOut, GuiInfo, HostedPlugin, HostedProcessor, HostingError, ProcessBlock,
    ProcessStatus,
};

/// Bridge main thread. Loads plugin `plugin_id` from `bundle` and initialises it (not activated).
/// The calling thread becomes the plugin's main thread for the lifetime of the instance.
pub fn load(
    format: PluginFormat,
    bundle: &Path,
    plugin_id: &str,
    host: HostContext,
) -> Result<Box<dyn HostedPlugin>, HostingError> {
    todo!("WP-26 (CLAP), WP-32 (VST3)")
}

/// Lists the plugins in a bundle (`komponist-bridge --scan`), without activating them. The format
/// comes from [`bundle_format`]. Runs in a throwaway process, so a crashing bundle only fails its
/// own scan.
pub fn scan(bundle: &Path) -> Result<Vec<PluginDescriptor>, HostingError> {
    todo!("WP-26 (CLAP), WP-32 (VST3)")
}

/// The format of a bundle path by its extension (`.clap`, `.vst3`), case-insensitive.
pub fn bundle_format(path: &Path) -> Option<PluginFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "clap" => Some(PluginFormat::Clap),
        "vst3" => Some(PluginFormat::Vst3),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use komponist_ipc::WireEvent;

    #[test]
    fn bundle_format_by_extension() {
        assert_eq!(
            bundle_format(Path::new("/x/Klopfer.clap")),
            Some(PluginFormat::Clap)
        );
        assert_eq!(
            bundle_format(Path::new("/x/Vital.VST3")),
            Some(PluginFormat::Vst3)
        );
        assert_eq!(bundle_format(Path::new("/x/readme.txt")), None);
        assert_eq!(bundle_format(Path::new("/x/noext")), None);
    }

    #[test]
    fn event_out_counts_drops() {
        let mut slots = [WireEvent::default(); 2];
        let mut out = EventOut::new(&mut slots);
        let e = WireEvent {
            offset: 7,
            ..WireEvent::default()
        };
        assert!(out.push(e) && out.push(e));
        assert!(!out.push(e));
        assert_eq!(out.as_slice().len(), 2);
        assert_eq!(out.dropped(), 1);
    }

    #[test]
    fn fd_events_bits() {
        let rw = FdEvents::READ.union(FdEvents::WRITE);
        assert!(rw.contains(FdEvents::READ) && rw.contains(FdEvents::WRITE));
        assert!(!rw.contains(FdEvents::ERROR));
    }
}
