A request profiler, off by default. Set `PROFILER_ENABLED=1` and a holder of the
new `view request profiles` permission (granted to no role; a superuser holds
it) can read, at `/admin/reports/requests`, where each recent request's time
went: the phases as a tree (the access checks, the handler, gathers, each
plugin's tap calls, template rendering), every SQL statement with its duration
and its literal values replaced by `?`, the tap calls by plugin, and the cache
hits and misses per cache. The last `PROFILER_CAPTURE` requests (default 100)
are kept in memory, per process, and never written anywhere.
`PROFILER_SHOW_SQL_VALUES` keeps the literals, and is logged at WARN when set.
A profile never holds headers, cookies, the session, the request body, the
query string, cache keys or tap payloads, and a path is stored as the route it
matched, so a reset token in the address is not kept. See `docs/profiler.md`.

The instrumentation is `tracing` and nothing else: spans at the phase
boundaries under one target, sqlx's own statement events for the SQL, and the
cache-counting helper's event for the caches, all collected by one layer. With
the profiler off that layer is not installed and the spans are disabled at
their callsites, so a request does no profiling work; the overhead measured
for this change is in `docs/profiler.md`. The console filter is now a
per-layer filter, so the console prints exactly what it printed before.

`tap_duration_seconds` and `db_query_duration_seconds` on `/metrics` were, like
the cache counters before them, registered and never recorded; the profiler's
layer records them now, so they move while the profiler is on and stay at zero
while it is off.
