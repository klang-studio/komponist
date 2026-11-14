#!/usr/bin/env bash
# Builds komponist and its plugin bridge (release) and starts komponist with the given arguments.
# `cargo run -p komponist-app` alone does not build `komponist-bridge`, and without it every plugin
# except pisynth fails to load.
#
#   scripts/run.sh --demo --open-plugin klopfer
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p komponist-app --bin komponist
cargo build --release -p komponist-bridge
exec target/release/komponist "$@"
