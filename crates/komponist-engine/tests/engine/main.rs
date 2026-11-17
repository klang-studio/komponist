//! Engine tests through the public API (PLAN WP-23 "Done when"). Every `Engine::process` and
//! `Engine::render` call runs inside `assert_no_alloc`, which aborts on any allocation or free;
//! `export::export` wraps its own render calls (WP-61).

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod audio;
mod bench;
mod chase;
#[allow(dead_code)]
mod common;
mod export;
mod export_demo;
mod live;
mod midi_fx;
mod mixing;
mod parallel;
mod random;
mod recording;
mod routing;
mod swap;
mod tails;
mod timing;
mod volt;
