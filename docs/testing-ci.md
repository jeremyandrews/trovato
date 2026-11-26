# Testing in CI

How the CI workflow (`.github/workflows/ci.yml`) runs the tests, where a new
test lands, and the time budget that keeps a pull request's wait short.

## How CI is split

Every job starts at once, so the slowest job is how long a pull request waits.
The test work is in three jobs.

**Test**, three shards. Each shard is one runner with its own Postgres and
Redis. Every shard runs:

1. One part of the unit and doc tests, chosen so the three come out even:

   | Shard | Runs | Runner |
   | --- | --- | --- |
   | 1 | the migration compatibility rule, then every library unit test (`--lib`) | nextest |
   | 2 | every binary target's unit tests (`--bins`) | nextest |
   | 3 | every doc test (`--doc`) | cargo |

2. Its share of the integration test targets (`scripts/ci-test-shard.sh`),
   twice, against the same database, then the guards that the second pass did
   not grow the shared fixture and that the shared conference fixture is unique.

3. The slow test budget (below).

**Cluster** builds the release image and runs `cluster_test` against two
kernels behind a proxy, under nextest.

**Coverage** is report only, so it is not on the pull request path. It runs on
every push to `main` and by hand (Actions, CI, "Run workflow").

### Unit tests under nextest, integration targets under libtest

Unit tests build nothing on start, so nextest's one process per test costs
nothing and buys isolation: no test sees another's environment variables or
globals, and they all run at once.

The integration targets are different. Every process builds its own `TestApp`:
the WASM plugins compiled, the templates parsed, the database checked. Measured
on `front_page_test`, its fifteen tests take 3.6 s in one process and 26 s as
fifteen processes, 45 CPU-seconds against 3.3. A four core runner cannot win
back thirteen times the work, so these targets run under libtest: one target at
a time, its tests in parallel inside it. That is also what the targets' own
locks (the `static` mutexes at the top of many files) are written for, and what
a local `cargo test` does.

The integration targets of one shard build and run in a single `cargo test`
invocation, so a target in another package (`trovato-mcp`) does not rebuild the
kernel under different features.

### Four tests at a time inside a target

`scripts/ci-test-shard.sh` sets `RUST_TEST_THREADS=4`, the runner's core
count. Every `TestApp` opens a pool of up to 25 connections against a Postgres
allowing 100, and with more threads the busiest targets (`config_storage_test`)
time out waiting for one. Set the variable to run more or fewer.

Tests that share site-wide state take a `static` lock, and two things about
parallel tests have to be right for that lock to work:

- **Hold the lock through cleanup.** `common::run_test` runs the cleanups a
  test registered with `defer_cleanup` after the test body returns, so a guard
  bound with `let _guard = LOCK.lock().await;` is released before the restore
  runs, and the next test sees the unrestored state. Pass the guard to
  `common::hold_until_cleanup(LOCK.lock().await)` instead: it is dropped after
  the last cleanup. (A lock taken by a helper that a test calls more than once
  keeps `let _guard`; it is released before the helper is called again.)
- **Invalidate the caches your raw writes skip.** A model-level write (a menu
  link, a tile) does not bump the process's caches the way the admin routes
  do, and a concurrent test's request refills the cache in between. Invalidate
  it (`menu::navigation::invalidate_config_menus()`), or wait for the
  generations with `common::settle_cache_generations()`.
- **A restore must outlast the bumps it follows.** A save through an admin
  route bumps a cache kind, and every server acts on the bump at its next
  poll; for the themes cache that means reloading the active theme from the
  database. A cleanup that deletes the stored setting and releases the lock at
  once lets that reload arrive in the next test's turn, over whatever it has
  just set. End such a cleanup with `common::settle_cache_generations().await`
  (`restore_stock_on_exit` in `appearance_test`).
- **An invalidation is not enough for a cache read back at once.** A render
  that began reading the menus before your write can fill the cache after
  your invalidation, without your link, for the cache's whole TTL. A test
  that writes a menu link and then reads it off a page holds a lock that the
  other renders in its file respect (`MENUS` in `nav_landmarks_test`,
  `gather_permission_test` and `language_navigation_test`, `MENU_CACHE` in
  `menu_tree_test`).

A test that captures one request's events with `.with_subscriber(..)` must call
`common::scoped_subscribers_see_every_callsite()` first. Without it a
concurrent test's request can fix a callsite's interest at "never", and the
capture misses those events for the rest of the binary (see the function's
comment).

A table a plugin creates on first use, from more than one test, is created
under a Postgres advisory lock: `CREATE TABLE IF NOT EXISTS` is not safe
against itself.

### Dependencies optimized

`Cargo.toml` builds argon2 and Cranelift (and what they need) at
`opt-level = 3` in the dev profile. Most of a test's CPU is in those two, and
this cuts it about three times. Optimizing every dependency cuts it further but
triples a cold dependency build in CI; the comment there has the numbers. The
workspace's own crates still build unoptimized.

The Test job also builds with line tables instead of full debug info
(`CARGO_PROFILE_DEV_DEBUG`): backtraces keep their files and lines, and the
build takes a quarter less CPU. A local build is unchanged.

## Where a new test goes

Nowhere in particular: there is no list to add it to.

- A unit test (`#[test]` in `src/`) runs in the shard that runs its target kind.
- A doc test runs in shard 3.
- A new file in `crates/*/tests/` is found by `cargo metadata` and given to one
  shard. Until its time is recorded in `.config/ci-test-weights.txt` it is
  scheduled at the `@default` weight.

An integration target must pass alone on a fresh database: the shard it lands
in, and which targets share its database, change whenever the weights do. If it
needs a plugin's tables, call `common::ensure_plugin_tables`; if it needs a
plugin enabled, enable it; never rely on a target that sorts before it.

## The budgets

| What | Budget | Where |
| --- | --- | --- |
| One test | 30 s, else on the allowlist with its time and reason | `scripts/ci-slow-tests.sh`, `.config/slow-tests.txt` |
| An allowlisted test | twice its recorded time | the same |
| A Test shard | 30 minutes: expected 20, plus half | `timeout-minutes` on the Test job |
| Cluster | 40 minutes: expected 27 (a pull request that changes `Cargo.lock`: 24 and 30 in two runs), plus half | `timeout-minutes` on the Cluster job |

nextest marks a unit test SLOW in the log after 10 s (`slow-timeout` in the
`ci` profile, `.config/nextest.toml`). That is a warning; the 30 s line is the
gate, and it applies to each test:

- unit tests, from nextest's JUnit report;
- integration tests, from libtest's `--report-time`, which a stable toolchain
  accepts when the test process sees `RUSTC_BOOTSTRAP`. `scripts/ci-test-shard.sh`
  sets it through a runner, for the test process only: build scripts read it to
  switch on nightly-only code, so setting it for the build would change what is
  built;
- doc tests, one crate at a time (`doc::<crate>`), because rustdoc rejects
  `--report-time` on a stable toolchain.

libtest times a test from its start, so a test that waits for a lock is charged
the wait. That is why the allowlist takes a whole target as well as a test: a
target entry covers each of its tests, and is for a target whose tests queue on
one lock by design (`plugin_queue_v2_test`), where the last test in the queue
carries nearly the whole target's time.

An allowlist line is `name | measured seconds | reason`. The name is what the
guard prints: `<binary id> <test name>` for a unit test, `<package>::<target>
<test name>` for an integration test, `doc::<crate>` for doc tests, or a bare
`<package>::<target>` for a target as above.

The budget step of each shard passes every report that shard should have, and
fails on one that is missing or empty. `scripts/ci-scripts-test.sh` tests the
parser (`scripts/ci-test-timings.awk`), the guard and the shard script's target
checks against made-up output; it runs first on shard 1 and in
`scripts/pre-commit-check.sh`.

## Remeasuring

Each shard uploads its timings as the `test-timings-shard-N` artifact: one
line per integration target per pass (`pass-N.txt`), one per integration test
(`pass-N.tests`), the doc tests' (`doc.txt`), and the nextest JUnit report. To
rebalance, take a recent run's artifacts and set each target's weight to the
sum of its two passes; set the `@shard` lines to what each shard's unit or doc
step took, and `@per_target` to what building one test binary costs (pass 1's
step time minus its targets' own times, over the number of targets). The
partition is longest first onto the least loaded shard, so the weights only
need to be right relative to each other.

The timeouts are budgets for a hang, not for a slow run: if the expected times
move, move them too, to the new expected time plus half, and say so in their
comments. A cold dependency cache (a new toolchain, a change to the dev
profile) takes the Test shards close to their 30 minutes; if one is cancelled,
raise the limit for that run so the cache can save, rather than retrying,
because a cancelled job saves nothing and the next run is as cold.

Locally, with Postgres and Redis up (`docker compose up -d postgres redis`):

```sh
createdb trovato_test   # once; the harness migrates it
export DATABASE_URL=postgres://trovato:trovato@localhost:5432/trovato_test
export REDIS_URL=redis://127.0.0.1:6379

# one shard, as CI runs it, with its timings
time SHARD=1 TOTAL_SHARDS=3 TIMINGS_FILE=/tmp/shard1.txt ./scripts/ci-test-shard.sh
./scripts/ci-slow-tests.sh /tmp/shard1.txt

# the unit tests, with the JUnit report the guard reads
cargo nextest run --workspace --lib --bins --profile ci
./scripts/ci-slow-tests.sh target/nextest/ci/junit.xml

# every test that exists, by name: nothing may disappear from these
cargo nextest list --workspace --run-ignored all --message-format oneline
cargo test --all --doc -- --list
```
