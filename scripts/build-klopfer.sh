#!/usr/bin/env bash
# Optional dev script, not needed to build komponist: builds the klopfer plugin bundles (CLAP + VST3)
# from a klopfer checkout for the plugin tests that use an installed Klopfer (those skip without it).
# The checkout is $KLOPFER_DIR, default: a `klopfer` folder next to this repository.
#
# Only writes <klopfer>/target. It deliberately does not run `git submodule update`:
# that could move the submodule commits checked out in <klopfer>/vendor.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
klopfer="${KLOPFER_DIR:-$(cd "$root/.." && pwd)/klopfer}"

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
