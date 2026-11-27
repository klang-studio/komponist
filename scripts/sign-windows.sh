#!/usr/bin/env bash
# Authenticode-signs the given files in place (signtool from the Windows SDK). Windows only, run from Git Bash.
# Used by scripts/bundled-clap.py for each bundled CLAP device and by scripts/package-windows.sh for komponist.exe.
#
#   scripts/sign-windows.sh FILE...
#
# Off unless one of these is set; without any of them the script does nothing and exits 0 (unsigned build):
#
#   KOMPONIST_SIGN_CERT        path of a .pfx file, with KOMPONIST_SIGN_PASSWORD (its password), or
#   KOMPONIST_SIGN_THUMBPRINT  SHA-1 thumbprint of a certificate in the user or machine store (signtool /sha1)
#   KOMPONIST_SIGN_TIMESTAMP_URL  RFC 3161 timestamp server (default http://timestamp.digicert.com)
#
# signtool is looked up on PATH and in "C:\Program Files (x86)\Windows Kits\10\bin\<version>\x64".
# The signing path has not been run yet (no code signing certificate at the time of writing).
set -euo pipefail
# Git Bash (MSYS2) rewrites arguments that look like POSIX paths, which mangles SignTool's /fd, /pa ... options.
# Switch the conversion off for every call below; the file paths already go through `cygpath -w`.
export MSYS2_ARG_CONV_EXCL='*'

if [ -z "${KOMPONIST_SIGN_CERT:-}" ] && [ -z "${KOMPONIST_SIGN_THUMBPRINT:-}" ]; then
    exit 0
fi
[ $# -gt 0 ] || { echo "sign-windows.sh: no files given" >&2; exit 2; }

signtool="$(command -v signtool.exe || true)"
if [ -z "$signtool" ]; then
    # Newest SDK version first; `sort -V` is in Git Bash's coreutils.
    signtool="$(ls -d "/c/Program Files (x86)/Windows Kits/10/bin/"*/x64/signtool.exe 2>/dev/null | sort -V | tail -n 1 || true)"
fi
[ -n "$signtool" ] || { echo "sign-windows.sh: signtool.exe not found (Windows SDK)" >&2; exit 1; }
sign=(sign /fd SHA256 /tr "${KOMPONIST_SIGN_TIMESTAMP_URL:-http://timestamp.digicert.com}" /td SHA256)
if [ -n "${KOMPONIST_SIGN_CERT:-}" ]; then
    sign+=(/f "$(cygpath -w "$KOMPONIST_SIGN_CERT")")
    [ -z "${KOMPONIST_SIGN_PASSWORD:-}" ] || sign+=(/p "$KOMPONIST_SIGN_PASSWORD")
else
    sign+=(/sha1 "$KOMPONIST_SIGN_THUMBPRINT")
fi
for file in "$@"; do
    "$signtool" "${sign[@]}" "$(cygpath -w "$file")"
    "$signtool" verify /pa "$(cygpath -w "$file")"
done
