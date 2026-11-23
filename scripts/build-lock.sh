# Sourced by the build scripts (check.sh, check-gui.sh, run.sh): one heavy build at a time across all
# worktrees, so the owner's desktop stays responsive (2026-11-14). The lock is held until the sourcing
# shell exits (Linux with flock: or closes fd 9).
#
# Portable (Linux and macOS, bash 3.2 included): with `flock` (Linux) it locks a file; without it (macOS has
# none) it takes the lock by creating a directory with `mkdir`, which is atomic. That directory holds the pid
# of the owner, so a lock whose owner died (kill -9, power loss) is broken instead of blocking for ever.
#
# `lowprio CMD …` runs CMD at lower CPU priority (and lower I/O priority where `ionice` exists, i.e. Linux).
# Use it for compiling only: timing-sensitive tests run at normal priority, otherwise other agents' builds
# starve them and they flake (seen 2026-11-15). It wraps the command because a process cannot raise its
# priority again, and bash 3.2 has no $BASHPID to renice a subshell.

_kp_lock_base="${XDG_RUNTIME_DIR:-/tmp}/komponist-build"

if [ -n "${KOMPONIST_BUILD_LOCK_HELD:-}" ]; then
    # A script that holds the lock started this one (check.sh runs check-warp-compat.sh): waiting for
    # the lock again would deadlock.
    :
elif command -v flock >/dev/null 2>&1; then
    exec 9>"$_kp_lock_base.lock"
    if ! flock -n 9; then
        echo "waiting for the build lock (another worktree is building) …" >&2
        flock 9
    fi
else
    _kp_lock_dir="$_kp_lock_base.lockdir"
    _kp_lock_announced=0
    until mkdir "$_kp_lock_dir" 2>/dev/null; do
        _kp_holder="$(cat "$_kp_lock_dir/pid" 2>/dev/null || true)"
        _kp_stale=0
        if [ -n "$_kp_holder" ]; then
            kill -0 "$_kp_holder" 2>/dev/null || _kp_stale=1
        elif [ -d "$_kp_lock_dir" ] && [ -n "$(find "$_kp_lock_dir" -maxdepth 0 -mmin +1 2>/dev/null)" ]; then
            # No pid file for over a minute: the owner died between mkdir and writing it.
            _kp_stale=1
        fi
        if [ "$_kp_stale" = 1 ]; then
            echo "breaking a stale build lock (owner ${_kp_holder:-unknown} is gone) …" >&2
            # mv is atomic: of several waiters only one gets to remove the dead lock.
            if mv "$_kp_lock_dir" "$_kp_lock_dir.stale.$$" 2>/dev/null; then
                rm -rf "$_kp_lock_dir.stale.$$"
            fi
            continue
        fi
        if [ "$_kp_lock_announced" = 0 ]; then
            echo "waiting for the build lock (another worktree is building) …" >&2
            _kp_lock_announced=1
        fi
        sleep 2
    done
    echo "$$" >"$_kp_lock_dir/pid"
    # Released when the sourcing shell exits (a subshell that sources this has its own trap).
    trap 'rm -rf "$_kp_lock_dir"' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
fi

# Children that source this file (check.sh -> check-warp-compat.sh) must not wait for the lock their parent holds.
export KOMPONIST_BUILD_LOCK_HELD=1

lowprio() {
    if command -v ionice >/dev/null 2>&1; then
        ionice -c2 -n7 nice -n 10 "$@"
    else
        nice -n 10 "$@"
    fi
}
