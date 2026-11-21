#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 4 ]]; then
    echo "usage: $0 CLAP_INCLUDE ABI_INCLUDE EXAMPLE_JSON OUTPUT_DIR" >&2
    exit 2
fi

source_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
clap_include="$1"
abi_include="$2"
example_json="$3"
output="$4"

case "$(uname -s)" in
    Linux) shared=(-fPIC -fvisibility=hidden -shared); probe_flags=(-ldl); probe=probe ;;
    Darwin) shared=(-fPIC -fvisibility=hidden -dynamiclib); probe_flags=(); probe=probe ;;
    MINGW*|MSYS*|CYGWIN*) shared=(-shared); probe_flags=(); probe=probe.exe ;;
    *) echo "unsupported C fixture platform" >&2; exit 1 ;;
esac

mkdir -p "$output"
python3 "$source_dir/embed_layout.py" "$example_json" "$output/fixture-layouts.h"
common=(-std=c11 -Wall -Wextra -Werror -pedantic -I "$clap_include" -I "$abi_include")
"${CC:-cc}" "${common[@]}" "${shared[@]}" -I "$output" "$source_dir/plugin.c" -o "$output/layout-fixture.clap"
"${CC:-cc}" "${common[@]}" "$source_dir/probe.c" "${probe_flags[@]}" -o "$output/$probe"
