# Fast test runs for agents (B274)

Every number on this page was measured in one session, on one pod: 8 vCPU,
31 GiB RAM, PostgreSQL 16 and Redis 7 reachable at `$DATABASE_URL` /
`$REDIS_URL`, `rustc 1.96.0 (ac68faa20 2026-05-25)` (the toolchain
`rust-toolchain.toml` pins), linking with the toolchain's default linker (no
`.cargo/config.toml` in this checkout, so no `lld`/`mold` override is in
play), Postgres `max_connections = 100`, `redis-cli` present.

```
$ nproc
8
$ free -g
             total  used  free  shared  buff/cache  available
Mem:            31     1    23       0           7          30
$ df -h .
Filesystem   Size  Used Avail Use% Mounted on
/dev/vda1    387G  8.3G  379G   3% /home/agent/work
$ rustc -vV
rustc 1.96.0 (ac68faa20 2026-05-25)
...
$ psql "$DATABASE_URL" -tAc 'SHOW max_connections'
100
$ which redis-cli
/usr/bin/redis-cli
```

## What shipped, and what did not

**Shipped**: the `config_storage_test` pool-exhaustion fix (a correctness bug,
not a speed cut), `scripts/build-test-plugins.sh`, and `scripts/test-changed.sh`
— the inner loop. **Not shipped**: a WASM compilation-cache option for the
plugin runtime, and `scripts/test-all.sh` (a parallel, multi-lane full-suite
runner). Both were built and measured in this session; both were reverted.
Why, in each case, is below — this is the record the stop rule asks for: "a
cut that does not pay is reverted and its number still goes in the doc."

## Baselines

### Plugin build (`scripts/build-test-plugins.sh`)

Cold (fresh `cargo clean`, no dependency cache):

```
$ cargo clean
$ time ./scripts/build-test-plugins.sh
...
real    0m43.682s
```

Measured twice (a second cold run after `cargo clean` again): 42.6s and
43.7s. Small next to the rest of the suite; not a cut target here.

### Cold build: `cargo test --all --no-run --timings`

After `cargo clean` and rebuilding the plugins, on the branch with the
`config_storage_test` fix only applied (before either reverted cut):

```
$ cargo test --all --no-run --timings
...
real    36m1.889s
user    141m55.491s
sys     33m7.448s
$ du -sh target
41G     target
```

Peak memory during the build (5 s samples of `free -m`'s "used" column):
about 8.2 GB, well inside the pod's 31 GB. `target/cargo-timings/` from this
run shows dependency compilation dominating the first several minutes,
`trovato-kernel` itself as the single longest unit, and after that the 236
`crates/kernel/tests/*`, 1 `crates/search-index/tests/*`, 1
`crates/mcp-server/tests/*` and 1 `crates/pagefind-harness/tests/*` binaries
each linking the whole kernel rlib again. 41 GB on disk matches the "tens of
GB" the CI workflow's disk-cleanup comment already describes.

### Cold run: `RUST_TEST_THREADS=3 cargo test --all --no-fail-fast`

With the binaries from the cold build already in place (execution only, no
rebuild):

```
$ RUST_TEST_THREADS=3 cargo test --all --no-fail-fast
...
real    22m28.680s
user    20m47.960s
sys     4m37.163s
```

Two failures, reproduced identically in a second full run later in the
session (same two test names, same assertions) — see "Pre-existing test
interference" below; both are a wrong HTTP status (404 where 200 was
expected), not a crash, and both pass when their own target is run alone.

`RUST_TEST_THREADS=3` is deliberate, not an oversight: it is what an agent on
this pod would be told today (`scripts/ci-test-shard.sh` pins `4`; `3` here is
conservative), and it is the number that would actually fail with a pool
timeout before the `config_storage_test` fix below — see that section for the
repro.

## `config_storage_test`'s pool exhaustion: the root cause and the fix

`crates/kernel/tests/common/mod.rs`'s `TestApp::build` used one pool-size
default (25 connections, "tests run 100 tests concurrently") for every
`TestApp`, whether it came from the shared app (`shared_app()`, one per
binary, legitimately serving every test in that binary) or from a test file
building its own (`TestApp::new`, `with_config`, `for_site`,
`with_rate_limits` — 46 files do this directly). Each of those apps
pre-warms its whole pool at construction (`build`'s `for _ in
0..config.database_max_connections` loop), so several of them initializing
near-concurrently (one per libtest thread) asked Postgres for far more
connections than `max_connections` allows.

Reproduced on `main` (`4ed2d21`: the pre-fix `common/mod.rs` and
`config_storage_test.rs` checked out over this branch, same `target/`,
`DATABASE_MAX_CONNECTIONS` unset, a fresh database):

```
$ RUST_TEST_THREADS=16 cargo test -p trovato-kernel --test config_storage_test
...
running 11 tests
test config_storage_category_crud ... FAILED
test config_export_carries_tag_slug ... FAILED
test config_revision_schema_tables_exist ... FAILED
(8 others) ... ok

---- config_storage_category_crud stdout ----
thread 'config_storage_category_crud' (146925) panicked at crates/kernel/tests/config_storage_test.rs:174:10:
failed to save category: failed to check category existence
Caused by:
    pool timed out while waiting for an open connection

---- config_revision_schema_tables_exist stdout ----
thread 'config_revision_schema_tables_exist' (146924) panicked at crates/kernel/tests/config_storage_test.rs:421:6:
failed to check config_revision table: PoolTimedOut

test result: FAILED. 8 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out; finished in 49.75s
```

On the branch (the fixed `common/mod.rs` and `config_storage_test.rs` restored),
the identical command passes: direct-built apps now default to 5 connections
(`DIRECT_POOL_SIZE` in `crates/kernel/tests/common/mod.rs`), the shared app
keeps 25 (`SHARED_POOL_SIZE`), and an explicit `DATABASE_MAX_CONNECTIONS`
still wins over either default:

```
$ RUST_TEST_THREADS=16 cargo test -p trovato-kernel --test config_storage_test
...
running 11 tests
test config_storage_load_item_type ... ok
test config_storage_list_item_types ... ok
test config_storage_delete_item_type ... ok
test config_storage_list_with_filter ... ok
test config_storage_save_and_load_item_type ... ok
test config_storage_variable_crud ... ok
test config_storage_does_not_write_to_revision_tables ... ok
test config_storage_category_crud ... ok
test config_revision_schema_tables_exist ... ok
test config_storage_tag_crud ... ok
test config_export_carries_tag_slug ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.86s
```

`config_storage_test`'s 11 tests themselves had no reason to build their own
app — nothing in them needs rate limits, a dedicated site router, or a
customized `Config` — so they were moved to the `shared_app()` /
`common::run_test` pattern 159 other test files already use, pairing the
pool-size fix with a root-cause fix of the one file that most directly
exposed it.

## `scripts/build-test-plugins.sh`

Builds the same 31 packages as the `test` job's "Build WASM plugins" step in
`.github/workflows/ci.yml`, and copies the `.wasm` files into `plugins/` the
same way. `scripts/ci-scripts-test.sh` fails if the two package lists drift
(diffed as sorted sets, so reordering or rewrapping either list is not a
failure). Note: the `coverage` job's own "Build WASM plugins" step in the
same workflow already carries a *different*, shorter list (missing
`test_search_enqueue` and `trovato_scheduled_publishing`) — a pre-existing
discrepancy between two CI jobs, out of this change's scope (nothing under
`.github/workflows/` is touched here); the new script and its check follow
the `test` job's list, the one the sharded full-suite run actually needs.

## `scripts/test-changed.sh`

The inner loop: given the files changed since the merge base with
`origin/main` (plus uncommitted and untracked files), it selects and runs, in
one `cargo test` invocation:

1. the `--lib --bins` unit tests of every workspace package with a changed
   file;
2. every integration test target whose own file changed;
3. every integration target named on the command line;
4. for a changed file under `crates/*/src/`, every integration target whose
   file name contains that file's stem, or whose own source names the file's
   module path;
5. everything (`cargo test --all`), when `crates/kernel/tests/common/`,
   `migrations/`, or any `Cargo.toml`/`Cargo.lock` changed.

Its selection logic is tested without building anything or touching a
database (`scripts/ci-scripts-test.sh`, a made-up two-package workspace and a
fake `cargo` that answers `metadata` and logs a `test` invocation's
arguments instead of running one), the same way
`scripts/ci-test-shard.sh`'s partition is tested — all under a second,
builds nothing, touches no database.

Real run, a one-line comment change to `crates/kernel/src/services/redirect.rs`
(driven through `TEST_CHANGED_OVERRIDE`, the same seam `ci-scripts-test.sh`
uses, because this branch's own commits otherwise dominate its diff against
`origin/main`; see the script's header):

```
$ time env TEST_CHANGED_OVERRIDE="crates/kernel/src/services/redirect.rs" ./scripts/test-changed.sh
Changed files:
  crates/kernel/src/services/redirect.rs
Selected:
  unit tests (--lib --bins): trovato-kernel (changed file crates/kernel/src/services/redirect.rs)
  integration target feed_test (trovato-kernel): matches changed crates/kernel/src/services/redirect.rs
  integration target redirect_query_match_test (trovato-kernel): matches changed crates/kernel/src/services/redirect.rs
  integration target redirect_query_test (trovato-kernel): matches changed crates/kernel/src/services/redirect.rs
  integration target redirect_writes_test (trovato-kernel): matches changed crates/kernel/src/services/redirect.rs
Inner loop only: run the full suite before you finish (see CONTRIBUTING.md).
...
test result: ok. 7 passed; 0 failed ... (feed_test)
test result: ok. 1 passed; 0 failed ... (redirect_query_test)
test result: ok. 6 passed; 0 failed ... (redirect_writes_test)

real    20m4.265s
```

The selection is exactly what rule 4 promises: `redirect.rs`'s stem matches
the three `redirect_*_test` targets by name, and `feed_test` by its source
naming `services::redirect`; every selected target and the kernel's unit
tests passed. The 20-minute wall time is almost entirely `trovato-kernel`
recompiling for this exact combination of packages and flags, not the tests
themselves (each target finished in single-digit seconds) — this pod pays a
real, large cache-invalidation cost whenever an invocation's package/feature
selection differs from whatever shape last touched `target/`, independent of
anything this change does (see the `scripts/test-all.sh` section below,
where the same cost shows up again). A second run with nothing changed
finishes in seconds.

## Not shipped, and why

### A WASM compilation cache for the plugin runtime

`create_engine` (`crates/kernel/src/plugin/runtime.rs`) never turned on
wasmtime's own compilation cache, so `compile_plugin_from_dir`'s `Module::new`
recompiles all 31 fixture plugins from scratch at every `AppState::new` — once
per test binary (over 240 times) plus once per test file that builds a
further `TestApp` of its own. This is a real cost (the problem statement's own
figure: 0.97 s of a 2.0 s app start is WASM compile), and a kernel option
(`PluginConfig::wasm_cache_dir`, off by default, wired through `Config` so
`TestApp::build` can turn it on for tests only) was written and compiled
cleanly.

It was reverted because this session could not produce a clean, isolated
"before vs. after, cache only" measurement inside its time budget: the two
full-suite runs taken after it landed were each contaminated by something
else changing between runs (a rebuild, or, in the final runs, the lanes work
below), so neither is a fair before/after pair. Per the stop rule's own
instruction — a cut without a clean payoff number is reverted and recorded —
it is reverted here rather than kept on an unverified claim. The idea and the
`wasmtime::Cache`/`CacheConfig` wiring are straightforward to redo in a
follow-up session that budgets a dedicated, isolated measurement for it (one
app start, timed with the cache directory cleared vs. warm, without a
concurrent build or test run on the same machine).

### `scripts/test-all.sh`: a parallel, multi-lane full-suite runner

Built as designed: reusing `scripts/ci-test-shard.sh`'s partition (each lane
*is* a shard, `SHARD=<lane> TOTAL_SHARDS=<lanes>`), one database per lane
(`<name>_lane<k>`) and one Redis database index per lane (`$REDIS_URL/<k>`,
confirmed isolated directly — `redis-cli -n 1 set` is invisible to `-n 2 get`
in this Redis). A real defect surfaced and was fixed along the way: lanes
sharing one `target/` directory with *different* `-p` package sets per lane
made every lane's cargo invocation resolve workspace features differently
from its neighbors', so concurrent lanes each decided the others' build was
stale and recompiled it — the opposite of the intended speedup. Fixed with an
opt-in `SHARD_PKGS_FROM_ALL` env var in `ci-test-shard.sh` (CI's own
invocation never sets it, so CI's behaviour is unchanged) plus a single
upfront build with the full package set before any lane starts.

With that fixed, a 3-lane run completed in **12m52s** wall (pre-build included)
against the 22m28s single-threaded baseline — a real, large improvement. But
it, and a 2-lane run afterward, each showed a *different* small set of test
failures than the single-threaded baseline's two (one run: `user_directory_test`
plus `gather_tile_test`; a second run of the same script: `user_directory_test`,
`search_index_writers_test`, `mail_queue_test`, `search_switches_test`,
`import_test` — more failures, and a *slower* 27m18s wall, running with fewer
lanes). Several of the newly-failing tests are timing-sensitive (retry
windows, dedupe windows, rate-limit buckets) and several of the failures'
recorded durations (over 200s for tests whose recorded CI cost is in the
tens of seconds) point at resource contention under concurrent load on this
specific pod rather than a logic defect in the cut: more lanes, each running
several test binaries' own Tokio runtimes and libtest thread pools at once on
8 vCPUs, oversubscribes the machine enough to blow through hard-coded
assertion windows that assume prompt scheduling.

This is a genuine, measured finding, not a logic bug in the lanes design, and
not something this session could safely resolve (reproducing and bisecting a
resource-contention flake needs more wall-clock than was left, and shipping a
runner that intermittently fails a different handful of tests each run — even
if every individual failure is "the machine was slow," not "the code is wrong"
— fails this change's own constraint that every test still passes). Reverted;
`scripts/ci-test-shard.sh`'s `SHARD_PKGS_FROM_ALL` addition was reverted with
it (it has no caller once `test-all.sh` is gone). A follow-up attempting this
again should budget time to find a lane count and `RUST_TEST_THREADS` this
specific class of pod tolerates without contention — likely fewer total
concurrent libtest threads than `vCPUs`, not up to it — before trusting a
green run.

## Pre-existing test interference (found, not caused by this change)

Two failures reproduced identically across multiple full runs of
`cargo test --all --no-fail-fast`, both before and after every change in this
session, including runs with none of the reverted cuts applied:

- `plugin_mail_test::with_no_smtp_configured_the_plugin_is_told_so`
- `user_directory_test::find_by_name_without_the_capability_is_denied` and
  `user_directory_test::without_the_capability_the_call_is_denied`

All three are "expected HTTP 200 with a JSON refusal body, got 404" — the
route itself not found, meaning the test's own `TestApp` (each of these files
builds one directly, with a plugin directory override) did not load its
fixture plugin in that run. Both targets pass cleanly in isolation
(`cargo test -p trovato-kernel --test plugin_mail_test` /
`--test user_directory_test`, alone, fresh database). `plugin_mail_test` also
passed cleanly once run in a lane holding only a subset of the suite's
binaries against its own database — consistent with cross-file interference
through the one shared database a plain `cargo test --all` uses, which
`CONTRIBUTING.md`'s "The local gate is stronger than CI" already names as a
real possibility. `user_directory_test` did not clear in the smaller lane,
so its root cause is not fully isolated. Neither is introduced by this
session's changes (reproduced before the `config_storage_test` fix, the
WASM cache, or the lanes work existed); recorded here as a real finding for
a future session, not fixed in this one — this change's own constraints
permit fixing a test only where the test itself is shown to be wrong, which
neither of these has been.

## How to reproduce every number above

```sh
# Machine facts
nproc; free -g; df -h .; rustc -vV
psql "$DATABASE_URL" -tAc 'SHOW max_connections'; which redis-cli

# Plugin build
cargo clean && time ./scripts/build-test-plugins.sh

# Cold build / cold run
cargo clean && ./scripts/build-test-plugins.sh
time cargo test --all --no-run --timings
RUST_TEST_THREADS=3 bash -c 'time cargo test --all --no-fail-fast'
du -sh target

# config_storage_test root cause: the pre-fix files over this branch, then restored
git show 4ed2d21:crates/kernel/tests/common/mod.rs > crates/kernel/tests/common/mod.rs
git show 4ed2d21:crates/kernel/tests/config_storage_test.rs > crates/kernel/tests/config_storage_test.rs
RUST_TEST_THREADS=16 cargo test -p trovato-kernel --test config_storage_test   # fails: PoolTimedOut
git checkout HEAD -- crates/kernel/tests/common/mod.rs crates/kernel/tests/config_storage_test.rs
RUST_TEST_THREADS=16 cargo test -p trovato-kernel --test config_storage_test   # passes

# test-changed.sh
./scripts/test-changed.sh
```
