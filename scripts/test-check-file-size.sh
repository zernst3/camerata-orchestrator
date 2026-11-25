#!/bin/bash
# test-check-file-size: proves scripts/check-file-size.sh catches what it exists to catch.
#
# Every case runs against a throwaway copy in a temp folder: the tracked .rs files under
# crates/ and tools/, plus the exceptions list, in a fresh git repo. Real sources are never
# edited. Each case starts from a clean reset of that copy.
#
# Usage: scripts/test-check-file-size.sh     (exit 0 when every case behaves as expected)

set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(git -C "$here" rev-parse --show-toplevel)"
gate="$here/check-file-size.sh"
LIMIT=3000

tmp="$(mktemp -d "${TMPDIR:-/tmp}/test-check-file-size.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
copy="$tmp/copy"
mkdir -p "$copy/scripts"

# The copy holds exactly what the gate reads, taken from the working tree (so an uncommitted
# change to the exceptions list is what gets tested).
(cd "$repo" && git ls-files -z -- 'crates/*.rs' 'tools/*.rs' | tar --null -T - -cf -) \
    | tar -xf - -C "$copy"
cp "$repo/scripts/file-size-exceptions.txt" "$copy/scripts/"
git -C "$copy" init -q
git -C "$copy" add -A
git -C "$copy" -c user.name=test -c user.email=test@example.invalid commit -q -m baseline

reset_copy() {
    git -C "$copy" reset -q --hard
    git -C "$copy" clean -q -fdx
}

# lines N: N numbered lines of Rust-looking text.
lines() {
    awk -v n="$1" 'BEGIN { for (i = 1; i <= n; i++) print "// line " i }'
}

# The first and last listed file: the largest one and the one closest to the limit.
first_listed="$(awk '!/^#/ && NF { print $2; exit }' "$copy/scripts/file-size-exceptions.txt")"
last_listed="$(awk '!/^#/ && NF { p = $2 } END { print p }' "$copy/scripts/file-size-exceptions.txt")"
[ -n "$first_listed" ] || { echo "the exceptions list is empty; nothing to test against" >&2; exit 1; }

passed=0
failed=0

# expect NAME WANT_EXIT WANT_TEXT: run the gate on the copy, check its exit status and that its
# output contains WANT_TEXT (empty WANT_TEXT: no text check).
expect() {
    local name="$1" want_exit="$2" want_text="$3" out status
    set +e
    out="$("$gate" --root "$copy" 2>&1)"
    status=$?
    set -e
    if [ "$status" -ne "$want_exit" ]; then
        echo "not ok - $name: exit $status, wanted $want_exit"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=$((failed + 1))
    elif [ -n "$want_text" ] && ! printf '%s\n' "$out" | grep -q -F -- "$want_text"; then
        echo "not ok - $name: output lacks: $want_text"
        printf '%s\n' "$out" | sed 's/^/    /'
        failed=$((failed + 1))
    else
        echo "ok - $name"
        passed=$((passed + 1))
    fi
    reset_copy
}

# 1. The tree as it is passes.
expect "the current tree passes" 0 "check-file-size: OK"

# 2. A new file one line over the limit fails, and is named.
mkdir -p "$copy/crates/newcrate/src"
lines $((LIMIT + 1)) >"$copy/crates/newcrate/src/big.rs"
git -C "$copy" add -A
expect "a new file over the limit fails" 1 "crates/newcrate/src/big.rs has $((LIMIT + 1)) lines"

# 3. The same under tools/.
mkdir -p "$copy/tools/newtool/src"
lines $((LIMIT + 1)) >"$copy/tools/newtool/src/big.rs"
git -C "$copy" add -A
expect "a new file over the limit under tools/ fails" 1 "tools/newtool/src/big.rs has"

# 4. Exactly at the limit is allowed.
mkdir -p "$copy/crates/newcrate/src"
lines "$LIMIT" >"$copy/crates/newcrate/src/edge.rs"
git -C "$copy" add -A
expect "a new file exactly at the limit passes" 0 "check-file-size: OK"

# 5. A last line without a trailing newline still counts.
mkdir -p "$copy/crates/newcrate/src"
{ lines "$LIMIT"; printf '// no newline'; } >"$copy/crates/newcrate/src/nonl.rs"
git -C "$copy" add -A
expect "a final line without a newline is counted" 1 "nonl.rs has $((LIMIT + 1)) lines"

# 6. A listed file that grows by one line fails, and is named.
echo "// one more line" >>"$copy/$first_listed"
expect "a listed file that grows fails" 1 "$first_listed grew to"

# 7. A listed file that shrinks but stays over the limit passes, with a note.
sed -i.bak '$d' "$copy/$first_listed" && rm -f "$copy/$first_listed.bak"
expect "a listed file that shrinks passes" 0 "note: $first_listed shrank"

# 8. A listed file that drops to the limit must leave the list.
lines "$LIMIT" >"$copy/$last_listed"
expect "a listed file back within the limit must be unlisted" 1 \
    "$last_listed is down to $LIMIT lines"

# 9. A listed file that was deleted must leave the list.
git -C "$copy" rm -q "$last_listed"
expect "a listed file that no longer exists must be unlisted" 1 "$last_listed is listed"

# 10. An untracked oversized file is not checked (build output, scratch files).
mkdir -p "$copy/crates/newcrate/src"
lines $((LIMIT + 500)) >"$copy/crates/newcrate/src/untracked.rs"
expect "an untracked file is ignored" 0 "check-file-size: OK"

# 11. A malformed exceptions line is an error, not a silent skip.
echo "lots crates/x.rs" >>"$copy/scripts/file-size-exceptions.txt"
expect "a malformed exceptions line is an error" 2 "expected '<ceiling> <path>'"

# 12. A duplicate entry is an error.
grep -v '^#' "$copy/scripts/file-size-exceptions.txt" | head -1 \
    >>"$copy/scripts/file-size-exceptions.txt"
expect "a duplicate exceptions entry is an error" 2 "is listed twice"

# 13. The resets work: after every case above, the copy is back at its baseline.
expect "the copy is back to the baseline after every case" 0 "check-file-size: OK"

echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
