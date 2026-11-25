#!/usr/bin/env bash
# Targeted mutation testing: run cargo-mutants only on the lines this branch changed, and only
# in risk-class code (billing and resume, coverage accounting, the release gate, redaction and
# secrets, deletion, client-read export). Survivors are review findings: each one is either a
# test gap or an equivalent mutant, and the reviewer says which.
#
#   scripts/mutants-in-diff.sh <base-ref> [extra cargo-mutants args...]
#   scripts/mutants-in-diff.sh origin/feat/audit-report-export
#   MUTANTS_JOBS=3 scripts/mutants-in-diff.sh origin/main
#
# It diffs <base-ref>...HEAD (the merge base to HEAD, so only this branch's own changes count),
# keeps the risk paths below, and runs cargo-mutants in diff mode on the camerata-server unit
# tests. `--cargo-arg=--lib` keeps both the per-mutant build and the test run to the library
# target, so the 80-odd end-to-end binaries under crates/server/tests are neither built nor run.
# An empty filtered diff exits 0 with a one-line note.
#
# The test run is narrowed further by a name filter, derived from the changed risk files: each
# file's module stem (`scan_ledger`, `coverage_gaps`, `cost_checkpoint`, `checkpoint`; a
# `mod.rs` gives its directory's name), and for a non-test file under one of the directory risk
# paths that directory's module too (`report_export`, `release_gate`, `run_store`,
# `usage_meter`), because those modules keep their tests in sibling `*_tests` files named after
# the behaviour, not the file. A billing file (`usage_meter`, `cost_checkpoint`) also adds the
# modules that test the limit through the stages obeying it (`efficiency_cap`,
# `group_review_tests`, `spend_limit`, `cost_cap`, `resume_limit`). libtest matches each word as
# a substring, so `coverage_gaps` also selects `coverage_gaps_tests`.
#
# Why a filter at all: the whole unit suite takes about 25 minutes on a 4-core container (four
# scale tests dominate it), which is minutes per mutant; the filtered run takes seconds to about
# a minute, so a mutant costs about one incremental rebuild plus that. The price is that a test
# elsewhere that would catch a mutant is not run: confirm each survivor against the wider suite
# before calling it a gap (MUTANTS_TEST_FILTER=all, or a module list of your own).
#
# Four scale tests take several minutes each in a debug build and are skipped by default
# (`--skip`, never `#[ignore]`): one is a release gate secrets test and one a resume test, so a
# diff in those areas wants a full run with MUTANTS_INCLUDE_SLOW=1 before release. Their full
# names contain `ai_audit`, `release_gate` or `report_export`, so the filter alone would select
# them.
#
# Commit first: cargo-mutants mutates a copy of the working tree, and the diff is of commits,
# so uncommitted edits to a risk path would make the diff and the tree disagree.
#
# Settings (environment):
#   MUTANTS_TEST_FILTER  space-separated libtest name filters in place of the derived stems,
#                        or `all` for no filter (the whole unit suite: slow)
#   MUTANTS_INCLUDE_SLOW `1` to keep the four scale tests skipped by default (above)
#   MUTANTS_TIMEOUT      seconds one mutant's test run may take before it counts as a timeout
#                        (default 300: the filtered runs measured 1 to 45 seconds alone, and
#                        two jobs at once can double that; a hung mutant waits this long)
#   MUTANTS_JOBS         mutants built and tested at once (default 2; each job holds its own
#                        build directory of about 4 GiB)
#   MUTANTS_OUT          where cargo-mutants writes mutants.out (default target/mutants-in-diff)
#
# Exit status is cargo-mutants' own: 0 every mutant caught, 2 survivors, 3 timeouts,
# 4 the unmutated baseline failed. Needs cargo-mutants (`cargo install cargo-mutants --locked`;
# the trial used 27.1.0).
#
# Bash 3.2 and BSD tools compatible: no arrays, no `sed -i`, no `grep -P`.
set -euo pipefail

if [ $# -lt 1 ]; then
  echo "usage: scripts/mutants-in-diff.sh <base-ref> [extra cargo-mutants args...]" >&2
  exit 64
fi
base="$1"
shift

cd "$(git rev-parse --show-toplevel)"

if ! git rev-parse --verify --quiet "${base}^{commit}" >/dev/null; then
  echo "mutants-in-diff: '$base' is not a commit (fetch it first?)" >&2
  exit 64
fi
if ! cargo mutants --version >/dev/null 2>&1; then
  echo "mutants-in-diff: cargo-mutants is not installed (cargo install cargo-mutants --locked)" >&2
  exit 69
fi

# The risk paths. A path that does not exist on either side is simply not matched.
risk_paths='crates/server/src/run_store/
crates/server/src/ai_audit/usage_meter.rs
crates/server/src/ai_audit/usage_meter/
crates/server/src/cost_checkpoint.rs
crates/server/src/ai_audit/batch_mode.rs
crates/server/src/ai_audit/stage_batch.rs
crates/server/src/release_gate/
crates/server/src/scan_ledger.rs
crates/server/src/onboard/files.rs
crates/server/src/report_export/'

# The scale tests skipped unless MUTANTS_INCLUDE_SLOW=1 (see the header).
slow_tests='a_resumed_30k_site_calibration_resubmits_nothing_already_absorbed
a_five_thousand_file_repo_assembles_within_the_window_and_the_batch_limit
gate_failure_message_never_contains_the_secret
a_package_of_thirty_thousand_places_clears_the_release_gate'

timeout="${MUTANTS_TIMEOUT:-300}"
jobs="${MUTANTS_JOBS:-2}"
out="${MUTANTS_OUT:-target/mutants-in-diff}"

diff_file="$(mktemp "${TMPDIR:-/tmp}/mutants-in-diff.XXXXXX")"
trap 'rm -f "$diff_file"' EXIT

# The paths are word-split on purpose (none holds a space); `set -f` stops any globbing.
set -f
# shellcheck disable=SC2086
git diff --no-color --no-ext-diff "${base}...HEAD" -- $risk_paths >"$diff_file"
set +f

if [ ! -s "$diff_file" ]; then
  echo "mutants-in-diff: no risk-path changes in ${base}...HEAD; nothing to mutate."
  exit 0
fi

files_changed="$(grep -c '^diff --git ' "$diff_file" || true)"

# The test filter: each changed risk file's module stem and directory module, once each.
if [ -n "${MUTANTS_TEST_FILTER:-}" ]; then
  filter="$MUTANTS_TEST_FILTER"
else
  set -f
  dir_paths="$(printf '%s\n' "$risk_paths" | grep '/$' | tr '\n' ' ')"
  # shellcheck disable=SC2086
  filter="$(git diff --name-only "${base}...HEAD" -- $risk_paths |
    awk -v dirs="$dir_paths" '
      BEGIN { n = split(dirs, d, " ") }
      {
        k = split($0, part, "/")
        f = part[k]
        if (f == "mod.rs" && k > 1) f = part[k - 1]
        sub(/\.rs$/, "", f)
        print f
        # A changed test file holds no mutants: its directory module would only add run time.
        if (f ~ /tests$/ || $0 ~ /\/tests\//) next
        # The spend limit and checkpoint are tested through the stages that obey them.
        if ($0 ~ /usage_meter|cost_checkpoint/) {
          print "efficiency_cap"; print "group_review_tests"; print "spend_limit"
          print "cost_cap"; print "resume_limit"
        }
        for (i = 1; i <= n; i++) {
          if (index($0, d[i]) == 1) {
            m = split(d[i], dp, "/")
            print dp[m - 1]
          }
        }
      }' |
    sort -u | tr '\n' ' ')"
  set +f
fi
skips=""
if [ "${MUTANTS_INCLUDE_SLOW:-0}" != "1" ]; then
  for t in $slow_tests; do
    skips="$skips --skip $t"
  done
fi
if [ "$filter" = "all" ]; then
  filter=""
  shown="none (the whole unit suite)"
else
  shown="$filter"
fi

# Listing is instant; the count says up front whether this is minutes or an hour.
n_mutants="$(cargo mutants --list --in-diff "$diff_file" --package camerata-server 2>/dev/null |
  grep -c . || true)"
echo "mutants-in-diff: ${files_changed} risk-path file(s) changed in ${base}...HEAD;" \
  "${n_mutants} mutant(s)"
echo "mutants-in-diff: test filter: ${shown}"
if [ -n "$skips" ]; then
  echo "mutants-in-diff: skipping the four scale tests (MUTANTS_INCLUDE_SLOW=1 keeps them)"
fi
echo "mutants-in-diff: timeout ${timeout}s, ${jobs} job(s), output in ${out}/mutants.out"

# A dry run (`--list` among the extra arguments) prints the mutants and stops: no summary.
for arg in ${1+"$@"}; do
  if [ "$arg" = "--list" ]; then
    exec cargo mutants --in-diff "$diff_file" --package camerata-server ${1+"$@"}
  fi
done

mkdir -p "$out"
# cargo-mutants builds in a copy without `.git`, where crates/server/build.rs finds no commit
# and the export tests that need one (release_gate::published_example_tests) fail the
# unmutated baseline. The build script takes the commit from this variable instead.
CAMERATA_GIT_COMMIT="$(git rev-parse HEAD)"
export CAMERATA_GIT_COMMIT
start="$(date +%s)"
set +e
set -f
# The filter and skips are word-split on purpose: each word is one libtest argument.
# shellcheck disable=SC2086
cargo mutants \
  --in-diff "$diff_file" \
  --package camerata-server \
  --cargo-arg=--lib \
  --timeout "$timeout" \
  --jobs "$jobs" \
  --output "$out" \
  --no-shuffle \
  ${1+"$@"} \
  -- -- $filter $skips
status=$?
set +f
set -e
elapsed=$(($(date +%s) - start))

# 0, 2 and 3 mean mutants were tested. Anything else (a usage error, 4 for a failed baseline)
# tested none, and the output folder may hold an earlier run's lists: no summary from those.
case "$status" in
  0 | 2 | 3) ;;
  *)
    echo "mutants-in-diff: cargo-mutants exited ${status} after ${elapsed}s; no mutants tested" >&2
    exit "$status"
    ;;
esac

count() {
  if [ -f "$out/mutants.out/$1" ]; then
    grep -c . "$out/mutants.out/$1" || true
  else
    echo 0
  fi
}

echo
echo "mutants-in-diff summary (${elapsed}s wall):"
echo "  caught   $(count caught.txt)"
echo "  missed   $(count missed.txt)"
echo "  timeout  $(count timeout.txt)"
echo "  unviable $(count unviable.txt)"
if [ -s "$out/mutants.out/missed.txt" ]; then
  echo "survivors (review each: a test gap, or an equivalent mutant):"
  sed 's/^/  /' "$out/mutants.out/missed.txt"
fi
if [ -s "$out/mutants.out/timeout.txt" ]; then
  echo "timeouts:"
  sed 's/^/  /' "$out/mutants.out/timeout.txt"
fi
exit "$status"
