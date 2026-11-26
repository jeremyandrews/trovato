#!/usr/bin/env bash
#
# Fails when CI's Test and Coverage jobs could build different WASM plugins.
#
# Both jobs must build the plugins the tests load by calling
# scripts/ci-build-plugins.sh, which holds the one list of them. Coverage runs
# only on pushes to main, so a pull request never shows it: when B218 added two
# plugins to the Test job's hand-written list and not to Coverage's, Coverage
# failed on every push to main and no pull request was red (B235). This check
# runs on pull requests, costs a second, and builds nothing.
#
# It fails when:
#   - the Test or the Coverage job does not run ./scripts/ci-build-plugins.sh;
#   - any job in the workflow names plugins by hand on a wasm32-wasip1 build
#     (`cargo build -p <name> ... --target wasm32-wasip1`, continuation lines
#     joined) or copies a named plugin out of target/wasm32-wasip1/release, the
#     shape of the list that drifted. A job that builds a different set for its
#     own reason reads it from a file, as admin-a11y does with seed-plugins.txt;
#   - a plugin in the script's list has no plugins/<name>/Cargo.toml.
#
#   ./scripts/ci-check-plugin-build-parity.sh [workflow]   (default: .github/workflows/ci.yml)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORKFLOW=${1:-$ROOT/.github/workflows/ci.yml}
SCRIPT=./scripts/ci-build-plugins.sh
SCRIPT_RE='\./scripts/ci-build-plugins\.sh'
FAILS=0

fail() { echo "FAIL $1"; FAILS=$((FAILS + 1)); }

# job_body <key>: the lines of one top-level job, from `  <key>:` to the next.
job_body() {
  awk -v key="  $1:" '
    $0 == key { on = 1; next }
    on && /^  [A-Za-z0-9_-]+:[[:space:]]*$/ { exit }
    on { print }' "$WORKFLOW"
}

for job in test coverage; do
  body=$(job_body "$job")
  if [ -z "$body" ]; then
    fail "no job '$job' in $WORKFLOW"
  elif ! grep -Eq "^[[:space:]]+(- )?run:[[:space:]]*${SCRIPT_RE}[[:space:]]*$|^[[:space:]]+${SCRIPT_RE}[[:space:]]*$" <<< "$body"; then
    fail "job '$job' does not build its plugins with $SCRIPT"
  else
    echo "ok   job '$job' builds its plugins with $SCRIPT"
  fi
done

# Every logical line (backslash continuations joined) that builds for
# wasm32-wasip1 and names a package literally, and every copy of a literally
# named plugin out of the release directory.
hand=$(awk '
  { sub(/\r$/, "") }
  /\\$/ { buf = buf substr($0, 1, length($0) - 1) " "; if (!start) start = NR; next }
  {
    line = buf $0; at = start ? start : NR; buf = ""; start = 0
    if (line ~ /^[[:space:]]*#/) next
    if (line ~ /--target[ =]wasm32-wasip1/ && line ~ /(^|[[:space:]])-p[[:space:]]+[A-Za-z0-9_-]+/) print at ": " line
    else if (line ~ /wasm32-wasip1\/release\/[A-Za-z0-9_-]+\.wasm/) print at ": " line
  }' "$WORKFLOW")
if [ -n "$hand" ]; then
  fail "a job keeps its own list of plugins; call $SCRIPT instead:"
  sed 's/^/     line /' <<< "$hand" | cut -c1-160 | head -n 12
  [ "$(wc -l <<< "$hand")" -gt 12 ] && echo "     ... and $(( $(wc -l <<< "$hand") - 12 )) more"
else
  echo "ok   no job names its own plugins on a wasm32-wasip1 build"
fi

missing=0
while read -r name; do
  if [ ! -f "$ROOT/plugins/$name/Cargo.toml" ]; then
    fail "$SCRIPT lists $name, which has no plugins/$name/Cargo.toml"
    missing=1
  fi
done < <("$ROOT/scripts/ci-build-plugins.sh" --list)
[ "$missing" = 0 ] && echo "ok   every plugin $SCRIPT lists is a crate under plugins/"

if [ "$FAILS" -ne 0 ]; then
  echo "$FAILS plugin build parity check(s) failed"
  exit 1
fi
echo "plugin build parity: ok"
