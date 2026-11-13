//! The link between komponist and its plugin bridge processes.
//!
//! Shared memory (memfd), futex signalling with deadlines, SPSC event rings in shared memory and
//! the control protocol over a Unix socket. Both sides use this crate: the app's
//! `ExternalProcessor` and plugin manager (`komponist-plugins`) and `komponist-bridge`.
//!
//! Module map (ARCHITECTURE §8.1):
//! - [`shm`]: the `#[repr(C)]` shared layout, [`ShmRegion`], the RT roles [`BridgeClient`] (app)
//!   and [`BridgeServer`] (bridge), and [`BridgeMonitor`] (app UI thread).
//! - [`ring`]: [`ShmRing`], the SPSC ring inside the mapping.
//! - [`signal`]: futex wait with deadline and wake.
//! - [`control`]: [`ControlMsg`] / [`BridgeMsg`] over a seqpacket [`Channel`], and the shared
//!   plugin vocabulary ([`PluginDescriptor`], [`ParamInfo`], [`NoteName`], …).
//!
//! This crate links no plugin format and no audio backend.

// Contract stage (WP-01): bodies are `todo!()` until WP-16 implements them, so parameters are
// unused for now. Remove this allow when the implementation lands.
#![allow(unused_variables)]

pub mod control;
pub mod ring;
pub mod shm;
pub mod signal;

pub use control::{
    AppChannel, BridgeChannel, BridgeMsg, Channel, ChannelError, ControlMsg, ErrorContext, GuiSize,
    LoadedInfo, LogLevel, NoteName, ParamInfo, ParamValue, PluginDescriptor, PluginFormat,
    PluginKind, RenderMode,
};
pub use ring::ShmRing;
pub use shm::{
    BlockError, BlockRequest, BlockResult, BlockSpec, BlockStatus, BridgeClient, BridgeMonitor,
    BridgeServer, BridgeState, ShmError, ShmLayout, ShmRegion, WireEvent, WireEventKind,
    WireTransport,
};
pub use signal::WaitOutcome;
