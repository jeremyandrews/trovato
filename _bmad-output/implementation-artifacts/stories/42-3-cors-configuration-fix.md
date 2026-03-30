# Story 42.3: CORS Configuration Fix

Status: ready-for-dev

## Story

As a site operator,
I want CORS restricted to explicit origins by default,
so that cross-origin requests are blocked unless I deliberately allow specific domains.

## Acceptance Criteria

1. Default `CORS_ALLOWED_ORIGINS` changed from `"*"` to empty string (no cross-origin allowed by default)
2. When origins list is empty, no CORS headers are sent on any response
3. Explicit `"*"` still works as an opt-in wildcard for development or public APIs
4. `Vary: Origin` header included when origin-specific CORS headers are sent (not wildcard)
5. Preflight `OPTIONS` requests handled correctly for configured origins
6. `CORS_ALLOWED_METHODS` env var added with default `GET,POST,PUT,DELETE,OPTIONS`
7. `CORS_ALLOWED_HEADERS` env var added with default `Content-Type,Authorization,X-CSRF-Token`
8. `.env.example` updated with new CORS env vars and documentation comments

## Tasks / Subtasks

- [ ] Change default for `CORS_ALLOWED_ORIGINS` in `crates/kernel/src/config.rs` from `"*"` to `""` (AC: #1)
- [ ] Update CORS middleware to skip all CORS headers when origins is empty (AC: #2)
- [ ] Ensure wildcard `"*"` origin still functions when explicitly configured (AC: #3)
- [ ] Add `Vary: Origin` header when responding with origin-specific `Access-Control-Allow-Origin` (AC: #4)
- [ ] Verify preflight `OPTIONS` handling returns correct `Access-Control-Allow-Methods` and `Access-Control-Allow-Headers` (AC: #5)
- [ ] Add `CORS_ALLOWED_METHODS` config field with default (AC: #6)
- [ ] Add `CORS_ALLOWED_HEADERS` config field with default (AC: #7)
- [ ] Update `.env.example` with all CORS env vars and comments (AC: #8)
- [ ] Write integration test: no CORS headers when origins empty (AC: #2)
- [ ] Write integration test: wildcard origin sends `Access-Control-Allow-Origin: *` (AC: #3)
- [ ] Write integration test: specific origin sends `Vary: Origin` (AC: #4)
- [ ] Write integration test: preflight returns configured methods/headers (AC: #5, #6, #7)

## Dev Notes

### Architecture

The existing CORS setup likely uses `tower-http`'s `CorsLayer`. The change is primarily config-level: the default changes and the middleware skips entirely when no origins are configured. The `Vary: Origin` header is important for cache correctness -- CDNs/proxies must not cache a response with one origin's CORS headers and serve it for another origin.

### Security

- The current `"*"` default is overly permissive and allows any site to make credentialed requests to the Trovato API. Changing to empty-by-default is a breaking change for sites relying on the implicit wildcard, but it is the secure default.
- `X-CSRF-Token` must be in the allowed headers list since all state-changing endpoints require CSRF tokens.
- Wildcard `"*"` and credentials cannot be combined per the CORS spec -- if credentials are needed, specific origins must be listed.

### Testing

- Test with `Origin: https://evil.com` and empty config -- no CORS headers should appear.
- Test with `Origin: https://allowed.com` and `CORS_ALLOWED_ORIGINS=https://allowed.com` -- proper CORS headers plus `Vary: Origin`.
- Test preflight with `Access-Control-Request-Method: PUT` -- verify it is in the response.

### References

- `crates/kernel/src/config.rs` -- CORS config fields
- `.env.example` -- env var documentation
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
