#!/usr/bin/env bash
# Fails if any test in a tracked `.rs` file is `#[ignore]`d without being on the checked-in
# allowlist. The standing rule: a test that exposes a bug is never ignored; it stays red until
# the fix lands. An ignore is allowed only for an on-demand harness, and every such harness is
# named on the allowlist with its category.
#
#   scripts/check-test-ignores.sh                      # this repository, default allowlist
#   scripts/check-test-ignores.sh --list FILE          # another allowlist
#   scripts/check-test-ignores.sh --repo DIR           # another repository
#
# Exit status: 0 when every ignored test is listed (stale list entries only warn), 1 when an
# ignored test is not listed or a list entry is malformed, 2 on a usage or environment error.
#
# What counts as an ignore attribute. Only a line whose first non-blank text is an attribute
# (`#[...]`) is read. Leading attributes on that line are skipped one at a time, so
# `#[test] #[ignore] fn f()` is found. An ignore attribute is then one of:
#   #[ignore]                       (anything after it on the line, such as a comment, is fine)
#   #[ignore = "reason"]            (the reason may continue on later lines)
#   #[cfg_attr(..., ignore ...)]    (a conditional ignore is still an ignore, also when
#                                   rustfmt has wrapped the attribute across lines)
# Doc comments and line comments start with `/`, and a string literal on its own line starts
# with a quote, so neither is read. A line inside a multi-line string literal that begins with
# `#[ignore` IS read and reported; that errs toward failing, never toward missing an ignore.
#
# Each ignored test is named `path::fn_name`, from the next `fn <name>` after the attribute.
# An attribute with no `fn` after it in the same file is named `path::?` and always fails.
#
# List format: one entry per line, `path::fn_name  # category: short reason`. Blank lines and
# lines starting with `#` are comments. The category must be one of the allowed ones below.
#
# Portable to macOS (bash 3.2, BSD awk and xargs) and Linux: POSIX awk only, no GNU flags.
set -u

allowed_categories='perf|live network|keychain|real tokens|outside data|baseline re-approval|external tool|wall time (run by the gate)'

usage() {
  echo "usage: check-test-ignores.sh [--repo DIR] [--list FILE]" >&2
  exit 2
}

repo_arg=""
list_arg=""
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) [ $# -ge 2 ] || usage; repo_arg="$2"; shift 2 ;;
    --list) [ $# -ge 2 ] || usage; list_arg="$2"; shift 2 ;;
    -h|--help) usage ;;
    *) usage ;;
  esac
done

if [ -n "$repo_arg" ]; then
  repo="$(git -C "$repo_arg" rev-parse --show-toplevel 2>/dev/null)" || {
    echo "check-test-ignores: not a git repository: $repo_arg" >&2
    exit 2
  }
else
  repo="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "check-test-ignores: run inside a git repository or pass --repo" >&2
    exit 2
  }
fi

if [ -n "$list_arg" ]; then
  # Resolve a relative --list against the caller's directory before changing directory.
  case "$list_arg" in
    /*) list="$list_arg" ;;
    *) list="$(pwd)/$list_arg" ;;
  esac
else
  list="$repo/scripts/test-ignore-allowlist.txt"
fi
if [ ! -f "$list" ]; then
  echo "check-test-ignores: allowlist not found: $list" >&2
  exit 2
fi

cd "$repo" || exit 2

work="$(mktemp -d "${TMPDIR:-/tmp}/check-test-ignores.XXXXXX")" || exit 2
trap 'rm -rf "$work"' EXIT

# Tracked `.rs` files that exist in the working tree, NUL separated. A tracked file deleted
# from the working tree has nothing to scan and would make awk stop with an error.
files="$work/files"
: > "$files"
file_count=0
ls_status=0
git ls-files -z -- '*.rs' > "$work/ls" || ls_status=$?
if [ "$ls_status" -ne 0 ]; then
  echo "check-test-ignores: git ls-files failed (exit $ls_status)" >&2
  exit 2
fi
while IFS= read -r -d '' f; do
  if [ -f "$f" ]; then
    printf '%s\0' "$f" >> "$files"
    file_count=$((file_count + 1))
  fi
done < "$work/ls"

# The scanner. Prints one line per ignore attribute: path::name TAB line TAB reason.
# POSIX awk only (it runs under BSD awk on macOS): no gensub, no three-argument match.
# Both awk programs are read from quoted heredocs with `read`, not `$(cat <<...)`: bash 3.2
# misparses a heredoc inside `$(...)` whose text has an unbalanced quote or parenthesis.
# `read -d ''` returns 1 at the end of input, which is expected here.
scanner=""
IFS= read -r -d '' scanner <<'AWK' || true
# Prints every ignore attribute waiting for its fn, all named after the same fn.
function flush(name,   i) {
  for (i = 1; i <= pn; i++) emit(name, pl[i], pr[i])
  pn = 0
  emit(name, attr_line, reason)
  state = 0; reason = ""
}
function emit(name, lineno, r) {
  gsub(/\t/, " ", r)
  sub(/^[ ]+/, "", r); sub(/[ ]+$/, "", r)
  if (r == "") r = "(no reason given)"
  print file "::" name "\t" lineno "\t" r
}
# Starts a new ignore attribute at line lineno. An ignore already waiting for its fn (two
# ignore attributes on one test) is kept, so both are named after the same fn.
function begin_ignore(lineno) {
  if (state == 2) { pn++; pl[pn] = attr_line; pr[pn] = reason }
  attr_line = lineno; reason = ""
}
# True when the text of a cfg_attr attribute holds an ignore.
function has_ignore(t) {
  return t ~ /[^A-Za-z0-9_]ignore([^A-Za-z0-9_]|$)/
}
# Index just past the `]` that closes the attribute starting at position p of s (s at p is
# `#`), or 0 when it does not close on this line. Skips brackets inside string literals.
function attr_end(s, p,   i, c, depth, instr) {
  depth = 0; instr = 0
  for (i = p; i <= length(s); i++) {
    c = substr(s, i, 1)
    if (instr) {
      if (c == "\\") { i++; continue }
      if (c == "\"") instr = 0
      continue
    }
    if (c == "\"") { instr = 1; continue }
    if (c == "[") depth++
    else if (c == "]") { depth--; if (depth == 0) return i + 1 }
  }
  return 0
}
# Reads string text from s for the reason. Sets in_str to 1 when the string runs past the end
# of the line and returns the rest of the line after the closing quote otherwise.
function read_str(s, continued,   i, c, n, k, ok) {
  if (continued) {
    # A new line inside the string. After a trailing backslash Rust drops the next line's
    # leading whitespace and joins the lines directly. After a plain newline the string keeps
    # the newline; for a one-line display it becomes a single space.
    sub(/^[ \t]+/, "", s)
    if (!bs_cont) reason = reason " "
  }
  bs_cont = 0
  for (i = 1; i <= length(s); i++) {
    c = substr(s, i, 1)
    if (raw_hashes < 0 && c == "\\") {
      if (i == length(s)) { in_str = 1; bs_cont = 1; return "" }
      i++
      c = substr(s, i, 1)
      if (c == "\"" || c == "\\") reason = reason c
      else reason = reason "\\" c
      continue
    }
    if (c == "\"") {
      if (raw_hashes <= 0) { in_str = 0; return substr(s, i + 1) }
      ok = 1
      for (k = 1; k <= raw_hashes; k++) if (substr(s, i + k, 1) != "#") ok = 0
      if (ok) { in_str = 0; return substr(s, i + raw_hashes + 1) }
    }
    reason = reason c
  }
  in_str = 1
  return ""
}
# Looks for `fn <name>` in s. Returns the name, or "" when there is none.
function find_fn(s,   t) {
  if (s ~ /^[ \t]*\/\//) return ""
  if (!match(s, /(^|[^A-Za-z0-9_])fn[ \t]+[A-Za-z_][A-Za-z0-9_]*/)) return ""
  t = substr(s, RSTART, RLENGTH)
  sub(/^[^f]*fn[ \t]+/, "", t)
  return t
}
# Starts on an attribute line. Returns 1 when an ignore attribute was found, or when a
# wrapped cfg_attr began and the following lines must be gathered before deciding.
function try_attr(s,   p, e, rest, head) {
  while (1) {
    if (!match(s, /^[ \t]*#[ \t]*\[/)) return 0
    p = RSTART + RLENGTH - 1
    while (substr(s, RSTART, 1) != "#") RSTART++
    head = substr(s, p + 1)
    if (head ~ /^[ \t]*ignore[ \t]*\]/) {
      begin_ignore(FNR)
      sub(/^[ \t]*ignore[ \t]*\]/, "", head)
      seek(head)
      return 1
    }
    if (head ~ /^[ \t]*ignore[ \t]*=/) {
      begin_ignore(FNR)
      sub(/^[ \t]*ignore[ \t]*=[ \t]*/, "", head)
      raw_hashes = -1
      if (head ~ /^r#*"/) {
        raw_hashes = 0
        head = substr(head, 2)
        while (substr(head, 1, 1) == "#") { raw_hashes++; head = substr(head, 2) }
      }
      if (substr(head, 1, 1) != "\"") {
        # Not a string literal: report the raw text so it is visible.
        reason = head
        seek("")
        return 1
      }
      rest = read_str(substr(head, 2), 0)
      if (in_str) { state = 1; return 1 }
      seek(rest)
      return 1
    }
    if (head ~ /^[ \t]*cfg_attr[ \t]*\(/) {
      e = attr_end(s, RSTART)
      if (!e) {
        # Wrapped across lines (rustfmt does this to a long cfg_attr): gather it first.
        acc = substr(s, RSTART); acc_line = FNR; resume_state = state; state = 3
        return 1
      }
      if (has_ignore(substr(s, RSTART, e - RSTART))) {
        begin_ignore(FNR)
        reason = "conditional: " substr(s, RSTART, e - RSTART)
        seek(substr(s, e))
        return 1
      }
    }
    e = attr_end(s, RSTART)
    if (!e) return 0
    s = substr(s, e)
  }
}
# After the attribute: names the test from s if it holds the fn, else keeps looking.
function seek(s,   n) {
  n = find_fn(s)
  if (n != "") { flush(n); return }
  state = 2
}
FNR == 1 {
  if (state != 0) flush("?")
  file = FILENAME; state = 0; reason = ""; in_str = 0; pn = 0
}
{
  line = $0
  sub(/\r$/, "", line)
  if (state == 1) {
    rest = read_str(line, 1)
    if (!in_str) seek(rest)
    next
  }
  if (state == 3) {
    # Inside a wrapped cfg_attr. Joined with single spaces for display.
    t = line
    sub(/^[ \t]+/, "", t)
    acc = acc " " t
    e = attr_end(acc, 1)
    if (!e) next
    state = resume_state
    if (has_ignore(substr(acc, 1, e - 1))) {
      begin_ignore(acc_line)
      reason = "conditional: " substr(acc, 1, e - 1)
      seek(substr(acc, e))
    }
    next
  }
  if (state == 2) {
    # Still looking for the fn. Another ignore attribute on the way is kept with this one.
    if (line ~ /^[ \t]*#/ && try_attr(line)) next
    n = find_fn(line)
    if (n != "") flush(n)
    next
  }
  if (line ~ /^[ \t]*#/) try_attr(line)
}
END { if (state != 0) flush("?") }
AWK

found="$work/found"
: > "$found"
if [ "$file_count" -gt 0 ]; then
  scan_status=0
  xargs -0 awk "$scanner" < "$files" > "$found" || scan_status=$?
  if [ "$scan_status" -ne 0 ]; then
    echo "check-test-ignores: scanning the .rs files failed (exit $scan_status)" >&2
    exit 2
  fi
fi

# The comparison. Reads the allowlist and then the scan, and prints the report. Exit status 1
# when any ignored test is unlisted or any list entry is malformed.
list_label="${list#"$repo"/}"
comparer=""
IFS= read -r -d '' comparer <<'AWK' || true
BEGIN {
  FS = "\t"
  ncat = split(cats, catlist, "|")
  for (i = 1; i <= ncat; i++) allowed[catlist[i]] = 1
  bad = 0; nentries = 0
}
# The allowlist is the first file. `NR == FNR` would also match the scan when the list is
# empty, so the file is told apart by name.
FILENAME == ARGV[1] {
  raw = $0
  sub(/\r$/, "", raw)
  if (raw ~ /^[ \t]*$/ || raw ~ /^[ \t]*#/) next
  entry = raw; note = ""
  h = index(raw, "#")
  if (h) { entry = substr(raw, 1, h - 1); note = substr(raw, h + 1) }
  gsub(/^[ \t]+|[ \t]+$/, "", entry)
  gsub(/^[ \t]+|[ \t]+$/, "", note)
  if (entry !~ /^[^ \t]+::[A-Za-z_][A-Za-z0-9_]*$/) {
    bad_lines[++bad] = "line " FNR ": not of the form path::fn_name: " raw
    next
  }
  c = index(note, ":")
  cat = c ? substr(note, 1, c - 1) : note
  gsub(/^[ \t]+|[ \t]+$/, "", cat)
  why = c ? substr(note, c + 1) : ""
  gsub(/^[ \t]+|[ \t]+$/, "", why)
  if (note == "" || !(cat in allowed) || why == "") {
    if (note == "") problem = "no category"
    else if (!(cat in allowed)) problem = "category \"" cat "\" is not allowed"
    else problem = "no reason after the category"
    bad_lines[++bad] = "line " FNR ": " entry ": " problem
    next
  }
  if (entry in listed) {
    dup[entry] = 1
    next
  }
  listed[entry] = cat
  order[++nentries] = entry
  next
}
{
  nfound++
  key = $1
  seen[key] = 1
  if (key in listed) { nlisted++; next }
  unl_key[++nunl] = key; unl_line[nunl] = $2; unl_reason[nunl] = $3
}
END {
  printf "check-test-ignores: %d tracked .rs files, %d ignored tests, %d on %s\n", \
    files, nfound, nlisted, list_label
  status = 0
  if (nunl > 0) {
    status = 1
    printf "\nUNLISTED: %d ignored test(s) are not on %s.\n", nunl, list_label
    print "A test that exposes a bug is never ignored; un-ignore it and leave it red until the"
    print "fix lands. An on-demand harness goes on the list with its category."
    for (i = 1; i <= nunl; i++) {
      k = unl_key[i]
      printf "  %s (line %s)\n", k, unl_line[i]
      if (k ~ /::\?$/) print "      no fn follows this ignore attribute in the file"
      printf "      reason: %s\n", unl_reason[i]
    }
  }
  if (bad > 0) {
    status = 1
    printf "\nBAD ENTRY: %d line(s) of %s are malformed.\n", bad, list_label
    print "Allowed categories: perf, live network, keychain, real tokens, outside data,"
    print "baseline re-approval, external tool, wall time (run by the gate)."
    for (i = 1; i <= bad; i++) printf "  %s\n", bad_lines[i]
  }
  nstale = 0
  for (i = 1; i <= nentries; i++) if (!(order[i] in seen)) stale[++nstale] = order[i]
  if (nstale > 0) {
    printf "\nWARNING: %d list entr%s no longer match an ignored test; remove them:\n", \
      nstale, (nstale == 1 ? "y" : "ies")
    for (i = 1; i <= nstale; i++) printf "  %s\n", stale[i]
  }
  for (k in dup) printf "\nWARNING: %s is listed more than once\n", k
  if (status == 0) print "check-test-ignores: OK"
  exit status
}
AWK
compare_status=0
awk -v cats="$allowed_categories" -v list_label="$list_label" -v files="$file_count" \
  "$comparer" "$list" "$found" || compare_status=$?

exit "$compare_status"
