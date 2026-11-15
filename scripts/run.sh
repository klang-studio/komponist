#!/usr/bin/env bash
# Builds komponist (release) and starts it with the given arguments. The plugin bridge is part of the
# executable (`komponist --bridge`, WP-94), so one build is enough. The build runs under the shared
# build lock at low priority; the app itself starts at normal priority without the lock.
#
#   scripts/run.sh --demo --open-plugin klopfer
set -euo pipefail
cd "$(dirname "$0")/.."
(
    source scripts/build-lock.sh
    cargo build --release -p komponist-app --bin komponist
)
exec target/release/komponist "$@"
