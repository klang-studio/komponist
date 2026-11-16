#!/usr/bin/env bash
# Builds komponist (release) and packs the Windows zip: dist/komponist-<version>-windows-x86_64.zip with its .sha256.
# Windows only, run from Git Bash (MSVC toolchain, bsdtar from Windows 10+). Used by the release job and by hand.
#
#   scripts/package-windows.sh [--skip-build] [--no-asio] [--out DIR] [--name NAME]
#
# The build has ASIO (cargo feature `asio`, W8) unless --no-asio is given. ASIO needs LLVM's libclang for bindgen
# (LIBCLANG_PATH, or "C:\Program Files\LLVM\bin", where the LLVM installer and the windows-2025 runner put it) and
# downloads Steinberg's ASIO SDK at build time (https://www.steinberg.net/asiosdk; docs/ARCHITECTURE.md §7). Without
# libclang the script stops with an error instead of shipping a build without ASIO silently. The ASIO SDK is
#
# not part of the zip. With --skip-build nothing is built or checked here.
# NAME is the base name of the folder inside the zip and of the zip itself; default komponist-<version>-windows-x86_64
# (the release job passes komponist-<tag>-windows-x86_64). The zip holds komponist.exe (the plugin bridge is
# `komponist.exe --bridge`, nothing else to ship; fonts, texts and the window icon are compiled in, the file icon
# and the manifest are linked in by crates/komponist-app/build.rs), LICENSE and a short README.txt.
#
# The executable is unsigned by default; Windows SmartScreen then warns on the first start (docs/INSTALL.md).
# Authenticode signing is optional and off unless one of these is set (signtool from the Windows SDK, on PATH or
# in "C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64"):
#
#   KOMPONIST_SIGN_CERT        path of a .pfx file, with KOMPONIST_SIGN_PASSWORD (its password), or
#   KOMPONIST_SIGN_THUMBPRINT  SHA-1 thumbprint of a certificate in the user or machine store (signtool /sha1)
#   KOMPONIST_SIGN_TIMESTAMP_URL  RFC 3161 timestamp server (default http://timestamp.digicert.com)
#
# The signing path has not been run yet (no code signing certificate at the time of writing).
set -euo pipefail
cd "$(dirname "$0")/.."

skip_build=0
asio=1
out=dist
name=
while [ $# -gt 0 ]; do
    case "$1" in
        --skip-build) skip_build=1 ;;
        --no-asio) asio=0 ;;
        --out) shift; out="${1:?--out needs a folder}" ;;
        --name) shift; name="${1:?--name needs a name}" ;;
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

if [ "$skip_build" = 0 ]; then
    # One heavy build at a time across worktrees, at low priority (CLAUDE.md, "Build load").
    (
        source scripts/build-lock.sh
        lowprio cargo build --release -p komponist-app --bin komponist ${features[@]+"${features[@]}"}
    )
fi
exe=target/release/komponist.exe
[ -f "$exe" ] || { echo "package-windows.sh: $exe is missing (build it first)" >&2; exit 1; }

audio="Audio goes through WASAPI or an ASIO driver (Settings, Treiber). ASIO is a registered trademark of
Steinberg Media Technologies GmbH."
[ "$asio" = 1 ] || audio="This build has no ASIO; audio goes through WASAPI."
signed="unsigned"
if [ -n "${KOMPONIST_SIGN_CERT:-}" ] || [ -n "${KOMPONIST_SIGN_THUMBPRINT:-}" ]; then
    signtool="$(command -v signtool.exe || true)"
    if [ -z "$signtool" ]; then
        # Newest SDK version first; `sort -V` is in Git Bash's coreutils.
        signtool="$(ls -d "/c/Program Files (x86)/Windows Kits/10/bin/"*/x64/signtool.exe 2>/dev/null | sort -V | tail -n 1 || true)"
    fi
    [ -n "$signtool" ] || { echo "package-windows.sh: signtool.exe not found (Windows SDK)" >&2; exit 1; }
    sign=(sign /fd SHA256 /tr "${KOMPONIST_SIGN_TIMESTAMP_URL:-http://timestamp.digicert.com}" /td SHA256)
    if [ -n "${KOMPONIST_SIGN_CERT:-}" ]; then
        sign+=(/f "$(cygpath -w "$KOMPONIST_SIGN_CERT")")
        [ -z "${KOMPONIST_SIGN_PASSWORD:-}" ] || sign+=(/p "$KOMPONIST_SIGN_PASSWORD")
    else
        sign+=(/sha1 "$KOMPONIST_SIGN_THUMBPRINT")
    fi
    # Sign a copy, so target/ keeps the build output as built; the copy is what goes into the zip.
    signed_exe="$out/.signing/komponist.exe"
    mkdir -p "$(dirname "$signed_exe")"
    cp "$exe" "$signed_exe"
    "$signtool" "${sign[@]}" "$(cygpath -w "$signed_exe")"
    "$signtool" verify /pa "$(cygpath -w "$signed_exe")"
    exe="$signed_exe"
    signed="Authenticode signed"
fi

dir="$out/$name"
rm -rf "$dir" "$out/$name.zip" "$out/$name.zip.sha256"
mkdir -p "$dir"
cp "$exe" "$dir/komponist.exe"
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
