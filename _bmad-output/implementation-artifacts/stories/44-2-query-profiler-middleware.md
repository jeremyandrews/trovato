# Story 44.2: Query Profiler Middleware

Status: ready-for-dev

## Story

As a **site operator**,
I want slow database queries logged automatically,
so that I can identify and resolve performance bottlenecks without manual profiling.

## Acceptance Criteria

1. Middleware wraps database queries measuring wall-clock duration
2. Queries exceeding the slow threshold are logged at WARN level with query text, duration, route, and request ID
3. `QUERY_SLOW_THRESHOLD_MS` env var configures the threshold (default 100ms)
4. Queries exceeding 5x the threshold are logged at ERROR level
5. Gated behind a compile-time feature flag (`--features query-profiler`)
6. When enabled, responses include a `Server-Timing: db;dur=X` header with cumulative DB time in milliseconds
7. Query profiler logging is suppressed in tests unless `QUERY_PROFILER_IN_TESTS=true` is set
8. At least 1 integration test using `pg_sleep` to verify slow query detection

## Tasks / Subtasks

- [ ] Add `query-profiler` feature flag to `crates/kernel/Cargo.toml` (AC: #5)
- [ ] Create `crates/kernel/src/middleware/query_profiler.rs` with timing wrapper (AC: #1)
- [ ] Read `QUERY_SLOW_THRESHOLD_MS` from env, default to 100 (AC: #3)
- [ ] Log WARN for queries exceeding threshold, include query text, duration, route, request ID (AC: #2)
- [ ] Log ERROR for queries exceeding 5x threshold (AC: #4)
- [ ] Add `Server-Timing: db;dur=X` response header when feature is enabled (AC: #6)
- [ ] Check `QUERY_PROFILER_IN_TESTS` env var — suppress logging in test context when not set (AC: #7)
- [ ] Register middleware in router conditionally on feature flag (AC: #5)
- [ ] Write integration test: execute `SELECT pg_sleep(0.2)` and verify WARN log output (AC: #8)

## Dev Notes

### Architecture

The profiler middleware sits at the Axum layer level. It intercepts database calls by wrapping the connection pool or query executor with a timing decorator. A `RequestLocal` or task-local accumulator tracks total DB time per request for the `Server-Timing` header. The middleware is conditionally compiled via `#[cfg(feature = "query-profiler")]` so there is zero overhead in production builds that do not opt in.

### Security

Query text in logs may contain sensitive data. The profiler should truncate query text to a reasonable length (e.g., 500 chars) and never log bind parameter values.

### Testing

- Use `SELECT pg_sleep(0.2)` to simulate a slow query exceeding the 100ms default threshold.
- Verify log output contains the expected fields (duration, route).
- Verify `Server-Timing` header is present and contains a valid `db;dur=` value.

### References

- `crates/kernel/src/middleware/` — existing middleware modules
- W3C Server-Timing specification for header format
