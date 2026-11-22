# Sourced by the local build and packaging scripts.
clap_python=python3
if ! "$clap_python" -c 'import sys' >/dev/null 2>&1; then clap_python=python; fi

native_clap_target() {
    rustc -vV | sed -n 's/^host: //p'
}

stage_native_bundled_clap() {
    lowprio "$clap_python" scripts/bundled-clap.py --target "$(native_clap_target)" --inventory all stage "$1"
}

bundled_clap_manifest_digest() {
    "$clap_python" -c 'import hashlib, pathlib, sys; print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())' "$1/manifest.json"
}
