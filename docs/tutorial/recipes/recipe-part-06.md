# Recipe: Part 6 — Community & Plugin Communication

> **Synced with:** `docs/tutorial/part-06-community.md`
> **Sync hash:** 2947e7e2
> **Last verified:** 2026-03-15
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1–5 must be completed (forms, block editor, ritrovo_access plugins).
- Check `TOOLS.md` for server start commands, database connection, admin credentials, plugin build commands.
- Database backup recommended:

```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/pre-part-06-$(date +%Y%m%d).dump
```

---

## Step 1: Threaded Comments

### 1.1 Understand Comment Model

`[REFERENCE]` Key concepts:
- Comments are items with type `comment` (same table, same access control, same revisions)
- Three fields: `field_body` (TextValue, `filtered_html`), `field_conference` (RecordReference to parent), `field_parent` (RecordReference to self, nullable for top-level)
- Threading via recursive CTE query on `field_parent` self-reference
- `sort_path` array in CTE ensures chronological ordering within thread branches
- `depth` column controls indentation in template

### 1.2 Verify Comment Type Exists

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item WHERE type = 'comment';"
# 0 (no comments yet — will be created by users)
```

### 1.3 Verify Comment Permissions

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT DISTINCT permission FROM role_permissions WHERE permission LIKE '%comment%' ORDER BY permission;"
```

**Verify:** `edit any comments`, `edit own comments`, `post comments` present (declared by `ritrovo_access` plugin from Part 5).

### 1.4 Test Comment Posting

`[UI-ONLY]` Navigate to a conference detail page as editor_alice:
1. Scroll to the comment section below the conference content
2. Enter a comment in the body field
3. Submit with CSRF token
4. Verify the comment appears threaded under the conference

### 1.5 Test Comment Reply

`[UI-ONLY]` On a conference with an existing comment:
1. Click "Reply" on the comment
2. Enter reply text
3. Submit
4. Verify the reply appears indented under the parent comment

---

## Step 2: Comment Moderation

### 2.1 Understand Moderation Queue

`[REFERENCE]` Key concepts:
- Moderation queue at `/admin/comments` shows recent comments with conference, author, date, body preview
- Actions: Approve (set status=1), Delete (cascade removes replies)
- Requires `edit any comments` permission
- Filterable by date range and conference
- CSRF-protected POST for all actions

### 2.2 Verify Moderation Access

`[CLI]`

```bash
# Admin can access moderation queue
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/comments
# Expect: 200

# editor_alice with edit any comments permission
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/comments
# Expect: depends on whether moderation UI checks is_admin or permission
```

### 2.3 Test Moderation Actions

`[UI-ONLY]` As admin:
1. Navigate to `/admin/comments`
2. Find a comment and click "Approve" or "Delete"
3. Verify flash message confirms the action
4. Verify the comment's status changed (or it was removed)

---

## Step 3: User Subscriptions

### 3.1 Verify Subscription Table

> **Not yet implemented.** The `user_subscriptions` table does not exist yet. Steps 3.1–3.5 will fail until the migration is created.

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "\d user_subscriptions"
```

**Verify:** Table exists with user_id (uuid), item_id (uuid), created (bigint), composite PK.

### 3.2 Test Subscribe via SQL

`[CLI]` Create a test subscription:

```bash
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")

$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "INSERT INTO user_subscriptions (user_id, item_id, created)
      SELECT u.id, '$ID'::uuid, EXTRACT(EPOCH FROM NOW())::bigint
      FROM users u WHERE u.name = 'editor_alice'
      ON CONFLICT DO NOTHING;"
```

### 3.3 Verify Subscription

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT u.name, i.title FROM user_subscriptions us
      JOIN users u ON us.user_id = u.id
      JOIN item i ON us.item_id = i.id;"
```

**Verify:** editor_alice subscribed to the test conference.

### 3.4 Test Subscriptions Page

`[CLI]`

```bash
# Get editor_alice's user ID
ALICE_UID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM users WHERE name = 'editor_alice';")

# My Subscriptions page
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/user/$ALICE_UID/subscriptions"
# Expect: 200
```

### 3.5 Test Subscription Privacy

`[CLI]`

```bash
# viewer_carol should not see editor_alice's subscriptions
curl -s -b /tmp/trovato-carol.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/user/$ALICE_UID/subscriptions"
# Expect: 403
```

Record subscription testing commands in `TOOLS.md -> Subscriptions`.

---

## Step 4: The `ritrovo_notify` Plugin

### 4.1 Review Plugin Design

`[REFERENCE]` Key taps and behaviors:
- `tap_menu`: registers `/user/{uid}/subscriptions`
- `tap_item_view`: injects Subscribe/Unsubscribe toggle on conference pages
- `tap_item_update`: queues notifications for subscribers when conference changes
- `tap_queue_info`: declares `ritrovo_notifications` queue
- `tap_queue_worker`: processes notification events (email or digest)
- `tap_cron`: daily digest aggregation
- Email: logged but not sent when no SMTP configured; configure `SMTP_HOST`, `SMTP_PORT`, etc. for production

### 4.2 Build the Plugin

> **Not yet implemented.** The `ritrovo_notify` plugin source does not exist yet. Skip steps 4.2–4.6 until it is written.

`[CLI]`

```bash
cd plugins/ritrovo_notify
cargo build --target wasm32-wasip1 --release
cp target/wasm32-wasip1/release/ritrovo_notify.wasm ../../plugin-dist/
```

**Verify:** `plugin-dist/ritrovo_notify.wasm` exists.

### 4.3 Install the Plugin

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin install ritrovo_notify
```

### 4.4 Verify Plugin Installation

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugins WHERE name = 'ritrovo_notify';"
```

**Verify:** ritrovo_notify, status 1 (enabled).

### 4.5 Test Subscribe Toggle

`[UI-ONLY]` Navigate to a conference detail page as editor_alice:
1. Verify the Subscribe/Unsubscribe button appears
2. Click to toggle subscription state
3. Verify the button label changes

### 4.6 Test Notification Queue

`[CLI]`

```bash
# Check notification queue
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT queue_name, data->>'event_type', status FROM plugin_queue WHERE queue_name = 'ritrovo_notifications' ORDER BY created DESC LIMIT 5;"
```

Record plugin commands in `TOOLS.md -> Plugins`.

---

## Step 5: Plugin-to-Plugin Communication

### 5.1 Review Architecture

`[REFERENCE]` Key concepts:
- `ritrovo_cfp` writes `cfp_closing_soon` events to `ritrovo_notifications` queue
- `ritrovo_notify` reads from `ritrovo_notifications` queue, processes events
- Neither plugin imports or depends on the other — queue name is the only coupling
- Can install either plugin independently
- Queue message format: `{ "event_type": "...", "item_id": "...", "metadata": {...} }`

### 5.2 Test Full Event Flow

`[CLI]` Set a conference's CFP end date to near-future, trigger cron, verify notification queued:

```bash
# Set CFP end date to 5 days from now
FUTURE=$(date -v+5d +%Y-%m-%d)
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")

$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "UPDATE item SET fields = jsonb_set(fields, '{field_cfp_end_date}', '\"$FUTURE\"')
      WHERE id = '$ID';"

# Ensure editor_alice is subscribed
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "INSERT INTO user_subscriptions (user_id, item_id, created)
      SELECT u.id, '$ID'::uuid, EXTRACT(EPOCH FROM NOW())::bigint
      FROM users u WHERE u.name = 'editor_alice'
      ON CONFLICT DO NOTHING;"

# Trigger cron
cargo run --release --bin trovato -- cron

# Check notification queue
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT queue_name, data->>'event_type', status FROM plugin_queue WHERE queue_name = 'ritrovo_notifications' ORDER BY created DESC LIMIT 5;"
```

**Verify:** `cfp_closing_soon` event in the `ritrovo_notifications` queue.

### 5.3 Verify All Three Plugins

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugins WHERE name IN ('ritrovo_cfp', 'ritrovo_access', 'ritrovo_notify') ORDER BY name;"
```

**Verify:** Three rows, all status 1 (enabled).

---

## Step 6: Comment Notifications

### 6.1 Understand Comment Notification Flow

`[REFERENCE]` Key concepts:
- `ritrovo_notify.tap_item_insert` checks if new item is type `comment`
- Reads `field_conference` to find parent conference
- Queries `user_subscriptions` for subscribers
- Skips self-notification (comment author is excluded)
- Notifications sent only for published comments (status=1)
- Digest aggregation: multiple comments → "3 new comments on RustConf 2026"

### 6.2 Test Comment Notification

`[UI-ONLY]` As publisher_bob:
1. Navigate to a conference that editor_alice is subscribed to
2. Post a comment
3. Check the notification queue for a new event

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT data->>'event_type' FROM plugin_queue WHERE queue_name = 'ritrovo_notifications' AND data->>'event_type' = 'new_comment' ORDER BY created DESC LIMIT 3;"
```

---

## Completion Checklist

```bash
echo "=== Part 6 Completion Checklist ==="
echo -n "1. Comment type available: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT CASE WHEN EXISTS (SELECT 1 FROM content_type WHERE machine_name = 'comment') THEN 'yes' ELSE 'no' END;" | tr -d ' '
echo -n "2. Comment permissions: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(DISTINCT permission) FROM role_permissions WHERE permission LIKE '%comment%';" | tr -d ' '
echo -n "3. Subscription table: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT CASE WHEN EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'user_subscriptions') THEN 'yes' ELSE 'no' END;" | tr -d ' '
echo -n "4. ritrovo_notify: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COALESCE((SELECT status::text FROM plugins WHERE name = 'ritrovo_notify'), 'not installed');" | tr -d ' '
echo -n "5. All plugins enabled: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM plugins WHERE name IN ('ritrovo_cfp', 'ritrovo_access', 'ritrovo_notify') AND status = 1;" | tr -d ' '
echo ""
```

Expected output:
```
1. Comment type available: yes
2. Comment permissions: >= 3
3. Subscription table: yes (or "no" if migration not yet created)
4. ritrovo_notify: 1 (or "not installed" if plugin not yet written)
5. All plugins enabled: 3 (or fewer if plugins not yet written)
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-06-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
