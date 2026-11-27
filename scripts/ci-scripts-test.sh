#!/usr/bin/env bash
#
# Tests for the CI timing scripts: scripts/ci-test-timings.awk (what each test
# and target took), scripts/ci-slow-tests.sh (the 30 s budget) and the target
# checks in scripts/ci-test-shard.sh. Fed made-up cargo output and reports, so
# it needs no database and builds nothing; it runs in a second.
#
#   ./scripts/ci-scripts-test.sh
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
FAILS=0

pass() { echo "ok   $1"; }
fail() { echo "FAIL $1"; FAILS=$((FAILS + 1)); }

# expect <name> <0|1> <command...>: the command must exit 0 (0) or not (1).
expect() {
  local name=$1 want=$2; shift 2
  if "$@" > "$WORK/out" 2>&1; then got=0; else got=1; fi
  if [ "$got" = "$want" ]; then pass "$name"; else fail "$name"; sed 's/^/     /' "$WORK/out"; fi
}

# has <name> <file> <line>: the file must contain the line exactly.
has() {
  if grep -qxF -- "$3" "$2"; then pass "$1"; else fail "$1"; echo "     wanted: $3"; sed 's/^/     got:    /' "$2"; fi
}

timings() { # timings <MINE> <input>, writing $WORK/targets and $WORK/tests
  rm -f "$WORK/targets" "$WORK/tests"
  MINE="$1" awk -v targets="$WORK/targets" -v tests="$WORK/tests" \
    -f "$ROOT/scripts/ci-test-timings.awk" < "$2" > /dev/null
}

# ── ci-test-timings.awk ──────────────────────────────────────────────────────

cat > "$WORK/cargo.out" <<'EOF'
     Running tests/front_page_test.rs (target/debug/deps/front_page_test-0123abcd)

running 2 tests
test a_fast_one ... ok <0.012s>
test the_page_lists_items ... ok <1.250s>

test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.26s

     Running elsewhere/custom.rs (target/debug/deps/custom_path-89ef0123)

running 1 test
test with spaces in its output ... ok <35.000s>

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 35.00s
EOF
MINE="trovato-kernel front_page_test
trovato-kernel custom-path"
expect "timings: a run of listed targets passes" 0 timings "$MINE" "$WORK/cargo.out"
has "timings: a target's time" "$WORK/targets" "trovato-kernel::front_page_test 1.26"
has "timings: each test's own time" "$WORK/tests" "trovato-kernel::front_page_test the_page_lists_items 1.250"
has "timings: a target outside tests/ is recorded under its own name" "$WORK/targets" "trovato-kernel::custom-path 35.00"
has "timings: a test is recorded with its target" "$WORK/tests" "trovato-kernel::custom-path with spaces in its output 35.000"

expect "timings: a listed target that never reports fails" 1 timings "$MINE
trovato-kernel never_ran_test" "$WORK/cargo.out"
expect "timings: a binary that is not on the list fails" 1 timings "trovato-kernel front_page_test" "$WORK/cargo.out"

# What CI sees: CARGO_TERM_COLOR=always colours cargo's own lines.
E=$(printf '\033')
sed "s/^     Running /${E}[1m${E}[92m     Running${E}[0m /" "$WORK/cargo.out" > "$WORK/colour.out"
expect "timings: coloured cargo output" 0 timings "$MINE" "$WORK/colour.out"
has "timings: a target's time, coloured" "$WORK/targets" "trovato-kernel::front_page_test 1.26"

cat > "$WORK/doc.out" <<'EOF'
   Doc-tests trovato_kernel

running 1 test
test crates/kernel/src/lib.rs - f (line 1) ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 35.20s
EOF
sed "s/^   Doc-tests /${E}[1m${E}[92m   Doc-tests${E}[0m /" "$WORK/doc.out" > "$WORK/doc-colour.out" && mv "$WORK/doc-colour.out" "$WORK/doc.out"
rm -f "$WORK/doc.txt"
expect "timings: doc tests" 0 sh -c "awk -v doc=1 -v targets='$WORK/doc.txt' -f '$ROOT/scripts/ci-test-timings.awk' < '$WORK/doc.out'"
has "timings: a crate's doc tests are one entry" "$WORK/doc.txt" "doc::trovato_kernel 35.20"

# ── ci-slow-tests.sh ─────────────────────────────────────────────────────────

budget() { ALLOW="$WORK/allow.txt" "$ROOT/scripts/ci-slow-tests.sh" "$@"; }

cat > "$WORK/allow.txt" <<'EOF'
trovato-kernel::queue_test | 32 | its tests queue on one lock by design
trovato-kernel::slow_test one_slow_test | 20 | measured
EOF

# Eight 16 s tests, four at a time, make a target of 32 s: no test is slow.
: > "$WORK/many.tests"
for i in 1 2 3 4 5 6 7 8; do echo "trovato-kernel::many_test test_$i 16.0" >> "$WORK/many.tests"; done
expect "budget: many tests inside 30 s each pass, whatever their target's total" 0 budget "$WORK/many.tests"

echo "trovato-kernel::zero_downtime_test a_new_slow_one 35.0" > "$WORK/slow.tests"
expect "budget: a 35 s test fails" 1 budget "$WORK/slow.tests"
if grep -q "trovato-kernel::zero_downtime_test a_new_slow_one took 35.0 s" "$WORK/out"; then
  pass "budget: the failure names the test"
else
  fail "budget: the failure names the test"; sed 's/^/     /' "$WORK/out"
fi

echo "trovato-kernel::slow_test one_slow_test 35.0" > "$WORK/allowed.tests"
expect "budget: an allowlisted test inside twice its time passes" 0 budget "$WORK/allowed.tests"
echo "trovato-kernel::slow_test another_test 35.0" > "$WORK/neighbour.tests"
expect "budget: allowing one test does not allow its neighbour" 1 budget "$WORK/neighbour.tests"

echo "trovato-kernel::queue_test waited_for_the_lock 40.0" > "$WORK/queue.tests"
expect "budget: a target entry covers its tests" 0 budget "$WORK/queue.tests"
echo "trovato-kernel::queue_test waited_for_ever 70.0" > "$WORK/queue2.tests"
expect "budget: a target entry still fails at twice its time" 1 budget "$WORK/queue2.tests"

expect "budget: a 35 s crate of doc tests fails" 1 budget "$WORK/doc.txt"

expect "budget: a missing report fails" 1 budget "$WORK/many.tests" "$WORK/no-such-junit.xml"
: > "$WORK/empty.xml"
expect "budget: an empty report fails" 1 budget "$WORK/many.tests" "$WORK/empty.xml"

cat > "$WORK/junit.xml" <<'EOF'
<testsuites><testsuite name="trovato-kernel"><testcase classname="trovato-kernel" name="unit::slow" time="31.0"/></testsuite></testsuites>
EOF
expect "budget: a 31 s unit test fails" 1 budget "$WORK/junit.xml"

# ── ci-test-shard.sh ─────────────────────────────────────────────────────────

# A cargo that only answers `metadata`, for the checks the script makes before
# it builds anything.
mkdir -p "$WORK/bin"
cat > "$WORK/bin/cargo" <<'EOF'
#!/bin/sh
[ "$1" = metadata ] || { echo "fake cargo: $*" >&2; exit 99; }
cat "$FAKE_METADATA"
EOF
chmod +x "$WORK/bin/cargo"
cat > "$WORK/dupes.json" <<'EOF'
{"packages": [
  {"name": "a", "targets": [{"name": "shared_test", "kind": ["test"]}, {"name": "only_a_test", "kind": ["test"]}]},
  {"name": "b", "targets": [{"name": "shared_test", "kind": ["test"]}]}
]}
EOF
shard() { PATH="$WORK/bin:$PATH" FAKE_METADATA="$1" SHARD=1 TOTAL_SHARDS=3 "$ROOT/scripts/ci-test-shard.sh"; }
expect "shard: a target name in two packages is refused" 1 shard "$WORK/dupes.json"
if grep -q "shared_test" "$WORK/out" && ! grep -q "fake cargo" "$WORK/out"; then
  pass "shard: it is refused by name, before anything runs"
else
  fail "shard: it is refused by name, before anything runs"; sed 's/^/     /' "$WORK/out"
fi

# ── scripts/build-test-plugins.sh stays in lock-step with CI ────────────────
#
# Both build the same WASM test plugins; a package added to one and not the
# other is a plugin CI builds that a local run silently skips, or the other
# way round. Compared as sorted sets, not text, so reordering or rewrapping
# either list is not a failure.

ci_packages() {
  awk '/- name: Build WASM plugins/{found++} found==1 && /cargo build/{incargo=1} incargo{print} incargo && /--target wasm32-wasip1 --release/{exit}' \
    "$ROOT/.github/workflows/ci.yml" \
    | grep -oE -- '-p [A-Za-z0-9_]+' | awk '{print $2}' | sort -u
}

script_packages() {
  awk '
    /^PACKAGES=/ { inpkgs = 1; sub(/^PACKAGES="/, ""); sub(/"$/, "") }
    inpkgs { print; if ($0 ~ /"$/) inpkgs = 0 }
  ' "$ROOT/scripts/build-test-plugins.sh" | tr -d '"' | tr ' \n' '\n\n' | grep -v '^$' | sort -u
}

CI_PKGS=$(ci_packages)
SCRIPT_PKGS=$(script_packages)
if [ -z "$CI_PKGS" ]; then
  fail "build-test-plugins: found the CI package list"
elif [ "$CI_PKGS" = "$SCRIPT_PKGS" ]; then
  pass "build-test-plugins: package set matches .github/workflows/ci.yml's Build WASM plugins step"
else
  fail "build-test-plugins: package set differs from CI"
  echo "     only in CI:              $(comm -23 <(printf '%s\n' "$CI_PKGS") <(printf '%s\n' "$SCRIPT_PKGS") | tr '\n' ' ')"
  echo "     only in build-test-plugins.sh: $(comm -13 <(printf '%s\n' "$CI_PKGS") <(printf '%s\n' "$SCRIPT_PKGS") | tr '\n' ' ')"
fi

# ── scripts/test-changed.sh selection logic ─────────────────────────────────
#
# A made-up two-package workspace, entirely under $WORK: no real build, no
# real git history. TEST_CHANGED_ROOT_OVERRIDE points the script at it instead
# of this checkout; TEST_CHANGED_OVERRIDE supplies the "changed files" list
# instead of git. The fake cargo answers `metadata` from a canned JSON and
# logs a `test` invocation's arguments instead of building anything.
TC_WS="$WORK/tc-ws"
mkdir -p "$TC_WS/crates/kernel/tests" "$TC_WS/crates/search_index/tests" "$TC_WS/migrations"
echo '// fixture' > "$TC_WS/crates/kernel/tests/redirect_query_test.rs"
echo '// fixture' > "$TC_WS/crates/kernel/tests/front_page_test.rs"
echo 'use services::redirect;' > "$TC_WS/crates/kernel/tests/feed_test.rs"
echo '// fixture' > "$TC_WS/crates/search_index/tests/search_index_test.rs"

cat > "$WORK/tc-metadata.json" <<EOF
{"packages": [
  {"name": "trovato-kernel", "manifest_path": "$TC_WS/crates/kernel/Cargo.toml", "targets": [
    {"name": "redirect_query_test", "kind": ["test"], "src_path": "$TC_WS/crates/kernel/tests/redirect_query_test.rs"},
    {"name": "front_page_test", "kind": ["test"], "src_path": "$TC_WS/crates/kernel/tests/front_page_test.rs"},
    {"name": "feed_test", "kind": ["test"], "src_path": "$TC_WS/crates/kernel/tests/feed_test.rs"}
  ]},
  {"name": "trovato-search-index", "manifest_path": "$TC_WS/crates/search_index/Cargo.toml", "targets": [
    {"name": "search_index_test", "kind": ["test"], "src_path": "$TC_WS/crates/search_index/tests/search_index_test.rs"}
  ]}
]}
EOF

mkdir -p "$WORK/tc-bin"
cat > "$WORK/tc-bin/cargo" <<EOF
#!/bin/sh
if [ "\$1" = metadata ]; then
  cat "$WORK/tc-metadata.json"
elif [ "\$1" = test ]; then
  shift
  echo "cargo test \$*" >> "$WORK/tc-cargo-test.log"
else
  echo "fake cargo: \$*" >&2
  exit 99
fi
EOF
chmod +x "$WORK/tc-bin/cargo"

changed() { # changed <changed-files> <args...>: run the script, capture stdout in $WORK/out
  local files="$1"; shift
  : > "$WORK/tc-cargo-test.log"
  PATH="$WORK/tc-bin:$PATH" TEST_CHANGED_ROOT_OVERRIDE="$TC_WS" TEST_CHANGED_OVERRIDE="$files" \
    "$ROOT/scripts/test-changed.sh" "$@" > "$WORK/out" 2>&1
}

changed "crates/kernel/src/services/redirect.rs"
if grep -q "integration target redirect_query_test" "$WORK/out" \
  && grep -q "integration target feed_test" "$WORK/out" \
  && ! grep -q "integration target search_index_test" "$WORK/out" \
  && grep -q "unit tests (--lib --bins): trovato-kernel" "$WORK/out"; then
  pass "test-changed: a src/ change selects its crate's unit tests and matching targets"
else
  fail "test-changed: a src/ change selects its crate's unit tests and matching targets"; sed 's/^/     /' "$WORK/out"
fi
if grep -q "cargo test --no-fail-fast -p trovato-kernel --lib --bins --test redirect_query_test --test feed_test" "$WORK/tc-cargo-test.log"; then
  pass "test-changed: it runs the selection in one cargo invocation"
else
  fail "test-changed: it runs the selection in one cargo invocation"; sed 's/^/     /' "$WORK/tc-cargo-test.log"
fi

changed "" front_page_test search_index_test
if grep -q "integration target front_page_test (trovato-kernel): named on the command line" "$WORK/out" \
  && grep -q "integration target search_index_test (trovato-search-index): named on the command line" "$WORK/out"; then
  pass "test-changed: named targets are selected across packages with no change at all"
else
  fail "test-changed: named targets are selected across packages with no change at all"; sed 's/^/     /' "$WORK/out"
fi

changed "crates/kernel/tests/front_page_test.rs"
if grep -q "integration target front_page_test (trovato-kernel): own file" "$WORK/out"; then
  pass "test-changed: a target's own file changing selects it"
else
  fail "test-changed: a target's own file changing selects it"; sed 's/^/     /' "$WORK/out"
fi

changed "crates/kernel/tests/common/mod.rs"
if grep -q "Running everything" "$WORK/out" && grep -q "cargo test --all --no-fail-fast" "$WORK/tc-cargo-test.log"; then
  pass "test-changed: a shared-fixture change runs everything"
else
  fail "test-changed: a shared-fixture change runs everything"; sed 's/^/     /' "$WORK/out"
fi

changed "migrations/20260101000000_x.sql"
if grep -q "Running everything" "$WORK/out" && grep -q "is a migration" "$WORK/out"; then
  pass "test-changed: a migration change runs everything"
else
  fail "test-changed: a migration change runs everything"; sed 's/^/     /' "$WORK/out"
fi

changed "Cargo.lock"
if grep -q "Running everything" "$WORK/out"; then
  pass "test-changed: a Cargo.lock change anywhere runs everything"
else
  fail "test-changed: a Cargo.lock change anywhere runs everything"; sed 's/^/     /' "$WORK/out"
fi

changed ""
if grep -qxF "Nothing in the change set selects a package or a target; nothing to run." "$WORK/out"; then
  pass "test-changed: no changes and no named targets selects nothing"
else
  fail "test-changed: no changes and no named targets selects nothing"; sed 's/^/     /' "$WORK/out"
fi

expect "test-changed: an unknown named target is refused" 1 \
  env PATH="$WORK/tc-bin:$PATH" TEST_CHANGED_ROOT_OVERRIDE="$TC_WS" TEST_CHANGED_OVERRIDE="" \
  "$ROOT/scripts/test-changed.sh" no_such_target_xyz

echo
if [ "$FAILS" -ne 0 ]; then
  echo "$FAILS check(s) failed"
  exit 1
fi
echo "all checks passed"
