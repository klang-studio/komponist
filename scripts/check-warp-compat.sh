#!/usr/bin/env bash
# Build the genuine pre-Warp model separately; keep ordinary Cargo package selection unambiguous.
# Requires the pinned Git object (CI checks out full history). No audio hardware or file decoding.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo="$PWD"
source scripts/build-lock.sh
revision=f704f69e83b9ec278fdf7319b6a7d0351d4ffb82
if ! git cat-file -e "$revision:crates/komponist-model/src/lib.rs"; then
    echo "Missing old-reader Git object; fetch $revision from origin before this check." >&2
    exit 1
fi
build_target="${CARGO_TARGET_DIR:-$repo/target}"
mkdir -p "$build_target"
build_target="$(cd "$build_target" && pwd)"
work="$(mktemp -d "$build_target/warp-compat.XXXXXX")"
# Leave the exchange files/source/logs for inspection. Never mutate a checkout of the old branch.
git archive "$revision" crates/komponist-model/src crates/komponist-tokens/src crates/komponist-tokens/build crates/komponist-tokens/build.rs design/tokens.json design/device-tokens.json assets/fonts assets/icons LICENSE Cargo.lock | tar -x -C "$work"
legacy="$work/crates/komponist-model"
cp "$work/Cargo.lock" "$legacy/Cargo.lock"
mkdir -p "$legacy/src/bin"
cp scripts/warp-compat/old-reader.rs "$legacy/src/bin/legacy-reader.rs"
cat > "$legacy/Cargo.toml" <<'EOF'
[package]
name = "legacy-model"
version = "0.0.0"
edition = "2024"
license = "GPL-3.0-or-later"
publish = false
[workspace]
[dependencies]
serde = { version = "=1.0.229", features = ["derive", "rc"] }
serde_json = "=1.0.154"
base64 = "=0.23.1"
komponist-tokens = { path = "../komponist-tokens" }
EOF
cat > "$work/crates/komponist-tokens/Cargo.toml" <<'EOF'
[package]
name = "komponist-tokens"
version = "0.0.0"
edition = "2024"
license = "GPL-3.0-or-later"
publish = false
[workspace]
[features]
egui = []
[build-dependencies]
serde_json = "=1.0.154"
EOF
# Only the test harness/package metadata differs; every historical library source stays exact.
lowprio cargo build -p komponist-model --example warp_compat
"$build_target/debug/examples/warp_compat" prepare "$work/exchange"
lowprio cargo build --manifest-path "$legacy/Cargo.toml" --target-dir "$build_target/legacy-reader-build" --bin legacy-reader
"$build_target/legacy-reader-build/debug/legacy-reader" "$work/exchange"
"$build_target/debug/examples/warp_compat" verify "$work/exchange"
echo "Historical reader: $revision; artifacts: $work"
