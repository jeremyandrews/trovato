# Story 37.1: Comment Item Type with Threaded Display

Status: done

## Story

As a **site visitor**,
I want to post and read threaded comments on conference pages,
so that I can participate in discussions about conferences with proper conversation structure.

## Acceptance Criteria

1. Comment model with fields: id (UUIDv7), item_id, parent_id (nullable for threading), author_id, body, body_format, status, created, changed, depth
2. REST API endpoints: list comments (GET), create comment (POST), get single comment (GET), update comment (PUT), delete comment (DELETE)
3. Comment creation computes thread depth from parent comment
4. Comment body rendered through `FilterPipeline::for_format_safe()` for XSS prevention
5. Comment list template displays threaded comments with indentation based on depth
6. Reply links set parent_id for threaded replies
7. Login prompt shown to unauthenticated users
8. Comment form with CSRF-protected submission

## Tasks / Subtasks

- [x] Define Comment, CreateComment, UpdateComment models in `models/comment.rs` (AC: #1)
- [x] Implement Comment::create() with UUIDv7, depth computation from parent (AC: #1, #3)
- [x] Implement Comment::find_by_id(), list_for_item(), update(), delete() (AC: #1)
- [x] Create comment REST routes: list, create, get, update, delete (AC: #2)
- [x] Add render_comment_body() helper using for_format_safe() (AC: #4)
- [x] Build CommentResponse with body_html and optional AuthorInfo (AC: #2)
- [x] Create comments.html template with threaded display and depth indentation (AC: #5, #6)
- [x] Add reply link with data-parent-id and JavaScript handler (AC: #6)
- [x] Show login prompt for unauthenticated users (AC: #7)
- [x] Wire CSRF protection via require_csrf_header on POST endpoints (AC: #8)

## Dev Notes

### Architecture

Comments use a flat storage model with `parent_id` for threading and `depth` for display indentation. The `depth` field is computed on insert based on the parent comment's depth + 1 (top-level comments have depth 0).

- **Model** (`models/comment.rs`, 293 lines): Standard CRUD with `sqlx::FromRow`. `CreateComment` accepts optional `parent_id`, `body_format` (defaults to "filtered_html"), and `status` (defaults to 1/published).
- **Routes** (`routes/comment.rs`, 745 lines): Full REST API at `/api/item/{id}/comments` (list, create) and `/api/comment/{id}` (get, update, delete). Includes `CommentResponse` and `CommentListResponse` envelopes with optional author info expansion via `?include=author`.
- **Template** (`templates/elements/comments.html`, 74 lines): Server-rendered comment section with CSS-based depth indentation (`margin-left: depth * 2rem`). Reply links use JavaScript event delegation to set `parent_id` on the comment form. Cancel button clears the reply state.

Comment body HTML uses `{# SAFE: #}` justification per security rules -- body is pre-sanitized through `FilterPipeline::for_format_safe()` before template rendering.

### Testing

- Comment CRUD tested via integration tests through REST API
- Threaded depth computation tested
- Format whitelisting verified in XSS audit (Story 27.1)

### References

- `crates/kernel/src/models/comment.rs` (293 lines) -- Comment model and CRUD
- `crates/kernel/src/routes/comment.rs` (745 lines) -- REST API routes
- `templates/elements/comments.html` (74 lines) -- Threaded comment template
