# Story 27.3: CSRF Audit

Status: review

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

- [x] Fix logout endpoint — change from GET to POST with CSRF (AC: #1, #6)
  - [x] Change `GET /user/logout` to `POST /user/logout` in route registration
  - [x] Add CSRF token validation to logout handler
  - [x] Update logout links/buttons in templates to use POST forms
- [x] Add CSRF protection to JSON API endpoints (AC: #2, #3, #6)
  - [x] Comment API: POST, PUT, DELETE with `X-CSRF-Token` header
  - [x] API Token routes: POST, DELETE with `X-CSRF-Token` header
  - [x] Item CRUD: create, update, delete, revert with `X-CSRF-Token` header
  - [x] File upload: upload_file, block_editor_upload, block_editor_preview
  - [x] Lock endpoints: heartbeat, break_lock
  - [x] Approach: `X-CSRF-Token` custom header validated via `require_csrf_header()`
- [x] Audit AJAX endpoint (AC: #2)
  - [x] Added CSRF header validation to `POST /system/ajax`
  - [x] Added CSRF header validation to `POST /admin/stage/switch`
- [x] Verify session cookie SameSite attribute (AC: #4)
  - [x] Default is "strict" in `config.rs:96-98`
  - [x] `session.rs:37` applies SameSite from config
  - [x] Supports "strict", "lax", "none" via `COOKIE_SAME_SITE` env var
- [x] Audit password reset endpoint (AC: #1)
  - [x] `POST /user/password-reset` — unauthenticated, always returns success (safe)
  - [x] `POST /user/password-reset/{token}` — protected by single-use URL token (equivalent to CSRF)
- [x] Verify all admin form-based POST routes have CSRF (AC: #1)
  - [x] All 49+ `require_csrf` call sites cover all admin form operations
- [x] Document all findings with severity ratings (AC: #5, #6)

## Findings Summary

### Fixed (Critical/High)

| # | Severity | Endpoint | Issue | Fix |
|---|----------|----------|-------|-----|
| 1 | CRITICAL | `GET /user/logout` | Logout via GET, no CSRF. `<img src="/user/logout">` forces logout. | Changed to POST with CSRF token validation. Updated templates. |
| 2 | HIGH | `POST /api/item/{id}/comments` | No CSRF on comment create | Added `X-CSRF-Token` header validation |
| 3 | HIGH | `PUT /api/comment/{id}` | No CSRF on comment update | Added `X-CSRF-Token` header validation |
| 4 | HIGH | `DELETE /api/comment/{id}` | No CSRF on comment delete | Added `X-CSRF-Token` header validation |
| 5 | HIGH | `POST /api/tokens` | No CSRF on API token create | Added `X-CSRF-Token` header validation |
| 6 | HIGH | `DELETE /api/tokens/{id}` | No CSRF on API token revoke | Added `X-CSRF-Token` header validation |
| 7 | HIGH | `POST /item/add/{type}` | No CSRF on item create | Added `X-CSRF-Token` header validation |
| 8 | HIGH | `POST /item/{id}/edit` | No CSRF on item update | Added `X-CSRF-Token` header validation |
| 9 | HIGH | `POST /item/{id}/delete` | No CSRF on item delete | Added `X-CSRF-Token` header validation |
| 10 | HIGH | `POST /item/{id}/revert/{rev_id}` | No CSRF on revision revert | Added `X-CSRF-Token` header validation |
| 11 | HIGH | `POST /file/upload` | No CSRF on file upload | Added `X-CSRF-Token` header validation |
| 12 | HIGH | `POST /api/block-editor/upload` | No CSRF on block editor upload | Added `X-CSRF-Token` header validation |
| 13 | HIGH | `POST /api/block-editor/preview` | No CSRF on block editor preview | Added `X-CSRF-Token` header validation |
| 14 | MEDIUM | `POST /system/ajax` | No CSRF on AJAX callback | Added `X-CSRF-Token` header validation |
| 15 | MEDIUM | `POST /admin/stage/switch` | No CSRF on stage switch | Added `X-CSRF-Token` header validation |
| 16 | MEDIUM | `POST /api/lock/heartbeat` | No CSRF on lock heartbeat | Added `X-CSRF-Token` header validation |
| 17 | MEDIUM | `POST /api/lock/break` | No CSRF on lock break | Added `X-CSRF-Token` header validation |

### Acceptable (Low/No Fix Required)

| # | Severity | Endpoint | Assessment |
|---|----------|----------|------------|
| 18 | LOW | `POST /user/password-reset` | Unauthenticated. Always returns success. Side-effect limited to email sending. |
| 19 | LOW | `POST /user/password-reset/{token}` | Protected by single-use URL token (equivalent to CSRF protection). |
| 20 | INFO | `POST /api/category`, `POST /api/tag` | No session auth. Pure API endpoints without cookie authentication. |
| 21 | INFO | `POST /api/batch`, `POST /api/batch/{id}/cancel` | No session auth. Pure API endpoints. |
| 22 | INFO | `POST /oauth/token`, `POST /oauth/revoke` | Client credentials auth (client_id/client_secret), not session cookies. |
| 23 | INFO | `POST /cron/{key}` | CRON_KEY env var auth, not session cookies. |
| 24 | LOW | `POST /api/gather/query` | Read-only query (SELECT). Optional session for user context. CSRF cannot exfiltrate response data. |
| 25 | INFO | `POST /install/*` | One-time installer endpoints. No session auth during setup. |

### Already Protected

- **49+ admin form POST routes** — All use `require_csrf()` via `CsrfOnlyForm` pattern
- **Session cookies** — SameSite=Strict by default, Secure, HttpOnly
- **CSRF token implementation** — SHA256-hashed, single-use, 1-hour TTL, max 10 per session

## Implementation Details

### New Helper: `require_csrf_header()`

Added `routes/helpers.rs:189-210`: Validates `X-CSRF-Token` custom header against session CSRF tokens. Returns JSON error on failure. Used by all JSON API endpoints with session auth.

### CSRF Token Availability in Templates

Added CSRF token generation in `inject_site_context()` (helpers.rs:102-107) for all authenticated users. This makes `{{ csrf_token }}` available in any page template that uses the shared context (front page, item views, etc.).

Added explicit CSRF token generation in the admin dashboard handler so the logout form works correctly.

### Auth-Before-CSRF Ordering

All handlers check authentication before CSRF validation. This ensures:
- Unauthenticated requests get 401 (not 403 CSRF error)
- CSRF only validated for authenticated sessions where it's relevant

### Files Changed

- `crates/kernel/src/routes/helpers.rs` — `require_csrf_header()`, CSRF in `inject_site_context()`
- `crates/kernel/src/routes/auth.rs` — Logout changed GET→POST with CSRF
- `crates/kernel/src/routes/comment.rs` — CSRF on create/update/delete
- `crates/kernel/src/routes/api_token.rs` — CSRF on create/delete
- `crates/kernel/src/routes/item.rs` — CSRF on create/update/delete/revert
- `crates/kernel/src/routes/file.rs` — CSRF on upload/block-editor endpoints
- `crates/kernel/src/routes/admin.rs` — CSRF on ajax_callback, switch_stage, dashboard CSRF token
- `crates/kernel/src/routes/lock.rs` — CSRF on heartbeat/break_lock
- `templates/page.html` — Logout form with CSRF
- `templates/admin/dashboard.html` — Logout form with CSRF
- `crates/kernel/tests/integration_test.rs` — Updated tests with CSRF headers

### Test Coverage

- 82 integration tests pass (all existing + CSRF header additions)
- All unit tests pass across all crates
- `cargo clippy --all-targets -- -D warnings` clean
- `cargo fmt --all --check` clean
