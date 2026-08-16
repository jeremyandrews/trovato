# Recipe: Part 4 — The Editorial Engine

> **Synced with:** `docs/tutorial/part-04-editorial-engine.md`
> **Sync hash:** b36dd8b7
> **Last verified:** 2026-03-13
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
- Users stored in `users` table with UUID, name (`name` column), email (`mail` column), Argon2id password hash (`pass` column)
- Sessions stored in Redis with HttpOnly cookie
- Session ID cycled after auth state changes (fixation protection)
- Minimum password: 12 characters
- Logout is POST (never GET) with CSRF protection

### 1.2 Import Registration Config

`[CLI]`

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Config imported (includes `variable.allow_user_registration.yml`).

### 1.3 Register Test Users

`[CLI]` Register three users for the editorial workflow demo. The registration endpoint returns 200 with a success message (not a redirect), and creates users in **inactive** status pending email verification. Since there is no mail server in the tutorial, we activate them via SQL after registration.

**Important:** The `register` rate limit allows only 3 registrations per hour per IP. If you hit 429, clear the rate limit key: `docker exec trovato-redis-1 redis-cli DEL 'rate:register:unknown'` (on localhost without a reverse proxy the key uses `unknown`; behind a proxy, replace `unknown` with the client IP)

```bash
# editor_alice
rm -f /tmp/trovato-register.txt
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=editor_alice&mail=alice@example.com&password=tutorial-editor1&confirm_password=tutorial-editor1&_token=$CSRF" \
  | grep -o 'Registration successful'
# Expect: Registration successful

# Clear rate limit between registrations
docker exec trovato-redis-1 redis-cli DEL 'rate:register:unknown' > /dev/null

# publisher_bob
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=publisher_bob&mail=bob@example.com&password=tutorial-publish1&confirm_password=tutorial-publish1&_token=$CSRF" \
  | grep -o 'Registration successful'
# Expect: Registration successful

docker exec trovato-redis-1 redis-cli DEL 'rate:register:unknown' > /dev/null

# viewer_carol
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=viewer_carol&mail=carol@example.com&password=tutorial-viewer1&confirm_password=tutorial-viewer1&_token=$CSRF" \
  | grep -o 'Registration successful'
# Expect: Registration successful
```

**Activate users** (no mail server = no email verification, so activate via SQL):

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "UPDATE users SET status = 1 WHERE name IN ('editor_alice', 'publisher_bob', 'viewer_carol');"
# Expect: UPDATE 3
```

### 1.4 Verify Users Created

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, mail FROM users WHERE name IN ('editor_alice', 'publisher_bob', 'viewer_carol');"
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

`[CLI]` The profile page at `/user/profile` requires authentication — there is no `/user/{username}` public route:

```bash
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
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
- **viewer**: access content, view incoming conferences, view curated conferences
- **editor**: viewer permissions + create/edit conferences and speakers, access files, use filtered_html, edit conferences
- **publisher**: editor permissions + delete any, administer files, use full_html, publish conferences

> **Note:** The `view incoming conferences`, `view curated conferences`, `edit conferences`, and `publish conferences` permissions are declared by the `ritrovo_access` plugin (Part 5). They are included in the role YAML files to document the final intended state. At this point in the tutorial, only the base permissions are assigned; the ritrovo_access permissions are added in Part 5 after the plugin is installed.

### 2.2 Note: Role Config Not Importable

`[REFERENCE]` ConfigStorage does not yet support the `role` entity type. The YAML files serve as reference documentation. Roles can be created via `/admin/people/roles/add` and permissions assigned via `/admin/people/permissions`, but there is no admin UI for assigning roles to users — use SQL for that.

> Also review `docs/tutorial/config/role.viewer.yml` — the viewer role for viewer_carol.

### 2.3 Log In as Admin

`[CLI]` Role creation requires an admin session. Log in and store the cookie jar at `/tmp/trovato-cookies.txt`:

```bash
rm -f /tmp/trovato-cookies.txt
LOGIN_PAGE=$(curl -s -c /tmp/trovato-cookies.txt http://localhost:3000/user/login)
CSRF=$(echo "$LOGIN_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

### 2.4 Create Roles

`[CLI]` Create the viewer, editor, and publisher roles via the admin API:

```bash
# Create viewer role
FORM_PAGE=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt http://localhost:3000/admin/people/roles/add)
CSRF=$(echo "$FORM_PAGE" | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
FBID=$(echo "$FORM_PAGE" | grep -oE 'name="_form_build_id" value="[^"]+"' | sed 's/.*value="//' | sed 's/"//')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/people/roles/add \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "_form_build_id=$FBID" \
  --data-urlencode "name=viewer" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# Create editor role
FORM_PAGE=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt http://localhost:3000/admin/people/roles/add)
CSRF=$(echo "$FORM_PAGE" | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
FBID=$(echo "$FORM_PAGE" | grep -oE 'name="_form_build_id" value="[^"]+"' | sed 's/.*value="//' | sed 's/"//')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/people/roles/add \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "_form_build_id=$FBID" \
  --data-urlencode "name=editor" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# Create publisher role
FORM_PAGE=$(curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt http://localhost:3000/admin/people/roles/add)
CSRF=$(echo "$FORM_PAGE" | grep -oE 'csrf-token" content="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
FBID=$(echo "$FORM_PAGE" | grep -oE 'name="_form_build_id" value="[^"]+"' | sed 's/.*value="//' | sed 's/"//')
curl -s -b /tmp/trovato-cookies.txt -c /tmp/trovato-cookies.txt \
  -X POST http://localhost:3000/admin/people/roles/add \
  --data-urlencode "_token=$CSRF" \
  --data-urlencode "_form_build_id=$FBID" \
  --data-urlencode "name=publisher" \
  -o /dev/null -w "%{http_code}"
# Expect: 303
```

### 2.5 Assign Permissions and Roles via SQL

`[CLI]` Assign permissions from the YAML reference configs, and assign roles to users. There is no admin UI for user-role assignment, so SQL is required:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato <<'SQL'
-- Viewer permissions (from role.viewer.yml — base permissions only;
-- ritrovo_access permissions added in Part 5 after plugin install)
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r, (VALUES
  ('access content')
) AS p(perm)
WHERE r.name = 'viewer'
ON CONFLICT (role_id, permission) DO NOTHING;

-- Editor permissions (from role.editor.yml — base permissions only;
-- ritrovo_access permissions added in Part 5 after plugin install)
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r, (VALUES
  ('access content'), ('create content'), ('edit own content'),
  ('edit any content'), ('access files'), ('use filtered_html')
) AS p(perm)
WHERE r.name = 'editor'
ON CONFLICT (role_id, permission) DO NOTHING;

-- Publisher permissions (from role.publisher.yml — base permissions only;
-- ritrovo_access permissions added in Part 5 after plugin install)
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r, (VALUES
  ('access content'), ('create content'), ('edit own content'),
  ('edit any content'), ('delete any content'),
  ('access files'), ('administer files'),
  ('use filtered_html'), ('use full_html')
) AS p(perm)
WHERE r.name = 'publisher'
ON CONFLICT (role_id, permission) DO NOTHING;

-- Assign viewer role to viewer_carol
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r
WHERE u.name = 'viewer_carol' AND r.name = 'viewer'
ON CONFLICT DO NOTHING;

-- Assign editor role to editor_alice
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r
WHERE u.name = 'editor_alice' AND r.name = 'editor'
ON CONFLICT DO NOTHING;

-- Assign publisher role to publisher_bob
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r
WHERE u.name = 'publisher_bob' AND r.name = 'publisher'
ON CONFLICT DO NOTHING;
SQL
```

### 2.6 Verify Role Assignment

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT u.name, r.name AS role FROM users u LEFT JOIN user_roles ur ON u.id = ur.user_id LEFT JOIN roles r ON ur.role_id = r.id WHERE u.name IN ('editor_alice', 'publisher_bob', 'viewer_carol') ORDER BY u.name;"
```

**Verify:** editor_alice has editor role, publisher_bob has publisher role, viewer_carol has viewer role.

### 2.7 Test Access Control

`[CLI]` Test that admin pages require `is_admin` and that role-based permissions control item editing:

```bash
# Anonymous cannot access admin
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# Expect: 302 or 303 (redirect to login)

# Authenticated admin can access admin pages
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# Expect: 200

# Login as editor_alice
rm -f /tmp/trovato-alice.txt
LP=$(curl -s -c /tmp/trovato-alice.txt http://localhost:3000/user/login)
CSRF=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-alice.txt -c /tmp/trovato-alice.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=editor_alice&password=tutorial-editor1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# editor_alice can edit items (has "edit any content")
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/item/$ID/edit"
# Expect: 200

# editor_alice cannot access admin pages (requires is_admin)
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# Expect: 403

# Login as viewer_carol
rm -f /tmp/trovato-carol.txt
LP=$(curl -s -c /tmp/trovato-carol.txt http://localhost:3000/user/login)
CSRF=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-carol.txt -c /tmp/trovato-carol.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=viewer_carol&password=tutorial-viewer1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# viewer_carol cannot edit items (no edit permissions)
curl -s -b /tmp/trovato-carol.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/item/$ID/edit"
# Expect: 403
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

### 3.2 Create Incoming and Curated Stages

`[CLI]` Stage configuration is not importable via `config import`. The YAML files serve as reference documentation. Stages are stored as tags in the `stages` category with corresponding `stage_config` rows. The Live stage already exists (created by the installer). Create Incoming and Curated via SQL:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato <<'SQL'
-- Incoming stage (internal, weight=0 so it sorts first)
INSERT INTO category_tag (id, category_id, label, weight, created, changed)
VALUES ('0193a5a0-0000-7000-8000-000000000002', 'stages', 'Incoming', 0,
  EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (id) DO NOTHING;

INSERT INTO stage_config (tag_id, machine_name, visibility, is_default)
VALUES ('0193a5a0-0000-7000-8000-000000000002', 'incoming', 'internal', false)
ON CONFLICT (tag_id) DO NOTHING;

-- Curated stage (internal, weight=5)
INSERT INTO category_tag (id, category_id, label, weight, created, changed)
VALUES ('0193a5a0-0000-7000-8000-000000000003', 'stages', 'Curated', 5,
  EXTRACT(EPOCH FROM NOW())::bigint, EXTRACT(EPOCH FROM NOW())::bigint)
ON CONFLICT (id) DO NOTHING;

INSERT INTO stage_config (tag_id, machine_name, visibility, is_default)
VALUES ('0193a5a0-0000-7000-8000-000000000003', 'curated', 'internal', false)
ON CONFLICT (tag_id) DO NOTHING;

-- Update Live stage weight to 10 (sorts last = final stage)
UPDATE category_tag SET weight = 10 WHERE id = '0193a5a0-0000-7000-8000-000000000001';
SQL
```

**Note:** `is_default` has a unique constraint — only one stage can be default. Live is the default. In a production editorial setup, you would set Incoming as default so new imports land there instead of Live.

### 3.3 Verify Stages Exist

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT ct.id, ct.label, sc.machine_name, sc.visibility FROM stage_config sc JOIN category_tag ct ON sc.tag_id = ct.id ORDER BY ct.weight;"
```

**Verify:** Three rows: Incoming (internal), Curated (internal), Live (public).

### 3.4 Review Workflow Configuration

`[REFERENCE]`

```bash
cat docs/tutorial/config/variable.workflow.editorial.yml
```

The workflow defines four transitions:
- `incoming → curated` (requires: `edit any content`)
- `curated → live` (requires: `publish conferences`)
- `live → curated` (requires: `publish conferences`)
- `curated → incoming` (requires: `edit any content`)

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

`[REFERENCE]` The `stage.legal_review.yml` config demonstrates that stages are just configuration — you could add a "Legal Review" stage between Curated and Live by creating the stage and adding `curated → legal_review` and `legal_review → live` transitions to the workflow config. No code changes needed. The current `variable.workflow.editorial.yml` does NOT include these transitions — they are left as an exercise.

Record stage and workflow details in `TOOLS.md -> Stages & Workflows`.

---

## Step 4: Revision History

### 4.1 Verify Revisions Exist

`[CLI]` Items created via the kernel API (JSON POST) get an initial revision. Plugin-imported items (via `tap_queue_worker`) do not create revisions. The three hand-created conferences from Part 1 should each have at least 1 revision:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT i.title, COUNT(r.id) AS revisions FROM item i JOIN item_revision r ON i.id = r.item_id WHERE i.type = 'conference' GROUP BY i.title ORDER BY revisions DESC LIMIT 5;"
```

**Verify:** The three hand-created conferences (RustConf, EuroRust, WasmCon) each have at least 1 revision.

### 4.2 View Revision History Page

`[CLI]` The revision history page requires authentication:

```bash
# Use a hand-created conference (has revisions), not an imported one
ID=$($(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT i.id FROM item i WHERE i.current_revision_id IS NOT NULL AND i.type = 'conference' LIMIT 1;" | tr -d ' ')

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
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/item/$ID/revisions | grep -c 'class="admin-table"'
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
echo -n "1. Users created: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM users WHERE name IN ('editor_alice', 'publisher_bob', 'viewer_carol');" | tr -d ' '
echo -n "2. Login works: "
rm -f /tmp/trovato-test.txt
LP=$(curl -s -c /tmp/trovato-test.txt http://localhost:3000/user/login)
TC=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-test.txt -c /tmp/trovato-test.txt -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$TC" -o /dev/null -w "%{http_code}"
echo ""
echo -n "3. Profile page: "; curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
echo -n "4. Stages exist: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM stage_config;" | tr -d ' '
echo -n "5. Revisions page: "; ID=$($(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT i.id FROM item i WHERE i.current_revision_id IS NOT NULL LIMIT 1;" | tr -d ' '); curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/revisions
echo -n "6. Admin content: "; curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
echo -n "7. Bulk actions: "; curl -s -b /tmp/trovato-test.txt http://localhost:3000/admin/content | grep -c 'name="action"'
echo ""
```

Expected output:
```
1. Users created: 3
2. Login works: 303
3. Profile page: 200
4. Stages exist: 3
5. Revisions page: 200
6. Admin content: 200
7. Bulk actions: >= 1
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-04-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
