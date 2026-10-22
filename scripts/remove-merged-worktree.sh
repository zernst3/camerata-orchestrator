#!/bin/bash
# Remove an agent worktree (and its local branch) ONLY if its branch is already merged into
# the current branch of the main checkout. Refuses otherwise. Used by orchestrating sessions
# right after merging an agent's work (see AGENTS.md "Disk hygiene").
#
# Usage: scripts/remove-merged-worktree.sh <agent-id | worktree-path>
set -eu
repo="$(cd "$(dirname "$0")/.." && pwd)"
arg="${1:?usage: remove-merged-worktree.sh <agent-id | worktree-path>}"
case "$arg" in
    /*) wt="$arg" ;;
    *) wt="$repo/.claude/worktrees/agent-${arg#agent-}" ;;
esac
[ -d "$wt" ] || { echo "no worktree at $wt"; exit 0; }
branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD)"
into="$(git -C "$repo" rev-parse --abbrev-ref HEAD)"
if ! git -C "$repo" merge-base --is-ancestor "$branch" "$into"; then
    echo "REFUSED: $branch is not merged into $into; leaving $wt alone"
    exit 1
fi
git -C "$repo" worktree unlock "$wt" 2>/dev/null || true
git -C "$repo" worktree remove --force "$wt"
git -C "$repo" branch -D "$branch" >/dev/null
echo "removed $wt (branch $branch, merged into $into)"
