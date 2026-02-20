# Story 27.4: Authentication and Session Security Audit

Status: ready-for-dev

## Story

As a **security reviewer**,
I want authentication and session management audited for security weaknesses,
So that user accounts and sessions are protected from attack.

## Acceptance Criteria

1. Argon2id usage verified with appropriate parameters (memory, iterations, parallelism)
2. Session ID rotation on login verified (prevents session fixation)
3. Session cookie flags verified (HttpOnly, Secure, SameSite=Strict)
4. Rate limiting on login attempts verified (brute force protection)
5. All authenticated routes verified — no routes accidentally public
6. Stage access control verified — users cannot access stages they lack permission for
7. Password reset flow verified for security (token expiry, no user enumeration)
8. All findings documented with severity ratings
9. All Critical/High findings fixed

## Tasks / Subtasks

- [ ] Verify Argon2id configuration parameters (AC: #1)
  - [ ] Check if `Argon2::default()` parameters meet RFC 9106 recommendations
  - [ ] If not, configure explicit parameters (46-64 MiB memory, 1-3 iterations, parallelism 4)
- [ ] Verify session ID rotation on login (AC: #2)
  - [ ] Check `setup_session()` in `auth.rs:192-243` for session rotation call
  - [ ] If missing, add `session.cycle_id().await` before storing user data
- [ ] Verify session cookie flags (AC: #3)
  - [ ] Confirm HttpOnly, Secure, SameSite settings in `session.rs:34-40`
- [ ] Verify login rate limiting / account lockout (AC: #4)
  - [ ] Confirm `lockout.rs` parameters (5 attempts, 15-minute window)
  - [ ] Confirm lockout check before password verification in `auth.rs`
  - [ ] Verify timing-attack prevention (non-existent user still records attempt)
- [ ] Audit all routes for authentication requirements (AC: #5)
  - [ ] Verify install routes are gated post-installation via `check_installed()` middleware
  - [ ] Verify admin routes all use `require_admin()`
  - [ ] Check for any routes that should require auth but don't
  - [ ] Verify file metadata endpoint `GET /file/{id}` access control
- [ ] Verify stage-based access control enforcement (AC: #6)
  - [ ] Verify stage ancestry chain with cycle detection (`stage.rs:91-110`)
  - [ ] Verify stage permissions are checked on content access
- [ ] Audit password reset flow for security (AC: #7)
  - [ ] Verify token is SHA-256 hashed before storage (not stored in plaintext)
  - [ ] Verify 1-hour token expiry
  - [ ] Verify single-use tokens (marked used after consumption)
  - [ ] Verify user enumeration prevention (always returns success)
  - [ ] Verify all user tokens invalidated after successful reset
- [ ] Check password strength validation (AC: #1)
  - [ ] Determine if password policy enforcement exists
  - [ ] If not, document as finding and recommend minimum requirements
- [ ] Document all findings with severity ratings (AC: #8, #9)

## Dev Notes

### Dependencies

No dependencies on other stories. Can be worked independently.

### Codebase Research Findings

#### HIGH: Argon2id Uses Default Parameters

**Location:** `crates/kernel/src/models/user.rs:303-311`

```rust
Argon2::default()
```

Uses argon2 crate v0.5.3 defaults without explicit configuration. RFC 9106 recommends: 46-64 MiB memory, 1-3 iterations, parallelism 4. The crate defaults (19 MiB, 2 iterations, 1 thread) are functional but below recommended security levels.

#### HIGH: No Session ID Rotation on Login

**Location:** `crates/kernel/src/routes/auth.rs:192-243`

The `setup_session()` function stores user data in the session but does not call `session.cycle_id()` or equivalent to rotate the session ID. This makes the application vulnerable to session fixation attacks — if an attacker can set a session cookie before login, they retain access after authentication.

#### PROTECTED: Session Cookie Flags

**Location:** `crates/kernel/src/session.rs:34-40`

All three critical flags are set:
- `with_http_only(true)` — Prevents XSS-based cookie theft
- `with_secure(true)` — HTTPS only
- `with_same_site(same_site)` — Default "strict"

#### PROTECTED: Login Rate Limiting

**Location:** `crates/kernel/src/lockout.rs`

- 5 failed attempts trigger 15-minute lockout
- Returns 429 Too Many Requests
- Timing-attack prevention: non-existent users also record failed attempts (`auth.rs:280-286`)
- Inactive users also record failed attempts (`auth.rs:297-305`)
- Attempts cleared on successful login

#### PROTECTED: Password Reset Flow

**Location:** `crates/kernel/src/models/password_reset.rs`

Strong implementation:
- Token: 32 random bytes, hex-encoded (64 chars)
- Storage: SHA-256 hash stored, not plaintext
- Expiry: 1 hour
- Single-use: `used_at` timestamp set after consumption
- User enumeration prevention: `request_reset()` always returns success (`password_reset.rs:108`)
- Post-reset: all other tokens for user invalidated

#### NEEDS REVIEW: Install Route Gating

**Location:** `crates/kernel/src/routes/install.rs`

Install routes (`/install`, `/install/welcome`, `/install/admin`, `/install/site`, `/install/complete`) are gated by `check_installed()` helper (lines 46-49). The `check_installation` middleware in `main.rs:212-214` should prevent access post-install, but needs verification.

#### NEEDS REVIEW: File Metadata Endpoint

**Location:** `crates/kernel/src/routes/file.rs:200-223`

`GET /file/{id}` returns file metadata as JSON without authentication. Potentially allows enumeration of uploaded file metadata (filenames, MIME types, sizes, URLs).

#### NEEDS REVIEW: Password Strength Policy

No password strength validation detected in the codebase. Users can set arbitrarily weak passwords. Consider minimum length, complexity requirements.

### Key Files

- `crates/kernel/src/models/user.rs` — Argon2id hashing
- `crates/kernel/src/routes/auth.rs` — Login/logout/session setup
- `crates/kernel/src/session.rs` — Session layer configuration
- `crates/kernel/src/lockout.rs` — Account lockout logic
- `crates/kernel/src/models/password_reset.rs` — Password reset tokens
- `crates/kernel/src/permissions.rs` — Permission service with DashMap cache
- `crates/kernel/src/models/role.rs` — Role model with well-known roles
- `crates/kernel/src/models/stage.rs` — Stage hierarchy with cycle detection
- `crates/kernel/src/routes/install.rs` — Installation routes

### References

- [Source: crates/kernel/src/models/user.rs — Argon2id password hashing]
- [Source: crates/kernel/src/routes/auth.rs — Login/session setup]
- [Source: crates/kernel/src/session.rs — Session cookie configuration]
- [Source: crates/kernel/src/lockout.rs — Account lockout parameters]
- [Source: crates/kernel/src/models/password_reset.rs — Password reset flow]
