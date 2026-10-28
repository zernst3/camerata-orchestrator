#!/usr/bin/env bash
# Run one CI gate unchanged, and make its failure legible from the Actions UI and API.
#
# Usage: scripts/ci-gate.sh <title> <command> [args...]
#
# The command runs exactly as given and its output streams to the job log as usual. If it
# fails, the tail of its output (plus the lines that name the failure) is re-emitted as
# `::error` workflow commands, which GitHub exposes as check-run annotations. Annotations are
# readable without authentication on a public repo; raw job logs are not. The exit status of
# the gate is preserved, so this wrapper can only ever make a red gate easier to diagnose, it
# can never turn a red gate green.
#
# Runs on the GitHub Linux runner (GNU sed/awk).

set -uo pipefail

if [ "$#" -lt 2 ]; then
  echo "usage: $0 <title> <command> [args...]" >&2
  exit 2
fi

title="$1"
shift

out="$(mktemp)"
"$@" 2>&1 | tee "$out"
status="${PIPESTATUS[0]}"

if [ "$status" -ne 0 ]; then
  # Strip ANSI colour codes and CRs, and cap line length.
  clean="$(mktemp)"
  sed -e 's/\x1b\[[0-9;]*[A-Za-z]//g' -e 's/\r//g' "$out" | cut -c1-400 > "$clean"

  # Escape for a workflow command: `%` -> %25, then join lines with %0A (LF).
  emit() { # $1 = annotation title (no commas or colons), $2 = file of lines
    local body
    body="$(sed -e 's/%/%25/g' "$2" | awk 'BEGIN{ORS="%0A"} {print}')"
    echo "::error title=${1}::${body}"
  }

  # GitHub caps error annotations per step, so emit one multi-line annotation per view
  # rather than one per line.
  grep -E -A 6 '^(error|warning)(\[|:)|FAILED|panicked at|^failures:|test result: FAILED|could not compile|^ +--> ' "$clean" | head -n 80 > "${clean}.hits" || true
  if [ -s "${clean}.hits" ]; then
    emit "${title} failure lines" "${clean}.hits"
  fi
  tail -n 40 "$clean" > "${clean}.tail"
  emit "${title} output tail (exit ${status})" "${clean}.tail"
fi

exit "$status"
