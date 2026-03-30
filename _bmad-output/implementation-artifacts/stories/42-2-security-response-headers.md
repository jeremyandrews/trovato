# Story 42.2: Security Response Headers

Status: ready-for-dev

## Story

As a site operator,
I want standard security headers on all HTTP responses,
so that browsers enforce protections against clickjacking, MIME-sniffing, and other common attacks.

## Acceptance Criteria

1. `X-Frame-Options: DENY` header on all responses
2. `Strict-Transport-Security: max-age=31536000; includeSubDomains` header on HTTPS responses only (determined by checking `X-Forwarded-Proto` header)
3. `X-Content-Type-Options: nosniff` header on all responses globally
4. `Referrer-Policy: strict-origin-when-cross-origin` header on all responses
5. `Permissions-Policy: camera=(), microphone=(), geolocation=()` header on all responses
6. Headers are configurable via env vars (e.g., `HSTS_MAX_AGE`, `REFERRER_POLICY`, `PERMISSIONS_POLICY`)
7. Implemented as a single middleware layer in `crates/kernel/src/middleware/security_headers.rs`
8. Existing per-route `nosniff` on file serving routes still present (defense in depth; global header supplements, does not replace)

## Tasks / Subtasks

- [ ] Create `crates/kernel/src/middleware/security_headers.rs` with a single middleware layer (AC: #7)
- [ ] Add `X-Frame-Options: DENY` to responses (AC: #1)
- [ ] Add HSTS header with `X-Forwarded-Proto` check (AC: #2)
- [ ] Add `X-Content-Type-Options: nosniff` globally (AC: #3)
- [ ] Add `Referrer-Policy: strict-origin-when-cross-origin` (AC: #4)
- [ ] Add `Permissions-Policy: camera=(), microphone=(), geolocation=()` (AC: #5)
- [ ] Add config fields for env var overrides (`HSTS_MAX_AGE`, `REFERRER_POLICY`, `PERMISSIONS_POLICY`) (AC: #6)
- [ ] Apply middleware layer to root router (AC: #7)
- [ ] Verify existing file-route nosniff is retained (AC: #8)
- [ ] Write integration tests asserting all headers on a standard response (AC: #1, #3, #4, #5)
- [ ] Write integration test verifying HSTS only present when `X-Forwarded-Proto: https` (AC: #2)
- [ ] Write integration test verifying env var overrides work (AC: #6)

## Dev Notes

### Architecture

A single middleware layer keeps the security headers consolidated and easy to audit. The middleware inserts headers into the response after the inner handler runs, so it does not interfere with route-specific headers. Apply this middleware at the same level as the CSP middleware (Story 42.1) -- ordering does not matter since they set different headers.

### Security

- HSTS must only be sent over HTTPS to avoid breaking HTTP-only dev environments. The `X-Forwarded-Proto` check handles reverse proxy deployments (nginx, CloudFront, etc.).
- `X-Frame-Options: DENY` overlaps with CSP `frame-ancestors 'none'` (Story 42.1) for defense in depth -- older browsers support X-Frame-Options but not CSP.
- The `Permissions-Policy` restricts browser features that Trovato does not need, reducing attack surface from compromised plugins.

### Testing

- Integration tests should send requests with and without `X-Forwarded-Proto: https` to verify conditional HSTS.
- Verify that overriding `REFERRER_POLICY` env var changes the header value.
- Verify file download routes still have `nosniff` even if the global middleware were somehow skipped (defense in depth check).

### References

- `crates/kernel/src/middleware/` -- middleware directory
- `crates/kernel/src/routes/` -- root router setup
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
