#!/usr/bin/env bash
# The GUI part of the gate: tests that need a real X11 display (plugin frames as real windows,
# acceptance flow 4's GUI step). Runs them under a private Xvfb, never on the desktop, with the
# WP-27 test plugins (built here). HOME and the XDG folders are scratch folders under target/tmp,
# so nothing reaches the real home or ~/.config.
#
# Linux only (Xvfb, xdotool): on macOS it says so and exits with 0. The plugin frame tests there are covered
# by hand, see the macOS section of CLAUDE.md.
set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
    echo "check-gui.sh runs on Linux only (it needs Xvfb); skipping on $(uname -s)." >&2
    exit 0
fi

cd "$(dirname "${BASH_SOURCE[0]}")/.."
# Before XDG_RUNTIME_DIR is redirected below, so the lock file is the shared one.
source scripts/build-lock.sh

lowprio scripts/build-test-plugins.sh >/dev/null

# The toolchain lives in the real home; keep it reachable.
export CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}"

scratch="$PWD/target/tmp/check-gui-home"
rm -rf "$scratch"
mkdir -p "$scratch"/{home,config,data,cache,run}
chmod 700 "$scratch/run"

export HOME="$scratch/home"
export XDG_CONFIG_HOME="$scratch/config"
export XDG_DATA_HOME="$scratch/data"
export XDG_CACHE_HOME="$scratch/cache"
export XDG_RUNTIME_DIR="$scratch/run"
export KP_GUI_TESTS=1

# One binary per script: winit allows one event loop per process.
for test in acceptance_flow4_gui plugin_frame_gui; do
    xvfb-run -a -s "-screen 0 1920x1080x24" \
        cargo test -p komponist-app --test "$test" -- --ignored --nocapture
done
