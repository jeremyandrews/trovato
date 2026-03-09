# Story 5.1: User Registration & Login

Status: ready-for-dev

## Story

As a site visitor,
I want to register for an account and log in,
So that I can access authenticated features of the site.

## Acceptance Criteria

1. Registration creates account in pending/unapproved state
2. Password stored as Argon2id hash (m=65536, t=3, p=4) with minimum 12 characters
3. Login with correct credentials authenticates and redirects to home
4. Login with wrong credentials shows generic error (no username enumeration)
5. Logout via POST destroys session and redirects
6. GET logout rejected (must be POST)

## Tasks / Subtasks

- [ ] Configure registration mode: `variable.user_registration.yml` — open with admin approval (AC: #1)
- [ ] Verify registration form at `/user/register` (AC: #1)
- [ ] Verify Argon2id params and minimum password length (AC: #2)
- [ ] Verify login flow at `/user/login` (AC: #3, #4)
- [ ] Verify POST-only logout (AC: #5, #6)
- [ ] Import registration config

## Dev Notes

### Architecture

- Auth routes: `crates/kernel/src/routes/auth.rs`
- Password hashing: Argon2id with RFC 9106 params — DO NOT weaken
- Session: Redis-backed via `SESSION_USER_ID` from `crate::routes::auth`
- CSRF: all POST endpoints use `require_csrf` from `crate::routes::helpers`
- Users table exists with admin user from Part 1 installer

### Security (CRITICAL)

- Argon2id params: m=65536, t=3, p=4 — DO NOT reduce
- Minimum password: 12 characters — DO NOT reduce
- Logout MUST be POST with CSRF token
- Generic login error: never reveal if username exists

### References

- [Source: docs/design/Design-Infrastructure.md] — auth design
- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 1] — user auth
