#!/usr/bin/env bash
# Builds a local release app with its authenticated CLAP inventory, then starts it.
# The build lock is released before the app starts.
#
#   scripts/run.sh --demo --open-plugin klopfer
set -euo pipefail
cd "$(dirname "$0")/.."
build_only=false
if [[ "${1:-}" == --build-only ]]; then
    build_only=true
    shift
fi
source scripts/bundled-clap-build.sh
target="$(native_clap_target)"
target_dir="$(cargo metadata --locked --no-deps --format-version 1 | "$clap_python" -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
mkdir -p "$target_dir"
run_dir="$(mktemp -d "$target_dir/komponist-run.XXXXXX")"
trap 'rm -rf -- "$run_dir"' EXIT
mkdir -p "$run_dir/bin"
exe=komponist
resources="$run_dir/bin"
case "$target" in
    *-apple-darwin) resources="$run_dir/Resources" ;;
    *-windows-msvc) exe=komponist.exe ;;
esac
(
    source scripts/build-lock.sh
    digest="$(stage_native_bundled_clap "$resources/bundled-clap")"
    export KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256="$digest"
    lowprio cargo build --locked --release -p komponist-app --bin komponist
    cp "$target_dir/release/$exe" "$run_dir/bin/$exe"
)
if "$build_only"; then
    trap - EXIT
    printf '%s\n' "$run_dir/bin/$exe"
else
    "$run_dir/bin/$exe" "$@"
fi
