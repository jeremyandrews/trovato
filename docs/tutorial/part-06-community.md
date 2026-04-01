# Part 6: Community & Plugin Communication

Part 5 opened the front door to user input with forms, rich text editing, and two new plugins. Part 6 turns Ritrovo into a community platform.

Users will post **threaded comments** on conferences, **subscribe** to events they care about, and receive **notifications** when things change. Behind the scenes, three plugins cooperate through shared queues -- demonstrating the most sophisticated pattern in the Trovato plugin ecosystem: plugin-to-plugin communication without direct dependencies.

**Start state:** Form API, two plugin designs (`ritrovo_cfp`, `ritrovo_access`), user profile editing, no discussion features.
**End state:** Threaded comments with moderation, user subscriptions, notification delivery via `ritrovo_notify`, plugin-to-plugin collaboration through shared queues.

> **Implementation note:** The kernel has a comment model (`models/comment.rs`, 293 lines) with a full REST API (`routes/comment.rs`, 745 lines) and a threaded display template (`templates/elements/comments.html`). The `user_subscriptions` table exists with subscribe/unsubscribe/list operations (`models/subscription.rs`). The `ritrovo_notify` plugin (132 lines) provides notification infrastructure with queue workers. The queue system is shared with `ritrovo_importer`. All features described in this part are implemented.

---

## Step 1: Threaded Comments

Comments are a third content type built on the same Item model as conferences and speakers. But unlike those types, comments have a self-referencing RecordReference field that creates threaded hierarchies.

### The Comment Model

Comments are stored as items with type `comment` and three key fields:

| Field | Type | Purpose |
|---|---|---|
| `field_body` | TextValue (`filtered_html`) | Comment text |
| `field_conference` | RecordReference | Parent conference being discussed |
| `field_parent` | RecordReference (self) | Parent comment for threading (null = top-level) |

The `field_parent` self-reference is what makes threading work. A reply to a reply is simply a comment whose `field_parent` points to another comment rather than being null.

### Comment Display: Recursive CTE

Building a threaded comment tree from flat database rows uses a recursive Common Table Expression:

```sql
WITH RECURSIVE comment_tree AS (
    -- Base case: top-level comments on this conference
    SELECT c.id, c.title, c.fields, c.author_id, c.created, c.changed,
           0 AS depth, ARRAY[c.created] AS sort_path
    FROM item c
    WHERE c.type = 'comment'
      AND c.fields->>'field_conference' = $1
      AND c.fields->>'field_parent' IS NULL
      AND c.status = 1

    UNION ALL

    -- Recursive case: replies to comments in the tree
    SELECT c.id, c.title, c.fields, c.author_id, c.created, c.changed,
           ct.depth + 1, ct.sort_path || c.created
    FROM item c
    JOIN comment_tree ct ON c.fields->>'field_parent' = ct.id::text
    WHERE c.type = 'comment' AND c.status = 1
)
SELECT * FROM comment_tree
ORDER BY sort_path;
```

The `sort_path` array ensures comments are ordered chronologically within each thread branch. The `depth` column controls indentation in the template.

### Comment Permissions

The `ritrovo_access` plugin (Part 5) declares comment-related permissions:

| Permission | Who | What |
|---|---|---|
| `post comments` | authenticated + editor + publisher | Submit new comments |
| `edit own comments` | authenticated + editor + publisher | Edit/delete own comments |
| `edit any comments` | editor + publisher | Edit/delete any comment (moderation) |

Anonymous users can read published comments but cannot post.

### Comment Form

Below each conference detail page, authenticated users with `post comments` see a comment form:
- Body field with `filtered_html` format (basic formatting, no arbitrary HTML)
- Hidden field for `field_conference` (the current conference UUID)
- Hidden field for `field_parent` (null for top-level, comment UUID for replies)
- CSRF token
- Reply links on existing comments that pre-fill `field_parent`

### Access Control

Comments use the standard Item access control chain. The `check_access()` model from Part 4 applies:
1. Admin bypass
2. Published view (anyone with `access content` can read published comments)
3. Plugin hooks (`tap_item_access` from `ritrovo_access`)
4. Role-based fallback (checking `edit own comments`, `edit any comments`, etc.)

### Verify

```bash
# Comment model exists
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item WHERE type = 'comment';"
# 0 (no comments yet)

# Comment-related permissions exist
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT DISTINCT permission FROM role_permissions WHERE permission LIKE '%comment%';"
```

<details>
<summary>Under the Hood: Why Comments Are Items</summary>

Comments could have been a separate table with their own model. Making them items has three advantages:

1. **Access control for free.** The entire `check_access()` pipeline -- admin bypass, plugin hooks, role-based fallback -- works on comments without any comment-specific code.

2. **Revisions for free.** Every comment edit creates a revision, giving moderators an audit trail.

3. **Render pipeline for free.** Comments go through the same Build > Alter > Sanitize > Render pipeline, so `filtered_html` sanitization, `tap_item_view` hooks, and template resolution all apply.

The cost is that comments share the `item` table with conferences and speakers. For a tutorial-sized site this is fine. At scale, a denormalized comment table might be more efficient, but the access control and revision benefits are significant.

</details>

---

## Step 2: Comment Moderation

Editors need tools to manage comments across the site. The comment moderation queue provides a filtered view of recent comments with approve/delete actions.

### Moderation Queue

The moderation view (accessible to users with `edit any comments`) shows:

| Column | Content |
|---|---|
| Conference | Title of the parent conference (linked) |
| Author | Username of the commenter |
| Date | Comment creation timestamp |
| Body | Truncated preview of the comment text |
| Actions | Approve, Delete |

The queue is filterable by date range and conference. It shows both published and unpublished comments, with unpublished comments highlighted for review.

### Moderation Actions

| Action | Effect | Security |
|---|---|---|
| Approve | Sets `status = 1` (published) | CSRF-protected POST |
| Delete | Removes the comment (cascades to replies or re-parents them) | CSRF-protected POST with confirmation |

When a comment is deleted, its replies can either be:
- **Cascaded** -- deleted along with the parent (simpler)
- **Re-parented** -- promoted to the parent's level (preserves discussion)

The default behavior is cascade deletion, which is simpler to reason about and prevents orphaned threads.

### Flash Messages

After moderation actions, a flash message confirms the result: "Comment approved." or "Comment and 2 replies deleted."

### Verify

```bash
# Moderation requires edit any comments permission (editor or publisher)
# Admin always has access via is_admin bypass
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/comments
# 200 (admin access)
```

---

## Step 3: User Subscriptions

Authenticated users can subscribe to conferences to receive notifications about changes.

### Subscription Storage

Subscriptions will be stored in a `user_subscriptions` join table:

```sql
CREATE TABLE user_subscriptions (
    user_id UUID NOT NULL REFERENCES users(id),
    item_id UUID NOT NULL REFERENCES item(id),
    created BIGINT NOT NULL,
    PRIMARY KEY (user_id, item_id)
);
```

> **Not yet implemented.** This table does not exist yet. A migration will be needed to create it before the subscription feature is available.

The composite primary key prevents duplicate subscriptions.

### Subscribe/Unsubscribe Toggle

Conference detail pages show a toggle button for authenticated users:

- **Not subscribed:** "Subscribe" button
- **Subscribed:** "Subscribed" button (visually distinct, click to unsubscribe)

The toggle works via AJAX (POST with CSRF token, returns updated button HTML) with a progressive enhancement fallback (full page reload).

### My Subscriptions Page

Each user has a private subscriptions page at `/user/{uid}/subscriptions` showing:
- Conference title (linked to detail page)
- Event dates
- Current stage badge
- Unsubscribe button

Subscription privacy: users can only view their own subscriptions. Attempting to view another user's subscriptions returns 403.

### Verify

```bash
# Subscription table exists
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "\d user_subscriptions"
# Expect: user_id, item_id, created columns with PK

# Subscription count for a user
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM user_subscriptions;"
# 0 (no subscriptions yet)
```

---

## Step 4: The `ritrovo_notify` Plugin

The `ritrovo_notify` plugin is the fourth Ritrovo plugin. It implements subscriptions, notification delivery, and digest emails.

> **Not yet implemented.** The `ritrovo_notify` plugin source does not exist yet. This step describes its design and the SDK features it will use. When written, it will live at `plugins/ritrovo_notify/`.

### What It Does

| Tap | Behavior |
|---|---|
| `tap_menu` | Registers the `/user/{uid}/subscriptions` route |
| `tap_item_view` | Injects Subscribe/Unsubscribe toggle button on conference detail pages |
| `tap_item_insert` | When a comment is posted on a subscribed conference, queues notifications for subscribers |
| `tap_item_update` | When a subscribed conference changes, queues notifications for each subscriber |
| `tap_queue_info` | Declares the `ritrovo_notifications` queue |
| `tap_queue_worker` | Processes notification events: sends email or queues for digest |
| `tap_cron` | Daily digest aggregation -- batches pending notifications per user |

### Notification Flow

```
Conference updated (dates change, venue change, CFP status)
  → tap_item_update fires in ritrovo_notify
  → Plugin queries user_subscriptions for this item
  → For each subscriber, pushes notification to ritrovo_notifications queue
  → tap_queue_worker picks up notification events
  → Sends email (immediate) or stores for digest (daily)
  → tap_cron (daily) aggregates pending digests and sends batch emails
```

### Email Delivery

For the tutorial, email delivery is handled by the kernel's email service. If no SMTP server is configured, emails are logged but not sent. In production, configure SMTP via environment variables:

```
SMTP_HOST=smtp.example.com
SMTP_PORT=587
SMTP_USERNAME=notifications@example.com
SMTP_PASSWORD=...
SMTP_ENCRYPTION=starttls
SMTP_FROM_EMAIL=notifications@ritrovo.example.com
```

The email service (`services/email.rs`) supports both plain text and HTML email bodies.

### Notification Preferences

Users can set their notification preference in their profile:
- **Immediate** -- each notification triggers an email
- **Daily digest** -- notifications are batched and sent once per day

The preference is stored in the user's JSONB `data` field:

```json
{
  "notification_preference": "digest"
}
```

### Building the Plugin

```bash
cd plugins/ritrovo_notify
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/ritrovo_notify.wasm ../../plugin-dist/
cargo run --release --bin trovato -- plugin install plugin-dist/ritrovo_notify.wasm
```

### SDK Features Used

- `tap_menu` -- Route registration
- `tap_item_view` -- Subscribe button injection
- `tap_item_update` -- Change detection and notification queueing
- `tap_queue_info` / `tap_queue_worker` -- Queue declaration and processing
- `tap_cron` -- Scheduled digest aggregation
- `queue_push()` host function -- Writing notification events
- User lookup host functions -- Finding subscribers
- Email dispatch host function -- Sending notifications

### Verify

```bash
# Plugin installed
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugins WHERE name = 'ritrovo_notify';"
# Expect: ritrovo_notify, 1

# Queue exists
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM plugin_queue WHERE queue_name = 'ritrovo_notifications';"
```

<details>
<summary>Under the Hood: The Queue System</summary>

Trovato's queue system is backed by both PostgreSQL and Redis:

- **`plugin_queue` table** -- Persistent storage for queue items with `queue_name`, `data` (JSONB), `status`, `attempts`, `created`, and `locked_until` columns.
- **Redis** -- Used for queue signaling (notify workers that items are available).

The `tap_queue_worker` dispatch cycle:

```
Cron runs → dispatch tap_queue_worker for each queue
  → Lock N items (SET locked_until = NOW() + timeout)
  → Pass items to the plugin worker
  → On success: delete the queue item
  → On failure: increment attempts, unlock
  → After max attempts: move to dead letter (status = 'dead')
```

Queue items are processed serially within a queue to prevent ordering issues. Failed items are retried up to N times (configurable per queue), then dead-lettered for manual inspection.

The `queue_push()` WASM host function lets plugins write to any queue by name. The producing plugin does not need to know which plugin consumes from that queue -- the queue name is the only coupling point.

</details>

---

## Step 5: Plugin-to-Plugin Communication

Three plugins now form a collaboration chain. This step demonstrates the full event flow from CFP deadline detection to user notification delivery.

### The Event Flow

```
ritrovo_cfp (Part 6)                    ritrovo_notify (Part 6)
┌──────────────────┐                    ┌──────────────────┐
│ tap_cron         │                    │ tap_queue_worker  │
│  detects CFP     │── queue_push() ──→ │  reads event      │
│  entering 7-day  │   "ritrovo_       │  finds subscribers│
│  window          │   notifications"  │  sends email/     │
└──────────────────┘                    │  queues digest    │
                                        └──────────────────┘
```

### No Direct Dependency

The key architectural insight: `ritrovo_cfp` writes to the `ritrovo_notifications` queue by name. `ritrovo_notify` reads from it by name. Neither plugin imports, references, or even knows about the other.

This means:
- You can install `ritrovo_cfp` without `ritrovo_notify` -- events accumulate in the queue but nobody processes them
- You can install `ritrovo_notify` without `ritrovo_cfp` -- the notification worker processes whatever events arrive
- You can replace either plugin with a different implementation that uses the same queue name

[<img src="images/part-06/conference-with-cfp.png" width="600" alt="A conference detail page showing the CFP closing soon badge with deadline date">](images/part-06/conference-with-cfp.png)

### Queue Message Format

Events on the `ritrovo_notifications` queue follow a common JSON format:

```json
{
  "event_type": "cfp_closing_soon",
  "item_id": "conference-uuid",
  "metadata": {
    "conference_name": "RustConf 2026",
    "cfp_end_date": "2026-04-15",
    "days_remaining": 6
  }
}
```

The `event_type` field tells the consumer how to handle the event. Different event types produce different notification templates.

### Error Handling

- **Failed notifications** -- logged and retried on the next queue processing cycle
- **Poison messages** -- malformed JSON or unknown event types are dead-lettered after max retries
- **Dead letter inspection** -- dead-lettered items remain in `plugin_queue` with `status = 'dead'` for manual review

### Testing the Flow

```bash
# Set a conference's CFP end date to 5 days from now
FUTURE=$(date -v+5d +%Y-%m-%d)
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "UPDATE item SET fields = jsonb_set(fields, '{field_cfp_end_date}', '\"$FUTURE\"')
      WHERE id = '$ID';"

# Subscribe to that conference (via the UI or directly)
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "INSERT INTO user_subscriptions (user_id, item_id, created)
      SELECT u.id, '$ID'::uuid, EXTRACT(EPOCH FROM NOW())::bigint
      FROM users u WHERE u.name = 'editor_alice'
      ON CONFLICT DO NOTHING;"

# Trigger cron (which dispatches tap_cron to ritrovo_cfp)
cargo run --release --bin trovato -- cron

# Check the notification queue
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT queue_name, data->>'event_type', status FROM plugin_queue ORDER BY created DESC LIMIT 5;"
# Expect: ritrovo_notifications, cfp_closing_soon, pending
```

### Verify

```bash
# Verify plugin collaboration
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugins WHERE name IN ('ritrovo_cfp', 'ritrovo_access', 'ritrovo_notify') ORDER BY name;"
# Expect: three rows, all status 1
```

[<img src="images/part-06/plugins-admin.png" width="600" alt="The plugin management admin page showing installed plugins with status and actions">](images/part-06/plugins-admin.png)

---

## Step 6: Comment Notifications

The notification system extends to comments: when someone comments on a conference you are subscribed to, you receive a notification.

### How It Works

The `ritrovo_notify` plugin's `tap_item_insert` handler checks:
1. Is the new item a `comment`?
2. What conference does it belong to? (read `field_conference`)
3. Who is subscribed to that conference?
4. Queue a notification for each subscriber

### Self-Notification Suppression

If you post a comment on a conference you are subscribed to, you do not receive a notification about your own comment. The plugin compares the comment author's user ID against the subscriber list and skips matches.

### Digest Aggregation

Multiple comments on the same conference are grouped in the daily digest:
- "3 new comments on RustConf 2026" (with a link to the conference page)
- Not "Comment by Alice", "Comment by Bob", "Comment by Carol" separately

### Published Comments Only

Notifications are sent only for published comments (`status = 1`). If comment moderation requires approval, the notification fires when the moderator approves, not when the comment is first submitted.

### Verify

```bash
# Post a comment (via UI) on a conference that editor_alice is subscribed to
# Check that a notification was queued
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT data->>'event_type' FROM plugin_queue WHERE queue_name = 'ritrovo_notifications' ORDER BY created DESC LIMIT 3;"
```

---

## What You've Built

By the end of Part 6, you have:

- **Threaded comments** on conferences using recursive CTE queries, with `filtered_html` body text and per-comment access control.
- **Comment moderation** with approve/delete actions, filterable queue, and cascade deletion.
- **User subscriptions** with AJAX toggle, private "My Subscriptions" page, and privacy enforcement.
- **`ritrovo_notify` plugin** implementing subscription management, notification queuing, email delivery (console for tutorial, SMTP for production), and daily digest aggregation.
- **Plugin-to-plugin communication** through shared queues: `ritrovo_cfp` produces events, `ritrovo_notify` consumes them, with zero coupling.
- **Comment notifications** with self-notification suppression, digest aggregation, and moderation-aware delivery.

You also now understand:

- How a recursive CTE builds threaded comment trees from flat database rows.
- How per-item access control lets different users read, post, edit, and delete comments based on their permissions.
- How the queue system enables decoupled plugin collaboration without shared code or imports.
- How `tap_cron` and `tap_queue_worker` cooperate for scheduled processing and event delivery.
- How three plugins (`ritrovo_access`, `ritrovo_cfp`, `ritrovo_notify`) work together through taps and queues with zero coupling.

The plugin architecture has proven itself. Each plugin is independently deployable, testable, and understandable. The kernel dispatches, the queue connects, and plugins never need to know each other exist.

[<img src="images/part-06/front-page-community.png" width="600" alt="The Ritrovo front page with community features including comments, subscriptions, and CFP badges">](images/part-06/front-page-community.png)

---

## What's Deferred

| Feature | Deferred To | Reason |
|---|---|---|
| Internationalization | Part 7 | Separate concern |
| REST API | Part 7 | API endpoints, authentication, rate limiting |
| Translation workflow | Part 7 | `ritrovo_translate` plugin |
| Caching & performance | Part 8 | Tag-based invalidation, L1/L2 cache |
| Batch operations | Part 8 | Bulk publish at scale |
| S3 storage | Part 8 | Production file storage |
| Comment spam prevention | Future | CAPTCHA, rate limiting on comments |
| Real email delivery | Future | Tutorial uses console logging; production uses SMTP |
| User avatars | Future | File field on user profiles |

---

## Related

- [Part 5: Forms & User Input](part-05-forms-and-input.md)
- [Part 7: Going Global](part-07-going-global.md)
- [Plugin SDK Design](../design/Design-Plugin-SDK.md)
- [Web Layer Design](../design/Design-Web-Layer.md)
- [Epic 8: Community & Plugin Communication](../ritrovo/epic-08.md)
