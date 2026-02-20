# Story 27.3: CSRF Audit

Status: ready-for-dev

## Story

As a **security reviewer**,
I want every state-changing endpoint verified for CSRF protection,
So that attackers cannot forge requests on behalf of authenticated users.

## Acceptance Criteria

1. Every state-changing form verified to include and validate a CSRF token
2. All AJAX endpoints that modify state verified as CSRF-protected
3. API endpoints using cookie auth verified as CSRF-protected (or using non-cookie auth)
4. SameSite cookie policy verified as enforced (SameSite=Strict or Lax)
5. All findings documented with severity ratings
6. All Critical/High findings fixed

## Tasks / Subtasks

- [ ] Fix logout endpoint — change from GET to POST with CSRF (AC: #1, #6)
  - [ ] Change `GET /user/logout` to `POST /user/logout` in route registration
  - [ ] Add CSRF token validation to logout handler
  - [ ] Update logout links/buttons in templates to use POST forms
- [ ] Add CSRF protection to JSON API endpoints (AC: #2, #3, #6)
  - [ ] Comment API: POST `/api/item/{id}/comments`, PUT `/api/comment/{id}`, DELETE `/api/comment/{id}`
  - [ ] API Token routes: POST `/api/tokens`, DELETE `/api/tokens/{id}`
  - [ ] Item delete: POST `/item/{id}/delete`
  - [ ] Choose approach: CSRF header token or JSON body token for API endpoints
- [ ] Audit AJAX endpoint (AC: #2)
  - [ ] Verify `POST /system/ajax` CSRF handling in form service
  - [ ] Add CSRF token to `AjaxRequest` struct if missing
- [ ] Verify session cookie SameSite attribute (AC: #4)
  - [ ] Confirm default is "strict" in `config.rs`
  - [ ] Confirm `session.rs` applies SameSite correctly
- [ ] Audit password reset endpoint (AC: #1)
  - [ ] Verify `POST /user/password-reset/{token}` has CSRF or is safe via token-only auth
- [ ] Verify all admin form-based POST routes have CSRF (AC: #1)
  - [ ] Confirm all 49 `require_csrf` call sites cover all admin operations
- [ ] Document all findings with severity ratings (AC: #5, #6)

## Dev Notes

### Dependencies

No dependencies on other stories. Can be worked independently.

### Codebase Research Findings

#### CRITICAL: Logout on GET Request

**Location:** `crates/kernel/src/routes/auth.rs:388`

```rust
// GET /user/logout
pub async fn logout(session: Session, ...) -> impl IntoResponse {
    session.delete().await;
    ...
}
```

Logout is registered as GET. No CSRF protection. An attacker can force logout by embedding `<img src="/user/logout">` on any page. Must be converted to POST with CSRF token.

#### HIGH: Unprotected JSON API Endpoints

Multiple state-changing JSON API endpoints lack CSRF protection:

1. **Comment API** (`routes/comment.rs`):
   - POST `/api/item/{id}/comments` (line 187) — Create comment, no CSRF
   - PUT `/api/comment/{id}` (line 381) — Update comment, no CSRF
   - DELETE `/api/comment/{id}` (line 496) — Delete comment, no CSRF

2. **API Token Routes** (`routes/api_token.rs`):
   - POST `/api/tokens` (line 50) — Create API token, no CSRF
   - DELETE `/api/tokens/{id}` (line 174) — Revoke token, no CSRF

3. **Item Delete** (`routes/item.rs:703`):
   - POST `/item/{id}/delete` — Delete item, no CSRF

All only check session authentication but do not validate a CSRF token.

#### MEDIUM: Password Reset Endpoint

**Location:** `crates/kernel/src/routes/password_reset.rs:224`

POST `/user/password-reset/{token}` — No `require_csrf()`. However, the password reset token itself is single-use and time-limited (1 hour), which provides some protection.

#### MEDIUM: AJAX Endpoint

**Location:** `crates/kernel/src/routes/admin.rs:269-318`

POST `/system/ajax` — Requires admin auth via `require_admin()` but no explicit CSRF token in `AjaxRequest` struct (`form/ajax.rs:200-209`).

#### PROTECTED: Session Cookie Configuration

**Location:** `crates/kernel/src/session.rs:34-40`

- `with_secure(true)` — HTTPS only
- `with_http_only(true)` — No JavaScript access
- `with_same_site(same_site)` — Configurable, defaults to "strict"
- Default SameSite: "strict" in `config.rs:96-98`

#### PROTECTED: Form-Based Admin Routes

All 49 admin form POST handlers use `require_csrf()` via `CsrfOnlyForm` pattern. Well-covered.

### CSRF Token Implementation

**Location:** `crates/kernel/src/form/csrf.rs`

- SHA256-hashed tokens with timestamp
- Single-use (removed after verification)
- 1-hour validity window
- Max 10 tokens per session
- Stored in Redis session

### Recommended Fix Approach

For JSON API endpoints, two approaches:
1. **Custom header token** — Client sends CSRF token in `X-CSRF-Token` header. Server validates. This leverages CORS Same-Origin policy since custom headers require preflight.
2. **JSON body token** — Include `_token` field in JSON request body. Simpler but requires body parsing.

Option 1 is recommended as it's the standard approach for SPA-style CSRF protection and doesn't require modifying request body structures.

### References

- [Source: crates/kernel/src/routes/auth.rs — Logout handler (GET)]
- [Source: crates/kernel/src/routes/comment.rs — Comment API endpoints]
- [Source: crates/kernel/src/routes/api_token.rs — API token endpoints]
- [Source: crates/kernel/src/routes/helpers.rs — require_csrf helper]
- [Source: crates/kernel/src/form/csrf.rs — CSRF token implementation]
- [Source: crates/kernel/src/session.rs — Session cookie configuration]
