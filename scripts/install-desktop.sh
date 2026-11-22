#!/usr/bin/env bash
# Installs the icon theme files, desktop entry, mime type and AppStream metainfo of komponist.
# Install the executable and its bundled-clap directory separately. See docs/INSTALL.md.
#
#   PREFIX=/usr scripts/install-desktop.sh        # system-wide (packaging: add DESTDIR=...)
#   scripts/install-desktop.sh                    # PREFIX defaults to ~/.local
#
# Before copying, every hicolor PNG is checked against the size its folder names.
set -euo pipefail
cd "$(dirname "$0")/.."
PREFIX="${PREFIX:-$HOME/.local}"
DEST="${DESTDIR:-}${PREFIX}/share"
ICON=komponist
APP_ID=io.github.klang_studio.komponist

for n in 16 24 32 48 64 128 256 512; do
    f="assets/icons/hicolor/${n}x${n}/apps/$ICON.png"
    got=$(file -b "$f" | sed -n 's/.*PNG image data, \([0-9]*\) x \([0-9]*\),.*/\1x\2/p')
    [ "$got" = "${n}x${n}" ] || { echo "install-desktop: $f is $got, expected ${n}x${n}" >&2; exit 1; }
done

for n in 16 24 32 48 64 128 256 512; do
    install -Dm644 "assets/icons/hicolor/${n}x${n}/apps/$ICON.png" "$DEST/icons/hicolor/${n}x${n}/apps/$ICON.png"
done
install -Dm644 "assets/icons/hicolor/scalable/apps/$ICON.svg" "$DEST/icons/hicolor/scalable/apps/$ICON.svg"
install -Dm644 "assets/icons/hicolor/symbolic/apps/$ICON-symbolic.svg" "$DEST/icons/hicolor/symbolic/apps/$ICON-symbolic.svg"
install -Dm644 "assets/$APP_ID.desktop" "$DEST/applications/$APP_ID.desktop"
install -Dm644 assets/komponist-mime.xml "$DEST/mime/packages/komponist.xml"
install -Dm644 "assets/$APP_ID.metainfo.xml" "$DEST/metainfo/$APP_ID.metainfo.xml"

echo "Installed into $DEST."
echo "Refresh the caches if your desktop does not pick the files up:"
echo "  gtk-update-icon-cache -f -t '$DEST/icons/hicolor'; update-desktop-database '$DEST/applications'; update-mime-database '$DEST/mime'"
