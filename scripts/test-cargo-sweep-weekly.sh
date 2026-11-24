#!/bin/bash
# Shell test for scripts/cargo-sweep-weekly.sh with a fake cargo-sweep on PATH that records its
# arguments (the real one never runs). The fake emulates --recursive by recording every project
# dir under the given root, so the OLD unguarded command form can be shown red against it.
# Usage: test-cargo-sweep-weekly.sh [command-to-test]   (default: the wrapper next to this file)
set -u
SUT="${1:-$(cd "$(dirname "$0")" && pwd)/cargo-sweep-weekly.sh}"
T="$(mktemp -d)"; T="$(cd -P "$T" && pwd)"
mkdir -p "$T/root/.claude/worktrees/x/target" "$T/root/.hidden/proj/target"
for d in .claude/worktrees/x .hidden/proj; do echo "Signature: 8a477f597d28d172789f06886806bc55" > "$T/root/$d/target/CACHEDIR.TAG"; done
mkdir -p "$T/bin" "$T/root/camerata-scan-t1/target/release" "$T/root/ordinary/target" "$T/root/my-camerata-scan-x/target"
for d in camerata-scan-t1 ordinary my-camerata-scan-x; do echo "Signature: 8a477f597d28d172789f06886806bc55" > "$T/root/$d/target/CACHEDIR.TAG"; done
cat > "$T/bin/cargo-sweep" <<F
#!/bin/bash
# fake: record each swept project dir; with --recursive, every project dir under the last argument
rec=0; for a in "\$@"; do [ "\$a" = "--recursive" ] && rec=1; done
last="\${@: -1}"
if [ "\$rec" = 1 ]; then
    find "\$last" -type f -name CACHEDIR.TAG -path '*/target/CACHEDIR.TAG' | sed 's|/target/CACHEDIR.TAG\$||' >> "$T/calls"
else
    echo "\$last" >> "$T/calls"
fi
F
chmod +x "$T/bin/cargo-sweep"
fail=0
ok() { if [ "$2" = "$3" ]; then echo "PASS $1 ($3)"; else echo "FAIL $1 expected=$2 got=$3"; fail=1; fi; }
run() { : > "$T/calls"; env PATH="$T/bin:$PATH" CARGO_SWEEP_REPOS_ROOT="$1" bash "$SUT" "${@:2}" 2>&1; }
swept() { grep -qxF "$1" "$T/calls" && echo swept || echo not; }

run "$T/root" > "$T/out"
ok "scan build never passed to cargo-sweep" not "$(swept "$T/root/camerata-scan-t1")"
ok "ordinary repo swept" swept "$(swept "$T/root/ordinary")"
ok "my-camerata-scan-x is not protected" swept "$(swept "$T/root/my-camerata-scan-x")"
grep -qF "keep camerata-scan-t1/target (kept scan build, never swept)" "$T/out" && r=logged || r=silent; ok "kept scan build logged" logged "$r"

ok "dot-folder .claude/worktrees target not swept" not "$(swept "$T/root/.claude/worktrees/x")"
ok "dot-folder .hidden target not swept" not "$(swept "$T/root/.hidden/proj")"

run "$T/root" --dry-run > "$T/out"
[ -s "$T/calls" ] && r=ran || r=none; ok "dry run calls nothing" none "$r"
grep -qF "would sweep $T/root/ordinary" "$T/out" && r=listed || r=missing; ok "dry run lists ordinary repo" listed "$r"

ln -s "$T/root/camerata-scan-t1" "$T/lnk"
run "$T/lnk/" > /dev/null
ok "symlink root with trailing slash: scan build not swept" not "$(swept "$T/root/camerata-scan-t1")"
(cd "$T/root/camerata-scan-t1" && run . > /dev/null)
ok "relative root inside the kept folder: not swept" not "$(swept "$T/root/camerata-scan-t1")"
(cd "$T" && run root > /dev/null)
ok "relative root: ordinary repo swept" swept "$(swept "$T/root/ordinary")"

: > "$T/calls"; env PATH="$T/bin:$PATH" CARGO_SWEEP_REPOS_ROOT="$T/root" CARGO_SWEEP_DAYS=3 bash "$SUT" >/dev/null 2>&1
[ -s "$T/calls" ] && r=ran || r=none; ok "custom days still sweeps" ran "$r"
[ "$fail" = 0 ] && echo "ALL PASS" || echo "FAILED"
exit "$fail"
