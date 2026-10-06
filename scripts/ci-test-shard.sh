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
# and partitioned by index modulo TOTAL_SHARDS over a sorted list. That matters:
# with a hard-coded list, adding a test file and forgetting to register it would
# leave it silently unrun and CI green, which is a worse failure than a red
# build. Here every discovered target lands in exactly one shard by
# construction.
#
# The enumeration itself is the one thing that could fail open, so it is
# checked: an empty workspace list, or an empty shard, fails the job.
ALL=$(
  cargo metadata --no-deps --format-version 1 \
    | jq -r '.packages[] | .name as $p
             | .targets[] | select(.kind[] == "test")
             | "\($p) \(.name)"' \
    | sort
)

TOTAL=$(printf '%s\n' "$ALL" | grep -c . || true)
if [ "$TOTAL" -eq 0 ]; then
  echo "::error::enumerated zero integration test targets — refusing to pass vacuously"
  exit 1
fi
echo "Discovered $TOTAL integration test targets across the workspace."

MINE=$(printf '%s\n' "$ALL" \
  | awk -v s="$SHARD" -v n="$TOTAL_SHARDS" '(NR - 1) % n == (s - 1)')

COUNT=$(printf '%s\n' "$MINE" | grep -c . || true)
if [ "$COUNT" -eq 0 ]; then
  echo "::error::shard $SHARD selected no targets out of $TOTAL"
  exit 1
fi
echo "shard $SHARD runs $COUNT of $TOTAL target(s):"
printf '%s\n' "$MINE" | sed 's/^/  /'

# One cargo invocation per package, with all of that package's selected test
# targets.
for pkg in $(printf '%s\n' "$MINE" | awk '{print $1}' | sort -u); do
  FLAGS=$(printf '%s\n' "$MINE" | awk -v p="$pkg" '$1 == p {printf " --test %s", $2}')
  echo "::group::cargo test -p $pkg$FLAGS"
  # Unquoted on purpose: FLAGS is a built-up list of --test arguments.
  # shellcheck disable=SC2086
  cargo test -p "$pkg" $FLAGS -- --test-threads=1
  echo "::endgroup::"
done

df -h /
