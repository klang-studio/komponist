#!/usr/bin/env bash
# The workflow runs this from crates/plugin-sdk on each native target.
set -euo pipefail

if [[ $# != 2 ]]; then
    echo "usage: $0 TARGET SOURCE_REVISION" >&2
    exit 2
fi
export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}"
target="$1"
revision="$2"

verify_source() {
    local prefix head status
    if ! prefix="$(git -C ../.. rev-parse --show-prefix 2>/dev/null)" || [[ -n "$prefix" ]]; then
        echo "Build release packages from a Git checkout of the reviewed source." >&2
        return 1
    fi
    head="$(git -C ../.. rev-parse --verify HEAD)"
    if [[ "$head" != "$revision" ]]; then
        printf 'Requested source revision %s differs from checkout HEAD %s.\n' "$revision" "$head" >&2
        return 1
    fi
    status="$(git --no-optional-locks -C ../.. status --porcelain=v1 --untracked-files=normal)"
    if [[ -n "$status" ]]; then
        echo "Release packaging requires a clean checkout." >&2
        return 1
    fi
}

verify_source
bundle=native_clap_example.clap
dist="${CARGO_TARGET_DIR:-target}/native-clap-dist"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
mkdir -p "$scratch/native" "$scratch/plain" "$scratch/extracted" "$dist"

xtask() {
    cargo run --locked -p native-clap-xtask -- "$@"
}

xtask bundle --release --target "$target" --out-dir "$scratch/native"
xtask validate "$scratch/native/$bundle" > "$scratch/native.json"
xtask bundle --release --target "$target" --no-default-features --out-dir "$scratch/plain"
xtask validate "$scratch/plain/$bundle" --plain > "$scratch/plain.json"
# Git Bash may expose the Windows runner's Python as "python" only.
python=python3
if ! "$python" -c 'import sys' >/dev/null 2>&1; then
    python=python
fi
"$python" - "$scratch/native.json" "$scratch/plain.json" <<'PY'
import json
import sys
from pathlib import Path

native, plain = (json.loads(Path(path).read_bytes()) for path in sys.argv[1:])
if native["plugin"] != plain["plugin"] or native["params"] != plain["params"]:
    sys.exit("CLAP identity or parameter inventory changed in the standard-only build")
PY

cp ../../LICENSE "$scratch/native/LICENSE"
cp licenses/nice-plug.txt "$scratch/native/LICENSE-nice-plug"
stem="native-clap-example-$target"
archive="$(cd "$dist" && pwd)/$stem.tar.gz"
COPYFILE_DISABLE=1 tar -czf "$archive" -C "$scratch/native" "$bundle" LICENSE LICENSE-nice-plug
tar -xzf "$archive" -C "$scratch/extracted"
entries="$(cd "$scratch/extracted" && LC_ALL=C find . -mindepth 1 -print | LC_ALL=C sort)"
case "$target" in
    *-apple-darwin)
        expected="$(printf '%s\n' ./LICENSE ./LICENSE-nice-plug "./$bundle" "./$bundle/Contents" \
            "./$bundle/Contents/Info.plist" "./$bundle/Contents/MacOS" \
            "./$bundle/Contents/MacOS/native_clap_example")"
        test -x "$scratch/extracted/$bundle/Contents/MacOS/native_clap_example"
        ;;
    *)
        expected="$(printf '%s\n' ./LICENSE ./LICENSE-nice-plug "./$bundle")"
        test -f "$scratch/extracted/$bundle"
        ;;
esac
if [[ "$entries" != "$expected" ]]; then
    printf 'Unexpected archive contents:\n%s\n' "$entries" >&2
    exit 1
fi
diff -r "$scratch/native" "$scratch/extracted"
xtask validate "$scratch/extracted/$bundle" > "$scratch/extracted.json"
cmp "$scratch/native.json" "$scratch/extracted.json"
verify_source
xtask metadata "$scratch/extracted/$bundle" "$archive" "$target" "$revision" \
    "$dist/$stem.json"
cat "$dist/$stem.json"
