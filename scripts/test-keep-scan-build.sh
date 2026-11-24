#!/bin/bash
# Shell test for keep-scan-build.sh, run in a throwaway repo under a temp folder. Leaves the temp folder.
set -eu
here="$(cd "$(dirname "$0")" && pwd)"
base="$(cd "$(mktemp -d "${TMPDIR:-/tmp}/keep-scan-test.XXXXXX")" && pwd -P)"
repo="$base/main"; mkdir "$repo"; cd "$repo"
git init -q; git config user.email t@example.test; git config user.name t
echo a > f; git add f; git commit -q -m one
S="$here/keep-scan-build.sh"
fail() { echo "FAIL: $1" >&2; exit 1; }

# dry run changes nothing
"$S" t1 --dry-run | grep -q "Nothing changed" || fail "dry run text"
[ -z "$(git tag)" ] || fail "dry run created a tag"
[ ! -e "$base/camerata-scan-t1" ] || fail "dry run created a folder"

# dirty tree refused (modified, then untracked)
echo b >> f
if "$S" t1 --no-build 2>/dev/null; then fail "dirty tree accepted"; fi
git checkout -q f; echo x > untracked
if "$S" t1 --no-build 2>/dev/null; then fail "untracked file accepted"; fi
mv untracked "$base/untracked.keep"

# files under .claude/ never refuse (the main checkout always carries a local settings change)
mkdir .claude; echo '{}' > .claude/settings.json; git add .claude/settings.json; git commit -q -m settings
echo '{"local":1}' > .claude/settings.json
"$S" t1 --dry-run 2>&1 | grep -q "Nothing changed" || fail "modified tracked file under .claude/ refused"
echo y > .claude/scratch.txt
"$S" t1 --dry-run 2>&1 | grep -q "Nothing changed" || fail "untracked file under .claude/ refused"
# ... but a change elsewhere still refuses, even alongside the .claude/ ones
echo b >> f
if "$S" t1 --dry-run 2>/dev/null; then fail "modified file elsewhere accepted next to .claude/ changes"; fi
git checkout -q f
git checkout -q .claude/settings.json; rm .claude/scratch.txt

# real run: local tag plus worktree at the tag, no remote needed
"$S" t1 --no-build >/dev/null
[ "$(git rev-parse scan-build/t1)" = "$(git rev-parse HEAD)" ] || fail "tag not on HEAD"
[ "$(git -C "$base/camerata-scan-t1" rev-parse HEAD)" = "$(git rev-parse HEAD)" ] || fail "worktree not on commit"

# second run with the same name refuses, nothing overwritten
if "$S" t1 --no-build 2>/dev/null; then fail "duplicate accepted"; fi
# from inside the kept worktree the folder is still a sibling of the main checkout
cd "$base/camerata-scan-t1"; "$S" t2 --dry-run | grep -q "Worktree: $base/camerata-scan-t2" || fail "sibling path"
# the script contains no push
if grep -qE 'git[^#]*\bpush\b' "$S"; then fail "script mentions push in a command"; fi
echo "keep-scan-build tests passed in $base"
