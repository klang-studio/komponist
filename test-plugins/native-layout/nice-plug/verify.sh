#!/usr/bin/env bash
# Build independent plugin variants, probe them through CLAP and compare inventories/layouts.
set -euo pipefail

revision=39e9ad41cf84f98762d86b83abdb9c422fcb58a5
source_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
if [[ "$(uname -s)" != Linux ]]; then
    echo "The spike's bundle staging script targets Linux." >&2
    exit 1
fi
if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "usage: verify.sh <nice-plug-git-repository> [scratch-output-directory]" >&2
    exit 2
fi

fork_repo="$(realpath -- "$1")"
fork_worktree="$(git -C "$fork_repo" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$fork_repo")"
worktree="$(git -C "$source_dir" rev-parse --show-toplevel 2>/dev/null || printf '%s' "$source_dir")"
if [[ $# -eq 2 ]]; then
    scratch="$(realpath -m -- "$2")"
else
    scratch="$(mktemp -d "${TMPDIR:-/tmp}/komponist-180-nice-plug.XXXXXXXX")"
fi
if [[ "$scratch/" == "$worktree/"* || "$scratch/" == "$fork_worktree/"* ]]; then
    echo "Choose a scratch output directory outside both source trees." >&2
    exit 2
fi
mkdir -p "$scratch"
if [[ -n "$(find "$scratch" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
    echo "The scratch output directory must be empty: $scratch" >&2
    exit 2
fi

cp -R -- "$source_dir/prototype" "$scratch/prototype"
mkdir -- "$scratch/patched-nice-plug"
git -C "$fork_repo" archive --format=tar "$revision" | tar -x -C "$scratch/patched-nice-plug"
git -C "$scratch/patched-nice-plug" apply --check "$source_dir/nice-plug-custom-extension.patch"
git -C "$scratch/patched-nice-plug" apply "$source_dir/nice-plug-custom-extension.patch"

cd "$scratch/prototype"
export CARGO_TARGET_DIR="$scratch/target"
export CARGO_NET_OFFLINE="${CARGO_NET_OFFLINE:-true}"

build() {
    local variant="$1"
    shift
    nice -n 10 cargo build --locked -j 3 --lib "$@"
    local output="$scratch/artifacts/$variant"
    mkdir -p "$output"
    cp -- "$CARGO_TARGET_DIR/debug/libnice_layout_prototype.so" "$output/nice-layout-prototype.clap.tmp"
    mv -- "$output/nice-layout-prototype.clap.tmp" "$output/nice-layout-prototype.clap"
}

build baseline
# Remove the prototype's compiled files. The toolchain/dependency cache stays intact.
nice -n 10 cargo clean --locked -p nice-layout-prototype
build rebuild
build reordered --features reordered
build collision --features collision
nice -n 10 cargo build --locked -j 3 --bin probe

for variant in baseline rebuild reordered collision; do
    output="$scratch/artifacts/$variant"
    options=()
    if [[ "$variant" == collision ]]; then
        options+=(--collision)
    fi
    "$CARGO_TARGET_DIR/debug/probe" "$output/nice-layout-prototype.clap" "${options[@]}" > "$output/ids.json"
done

python3 - "$scratch/artifacts" <<'PY'
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
reports = {variant: json.loads((root / variant / "ids.json").read_text())
           for variant in ("baseline", "rebuild", "reordered", "collision")}
assert reports["baseline"] == reports["rebuild"], "a clean rebuild changed the CLAP contract"
baseline = reports["baseline"]
reordered = reports["reordered"]
assert baseline["params"] == reordered["params"][::-1], "CLAP enumeration did not reverse"
assert baseline["layout"] == reordered["layout"], "reordering parameters changed the layout"
assert reports["collision"]["layout"] is None
for case, report in reports.items():
    print(f"{case}: {[(p['name'], p['id']) for p in report['params']]}")
PY

printf 'Artifacts: %s\n' "$scratch/artifacts"
