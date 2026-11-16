#!/usr/bin/env bash
# Regenerates assets/windows/komponist.ico from assets/icons/hicolor/<size>x<size>/apps/komponist.png.
#
#   assets/windows/make-ico.sh
#
# An ICO file is a 6-byte header, one 16-byte directory entry per image and the image data. The images are the
# PNG files verbatim (PNG-in-ICO, valid for every size on Windows Vista and newer). No tools beyond bash,
# printf and stat/wc are needed. Sizes: 16 24 32 48 64 128 256 (512 does not fit an ICO directory entry).
set -euo pipefail
cd "$(dirname "$0")/../.."

sizes="16 24 32 48 64 128 256"
out=assets/windows/komponist.ico
src=assets/icons/hicolor

le16() { printf "\x$(printf %02x $(($1 & 255)))\x$(printf %02x $((($1 >> 8) & 255)))"; }
le32() { le16 $(($1 & 65535)); le16 $((($1 >> 16) & 65535)); }

count=0
for s in $sizes; do count=$((count + 1)); done
offset=$((6 + 16 * count))
{
    le16 0; le16 1; le16 "$count" # reserved, type 1 = icon, image count
    for s in $sizes; do
        png="$src/${s}x${s}/apps/komponist.png"
        len=$(wc -c <"$png")
        dim=$s; [ "$s" = 256 ] && dim=0 # 0 stands for 256
        le16 $((dim | (dim << 8))) # width, height
        le16 0                     # colour count, reserved
        le16 1; le16 32            # planes, bits per pixel
        le32 "$len"; le32 "$offset"
        offset=$((offset + len))
    done
    for s in $sizes; do cat "$src/${s}x${s}/apps/komponist.png"; done
} >"$out"
echo "wrote $out ($(wc -c <"$out") bytes, $count images)"
