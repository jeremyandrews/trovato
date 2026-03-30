# Story 47.2: API Versioning Infrastructure

Status: ready-for-dev

## Story

As a **platform maintaining backward compatibility**,
I want API versioning infrastructure,
so that future breaking changes can be introduced without disrupting existing consumers.

## Acceptance Criteria

1. Version routing middleware routes `/api/v{N}/...` requests to the appropriate version router
2. Version routers are separate Axum routers composed together (formalizing the existing `api_v1.rs` pattern)
3. `Sunset` header per RFC 8594 is supported for deprecated versions/routes
4. `Deprecation` header is supported for deprecated versions/routes
5. `Link` header pointing to successor endpoint is supported
6. Deprecation can be configured per-route or per-version
7. No v2 routes are created yet -- this is infrastructure only
8. All API responses include `X-API-Version: 1` response header
9. At least 2 integration tests: version header presence, deprecation headers on a test route

## Tasks / Subtasks

- [ ] Create version routing middleware that dispatches `/api/v{N}/...` to versioned routers (AC: #1)
- [ ] Refactor existing `api_v1.rs` routes into a composable v1 router (AC: #2)
- [ ] Implement `Sunset` header injection per RFC 8594 (AC: #3)
- [ ] Implement `Deprecation` header injection (AC: #4)
- [ ] Implement `Link` header with `rel="successor-version"` (AC: #5)
- [ ] Create deprecation configuration structure (per-route and per-version) (AC: #6)
- [ ] Add `X-API-Version: 1` header to all v1 API responses via middleware layer (AC: #8)
- [ ] Write integration test: verify `X-API-Version` header on API responses (AC: #9)
- [ ] Write integration test: mark a test route as deprecated, verify `Sunset` and `Deprecation` headers (AC: #9)

## Dev Notes

### Architecture

The version dispatch can be a simple nested router in Axum: `/api/v1` is nested under the v1 router, `/api/v2` (future) under v2, etc. This formalizes the existing pattern where `api_v1.rs` already handles v1 routes.

Deprecation metadata is stored in a configuration structure (not in route metadata from 47.1, though they can reference each other). The deprecation middleware reads this config and injects headers on matching routes.

The `X-API-Version` header is added via an Axum layer on the versioned router, so it automatically applies to all routes within that version.

### Security

- Version routing must not allow version bypass (e.g., requesting `/api/v0/...` should 404, not fall through to v1).

### Testing

- Make any v1 API request, verify `X-API-Version: 1` is in the response headers.
- Configure a test route as deprecated with a sunset date. Request it and verify `Sunset`, `Deprecation`, and `Link` headers are present and correctly formatted.

### References

- `crates/kernel/src/routes/api_v1.rs` -- existing v1 API routes
- `crates/kernel/src/routes/mod.rs` -- route composition
- RFC 8594 -- The Sunset HTTP Header Field
