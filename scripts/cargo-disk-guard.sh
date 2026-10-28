#!/bin/bash
# cargo-disk-guard: keep Rust build artifacts from filling the disk.
#
# Why this exists: cargo has no garbage collector, and AI-agent development multiplies the
# problem. Every agent runs in its own git worktree with its own full `target/` (5-20 GiB
# each), and nothing deleted those after the agent's branch was merged. The weekly
# `cargo-sweep --time 7` job only removes artifacts older than a
# week, so it can never catch same-day growth. On 2026-10-21 the data volume hit 99% full
# (17 GiB free) with ~135 GiB of cargo output in this repo alone. This guard runs every 30
# minutes (launchd: scripts/launchd/dev.camerata.cargo-disk-guard.plist) and escalates with
# disk pressure.
#
# Everything it deletes is regenerable build output or an already-merged worktree. It never
# touches a worktree with uncommitted changes, never deletes an unmerged branch's source, and
# never touches anything modified in the last ACTIVE_MINUTES (a running agent or build).
#
# Tiers (free space on the data volume):
#   always          remove merged, idle agent worktrees (+ their branches); drop the target/
#                   of idle, unmerged agent worktrees
#   < TIER1_GIB     cargo-sweep --time 1 across REPOS_ROOT
#   < TIER2_GIB     delete target/*/incremental and target/release in idle repos
#   < TIER3_GIB     delete the whole target/ of every idle repo (full rebuild next time)
#
# Usage: scripts/cargo-disk-guard.sh [--dry-run] [--throttle]
#
# macOS privacy (TCC): a launchd job cannot read your Documents folder unless the executable it runs
# (/bin/bash here) has Full Disk Access. Without it every scheduled run logs "Operation not
# permitted" and does nothing. The weekly cargo-sweep job had the same silent failure. Either grant
# Full Disk Access (System Settings > Privacy & Security > Full Disk Access), or trigger this from
# a Claude Code hook with --throttle, which runs inside an already-permitted process.

set -u

# Default layout: the folder that holds this repository's main checkout is the root that is
# swept. The main checkout is found through the shared git directory, so the default is the
# same whether this script runs from the main checkout or from an agent worktree. Set
# CARGO_GUARD_REPOS_ROOT (and optionally CARGO_GUARD_ORCH_REPO) to sweep a different layout.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
if [ -n "${CARGO_GUARD_REPOS_ROOT:-}" ]; then
    REPOS_ROOT="$CARGO_GUARD_REPOS_ROOT"
    ORCH_REPO="${CARGO_GUARD_ORCH_REPO:-$REPOS_ROOT/camerata-orchestrator}"
else
    COMMON_GIT_DIR="$(git -C "$SCRIPT_DIR" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    if [ -n "$COMMON_GIT_DIR" ]; then
        MAIN_CHECKOUT="$(dirname "$COMMON_GIT_DIR")"
    else
        MAIN_CHECKOUT="$(dirname "$SCRIPT_DIR")"
    fi
    ORCH_REPO="${CARGO_GUARD_ORCH_REPO:-$MAIN_CHECKOUT}"
    REPOS_ROOT="$(dirname "$ORCH_REPO")"
fi
MERGE_TARGETS="${CARGO_GUARD_MERGE_TARGETS:-feat/audit-report-export main}"
ACTIVE_MINUTES="${CARGO_GUARD_ACTIVE_MINUTES:-60}"
TIER1_GIB="${CARGO_GUARD_TIER1_GIB:-150}"
TIER2_GIB="${CARGO_GUARD_TIER2_GIB:-75}"
TIER3_GIB="${CARGO_GUARD_TIER3_GIB:-40}"
VOLUME="${CARGO_GUARD_VOLUME:-/System/Volumes/Data}"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

DRY_RUN=0
THROTTLE=0
for a in "$@"; do
    case "$a" in
        --dry-run) DRY_RUN=1 ;;
        # --throttle: exit immediately if a run finished in the last 30 minutes. Lets a frequent
        # trigger (a Claude Code hook that fires every turn) call this cheaply.
        --throttle) THROTTLE=1 ;;
    esac
done
STAMP="${CARGO_GUARD_STAMP:-$HOME/Library/Caches/cargo-disk-guard.stamp}"
if [ "$THROTTLE" = 1 ] && [ -f "$STAMP" ] && [ -n "$(find "$STAMP" -mmin -30 2>/dev/null)" ]; then
    exit 0
fi
# A second concurrent run (launchd + hook) would only duplicate work; skip if one is running.
LOCK="${STAMP}.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    [ -n "$(find "$LOCK" -mmin -60 2>/dev/null)" ] && exit 0
    rmdir "$LOCK" 2>/dev/null; mkdir "$LOCK" 2>/dev/null || exit 0
fi
trap 'rmdir "$LOCK" 2>/dev/null; [ "$DRY_RUN" = 1 ] || touch "$STAMP"' EXIT

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') cargo-disk-guard: $*"; }

free_gib() {
    # df -g reports 1 GiB blocks; column 4 is "Available".
    df -g "$VOLUME" 2>/dev/null | awk 'NR==2 {print $4}'
}

# True when nothing under $1 changed in the last ACTIVE_MINUTES (no running agent or build).
is_idle() {
    [ -z "$(find "$1" -mmin "-$ACTIVE_MINUTES" -print -quit 2>/dev/null)" ]
}

remove_path() {
    if [ "$DRY_RUN" = 1 ]; then
        log "[dry-run] would remove $1 ($(du -sh "$1" 2>/dev/null | cut -f1))"
    else
        log "removing $1 ($(du -sh "$1" 2>/dev/null | cut -f1))"
        rm -rf "$1"
    fi
}

log "start; free=$(free_gib) GiB"

# ── 1. agent worktrees ─────────────────────────────────────────────────────────────────────
if [ -d "$ORCH_REPO/.git" ] || [ -f "$ORCH_REPO/.git" ]; then
    for wt in "$ORCH_REPO"/.claude/worktrees/agent-*; do
        [ -d "$wt" ] || continue
        if ! is_idle "$wt"; then
            log "skip active worktree $wt"
            continue
        fi
        branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null)"
        dirty="$(git -C "$wt" status --porcelain 2>/dev/null | grep -v ' sample-report/' | head -1)"
        merged=0
        for target in $MERGE_TARGETS; do
            if git -C "$ORCH_REPO" rev-parse --verify -q "$target" >/dev/null &&
                git -C "$ORCH_REPO" merge-base --is-ancestor "$branch" "$target" 2>/dev/null; then
                merged=1
            fi
        done
        if [ "$merged" = 1 ] && [ -z "$dirty" ] && [ -n "$branch" ] && [ "$branch" != "HEAD" ]; then
            if [ "$DRY_RUN" = 1 ]; then
                log "[dry-run] would remove merged worktree $wt (branch $branch)"
            else
                log "removing merged worktree $wt (branch $branch, $(du -sh "$wt" 2>/dev/null | cut -f1))"
                git -C "$ORCH_REPO" worktree unlock "$wt" 2>/dev/null
                git -C "$ORCH_REPO" worktree remove --force "$wt" &&
                    git -C "$ORCH_REPO" branch -D "$branch" >/dev/null 2>&1
            fi
        elif [ -d "$wt/target" ]; then
            # Unmerged or dirty, but idle: keep the source, drop the regenerable build output.
            remove_path "$wt/target"
        fi
    done
    [ "$DRY_RUN" = 1 ] || git -C "$ORCH_REPO" worktree prune 2>/dev/null
fi

# ── 2. pressure tiers across every repo ────────────────────────────────────────────────────
# Every cargo target dir under REPOS_ROOT (a dir named target with a CACHEDIR.TAG, which
# cargo writes into every target dir it creates).
list_targets() {
    find "$REPOS_ROOT" -maxdepth 6 -type f -name CACHEDIR.TAG -path '*/target/CACHEDIR.TAG' 2>/dev/null |
        sed 's|/CACHEDIR.TAG$||'
}

# Decide idleness ONCE, before any tier runs: cargo-sweep (tier 1) rewrites files inside the
# targets it trims, which would otherwise make every swept target look "active" to tiers 2-3.
IDLE_TARGETS="$(list_targets | while read -r t; do
    if is_idle "$t"; then echo "$t"; else log "skip active $t" >&2; fi
done)"

free="$(free_gib)"
if [ -n "$free" ] && [ "$free" -lt "$TIER1_GIB" ]; then
    log "tier 1 (free ${free} GiB < ${TIER1_GIB}): cargo-sweep --time 1"
    if [ "$DRY_RUN" = 0 ] && command -v cargo-sweep >/dev/null; then
        cargo-sweep sweep --recursive --time 1 "$REPOS_ROOT" >/dev/null 2>&1
    fi
fi

free="$(free_gib)"
if [ -n "$free" ] && [ "$free" -lt "$TIER2_GIB" ]; then
    log "tier 2 (free ${free} GiB < ${TIER2_GIB}): incremental caches + release builds of idle repos"
    echo "$IDLE_TARGETS" | while read -r t; do
        [ -n "$t" ] || continue
        for sub in "$t"/*/incremental "$t/release"; do
            [ -d "$sub" ] && remove_path "$sub"
        done
    done
fi

free="$(free_gib)"
if [ -n "$free" ] && [ "$free" -lt "$TIER3_GIB" ]; then
    log "tier 3 (free ${free} GiB < ${TIER3_GIB}): whole target/ of idle repos"
    echo "$IDLE_TARGETS" | while read -r t; do
        [ -n "$t" ] && [ -d "$t" ] && remove_path "$t"
    done
fi

log "done; free=$(free_gib) GiB"
