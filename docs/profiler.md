# The request profiler

The request profiler shows, for each recent request, where its time went. It is
a diagnostic tool: switch it on while you are looking for something, and leave
it off in production unless you need it there.

## Turning it on

| Setting | Default | What it does |
|---|---|---|
| `PROFILER_ENABLED` | `0` | `1` (or `true`, `yes`, `on`) installs the profiler and serves its pages. |
| `PROFILER_CAPTURE` | `100` | How many recent requests are kept, 1 to 10000. |
| `PROFILER_SHOW_SQL_VALUES` | `0` | Keep SQL literal values instead of replacing them with `?`. |

All three are read once, at startup. With the profiler on, the startup log says
so (`request profiler enabled`); with `PROFILER_SHOW_SQL_VALUES` set it also
logs a WARN, because literal values can hold personal data.

The pages are `/admin/reports/requests` (the recent requests) and
`/admin/reports/requests/{id}` (one of them). They ask for the
`view request profiles` permission, which no role is granted by default. A
superuser holds it implicitly; grant it to a role on the permissions screen to
delegate it. The admin sidebar links the pages, under System, only while the
profiler is on and only for a viewer holding the permission. While it is off
the pages answer 404 to everyone.

## What it records

For each request, from the outermost middleware in:

- the method, the route template that matched (`/item/{id}`, not the address
  asked for), the status and the total time;
- the phases, as a tree in start order, each with its duration and the number
  of SQL statements run directly inside it: `handler` (the route handler, as
  distinct from the middleware around it), `access` (the permission helpers:
  `require_permission` names the permission, `require_login` and
  `require_admin`), `item_access` (an item access decision, including the
  `tap_item_access` dispatch), `gather` (a gather query, named for what it
  lists), `tap` (one plugin's handler of one tap), and `render` (page, item and
  admin template rendering, and render elements);
- every SQL statement: its text with literal values replaced by `?`, its
  duration as sqlx measured it, the rows it returned and the phase it ran in;
- every plugin tap call, by plugin and tap, with its duration;
- cache hits and misses, per cache, with the same names `cache_hits_total` and
  `cache_misses_total` use on `/metrics`.

Routing is not timed: axum matches the route before the first middleware runs,
so it falls outside every span. The profile records which route matched.

A profile keeps at most 250 phases, 250 statements and 250 tap calls, and a
statement's text up to 2000 characters. Past that it stops adding entries and is
marked truncated; the counts and totals still include everything.

## What it never records

A profile is built from the fields the profiler's own spans carry, so what it
leaves out is left out by construction, not scrubbed afterwards:

- no headers, no cookies, no session or session id;
- no request body and no query string, and no path: only the route template, so
  a secret in the address (a password reset token) is never stored;
- no cache keys: a cache read reports the cache's name and whether it hit;
- no tap payloads: a tap call reports the plugin and the tap;
- no SQL literal values, unless `PROFILER_SHOW_SQL_VALUES` is set. The kernel
  binds its values rather than writing them into SQL, so most statements read
  the same either way; the redaction covers the statements that do not, and
  plugins' statements.

Requests to the profiler's own pages are not profiled.

## Where profiles live

In memory, in a ring of the last `PROFILER_CAPTURE` requests, newest first.
Nothing is written to the database, to Redis or to a file, and a restart
forgets everything.

The ring is per process. Behind a load balancer with several instances, each
keeps its own last N requests, and the page shows only the instance that served
it; there is no shared view. Pin yourself to one instance, or run one, while you
profile.

## How it works

The instrumentation is `tracing` spans at the phase boundaries, all under the
target `trovato::profile` at `TRACE`, plus the events sqlx already emits on
`sqlx::query` for every statement (its text, rows and `elapsed_secs`, at
`DEBUG`, or `WARN` past sqlx's one-second slow threshold). One
`tracing_subscriber` layer (`crates/kernel/src/profiler/collector.rs`) collects
a request's spans and those events into its profile, and puts the finished
profile in the ring. The console log has its own per-layer filter (`RUST_LOG`),
so the profiler neither adds to nor removes from what the console prints.

The same layer records `tap_duration_seconds` and `db_query_duration_seconds` on
`/metrics`, for every tap call and statement, in a request or not. Those two
histograms therefore move only while the profiler is on.

With the profiler off, the layer is not installed, its spans are disabled at
their callsites by the console filter (which does not enable `TRACE` for
`trovato::profile` unless `RUST_LOG` asks it to), the `handler` middleware is not
in the stack, and no store exists: a request does no profiling work.

## Overhead

OVERHEAD-TABLE
