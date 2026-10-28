#!/usr/bin/env bash
# Fails if any tracked file names a personal filesystem location: a macOS home directory
# (anything under the macOS home root other than the placeholder user `auditor`) or the
# private fixture folder that the benchmark repositories live in. This repository is public, so a path like
# that discloses a username, a directory layout and where private material is kept.
#
#   scripts/check-personal-paths.sh          # the working tree's tracked files
#   scripts/check-personal-paths.sh <rev>    # the files of a commit, e.g. origin/main
#
# Write examples as /Users/auditor/..., and have tests that need a local fixture read
# CAMERATA_FIXTURES_DIR and skip when it is unset.
#
# The patterns are assembled from pieces so this script does not match itself.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

home_prefix='/Use''rs/'
fixture_folder='Camerata'' tests'

# `.claude/settings.json` is the agent permission deny list. Its entries name the private
# fixture folder and its answer keys on purpose, so that agents are refused access to them;
# rewriting those paths would silently disarm the deny rules.
exclusions=(':(exclude).claude/settings.json')

rev_args=()
if [[ $# -gt 0 ]]; then
  rev_args=("$1")
fi

# Case-insensitive here so the fixture folder is caught in any casing; the home prefix is
# matched case-sensitively below.
# `git grep` exits 1 when nothing matches; only a status above 1 is an error.
set +e
hits="$(git grep -n -I -i -F -e "$home_prefix" -e "$fixture_folder" "${rev_args[@]}" -- . "${exclusions[@]}")"
status=$?
set -e
if [[ $status -gt 1 ]]; then
  echo "check-personal-paths: git grep failed (exit $status)" >&2
  exit "$status"
fi

# Keep a line only if, after removing every allowed placeholder, it still matches.
offending="$(printf '%s\n' "$hits" | HOME_PREFIX="$home_prefix" FIXTURE_FOLDER="$fixture_folder" perl -ne '
  next if /^$/;
  my $line = $_;
  my $home = quotemeta $ENV{HOME_PREFIX};
  my $fixture = quotemeta $ENV{FIXTURE_FOLDER};
  (my $rest = $line) =~ s{${home}auditor(?=[/"\x27\x60\s)]|$)}{}g;
  print $line if $rest =~ /$home/ || $line =~ /$fixture/i;
')"

if [[ -n "$offending" ]]; then
  echo "Personal paths found in tracked files (use /Users/auditor/... or CAMERATA_FIXTURES_DIR):" >&2
  printf '%s\n' "$offending" >&2
  exit 1
fi
echo "check-personal-paths: no personal paths in tracked files"
