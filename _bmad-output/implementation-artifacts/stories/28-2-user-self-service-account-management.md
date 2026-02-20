# Story 28.2: User Self-Service Account Management

Status: ready-for-dev

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

- [ ] Add profile view/edit handler (GET/POST `/user/profile`) (AC: #1, #2, #6)
- [ ] Add password change handler (POST `/user/password`) (AC: #3, #6)
- [ ] Email change verification flow (AC: #4)
- [ ] Dispatch `tap_user_update` on profile changes (AC: #5)
- [ ] Write integration tests

## Dev Notes

### Dependencies

- Depends on Story 28.1 for email verification token infrastructure
- `tap_user_update` already dispatched in admin user update handler
- User model already has all needed fields

### Key Files

- `crates/kernel/src/routes/auth.rs` — add profile/password endpoints
- `templates/user/profile.html` — profile edit form template
