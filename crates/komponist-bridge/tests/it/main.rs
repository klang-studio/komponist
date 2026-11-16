//! One integration-test binary per crate (WP-66): every former `tests/<name>.rs` is a module here,
//! so egui/wgpu link once. Binaries that need their own process stay next to this folder.

#[path = "../common/mod.rs"]
mod common;

#[cfg(target_os = "macos")]
mod frame_mac;
#[cfg(target_os = "linux")]
mod gui;
mod installed;
mod instance;
mod scan;
mod vst3;
