#!/usr/bin/env bash
# Builds komponist (release) and packs the Windows zip: dist/komponist-<version>-windows-x86_64.zip with its .sha256.
# Windows only, run from Git Bash (MSVC toolchain, bsdtar from Windows 10+). Used by the release job and by hand.
#
#   scripts/package-windows.sh [--skip-build] [--no-asio] [--out DIR] [--name NAME] [--bundled-clap DIR]
#
# The build has ASIO (cargo feature `asio`, W8) unless --no-asio is given. ASIO needs LLVM's libclang for bindgen
# (LIBCLANG_PATH, or "C:\Program Files\LLVM\bin", where the LLVM installer and the windows-2025 runner put it). The
# ASIO SDK (host subset) is vendored in vendor/asio-sdk and found through CPAL_ASIO_DIR in .cargo/config.toml: nothing is
# downloaded (docs/ARCHITECTURE.md §7). Without libclang the script stops with an error instead of shipping a build
# without ASIO silently. The SDK sources are not part of the zip.
# With --skip-build nothing is built or checked here.
# NAME is the base name of the folder inside the zip and of the zip itself; default komponist-<version>-windows-x86_64
# (the release job passes komponist-<tag>-windows-x86_64). The zip holds komponist.exe (the plugin bridge is
# `komponist.exe --bridge`, nothing else to ship; fonts, texts and the window icon are compiled in, the file icon
# and the manifest are linked in by crates/komponist-app/build.rs), LICENSE and a short README.txt.
#
# The executable is unsigned by default; Windows SmartScreen then warns on the first start (docs/INSTALL.md).
# Authenticode signing is optional and off unless KOMPONIST_SIGN_CERT or KOMPONIST_SIGN_THUMBPRINT is set (see
# scripts/sign-windows.sh). It covers komponist.exe here and each bundled CLAP device, which scripts/bundled-clap.py
# signs while staging, before the manifest hashes the final files.
set -euo pipefail
cd "$(dirname "$0")/.."

skip_build=0
asio=1
out=dist
name=
bundled_clap=
while [ $# -gt 0 ]; do
    case "$1" in
        --skip-build) skip_build=1 ;;
        --no-asio) asio=0 ;;
        --out) shift; out="${1:?--out needs a folder}" ;;
        --name) shift; name="${1:?--name needs a name}" ;;
        --bundled-clap) shift; bundled_clap="${1:?--bundled-clap needs a folder}" ;;
        -h | --help) sed -n '2,26p' "$0"; exit 0 ;;
        *) echo "package-windows.sh: unknown argument $1" >&2; exit 2 ;;
    esac
    shift
done

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) ;;
    *) echo "package-windows.sh: Windows only (Git Bash)" >&2; exit 1 ;;
esac

# The workspace version (`[workspace.package]`), the one `komponist` reports.
version="$(sed -n '/^\[workspace.package\]/,/^\[/{s/^version *= *"\(.*\)"/\1/p;}' Cargo.toml | head -n 1)"
if [ -z "$version" ]; then
    echo "package-windows.sh: no version in Cargo.toml" >&2
    exit 1
fi
[ -n "$name" ] || name="komponist-$version-windows-x86_64"

features=()
if [ "$skip_build" = 0 ] && [ "$asio" = 1 ]; then
    if [ -z "${LIBCLANG_PATH:-}" ] && [ -f "/c/Program Files/LLVM/bin/libclang.dll" ]; then
        export LIBCLANG_PATH="C:\Program Files\LLVM\bin"
    fi
    if [ -z "${LIBCLANG_PATH:-}" ] || [ ! -f "$(cygpath -u "$LIBCLANG_PATH")/libclang.dll" ]; then
        echo "package-windows.sh: ASIO needs LLVM's libclang.dll: install LLVM (https://releases.llvm.org) or set LIBCLANG_PATH to its bin folder;" >&2
        echo "  or build without ASIO with --no-asio" >&2
        exit 1
    fi
    features=(--features asio)
fi

source scripts/bundled-clap-build.sh
stage_bundled_clap=0
if [ -z "$bundled_clap" ]; then
    if [ "$skip_build" = 1 ]; then
        echo "package-windows.sh: --skip-build requires the matching --bundled-clap directory" >&2
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
        lowprio cargo build --locked --release -p komponist-app --bin komponist ${features[@]+"${features[@]}"}
    )
fi
exe=target/release/komponist.exe
[ -f "$exe" ] || { echo "package-windows.sh: $exe is missing (build it first)" >&2; exit 1; }

audio="Audio goes through WASAPI or an ASIO driver (Settings, Treiber). ASIO is a registered trademark of
Steinberg Media Technologies GmbH."
[ "$asio" = 1 ] || audio="This build has no ASIO; audio goes through WASAPI."
signed="unsigned"
if [ -n "${KOMPONIST_SIGN_CERT:-}" ] || [ -n "${KOMPONIST_SIGN_THUMBPRINT:-}" ]; then
    # Sign a copy, so target/ keeps the build output as built; the copy is what goes into the zip.
    signed_exe="$out/.signing/komponist.exe"
    mkdir -p "$(dirname "$signed_exe")"
    cp "$exe" "$signed_exe"
    scripts/sign-windows.sh "$signed_exe"
    exe="$signed_exe"
    signed="Authenticode signed"
fi

dir="$out/$name"
rm -rf "$dir" "$out/$name.zip" "$out/$name.zip.sha256"
mkdir -p "$dir"
cp "$exe" "$dir/komponist.exe"
cp -R "$bundled_clap" "$dir/bundled-clap"
"$dir/komponist.exe" --verify-bundled-clap
rm -rf "$out/.signing"
cp LICENSE "$dir/LICENSE"
cat >"$dir/README.txt" <<README
komponist $version for Windows 11 (x86_64)

Start: double-click komponist.exe (any folder works, nothing to install). From a terminal,
"komponist.exe --help" lists the options.

This build is $signed. Windows SmartScreen may show "Windows protected your PC" on the first start: click
"More info", then "Run anyway". Check the download against the .sha256 file next to the zip:
  Get-FileHash komponist-*-windows-x86_64.zip -Algorithm SHA256

Settings and the recent projects live in %APPDATA%\komponist.
Plugins (CLAP, VST3) are found in %COMMONPROGRAMFILES%\CLAP and \VST3, %LOCALAPPDATA%\Programs\Common\CLAP
and \VST3, and in %CLAP_PATH%. $audio

Double-clicking a .komponist file does not open komponist yet (the file association comes with a later installer);
open it from inside komponist or drop it on komponist.exe.

Licence: GPL-3.0-or-later (LICENSE). Source and documentation: https://github.com/klang-studio/komponist
README

# bsdtar (Windows 10+, System32) writes a real zip with -a; Git Bash's own GNU tar cannot.
bsdtar="$(cygpath -u "${SYSTEMROOT:-C:\Windows}")/System32/tar.exe"
[ -x "$bsdtar" ] || { echo "package-windows.sh: $bsdtar not found (needs Windows 10 or newer)" >&2; exit 1; }
"$bsdtar" -a -c -f "$out/$name.zip" -C "$out" "$name"
(cd "$out" && sha256sum "$name.zip" >"$name.zip.sha256" && cat "$name.zip.sha256")

echo "package: $out/$name.zip (version $version, $signed)"
