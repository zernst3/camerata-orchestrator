#!/bin/bash
# Shell test for scripts/cargo-disk-guard.sh. Builds a throwaway repo with worktrees in a temp
# folder and asserts, with --dry-run only, exactly which worktrees the guard would remove.
# The temp folder is left in place; its path is printed. Usage: test-cargo-disk-guard.sh [guard-script]
set -u
GUARD="${1:-$(cd "$(dirname "$0")" && pwd)/cargo-disk-guard.sh}"
T="$(mktemp -d)"; T="$(cd -P "$T" && pwd)"
echo "temp folder: $T"
ORCH="$T/orch"; WT="$ORCH/.claude/worktrees"
g() { git -C "$ORCH" -c user.email=t@t -c user.name=t "$@"; }
mkdir -p "$ORCH" "$T/bin" && cd "$ORCH" || exit 1
git init -q -b main . && echo a > f.txt && g add f.txt && g commit -q -m init
mkdir -p "$WT"
# merged branches: commit on the branch, then fast-forward main to it.
for n in mc md mu run; do
    g worktree add -q -b "$n" "$WT/$n" main
    git -C "$WT/$n" -c user.email=t@t -c user.name=t commit -q --allow-empty -m "work $n"
    g merge -q --ff-only "$n"
done
g worktree add -q -b um "$WT/um" main
git -C "$WT/um" -c user.email=t@t -c user.name=t commit -q --allow-empty -m "unmerged work"
echo changed >> "$WT/md/f.txt"      # merged, tracked file modified
echo new > "$WT/mu/untracked.txt"   # merged, untracked file only
# stand-in running "cargo" whose cwd is inside the run worktree (a symlink gives it the name)
ln -s /bin/sleep "$T/bin/cargo"
(cd "$WT/run" && exec "$T/bin/cargo" 120) &
STANDIN=$!
trap 'kill $STANDIN 2>/dev/null' EXIT
sleep 1

out="$(CARGO_GUARD_REPOS_ROOT="$T" CARGO_GUARD_ORCH_REPO="$ORCH" CARGO_GUARD_MERGE_TARGETS=main \
    CARGO_GUARD_ACTIVE_MINUTES=0 CARGO_GUARD_STAMP="$T/stamp" CARGO_GUARD_VOLUME="$T" \
    CARGO_GUARD_TIER1_GIB=0 CARGO_GUARD_TIER2_GIB=0 CARGO_GUARD_TIER3_GIB=0 \
    bash "$GUARD" --dry-run 2>&1)"
echo "$out" | sed 's/^/  guard: /'

fail=0
check() { # name, expected (yes|no) that the worktree is listed as would-remove
    local listed=no
    echo "$out" | grep -q "would remove merged worktree $WT/$1 " && listed=yes
    if [ "$listed" = "$2" ]; then echo "PASS $1 removed=$listed"; else echo "FAIL $1 expected removed=$2 got $listed"; fail=1; fi
}
check mc yes   # merged, clean, idle
check md no    # merged but tracked file modified
check mu no    # merged but untracked file present (dirty)
check um no    # unmerged
check run no   # merged clean but a running cargo has its cwd inside
echo "$out" | grep -q "would remove merged worktree $ORCH " && { echo "FAIL main checkout listed"; fail=1; }
[ "$fail" = 0 ] && echo "ALL PASS" || echo "FAILED"
exit "$fail"
