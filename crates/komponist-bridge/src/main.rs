//! `komponist-bridge`: the bridge as a binary of its own (tests, tools, the standalone setup). The
//! app does not need it: `komponist --bridge …` runs the same code (see the library).

// Debug builds: an allocation inside the RT loop (`assert_no_alloc`) aborts the bridge.
#[cfg(debug_assertions)]
#[global_allocator]
static ALLOC: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn main() -> std::process::ExitCode {
    komponist_bridge::run(std::env::args_os().skip(1))
}
