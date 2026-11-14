#!/usr/bin/env bash
# The gate every work package must pass before it reports: formatting, clippy without warnings,
# all tests of the workspace. Runs under the shared build lock (scripts/build-lock.sh), so it may
# wait for another worktree's gate: start it in the background or with a long timeout.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
source scripts/build-lock.sh

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
