#!/usr/bin/env bash
#
# The per-test time budget: fail when a test runs longer than LIMIT seconds
# (30) and is not on the allowlist in .config/slow-tests.txt.
#
#   ./scripts/ci-slow-tests.sh target/nextest/ci/junit.xml target/ci-timings/*.tests
#
# Three kinds of report, told apart by extension:
#
#   *.xml    a nextest JUnit report (the `ci` profile in .config/nextest.toml
#            writes one): the unit tests. One entry per test:
#            `<binary id> <test name>`.
#   *.tests  a per-test file from scripts/ci-test-shard.sh: the integration
#            tests, `<package>::<target> <test name> <seconds>`, as libtest's
#            `--report-time` measured them.
#   other    one `<name> <seconds>` line per entry; the doc tests, one entry
#            per crate (`doc::<crate>`), since rustdoc cannot time a single
#            doc test on a stable toolchain.
#
# The allowlist names a test, or a whole integration target. A target entry
# covers each of its tests, and is for one case only: a target whose tests
# queue on one lock by design. libtest times a test from its start, so a test
# that waited for the lock is charged the wait, and the last one in the queue
# carries nearly the whole target's time.
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
    *.tests)
      awk 'NF >= 3 { t = $2; for (i = 3; i < NF; i++) t = t " " $i; print $1 " " t "\t" $NF }' "$report" >> "$ENTRIES"
      ;;
    *)
      awk 'NF >= 2 { print $1 "\t" $2 }' "$report" >> "$ENTRIES"
      ;;
  esac
done

echo "slowest entries:"
# awk takes the first ten rather than `head`, which exits early and, under
# pipefail, fails the script on sort's SIGPIPE.
sort -t "$(printf '\t')" -k2,2 -rn "$ENTRIES" | awk -F '\t' 'NR <= 10 { printf "  %8.1f s  %s\n", $2, $1 }'

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
    # A target entry covers the tests of that target: `<target> <test>`.
    key = $1
    if (!(key in rec)) { sp = index(key, " "); if (sp > 0 && (substr(key, 1, sp - 1) in rec)) key = substr(key, 1, sp - 1) }
    if (key in rec) {
      if ($2 > 2 * rec[key])
        printf "%s took %.1f s, over twice the %.1f s %s is allowlisted at\n", $1, $2, rec[key], key
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
