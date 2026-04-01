# Story 38.6: API Authentication & Rate Limiting

Status: done

## Story

As an **API consumer**,
I want to authenticate with Bearer tokens and have rate limits enforced per endpoint,
so that I can access protected resources securely and the API remains available under load.

## Acceptance Criteria

1. API token authentication via `Authorization: Bearer <token>` header
2. Valid token injects user_id into session so existing handlers work unchanged
3. Invalid or expired tokens return 401 JSON error
4. Missing Authorization header passes through (session/cookie auth may still work)
5. Existing cookie session takes precedence over Bearer token
6. JWT bearer auth takes precedence over API token lookup (no double-lookup)
7. `last_used` timestamp updated in background on successful token auth
8. Rate limiting per endpoint category via Redis sliding window counters
9. Rate limit categories: login (5/min), forms (30/min), api (100/min), search (20/min), uploads (10/min), register (3/hr), verify_email (10/min), profile (10/min), password (5/min)
10. Rate limit failure returns 429 with retry-after seconds
11. Redis failure fails open (allows request rather than blocking)

## Tasks / Subtasks

- [x] Implement authenticate_api_token middleware (AC: #1, #2, #3, #4)
- [x] Add session precedence check -- skip token lookup if session has user_id (AC: #5)
- [x] Add JWT bearer auth check -- skip API token lookup if BearerAuth present (AC: #6)
- [x] Update last_used via background tokio::spawn task (AC: #7)
- [x] Define RateLimitConfig with per-category limits and windows (AC: #8, #9)
- [x] Implement RateLimiter with Redis INCR + EXPIRE sliding window (AC: #8)
- [x] Return Ok(()) or Err(retry_after) from check() (AC: #10)
- [x] Fail open on Redis errors (AC: #11)
- [x] Wire rate limiting middleware to API router

## Dev Notes

### Architecture

Two complementary middleware components:

**API Token Auth** (`middleware/api_token.rs`, 104 lines):
- Extracts Bearer token from Authorization header
- Three-level precedence: (1) JWT BearerAuth already present -> skip, (2) session already has user_id -> skip, (3) look up ApiToken in DB
- On valid token, injects `user_id` into session via `session.insert(SESSION_USER_ID, token.user_id)` so all downstream handlers work without modification
- `touch_last_used` runs in a background task to avoid adding latency
- Note on session creation: when no cookie exists, inserting user_id creates a server-side Redis session bounded by global TTL (24h)

**Rate Limiter** (`middleware/rate_limit.rs`, 247 lines):
- Redis-backed sliding window counter using `INCR` + `EXPIRE`
- `RateLimitConfig` defines 9 endpoint categories with (max_requests, window_duration) tuples
- `check()` returns `Ok(())` if within limit, `Err(retry_after_seconds)` if exceeded
- Fail-open design: Redis connection failures log a warning and allow the request through, preventing a Redis outage from taking down the entire API
- Key format: `rate:{category}:{identifier}` (identifier is typically IP or user ID)

### Testing

- Token auth tested with valid/invalid/missing tokens
- Session precedence tested (cookie auth not overwritten)
- Rate limiting tested at boundary conditions
- Fail-open behavior tested with Redis unavailable

### References

- `crates/kernel/src/middleware/api_token.rs` (104 lines) -- Bearer token authentication
- `crates/kernel/src/middleware/rate_limit.rs` (247 lines) -- Redis-backed rate limiting
