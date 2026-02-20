# Story 27.4: Authentication and Session Security Audit

Status: done

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

- [x] Verify Argon2id configuration parameters (AC: #1)
  - [x] Check if `Argon2::default()` parameters meet RFC 9106 recommendations
  - [x] Configure explicit parameters: 64 MiB memory, 3 iterations, parallelism 4
- [x] Verify session ID rotation on login (AC: #2)
  - [x] `setup_session()` was missing `session.cycle_id()`
  - [x] Added `session.cycle_id().await` before storing user data
- [x] Verify session cookie flags (AC: #3)
  - [x] Confirmed HttpOnly, Secure, SameSite=Strict in `session.rs:34-40`
- [x] Verify login rate limiting / account lockout (AC: #4)
  - [x] Confirmed 5 attempts, 15-minute window in `lockout.rs`
  - [x] Confirmed lockout check before password verification in `auth.rs`
  - [x] Verified timing-attack prevention (non-existent user still records attempt)
- [x] Audit all routes for authentication requirements (AC: #5)
  - [x] Verified install routes double-gated: middleware + per-handler `check_installed()`
  - [x] Verified all 62+ admin routes use `require_admin()`
  - [x] Verified plugin-gated routes protected via `plugin_gate!()` macros
  - [x] Assessed file metadata endpoint `GET /file/{id}` (documented as LOW)
- [x] Verify stage-based access control enforcement (AC: #6)
  - [x] Verified stage ancestry chain with cycle detection (HashSet + max 10 iterations)
  - [x] Verified stage permissions checked at handler level via `require_admin()`
- [x] Audit password reset flow for security (AC: #7)
  - [x] Token: SHA-256 hashed before storage
  - [x] 1-hour token expiry
  - [x] Single-use (marked used after consumption)
  - [x] User enumeration prevention (always returns success)
  - [x] All user tokens invalidated after successful reset
  - [x] Added missing password length validation (8 char minimum)
- [x] Check password strength validation (AC: #1)
  - [x] Install form enforces 8 chars minimum
  - [x] Admin user create/edit enforces 8 chars minimum
  - [x] Password reset was missing validation — fixed
- [x] Document all findings with severity ratings (AC: #8, #9)

## Findings Summary

### Fixed (Critical/High)

| # | Severity | Location | Issue | Fix |
|---|----------|----------|-------|-----|
| 1 | HIGH | `user.rs:hash_password()` | Argon2id using crate defaults (19 MiB, 2 iter, 1 thread) below RFC 9106 recommendations | Configured explicit `argon2_instance()` with 64 MiB, 3 iterations, 4 parallelism lanes |
| 2 | HIGH | `auth.rs:setup_session()` | No session ID rotation on login. Vulnerable to session fixation. | Added `session.cycle_id().await` before storing user data |
| 3 | HIGH | `user.rs:verify_password()` | Using `Argon2::default()` for verification (worked but inconsistent) | Updated to use `argon2_instance()` for consistent RFC 9106 params |

### Fixed (Medium)

| # | Severity | Location | Issue | Fix |
|---|----------|----------|-------|-----|
| 4 | MEDIUM | `password_reset.rs:set_password()` | No password length validation on reset. User could set 1-char password bypassing other forms' 8-char minimum. | Added 8-char minimum validation before password update |

### Acceptable (Low/No Fix Required)

| # | Severity | Location | Assessment |
|---|----------|----------|------------|
| 5 | LOW | `GET /file/{id}` | Returns file metadata without auth. UUIDs prevent enumeration. Files are already publicly accessible via storage URL. Requiring auth would break headless CMS API patterns. |
| 6 | LOW | Password policy | 8-char minimum enforced at all entry points (install, admin, reset). No complexity requirements (uppercase, digits, special chars). Acceptable for v1.0 with account lockout protection. |

### Already Protected

| # | Aspect | Status | Details |
|---|--------|--------|---------|
| 7 | Session cookies | PROTECTED | HttpOnly, Secure, SameSite=Strict by default |
| 8 | Account lockout | PROTECTED | 5 failed attempts, 15-min lockout, timing-attack safe |
| 9 | Password reset tokens | PROTECTED | SHA-256 hashed, 1-hour expiry, single-use, no enumeration |
| 10 | Admin routes | PROTECTED | All 62+ routes use `require_admin()` |
| 11 | Install routes | PROTECTED | Double-gated: middleware redirect + per-handler `check_installed()` |
| 12 | Stage access control | PROTECTED | Cycle detection (HashSet + iteration limit), handler-level permissions |
| 13 | Plugin-gated routes | PROTECTED | `plugin_gate!()` macros with middleware-level protection |

## Implementation Details

### Argon2id RFC 9106 Configuration

Added `argon2_instance()` function in `models/user.rs` that creates an Argon2id hasher with:
- **Algorithm:** Argon2id (v0x13)
- **Memory:** 64 MiB (64 * 1024 KiB)
- **Iterations:** 3 (time cost)
- **Parallelism:** 4 lanes

Both `hash_password()` and `verify_password()` now use `argon2_instance()` instead of `Argon2::default()`. Existing password hashes remain compatible because argon2 verification reads parameters from the hash string itself.

### Session Fixation Prevention

Added `session.cycle_id().await` as the first operation in `setup_session()`, before any user data is stored. This rotates the session ID on every successful login, preventing session fixation attacks where an attacker sets a known session cookie before the victim authenticates.

### Password Reset Validation

Added 8-character minimum password length check to `set_password()` in `password_reset.rs`, matching the validation enforced by the installer and admin user forms.

### Files Changed

- `crates/kernel/src/models/user.rs` — `argon2_instance()` with RFC 9106 params, updated test
- `crates/kernel/src/routes/auth.rs` — `session.cycle_id()` in `setup_session()`
- `crates/kernel/src/routes/password_reset.rs` — Password length validation in `set_password()`

### Test Coverage

- Password hashing unit test updated to verify Argon2id algorithm identifier and use configured instance
- All 82 integration tests pass
- All unit tests pass across all crates
- `cargo clippy --all-targets -- -D warnings` clean
- `cargo fmt --all --check` clean
