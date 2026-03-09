# Story 5.2: Session Management & Fixation Protection

Status: ready-for-dev

## Story

As a site user,
I want my session to be secure against hijacking and fixation attacks,
So that my account cannot be compromised.

## Acceptance Criteria

1. Session cookie: HttpOnly, Secure, SameSite=Strict
2. Session data stored in Redis (not cookie)
3. Session ID cycled after login (fixation protection)
4. Session ID invalidated on logout
5. CSRF protection on all POST/PUT/DELETE endpoints

## Tasks / Subtasks

- [ ] Verify cookie flags: HttpOnly, Secure, SameSite=Strict (AC: #1)
- [ ] Verify Redis session storage (AC: #2)
- [ ] Verify `session.cycle_id()` called after login (AC: #3)
- [ ] Verify session destruction on logout (AC: #4)
- [ ] Verify CSRF on state-changing endpoints (AC: #5)

## Dev Notes

- Session cycling: `session.cycle_id()` after auth state changes
- Redis sessions configured in `crates/kernel/src/` — session middleware
- CSRF: `require_csrf` helper enforces on all POST/PUT/DELETE
- See CLAUDE.md § Authentication & Sessions

### References

- [Source: docs/design/Design-Infrastructure.md] — session design
