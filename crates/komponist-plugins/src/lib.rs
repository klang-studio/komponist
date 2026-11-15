//! The app side of plugin hosting in komponist.
//!
//! The catalog (built-in pisynth and internal devices plus scanned CLAP and VST3 plugins), the scan
//! cache, the lifecycle of bridge processes, device status and the `ExternalProcessor` that feeds a
//! bridge from the engine.
//!
//! Module map (ARCHITECTURE §8):
//! - [`builtin`]: the built-in entries (pisynth and the internal devices, among them the Sampler).
//! - [`catalog`]: [`Catalog`], [`CatalogEntry`], [`PluginDirs`], scan cache and report.
//! - [`manager`]: [`PluginManager`], [`DeviceStatus`], [`PluginManagerEvent`], [`MissingPlugin`].
//! - [`params`]: bridged plugin parameters as `komponist_model::params::ParamInfo`.
//! - [`external`]: [`ExternalProcessor`], the engine's `Processor` for a bridge.
//!
//! This crate links no plugin format (only `komponist-bridge` does) and no UI.
//!
//! Internals: `bridge` (one bridge process: spawn, writer thread, messages, watchdog) and `scan`
//! (bundle discovery and scan processes).

mod bridge;
pub mod builtin;
pub mod catalog;
pub mod external;
pub mod manager;
pub mod params;
mod scan;

pub use catalog::{
    BuiltinKind, Catalog, CatalogEntry, EffectCategory, PluginDirs, PluginLocation, ScanCache,
    ScanReport,
};
pub use external::ExternalProcessor;
pub use manager::{
    BRIDGE_FLAG, DeviceStatus, FailReason, MissingPlugin, PluginError, PluginManager,
    PluginManagerConfig, PluginManagerEvent, ProcessorSetup,
};

/// How long a bridged device stays `Loading` after its bridge reported the plugin active (also
/// after a re-activation for another rate). Audio already flows meanwhile; the time covers plugins
/// that report active before they can sound: klopfer prepares its kit in the background (WP-54 saw
/// about 70 ms of silence; WP-61 measured 0.2–1 ms in realtime and 9 ms offline, see
/// `docs/PERFORMANCE.md`). Exports wait for `Ok`, so they do not miss the first hits.
pub const WARM_UP: std::time::Duration = std::time::Duration::from_millis(250);

// The shared-memory audio buffers must hold the engine's largest block.
const _: () = assert!(komponist_ipc::shm::MAX_BLOCK == komponist_engine::MAX_BLOCK);
