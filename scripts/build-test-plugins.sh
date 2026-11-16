#!/usr/bin/env bash
# Builds the test plugins in test-plugins/ (a Cargo workspace of its own, PLAN WP-27) in release
# mode and copies their bundles to target/test-plugins/<os>/clap/ and target/test-plugins/<os>/vst3/,
# <os> being `linux` or `macos` (what Rust's std::env::consts::OS says, which the test helpers use). A bundle
# is a file on Linux and a directory on macOS, and one worktree may be built on both (macOS natively, Linux
# in the Docker gate), so each OS gets its own build folder and its own output folder.
# Prints the path of every copied bundle.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugins="$root/test-plugins"
case "$(uname -s)" in
Darwin) os=macos ;;
Linux) os=linux ;;
*)
    echo "error: unsupported OS $(uname -s)" >&2
    exit 1
    ;;
esac
out="$root/target/test-plugins/$os"

# Pinned, so the bundles land where this script picks them up whatever CARGO_TARGET_DIR says.
export CARGO_TARGET_DIR="$plugins/target/$os"
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
