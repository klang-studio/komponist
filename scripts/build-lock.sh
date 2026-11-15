# Sourced by the build scripts (check.sh, check-gui.sh, run.sh): one heavy build at a time across all
# worktrees, so the owner's desktop stays responsive (2026-11-14). The lock is held until the sourcing
# shell exits or closes fd 9.
#
# `low_priority` lowers CPU and I/O priority of the calling (sub)shell. Use it for compiling only:
# timing-sensitive tests run at normal priority, otherwise other agents' builds starve them and they
# flake (seen 2026-11-15). Call it inside a subshell — a process cannot raise its priority again.

exec 9>"${XDG_RUNTIME_DIR:-/tmp}/komponist-build.lock"
if ! flock -n 9; then
    echo "waiting for the build lock (another worktree is building) …" >&2
    flock 9
fi

low_priority() {
    renice -n 10 "$BASHPID" >/dev/null
    ionice -c2 -n7 -p "$BASHPID" 2>/dev/null || true
}
