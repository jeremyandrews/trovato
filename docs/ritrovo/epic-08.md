# Epic 8: Community & Plugin Communication

**Tutorial Part:** 6
**Trovato Phase Dependency:** Phase 4 (Queue API, Comments), Phase 5 (Subscriptions, Notifications)
**BMAD Epic:** 37
**Status:** Not started

---

## Narrative

*A conference directory without discussion is a database with a UI. Part 6 turns Ritrovo into a community. Users post threaded comments on conferences, subscribe to events they care about, and receive notifications when things change. Behind the scenes, two plugins cooperate through shared queues -- demonstrating the most sophisticated pattern in the Trovato plugin ecosystem: plugin-to-plugin communication without direct dependencies.*

The reader builds the `ritrovo_notify` plugin, which implements subscriptions, notification delivery, and digest emails. They connect it to the `ritrovo_cfp` plugin (from Part 5), which already writes `cfp_closing_soon` events to the notification queue. They add threaded comments with per-item access control and moderation. And they see how the kernel's Queue API and tap dispatch system enable decoupled plugin collaboration.

This is the part where the plugin architecture proves its value. Three plugins -- `ritrovo_access`, `ritrovo_cfp`, and `ritrovo_notify` -- work together without any direct imports or shared code. The kernel dispatches taps, the queue system passes messages, and each plugin remains independently deployable.

---

## Tutorial Steps

### Step 1: Threaded Comments

Add user discussion to conferences. Comments are a third Item Type (`comment`) with self-referencing RecordReference for threading and per-item access control.

**What to cover:**

- Comment Item Type: body (TextValue, `filtered_html`), conference (RecordReference to parent conference), parent (RecordReference to self for threading)
- Comment display: threaded tree below the conference detail page, with author, timestamp, and reply/edit/delete actions
- Recursive CTE query for building the comment tree from the `parent` self-reference
- Access control: authenticated users can post comments (`post comments` permission from `ritrovo_access`), edit own comments (`edit own comments`), editors can edit/delete any comment (`edit any comments`)
- Comment moderation: editors see a moderation queue of recent comments with approve/delete actions
- CSRF protection on comment submission
- `filtered_html` format for comment body -- users get basic formatting but no arbitrary HTML

### Step 2: Subscriptions

Let authenticated users subscribe to conferences they care about, with a personal "My Subscriptions" page.

**What to cover:**

- Subscription storage: join table (`user_subscriptions`) linking user_id to item_id
- Subscribe/unsubscribe toggle on conference detail pages (authenticated users only)
- AJAX toggle: clicking the button subscribes/unsubscribes without full page reload
- My Subscriptions page at `/user/{uid}/subscriptions` (private, own user only)
- Subscription privacy: users can only view their own subscriptions
- Subscription count shown on conference detail pages (optional)
- `ritrovo_notify` plugin registers the subscription route via `tap_menu`

### Step 3: The `ritrovo_notify` Plugin

Build the notification and subscription plugin: queue processing, digest emails, and the My Subscriptions page.

**What to cover:**

- `tap_menu` -- Registers `/user/{uid}/subscriptions` route
- `tap_item_view` -- Injects "Subscribe/Unsubscribe" toggle button on conference detail pages (authenticated users only, via AJAX endpoint)
- `tap_item_update` -- When a subscribed conference changes (dates, venue, CFP status), queues a notification for each subscriber
- `tap_queue_info` -- Declares the `ritrovo_notifications` queue
- `tap_queue_worker` -- Processes notification events: sends email or queues for digest
- `tap_cron` -- Daily: aggregates pending notifications per user and sends digest emails
- SDK features demonstrated: queue declaration and processing, user-context operations, AJAX endpoints from plugins, cron scheduling, email dispatch (console logging for tutorial, SMTP for production)

### Step 4: Plugin-to-Plugin Communication

Connect `ritrovo_cfp` and `ritrovo_notify` through the shared notification queue. Demonstrate the full event flow from CFP deadline detection to user notification delivery.

**What to cover:**

- Event flow: `ritrovo_cfp` detects a CFP entering the 7-day window → writes `cfp_closing_soon` event to `ritrovo_notifications` queue → `ritrovo_notify.tap_queue_worker` picks it up → finds subscribers → sends notification
- No direct dependency: `ritrovo_cfp` writes to a queue name; `ritrovo_notify` reads from it; neither imports the other
- Queue message format: JSON with event type, item_id, metadata (CFP end date, conference name)
- Ordering guarantees: the kernel dispatches taps in plugin weight order; queue processing is serial per queue
- Error handling: failed notifications are logged and retried; poison messages are dead-lettered after N attempts
- Testing the flow: set a conference's `cfp_end_date` to 5 days from now, trigger cron, verify notification queued for subscribers

### Step 5: Comment Notifications

Wire comments into the notification system: new comments on subscribed conferences trigger notifications.

**What to cover:**

- `ritrovo_notify` extends its `tap_item_insert` handler: when a `comment` item is created, check if the parent conference has subscribers, queue notifications
- Notification deduplication: a user who is both subscribed and is the comment author does not receive a self-notification
- Digest aggregation: multiple comments on the same conference are grouped into a single digest entry ("3 new comments on RustConf 2026")
- Comment moderation interaction: notifications sent only for published (status=1) comments

---

## BMAD Stories

### Story 37.1: Comment Item Type with Threaded Display

**Status:** Not started

**As a** registered user,
**I want to** post comments on conferences and reply to other comments,
**So that** I can discuss events with other community members.

**Acceptance criteria:**

- Comment Item Type created: body (TextValue, `filtered_html`), conference (RecordReference), parent (RecordReference to self, nullable)
- Threaded comment display below conference detail pages using recursive CTE query
- Each comment shows: author name, timestamp, body, reply link, edit/delete links (if permitted)
- Comment form with CSRF protection, `filtered_html` body field
- `post comments` permission required to submit comments
- `edit own comments` allows editing/deleting own comments
- `edit any comments` allows editors to edit/delete any comment
- Comments use standard Item access control (the kernel's `check_access` layer)
- Empty state: "No comments yet" when no comments exist

### Story 37.2: Comment Moderation Queue

**Status:** Not started

**As a** content editor,
**I want** a moderation queue for recent comments,
**So that** I can review and remove inappropriate content.

**Acceptance criteria:**

- Moderation view showing recent comments with conference title, author, timestamp, body preview
- Approve and delete actions per comment with CSRF protection
- Filterable by date range and conference
- Accessible to users with `edit any comments` permission
- Delete removes the comment and its replies (cascade or re-parent)
- Flash messages confirm moderation actions

### Story 37.3: User Subscriptions

**Status:** Not started

**As a** registered user,
**I want to** subscribe to conferences I'm interested in,
**So that** I receive notifications when they change.

**Acceptance criteria:**

- `user_subscriptions` join table linking user_id to item_id
- Subscribe/unsubscribe toggle button on conference detail pages (authenticated users only)
- AJAX toggle: subscribe/unsubscribe without full page reload, with visual feedback
- My Subscriptions page at `/user/{uid}/subscriptions` showing subscribed conferences with dates and status
- Subscription privacy: users can only view their own subscriptions (403 for other users)
- Subscription state reflected in the conference detail template (button shows "Subscribed" or "Subscribe")
- Progressive enhancement: toggle works via standard POST if JavaScript is disabled

### Story 37.4: `ritrovo_notify` Plugin -- Notifications & Digests

**Status:** Not started

**As a** subscribed user,
**I want to** receive notifications when conferences I follow change,
**So that** I stay informed about date changes, venue updates, and CFP deadlines.

**Acceptance criteria:**

- WASM plugin `ritrovo_notify` compiled and installable
- `tap_menu`: registers `/user/{uid}/subscriptions` route
- `tap_item_view`: injects Subscribe/Unsubscribe toggle on conference detail pages (AJAX endpoint)
- `tap_item_update`: when a subscribed conference changes (dates, venue, CFP), queues a notification per subscriber
- `tap_queue_info`: declares the `ritrovo_notifications` queue
- `tap_queue_worker`: processes notification events -- sends immediate email or queues for digest based on user preference
- `tap_cron`: daily aggregation of pending notifications into digest emails per user
- Email delivery: console log for tutorial, SMTP transport configurable for production
- User notification preferences: immediate or daily digest (stored in user JSONB data)
- Failed notifications logged; retried on next cron cycle
- Plugin uses SDK host functions: queue operations, user lookup, email dispatch, structured logging

### Story 37.5: Plugin-to-Plugin Communication via Shared Queues

**Status:** Not started

**As a** plugin developer,
**I want** plugins to communicate through shared queues without direct dependencies,
**So that** I can build decoupled plugin ecosystems.

**Acceptance criteria:**

- `ritrovo_cfp` writes `cfp_closing_soon` events to the `ritrovo_notifications` queue
- `ritrovo_notify` processes these events in `tap_queue_worker`, finding subscribers and sending notifications
- Queue message format: JSON with `event_type`, `item_id`, `metadata` fields
- Neither plugin imports or depends on the other -- communication is through queue name convention
- Events processed in insertion order; failed events retried up to N times before dead-lettering
- Full flow testable: set `cfp_end_date` to near-future, trigger cron, verify notification queued
- Dead letter handling: poison messages (malformed JSON, unknown event types) logged and removed after max retries

### Story 37.6: Comment Notifications

**Status:** Not started

**As a** subscriber,
**I want to** receive notifications when someone comments on a conference I follow,
**So that** I can join the discussion.

**Acceptance criteria:**

- `ritrovo_notify` handles `tap_item_insert` for `comment` items: checks parent conference for subscribers
- Self-notification suppressed: comment author does not receive notification for their own comment
- Digest aggregation: multiple comments on the same conference grouped into one entry ("3 new comments on RustConf 2026")
- Notifications sent only for published comments (status=1)
- Notification includes: conference title, comment author, comment snippet, link to conference page
- Works independently of `ritrovo_cfp` -- comment notifications use the same queue but different event types

---

## Payoff

A community platform. The reader understands:

- How a third Item Type (comment) with self-referencing RecordReference creates threaded discussions
- How recursive CTE queries build comment trees from flat database rows
- How per-item access control lets different users edit, delete, or moderate comments based on permissions
- How subscriptions create a user-item relationship that drives notifications
- How the Queue API enables plugin-to-plugin communication without shared code or direct dependencies
- How `tap_cron` and `tap_queue_worker` cooperate to deliver notifications and digests
- How three plugins (`ritrovo_access`, `ritrovo_cfp`, `ritrovo_notify`) work together through taps and queues with zero coupling

The plugin architecture has proven itself. Each plugin is independently deployable, testable, and understandable. The kernel dispatches, the queue connects, and plugins never need to know each other exist.

---

## What's Deferred

These are explicitly **not** in Part 6 (and the tutorial should say so):

- **Internationalization** -- Part 7 (separate concern)
- **REST API** -- Part 7 (API endpoints, authentication, rate limiting)
- **Translation workflow** -- Part 7 (ritrovo_translate plugin)
- **Caching & performance** -- Part 8 (tag-based invalidation, L1/L2 cache)
- **Batch operations** -- Part 8 (bulk publish at scale)
- **S3 storage** -- Part 8 (production file storage)
- **Comment spam prevention** -- Future (CAPTCHA, rate limiting on comments)
- **Real email delivery** -- Future (tutorial uses console logging; production uses SMTP/Postmark)
- **User avatars** -- Future (file field on user profiles)

---

## Related

- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Epic 7: Forms & User Input](epic-07.md) -- Part 5 forms, ritrovo_cfp, ritrovo_access
- [Plugin SDK Design](../design/Design-Plugin-SDK.md)
- [Web Layer Design](../design/Design-Web-Layer.md)
