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
//!   (used by the tests and direct hosting; the bridge owns its loop), built like the bridge's
//!   loop on [`host_loop::PluginFds`] (plugin fds and their write-interest policy) and
//!   [`host_loop::PendingNotifications`] (notification bits and the wake); on Windows a
//!   message-pumping wait (`host_loop::wake`) shared with the bridge.
//! - `adapter` (private): what the CLAP and VST3 processors share: the stereo adaptation of a
//!   plugin's audio ports and the held notes behind all-notes-off (#243).
//! - `clap` (private): the CLAP format layer on clack-host 0.2 (WP-26).
//! - `library` (private): `dlopen` (Windows: `LoadLibraryExW` + pin) for both layers; plugin
//!   libraries are never unmapped.
//! - `bundle` (private, macOS): `CFBundle` of a `.clap` / `.vst3` directory and its executable.
//! - `vst3` (private): the VST3 format layer on the raw vst3 0.3 bindings (WP-32). Its parameters
//!   are normalized (`0..=1`); its state blob format is documented in `vst3::stream`.
//!
//! Format checklist for either implementation (ARCHITECTURE §8.2): CLAP audio-ports, note-ports,
//! params, state, latency, gui (x11, cocoa or win32, embedded), timer-support, posix-fd-support (Unix),
//! thread-check, log, render, note-name; VST3 IComponent / IAudioProcessor / IEditController (+ IConnectionPoint),
//! IHostApplication, IComponentHandler, IParameterChanges / IEventList, IBStream, IPlugView (X11
//! or NSView embed), IPlugFrame, Linux IRunLoop (Unix), IPlugViewContentScaleSupport.
//!
//! Shared vocabulary comes from `komponist-ipc` and is re-exported here: [`PluginDescriptor`],
//! [`ParamInfo`], [`NoteName`], [`RenderMode`], [`GuiSize`].

// Missing SAFETY comments: 81 (macOS build; the VST3 and CLAP host code is the bulk). Remove this allow when the crate is clean (#241 follow-up).
#![allow(clippy::undocumented_unsafe_blocks)]

use std::path::Path;

/// Unit tests check RT paths with `assert_no_alloc`, which needs its allocator.
#[cfg(test)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod adapter;
#[cfg(target_os = "macos")]
mod bundle;
mod clap;
pub mod host;
pub mod host_loop;
mod library;
pub mod plugin;
mod scan;
mod vst3;
mod waveform_markers;

pub use host::{FdEvents, HostContext, HostLoop, HostNotification, HostNotify, TimerId};
pub use komponist_ipc::{
    GuiSize, NoteName, PluginDescriptor, PluginFormat, PluginKind, RenderMode,
};
pub use komponist_plugin_api::params::ParamInfo;
pub use plugin::{
    ActivateConfig, EventOut, GUI_EMBEDDING, GuiInfo, GuiParent, HostedPlugin, HostedProcessor,
    HostingError, ProcessBlock, ProcessStatus,
};

pub use waveform_markers::edit_waveform_marker_state;

/// Bridge main thread. Loads plugin `plugin_id` from `bundle` and initialises it (not activated).
/// The calling thread becomes the plugin's main thread for the lifetime of the instance.
pub fn load(
    format: PluginFormat,
    bundle: &Path,
    plugin_id: &str,
    host: HostContext,
) -> Result<Box<dyn HostedPlugin>, HostingError> {
    match format {
        PluginFormat::Clap => clap::load(bundle, plugin_id, host),
        PluginFormat::Vst3 => vst3::load(bundle, plugin_id, host),
    }
}

/// Lists the plugins in a bundle (`komponist-bridge --scan`), without activating them. The format
/// comes from [`bundle_format`]. Runs in a throwaway process, so a crashing bundle only fails its
/// own scan.
pub fn scan(bundle: &Path) -> Result<Vec<PluginDescriptor>, HostingError> {
    scan::scan(bundle)
}

/// Lists presets from a CLAP bundle's discovery factory without activating a plug-in.
/// Call from a disposable process when inspecting external plug-ins.
pub fn discover_clap_presets(
    bundle: &Path,
) -> Result<Vec<komponist_plugin_api::clap_presets::ClapPreset>, HostingError> {
    if bundle_format(bundle) != Some(PluginFormat::Clap) {
        return Err(HostingError::Unsupported(
            "preset discovery requires a CLAP bundle",
        ));
    }
    clap::discover_presets(bundle)
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

// nice-plug's Windows fixtures share a window class namespace in the test process.
// Class names use only a performance-counter tick, which parallel factory calls can share.
#[cfg(all(test, windows))]
fn lock_fixture_creation() -> std::sync::MutexGuard<'static, ()> {
    static CREATION: std::sync::Mutex<()> = std::sync::Mutex::new(());
    CREATION
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../scripts/test-fixtures.rs"]
mod test_fixtures;
