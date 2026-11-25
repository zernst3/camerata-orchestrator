#!/bin/bash
# Shell test for scripts/cargo-disk-guard.sh. Builds a throwaway repo with worktrees in a temp
# folder, asserts with --dry-run which worktrees would be removed, then runs one REAL pass inside
# that temp repo only (REPOS_ROOT/ORCH_REPO point at the temp folder) and asserts what survives.
# The temp folder is left in place; its path is printed. Usage: test-cargo-disk-guard.sh [guard-script]
set -u
GUARD="${1:-$(cd "$(dirname "$0")" && pwd)/cargo-disk-guard.sh}"
T="$(mktemp -d)"; T="$(cd -P "$T" && pwd)"
echo "temp folder: $T"
ORCH="$T/orch"; WT="$ORCH/.claude/worktrees"
g() { git -C "$ORCH" -c user.email=t@t -c user.name=t "$@"; }
gw() { local d="$1"; shift; git -C "$d" -c user.email=t@t -c user.name=t "$@"; }
[ -n "$T" ] || exit 1
mkdir -p "$ORCH" "$T/bin" && cd "$ORCH" || exit 1
git init -q -b main . && echo a > f.txt && echo 'target/' > .gitignore && g add f.txt .gitignore && g commit -q -m init
mkdir -p "$WT"
# fresh worktrees with no commits: one at the current tip, one at a base that main then outgrows
g worktree add -q -b fr "$WT/fr" main
g worktree add -q -b fo "$WT/fo" main
# merged worktrees: commit on the branch, then a real merge (no fast-forward) into main.
mkbranch() { # name dir
    g worktree add -q -b "$1" "$2" main
    gw "$2" commit -q --allow-empty -m "work $1"
    g merge -q --no-ff -m "merge $1" "$1"
}
for n in mc md mu run ra tb gf wl; do mkbranch "$n" "$WT/$n"; done
mkbranch "sp" "$WT/sp ace"
mkbranch "backup/x" "$WT/bk"
mkbranch "scan-build/y" "$WT/sb"
mkbranch "ext" "$T/ext"; ln -s "$T/ext" "$WT/link"
g worktree add -q -b um "$WT/um" main
gw "$WT/um" commit -q --allow-empty -m "unmerged work"
g worktree add -q -b uo "$WT/uo" main   # unmerged, last commit 2 days old
GIT_AUTHOR_DATE="2020-01-01T00:00:00" GIT_COMMITTER_DATE="2020-01-01T00:00:00" gw "$WT/uo" commit -q --allow-empty -m "old unmerged work"
mkdir -p "$WT/uo/target"
g checkout -q --detach; g worktree add -q "$WT/wm" main   # a worktree sitting on main itself
g worktree lock "$WT/wl"
echo changed >> "$WT/md/f.txt"       # merged, tracked file modified
echo new > "$WT/mu/untracked.txt"    # merged, untracked file only
# corrupt index: git status fails in this worktree
GD="$(git -C "$WT/gf" rev-parse --absolute-git-dir)"; echo garbage > "$GD/index"
git init -q -b main "$T/other" && echo s > "$T/other/src.txt" && git -C "$T/other" add src.txt && git -C "$T/other" -c user.email=t@t -c user.name=t commit -q -m init
mkdir -p "$WT/mc/target" "$WT/um/target" "$WT/run/target" "$T/other/target" "$WT/mc/target/x"
for d in "$WT/mc/target" "$WT/um/target" "$WT/run/target" "$WT/uo/target" "$T/other/target"; do echo "Signature: 8a477f597d28d172789f06886806bc55" > "$d/CACHEDIR.TAG"; done
# a kept scan build: idle, release build plus incremental cache, never swept at any tier
SC="$T/camerata-scan-t1"
mkdir -p "$SC/target/release" "$SC/target/debug/incremental"
echo bin > "$SC/target/release/camerata-server"; echo inc > "$SC/target/debug/incremental/x"
echo "Signature: 8a477f597d28d172789f06886806bc55" > "$SC/target/CACHEDIR.TAG"
# a merged, clean worktree whose folder is named camerata-scan-*: must be skipped by the worktree loop
mkbranch "scw" "$WT/camerata-scan-w"
# a folder that only CONTAINS the name is not a kept build: matching is on a path component
mkdir -p "$T/my-camerata-scan-x/target"; echo "Signature: 8a477f597d28d172789f06886806bc55" > "$T/my-camerata-scan-x/target/CACHEDIR.TAG"
# kept builds reached through a relative root and through a symlink with a trailing slash
for n in t2 t3; do
    mkdir -p "$T/camerata-scan-$n/target/release"; echo bin > "$T/camerata-scan-$n/target/release/camerata-server"
    echo "Signature: 8a477f597d28d172789f06886806bc55" > "$T/camerata-scan-$n/target/CACHEDIR.TAG"
done
ln -s "$T/camerata-scan-t3" "$T/lnk"
# stand-ins for running builds. A symlink named cargo gives sleep that process name.
ln -s /bin/sleep "$T/bin/cargo"
printf '#!/bin/bash\nsleep 120\n' > "$T/bin/rustc"; chmod +x "$T/bin/rustc"
mkdir -p "$WT/tb/target/debug/deps"; ln -s /bin/sleep "$WT/tb/target/debug/deps/testbin"
PIDS=""
(cd "$WT/run" && exec "$T/bin/cargo" 120) >/dev/null 2>&1 & PIDS="$PIDS $!"
(cd "$T" && exec "$T/bin/rustc" --manifest-path "$WT/ra/Cargo.toml" --out-dir "$WT/ra/target") >/dev/null 2>&1 & PIDS="$PIDS $!"
(cd "$T" && exec "$WT/tb/target/debug/deps/testbin" 120) >/dev/null 2>&1 & PIDS="$PIDS $!"
trap 'pkill -P ${PIDS// /,} 2>/dev/null; kill $PIDS 2>/dev/null' EXIT
cd "$T"   # the test shell must not sit inside the repo under test
sleep 1

runguard() { # extra env first via env, args after
    CARGO_GUARD_REPOS_ROOT="$T" CARGO_GUARD_ORCH_REPO="$ORCH" CARGO_GUARD_MERGE_TARGETS=main \
    CARGO_GUARD_ACTIVE_MINUTES=0 CARGO_GUARD_STAMP="$T/stamp" CARGO_GUARD_VOLUME="$T" \
    CARGO_GUARD_TIER1_GIB="${T1:-${TIER:-0}}" CARGO_GUARD_TIER2_GIB="${T2:-${TIER:-0}}" CARGO_GUARD_TIER3_GIB="${T3:-${TIER:-0}}" \
    bash "$GUARD" "$@" 2>&1
}
out="$(runguard --dry-run)"
echo "$out" | sed 's/^/  guard: /'

fail=0
ok() { if [ "$2" = "$3" ]; then echo "PASS $1 ($3)"; else echo "FAIL $1 expected=$2 got=$3"; fail=1; fi; }
listed() { echo "$out" | grep -qF "would remove merged worktree $WT/$1 " && echo remove || echo keep; }
ok "dry mc merged clean idle" remove "$(listed mc)"
ok "dry sp path with spaces" remove "$(listed 'sp ace')"
ok "dry bk backup/x merged (worktree only)" remove "$(listed bk)"
for b in mc sp backup/x scan-build/y um; do
    git -C "$ORCH" rev-parse --verify -q "refs/heads/$b" >/dev/null && r=exists || r=deleted
    ok "dry run deletes no branch ($b)" exists "$r"
done
echo "$out" | grep -qF "[dry-run] would delete merged branch mc" && r=logged || r=silent; ok "dry run logs would-delete for mc" logged "$r"
echo "$out" | grep -qF "would delete merged branch backup/x" && r=logged || r=silent; ok "dry run does not offer backup/x" silent "$r"
ok "dry fr fresh no commits" keep "$(listed fr)"
ok "dry fo fresh at old base" keep "$(listed fo)"
ok "dry wm worktree on main" keep "$(listed wm)"
ok "dry md tracked modified" keep "$(listed md)"
ok "dry mu untracked (dirty)" keep "$(listed mu)"
ok "dry um unmerged" keep "$(listed um)"
ok "dry wl locked" keep "$(listed wl)"
ok "dry run cargo cwd" keep "$(listed run)"
ok "dry ra rustc args" keep "$(listed ra)"
ok "dry tb direct test binary" keep "$(listed tb)"
ok "dry gf git failure" keep "$(listed gf)"
ok "dry link symlink" keep "$(listed link)"
ok "dry ext via symlink target" keep "$(listed ext)"
echo "$out" | grep -q "$WT/ra: active" && r=active || r=other; ok "reason ra is process" active "$r"
echo "$out" | grep -q "$WT/tb: active" && r=active || r=other; ok "reason tb is process" active "$r"
echo "$out" | grep -q "$WT/gf: " && echo "$out" | grep "$WT/gf: " | grep -qi "git" && r=git || r=other; ok "reason gf is git failure" git "$r"

# fail closed: no lsof means abort and nothing happens
CARGO_GUARD_LSOF=/nonexistent/lsof runguard >/dev/null; rc=$?
[ "$rc" != 0 ] && r=aborted || r=ran; ok "lsof missing aborts" aborted "$r"
[ -d "$WT/mc" ] && r=present || r=gone; ok "lsof missing leaves mc" present "$r"

# fail closed on freshness: with 5 minutes of activity window everything just created is kept
out5="$(CARGO_GUARD_ACTIVE_MINUTES=5 bash -c 'true'; CARGO_GUARD_REPOS_ROOT="$T" CARGO_GUARD_ORCH_REPO="$ORCH" CARGO_GUARD_MERGE_TARGETS=main CARGO_GUARD_ACTIVE_MINUTES=5 CARGO_GUARD_STAMP="$T/stamp" CARGO_GUARD_VOLUME="$T" CARGO_GUARD_TIER1_GIB=0 CARGO_GUARD_TIER2_GIB=0 CARGO_GUARD_TIER3_GIB=0 bash "$GUARD" --dry-run 2>&1)"
echo "$out5" | grep -qF "would remove merged worktree" && r=removed || r=none; ok "recent activity keeps all" none "$r"

# um/target (fresh unmerged) and uo/target (unmerged, 2 days old) under the tier rules, dry-run
tgt() { runguard --dry-run > "$T/tgt.out"; grep -qF "would remove $WT/$1/target" "$T/tgt.out" && echo drop || echo keep; }
ok "above thresholds: um/target kept" keep "$(T1=0 T2=0 T3=0 tgt um)"
ok "below tier1 only: fresh um/target kept" keep "$(T1=100000 T2=0 T3=0 tgt um)"
ok "below tier1 only: 2-day-old uo/target dropped" drop "$(T1=100000 T2=0 T3=0 tgt uo)"
ok "above thresholds: uo/target kept" keep "$(T1=0 T2=0 T3=0 tgt uo)"
ok "below tier2: fresh um/target dropped" drop "$(T1=100000 T2=100000 T3=0 tgt um)"

# REAL pass inside the temp repo only, with real (huge) thresholds so every tier fires
TIER=100000 runguard | sed 's/^/  real: /'
for b in main backup/x scan-build/y uo fr fo md mu run ra tb gf wl um ext; do
    git -C "$ORCH" rev-parse --verify -q "refs/heads/$b" >/dev/null && r=exists || r=deleted
    ok "branch $b survives" exists "$r"
done
for b in mc sp; do
    git -C "$ORCH" rev-parse --verify -q "refs/heads/$b" >/dev/null && r=exists || r=deleted
    ok "real merged branch $b deleted with its worktree" deleted "$r"
done
[ -d "$WT/sb" ] && r=present || r=gone; ok "real scan-build/y worktree removed" gone "$r"
[ -d "$WT/mc" ] && r=present || r=gone;      ok "real mc worktree removed" gone "$r"
[ -d "$WT/sp ace" ] && r=present || r=gone;  ok "real 'sp ace' worktree removed" gone "$r"
[ -d "$WT/bk" ] && r=present || r=gone;      ok "real bk worktree removed" gone "$r"
for k in fr fo wm md mu um wl run ra tb gf; do
    [ -d "$WT/$k" ] && r=present || r=gone;  ok "real $k worktree kept" present "$r"
done
[ -f "$WT/md/f.txt" ] && grep -q changed "$WT/md/f.txt" && r=present || r=gone; ok "real md edit survives" present "$r"
[ -f "$WT/mu/untracked.txt" ] && r=present || r=gone; ok "real mu untracked file survives" present "$r"
[ -d "$T/ext" ] && r=present || r=gone;      ok "real ext (behind symlink) survives" present "$r"
[ -L "$WT/link" ] && r=present || r=gone;    ok "real symlink survives" present "$r"
[ -e "$GD/../wl/locked" ] || [ -e "$(git -C "$WT/wl" rev-parse --absolute-git-dir)/locked" ] && r=locked || r=unlocked; ok "real wl still locked" locked "$r"
[ -d "$WT/um/target" ] && r=present || r=gone;   ok "real unmerged idle target dropped (tier 1)" gone "$r"
[ -d "$WT/run/target" ] && r=present || r=gone;  ok "real running worktree target kept" present "$r"
[ -d "$T/other/target" ] && r=present || r=gone;    ok "real idle repo target dropped (tier 3)" gone "$r"
[ -f "$T/other/src.txt" ] && r=present || r=gone;     ok "real repo source survives" present "$r"
[ -f "$SC/target/CACHEDIR.TAG" ] && r=present || r=gone;                 ok "real kept scan build target survives (all tiers)" present "$r"
[ -f "$SC/target/release/camerata-server" ] && r=present || r=gone;      ok "real kept scan build release survives" present "$r"
[ -f "$SC/target/debug/incremental/x" ] && r=present || r=gone;          ok "real kept scan build incremental survives" present "$r"
runguard --dry-run | grep -qF "keep camerata-scan-" && r=logged || r=silent; ok "kept scan build is logged" logged "$r"
ok "worktree loop skips a camerata-scan-* worktree" present "$([ -d "$WT/camerata-scan-w" ] && echo present || echo gone)"
runguard --dry-run | grep -qF "would remove merged worktree $WT/camerata-scan-w" && r=listed || r=skipped; ok "dry run never lists a scan worktree" skipped "$r"
[ -d "$T/my-camerata-scan-x/target" ] && r=present || r=gone; ok "my-camerata-scan-x is NOT protected (component match only)" gone "$r"
# resolved roots: relative '.' from inside the kept folder, and a symlink with a trailing slash
(cd "$T/camerata-scan-t2" && CARGO_GUARD_REPOS_ROOT=. CARGO_GUARD_ORCH_REPO="$ORCH" CARGO_GUARD_MERGE_TARGETS=main CARGO_GUARD_ACTIVE_MINUTES=0 CARGO_GUARD_STAMP="$T/stamp" CARGO_GUARD_VOLUME="$T" CARGO_GUARD_TIER1_GIB=100000 CARGO_GUARD_TIER2_GIB=100000 CARGO_GUARD_TIER3_GIB=100000 exec bash "$GUARD" 2>&1) | sed 's/^/  rel: /'
[ -f "$T/camerata-scan-t2/target/release/camerata-server" ] && r=present || r=gone; ok "REPOS_ROOT=. inside the kept folder survives" present "$r"
CARGO_GUARD_REPOS_ROOT="$T/lnk/" CARGO_GUARD_ORCH_REPO="$ORCH" CARGO_GUARD_MERGE_TARGETS=main CARGO_GUARD_ACTIVE_MINUTES=0 CARGO_GUARD_STAMP="$T/stamp" CARGO_GUARD_VOLUME="$T" CARGO_GUARD_TIER1_GIB=100000 CARGO_GUARD_TIER2_GIB=100000 CARGO_GUARD_TIER3_GIB=100000 bash "$GUARD" 2>&1 | sed 's/^/  lnk: /'
[ -f "$T/camerata-scan-t3/target/release/camerata-server" ] && r=present || r=gone; ok "REPOS_ROOT=symlink/ to the kept folder survives" present "$r"
# remove_path backstop: pull the two functions out of the guard and call remove_path directly
mkdir -p "$T/camerata-scan-t4/target/release"; echo bin > "$T/camerata-scan-t4/target/release/b"
mkdir -p "$T/plain/target/release"; echo bin > "$T/plain/target/release/b"
bo="$(DRY_RUN=0; log() { echo "$*"; }; eval "$(awk '/^is_scan_path\(\)/,/^}/; /^remove_path\(\)/,/^}/' "$GUARD")"; remove_path "$T/camerata-scan-t4/target/release"; remove_path "$T/plain/target/release")"
[ -f "$T/camerata-scan-t4/target/release/b" ] && r=present || r=gone; ok "remove_path refuses a scan path" present "$r"
[ -f "$T/plain/target/release/b" ] && r=present || r=gone; ok "remove_path still removes an ordinary target path" gone "$r"
echo "$bo" | grep -q "REFUSED.*kept scan build" && r=logged || r=silent; ok "remove_path logs the refusal" logged "$r"
# branch deletion backstop: plain -d refuses an unmerged branch; protected names are never tried
bd="$(DRY_RUN=0; ORCH_REPO="$ORCH"; log() { echo "$*"; }; eval "$(awk '/^protected_branch\(\)/,/^}/; /^delete_merged_branch\(\)/,/^}/' "$GUARD")"; delete_merged_branch um; delete_merged_branch main)"
git -C "$ORCH" rev-parse --verify -q refs/heads/um >/dev/null && r=exists || r=deleted; ok "branch -d refuses unmerged um" exists "$r"
echo "$bd" | grep -q "branch kept: um (git refused:" && r=logged || r=silent; ok "refusal is logged with git's reason" logged "$r"
echo "$bd" | grep -q "branch kept: main (protected)" && r=logged || r=silent; ok "main is protected" logged "$r"
[ "$fail" = 0 ] && echo "ALL PASS" || echo "FAILED"
exit "$fail"
