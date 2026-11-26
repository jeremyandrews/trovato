#!/usr/bin/env bash
#
# Tests for the CI scripts: scripts/ci-test-timings.awk (what each test
# and target took), scripts/ci-slow-tests.sh (the 30 s budget) and the target
# checks in scripts/ci-test-shard.sh, and the plugin build parity guard
# (scripts/ci-check-plugin-build-parity.sh). Fed made-up cargo output, reports
# and workflows, so it needs no database and builds nothing; it runs in a second.
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


# ── ci-check-plugin-build-parity.sh ──────────────────────────────────────────

parity() { "$ROOT/scripts/ci-check-plugin-build-parity.sh" "$1"; }
expect "parity: the workflow as committed passes" 0 parity "$ROOT/.github/workflows/ci.yml"
# Coverage building its own list again, the B235 shape.
awk '
  /^  coverage:/ { inc = 1 }
  inc && /run: \.\/scripts\/ci-build-plugins\.sh/ {
    print "        run: |"
    print "          cargo build -p trovato_blog \\"
    print "            --target wasm32-wasip1 --release"
    print "          cp target/wasm32-wasip1/release/trovato_blog.wasm plugins/trovato_blog/"
    inc = 0; next
  }
  { print }' "$ROOT/.github/workflows/ci.yml" > "$WORK/own-list.yml"
expect "parity: a Coverage job with its own list fails" 1 parity "$WORK/own-list.yml"
if grep -q "job 'coverage' does not build" "$WORK/out" && grep -q "keeps its own list" "$WORK/out"; then
  pass "parity: it names the job and the hand-kept list"
else
  fail "parity: it names the job and the hand-kept list"; sed 's/^/     /' "$WORK/out"
fi
# A Test job that does not call the script at all.
awk '/^  test:/ { inc = 1 } /^  coverage:/ { inc = 0 }
  inc && /run: \.\/scripts\/ci-build-plugins\.sh/ { print "        run: echo no plugins"; next }
  { print }' "$ROOT/.github/workflows/ci.yml" > "$WORK/no-call.yml"
expect "parity: a Test job that skips the script fails" 1 parity "$WORK/no-call.yml"
# A job building a set it reads from a file is its own business.
cat "$ROOT/.github/workflows/ci.yml" > "$WORK/from-file.yml"
cat >> "$WORK/from-file.yml" <<'YML'
  extra:
    steps:
      - run: |
          PLUGINS=$(cat list.txt)
          cargo build $(printf -- '-p %s ' $PLUGINS) --target wasm32-wasip1 --release
          for p in $PLUGINS; do cp "target/wasm32-wasip1/release/$p.wasm" "plugins/$p/"; done
YML
expect "parity: a build whose list is read from a file passes" 0 parity "$WORK/from-file.yml"

echo
if [ "$FAILS" -ne 0 ]; then
  echo "$FAILS check(s) failed"
  exit 1
fi
echo "all checks passed"
