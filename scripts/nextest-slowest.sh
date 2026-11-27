#!/usr/bin/env bash
# Print the slowest tests recorded in one or more nextest JUnit files.
#
# Usage:
#   scripts/nextest-slowest.sh                       # every target/nextest/*/junit.xml, top 30
#   scripts/nextest-slowest.sh -n 50                 # top 50
#   scripts/nextest-slowest.sh junit-1.xml dir/ ...  # these files, or every junit*.xml under these dirs
#
# Several files (the CI partitions) are merged into one ranking. Each row is the test's wall time
# in seconds, its outcome, the test binary (nextest's binary id) and the test name. A test that
# was retried appears once per attempt, though both profiles in .config/nextest.toml set
# `retries = 0`.
#
# The CI partition jobs upload their JUnit files as the `junit-partition-*` artifacts; download
# them and point this script at the folder. Reads XML with Python's standard library, so it needs
# only python3 (preinstalled on the GitHub runners and with the macOS command line tools).

set -euo pipefail

count=30
paths=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    -n)
      [ "$#" -ge 2 ] || { echo "nextest-slowest: -n needs a number" >&2; exit 2; }
      count="$2"
      shift 2
      ;;
    -h|--help)
      sed -n '2,16p' "$0"
      exit 0
      ;;
    *)
      paths+=("$1")
      shift
      ;;
  esac
done

case "$count" in
  ''|*[!0-9]*) echo "nextest-slowest: -n wants a positive whole number, got '$count'" >&2; exit 2 ;;
esac

if [ "${#paths[@]}" -eq 0 ]; then
  cd "$(dirname "$0")/.."
  paths=(target/nextest/*/junit.xml)
fi

# -I: isolated mode, so nothing in the current directory or PYTHON* variables is imported.
python3 -I - "$count" "${paths[@]}" <<'PY'
import os
import sys
import xml.etree.ElementTree as ET

count = int(sys.argv[1])
files = []
for p in sys.argv[2:]:
    if os.path.isdir(p):
        for root, _dirs, names in os.walk(p):
            files.extend(os.path.join(root, n) for n in names
                         if n.startswith("junit") and n.endswith(".xml"))
    elif os.path.isfile(p):
        files.append(p)
    else:
        sys.exit(f"nextest-slowest: no such JUnit file or folder: {p}")
if not files:
    sys.exit("nextest-slowest: no JUnit files found (run with `--profile ci`, or pass paths)")

rows = []
for f in sorted(files):
    for case in ET.parse(f).getroot().iter("testcase"):
        if case.find("skipped") is not None:
            continue
        if case.find("failure") is not None or case.find("error") is not None:
            outcome = "FAIL"
        elif case.find("flakyFailure") is not None or case.find("flakyError") is not None:
            outcome = "FLAKY"
        else:
            outcome = "pass"
        rows.append((float(case.get("time", "0") or 0), outcome,
                     case.get("classname", "?"), case.get("name", "?")))

rows.sort(key=lambda r: r[0], reverse=True)
total = sum(r[0] for r in rows)
print(f"{len(rows)} tests from {len(files)} JUnit file(s), {total:.1f} s of test time in all")
print(f"{'rank':>4}  {'secs':>8}  {'result':<6}  binary :: test")
for i, (secs, outcome, binary, name) in enumerate(rows[:count], 1):
    print(f"{i:>4}  {secs:>8.2f}  {outcome:<6}  {binary} :: {name}")
PY
