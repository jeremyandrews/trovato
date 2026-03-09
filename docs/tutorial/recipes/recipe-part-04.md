# Recipe: Part 4 — The Editorial Engine

> **Synced with:** `docs/tutorial/part-04-editorial-engine.md`
> **Sync hash:** cb5864b9
> **Last verified:** 2026-03-09
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1, 2, and 3 must be completed (conference/speaker types, templates, tiles, menus, search).
- Check `TOOLS.md` for server start commands, database connection, admin credentials, config import, plugin build commands.
- Database backup recommended:

```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/pre-part-04-$(date +%Y%m%d).dump
```

---

## Step 1: Users & Authentication

### 1.1 Understand User Architecture

`[REFERENCE]` No action needed. Key concepts:
- Users stored in `users` table with UUID, username, email, Argon2id password hash
- Sessions stored in Redis with HttpOnly cookie
- Session ID cycled after auth state changes (fixation protection)
- Minimum password: 12 characters
- Logout is POST (never GET) with CSRF protection

### 1.2 Import Registration Config

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Config imported (includes `variable.user_registration.yml`).

### 1.3 Register Test Users

`[CLI]` Register three users for the editorial workflow demo:

```bash
# editor_alice
rm -f /tmp/trovato-register.txt
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=editor_alice&email=alice@example.com&password=tutorial-editor1&password_confirm=tutorial-editor1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# publisher_bob
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=publisher_bob&email=bob@example.com&password=tutorial-publish1&password_confirm=tutorial-publish1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# viewer_carol
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=viewer_carol&email=carol@example.com&password=tutorial-viewer1&password_confirm=tutorial-viewer1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

### 1.4 Verify Users Created

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT username, email FROM users WHERE username IN ('editor_alice', 'publisher_bob', 'viewer_carol');"
```

**Verify:** Three rows returned.

### 1.5 Test Login Flow

`[CLI]`

```bash
rm -f /tmp/trovato-alice.txt
LOGIN_PAGE=$(curl -s -c /tmp/trovato-alice.txt http://localhost:3000/user/login)
CSRF=$(echo "$LOGIN_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-alice.txt -c /tmp/trovato-alice.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=editor_alice&password=tutorial-editor1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

**Verify:** Login succeeds (303 redirect). Session cookie set in cookie jar.

### 1.6 Test User Profile

`[CLI]`

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/user/editor_alice
# Expect: 200
```

Record user creation commands and credentials in `TOOLS.md -> Roles & Access`.

---

## Step 2: Roles & Permissions

### 2.1 Review Role Definitions

`[REFERENCE]` Review the role YAML configs:

```bash
cat docs/tutorial/config/role.editor.yml
cat docs/tutorial/config/role.publisher.yml
```

Key permissions:
- **editor**: access content, create/edit conferences and speakers, access files, use filtered_html
- **publisher**: editor permissions + delete any, administer files, use full_html

### 2.2 Note: Role Config Not Importable

`[REFERENCE]` Role and permission assignment must be done through the admin UI. The YAML files serve as reference documentation for what to configure. ConfigStorage does not yet support the `role` entity type.

### 2.3 Assign Roles to Test Users

`[UI-ONLY]` Log in as admin and navigate to `/admin/people`:

1. Edit `editor_alice` → assign **Editor** role → Save
2. Edit `publisher_bob` → assign **Publisher** role → Save
3. Leave `viewer_carol` with no extra roles

### 2.4 Verify Role Assignment

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT username, data->'roles' AS roles FROM users WHERE username IN ('editor_alice', 'publisher_bob', 'viewer_carol');"
```

**Verify:** editor_alice has editor role, publisher_bob has publisher role, viewer_carol has no extra roles.

### 2.5 Test Access Control

`[CLI]` Test that admin pages require authentication:

```bash
# Anonymous cannot access admin
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# Expect: 302 or 303 (redirect to login)

# Authenticated admin can access
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# Expect: 200
```

Record role testing commands in `TOOLS.md -> Roles & Access`.

---

## Step 3: Stages & the Editorial Workflow

### 3.1 Review Stage Definitions

`[REFERENCE]` Review the stage YAML configs:

```bash
cat docs/tutorial/config/stage.incoming.yml
cat docs/tutorial/config/stage.curated.yml
cat docs/tutorial/config/stage.live.yml
cat docs/tutorial/config/stage.legal_review.yml
```

### 3.2 Note: Stage Config Not Importable

`[REFERENCE]` Stage configuration must be done through the admin UI or direct database setup. The YAML files serve as reference documentation. Stages are tags in the `stages` category.

### 3.3 Verify Stages Exist

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, label FROM stage ORDER BY weight;"
```

**Verify:** At minimum, the Live stage exists (well-known UUID `0193a5a0-0000-7000-8000-000000000001`).

### 3.4 Review Workflow Configuration

`[REFERENCE]`

```bash
cat docs/tutorial/config/variable.workflow_editorial.yml
```

The workflow defines four transitions:
- `incoming → curated` (requires: use editorial workflow)
- `curated → live` (requires: publish content)
- `live → curated` (requires: unpublish content)
- `curated → incoming` (requires: use editorial workflow)

### 3.5 Verify Stage-Aware Gathers

`[CLI]` Gathers have `stage_aware: true`, so anonymous users see only Live content:

```bash
# Anonymous query
curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq '.total'
# Returns count of Live conferences
```

### 3.6 Verify Stage-Aware Search

`[CLI]`

```bash
# Anonymous search — only Live results
curl -s 'http://localhost:3000/api/search?q=conference' | jq '.total'
```

### 3.7 Extensibility Demo: Legal Review Stage

`[REFERENCE]` The `stage.legal_review.yml` config demonstrates adding a new stage without code changes. The workflow config includes an additional transition path: `curated → legal_review → live`.

Record stage and workflow details in `TOOLS.md -> Stages & Workflows`.

---

## Step 4: Revision History

### 4.1 Verify Revisions Exist

`[CLI]` Every item has at least one revision (the initial creation):

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT i.title, COUNT(r.id) AS revisions FROM item i JOIN item_revision r ON i.id = r.item_id WHERE i.type = 'conference' GROUP BY i.title ORDER BY revisions DESC LIMIT 5;"
```

**Verify:** Each conference has at least 1 revision.

### 4.2 View Revision History Page

`[CLI]` The revision history page requires authentication:

```bash
ID=$(curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq -r '.items[0].id')

# Unauthenticated — should redirect to login
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/revisions
# Expect: 302 or 303

# Authenticated
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/revisions
# Expect: 200
```

### 4.3 Verify Revision History Content

`[CLI]`

```bash
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/item/$ID/revisions | grep -c 'class="revisions-table'
# Expect: 1 (revisions table rendered)
```

### 4.4 Test Revert Functionality

`[UI-ONLY]` Navigate to a conference's revision history. If there are multiple revisions, click **Revert** on an older one. Verify:
- A new revision is created (not a delete)
- The item's content matches the reverted-to version
- The revision history shows the new "revert" entry

### 4.5 Scenario: Basic Revision

`[CLI]` Edit a conference to create a new revision, then verify:

```bash
# Count revisions before
BEFORE=$($(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item_revision WHERE item_id = '$ID';" | tr -d ' ')

# Edit via admin UI or API... then:

AFTER=$($(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item_revision WHERE item_id = '$ID';" | tr -d ' ')

echo "Revisions: $BEFORE -> $AFTER"
```

**Verify:** AFTER > BEFORE (new revision created).

Record revision inspection commands in `TOOLS.md -> Revisions`.

---

## Step 5: Admin Content Management

### 5.1 Verify Content List

`[CLI]`

```bash
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# Expect: 200
```

### 5.2 Verify Type Filter

`[CLI]`

```bash
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/admin/content?type=conference"
# Expect: 200

curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/admin/content?type=speaker"
# Expect: 200
```

### 5.3 Verify Bulk Action Form

`[CLI]`

```bash
# Check for bulk action dropdown
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/admin/content | grep -c 'name="action"'
# Expect: 1

# Check for item checkboxes
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/admin/content | grep -c 'name="ids\[\]"'
# Expect: > 0
```

### 5.4 Verify Revisions Link in Operations

`[CLI]`

```bash
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/admin/content | grep -c 'Revisions'
# Expect: > 0 (one per item row)
```

### 5.5 Test Bulk Operation (Optional)

`[UI-ONLY]` On the content list:
1. Select 2-3 items via checkboxes
2. Choose "Unpublish" from the action dropdown
3. Click **Apply**
4. Verify flash message: "X item(s) unpublished."
5. Re-select and "Publish" to restore

### 5.6 Verify Flash Messages

`[CLI]` After a bulk operation, the content list page shows a flash message:

```bash
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/admin/content | grep -o 'class="messages[^"]*"'
```

**Verify:** Flash message container present after operations.

---

## Completion Checklist

```bash
echo "=== Part 4 Completion Checklist ==="
echo -n "1. Users created: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM users WHERE username IN ('editor_alice', 'publisher_bob', 'viewer_carol');" | tr -d ' '
echo -n "2. Login works: "
rm -f /tmp/trovato-test.txt
LP=$(curl -s -c /tmp/trovato-test.txt http://localhost:3000/user/login)
TC=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-test.txt -c /tmp/trovato-test.txt -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$TC" -o /dev/null -w "%{http_code}"
echo ""
echo -n "3. Profile page: "; curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/user/admin
echo -n "4. Stages exist: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM stage;" | tr -d ' '
echo -n "5. Revisions page: "; ID=$(curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq -r '.items[0].id'); curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/revisions
echo -n "6. Admin content: "; curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
echo -n "7. Bulk actions: "; curl -s -b /tmp/trovato-test.txt http://localhost:3000/admin/content | grep -c 'name="action"'
echo ""
```

Expected output:
```
1. Users created: 3
2. Login works: 303
3. Profile page: 200
4. Stages exist: > 0
5. Revisions page: 200
6. Admin content: 200
7. Bulk actions: 1
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-04-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
