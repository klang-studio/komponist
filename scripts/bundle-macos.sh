#!/usr/bin/env bash
# Builds komponist (release) and assembles the macOS app bundle dist/komponist.app (#53).
# macOS only (iconutil, codesign, plutil). Runs with the stock bash 3.2. Used by the release job and by hand.
#
#   scripts/bundle-macos.sh [--skip-build] [--out DIR] [--bundled-clap DIR]
#
# The bundle is signed with the hardened runtime and the entitlements in assets/macos/komponist.entitlements.
# Default: ad-hoc (`codesign -s -`), which is what v0.3 ships; macOS then asks for the Gatekeeper override
# (docs/INSTALL.md). With a Developer ID the same script signs for distribution and can notarize:
#
#   KOMPONIST_SIGN_IDENTITY   "Developer ID Application: Name (TEAMID)", a certificate in the keychain
#                             (in CI: import it into a temporary keychain first). Unset = ad-hoc.
#   Notarization (only with an identity; skipped when neither is set):
#     KOMPONIST_NOTARY_PROFILE  a notarytool keychain profile (`xcrun notarytool store-credentials`), or
#     APPLE_ID + APPLE_TEAM_ID + APPLE_APP_PASSWORD   an app-specific password
#   Notarization uploads a zip of the app, waits for the verdict and staples the ticket to the bundle.
#
# The Developer ID path has not been run yet (no Apple Developer account at the time of writing).
set -euo pipefail
cd "$(dirname "$0")/.."

skip_build=0
out=dist
bundled_clap=
while [ $# -gt 0 ]; do
    case "$1" in
        --skip-build) skip_build=1 ;;
        --out) shift; out="${1:?--out needs a folder}" ;;
        --bundled-clap) shift; bundled_clap="${1:?--bundled-clap needs a folder}" ;;
        -h | --help) sed -n '2,19p' "$0"; exit 0 ;;
        *) echo "bundle-macos.sh: unknown argument $1" >&2; exit 2 ;;
    esac
    shift
done

if [ "$(uname -s)" != Darwin ]; then
    echo "bundle-macos.sh: macOS only" >&2
    exit 1
fi

# The workspace version (`[workspace.package]`), the one `komponist` reports.
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' Cargo.toml | head -n 1)"
if [ -z "$version" ]; then
    echo "bundle-macos.sh: no version in Cargo.toml" >&2
    exit 1
fi

source scripts/bundled-clap-build.sh
stage_bundled_clap=0
if [ -z "$bundled_clap" ]; then
    if [ "$skip_build" = 1 ]; then
        echo "bundle-macos.sh: --skip-build requires the matching --bundled-clap directory" >&2
        exit 2
    fi
    staging="$(mktemp -d)"
    trap 'rm -rf -- "$staging"' EXIT
    bundled_clap="$staging/bundled-clap"
    stage_bundled_clap=1
fi

if [ "$skip_build" = 0 ]; then
    # One heavy build at a time across worktrees, at low priority (AGENTS.md, "Build load").
    (
        source scripts/build-lock.sh
        if [ "$stage_bundled_clap" = 1 ]; then
            digest="$(stage_native_bundled_clap "$bundled_clap")"
        else
            digest="$(bundled_clap_manifest_digest "$bundled_clap")"
        fi
        export KOMPONIST_BUNDLED_CLAP_MANIFEST_SHA256="$digest"
        lowprio cargo build --locked --release -p komponist-app --bin komponist
    )
fi
exe=target/release/komponist
[ -x "$exe" ] || { echo "bundle-macos.sh: $exe is missing (build it first)" >&2; exit 1; }

app="$out/komponist.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
# The plugin bridge is this executable (`komponist --bridge`): nothing else to bundle. Fonts and the window
# icon are compiled in.
install -m755 "$exe" "$app/Contents/MacOS/komponist"
# Nested signatures already exist and are part of the manifest's file hashes.
ditto "$bundled_clap" "$app/Contents/Resources/bundled-clap"
sed "s/@VERSION@/$version/g" assets/macos/Info.plist.in >"$app/Contents/Info.plist"
for lang in de en; do
    mkdir -p "$app/Contents/Resources/$lang.lproj"
    cp "assets/macos/$lang.lproj/InfoPlist.strings" "$app/Contents/Resources/$lang.lproj/"
done
plutil -lint "$app/Contents/Info.plist" >/dev/null
printf 'APPL????' >"$app/Contents/PkgInfo"

# Icon: the iconset wants 16 32 128 256 512 and their @2x. The hicolor PNGs have 16 24 32 48 64 128 256 512;
# 512@2x (1024 px, #96) is assets/macos/icon_512x512@2x.png, rendered from the hicolor SVG with headless Chrome
# (an <img> of 1024 × 1024 px, transparent background).
iconset="$(mktemp -d)/komponist.iconset"
mkdir -p "$iconset"
src=assets/icons/hicolor
for pair in 16:icon_16x16 32:icon_16x16@2x 32:icon_32x32 64:icon_32x32@2x 128:icon_128x128 \
    256:icon_128x128@2x 256:icon_256x256 512:icon_256x256@2x 512:icon_512x512; do
    cp "$src/${pair%%:*}x${pair%%:*}/apps/komponist.png" "$iconset/${pair#*:}.png"
done
cp "assets/macos/icon_512x512@2x.png" "$iconset/icon_512x512@2x.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/komponist.icns"
rm -rf "$(dirname "$iconset")"

identity="${KOMPONIST_SIGN_IDENTITY:-}"
if [ -n "$identity" ]; then
    codesign --force --options runtime --timestamp \
        --entitlements assets/macos/komponist.entitlements --sign "$identity" "$app"
else
    codesign --force --options runtime --timestamp=none \
        --entitlements assets/macos/komponist.entitlements --sign - "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"
"$app/Contents/MacOS/komponist" --verify-bundled-clap

if [ -n "$identity" ]; then
    notary=()
    if [ -n "${KOMPONIST_NOTARY_PROFILE:-}" ]; then
        notary=(--keychain-profile "$KOMPONIST_NOTARY_PROFILE")
    elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
        notary=(--apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD")
    fi
    if [ ${#notary[@]} -gt 0 ]; then
        upload="$(mktemp -d)/komponist-notarize.zip"
        ditto -c -k --keepParent "$app" "$upload"
        xcrun notarytool submit "$upload" "${notary[@]}" --wait
        rm -rf "$(dirname "$upload")"
        xcrun stapler staple "$app"
        xcrun stapler validate "$app"
    else
        echo "bundle-macos.sh: signed with a Developer ID, but no notarytool credentials set: not notarized" >&2
    fi
fi

echo "bundle: $app (version $version, $([ -n "$identity" ] && echo "signed: $identity" || echo "ad-hoc signed"))"
