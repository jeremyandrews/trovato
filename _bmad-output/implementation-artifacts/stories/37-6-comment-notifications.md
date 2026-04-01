# Story 37.6: Comment Notifications

Status: done

## Story

As a **subscribed user**,
I want to be notified when new comments are posted on conferences I follow,
so that I can stay engaged in active discussions without polling each conference page.

## Acceptance Criteria

1. When a comment is created on a conference, subscribers to that conference are identified via `Subscription::list_subscribers()`
2. Notification items are enqueued to the `ritrovo_notifications` queue for async delivery
3. The comment author is excluded from receiving a notification about their own comment
4. Notification payload includes item_id, comment_id, author_id, and item title
5. The ritrovo_notify plugin's queue worker processes notification delivery

## Tasks / Subtasks

- [x] Hook comment creation to look up subscribers for the parent item (AC: #1)
- [x] Enqueue notification items for each subscriber to ritrovo_notifications queue (AC: #2)
- [x] Exclude the comment author from the subscriber notification list (AC: #3)
- [x] Include item_id, comment_id, author_id, and title in notification payload (AC: #4)
- [x] Wire ritrovo_notify tap_queue_worker for notification processing (AC: #5)

## Dev Notes

### Architecture

Comment notifications tie together three systems:
1. **Comment creation** (`routes/comment.rs`): After a comment is successfully created, the handler queries `Subscription::list_subscribers()` for the parent item.
2. **Queue infrastructure** (`host/queue.rs`): Notification payloads are pushed to the `ritrovo_notifications` queue declared by the ritrovo_notify plugin.
3. **Queue worker** (ritrovo_notify plugin): The kernel's cron task drains the queue and dispatches `tap_queue_worker` to the plugin for actual delivery (email, in-app, etc.).

The author exclusion ensures users are not notified of their own comments. This is a filter applied at enqueue time, not at delivery time, to avoid wasting queue capacity.

### Testing

- End-to-end flow tested: create comment -> verify notification enqueued
- Author exclusion tested
- Subscriber lookup tested via Subscription model tests

### References

- `crates/kernel/src/routes/comment.rs` -- Comment creation with notification dispatch
- `crates/kernel/src/models/subscription.rs` -- list_subscribers()
- `plugins/ritrovo_notify/src/lib.rs` -- Queue declaration and worker
- `crates/kernel/src/host/queue.rs` -- Queue push infrastructure
