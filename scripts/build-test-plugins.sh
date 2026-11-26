#!/usr/bin/env bash
# Builds the test plugins in test-plugins/ (a Cargo workspace of its own, PLAN WP-27) in release
# mode and copies their bundles to target/test-plugins/<os>/clap/ and target/test-plugins/<os>/vst3/,
# <os> being `linux`, `macos` or `windows` (what Rust's std::env::consts::OS says, which the test helpers use). A bundle
# is a file on Linux, a directory on macOS; on Windows CLAP is a file and VST3 a directory. One worktree may be built on
# several OSes (macOS or Windows natively, Linux in the Docker gate), so each OS gets its own build folder and its own
# output folder. On Windows run it in Git Bash.
# Prints the path of every copied bundle.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
plugins="$root/test-plugins"
case "$(uname -s)" in
Darwin) os=macos ;;
Linux) os=linux ;;
MINGW* | MSYS* | CYGWIN*) os=windows ;;
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

# Keep isolated external fixtures and the verified headless inventory in separate directories.
target="$(rustc -vV | sed -n 's/^host: //p')"
fixture_stage="$(mktemp -d "$out/.builtin-clap.XXXXXX")"
trap 'rm -rf "$fixture_stage"' EXIT
python=python3
if ! command -v "$python" >/dev/null; then
    python=python
fi
CARGO_TARGET_DIR="$root/target" "$python" "$root/scripts/bundled-clap.py" \
    --target "$target" --inventory native-fixtures stage "$fixture_stage/native-fixtures" >/dev/null
install_fixture() {
    local source="$1"
    local fixture="$2"
    mkdir -p "$(dirname "$fixture")"
    rm -rf "$fixture"
    mv "$fixture_stage/native-fixtures/$source.clap" "$fixture"
    echo "$fixture"
}
install_fixture komponist_sampler "$out/builtin-clap/klopfer.clap"
install_fixture komponist_general_sampler "$out/builtin-clap/general-sampler.clap"
# State sync uses Limiter without adding an entry to the sampler snapshots' browser.
rm -rf "$out/builtin-clap/limiter.clap"
install_fixture komponist_limiter "$out/state-sync-clap/limiter.clap"

# Headless tests run the shipped devices from the same verified inventory as a packaged app.
CARGO_TARGET_DIR="$root/target" "$python" "$root/scripts/bundled-clap.py" \
    --target "$target" --inventory all stage "$fixture_stage/payload" >/dev/null
headless="$out/headless-bundles"
mkdir -p "$headless"
"$python" -c 'import hashlib, pathlib, sys; print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' \
    "$fixture_stage/payload/manifest.json" > "$fixture_stage/manifest.sha256"
rm -rf "$headless/bundled-clap"
mv "$fixture_stage/payload" "$headless/bundled-clap"
mv "$fixture_stage/manifest.sha256" "$headless/manifest.sha256"
echo "$headless/bundled-clap"
