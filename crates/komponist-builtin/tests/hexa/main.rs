//! hexa processor tests (PLAN WP-25 "Done when"). Every `process` and `reset` call runs inside
//! `assert_no_alloc`, which aborts on any allocation or free.

// Debug builds only: in release builds `assert_no_alloc` is a no-op without its allocator.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

mod bench;
mod card;
#[allow(dead_code)]
mod common;
mod engine;
mod events;
mod state;
