//! Allocation-checked integration tests with the engine.

#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

#[allow(dead_code)]
#[path = "../../../../komponist-engine/tests/engine/common.rs"]
mod common;
mod engine;
#[path = "../../../../komponist-engine/tests/engine/material.rs"]
mod material;
use common as engine_common;
