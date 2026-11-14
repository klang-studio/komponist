//! One integration-test binary per crate (WP-66): every former `tests/<name>.rs` is a module here,
//! so egui/wgpu link once. Binaries that need their own process stay next to this folder.

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[path = "../common/mod.rs"]
mod common;

mod engine;
mod gui;
mod installed;
mod lifecycle;
mod scan;
