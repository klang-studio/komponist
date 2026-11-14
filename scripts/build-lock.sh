# Sourced by the build scripts (check.sh, check-gui.sh, run.sh): one heavy build at a time across all
# worktrees, at low CPU and I/O priority, so the owner's desktop stays responsive (2026-11-14).
# The lock is held until the sourcing shell exits or closes fd 9.

exec 9>"${XDG_RUNTIME_DIR:-/tmp}/komponist-build.lock"
if ! flock -n 9; then
    echo "waiting for the build lock (another worktree is building) …" >&2
    flock 9
fi
renice -n 10 $$ >/dev/null
ionice -c2 -n7 -p $$ 2>/dev/null || true
