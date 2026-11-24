# Tracing and Performance Tests

**Status:** Design, built in BG14 (`bg14-tracing`, `bg14-tracing-2`, `bg14-tracing-3`).
**Read against:** `origin/main` at `99f7a68`.

## Goal

Every request, query, cache call, plugin call and queue job in Trovato can be
traced. CI fails when a change adds a database query or a cache call to a page,
using exact counts the way Drupal's Gander performance tests do. A data
generator makes tests against large sites repeatable. An operator can send
traces to any OpenTelemetry backend, and tracing costs nothing when it is off.

Trovato is about to host many blogs and has to scale to installs holding
100,000,000 items. The query budgets per page kind that BG11 sets stand on the
counts this work records.

## What `main` does today (rechecked)

Each fact in the brief was checked against `99f7a68`:

1. `tracing = "0.1"` and `tracing-subscriber` with `env-filter` are workspace
   dependencies (`Cargo.toml`), and `prometheus-client = "0.22"` is a kernel
   dependency (`crates/kernel/Cargo.toml`). **The kernel creates no span at
   all**: a search for `span!`, `#[instrument]` and `.instrument(` in
   `crates/kernel/src` finds nothing. The only spans are the ones
   `tower_http::trace::TraceLayer::new_for_http()` makes (`main.rs`), at
   `DEBUG`, carrying the full request URI.
   Metrics (`crates/kernel/src/metrics/mod.rs`): `http_requests_total` and
   `http_request_duration_seconds` (fed by the timing middleware),
   `cache_hits_total` and `cache_misses_total` by cache name (fed by
   `Metrics::count_cache`, which every in-process cache and `CacheLayer::get`
   already go through), pool gauges, upload, rate limit, permission failure and
   circuit breaker gauges. Two are registered and **never fed**:
   `tap_duration_seconds` (`record_tap` has no caller) and
   `db_query_duration_seconds` (`record_db_query` has no caller).
   `/metrics` (`routes/metrics.rs`) answers anyone who can reach the server,
   with no address check and no token.
2. `middleware/query_profiler.rs` `track_request_timing` adds
   `Server-Timing: total;dur=<ms>`, counts the request into the two HTTP
   metrics by matched route template, and logs requests over
   `QUERY_SLOW_THRESHOLD_MS` (warn above it, error above five times it). It is
   the outermost layer but one in `main.rs` (only `TraceLayer` is outside it).
   It counts no queries, cache calls or plugin calls; its own doc comment says
   per-query tracking "would require wrapping the `PgPool` and is not done".
   Unchanged from `28a029b` (#80).
3. `docs/BACKLOG.md` records `G-NO-REQUEST-PROFILER` as written. **Changed:**
   not three but seven design documents name Gander. What they intended:
   - `docs/design/Design-Web-Layer.md`: "We wrap the entire application in a
     custom profiling middleware ("Gander")", logging with
     `target: "gander"`; the profiler "logs detailed JSON traces for slow
     requests, including breakdown of DB query duration, WASM tap invocation
     duration, and template rendering duration ... structured for ingestion by
     tools like Jaeger or Honeycomb".
   - `docs/design/Design-Overview.md`: "Gander-style observability", drawn as
     the outermost box of the kernel ("Profiler ◄── Gander Middleware").
   - `docs/design/Architecture.md`: "Observability: Gander profiling
     middleware, Prometheus metrics, health check".
   - `docs/design/Overview.md`, `Phases.md` and `Design-Project-Meta.md` list
     "Gander profiling middleware" as Phase 1 scope (marked complete, though
     nothing was built), and `ai-integration.md` asks for "Gander integration
     for profiling" of AI requests (latency, model, plugin).
   This design honours each: the profiler stays the outermost layer, the spans
   use the tracing target `gander`, the slow request line carries the database,
   plugin and render breakdown, export speaks OTLP so Jaeger, Honeycomb or
   Grafana Tempo read it, and AI provider calls are outgoing call spans with
   their provider and model.
4. There is no test asserting query or cache counts per page, no OpenTelemetry
   export and no data generator. `benchmarks/load-test` is a concurrent load
   tool with a mixed random workload; it prints averages, not per scenario
   median and p99, and keeps no baseline.

Also found: `TraceLayer::new_for_http()`'s span records `uri=` with the query
string, and the default `RUST_LOG` (`tower_http=debug`) prints that span on
every log line inside a request. A search for `jane@example.com` logs
`uri=/search?q=jane%40example.com`. That is personal data in today's
telemetry; see "No personal data" below.

## Decisions

### 1. Where to instrument: choke points, not call sites

| What | Choke point | Why it sees everything |
|---|---|---|
| Database queries | sqlx's own `QueryLogger`, through a `tracing` layer that listens for its `sqlx::query` event | Every statement sqlx runs, on a pool, a transaction or a bare connection, ends by dropping a `QueryLogger`, which emits one event with the SQL text (never the bound values) and the elapsed time (`sqlx-core-0.9.1/src/logger.rs`). Wrapping `PgPool` would miss every `&mut *tx` and every service that holds its own pool clone; there are hundreds of call sites. |
| Redis cache | `CacheLayer::{get, set, invalidate, invalidate_tag}` (`cache/mod.rs`) | The only kernel type that reads and writes cached values in Redis. |
| In-process caches | `Metrics::count_cache` (`metrics/mod.rs`) | Every moka cache lookup already reports its hit or miss there, under a closed `CacheName`. |
| Plugin taps and plugin-to-plugin calls | `tap::dispatcher::instantiate_and_call_export` | The one function that makes a `Store` and calls a guest export. |
| Host calls | `host::trace` (`TracedLinker::func_wrap_async_traced` and `HostCallGuard`) | Every host function a plugin can import is already wrapped there. |
| Gather | `GatherService::run_definition` | Every `execute*` entry point, a site's and a plugin's, lands there. |
| Templates | `ThemeEngine::render_page`, `render_item`, `render_element` and `routes::helpers::render_admin_template` | The entry points that turn a page, an item, an element or an admin screen into HTML. About twenty handlers (login, password reset, comments, error and maintenance pages) still call `tera().render` themselves and are not counted as renders; their queries and cache calls are counted all the same. |
| Queue jobs | `cron::run_queue_job` | Every claimed plugin queue job runs through it, from cron and from the resident runner. A job gets a span, not a request profile: nothing would read its counts. |
| Outgoing HTTP | `telemetry::send`, which every kernel `send()` now goes through (a plugin's `http-request` and `http-open`, the AI provider calls of the services, search and the assistant, the release check); `host::ai` uses its `send_with` variant to add its provider and model to the span | One function, so a new send site that bypasses it is visible in review as a bare `.send()`. |

Nothing is instrumented call site by call site.

### 2. Per-request counters

A `RequestProfile` (new module `crates/kernel/src/telemetry/`) holds plain
atomic counters: queries, query time, cache gets, cache hits, cache misses,
cache sets, plugin calls, plugin time, host calls, gather runs, renders, render
time, outgoing calls. `track_request_timing` creates one per request and runs
the rest of the request inside a `tokio::task_local!` scope holding it; each
choke point adds to it with `try_with`, which is a no-op outside a request (a
cron run, a queue job, startup).

A task-local and not a span: counting must work, and be deterministic, when
tracing is off and no span exists. Work a request spawns onto another task
(a cache generation bump, a search analytics flush) is not counted, which is
also what keeps the counts deterministic.

The query listener is a per-layer filter that passes the `sqlx::query` event
to the listener only while a profile is in scope. Its presence can change what
sqlx does with every statement. sqlx builds its event when `log::log_enabled!`
or `tracing::enabled!` says yes for `sqlx::query`. With per-layer filters the
registry answers yes once the level passes, even when every filter then
declines; that is a limit of `tracing-subscriber`, and no filter can veto it.
Under the default `RUST_LOG`, whose `tower_http=debug` already puts the `log`
bridge at DEBUG, sqlx builds the event for every statement on `main` too, so
nothing changes. Under a `RUST_LOG` with no DEBUG directive (for example
`docker-compose.cluster.yml`), the listener's DEBUG level hint raises the
global `tracing` and `log` maximum level to DEBUG. sqlx then builds the event
for every statement, inside a request or not: the first four words and a copy
of the SQL, a few microseconds next to a round trip. Every `log::debug!` in a
dependency also reaches the dispatcher before it is dropped. The log output is
unchanged. The only alternative, a plain layer that vetoes the event globally,
would also silence sqlx's slow statement warnings outside requests, which is a
behaviour change. The cost is measured against `main` (see "Measurements" in
the report of each part).

**Fingerprints.** A fingerprint names a statement without its values:
`<verb>:<first table>:<8 hex of the SHA-256 of the normalised SQL>`, for
example `select:item:3fa2c1d0`. Normalising lowercases everything outside
quoted identifiers, drops comments (nested block comments whole), replaces
string and number literals with `?`, collapses `$1, $2, ...` lists to `$?` (so
an `IN` list does not change the fingerprint with its length) and squeezes
whitespace. Fingerprints are only computed when something reads them: the
performance tests (a `Config` field the test sets, with no environment
variable) or span export.

**Where they show.**
- `Server-Timing`, by `SERVER_TIMING` = `admin` (the default), `all` or
  `off`. Anonymous visitors and non-administrators get exactly today's header,
  `total;dur=<ms>`. An administrator, or everyone with `all`, also gets
  `db;dur=..;desc="N queries"`, `cache;desc="G gets M misses S sets"`,
  `plugin;dur=..;desc="P calls H host calls"`, `render;dur=..;desc="R renders"` and
  `http;desc="O calls"`.
  Administrators only by default because counts and durations per request are
  a timing oracle for anyone else. The administrator check runs after the
  counters are read, through the cached user, so it never shows in the counts.
  It does run on every signed-in request under `admin`: a user cache hit (one
  more in `cache_hits_total{cache="user"}`), or a query on a miss, after
  `total` is measured.
- The slow request line gains `route`, `status`, `queries`, `db_ms`,
  `cache_gets`, `cache_misses`, `plugin_calls`, `plugin_ms`, `render_ms` and
  `outgoing_calls`: the breakdown
  `Design-Web-Layer.md` asked for.
- The response's extensions carry a `RequestProfileSnapshot`, which is how the
  performance tests read a request's numbers without a header.

### 3. Spans

All spans use the tracing target `gander` at `TRACE` level. With export off
no layer is interested in that target, every callsite is disabled once and
for all, and a span costs one relaxed atomic load: no allocation, no clock
read. `RUST_LOG` defaults never enable `trace`, so the log does not change.

Names and attributes, following OpenTelemetry semantic conventions where one
exists:

| Span | Attributes |
|---|---|
| `http.request` | `http.request.method`, `http.route` (the matched template, `<fallback>` for aliases and 404s, never the path), `http.response.status_code`, and the request's counters at the end. `trovato.tenant` once BG4's resolution is in the request (it is not on `main` today). |
| `db.query` | `db.system.name=postgresql`, `db.query.summary` (the fingerprint), `db.query.text` (the normalised SQL: literals already replaced), `db.response.returned_rows` |
| `cache` (`otel.name` is `cache.get`, `cache.set`, `cache.invalidate` or `cache.invalidate_tag`) | `cache.tier` (`redis`), `cache.bin` (the key's first segment if it is `[a-z_]+`, else `other`), `cache.hit` |
| in-process cache lookups | an event `cache.get` on the enclosing span with `cache.tier=memory`, `cache.bin` (the `CacheName`) and `cache.hit` |
| `plugin.tap` | `trovato.plugin`, `trovato.tap` (the export name) |
| `plugin.host_call` | `trovato.plugin`, `trovato.host.module`, `trovato.host.function` |
| `gather.execute` | `trovato.gather.base_table`, `trovato.gather.item_type` (the definition reaches the choke point without its machine name) |
| `template.render` | `trovato.template.kind` (`page`, `item`, `element`, `admin`) |
| `queue.job` | `trovato.plugin`, `trovato.queue`, `trovato.queue.attempt`, `trovato.queue.outcome` |
| `http.client` | `http.request.method`, `server.address` (host only), `http.response.status_code`, and for AI calls `gen_ai.provider.name` and `gen_ai.request.model` |

In-process cache lookups are events, not spans, because they take well under
a microsecond: a span each would cost more than the lookup and bury the
queries in a trace. They are still counted exactly.

`db.query` spans are made after the fact: sqlx reports a statement when it
ends, with its elapsed time, so the exporter starts the span at `now -
elapsed` under the current span's context.

### 4. No personal data

Attributes are an allowlist, and each one above is either a template, a
machine name, a count, a duration or normalised SQL. Never recorded: a raw
path or query string, a header, a cookie, a bound parameter, a key beyond its
first segment, a user id, a name, an email, a body. sqlx's event carries no
bound values, and normalisation removes literals that `format!` built into
SQL text.

`TraceLayer`'s span is changed to carry the method and the matched route
instead of the URI, which removes the search term, reset token or email
that a URL carries from every log line. It is the one change to log output
with tracing off, and it is a privacy fix.

A test seeds a password, an email, a password reset token, an API token and a
search term, makes requests carrying them under a subscriber that captures
every Gander span and event at `TRACE` and the log at the production default,
and fails if any span, event, log line, `Server-Timing` value or metrics output
contains any of them. It also checks the signed-in user's id and name against
the spans, the header and the metrics (the existing login audit line records
them for the site log). Part 3 runs the same check with export on.

Nothing here touches the plugin API: no WIT change, no SDK change, no new
host function. Plugins are observed from the host side only.

### 5. Export

`TRACING_EXPORT` = `off` (default) or `otlp`. With `otlp`, spans go over
OTLP/HTTP (protobuf) to `OTEL_EXPORTER_OTLP_ENDPOINT` (default
`http://localhost:4318`) with the standard `OTEL_SERVICE_NAME` (default
`trovato`) and `OTEL_TRACES_SAMPLER_ARG` (a parent-based ratio, default
`1.0`), through a batch processor so the request never waits on the
exporter. OTLP is what Jaeger, Grafana Tempo, Honeycomb, Datadog, New Relic
and the OpenTelemetry Collector all accept, so one exporter reaches any
backend.

New crates, all from the OpenTelemetry family, compiled in always (an
operator turns export on with a setting, not a rebuild):

- `opentelemetry` and `opentelemetry_sdk` 0.33: the API and the batch span
  processor and sampler.
- `opentelemetry-otlp` 0.33 with `http-proto` and `reqwest-client` only (no
  gRPC, so no `tonic`): the exporter, over the `reqwest` the kernel already
  uses.
- `tracing-opentelemetry` 0.34: the bridge from `tracing` spans to OTel spans,
  maintained with `tracing` itself.

Cost with export off, the default: measured against `main` on the same
machine and data (part 3 adds the numbers here). Target: under 1% at p99.

### 6. Metrics

Through the existing `prometheus-client`, no new crate:

- `db_query_duration_seconds` and `tap_duration_seconds` are finally fed (from
  the query listener and `instantiate_and_call_export`), for work done while serving a request: they are observed through the
  request's profile, so cron, queue jobs and startup do not reach them.
- New `http_request_db_queries` histogram by route: queries per request.
- New `cache_sets_total` by cache (Redis sets).

`/metrics` answers only a loopback peer that sent no `Forwarded`,
`X-Forwarded-For` or `X-Real-IP` header (a reverse proxy on the same host
would otherwise make every visitor look local), or a request with
`Authorization: Bearer <METRICS_TOKEN>`, compared in constant time. Anything
else gets a 404, as if the route did not exist. This is a deliberate change:
today it answers the internet. Nothing in `deploy/` scrapes it.

### 7. Performance tests

A small API in the kernel's integration tests (`tests/common/perf.rs`),
described for contributors in `docs/performance-tests.md`:

```rust
let site = shared_site();                       // built once per test binary
let visit = site.visit("item", Visitor::Anonymous, Caches::Cold, &site.item_path).await;
check(&visit);                                  // against tests/fixtures/performance_counts.toml
```

- **Deterministic.** The site is built once per test binary:
  - a scratch database (`trovato_test_utils::ScratchDb`), migrated;
  - a plugin directory holding only the blog and search modules, which CI
    builds, with `trovato_comments` enabled without its module for the
    kernel's comment threads;
  - `trovato generate` with a fixed seed: 40 posts, 120 comments (three per post), 6 tags,
    7 members and an administrator;
  - a Redis database number of its own, flushed at start.

  Cold means every in-process cache cleared (a new
  `AppState::clear_local_caches`, the same clear a generation bump does) and
  every Redis key gone except the sessions, the session index and the
  generation counters. Warm means the same request made once just before.
  Visits take turns, and each waits out the generation watcher first. The
  signed-in sessions are registered by a request before any measurement. The
  binary installs the query listener as its subscriber, as `init_tracing`
  does. Counts, not timings.
- **Pages.** Front page, an item page with comments, a category page (a tag
  page), a feed, search, the login page and the admin content list. Each is
  visited anonymous and logged in where both exist, cold and warm.
- **Expected numbers** live in `tests/fixtures/performance_counts.toml`, one
  table per `page.visitor.caches`. Each table holds the counts and every
  distinct statement by fingerprint, with how often it ran and its
  normalised text, so a change shows in review as the statement it is.
  `TROVATO_PERF_RECORD=1` rewrites the file. A mismatch names every count and
  statement that changed.
- A test proves the harness bites: it adds one `SELECT 1` to the item page
  (a header only the test router answers) and asserts that the counts go up
  by one, that the difference names that statement, and that `check` fails.
- A cold visit that counts no statement fails on its own, so a broken
  listener cannot pass as a free page.

### 8. Scale data generator

`trovato generate --items N [--comments C] [--tags T] [--users U] [--seed S]
[--batch B] [--parallel P]` writes a deterministic site into `DATABASE_URL`.
The same arguments always produce the same rows: ids are
`md5('<seed>:<kind>:<n>')::uuid`, and titles, bodies, dates, tags and comment
counts are derived from the row number. It writes members (`gen_user_N`, with
`@example.invalid` addresses), an administrator (`gen_admin`), tags, blog
posts each with one revision, and comments, newest posts first. It sets the
front page to `/blog`, then runs `ANALYZE`.

The SQL is set-based: `INSERT ... SELECT ... FROM generate_series(lo, hi)`, one
statement per batch and `P` batches at once. Postgres does the work and the
binary only sends statements. Posts get their search vectors from the item
trigger as they are written. Comments are written with the two comment
triggers that update their post off: the search one, so comment text is not
searchable, and the count one, since each post is written with its comment
count. A database that already holds exactly this generated site is left
alone; any other database that is not empty (a different generated site, a run
that did not finish, a real site) is refused. A run that crashed with the
triggers off has them turned back on by the next run. There is no default
password, so no generated site has a known administrator login.

With `--items 20000000` it finished in under an hour on the Mac Studio (see
the measurements in the report). The data generator lives in the kernel
binary because it needs the kernel's own `AppState::new` (migrations, plugin
installation, content types) before it writes. Later runs reuse it to seed
their benchmarks.

### 9. Timing benchmarks

`scripts/bench.sh` starts nothing; it runs `trovato-loadtest`'s new
`bench` binary against a running server: named scenarios (`front`, `item`,
`feed`, `search`, `category`), a warm-up, then N sequential requests per
scenario, printing median and p99 per scenario. `--save` writes
`benchmarks/baseline.json`; `--compare` prints the change against it. By hand
only: a GitHub runner is too noisy for timings and has no generated site; a
scheduled job is left for when there is a fixed benchmark host.

### 10. Local trace viewer

`docker compose -f docker/observability/docker-compose.yml up -d` runs Jaeger
(which accepts OTLP directly), Prometheus (scraping the host's `/metrics`
with a token) and Grafana with both as provisioned data sources and one
committed dashboard (`docker/observability/grafana/dashboards/trovato.json`):
request rate and p99 by route, queries per request by route, cache hit ratio,
tap durations. Then `TRACING_EXPORT=otlp cargo run --release` and open
Jaeger at `http://localhost:16686` or Grafana at `http://localhost:3001`.

## What this does not build

- **No sampling by route or tail sampling.** A parent-based ratio is enough;
  a Collector does tail sampling better than the kernel would.
- **No OTel metrics or logs export.** Prometheus already carries the metrics,
  and duplicating them through OTLP would mean two metric pipelines.
- **No per-call-site query names.** The fingerprint names a statement; adding
  a name argument at each call site is the call-site instrumentation this
  avoids.
- **No in-process cache set counts.** Moka inserts happen at a dozen call
  sites with no shared function; a miss is followed by an insert at every one
  of them, so misses stand in for sets there. Redis sets are counted.
- **No session store or rate limiter spans.** They are Redis calls that are
  not cache calls; tower-sessions owns the session store.
- **No timing assertions in CI**, and no scheduled benchmark job.
- **No hosted backend**, no deploy.
- **No fixes to the slow paths the tests reveal.** They are backlog rows with
  their counts.
