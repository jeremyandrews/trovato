#!/usr/bin/env bash
#
# The per-test time budget: fail when a test runs longer than LIMIT seconds
# (30) and is not on the allowlist in .config/slow-tests.txt.
#
#   ./scripts/ci-slow-tests.sh target/nextest/ci/junit.xml target/ci-timings/*.txt
#
# Two kinds of report, told apart by extension:
#
#   *.xml  a nextest JUnit report (the `ci` profile in .config/nextest.toml
#          writes one). One entry per test: `<binary id> <test name>`.
#   other  a timings file from scripts/ci-test-shard.sh, one
#          `<package>::<target> <seconds>` line per integration target. Those
#          targets run under libtest, whose stable output times a whole target
#          and not its tests, so the budget applies to the target: one test
#          over 30 s makes its target over 30 s, which is what trips.
#
# An allowlisted entry is not exempt for ever: it fails again at twice the time
# it was recorded at, so a slow test that gets slower is still caught.
#
# Why this exists: CI wall clock is set by the slowest shard, and a shard is a
# sum of these. Nothing stopped one test from adding a minute to every pull
# request; the 30 s line makes adding one a decision written down with a
# reason, in a file a reviewer reads.
set -euo pipefail

LIMIT="${LIMIT:-30}"
ROOT=$(cd "$(dirname "$0")/.." && pwd)
ALLOW="${ALLOW:-$ROOT/.config/slow-tests.txt}"

if [ "$#" -eq 0 ]; then
  echo "usage: $0 <junit.xml | timings.txt>..." >&2
  exit 2
fi

ENTRIES=$(mktemp)
trap 'rm -f "$ENTRIES"' EXIT

for report in "$@"; do
  if [ ! -s "$report" ]; then
    echo "::error::report $report is missing or empty; the budget cannot be checked against nothing"
    exit 1
  fi
  case "$report" in
    *.xml)
      # JUnit attributes are XML-escaped, so parse it as XML rather than with
      # a regular expression. One `<name>\t<seconds>` line per test case.
      python3 - "$report" >> "$ENTRIES" <<'PY'
import sys
import xml.etree.ElementTree as ET

for case in ET.parse(sys.argv[1]).getroot().iter("testcase"):
    print(f"{case.get('classname')} {case.get('name')}\t{float(case.get('time', 0)):.3f}")
PY
      ;;
    *)
      awk 'NF >= 2 { print $1 "\t" $2 }' "$report" >> "$ENTRIES"
      ;;
  esac
done

echo "slowest entries:"
sort -t "$(printf '\t')" -k2,2 -rn "$ENTRIES" | head -10 | awk -F '\t' '{ printf "  %8.1f s  %s\n", $2, $1 }'

# Allowlist lines: `<name> | <recorded seconds> | <reason>`; `#` starts a comment.
FAILED=$(awk -F '\t' -v limit="$LIMIT" -v allow="$ALLOW" '
  BEGIN {
    while ((getline line < allow) > 0) {
      if (line ~ /^[ \t]*(#|$)/) continue
      n = split(line, f, "|")
      if (n < 3) { printf "malformed line in .config/slow-tests.txt (want name | seconds | reason): %s\n", line; continue }
      name = f[1]; gsub(/^[ \t]+|[ \t]+$/, "", name)
      rec[name] = f[2] + 0
    }
  }
  {
    if ($1 in rec) {
      if ($2 > 2 * rec[$1])
        printf "%s took %.1f s, over twice the %.1f s it is allowlisted at\n", $1, $2, rec[$1]
    } else if ($2 > limit) {
      printf "%s took %.1f s, over the %d s budget, and is not in .config/slow-tests.txt\n", $1, $2, limit
    }
  }' "$ENTRIES")

if [ -n "$FAILED" ]; then
  printf '%s\n' "$FAILED" | sed 's/^/::error::/'
  echo "Make it faster, or add it to .config/slow-tests.txt with its measured time and the reason it cannot be."
  exit 1
fi
echo "every test is inside the ${LIMIT} s budget or on the allowlist."
