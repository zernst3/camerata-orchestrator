#!/usr/bin/env bash
#
# Un-onboard repos so onboarding can be re-tested end to end. For each target repo this
# UNDOES what onboarding produced:
#
#   1. Deletes the GitHub issues onboarding filed (the stories): titles starting with
#      "Tech debt" (resolve-later / resolve-now) or "Wire mechanical rules into CI".
#   2. Removes the rules from the repo: deletes the `camerata/onboard-governance` branch
#      (AGENTS.md / CONVENTIONS.md / CI workflow / .camerata/baseline.json) and closes any
#      open PR from it.
#   3. Clears the local "onboarded" flag in Camerata's projects.json so the repo reads as
#      not-yet-onboarded and flows through onboarding again.
#
# Targets = repos in projects.json whose `owner/repo` matches one of the given substrings.
#
# Usage:
#   scripts/unonboard.sh                 # default substring "mini" (agora-mini, budget-mini)
#   scripts/unonboard.sh agora-mini      # one substring
#   scripts/unonboard.sh -y mini         # skip the confirmation prompt
#
# THIS IS DESTRUCTIVE (it permanently deletes issues + a branch on GitHub). Quit the
# Camerata app first (it flushes projects.json from memory on the next mutation).
#
# Requires: gh (authenticated) and jq.
set -euo pipefail

assume_yes=0
if [ "${1:-}" = "-y" ] || [ "${1:-}" = "--yes" ]; then assume_yes=1; shift; fi

patterns=("$@")
[ ${#patterns[@]} -eq 0 ] && patterns=("mini")

command -v jq >/dev/null 2>&1 || { echo "error: jq required (brew install jq)" >&2; exit 1; }
command -v gh >/dev/null 2>&1 || { echo "error: gh required (brew install gh; gh auth login)" >&2; exit 1; }

# Locate the data dir the way Rust's dirs::data_dir() does.
if [ -d "$HOME/Library/Application Support/camerata" ]; then
  dir="$HOME/Library/Application Support/camerata"            # macOS
elif [ -d "${XDG_DATA_HOME:-$HOME/.local/share}/camerata" ]; then
  dir="${XDG_DATA_HOME:-$HOME/.local/share}/camerata"        # Linux
else
  echo "error: could not find the camerata data dir" >&2; exit 1
fi
projects="$dir/projects.json"
[ -f "$projects" ] || { echo "error: no projects.json at $projects" >&2; exit 1; }

branch="camerata/onboard-governance"
# Onboarding-created issue titles (anchored prefixes).
issue_re='^(Tech debt|Wire mechanical rules into CI)'
# Substring OR-regex for repo matching.
repo_re=$(IFS='|'; echo "${patterns[*]}")

# Distinct target repos across all projects.
mapfile -t repos < <(jq -r --arg re "$repo_re" '.projects[].repos[] | select(test($re))' "$projects" | sort -u)
if [ ${#repos[@]} -eq 0 ]; then
  echo "No repos in projects.json match: ${patterns[*]}"; exit 0
fi

echo "Target repos (match \"${patterns[*]}\"):"
printf '  - %s\n' "${repos[@]}"
echo
echo "For each, this will: delete onboarding issues (Tech debt* / Wire mechanical rules*),"
echo "delete the '$branch' branch + close its PR, and clear the local onboarded flag."
echo
if [ "$assume_yes" -ne 1 ]; then
  read -r -p "Proceed? This permanently deletes GitHub issues + the branch. [y/N] " ans
  [ "$ans" = "y" ] || [ "$ans" = "Y" ] || { echo "Aborted."; exit 0; }
fi

for repo in "${repos[@]}"; do
  echo "==> $repo"

  # 1. Delete the onboarding-created issues.
  nums=$(gh issue list --repo "$repo" --state all --limit 200 --json number,title \
    | jq -r --arg re "$issue_re" '.[] | select(.title | test($re)) | .number')
  if [ -n "$nums" ]; then
    for n in $nums; do
      echo "   deleting issue #$n"
      gh issue delete "$n" --repo "$repo" --yes || echo "   (could not delete #$n)"
    done
  else
    echo "   no onboarding issues found"
  fi

  # 2. Close any open PR from the governance branch, then delete the branch.
  pr=$(gh pr list --repo "$repo" --head "$branch" --state open --json number -q '.[0].number' 2>/dev/null || true)
  if [ -n "${pr:-}" ]; then
    echo "   closing PR #$pr"
    gh pr close "$pr" --repo "$repo" || true
  fi
  if gh api -X DELETE "repos/$repo/git/refs/heads/$branch" >/dev/null 2>&1; then
    echo "   deleted branch $branch"
  else
    echo "   no $branch branch (or already gone)"
  fi
done

# 3. Clear the local onboarded flag for the matched repos (across all projects).
cp "$projects" "$projects.bak"
jq --arg re "$repo_re" '
  .projects |= map(.onboarded |= map(select(test($re) | not)))
' "$projects" > "$projects.tmp" && mv "$projects.tmp" "$projects"
echo
echo "Cleared local onboarded flags (backup: $projects.bak)."
echo "Restart Camerata; the matched repos now read as not-yet-onboarded."
