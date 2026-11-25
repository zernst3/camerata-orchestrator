#!/usr/bin/env bash
# Shell test for check-test-ignores.sh. Each case builds a throwaway git repository under a
# temp folder, writes a few `.rs` files and an allowlist, runs the check and asserts on its
# exit status and output. Prints one PASS or FAIL line per assertion and exits non-zero if
# any assertion failed. Removes the temp folder on exit.
#
#   scripts/test-check-test-ignores.sh
#
# Runs on macOS (bash 3.2, BSD tools) and Linux.
set -u

here="$(cd "$(dirname "$0")" && pwd)"
check="$here/check-test-ignores.sh"
base="$(mktemp -d "${TMPDIR:-/tmp}/check-test-ignores-test.XXXXXX")"
trap 'rm -rf "$base"' EXIT

failures=0
passes=0
pass() { passes=$((passes + 1)); echo "PASS: $1"; }
fail() { failures=$((failures + 1)); echo "FAIL: $1"; }

# new_repo NAME: creates an empty git repository at $base/NAME and sets $repo to it.
new_repo() {
  repo="$base/$1"
  mkdir -p "$repo"
  git -C "$repo" init -q
  git -C "$repo" config user.email t@example.test
  git -C "$repo" config user.name t
}

# commit_all: tracks and commits every file in $repo.
commit_all() {
  git -C "$repo" add -A
  git -C "$repo" commit -q -m fixture
}

# run_check: runs the check on $repo with $repo/allow.txt, setting $out and $status.
run_check() {
  if [ ! -x "$check" ]; then
    out="check-test-ignores.sh is missing or not executable at $check"
    status=127
    return
  fi
  out="$("$check" --repo "$repo" --list "$repo/allow.txt" 2>&1)"
  status=$?
}

# expect_status NAME WANT: WANT is "zero" or "nonzero".
expect_status() {
  if [ "$2" = zero ] && [ "$status" -eq 0 ]; then
    pass "$1 (exit $status)"
  elif [ "$2" = nonzero ] && [ "$status" -ne 0 ] && [ "$status" -ne 127 ]; then
    pass "$1 (exit $status)"
  else
    fail "$1 (wanted $2 exit, got $status). Output:"
    printf '%s\n' "$out" | sed 's/^/    | /'
  fi
}

# expect_output NAME TEXT: the output contains TEXT as a fixed string.
expect_output() {
  if printf '%s\n' "$out" | grep -F -q -- "$2"; then
    pass "$1"
  else
    fail "$1 (output lacks: $2). Output:"
    printf '%s\n' "$out" | sed 's/^/    | /'
  fi
}

# expect_no_output NAME TEXT: the output does not contain TEXT as a fixed string.
expect_no_output() {
  if printf '%s\n' "$out" | grep -F -q -- "$2"; then
    fail "$1 (output unexpectedly has: $2). Output:"
    printf '%s\n' "$out" | sed 's/^/    | /'
  else
    pass "$1"
  fi
}

# --- Case 1: an unlisted ignore fails, naming the test and its reason. ---
new_repo unlisted
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'EOF'
#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "bug: returns the wrong total"]
    fn totals_add_up() {
        assert_eq!(1 + 1, 2);
    }
}
EOF
printf '# no entries\n' > "$repo/allow.txt"
commit_all
run_check
expect_status "unlisted ignore fails" nonzero
expect_output "unlisted ignore is named as path::fn" "src/lib.rs::totals_add_up"
expect_output "unlisted ignore prints its reason" "bug: returns the wrong total"

# --- Case 2: a listed ignore passes. ---
new_repo listed
mkdir -p "$repo/tests"
cat > "$repo/tests/perf.rs" <<'EOF'
#[test]
#[ignore = "perf harness"]
fn measures_throughput() {}
EOF
cat > "$repo/allow.txt" <<'EOF'
# Allowlist for the test.

tests/perf.rs::measures_throughput  # perf: throughput harness
EOF
commit_all
run_check
expect_status "listed ignore passes" zero
expect_no_output "listed ignore is not reported as unlisted" "UNLISTED"

# --- Case 3: an ignore with a multi-line reason is found and named. ---
new_repo multiline
mkdir -p "$repo/src"
cat > "$repo/src/keys.rs" <<'EOF'
#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "touches the real OS keychain and may wait on a permission dialog; run with \
                --ignored in an attended session"]
    fn production_store_round_trips() {
        let _ = 1;
    }

    #[tokio::test]
    #[ignore = "first line of a long reason, \
                a \"quoted\" middle line, \
                and the last line"]
    async fn async_reason_spans_three_lines() {}
}
EOF
printf '' > "$repo/allow.txt"
commit_all
run_check
expect_status "multi-line ignores are unlisted so the check fails" nonzero
expect_output "multi-line ignores are reported as unlisted" "UNLISTED: 2 ignored test(s)"
expect_no_output "an empty allowlist has no malformed entries" "BAD ENTRY"
expect_output "multi-line ignore is named after the next fn" "src/keys.rs::production_store_round_trips"
expect_output "multi-line reason is joined across lines" "may wait on a permission dialog; run with --ignored in an attended session"
expect_output "async fn after a three-line reason is named" "src/keys.rs::async_reason_spans_three_lines"
expect_output "escaped quotes in a reason are kept" 'a "quoted" middle line'
expect_no_output "a reason line is never taken as the test name" "src/keys.rs::with"
# Listing both makes the same repository pass.
cat > "$repo/allow.txt" <<'EOF'
src/keys.rs::production_store_round_trips  # keychain: real OS keychain
src/keys.rs::async_reason_spans_three_lines  # live network: test fixture
EOF
run_check
expect_status "listing the multi-line ignores passes" zero

# --- Case 4: doc comments and line comments that mention #[ignore] are not counted. ---
new_repo comments
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'EOF'
//! This module's slow test is `#[ignore]`d.
//! #[ignore = "a module doc line that looks like an attribute"]

/// Marked `#[ignore]` because it is slow.
/// #[ignore]
#[test]
fn documented_but_not_ignored() {}

// #[ignore]
// #[ignore = "commented out"]
#[test]
fn commented_out_ignore() {}

/* #[ignore] */
#[test]
fn block_comment_mention() {}
EOF
printf '' > "$repo/allow.txt"
commit_all
run_check
expect_status "comment mentions are not counted" zero
expect_no_output "doc-commented test is not reported" "documented_but_not_ignored"
expect_no_output "line-commented test is not reported" "commented_out_ignore"
expect_no_output "block-comment mention is not reported" "block_comment_mention"

# --- Case 5: a string literal containing #[ignore] is not counted. ---
new_repo strings
mkdir -p "$repo/src"
cat > "$repo/src/fixture.rs" <<'EOF'
#[test]
fn checker_skips_ignored_tests() {
    let src = concat!(
        "#[test]\n#[ignore]",
        "#[ignore = \"inside a string\"]",
    );
    let other = "#[ignore]";
    assert!(!src.is_empty() && !other.is_empty());
}
EOF
printf '' > "$repo/allow.txt"
commit_all
run_check
expect_status "string literal mentions are not counted" zero
expect_no_output "string-literal test is not reported" "checker_skips_ignored_tests"

# --- Case 6: a stale list entry warns but passes. ---
new_repo stale
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'EOF'
#[test]
#[ignore]
fn still_here() {}
EOF
cat > "$repo/allow.txt" <<'EOF'
src/lib.rs::still_here  # external tool: needs a binary
src/lib.rs::long_gone  # perf: removed harness
EOF
commit_all
run_check
expect_status "stale entry alone passes" zero
expect_output "stale entry is printed as a warning" "src/lib.rs::long_gone"
expect_output "stale entry output says WARNING" "WARNING"
expect_no_output "a bare #[ignore] that is listed is not reported" "UNLISTED"

# --- Case 7: a bare #[ignore] is found and reported with no reason. ---
new_repo bare
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'EOF'
#[tokio::test]
#[serial_test::serial(env)]
#[ignore]
async fn spawns_a_real_server() {}

#[test] #[ignore] fn same_line_ignore() {}

#[test]
#[ignore] // flaky on slow machines
fn trailing_comment_ignore() {}
EOF
printf '' > "$repo/allow.txt"
commit_all
run_check
expect_status "bare ignores fail when unlisted" nonzero
expect_output "bare ignore is named" "src/lib.rs::spawns_a_real_server"
expect_output "bare ignore says it has no reason" "(no reason given)"
expect_output "ignore after another attribute on one line is found" "src/lib.rs::same_line_ignore"
expect_output "ignore with a trailing comment is found" "src/lib.rs::trailing_comment_ignore"

# --- Case 8: untracked files are not read. ---
new_repo untracked
mkdir -p "$repo/src"
printf '#[test]\nfn fine() {}\n' > "$repo/src/lib.rs"
printf '' > "$repo/allow.txt"
commit_all
printf '#[test]\n#[ignore]\nfn scratch_only() {}\n' > "$repo/src/scratch.rs"
run_check
expect_status "untracked file is not read" zero
expect_no_output "untracked ignore is not reported" "scratch_only"

# --- Case 9: a list entry with a missing or unknown category fails. ---
new_repo category
mkdir -p "$repo/src"
printf '#[test]\n#[ignore]\nfn a() {}\n#[test]\n#[ignore]\nfn b() {}\n' > "$repo/src/lib.rs"
cat > "$repo/allow.txt" <<'EOF'
src/lib.rs::a  # bug: known wrong answer
src/lib.rs::b
EOF
commit_all
run_check
expect_status "entries without an allowed category fail" nonzero
expect_output "unknown category is reported" "src/lib.rs::a"
expect_output "missing category is reported" "src/lib.rs::b"
cat > "$repo/allow.txt" <<'EOF'
src/lib.rs::a  # wall time (run by the gate): slow but correct
src/lib.rs::b  # outside data: reads a fixture checkout
EOF
run_check
expect_status "entries with allowed categories pass" zero

# --- Case 10: conditional ignores, including one wrapped across lines, are found. ---
new_repo conditional
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'EOF2'
#[test]
#[cfg_attr(miri, ignore)]
fn one_line_conditional() {}

#[test]
#[cfg_attr(
    target_os = "windows",
    ignore = "a long reason that rustfmt wrapped onto its own line"
)]
fn wrapped_conditional() {}

#[test]
#[cfg_attr(
    target_os = "windows",
    should_panic
)]
fn wrapped_without_ignore() {}
EOF2
printf '' > "$repo/allow.txt"
commit_all
run_check
expect_status "conditional ignores fail when unlisted" nonzero
expect_output "one-line conditional ignore is named" "src/lib.rs::one_line_conditional"
expect_output "wrapped conditional ignore is named" "src/lib.rs::wrapped_conditional"
expect_output "wrapped conditional ignore keeps its reason" "rustfmt wrapped onto its own line"
expect_no_output "a wrapped attribute without ignore is not reported" "wrapped_without_ignore"

# --- Case 11: two ignore attributes on one test are both named after that test. ---
new_repo twice
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'EOF2'
#[test]
#[cfg_attr(miri, ignore)]
#[ignore = "perf harness"]
fn doubly_ignored() {}
EOF2
printf 'src/lib.rs::doubly_ignored  # perf: harness\n' > "$repo/allow.txt"
commit_all
run_check
expect_status "a listed test with two ignore attributes passes" zero
expect_no_output "neither attribute is reported without a name" "::?"

echo
echo "check-test-ignores tests: $passes passed, $failures failed"
if [ "$failures" -ne 0 ]; then
  exit 1
fi
