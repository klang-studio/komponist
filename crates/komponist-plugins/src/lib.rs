//! The app side of plugin hosting in komponist.
//!
//! The catalog (built-in pisynth and klopfer entries plus scanned CLAP and VST3 plugins), the scan
//! cache, the lifecycle of bridge processes, device status and the `ExternalProcessor` that feeds a
//! bridge from the engine.
//!
//! Module map (ARCHITECTURE §8):
//! - [`builtin`]: the built-in entries (pisynth, klopfer · INTERN), klopfer's bundle lookup, pads
//!   and kit state.
//! - [`catalog`]: [`Catalog`], [`CatalogEntry`], [`PluginDirs`], scan cache and report.
//! - [`manager`]: [`PluginManager`], [`DeviceStatus`], [`PluginManagerEvent`], [`MissingPlugin`].
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
mod scan;

pub use catalog::{
    BuiltinKind, Catalog, CatalogEntry, EffectCategory, PluginDirs, PluginLocation, ScanCache,
    ScanReport,
};
pub use external::ExternalProcessor;
pub use manager::{
    DeviceStatus, FailReason, MissingPlugin, PluginError, PluginManager, PluginManagerConfig,
    PluginManagerEvent, ProcessorSetup,
};

// The shared-memory audio buffers must hold the engine's largest block.
const _: () = assert!(komponist_ipc::shm::MAX_BLOCK == komponist_engine::MAX_BLOCK);
