# Story 28.1: Public User Registration

Status: ready-for-dev

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

- [ ] Add registration form handler (GET `/user/register`) (AC: #1, #7)
- [ ] Add registration submit handler (POST `/user/register`) (AC: #2, #7)
- [ ] Create email verification token model and migration (AC: #3)
- [ ] Send verification email via EmailService (AC: #3)
- [ ] Add verification endpoint (GET `/user/verify/{token}`) (AC: #4)
- [ ] Add rate limiting on registration (AC: #5)
- [ ] Dispatch `tap_user_register` on success (AC: #6)
- [ ] Add JSON API endpoint for headless registration (AC: #8)
- [ ] Write integration tests

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
