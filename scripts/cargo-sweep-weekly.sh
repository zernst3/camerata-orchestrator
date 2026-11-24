#!/bin/bash
# Weekly cargo-sweep that never touches a kept scan build.
#
# Replaces the unguarded `cargo-sweep sweep --recursive --time 7 <repos folder>` job, which would
# sweep an idle kept scan build (scripts/keep-scan-build.sh) once it is a week old. This wrapper
# finds every cargo target dir under REPOS_ROOT the way cargo-disk-guard.sh does (a CACHEDIR.TAG
# inside a dir named target), skips any path with a camerata-scan-* component, and sweeps the rest
# one project at a time.
#
# Usage: scripts/cargo-sweep-weekly.sh [--dry-run]
# Env:   CARGO_SWEEP_REPOS_ROOT  folder to scan (default: the folder holding the main checkout)
#        CARGO_SWEEP_DAYS        age in days passed as --time (default 7)
# Install: scripts/install-cargo-sweep-weekly.sh
set -u
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# Twin of the root resolution in cargo-disk-guard.sh: keep the two in step.
if [ -n "${CARGO_SWEEP_REPOS_ROOT:-}" ]; then
    REPOS_ROOT="$CARGO_SWEEP_REPOS_ROOT"
else
    COMMON_GIT_DIR="$(git -C "$SCRIPT_DIR" rev-parse --path-format=absolute --git-common-dir 2>/dev/null || true)"
    if [ -n "$COMMON_GIT_DIR" ]; then MAIN_CHECKOUT="$(dirname "$COMMON_GIT_DIR")"; else MAIN_CHECKOUT="$(dirname "$SCRIPT_DIR")"; fi
    REPOS_ROOT="$(dirname "$MAIN_CHECKOUT")"
fi
REPOS_ROOT="$(cd -P "$REPOS_ROOT" 2>/dev/null && pwd)" || { echo "cargo-sweep-weekly: cannot resolve REPOS_ROOT" >&2; exit 1; }
DAYS="${CARGO_SWEEP_DAYS:-7}"
# launchd gives a bare PATH; add the usual places AFTER the caller's PATH (a test puts a fake first).
export PATH="$PATH:$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
DRY_RUN=0
for a in "$@"; do [ "$a" = "--dry-run" ] && DRY_RUN=1; done

log() { echo "$(date '+%Y-%m-%d %H:%M:%S') cargo-sweep-weekly: $*"; }
# Twin of is_scan_path in cargo-disk-guard.sh: a path with a camerata-scan-* component.
is_scan_path() { case "$1/" in */camerata-scan-*/*) return 0 ;; esac; return 1; }

command -v cargo-sweep >/dev/null 2>&1 || { log "cargo-sweep not found"; exit 1; }
log "start; root=$REPOS_ROOT, older than $DAYS days"
find "$REPOS_ROOT" -maxdepth 6 -type f -name CACHEDIR.TAG -path '*/target/CACHEDIR.TAG' 2>/dev/null |
    sed 's|/CACHEDIR.TAG$||' | while read -r t; do
    if is_scan_path "$t"; then log "keep ${t#"${t%%camerata-scan-*}"} (kept scan build, never swept)"; continue; fi
    proj="$(dirname "$t")"
    if [ "$DRY_RUN" = 1 ]; then log "[dry-run] would sweep $proj"; continue; fi
    log "sweeping $proj"
    cargo-sweep sweep --time "$DAYS" "$proj" >/dev/null 2>&1 || log "cargo-sweep failed for $proj"
done
log "done"
