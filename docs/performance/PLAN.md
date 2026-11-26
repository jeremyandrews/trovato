# Performance plan: targets, method, guardrails, first fixes

Trovato's public site states performance and scalability as a core tenet. This
plan gives that claim numbers, one way to measure them, rules that keep the
kernel simple while chasing them, a reading of where today's time goes, and a
ranked list of the first fixes. Every file and function named below was read
on `origin/main` while writing this plan.

Two terms from the existing performance tests are used throughout, in their
existing meaning (`docs/performance-tests.md`): a **cold** request is the first
one after every in-process cache is cleared and every non-session Redis key is
gone; a **warm** request is one immediately preceded by the same request once.
`scripts/bench.sh` approximates warm with its warmup requests before each
timed run; it has no isolated cold mode yet (see Method and First fix 6).

## Targets

| Scenario | p50 | p99 | Throughput (single node) | Why |
|---|---|---|---|---|
| Anonymous warm item page | 8 ms | 25 ms | 400 req/s | Measured at 10 to 12 ms median at 20,000,000 posts (`docs/BACKLOG.md` BL-166, BL-167); the item row is one primary-key lookup (`ItemService::load`, `crates/kernel/src/content/item_service.rs:1119`, backed by `Item::find_by_id`, `crates/kernel/src/models/item.rs:198`), so the number should hold at every item count. |
| Anonymous cold item page | 15 ms | 50 ms | 150 req/s | `tests/fixtures/performance_counts.toml` shows `item.anonymous.cold` at 21 queries against `item.anonymous.warm`'s 13, all 8 extra ones single indexed point lookups (cache fills), so the cold penalty should stay a near-constant add-on, not grow with item count. |
| Gather listing page (a category or tag page) | 15 ms | 60 ms at 10 thousand and 1 million items; 150 ms at 10 million | 150 req/s | Measured front and feed pages at 40 to 43 ms median and a tag page at 214 ms median at 20,000,000 posts, 4 to 20 times the item page (BL-166, BL-167); a listing reads an index range the same way an item page reads one row, so the gap should close toward 2x, not stay where it is. |
| Authenticated editor page (admin content list) | 20 ms | 80 ms | 100 req/s | `performance_counts.toml`'s `admin_content.admin.cold`/`.warm` (17 and 6 queries) are dominated by fixed per-request costs, including 9 duplicate `select * from users where id = $1` calls on one page (see Where the time goes); First fix 1 removes most of that, after which the remaining query is index-backed and should not grow with item count. |
| Item save (authenticated editor) | 40 ms | 150 ms | 50 req/s | The write itself is one transaction of 3 to 6 statements (`Item::create_with_file_refs`, `models/item.rs:306`; `update_with_file_refs`, `models/item.rs:405`), plus roughly 8 to 12 further round trips in `ItemService::update_moderated` (`content/item_service.rs:1575`) for access, alias, audit and cache work; none of this scales with total item count. |

Scale points: 10 thousand, 1 million and 10 million items are the three to
generate and measure at (`trovato generate`, below). 20 fields per type change
nothing in these numbers, since fields are read as one JSONB column regardless
of key count. 200 fields per type has no target yet for a plain field filter
or sort: `GatherQueryBuilder::field_expr` (`crates/kernel/src/gather/query_builder.rs:806`)
routes every `fields.X` comparison through a `->`/`->>` path expression, which
the schema's `idx_item_fields` GIN index does not accelerate (GIN on JSONB
speeds the `@>` containment operator, not path extraction), confirmed by
`docs/design/Design-Query-Engine.md`'s own note on the limitation and by
finding no `ExpressionIndex` code in the kernel. That scale point waits on the
indexed field filter work landing and being measured on its own; this plan
does not set a number it cannot back with a reading of working code.
Concurrent users: hold throughput within 20 percent of its single-client p99
from 8 to 100 concurrent clients against today's pool (`DATABASE_MAX_CONNECTIONS`
default 10, `crates/kernel/src/config.rs:779`); from 8 to 500 once First fix 5
lands; untested above that.

Every number is chosen to hold flat as item count rises, because the schema
already backs every lookup this plan measures with a primary key or a btree or
GIN index (`item.id` primary key, `idx_item_fields` GIN, `idx_item_search` GIN,
per the items migration). A target allowed to grow with item count would
concede "fast at any scale" before the first fix ships.

## Method

- **Harness.** `scripts/bench.sh` wraps the `bench` binary of
  `trovato-loadtest` (`benchmarks/load-test/src/bin/bench.rs`). For each named
  page it runs `--warmup` untimed requests, then `--requests` timed ones from
  `--concurrency` workers, `--runs` times, and reports the median over the runs
  of the median and the p99, with every run's figures beside it. `--save`
  writes `benchmarks/baseline.json`; `--baseline` compares a new run against
  it. This already covers the read scenarios in Targets; it has no
  cold-request mode yet (First fix 6).
- **Concurrent users and writes.** `benchmarks/load-test/src/main.rs`'s
  `loadtest` binary is a mixed-workload tool, but its "write" requests just hit
  `/health`, "for now, actual writes need auth" (its own comment, line 291).
  Goose, the Rust load testing tool, is the preferred load generator for a
  rewrite of this tool: scenarios that log in, hold a session and exercise
  real write routes, so item-save and authenticated-editor targets can be
  checked end to end (First fix 7).
- **Dataset generator.** `trovato generate --items N [--comments C] [--tags T]
  [--users U] --seed S --admin-password PW` (`crates/kernel/src/generate.rs`)
  writes a deterministic site; the same arguments always write the same rows,
  and a database already holding exactly that generated site is left alone.
  Generate once per scale point (10 thousand, 1 million, 10 million items)
  and keep the database.
- **Hardware.** One named, fixed host. Record its CPU model, core count and
  RAM with every report, as `docs/benchmarks/EPYC-BENCHMARK-INSTRUCTIONS.md`'s
  "Collecting System Info" step already asks (`lscpu`, `uname -a`, `free -h`,
  `rustc --version`, `cargo --version`). Every number marked measured in this
  plan came from either a Mac Studio (`docs/BACKLOG.md`'s BG14 rows) or the
  EPYC host that document targets, and the two are not comparable to each
  other, which is why one fixed host is the rule from here on.
- **Warmup, run length, what is recorded.** `bench`'s defaults (200 warmup
  requests, 2,000 timed, 8 workers, 3 runs) are the floor; raise `--requests`
  at the larger scale points if the p99 is noisy. Each report (median, p99,
  error count, every run's figures) is JSON under `benchmarks/`, named for the
  scale point and scenario, beside the system info file.
- **Never on a pull request.** `scripts/bench.sh`'s own comment says timings
  belong to the machine they were taken on and are run by hand; a shared CI
  runner is too noisy for them and has no generated site.
- **Query count fixtures stay the cheap regression gate in CI.**
  `crates/kernel/tests/performance_test.rs` and
  `tests/fixtures/performance_counts.toml` already run on every pull request
  and fail on the exact statement a change added or removed, catching a query
  added in a loop at review time, long before a timing benchmark would run.

## Guardrails (KISS and maintainability)

- Every performance change ships with before and after numbers from Method, on
  the same host, the same scale point, the same scenario.
- No cache layer, index, materialized view or concurrency trick ships without
  a measured win that simpler code, an index, a cheaper query or a bound,
  cannot already reach. Try the plain fix first and measure it before adding a
  moving part.
- No new runtime dependency (Redis beyond what already runs, a search engine,
  a CDN) is required to hit the Targets. PostgreSQL alone, with the schema's
  existing indexes, must meet them. Redis already backs sessions and the
  Redis-tier cache (`crates/kernel/src/cache/mod.rs`); the rule is no new
  infrastructure requirement, not no Redis.
- Correctness and security are never traded for speed. A faster path that
  skips an access check, a permission check or a transaction boundary is
  rejected regardless of its numbers.
- A performance change that makes the code harder to read says why the win is
  worth it in its pull request, next to the before and after numbers.

## Where the time goes, from the code

| Scenario | Cost found in the code | Status |
|---|---|---|
| Item page | 21 queries cold, 13 warm (`item.anonymous.cold`/`.warm`, `performance_counts.toml`); item row is one cached point lookup, plus a recursive comment-tree CTE and 4 tile region queries. Latency 10 to 12 ms median at 20,000,000 posts. | Measured |
| Gather listing (category or tag page) | At least 2 top-level `SELECT`s per view even when nothing is denied: a `jsonb_build_object` access-candidate scan up to `max_scan` (1,000 rows) wide, then a `row_to_json` window, both in `fetch_access_filtered` (`crates/kernel/src/gather/gather_service.rs:1472`), with `fetch_factor=2`, `max_scan=1000`, `max_backfill_rounds=6` (`gather/access.rs:72`). Query count measured in fixtures (`category.anonymous.cold`: 19 queries); its link to the 214 ms tag-page latency (BL-166) is plausible but not yet traced. | Query count measured; latency link inferred |
| Search | The pager's count query scans every match: `crates/kernel/src/search/mod.rs:258-290`. 6 to 9 seconds for a one-word query at 20,000,000 posts. | Measured (BL-165) |
| Authenticated session, every request | `get_user_context` and `anonymous_user_context` (`crates/kernel/src/routes/helpers.rs`) call `models::User::find_by_id` directly, bypassing `UserService::find_by_id`'s cache (`services/user.rs:131`). 9 duplicate identical statements on one admin page (`admin_content.admin.cold`, `select:users:37216782` count 9). Permissions add up to 3 more queries on a 60-second cache miss (`permissions.rs` `PermissionService::user_access`). | Measured |
| Item save | 3 to 6 statements in one transaction (`Item::create_with_file_refs`/`update_with_file_refs`), plus roughly 8 to 12 further round trips outside it in `ItemService::update_moderated` for access, moderation, alias, audit and cache work. | Transaction shape measured in fixtures for related write paths; the full round-trip count is read from the function, not yet in a fixture |
| WASM plugin calls | None on published content on the live stage (no plugin calls in the item, front or category fixtures). When a tap runs, instantiation is pooled at about 5 microseconds (`crates/kernel/src/plugin/runtime.rs:863`) against about 50 unpooled, and a call crosses the boundary as full JSON, about 23 to 27 microseconds (`tap/dispatcher.rs` `call_export_function`, numbers from the phase0 benchmark committed in `docs/benchmarks/EPYC-BENCHMARK-INSTRUCTIONS.md`). | Measured |
| Database pool | One `PgPool`, `max_connections` default 10 (`crates/kernel/src/db.rs` `create_pool`, `config.rs:779`), shared by every web request, the cron drain loop and up to 4 concurrent queue jobs per plugin queue (`cron/mod.rs` `QUEUE_CONCURRENCY_CAP`). | Default value measured; contention under load inferred, not yet benchmarked |
| Cache invalidation | A write bumps a Redis generation counter; a local watcher polls every 250 ms, and a cross-server wait is defined as 4 polls, 1,000 ms (`cache/generation.rs` `POLL_INTERVAL`, `SETTLE`). A read on another server can see stale data for up to that long after a write. | Measured (code constants) |

## First fixes, ranked

1. **Route every request's user load through the existing cache.** `routes/helpers.rs`'s `get_user_context` and `anonymous_user_context`, plus `routes/recovery.rs:217` and `routes/webauthn.rs:587`, call `models::User::find_by_id` directly instead of `UserService::find_by_id` (`services/user.rs:131`). Effect: removes up to 9 duplicate queries per request (measured). Risk: none, same row. Proof: `performance_counts.toml`'s `select:users:*` counts drop.
2. **Find and fix the feed's 500s under load (BL-164).** `/blog/feed.xml` fails 120 to 274 of 2,000 requests in one run of three, nothing logged, not yet root caused. Effect: unblocks measuring feed throughput at all. Risk: unknown until investigated. Proof: three back-to-back `scripts/bench.sh` runs against `feed`, zero errors.
3. **Skip the per-row access scan for types with no view-affecting taps.** `fetch_access_filtered` (`gather/gather_service.rs:1472`) always pays for a `jsonb_build_object` candidate scan and a `row_to_json` window, though a published item on the live stage needs no plugin check (`docs/performance-tests.md`'s own note). A plain count and one windowed select when a type registers no such tap drops the extra statement on the common case. Effect: candidate win on BL-166 and BL-167, unmeasured until tried. Risk: must preserve the backfill loop's own pager correctness. Proof: `scripts/bench.sh --compare` on `category` and `feed`, plus a test that the backfill round count stays 1.
4. **Bound the search pager's count (BL-165).** `search/mod.rs:258-290`'s count reads every match before the pager shows a number. Effect: a one-word query should fall from 6 to 9 seconds toward sub-100 ms if the count is capped. Risk: the pager's wording changes, a product decision; justify it in the pull request per Guardrails. Proof: `scripts/bench.sh --pages search --compare`.
5. **Give cron and queue workers a pool of their own.** The single `PgPool` (`db.rs` `create_pool`, default max 10) is shared with web requests. Effect: fewer stalls once queue jobs and page requests compete for the same 10 connections. Risk: one more `PgPoolOptions` call, not a new abstraction. Proof: `scripts/bench.sh` at 100 concurrent clients, before and after, the pool gauge in `/metrics`.
6. **Give the benchmark a cold mode.** `bench.rs` warms up, then measures steady state; it cannot check the cold row of Targets. Effect: a real gate for cold p99, what every visitor sees right after a write. Risk: low, test tooling only. Proof: the new mode's own first run.
7. **Move the load tool onto Goose with real writes.** `benchmarks/load-test/src/main.rs`'s "write" requests hit `/health` today. Effect: the item-save and editor targets become checkable end to end, not inferred from reading code. Risk: moderate effort, no production code touched. Proof: a Goose run reporting real p50/p99 for a logged-in save.
8. **Pipeline the independent tail of `update_moderated`.** Audit write, cache invalidation and embed-job enqueue do not depend on each other but run serially (`content/item_service.rs:1575`). Effect: lower item-save p99. Risk: readability; comment why each step is independent, keep anything that must follow commit (cache invalidation) after it. Proof: `scripts/bench.sh` against a save route, before and after.
9. **Land indexed Gather field filters for non-tag fields.** `GatherQueryBuilder::field_expr` never uses the GIN index outside tag containment; `Design-Query-Engine.md`'s "Expression Indexes" proposal is undesigned in code. Effect: the only way the 200-field, 1-million-item scale point gets a real target. Risk: highest here, a new admin-facing index lifecycle; keep it its own effort, as this plan already defers its target. Proof: a filter on a non-tag field, before and after, at 1 million items.
10. **Add a performance-test case for the backfill round count.** Once fix 3 lands, assert that a published-content gather runs exactly 1 backfill round, so a change that starts denying rows is caught by the cheap gate, not discovered at scale. Risk: none. Proof: the test, and `TROVATO_PERF_RECORD=1` showing the count.

## Baseline run

```sh
# Build once, release mode.
cargo build --release

# Generate one database per scale point, keep each (a second run against the
# same generated site is a no-op; any other non-empty database is refused).
DATABASE_URL=postgres://localhost/trovato_10k   target/release/trovato generate --items 10000    --seed 1 --admin-password <pw>
DATABASE_URL=postgres://localhost/trovato_1m    target/release/trovato generate --items 1000000  --seed 1 --admin-password <pw>
DATABASE_URL=postgres://localhost/trovato_10m   target/release/trovato generate --items 10000000 --seed 1 --admin-password <pw>

# Record the host once per baseline.
lscpu > system-info.txt
uname -a >> system-info.txt
free -h >> system-info.txt
rustc --version >> system-info.txt
cargo --version >> system-info.txt

# Start the server against one scale point, rate limits raised so the
# benchmark does not hit them (scripts/bench.sh's own comment).
TROVATO_RATE_LIMIT_PAGE=100000000 TROVATO_RATE_LIMIT_STATIC=100000000 \
TROVATO_RATE_LIMIT_SEARCH=100000000 DATABASE_URL=postgres://localhost/trovato_10m \
  target/release/trovato serve &

# Run and save the baseline.
scripts/bench.sh --pages "front item feed category" --runs 3 --requests 2000 --save
scripts/bench.sh --pages "search" --runs 3 --requests 50 --save

# The cheap gate, run the same way CI runs it.
cargo test -p trovato-kernel --test performance_test -- --test-threads=1
```

The run must record: the host's CPU model, core count and RAM; the scale
point (item count, field count if not the default); the git commit under
test; every page's median and p99 over the 3 runs with each run's own figures;
and the error count per page, since a non-zero one (BL-164) means the timing
cannot be trusted until it is explained.
