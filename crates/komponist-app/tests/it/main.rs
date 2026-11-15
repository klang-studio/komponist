//! One integration-test binary per crate (WP-66): every former `tests/<name>.rs` is a module here,
//! so egui/wgpu link once. Binaries that need their own process stay next to this folder.

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[path = "../acceptance_support/mod.rs"]
mod acceptance_support;
#[path = "../common/mod.rs"]
mod common;
#[path = "../fake_host/mod.rs"]
mod fake_host;

mod acceptance_flow1;
mod acceptance_flow2;
mod acceptance_flow2_realtime;
mod acceptance_flow3;
mod acceptance_flow4;
mod acceptance_flow5;
mod arrangement;
mod bridge_mode;
mod browser;
mod device_events;
mod device_frame;
mod device_integration;
mod devices;
mod dialogs;
mod dispatch;
mod export;
mod export_bridges;
mod inspector;
mod internal_devices;
mod knurr;
mod mixer;
mod params_project;
mod piano_roll;
mod pisynth_device;
mod plugin_frame;
mod plugins;
mod record;
mod sampler;
mod shell;
mod step_seq;
