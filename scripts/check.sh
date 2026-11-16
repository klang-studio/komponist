#!/usr/bin/env bash
# The gate every work package must pass before it reports: formatting, clippy without warnings,
# all tests of the workspace. Runs under the shared build lock (scripts/build-lock.sh), so it may
# wait for another worktree's gate: start it in the background or with a long timeout. Compiling
# runs at low priority; the tests run at normal priority so timing-sensitive tests don't flake.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/build-lock.sh

# Windows (Git Bash): wgpu's DX12 backend crashed parallel kittest tests with STATUS_ACCESS_VIOLATION on
# the first Windows gate (2026-11-16); Vulkan and GL pass. A WGPU_BACKEND set by the caller wins.
case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN*) export WGPU_BACKEND="${WGPU_BACKEND:-vulkan}" ;;
esac

lowprio cargo fmt --all --check
lowprio cargo clippy --workspace --all-targets -- -D warnings
lowprio cargo test --workspace --no-run
cargo test --workspace
