# Story 28.1: Public User Registration

Status: review

## Story

As a **visitor**,
I want to create an account on the site,
So that I can access authenticated features without admin intervention.

## Acceptance Criteria

1. GET `/user/register` renders a registration form (username, email, password, confirm password)
2. POST `/user/register` creates a new user with `active=false` (pending verification)
3. Registration sends a verification email with a time-limited token
4. GET `/user/verify/{token}` activates the user account
5. Registration rate-limited (prevent mass account creation)
6. `tap_user_register` dispatched on successful registration
7. CSRF protection on the registration form
8. JSON API endpoint at POST `/user/register/json` for headless use

## Tasks / Subtasks

- [x] Add registration form handler (GET `/user/register`) (AC: #1, #7)
- [x] Add registration submit handler (POST `/user/register`) (AC: #2, #7)
- [x] Create email verification token model and migration (AC: #3)
- [x] Send verification email via EmailService (AC: #3)
- [x] Add verification endpoint (GET `/user/verify/{token}`) (AC: #4)
- [x] Add rate limiting on registration (AC: #5)
- [x] Dispatch `tap_user_register` on success (AC: #6)
- [x] Add JSON API endpoint for headless registration (AC: #8)
- [x] Write integration tests

## Dev Notes

### Dependencies

- EmailService already exists in `crates/kernel/src/services/email.rs`
- `tap_user_register` already declared in KNOWN_TAPS
- Password reset token model in `crates/kernel/src/models/password_reset.rs` can serve as reference for verification tokens
- Rate limiting middleware already exists from Epic 14

### Key Files

- `crates/kernel/src/routes/auth.rs` — add registration endpoints
- `crates/kernel/src/models/` — new verification token model
- `crates/kernel/migrations/` — new migration for verification tokens
- `templates/user/register.html` — registration form template

### Code Review Fixes Applied

- **Rate limiting enforced** — `state.rate_limiter().check("login", ...)` added to both form and JSON registration handlers
- **Email enumeration mitigated** — changed duplicate email error to generic "Username or email is already in use"
- **JSON password confirmation** — added optional `confirm_password` field to `RegisterJsonRequest`; validated when provided
- **Token logging removed** — removed `plain_token` and `verify_url` from debug log messages

## Dev Agent Record

### Implementation Plan

All implementation was completed in a prior session. This session verified each AC against the codebase and added missing integration tests.

### Completion Notes

- **AC #1**: `register_form()` at auth.rs:489 renders `user/register.html` with username, mail, password, confirm_password fields and CSRF token
- **AC #2**: `register_form_submit()` at auth.rs:552 validates CSRF, input, creates user via `User::create_with_status(pool, input, 0)` (inactive)
- **AC #3**: `EmailVerificationToken` model in `models/email_verification.rs` with 24h expiry, SHA-256 hashed tokens; `do_register()` creates token and calls `email_service.send_verification_email()`
- **AC #4**: `verify_email()` at auth.rs:813 validates token, sets user status=1 (active), marks token used, invalidates remaining tokens
- **AC #5**: Rate limiting via `state.rate_limiter().check("login", &client_id)` on both form (auth.rs:562) and JSON (auth.rs:746) handlers
- **AC #6**: `tap_user_register` dispatched in `do_register()` at auth.rs:721-728 with `{ "user_id": ... }` payload
- **AC #7**: CSRF token generated via `generate_csrf_token()`, embedded in form as `_token`, verified on POST via `verify_csrf_token()`
- **AC #8**: `register_json()` at auth.rs:739 accepts JSON `{ username, mail, password, confirm_password? }`, returns `{ success, message }`
- **Integration tests**: 7 tests added covering registration form rendering, JSON registration, validation, disabled state, email verification, and invalid token handling
- All 653 unit tests pass, clippy clean, fmt clean

## File List

- `crates/kernel/src/routes/auth.rs` — registration handlers, validation, verification endpoints
- `crates/kernel/src/models/email_verification.rs` — EmailVerificationToken model with create/find/mark_used/cleanup
- `crates/kernel/src/models/mod.rs` — EmailVerificationToken export
- `crates/kernel/migrations/20260223000001_create_email_verification_tokens.sql` — verification token table
- `templates/user/register.html` — registration form template
- `crates/kernel/tests/integration_test.rs` — 7 new registration integration tests

## Change Log

- 2026-02-21: Story implementation verified, integration tests added, story marked for review
