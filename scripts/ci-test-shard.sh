#!/usr/bin/env bash
#
# Run one shard's integration test targets, in order, against $DATABASE_URL.
#
# CI calls this twice per shard, against the same database, because the property
# worth enforcing is not "the suite passes" but "the suite passes again". A test
# that leaves a role, a usage row or a fixed-name user behind passes the first
# time and fails the second, and until the second run existed nothing in CI
# could see it.
#
#   SHARD=1 TOTAL_SHARDS=3 ./scripts/ci-test-shard.sh
#
# Deliberately free of bash-4isms (no mapfile, no associative arrays) so it runs
# on a developer machine, including macOS's bash 3.2. A CI script nobody can
# execute locally is a script that only gets debugged by pushing.
set -euo pipefail

SHARD="${SHARD:?SHARD must be set}"
TOTAL_SHARDS="${TOTAL_SHARDS:?TOTAL_SHARDS must be set}"

# The target list is enumerated from `cargo metadata` rather than hard-coded,
# and partitioned over the whole enumerated list (see the partition below).
# That matters:
# with a hard-coded list, adding a test file and forgetting to register it would
# leave it silently unrun and CI green, which is a worse failure than a red
# build. Here every discovered target lands in exactly one shard by
# construction.
#
# The enumeration itself is the one thing that could fail open, so it is
# checked: an empty workspace list, or an empty shard, fails the job.
#
# One target is left out by name: `cluster_test`, which needs the two node
# harness and runs in the `cluster` job; here it would only skip itself.
#
# Every target passes alone on a fresh database (checked 2026-11-24 by running
# each one by itself against a new `createdb`), so which shard a target lands
# in, and which targets share its database, is a scheduling decision and not a
# correctness one. That was not always so: `language_navigation_test` used to
# pass only when an earlier target in the same database had migrated the
# translation plugin, which is why this list once could not be reordered.
ALL=$(
  cargo metadata --no-deps --format-version 1 \
    | jq -r '.packages[] | .name as $p
             | .targets[] | select(.kind[] == "test")
             | "\($p) \(.name)"' \
    | grep -v ' cluster_test$' \
    | sort
)

TOTAL=$(printf '%s\n' "$ALL" | grep -c . || true)
if [ "$TOTAL" -eq 0 ]; then
  echo "::error::enumerated zero integration test targets — refusing to pass vacuously"
  exit 1
fi
echo "Discovered $TOTAL integration test targets across the workspace."

# The partition. Longest first onto the least loaded shard, by the measured
# cost of each target in .config/ci-test-weights.txt, starting from the work
# each shard already carries outside this script (its `@shard` lines: the unit
# and doc tests). Index modulo three, which this replaced, put the three
# slowest targets and the unit tests on one runner and left it ten minutes
# behind the other two.
#
# Deterministic: ties go to the lower shard, and equal weights to the target
# that sorts first, so every runner computes the same partition. A target the
# weights file does not list gets the `@default` weight, so a new test file is
# still scheduled, just not cleverly until its time is recorded.
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WEIGHTS="${WEIGHTS:-$ROOT/.config/ci-test-weights.txt}"
if [ ! -r "$WEIGHTS" ]; then
  echo "::error::cannot read the target weights file $WEIGHTS"
  exit 1
fi

PLAN=$(printf '%s\n' "$ALL" | awk -v n="$TOTAL_SHARDS" -v wf="$WEIGHTS" '
  BEGIN {
    dflt = 10
    while ((getline line < wf) > 0) {
      sub(/#.*/, "", line)
      if (line !~ /[^ \t]/) continue
      k = split(line, f, " ")
      if (f[1] == "@default") dflt = f[2] + 0
      else if (f[1] ~ /^@shard[0-9]+$/) base[substr(f[1], 7) + 0] = f[2] + 0
      else w[f[1]] = f[2] + 0
    }
  }
  { pkg[NR] = $1; tgt[NR] = $2; key = $1 "::" $2; cost[NR] = (key in w) ? w[key] : dflt }
  END {
    for (s = 1; s <= n; s++) load[s] = base[s] + 0
    for (i = 1; i <= NR; i++) done[i] = 0
    for (k = 1; k <= NR; k++) {
      best = 0
      for (i = 1; i <= NR; i++)
        if (!done[i] && (best == 0 || cost[i] > cost[best])) best = i
      done[best] = 1
      to = 1
      for (s = 2; s <= n; s++) if (load[s] < load[to]) to = s
      load[to] += cost[best]
      shard[best] = to
    }
    for (i = 1; i <= NR; i++) print shard[i], pkg[i], tgt[i], cost[i]
    for (s = 1; s <= n; s++) printf "@load %d %d\n", s, load[s]
  }')

printf '%s\n' "$PLAN" | awk '$1 == "@load" {print "  planned load, shard " $2 ": " $3 " s"}'

MINE=$(printf '%s\n' "$PLAN" | awk -v s="$SHARD" '$1 == s {print $2, $3}')

COUNT=$(printf '%s\n' "$MINE" | grep -c . || true)
if [ "$COUNT" -eq 0 ]; then
  echo "::error::shard $SHARD selected no targets out of $TOTAL"
  exit 1
fi
echo "shard $SHARD runs $COUNT of $TOTAL target(s):"
printf '%s\n' "$MINE" | sed 's/^/  /'

# Where each target's time goes, one `<package>::<target> <seconds>` line per
# target, for scripts/ci-slow-tests.sh. Optional: unset, nothing is written.
TIMINGS_FILE="${TIMINGS_FILE:-}"

# ONE cargo invocation for the whole shard, every package and every selected
# target. One per package, which this was, built the kernel a second time for
# `trovato-mcp`'s single target, because each `-p` selection resolves the
# kernel's features on its own: two minutes of the shard for one test file
# (run 45397439767). cargo still runs the targets one after another, so no two
# of them touch the database at the same time; the tests INSIDE a target run
# in parallel (four threads, below), as they do in a local `cargo test`, which
# is what the targets' own locks are written for.
#
# libtest and not nextest, deliberately, for these targets. nextest runs every
# test in its own process, and every process here builds its own TestApp: the
# WASM plugins compiled, the templates parsed, the database checked. Measured
# on front_page_test: its fifteen tests take 3.6 s in one process and 26 s as
# fifteen, 45 CPU-seconds against 3.3. Four cores do not win back a thirteen
# fold increase in work. Unit tests, which build nothing, do run under nextest.
# Four test threads per target, the CPU count of a GitHub-hosted runner,
# whatever the machine. libtest's default is one per CPU, and some targets build
# a whole app per test (`TestApp::new`, a pool of up to 25 connections each):
# on a 32 core workstation that is more connections than the 100 the Postgres
# service allows, and the run fails with `PoolTimedOut` for a reason CI never
# sees. Pinned, a local run of this script is the run CI does.
export RUST_TEST_THREADS="${RUST_TEST_THREADS:-4}"

PKGS=$(printf '%s\n' "$MINE" | awk '{print $1}' | sort -u | awk '{printf " -p %s", $1}')
FLAGS=$(printf '%s\n' "$MINE" | awk '{printf " --test %s", $2}')
echo "::group::cargo test$PKGS$FLAGS"
# Unquoted on purpose: PKGS and FLAGS are built-up argument lists.
# shellcheck disable=SC2086
# MINE goes in through the environment: BSD awk refuses a newline in `-v`.
cargo test $PKGS $FLAGS 2>&1 | MINE="$MINE" awk -v out="$TIMINGS_FILE" '
  BEGIN { n = split(ENVIRON["MINE"], rows, "\n"); for (i = 1; i <= n; i++) { split(rows[i], f, " "); pkg[f[2]] = f[1] } }
  { print; fflush() }
  /Running/ && match($0, /tests\/[^ ]+\.rs/) { cur = substr($0, RSTART + 6, RLENGTH - 9) }
  /^test result:/ && cur != "" {
    if (out != "" && match($0, /finished in [0-9.]+s/))
      print pkg[cur] "::" cur " " substr($0, RSTART + 12, RLENGTH - 13) >> out
    cur = ""
  }'
echo "::endgroup::"

df -h /
