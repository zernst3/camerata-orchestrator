#!/usr/bin/env bash
# Fails if any tracked file names a personal filesystem location: a macOS home directory
# (anything under the macOS home root other than the placeholder user `auditor`), a path
# spelled relative to a home directory (`~` or `$HOME` followed by the Documents folder), the
# Documents-then-Repos layout, the Claude Code project-directory form of a home path
# (the macOS home root with its slashes turned into dashes), or the private fixture folder
# that the benchmark repositories live in. This repository is public, so a path like that
# discloses a username, a directory layout and where private material is kept.
#
#   scripts/check-personal-paths.sh          # the working tree's tracked files
#   scripts/check-personal-paths.sh <rev>    # the files of a commit, e.g. origin/main
#
# Write examples as /Users/auditor/..., and have tests that need a local fixture read
# CAMERATA_FIXTURES_DIR and skip when it is unset (or fail loudly when it is set but wrong).
#
# The home-root match is case-insensitive, except that the all-lowercase form is read as a
# URL route (`/users/:id`) and ignored: spelling a real home path in lowercase is not a case
# worth the flood of route false positives.
#
# The patterns are assembled from pieces so this script does not match itself.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

home_prefix='/Use''rs/'
home_dashed='-Use''rs-'
tilde_docs='~/Docu''ments'
env_docs='$HOME/Docu''ments'
docs_repos='Docu''ments/Re''pos'
fixture_folder='Camerata'' tests'

# TEMPORARY exclusion. `.claude/settings.json` is the agent permission deny list. Its entries
# name the private fixture folder and its answer keys on purpose, so that agents are refused
# access to them; rewriting those paths in place would silently disarm the deny rules. The
# exclusion stays only until those rules move out of the tracked file (to a user-level settings
# file, or a templated one). That move changes agent permissions, so it needs the maintainer's
# sign-off and has not been made. Delete this exclusion in the same change as the move.
exclusions=(':(exclude).claude/settings.json')

# `git grep` takes at most one revision here. Positional arguments keep this working on
# macOS's bash 3.2, where expanding an empty array under `set -u` is an error.
if [[ $# -gt 0 ]]; then
  set -- "$1"
else
  set --
fi

# Case-insensitive here so every spelling is a candidate; the perl pass below decides.
# `git grep` exits 1 when nothing matches; only a status above 1 is an error. The status is
# captured on the assignment itself so a failure inside the command can never read as "clean".
set +e
hits="$(git grep -n -I -i -F \
  -e "$home_prefix" -e "$home_dashed" -e "$tilde_docs" -e "$env_docs" -e "$docs_repos" \
  -e "$fixture_folder" \
  "$@" -- . "${exclusions[@]}")"
status=$?
set -e
if [[ $status -gt 1 ]]; then
  echo "check-personal-paths: git grep failed (exit $status)" >&2
  exit "$status"
fi

# Keep a line only if, after removing every allowed placeholder, it still matches.
offending="$(printf '%s\n' "$hits" | HOME_PREFIX="$home_prefix" HOME_DASHED="$home_dashed" \
  TILDE_DOCS="$tilde_docs" ENV_DOCS="$env_docs" DOCS_REPOS="$docs_repos" \
  FIXTURE_FOLDER="$fixture_folder" perl -ne '
  next if /^$/;
  my $line = $_;
  my $home = quotemeta $ENV{HOME_PREFIX};
  my $dashed = quotemeta $ENV{HOME_DASHED};
  my $tilde = quotemeta $ENV{TILDE_DOCS};
  my $envdocs = quotemeta $ENV{ENV_DOCS};
  my $docsrepos = quotemeta $ENV{DOCS_REPOS};
  my $fixture = quotemeta $ENV{FIXTURE_FOLDER};
  my $end = q{(?=[/"\x27\x60\s)\-]|$)};
  my $rest = $line;
  # The placeholder user, optionally followed by the Documents-then-Repos layout.
  $rest =~ s{${home}auditor(?:/$docsrepos)?$end}{}gi;
  $rest =~ s{${dashed}auditor$end}{}g;
  my $bad = 0;
  while ($rest =~ m{($home)}gi) { $bad = 1 if $1 ne lc $ENV{HOME_PREFIX} }
  $bad = 1 if $rest =~ /$dashed/;
  $bad = 1 if $rest =~ /$tilde/i || $rest =~ /$envdocs/i || $rest =~ /$docsrepos/i;
  $bad = 1 if $line =~ /$fixture/i;
  print $line if $bad;
')"

if [[ -n "$offending" ]]; then
  echo "Personal paths found in tracked files (use /Users/auditor/... or CAMERATA_FIXTURES_DIR):" >&2
  printf '%s\n' "$offending" >&2
  exit 1
fi
echo "check-personal-paths: no personal paths in tracked files"
