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
# touches a worktree with uncommitted changes, never deletes any branch, never removes
# a worktree with no commits of its own, and
# never touches anything a running cargo/rustc uses, whose branch committed in the last
# ACTIVE_MINUTES, or that has modified tracked files.
#
# Tiers (free space on the data volume):
#   always          remove merged, idle worktrees under .claude/worktrees/ (any name, + their
#                   branches); drop the target/ of idle, unmerged ones
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

# ── activity detection ──────────────────────────────────────────────────────────────────────
# APFS-cloned targets and cargo-sweep rewrite file mtimes, so mtimes under target/ say nothing.
# Activity is: ANY process of this user with its cwd inside the path or a command line that
# mentions a path inside it (cargo, rustc, cargo-nextest, a test binary under target/, a browser
# started by tests, an agent session); OR its branch's last commit is newer than ACTIVE_MINUTES;
# OR a tracked file is modified; OR any file outside target/ and .git, or the worktree's git
# admin files (HEAD, index), changed within ACTIVE_MINUTES. Fail closed: if ps, lsof or git
# fails, the path counts as active, and the whole run aborts when ps or lsof cannot run.
# The guard's own process tree (including the cargo-sweep it starts) never counts.
LSOF="${CARGO_GUARD_LSOF:-lsof}"
ME_USER="${USER:-$(id -un)}"
PROC_CWDS=""
PROC_ARGS=""
collect_procs() {
    local table excl
    command -v "$LSOF" >/dev/null 2>&1 || { log "ABORT: lsof not found, cannot detect running builds"; exit 1; }
    table="$(ps -axo pid=,ppid=,command= 2>/dev/null)" || { log "ABORT: ps failed"; exit 1; }
    [ -n "$table" ] || { log "ABORT: ps returned nothing"; exit 1; }
    excl="$(printf '%s\n' "$table" | awk -v me="$$" '{pp[$1]=$2} END{ex[me]=1; ch=1; while(ch){ch=0; for(p in pp) if(!(p in ex) && (pp[p] in ex)){ex[p]=1; ch=1}} for(p in ex) printf "%s ", p}')"
    PROC_ARGS="$(printf '%s\n' "$table" | awk -v ex=" $excl " 'index(ex, " " $1 " ")==0 {sub(/^ *[0-9]+ +[0-9]+ +/,""); print $0 " "}')"
    local out
    out="$("$LSOF" -a -d cwd -u "$ME_USER" -Fpn 2>/dev/null)" || { log "ABORT: lsof failed, cannot detect running builds"; exit 1; }
    PROC_CWDS="$(printf '%s\n' "$out" | awk -v ex=" $excl " '/^p/ {skip = (index(ex, " " substr($0,2) " ")>0); next} /^n/ && !skip {print substr($0,2)}')"
}
collect_procs

# Tracked files modified in git checkout $1 (untracked files do not count; same ignores as the
# dirty check plus .claude/settings.json). Returns 2 when git itself fails.
tracked_dirty() {
    local st
    st="$(git -C "$1" status --porcelain --untracked-files=no 2>/dev/null)" || return 2
    printf '%s\n' "$st" | grep -v -e '^$' -e ' sample-report/' -e ' \.claude/settings\.json$' | head -1
}

# Sets ACTIVE_WHY and returns 0 when $1 is in use (see above); 1 when idle.
is_active() {
    local d c ct now gd td rc
    d="$(cd -P "$1" 2>/dev/null && pwd)" || { ACTIVE_WHY="cannot resolve path"; return 0; }
    while IFS= read -r c; do
        [ -n "$c" ] || continue
        case "$c/" in "$d/"*) ACTIVE_WHY="a running process has its cwd at $c"; return 0 ;; esac
    done <<GUARD_EOF
$PROC_CWDS
GUARD_EOF
    if [ -n "$PROC_ARGS" ] && printf '%s\n' "$PROC_ARGS" | grep -qF -e "$d/" -e "$d "; then
        ACTIVE_WHY="a running process command line mentions it"
        return 0
    fi
    if [ -e "$d/.git" ]; then
        if [ "$(git -C "$d" rev-parse --show-toplevel 2>/dev/null)" != "$d" ]; then
            ACTIVE_WHY="git failed"; return 0
        fi
        ct="$(git -C "$d" log -1 --format=%ct 2>/dev/null)"
        [ -n "$ct" ] || { ACTIVE_WHY="git failed"; return 0; }
        now="$(date +%s)"
        if [ $(( (now - ct) / 60 )) -lt "$ACTIVE_MINUTES" ]; then
            ACTIVE_WHY="last commit under ${ACTIVE_MINUTES} min old"
            return 0
        fi
        td="$(tracked_dirty "$d")"; rc=$?
        if [ "$rc" = 2 ]; then ACTIVE_WHY="git failed"; return 0; fi
        if [ -n "$td" ]; then ACTIVE_WHY="tracked files modified"; return 0; fi
        gd="$(git -C "$d" rev-parse --absolute-git-dir 2>/dev/null)" || { ACTIVE_WHY="git failed"; return 0; }
        [ "$ACTIVE_MINUTES" -gt 0 ] 2>/dev/null || { ACTIVE_WHY=""; return 1; }  # BSD find treats -mmin -0 as "now"
        if [ -n "$(find "$gd" -maxdepth 1 \( -name HEAD -o -name index \) -mmin "-$ACTIVE_MINUTES" -print -quit 2>/dev/null)" ]; then
            ACTIVE_WHY="git HEAD/index changed under ${ACTIVE_MINUTES} min ago"
            return 0
        fi
        if [ -n "$(find "$d" \( -name target -o -name .git \) -prune -o -type f -mmin "-$ACTIVE_MINUTES" -print -quit 2>/dev/null)" ]; then
            ACTIVE_WHY="a file changed under ${ACTIVE_MINUTES} min ago"
            return 0
        fi
    fi
    ACTIVE_WHY=""
    return 1
}

# Re-check right before a removal: processes may have started since the run began.
still_idle() {
    collect_procs
    if is_active "$1"; then log "keep $1: became active ($ACTIVE_WHY)"; return 1; fi
    return 0
}

# Only regenerable build output goes through here: a target/ dir or something under one, never a
# symlink.
remove_path() {
    case "$1" in
        */target|*/target/*) ;;
        *) log "REFUSED to remove $1: not a target/ path"; return ;;
    esac
    if [ -L "$1" ]; then log "REFUSED to remove $1: symlink"; return; fi
    if [ "$DRY_RUN" = 1 ]; then
        log "[dry-run] would remove $1 ($(du -sh "$1" 2>/dev/null | cut -f1))"
    else
        log "removing $1 ($(du -sh "$1" 2>/dev/null | cut -f1))"
        rm -rf "$1"
    fi
}

# True when the tip of branch $1 is a real merge into a MERGE_TARGET: reachable from the target
# but NOT on its first-parent line. A branch with no work of its own (fresh, or still sitting at
# an old base) sits on that line, so it never counts as merged.
branch_merged() {
    local br="$1" tip target fp
    tip="$(git -C "$ORCH_REPO" rev-parse --verify -q "refs/heads/$br" 2>/dev/null)" || return 1
    for target in $MERGE_TARGETS; do
        git -C "$ORCH_REPO" rev-parse --verify -q "$target" >/dev/null || continue
        git -C "$ORCH_REPO" merge-base --is-ancestor "$tip" "$target" 2>/dev/null || continue
        fp="$(git -C "$ORCH_REPO" rev-list --first-parent "$target" 2>/dev/null)" || continue
        printf '%s\n' "$fp" | grep -qx "$tip" && continue
        return 0
    done
    return 1
}

log "start; free=$(free_gib) GiB"

# ── 1. worktrees ───────────────────────────────────────────────────────────────────────────
# Removes only whole worktrees (git worktree remove, no force) that are merged, clean, have work
# of their own and are idle. Branches are NEVER deleted (branch cleanup is manual).
if [ -d "$ORCH_REPO/.git" ] || [ -f "$ORCH_REPO/.git" ]; then
    for wt in "$ORCH_REPO"/.claude/worktrees/*; do
        [ -e "$wt" ] || continue
        if [ -L "$wt" ]; then log "keep $wt: symlink, ignored"; continue; fi
        [ -d "$wt" ] || continue
        [ -e "$wt/.git" ] || continue
        if is_active "$wt"; then
            log "keep worktree $wt: active ($ACTIVE_WHY)"
            continue
        fi
        gd="$(git -C "$wt" rev-parse --absolute-git-dir 2>/dev/null)"
        if [ -n "$gd" ] && [ -e "$gd/locked" ]; then
            log "keep worktree $wt: locked"
            continue
        fi
        branch="$(git -C "$wt" rev-parse --abbrev-ref HEAD 2>/dev/null)"
        if ! st="$(git -C "$wt" status --porcelain 2>/dev/null)"; then
            log "keep worktree $wt: git status failed"
            continue
        fi
        dirty="$(printf '%s\n' "$st" | grep -v -e '^$' -e ' sample-report/' -e ' \.claude/settings\.json$' | head -1)"
        merged=0
        if [ -n "$branch" ] && [ "$branch" != "HEAD" ] && branch_merged "$branch"; then merged=1; fi
        why="merged=$merged, dirty=$([ -n "$dirty" ] && echo yes || echo no), branch=$branch"
        if [ "$merged" = 1 ] && [ -z "$dirty" ]; then
            if [ "$DRY_RUN" = 1 ]; then
                log "[dry-run] would remove merged worktree $wt (branch $branch): merged, clean, idle, has own commits"
                log "branch kept: $branch (branch cleanup is manual)"
            else
                still_idle "$wt" || continue
                sz="$(du -sh "$wt" 2>/dev/null | cut -f1)"
                ign="$(git -C "$wt" status --porcelain --ignored 2>/dev/null | grep '^!!' | grep -v ' target/$' | head -5)"
                [ -z "$ign" ] || log "ignored files that go with $wt: $(printf '%s' "$ign" | tr '\n' ' ')"
                if git -C "$ORCH_REPO" worktree remove "$wt" 2>/dev/null; then
                    log "removed merged worktree $wt (branch $branch, $sz)"
                    log "branch kept: $branch (branch cleanup is manual)"
                else
                    log "keep worktree $wt: git refused to remove it"
                fi
            fi
        elif [ -d "$wt/target" ]; then
            free="$(free_gib)"
            ct="$(git -C "$wt" log -1 --format=%ct 2>/dev/null)"
            age_h=0; [ -n "$ct" ] && age_h=$(( ($(date +%s) - ct) / 3600 ))
            drop=0
            if [ -n "$free" ] && [ "$free" -lt "$TIER2_GIB" ]; then drop=1
            elif [ -n "$free" ] && [ "$free" -lt "$TIER1_GIB" ] && [ -n "$ct" ] && [ "$age_h" -ge 24 ]; then drop=1; fi
            if [ "$drop" = 1 ]; then
                log "keep worktree $wt source ($why); dropping idle target/ (free ${free} GiB, last commit ${age_h} h old)"
                if [ "$DRY_RUN" = 1 ] || still_idle "$wt"; then remove_path "$wt/target"; fi
            else
                log "keep worktree $wt ($why); target/ kept (free ${free} GiB, tier 1 ${TIER1_GIB}, tier 2 ${TIER2_GIB}, last commit ${age_h} h old; needs free < tier 2, or free < tier 1 and commit over 24 h old)"
            fi
        else
            log "keep worktree $wt: not removable ($why)"
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

# Decide idleness ONCE, before any tier runs. Activity is a live cargo/rustc process (collected
# above, before cargo-sweep runs), a recent commit, or modified tracked files in the owning
# checkout; never file mtimes, which APFS clones and cargo-sweep itself rewrite.
IDLE_TARGETS="$(list_targets | while read -r t; do
    if is_active "$(dirname "$t")"; then log "skip active $t ($ACTIVE_WHY)" >&2; else echo "$t"; fi
done)"

free="$(free_gib)"
if [ -n "$free" ] && [ "$free" -lt "$TIER1_GIB" ]; then
    log "tier 1 (free ${free} GiB < ${TIER1_GIB}): cargo-sweep --time 1"
    # Idle targets only: a sweep over a target a build is writing removed object files the
    # linker was about to read and failed the build (2026-10-28, twice).
    if [ "$DRY_RUN" = 0 ] && command -v cargo-sweep >/dev/null; then
        echo "$IDLE_TARGETS" | while read -r t; do
            [ -n "$t" ] || continue
            cargo-sweep sweep --time 1 "$(dirname "$t")" >/dev/null 2>&1
        done
    fi
fi

free="$(free_gib)"
if [ -n "$free" ] && [ "$free" -lt "$TIER2_GIB" ]; then
    log "tier 2 (free ${free} GiB < ${TIER2_GIB}): incremental caches + release builds of idle repos"
    echo "$IDLE_TARGETS" | while read -r t; do
        [ -n "$t" ] || continue
        for sub in "$t"/*/incremental "$t/release"; do
            [ -d "$sub" ] || continue
            [ "$DRY_RUN" = 1 ] || still_idle "$(dirname "$t")" || break
            remove_path "$sub"
        done
    done
fi

free="$(free_gib)"
if [ -n "$free" ] && [ "$free" -lt "$TIER3_GIB" ]; then
    log "tier 3 (free ${free} GiB < ${TIER3_GIB}): whole target/ of idle repos"
    echo "$IDLE_TARGETS" | while read -r t; do
        [ -n "$t" ] && [ -d "$t" ] || continue
        [ "$DRY_RUN" = 1 ] || still_idle "$(dirname "$t")" || continue
        remove_path "$t"
    done
fi

log "done; free=$(free_gib) GiB"
