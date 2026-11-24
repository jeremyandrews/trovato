Every request now runs inside a request profile that counts its database
statements, cache lookups and writes, plugin calls, host calls, gather runs,
template renders and outgoing HTTP requests, and the kernel has spans at the
few places all of that passes through: sqlx's own statement event (so no query
call site is wrapped), `CacheLayer`, the in-process cache counter, the plugin
export call, the host call wrapper, the gather runner, the theme engine's
render entry points, every outgoing HTTP call the kernel makes (plugin HTTP and AI calls, search, the
assistant and the release check), and the queue job
runner. The spans use the tracing target `gander` at `TRACE`, so with tracing
off, the default, nothing is interested in them and creating one allocates
nothing (`tracing_off_test` measures it). The design is
`docs/design/tracing-and-performance-tests.md`.

An administrator's responses carry the counts and durations in
`Server-Timing` (`db`, `cache`, `plugin`, `render`, `http`); everyone else
gets `total;dur=` exactly as before. `SERVER_TIMING=all` shows them to every
visitor and `SERVER_TIMING=off` to no one. The slow request line now carries
the route template and the same breakdown. `db_query_duration_seconds` and
`tap_duration_seconds`, registered since the start and never fed, are now fed
while serving a request, and `http_request_db_queries` (statements per request
by route) and `cache_sets_total` are new.

The request span `tower_http` logs now carries the matched route template
instead of the URI. The default `RUST_LOG` prints that span on every log line
inside a request, so a search term, a password reset token or an email in a
URL was written to the log with each one. `telemetry_privacy_test` seeds a
password, an email, a reset token, an API token and a search term and fails if
any of them reaches a log line, a span, `Server-Timing` or `/metrics`.
