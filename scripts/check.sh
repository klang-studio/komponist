#!/usr/bin/env bash
# The gate every work package must pass before it reports: formatting, clippy without warnings,
# all tests of the workspace.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
