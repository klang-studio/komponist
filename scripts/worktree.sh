#!/usr/bin/env bash
# Creates or removes an agent worktree. Worktrees live in klang-studio/.claude/worktrees/ (owner,
# 2026-11-14), and their build folders live on the NVMe so build I/O stays off the system disk.
#
#   scripts/worktree.sh add kp-wt-WP-82 wp/WP-82      # branch from main
#   scripts/worktree.sh remove kp-wt-WP-82            # worktree, its build folders and nothing else
#
# KOMPONIST_TARGET_ROOT overrides the build root (default <build-root>); if the
# root does not exist, the worktree keeps a normal target/ folder.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

trees=<workspace>/.claude/worktrees
root="${KOMPONIST_TARGET_ROOT:-<build-root>}"
cmd="${1:?add|remove}"
name="${2:?worktree name, e.g. kp-wt-WP-82}"
dir="$trees/$name"

case "$cmd" in
add)
    branch="${3:?branch, e.g. wp/WP-82}"
    git worktree add "$dir" -b "$branch" main
    if [ -d "$root" ]; then
        mkdir -p "$root/$name/target" "$root/$name/test-plugins-target"
        ln -s "$root/$name/target" "$dir/target"
        ln -s "$root/$name/test-plugins-target" "$dir/test-plugins/target"
    fi
    echo "$dir"
    ;;
remove)
    # Stop a rust-analyzer or cargo check still running in that worktree, then remove it.
    pids=$(pgrep -f "[m]anifest-path $dir/" || true)
    [ -n "$pids" ] && kill -TERM $pids || true
    git worktree remove --force "$dir" 2>/dev/null || { rm -rf "$dir"; git worktree prune; }
    rm -rf "${root:?}/$name"
    ;;
*)
    echo "usage: $0 add <name> <branch> | remove <name>" >&2
    exit 2
    ;;
esac
