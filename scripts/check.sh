#!/usr/bin/env bash
# The gate every work package must pass before it reports: the device boundary, formatting, clippy without warnings,
# all tests of the workspace. Runs under the shared build lock (scripts/build-lock.sh), so it may
# wait for another worktree's gate: start it in the background or with a long timeout. Compiling
# runs at low priority; the tests run at normal priority so timing-sensitive tests don't flake.
#
# Like the CI jobs (Linux, macOS), it builds the test plugins (incrementally, on every run) and runs the
# test-plugins workspace tests and the Warp compatibility check (needs the full Git history). Differences from
# CI: no `--locked`, no kittest tolerance file, no hang watchdog; the GUI tests are scripts/check-gui.sh.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
scripts/check-device-boundary.sh
source scripts/build-lock.sh

# A green gate must exercise the bundled plugin fixtures.
export KOMPONIST_REQUIRE_FIXTURES=1

# Windows (Git Bash): wgpu's DX12 backend crashed parallel kittest tests with STATUS_ACCESS_VIOLATION on
# the first Windows gate (2026-11-16); Vulkan and GL pass. A WGPU_BACKEND set by the caller wins.
case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN*) export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}" ;;
esac

# The test plugins' bundles for this OS; without them the plugin tests fail under REQUIRE_FIXTURES. Built on
# every run, so the tests always load bundles of the current test-plugins/ sources (cargo builds only what
# changed; a fresh worktree pays the full release build once).
lowprio scripts/build-test-plugins.sh >/dev/null

lowprio cargo fmt --all --check
lowprio cargo clippy --workspace --all-targets -- -D warnings
lowprio cargo test --workspace --no-run
cargo test --workspace

# The plugin SDK is a workspace of its own (excluded from the root workspace), but its adapter and author
# crates ship in every komponist-bundled-* crate (#234).
sdk=crates/plugin-sdk/Cargo.toml
lowprio cargo clippy --manifest-path "$sdk" --workspace --all-targets -- -D warnings
cargo test --manifest-path "$sdk" --workspace

# The test plugins are their own workspace; CI runs its tests too.
(cd test-plugins && cargo test --workspace --no-fail-fast)

scripts/check-warp-compat.sh
