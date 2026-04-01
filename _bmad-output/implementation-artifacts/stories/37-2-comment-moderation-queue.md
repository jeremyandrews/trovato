# Story 37.2: Comment Moderation Queue

Status: done

## Story

As an **administrator**,
I want to view, approve, unpublish, and delete comments from an admin interface,
so that I can moderate community discussions and remove inappropriate content.

## Acceptance Criteria

1. Admin comment list at `/admin/content/comments` displays all comments with status
2. Approve action at POST `/admin/content/comments/{id}/approve` sets comment status to published
3. Unpublish action at POST `/admin/content/comments/{id}/unpublish` sets comment status to unpublished
4. Delete action at POST `/admin/content/comments/{id}/delete` permanently removes a comment
5. Edit form at GET `/admin/content/comments/{id}/edit` allows body/status modification
6. All moderation actions require admin authentication
7. All state-changing actions require CSRF verification

## Tasks / Subtasks

- [x] Add admin comment list handler rendering comments with moderation controls (AC: #1)
- [x] Implement approve_comment handler setting status=1 (AC: #2, #6, #7)
- [x] Implement unpublish_comment handler setting status=0 (AC: #3, #6, #7)
- [x] Implement delete_comment_admin handler with permanent deletion (AC: #4, #6, #7)
- [x] Implement edit_comment_form handler for editing comment body/status (AC: #5)
- [x] Wire CSRF verification on all POST handlers via require_csrf (AC: #7)
- [x] Add admin route registrations in admin router (AC: #1-#5)

## Dev Notes

### Architecture

Moderation routes live in `routes/admin.rs` (the main admin router), not in `routes/comment.rs` (which handles the public REST API). This separation keeps admin-only functionality gated behind `require_admin()`.

The `set_comment_status()` helper function is shared between approve and unpublish actions, taking a target status value and operation name for logging. The delete action calls `Comment::delete()` for permanent removal.

All admin comment routes are registered in the admin router:
- GET `/admin/content/comments` -- list with filters
- GET `/admin/content/comments/{id}/edit` -- edit form
- POST `/admin/content/comments/{id}/approve` -- set status=1
- POST `/admin/content/comments/{id}/unpublish` -- set status=0
- POST `/admin/content/comments/{id}/delete` -- permanent delete

### Testing

- Admin comment moderation tested via integration tests
- CSRF verification tested on all state-changing endpoints

### References

- `crates/kernel/src/routes/admin.rs` -- approve_comment, unpublish_comment, delete_comment_admin, edit_comment_form handlers
- `crates/kernel/src/routes/comment.rs` -- Public REST API (separate from moderation)
- `templates/admin/comments.html` -- Admin comment list template
