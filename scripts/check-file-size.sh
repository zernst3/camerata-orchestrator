#!/bin/bash
# check-file-size: no hand-maintained Rust file grows past a size a human can review.
#
# Why this exists (GitHub issue #278): several source files are far past 5,000 lines, and every
# agent and every reviewer that edits one pays for the whole file. Splitting them is separate
# work; this gate stops the problem getting worse in the meantime.
#
# The rule:
#   - Every tracked `.rs` file under `crates/` or `tools/` has at most LIMIT (3,000) lines.
#   - A file listed in `scripts/file-size-exceptions.txt` may exceed LIMIT, but never its own
#     recorded ceiling. The ceiling is the file's line count when it was listed, so a listed file
#     may shrink but never grow.
#   - A listed file that is back at or under LIMIT must be removed from the list, and so must an
#     entry whose file no longer exists. The gate fails until it is, so the list only ever
#     shrinks.
#
# Lines are counted as a text editor shows them (a last line without a trailing newline counts).
# Only files tracked by git are checked; build output and untracked scratch files are ignored.
#
# Usage:
#   scripts/check-file-size.sh                 check the repository this script lives in
#   scripts/check-file-size.sh --root DIR      check another checkout (the self-test uses this)
#   scripts/check-file-size.sh --generate      print an exceptions list for the current tree
#                                              (bootstrap only: raising a ceiling is a reviewed
#                                              edit to the list, never a regeneration)
#
# Exit status: 0 clean, 1 a size rule is broken, 2 usage error or a malformed exceptions list.

set -euo pipefail

LIMIT=3000
EXCEPTIONS_REL="scripts/file-size-exceptions.txt"

usage() {
    sed -n '2,/^# Exit status/p' "$0" | sed 's/^# \{0,1\}//'
}

root=""
generate=0
while [ $# -gt 0 ]; do
    case "$1" in
        --root)
            [ $# -ge 2 ] || { echo "check-file-size: --root needs a directory" >&2; exit 2; }
            root="$2"
            shift 2
            ;;
        --generate) generate=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "check-file-size: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$root" ]; then
    root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
fi
if ! git -C "$root" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    echo "check-file-size: $root is not a git checkout" >&2
    exit 2
fi
root="$(cd "$root" && pwd)"
exceptions="$root/$EXCEPTIONS_REL"

work="$(mktemp -d "${TMPDIR:-/tmp}/check-file-size.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# "<lines> <path>" for every tracked .rs file under crates/ and tools/, largest first.
# Git pathspec `*` matches across `/`, so `crates/*.rs` is every .rs file at any depth.
# awk counts records, so a final line without a newline is counted (wc -l would miss it).
# Portable awk only (no gawk ENDFILE): this also runs on macOS.
(
    cd "$root"
    git ls-files -z -- 'crates/*.rs' 'tools/*.rs' \
        | xargs -0 awk '{ n[FILENAME]++ } END { for (f in n) print n[f], f }'
) | sort -k1,1nr -k2,2 >"$work/sizes"

if [ "$generate" -eq 1 ]; then
    echo "# Rust files allowed to exceed the $LIMIT-line limit of scripts/check-file-size.sh."
    echo "# Format: <ceiling> <path>. The ceiling is the file's line count when it was listed;"
    echo "# the file may shrink but never grow past it. Remove an entry once its file is at or"
    echo "# under $LIMIT lines. Sorted largest first. Never raise a ceiling to make the gate pass:"
    echo "# split the file instead (GitHub issue #278)."
    awk -v limit="$LIMIT" '$1 > limit' "$work/sizes"
    exit 0
fi

# Parse the exceptions list into "<ceiling> <path>" lines, rejecting anything malformed: a gate
# that silently skips a line it could not read would let that file grow unchecked.
: >"$work/ceilings"
malformed=0
if [ -f "$exceptions" ]; then
    lineno=0
    while IFS= read -r line || [ -n "$line" ]; do
        lineno=$((lineno + 1))
        line="${line%$'\r'}"
        case "$line" in ''|'#'*) continue ;; esac
        if ! [[ "$line" =~ ^([0-9]+)[[:space:]]+([^[:space:]]+)[[:space:]]*$ ]]; then
            echo "$EXCEPTIONS_REL:$lineno: expected '<ceiling> <path>', got: $line" >&2
            malformed=1
            continue
        fi
        ceiling="${BASH_REMATCH[1]}"
        path="${BASH_REMATCH[2]}"
        case "$path" in
            crates/*.rs|tools/*.rs) ;;
            *)
                echo "$EXCEPTIONS_REL:$lineno: $path is not a .rs file under crates/ or tools/" >&2
                malformed=1
                continue
                ;;
        esac
        if grep -q -F -x -- "$path" "$work/listed" 2>/dev/null; then
            echo "$EXCEPTIONS_REL:$lineno: $path is listed twice" >&2
            malformed=1
            continue
        fi
        echo "$path" >>"$work/listed"
        echo "$ceiling $path" >>"$work/ceilings"
    done <"$exceptions"
fi
if [ "$malformed" -ne 0 ]; then
    echo "check-file-size: fix $EXCEPTIONS_REL first" >&2
    exit 2
fi

failures=0
fail() {
    echo "FAIL: $*"
    failures=$((failures + 1))
}

# Every tracked file over the limit must be listed, and at or under its ceiling.
while read -r lines path; do
    [ "$lines" -gt "$LIMIT" ] || continue
    ceiling="$(awk -v p="$path" '$2 == p { print $1; exit }' "$work/ceilings")"
    if [ -z "$ceiling" ]; then
        fail "$path has $lines lines, over the $LIMIT-line limit. Split it; do not add it to" \
            "$EXCEPTIONS_REL."
    elif [ "$lines" -gt "$ceiling" ]; then
        fail "$path grew to $lines lines, past its recorded ceiling of $ceiling" \
            "(+$((lines - ceiling))). A listed file may shrink but never grow: move the new code" \
            "into another file."
    elif [ "$lines" -lt "$ceiling" ]; then
        echo "note: $path shrank to $lines lines (ceiling $ceiling); lower its ceiling in" \
            "$EXCEPTIONS_REL to lock the gain in."
    fi
done <"$work/sizes"

# Every listed file must still exist and still need its exception.
while read -r ceiling path; do
    lines="$(awk -v p="$path" '$2 == p { print $1; exit }' "$work/sizes")"
    if [ -z "$lines" ]; then
        fail "$path is listed in $EXCEPTIONS_REL but is not a tracked file any more. Remove" \
            "its entry."
    elif [ "$lines" -le "$LIMIT" ]; then
        fail "$path is down to $lines lines, within the $LIMIT-line limit. Remove it from" \
            "$EXCEPTIONS_REL so it stays there."
    elif [ "$ceiling" -le "$LIMIT" ]; then
        fail "$path has a ceiling of $ceiling in $EXCEPTIONS_REL, which is not above the" \
            "$LIMIT-line limit. Remove the entry."
    fi
done <"$work/ceilings"

checked="$(wc -l <"$work/sizes" | tr -d ' ')"
listed="$(wc -l <"$work/ceilings" | tr -d ' ')"
if [ "$failures" -ne 0 ]; then
    echo "check-file-size: $failures problem(s) in $checked files ($listed listed exceptions)."
    exit 1
fi
echo "check-file-size: OK, $checked files checked, $listed listed exceptions, limit $LIMIT lines."
