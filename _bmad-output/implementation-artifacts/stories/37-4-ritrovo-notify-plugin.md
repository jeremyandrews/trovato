# Story 37.4: ritrovo_notify Plugin

Status: done

## Story

As a **subscribed user**,
I want to receive notifications about conferences I follow,
so that I am informed of new comments and updates without manually checking each conference page.

## Acceptance Criteria

1. Plugin registers 2 permissions: "manage own subscriptions" and "administer notifications"
2. Plugin registers a menu entry at `/user/subscriptions` ("My Subscriptions")
3. `tap_item_view` renders a subscribe/unsubscribe toggle button on conference pages
4. Toggle button only appears for conference items (empty for other types)
5. Plugin declares the `ritrovo_notifications` queue via `tap_queue_info` with retry configuration
6. Queue configuration includes max_retries (3) and retry_delay_seconds (60)

## Tasks / Subtasks

- [x] Register permissions via tap_perm (AC: #1)
- [x] Register menu entry via tap_menu with permission gating (AC: #2)
- [x] Implement tap_item_view with subscribe toggle for conferences (AC: #3, #4)
- [x] Declare notification queue via tap_queue_info (AC: #5, #6)
- [x] Write unit tests for permissions, menu, view toggle, and queue declaration (AC: #1-#6)

## Dev Notes

### Architecture

The ritrovo_notify plugin (132 lines including tests) implements 4 taps:
- **`tap_perm`**: Registers "manage own subscriptions" and "administer notifications" permissions.
- **`tap_menu`**: Adds `/user/subscriptions` menu link gated by "manage own subscriptions".
- **`tap_item_view`**: Renders a `<div class="subscription-toggle">` with a subscribe button for conference items. The actual subscription state check happens at the route handler level, not in the plugin.
- **`tap_queue_info`**: Declares the `ritrovo_notifications` queue with 3 retries and 60s retry delay.

The plugin is a declaration layer -- the actual notification delivery logic runs in the kernel's queue worker infrastructure. The plugin declares what it needs, and the kernel provides the execution environment.

### Testing

5 unit tests: permission count, menu count and path, view toggle for conferences, empty view for non-conferences, queue declaration.

### References

- `plugins/ritrovo_notify/src/lib.rs` (132 lines) -- Full plugin implementation with tests
