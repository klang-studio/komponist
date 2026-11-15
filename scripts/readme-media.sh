#!/usr/bin/env bash
# Regenerates the README screenshots and GIFs in docs/media/ from the demo project.
#
#   scripts/readme-media.sh               everything
#   scripts/readme-media.sh hero mixer    only these scenarios (names: scripts/readme-media/capture.py)
#   SKIP_BUILD=1 scripts/readme-media.sh  use the existing target/release/komponist
#
# Builds komponist (release, under the shared build lock like run.sh), starts it with
# `--demo --null-audio --no-plugin-scan --lang en` (the media are English) under a private Xvfb (1920x1080, no window
# manager, so no decorations or borders), with a scratch HOME and XDG folders, drives it through its AccessKit tree
# (`egui-mcp`, see .mcp.json) and records the GIFs with ffmpeg x11grab. It never touches real audio or MIDI hardware
# and plays no sound. Idempotent: files are overwritten. The scratch folder is made under $TMPDIR (default /tmp).
#
# Needs: xvfb-run, ffmpeg, ImageMagick (`magick` or `convert`), python3, `egui-mcp` (cargo install egui_mcp) on PATH.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v magick >/dev/null || command -v convert >/dev/null || { echo "readme-media: missing tool: ImageMagick" >&2; exit 1; }
for tool in xvfb-run ffmpeg python3 egui-mcp; do
    command -v "$tool" >/dev/null || { echo "readme-media: missing tool: $tool" >&2; exit 1; }
done

if [ -z "${SKIP_BUILD:-}" ]; then
    (
        source scripts/build-lock.sh
        low_priority
        cargo build --release -p komponist-app --bin komponist
    )
fi
bin="$PWD/target/release/komponist"
[ -x "$bin" ] || { echo "readme-media: $bin not found" >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/tmp}/komponist-readme-media.XXXXXX")"
trap 'rm -rf "$work"' EXIT
mkdir -p docs/media

echo "writing to docs/media/ (scratch: $work)"
xvfb-run -a -s "-screen 0 1920x1080x24" \
    python3 -I scripts/readme-media/capture.py --bin "$bin" --out docs/media --work "$work" ${1:+--only "$@"}
