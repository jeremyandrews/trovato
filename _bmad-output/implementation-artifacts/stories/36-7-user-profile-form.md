# Story 36.7: User Profile Form

Status: done

## Story

As an **authenticated user**,
I want to edit my profile, change my password, and set my timezone,
so that I can keep my account information current without admin intervention.

## Acceptance Criteria

1. GET `/user/profile` renders a profile edit form with current user data (name, email, timezone)
2. POST `/user/profile` validates and updates user profile fields
3. Password change requires current password verification before accepting new password
4. Changing username or email requires current password confirmation
5. CSRF protection on both profile and password forms
6. Rate limiting on profile updates and password changes
7. Account lockout integration for failed password verification attempts
8. Timezone validation ensures valid IANA timezone identifiers

## Tasks / Subtasks

- [x] Implement profile_form handler for GET /user/profile (AC: #1)
- [x] Implement profile_update handler for POST /user/profile (AC: #2, #4)
- [x] Add current password verification for credential changes (AC: #3, #4)
- [x] Generate separate CSRF tokens for profile and password forms (AC: #5)
- [x] Add rate limiting via state.rate_limiter().check("profile", ...) (AC: #6)
- [x] Integrate lockout tracking for failed password attempts (AC: #7)
- [x] Add timezone validation with is_valid_timezone() (AC: #8)
- [x] Render profile template with success/error messages

## Dev Notes

### Architecture

Profile handling lives in `routes/auth.rs` alongside other authentication routes. Two separate forms on the same page with independent CSRF tokens (generated via `profile_csrf_pair()`):
- **Profile form**: Updates name, email, and timezone. Changing name or email requires current password confirmation (defense against session hijacking).
- **Password form**: Requires current password, new password, and confirmation.

Rate limiting uses the "profile" and "password" categories from `RateLimitConfig` (10/min and 5/min respectively). Failed password verification records a lockout attempt via `state.lockout().record_failed_attempt()`.

Validation includes: username format validation, email format validation, timezone format validation against IANA identifiers, and password minimum length (12 characters per security rules).

### Testing

- Profile form rendering tested in integration tests
- Profile update with validation errors tested
- Password change flow tested with correct/incorrect current passwords

### References

- `crates/kernel/src/routes/auth.rs` -- profile_form, profile_update, profile_csrf_pair handlers
- `templates/user/profile.html` -- Profile edit template
