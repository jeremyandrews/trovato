# Story 28.2: User Self-Service Account Management

Status: review

## Story

As an **authenticated user**,
I want to change my password and edit my profile,
So that I can manage my own account without admin help.

## Acceptance Criteria

1. GET `/user/profile` renders the current user's profile with edit form
2. POST `/user/profile` updates display name and email (requires current password confirmation)
3. POST `/user/password` changes password (requires current password, new password + confirm)
4. Email change triggers verification email to new address
5. `tap_user_update` dispatched on profile changes
6. CSRF protection on all forms

## Tasks / Subtasks

- [x] Add profile view/edit handler (GET/POST `/user/profile`) (AC: #1, #2, #6)
- [x] Add password change handler (POST `/user/password`) (AC: #3, #6)
- [x] Email change verification flow (AC: #4)
- [x] Dispatch `tap_user_update` on profile changes (AC: #5)
- [x] Write integration tests

## Dev Notes

### Dependencies

- Depends on Story 28.1 for email verification token infrastructure
- `tap_user_update` already dispatched in admin user update handler
- User model already has all needed fields

### Key Files

- `crates/kernel/src/routes/auth.rs` — add profile/password endpoints
- `templates/user/profile.html` — profile edit form template
- `crates/kernel/src/services/email.rs` — added `site_url()` accessor

### Code Review Fixes Applied

- **Email change verification** — email changes now store `pending_email` in user data and send verification email to new address; email updated only after clicking verification link at `/user/verify-email/{token}`
- **Form value preservation** — `render_profile` accepts optional `values` parameter; submitted form data preserved on validation errors

## Dev Agent Record

### Implementation Plan

All implementation was completed in a prior session. This session verified each AC against the codebase and added integration tests.

### Completion Notes

- **AC #1**: `profile_form()` at auth.rs:1068 renders `user/profile.html` with username, email, timezone fields and CSRF token; requires authentication via `get_current_user()`
- **AC #2**: `profile_update()` at auth.rs:1081 validates CSRF, updates display name; email changes require current password confirmation and trigger pending_email flow
- **AC #3**: `password_change()` at auth.rs:1330 validates current password, requires min 8 chars for new password, confirms match
- **AC #4**: Email changes store `pending_email` in user.data JSONB, create verification token, send email with `/user/verify-email/{token}` link; `verify_email_change()` at auth.rs:887 confirms the change
- **AC #5**: `tap_user_update` dispatched in both `profile_update()` (auth.rs:1272) and `password_change()` (auth.rs:1399)
- **AC #6**: CSRF token generated, rendered in both profile and password forms, verified on POST in both handlers
- **Integration tests**: 6 tests added covering authentication requirement, profile rendering, password change (success + wrong password + mismatch), and display name update
- All 653 unit tests pass, clippy clean, fmt clean

## File List

- `crates/kernel/src/routes/auth.rs` — profile/password handlers, email change verification
- `crates/kernel/src/services/email.rs` — `site_url()` accessor for verification URLs
- `templates/user/profile.html` — profile edit and password change forms
- `crates/kernel/tests/integration_test.rs` — 6 new profile/password integration tests

## Change Log

- 2026-02-21: Story implementation verified, integration tests added, story marked for review
