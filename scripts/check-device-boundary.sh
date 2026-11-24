#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# Git Bash can have a working python command and a Microsoft Store stub for python3.
python=python3
if ! "$python" -c 'import sys' >/dev/null 2>&1; then python=python; fi
exec "$python" scripts/check-device-boundary.py "$@"
