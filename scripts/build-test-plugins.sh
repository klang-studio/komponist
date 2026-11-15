#!/usr/bin/env bash
# Builds the test plugins in test-plugins/ (a Cargo workspace of its own, PLAN WP-27) in release
# mode and copies their bundles to target/test-plugins/clap/ and target/test-plugins/vst3/.
# Prints the path of every copied bundle.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugins="$root/test-plugins"
out="$root/target/test-plugins"

# Pinned, so the bundles land where this script picks them up whatever CARGO_TARGET_DIR says.
export CARGO_TARGET_DIR="$plugins/target"
(cd "$plugins" && cargo xtask bundle --release)

bundled="$CARGO_TARGET_DIR/bundled"
rm -rf "$out/clap" "$out/vst3"
mkdir -p "$out/clap" "$out/vst3"
for name in kp-test-synth kp-test-fx kp-test-crash kp-test-slow kp-test-sc; do
    cp -R "$bundled/$name.clap" "$out/clap/"
    cp -R "$bundled/$name.vst3" "$out/vst3/"
    echo "$out/clap/$name.clap"
    echo "$out/vst3/$name.vst3"
done
