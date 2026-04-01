# Story 39.5: Prometheus Metrics and Health Check

Status: done

## Story

As a **platform operator**,
I want Prometheus-format metrics and a health check endpoint,
so that I can monitor application performance, set up alerting, and integrate with container orchestration health probes.

## Acceptance Criteria

1. `/metrics` endpoint returns metrics in Prometheus text exposition format
2. `/health` endpoint returns 200 with `{"status":"healthy"}` when Postgres and Redis are reachable, 503 otherwise
3. HTTP request metrics tracked: total count by method/path/status, duration histogram
4. TAP invocation duration tracked by plugin name and tap name
5. Database query duration histogram collected
6. Cache hit/miss counters tracked
7. Active HTTP connections gauge maintained
8. File upload counter and bytes counter tracked
9. Rate limit rejection counter tracked
10. Rate limiting middleware with configurable per-category limits (login, forms, API, search, uploads, registration)

## Tasks / Subtasks

- [x] Define `Metrics` struct with `prometheus_client` registry and metric families (AC: #3, #4, #5, #6, #7, #8, #9)
- [x] Define `HttpLabels` (method/path/status) and `TapLabels` (plugin/tap) label sets (AC: #3, #4)
- [x] Register all metric families: http_requests_total, http_request_duration_seconds, tap_duration_seconds, db_query_duration_seconds, cache_hits, cache_misses, active_connections, file_uploads, file_upload_bytes, rate_limit_rejections (AC: #3-#9)
- [x] Implement `Metrics::encode()` for Prometheus text format output (AC: #1)
- [x] Implement `/metrics` route handler (AC: #1)
- [x] Implement `/health` route with concurrent Postgres and Redis health checks (AC: #2)
- [x] Implement `RateLimitConfig` with per-category defaults: login (5/min), forms (30/min), API (100/min), search (20/min), uploads (10/min), register (3/hour) (AC: #10)
- [x] Implement Redis-backed sliding window rate limiter with INCR + EXPIRE pattern (AC: #10)
- [x] Add unit tests for metrics and rate limiting (AC: #1, #10)

## Dev Notes

### Architecture

Metrics uses the `prometheus_client` crate (not the older `prometheus` crate) for type-safe metric registration. The `Metrics` struct owns the `Registry` and all metric families, exposed via `Arc<Metrics>` in `AppState`. Histograms use exponential buckets (`0.001 * 2^n` for 12 buckets) covering 1ms to ~4s for HTTP request durations.

The health check endpoint uses `tokio::join!` to concurrently probe Postgres (`SELECT 1`) and Redis (`PING`). The response includes individual component status for debugging partial failures.

Rate limiting uses a Redis sliding window counter pattern: each request increments a key `rate:{category}:{client_ip}` with `INCR` and sets expiry with `EXPIRE`. When the count exceeds the configured limit, the request is rejected with 429 Too Many Requests. Categories have independent limits to allow tight constraints on security-sensitive endpoints (login, registration) while being more permissive for general API usage.

### Testing

- Unit tests in `crates/kernel/src/metrics/mod.rs` (3 tests)
- Unit tests in `crates/kernel/src/middleware/rate_limit.rs` (2 tests)
- Integration tests in `crates/kernel/tests/` cover health endpoint responses

### References

- `crates/kernel/src/metrics/mod.rs` (300 lines) -- Metrics struct, registry, label types
- `crates/kernel/src/routes/health.rs` (51 lines) -- health check endpoint
- `crates/kernel/src/routes/metrics.rs` -- /metrics route handler
- `crates/kernel/src/middleware/rate_limit.rs` (247 lines) -- RateLimitConfig, sliding window implementation
