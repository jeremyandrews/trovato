# Story 5.3: User Profile Page

Status: ready-for-dev

## Story

As a registered user,
I want a profile page at `/user/{username}`,
So that other users can see my display name and bio.

## Acceptance Criteria

1. `/user/jdoe` displays user's display name and bio
2. `/user/nonexistent` returns 404
3. Logged-in user can view own profile

## Tasks / Subtasks

- [ ] Verify/create profile route at `/user/{username}` (AC: #1)
- [ ] Create/update `templates/user/profile.html` (AC: #1)
- [ ] Return 404 via `render_not_found()` for unknown users (AC: #2)
- [ ] Verify own-profile viewing (AC: #3)

## Dev Notes

- Use `render_not_found()` from `crate::routes::helpers` for 404
- User data: display_name, bio from users table
- Profile template: `templates/user/profile.html`

### References

- [Source: docs/tutorial/plan-parts-03-04.md#Part 4 Step 1] — user profile
