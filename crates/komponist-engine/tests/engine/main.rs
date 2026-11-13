//! Engine tests through the public API (PLAN WP-23 "Done when"). Every `Engine::process` and
//! `Engine::render` call runs inside `assert_no_alloc`, which aborts on any allocation or free.

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[allow(dead_code)]
mod common;
mod live;
mod mixing;
mod random;
mod swap;
mod timing;
