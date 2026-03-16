# Recipe: Part 5 — Forms & User Input

> **Synced with:** `docs/tutorial/part-05-forms-and-input.md`
> **Sync hash:** 519e0632
> **Last verified:** 2026-03-15 (plugins implemented and installed)
>
> Run `docs/tutorial/recipes/sync-check.sh` before starting to verify this recipe matches the current tutorial.

---

## Prerequisites

- Parts 1–4 must be completed (users, roles, stages, revisions, admin content management).
- Check `TOOLS.md` for server start commands, database connection, admin credentials, config import, plugin build commands.
- Database backup recommended:

```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/pre-part-05-$(date +%Y%m%d).dump
```

---

## Step 1: The Form API

### 1.1 Understand Form API Architecture

`[REFERENCE]` No action needed. Key concepts:
- Form API pipeline: Build (definition + `tap_form_alter`) → Validate (CSRF + field rules + `tap_form_validate`) → Submit (`tap_form_submit` + side effects)
- 13 element types: Textfield, Textarea, Select, Checkbox, Checkboxes, Radio, Hidden, Password, File, Submit, Fieldset, Markup, Container
- Fluent builder: `FormElement::textfield().title("Name").required().weight(0)`
- `AjaxConfig` attaches AJAX callbacks to elements
- `form_state_cache` table stores multi-step and AJAX form state in PostgreSQL
- Current admin forms use temporary `FormBuilder` in `content/form.rs`, not the Form API

### 1.2 Verify Form State Table

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = 'form_state_cache' ORDER BY ordinal_position;"
```

**Verify:** Columns: form_build_id (varchar), form_id (varchar), state (jsonb), created (bigint), updated (bigint).

### 1.3 Review Form API Source

`[REFERENCE]` Skim the Form API types and service:
- `crates/kernel/src/form/types.rs` — `Form`, `FormElement`, `ElementType`, `AjaxConfig` structs
- `crates/kernel/src/form/service.rs` — `FormService::build()`, `process()`, `ajax_callback()`, `save_state()`, `load_state()`, `cleanup_expired()`
- `crates/kernel/src/form/csrf.rs` — CSRF token generation and verification
- `crates/kernel/src/form/ajax.rs` — `AjaxRequest`, `AjaxResponse`, 12 AJAX command types
- `crates/kernel/src/content/form.rs` — Temporary `FormBuilder` that generates HTML from `ContentTypeDefinition`

Record key file locations in `TOOLS.md -> Form API`.

---

## Step 2: Rich Text Editing

### 2.1 Understand Text Formats

`[REFERENCE]` Key concepts:
- Three formats: `plain_text` (HTML-escaped), `filtered_html` (ammonia-sanitized), `full_html` (trusted, no sanitization)
- Permissions: `use filtered_html`, `use full_html` — assigned to editor and publisher roles in Part 4
- Storage: JSONB field stores `{ "value": "...", "format": "filtered_html" }`
- `FilterPipeline::for_format_safe()` — validates format against allowlist, applies sanitization
- `FilterPipeline::for_format()` — NEVER use with untrusted format strings

### 2.2 Verify Text Format Permissions

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT r.name, rp.permission FROM role_permissions rp JOIN roles r ON rp.role_id = r.id WHERE rp.permission LIKE 'use %' ORDER BY r.name, rp.permission;"
```

**Verify:** editor has `use filtered_html`, publisher has `use filtered_html` and `use full_html`.

### 2.3 Test Format Selection in Edit Form

`[CLI]` Log in as editor_alice and verify the text format selector shows appropriate options:

```bash
# Login as editor_alice (if not already)
rm -f /tmp/trovato-alice.txt
LP=$(curl -s -c /tmp/trovato-alice.txt http://localhost:3000/user/login)
CSRF=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-alice.txt -c /tmp/trovato-alice.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=editor_alice&password=tutorial-editor1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# Check edit form for text format selector
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")
curl -s -b /tmp/trovato-alice.txt "http://localhost:3000/item/$ID/edit" | grep -c 'filtered_html'
# Expect: >= 1 (format option present)
```

---

## Step 3: AJAX Form Interactions

### 3.1 Understand AJAX Infrastructure

`[REFERENCE]` Key concepts:
- `AjaxConfig::new("callback_name").event("change").wrapper("#target")` — attaches callbacks
- Built-in triggers: `add_{field}` (add multi-value item), `remove_{field}_{index}` (remove item)
- Custom triggers dispatched via `tap_form_ajax` to plugins
- `AjaxResponse` commands: replace, append, prepend, remove, invoke, alert, redirect, css, data, settings, restripe, add_css
- Progressive enhancement: forms work without JavaScript via full page reload
- Form state maintained in `form_state_cache` during AJAX round-trips

### 3.2 Review AJAX Source

`[REFERENCE]`
- `crates/kernel/src/form/ajax.rs` — `AjaxRequest`, `AjaxResponse`, command types
- `crates/kernel/src/form/service.rs` — `ajax_callback()`, `handle_ajax_trigger()`, `handle_add_item()`, `handle_remove_item()`

---

## Step 4: Multi-Step Conference Submission

### 4.1 Understand Multi-Step Architecture

`[REFERENCE]` Key concepts:
- Three steps: Basics (name, URL, dates, location) → Details (CFP, topics, description, uploads) → Review (read-only summary + submit)
- `FormState.step` tracks current step (0-indexed)
- State serialized to `form_state_cache` table between requests
- Each step validates before advancing; back navigation preserves data
- File uploads tracked as temp files in `extra` map; promoted on final submit
- Final submission creates Item on Incoming stage
- `create content` permission required; anonymous redirected to login
- Stale state cleaned by `FormService::cleanup_expired()` after 6 hours

### 4.2 Verify Form State Table Schema

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "\d form_state_cache"
```

**Verify:** Table exists with form_build_id (PK), form_id, state (jsonb), created, updated.

### 4.3 Verify Stale State Cleanup

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM form_state_cache WHERE updated < EXTRACT(EPOCH FROM NOW())::bigint - 21600;"
```

**Verify:** Returns a count (0 or more). Any stale entries would be cleaned by cron.

---

## Step 5: The `ritrovo_cfp` Plugin

### 5.1 Review Plugin Design

`[REFERENCE]` Key behaviors:
- `tap_item_view`: Computes CFP days remaining, injects color-coded badge (green >14d, yellow 7-14d, red <7d)
- `tap_item_insert`/`tap_item_update`: Validates `cfp_end_date <= end_date`
- Queues `cfp_closing_soon` events to `ritrovo_notifications` queue when CFP enters 7-day window
- SDK features: `item_load()`, `queue_push()`, structured logging

### 5.2 Build the Plugin

`[CLI]`

```bash
cd plugins/ritrovo_cfp
cargo build --target wasm32-wasip1 --release
mkdir -p ../../plugin-dist
cp ../../target/wasm32-wasip1/release/ritrovo_cfp.wasm ../../plugin-dist/
```

**Verify:** `plugin-dist/ritrovo_cfp.wasm` exists.

> **Note:** WASM output goes to the workspace `target/` directory, not `plugins/ritrovo_cfp/target/`.

### 5.3 Install the Plugin

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin install ritrovo_cfp
```

> Restart the server after installing so `tap_install` fires.

### 5.4 Verify Plugin Installation

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugin_status WHERE name = 'ritrovo_cfp';"
```

**Verify:** ritrovo_cfp, status 1 (enabled).

### 5.5 Test CFP Badge

`[CLI]` Set a test conference's CFP end date to a future date and verify the badge renders:

```bash
# Set CFP end date on a test conference
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")
# Update via admin edit form or SQL as appropriate

# View the conference page and check for the CFP badge
curl -s -b /tmp/trovato-cookies.txt "http://localhost:3000/item/$ID" | grep -c 'cfp-badge'
```

Record plugin build and install commands in `TOOLS.md -> Plugins`.

---

## Step 6: The `ritrovo_access` Plugin

### 6.1 Review Plugin Design

`[REFERENCE]` Key behaviors:
- `tap_item_access`: Kernel denies anonymous on internal stages; plugin checks authenticated users' permissions for Incoming/Curated; Neutral for Live
- `tap_perm`: Declares 7 permissions: `view incoming conferences`, `view curated conferences`, `edit conferences`, `publish conferences`, `post comments`, `edit own comments`, `edit any comments`
- `tap_item_view`: Strips `editor_notes` field from render tree for non-editors
- Grant/Deny/Neutral aggregation: any Deny wins → any Grant → Neutral falls through

### 6.2 Build the Plugin

`[CLI]`

```bash
cd plugins/ritrovo_access
cargo build --target wasm32-wasip1 --release
mkdir -p ../../plugin-dist
cp ../../target/wasm32-wasip1/release/ritrovo_access.wasm ../../plugin-dist/
```

**Verify:** `plugin-dist/ritrovo_access.wasm` exists.

> **Note:** WASM output goes to the workspace `target/` directory, not `plugins/ritrovo_access/target/`.

### 6.3 Install the Plugin

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin install ritrovo_access
```

> Restart the server after installing so `tap_install` fires.

### 6.4 Verify Plugin Installation

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugin_status WHERE name = 'ritrovo_access';"
```

**Verify:** ritrovo_access, status 1 (enabled).

### 6.5 Verify Plugin Is Active

`[CLI]` After enabling the plugin, verify it has the expected taps:

```bash
# Check that the plugin is installed and has the expected taps
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugin_status WHERE name = 'ritrovo_access';"
# Expect: ritrovo_access, 1

# The plugin declares 7 permissions via tap_perm — these will be visible
# in the admin permissions UI after the plugin is enabled. They are NOT
# in role_permissions yet (that happens in the next step).
```

### 6.6 Assign Plugin Permissions to Roles

`[CLI]` After installing `ritrovo_access`, assign its permissions to the viewer, editor, and publisher roles:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato <<'SQL'
-- Viewer: can see all editorial stages
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r, (VALUES
  ('view incoming conferences'), ('view curated conferences')
) AS p(perm)
WHERE r.name = 'viewer'
ON CONFLICT (role_id, permission) DO NOTHING;

-- Editor: viewer permissions + edit conferences
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r, (VALUES
  ('view incoming conferences'), ('view curated conferences'),
  ('edit conferences')
) AS p(perm)
WHERE r.name = 'editor'
ON CONFLICT (role_id, permission) DO NOTHING;

-- Publisher: editor permissions + publish conferences
INSERT INTO role_permissions (role_id, permission)
SELECT r.id, p.perm
FROM roles r, (VALUES
  ('view incoming conferences'), ('view curated conferences'),
  ('edit conferences'), ('publish conferences')
) AS p(perm)
WHERE r.name = 'publisher'
ON CONFLICT (role_id, permission) DO NOTHING;
SQL
```

**Verify:** Check the full permission matrix:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT r.name AS role, rp.permission FROM role_permissions rp JOIN roles r ON rp.role_id = r.id WHERE r.name IN ('viewer', 'editor', 'publisher') ORDER BY r.name, rp.permission;"
```

### 6.7 Test Stage-Based Access Control

`[CLI]` Move an item to the Incoming stage and verify anonymous users cannot access it:

```bash
# Look up stage IDs by machine name
INCOMING_ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT tag_id FROM stage_config WHERE machine_name = 'incoming';")
LIVE_ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT tag_id FROM stage_config WHERE machine_name = 'live';")

# Pick a test conference
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")

# Move a test conference to Incoming
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "UPDATE item SET stage_id = '$INCOMING_ID' WHERE id = '$ID';"

# Anonymous access should be denied (kernel denies anonymous on internal stages).
# The kernel returns 404 (not 403) to avoid revealing the item exists.
curl -s -o /dev/null -w "%{http_code}" "http://localhost:3000/item/$ID"
# Expect: 404

# Editor should have access (has view incoming conferences permission)
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/item/$ID"
# Expect: 200

# Restore to Live stage
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "UPDATE item SET stage_id = '$LIVE_ID' WHERE id = '$ID';"
```

> **Note:** After changing stage_id via SQL, you may need to restart the server to clear the item cache. Stage changes through the kernel API handle cache invalidation automatically.

Record plugin testing patterns in `TOOLS.md -> Plugins`.

---

## Step 7: User Profile Form

### 7.1 Test Profile Access Control

`[CLI]`

```bash
# Authenticated access
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
# Expect: 200

# Unauthenticated redirect
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
# Expect: 302 or 303
```

### 7.2 Verify Profile Form Content

`[CLI]`

```bash
curl -s -b /tmp/trovato-alice.txt http://localhost:3000/user/profile | grep -c 'name="mail"'
# Expect: >= 1 (email field present)
```

### 7.3 Test Profile Update

`[UI-ONLY]` Navigate to `/user/profile` as editor_alice:
1. Update the display name
2. Submit the form
3. Verify the change is reflected
4. Test password change (enter current password, new password, confirm)

---

## Completion Checklist

```bash
echo "=== Part 5 Completion Checklist ==="
echo -n "1. Form state table: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM information_schema.columns WHERE table_name = 'form_state_cache';" | tr -d ' '
echo -n "2. Text format perms: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COUNT(*) FROM role_permissions WHERE permission LIKE 'use %';" | tr -d ' '
echo -n "3. Profile page: "
rm -f /tmp/trovato-test.txt
LP=$(curl -s -c /tmp/trovato-test.txt http://localhost:3000/user/login)
TC=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-test.txt -c /tmp/trovato-test.txt -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$TC" -o /dev/null -w "%{http_code}"
echo ""
echo -n "4. Profile accessible: "; curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
echo -n "5. ritrovo_cfp plugin: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COALESCE((SELECT status::text FROM plugin_status WHERE name = 'ritrovo_cfp'), 'not installed');" | tr -d ' '
echo -n "6. ritrovo_access plugin: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COALESCE((SELECT status::text FROM plugin_status WHERE name = 'ritrovo_access'), 'not installed');" | tr -d ' '
echo ""
```

Expected output:
```
1. Form state table: 5
2. Text format perms: >= 2
3. Profile page: 303
4. Profile accessible: 200
5. ritrovo_cfp plugin: 1
6. ritrovo_access plugin: 1
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-05-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
