#!/usr/bin/env bash
# Builds the klopfer plugin bundles (CLAP + VST3) in the sibling repository ../klopfer.
#
# Only writes ../klopfer/target. It deliberately does not run `git submodule update`:
# that could move the submodule commits the owner has checked out in ../klopfer/vendor.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
klopfer="$(cd "$root/.." && pwd)/klopfer"

if [[ ! -f "$klopfer/vendor/nice-plug/Cargo.toml" ]]; then
    echo "error: $klopfer/vendor/nice-plug is not checked out." >&2
    echo "Initialise the submodules yourself, then run this script again:" >&2
    echo "    cd $klopfer && git submodule update --init" >&2
    exit 1
fi

(cd "$klopfer" && cargo xtask bundle klopfer --release)

bundled="$klopfer/target/bundled"
status=0
for bundle in Klopfer.clap Klopfer.vst3; do
    if [[ -e "$bundled/$bundle" ]]; then
        echo "$bundled/$bundle"
    else
        echo "error: $bundled/$bundle was not produced" >&2
        status=1
    fi
done
exit "$status"
