# Story 42.6: Session Rotation on Privilege Escalation

Status: ready-for-dev

## Story

As a security-conscious platform,
I want session tokens rotated on privilege changes,
so that stolen session tokens become invalid after a user's access level changes.

## Acceptance Criteria

1. `session.cycle_id()` called after login (already implemented -- verify still present)
2. `session.cycle_id()` called after `is_admin` status change on a user
3. `session.cycle_id()` called after role assignment to a user
4. `session.cycle_id()` called after role removal from a user
5. Admin user management in `admin_user.rs` triggers session rotation for the affected user
6. When roles/admin status change for OTHER users (not the acting admin), their active sessions are invalidated via Redis session key deletion
7. At least 2 integration tests covering privilege escalation scenarios

## Tasks / Subtasks

- [ ] Verify `session.cycle_id()` is still called after login (AC: #1)
- [ ] Add session rotation after `is_admin` change in `admin_user.rs` (AC: #2, #5)
- [ ] Add session rotation after role assignment in `admin_user.rs` (AC: #3, #5)
- [ ] Add session rotation after role removal in `admin_user.rs` (AC: #4, #5)
- [ ] Implement Redis session invalidation for other users' sessions on privilege change (AC: #6)
- [ ] Write integration test: admin grants role to user, user's old session is invalidated (AC: #3, #6, #7)
- [ ] Write integration test: admin revokes admin status, affected user's session is invalidated (AC: #2, #6, #7)

## Dev Notes

### Architecture

For the acting user's own session, `session.cycle_id()` generates a new session ID while preserving session data. For other users whose privileges are changed by an admin, we cannot call `cycle_id()` on their session -- instead, we delete their session keys from Redis, forcing re-authentication on their next request. This requires knowing the session key format used by the session store (typically `session:{session_id}`).

To find a user's active sessions, we need either: (a) a reverse index from user_id to session_ids in Redis, or (b) scan Redis keys and check session data. Option (a) is more efficient -- consider maintaining a Redis set `user_sessions:{user_id}` that tracks active session IDs.

### Security

- Session rotation on privilege change prevents session fixation attacks where an attacker obtains a low-privilege session and then waits for the user to be granted higher privileges.
- Invalidating other users' sessions on role changes ensures that a demoted user cannot continue operating with cached elevated permissions.
- Redis session deletion is immediate -- there is no grace period. The affected user is logged out on their next request.

### Testing

- Test flow: create user, login (capture session cookie), admin grants role, original session cookie returns 401/redirect to login.
- Test flow: create admin user, login, another admin revokes admin status, original session is invalidated.
- Verify the acting admin's own session is NOT invalidated when they change another user's privileges.

### References

- `crates/kernel/src/routes/admin_user.rs` -- admin user management routes
- `crates/kernel/src/routes/auth.rs` -- login session handling
- `docs/ritrovo/epic-12-security.md` -- Epic 42 source
