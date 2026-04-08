# Recipe: Part 5 — Forms & User Input

> **Synced with:** `docs/tutorial/part-05-forms-and-input.md`
> **Sync hash:** f271cfa1
> **Last verified:** 2026-04-02 (added AI Assist buttons section, updated deferred table)
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

## Step 2: Block-Based Content Editing

### 2.1 Understand Block Architecture

`[REFERENCE]` Key concepts:
- `FieldType::Blocks` — a dedicated field type for ordered arrays of content blocks
- Storage format: JSON array of `[{type, weight, data}]` in JSONB `fields` column
- Different from `Compound` which stores `{"sections": [{type, weight, data}]}`
- Eight standard block types: paragraph, heading, image, list, quote, code, delimiter, embed
- `BlockTypeRegistry` in `crates/kernel/src/content/block_types.rs` manages type definitions
- Client-side: Editor.js with Trovato adapter in `static/js/block-editor.js`
- Server-side rendering: `render_blocks()` in `crates/kernel/src/content/block_render.rs`

### 2.2 Update Conference Content Type

`[CLI]` The conference content type config has been updated to use `Blocks` for the description field. Import it:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

**Verify:** Check that `field_description` is now type Blocks:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT settings->'fields' FROM item_type WHERE type = 'conference';" | grep -o '"Blocks"'
```

**Verify:** Output contains `"Blocks"`.

### 2.3 Build and Install the trovato_block_editor Plugin

`[CLI]`

```bash
# Build WASM
cargo build --target wasm32-wasip1 -p trovato_block_editor --release
mkdir -p plugin-dist
cp target/wasm32-wasip1/release/trovato_block_editor.wasm plugin-dist/

# Install
cargo run --release --bin trovato -- plugin install trovato_block_editor
```

> Restart the server after installing so `tap_install` fires.

**Verify:**

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugin_status WHERE name = 'trovato_block_editor';"
```

**Verify:** trovato_block_editor, status 1 (enabled).

### 2.4 Verify Block Editor Loads in Form

`[CLI]` Check that the content form loads Editor.js scripts for the conference type:

```bash
# Login as admin
rm -f /tmp/trovato-admin.txt
LP=$(curl -s -c /tmp/trovato-admin.txt http://localhost:3000/user/login)
CSRF=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-admin.txt -c /tmp/trovato-admin.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303

# Check form loads block editor
curl -s -b /tmp/trovato-admin.txt http://localhost:3000/admin/content/add/conference | grep -c 'block-editor.js'
# Expect: 1
```

### 2.5 Test Block Editor Upload Endpoint

`[CLI]` Verify the upload endpoint is accessible (gated by trovato_block_editor plugin):

```bash
# Without auth, should get 403
curl -s -o /dev/null -w "%{http_code}" -X POST http://localhost:3000/api/block-editor/upload
# Expect: 403
```

### 2.6 Test Block Editor Preview Endpoint

`[CLI]`

```bash
# Preview with test block data (requires auth)
curl -s -b /tmp/trovato-admin.txt \
  -H "Content-Type: application/json" \
  -X POST http://localhost:3000/api/block-editor/preview \
  -d '{"blocks":[{"type":"paragraph","data":{"text":"Hello from preview"}}]}' | grep -o 'Hello from preview'
# Expect: Hello from preview
```

### 2.7 Create a Conference with Blocks

`[UI-ONLY]` Navigate to `/admin/content/add/conference`:
1. Enter a conference name and dates
2. In the Description field, the block editor should appear (or JSON fallback)
3. Add a paragraph block, a heading block
4. Submit the form
5. View the created conference and verify the blocks render as HTML

`[CLI]` Verify blocks stored as JSON array:

```bash
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' ORDER BY created DESC LIMIT 1;")
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT jsonb_typeof(fields->'field_description') FROM item WHERE id = '$ID';"
```

**Verify:** Returns `array` (not `string` or `object`).

---

## Step 3: Block Rendering on Display

### 3.1 Understand Render Pipeline

`[REFERENCE]` Key concepts:
- `render_blocks()` in `block_render.rs` converts block JSON to semantic HTML
- Each block type maps to specific HTML: paragraph → `<p>`, heading → `<h2>`-`<h6>`, code → `<pre><code>`, etc.
- Text content sanitized via ammonia (strips `<script>`, event handlers, etc.)
- Unknown block types are silently skipped
- Item view route detects `FieldType::Blocks` fields and calls `render_blocks()` instead of compound section path

### 3.2 Verify Syntax Highlighting

`[CLI]` Test that code blocks get syntax highlighting:

```bash
# Create a preview with a code block
curl -s -b /tmp/trovato-admin.txt \
  -H "Content-Type: application/json" \
  -X POST http://localhost:3000/api/block-editor/preview \
  -d '{"blocks":[{"type":"code","data":{"code":"fn main() {}","language":"rust"}}]}' | grep -c 'language-rust'
# Expect: 1
```

### 3.3 Verify Embed Security

`[CLI]` Test that embed whitelist works:

```bash
# Whitelisted source (YouTube) should produce iframe
curl -s -b /tmp/trovato-admin.txt \
  -H "Content-Type: application/json" \
  -X POST http://localhost:3000/api/block-editor/preview \
  -d '{"blocks":[{"type":"embed","data":{"service":"youtube","source":"https://youtube.com/watch?v=test","embed":"https://youtube.com/watch?v=test"}}]}' | grep -c 'iframe'
# Expect: 1

# Non-whitelisted source should not produce iframe
curl -s -b /tmp/trovato-admin.txt \
  -H "Content-Type: application/json" \
  -X POST http://localhost:3000/api/block-editor/preview \
  -d '{"blocks":[{"type":"embed","data":{"service":"evil","source":"https://evil.com/payload","embed":"https://evil.com/payload"}}]}' | grep -c 'iframe'
# Expect: 0
```

---

## Step 4: Text Formats & Sanitization

### 4.1 Understand Text Formats

`[REFERENCE]` Key concepts:
- Three formats: `plain_text` (HTML-escaped), `filtered_html` (ammonia-sanitized), `full_html` (trusted, no sanitization)
- Text formats apply to `TextLong` fields (not Blocks — blocks have their own sanitization)
- `FilterPipeline::for_format_safe()` — validates format against allowlist, applies sanitization
- `FilterPipeline::for_format()` — NEVER use with untrusted format strings

### 4.2 Verify Text Format Permissions

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT r.name, rp.permission FROM role_permissions rp JOIN roles r ON rp.role_id = r.id WHERE rp.permission LIKE 'use %' ORDER BY r.name, rp.permission;"
```

**Verify:** editor has `use filtered_html`, publisher has `use filtered_html` and `use full_html`.

---

## Step 5: Block Validation & Custom Block Types

### 5.1 Understand BlockTypeRegistry

`[REFERENCE]` Key concepts:
- `BlockTypeRegistry::with_standard_types()` registers the 8 standard block types
- `validate_block(type_name, data)` checks type is registered, validates required fields
- `sanitize_blocks(blocks)` runs ammonia on text-bearing fields in-place
- `process_blocks_fields()` in `compound.rs` orchestrates parsing, validation, sanitization, and size limits (512KB, 100 blocks)

### 5.2 Review Block Type Registration

`[REFERENCE]` Skim:
- `crates/kernel/src/content/block_types.rs` — `BlockTypeDefinition`, `BlockTypeRegistry`, standard type schemas
- `crates/kernel/src/content/block_render.rs` — individual block renderers
- `crates/kernel/src/content/compound.rs` — `process_blocks_fields()`, size limits

### 5.3 Review Client-Side Integration

`[REFERENCE]` Skim:
- `static/js/block-editor.js` — Trovato ↔ Editor.js format mapping, tool config, form save binding
- `templates/admin/content-form.html` — `has_blocks` detection, CDN script loading, `data-block-editor` container

---

## Step 6: File & Image Uploads

### 6.1 Understand Upload Architecture

`[REFERENCE]` Key concepts:
- `FileService` in `crates/kernel/src/file/` handles uploads: sanitize filename, validate MIME type and magic bytes, store to local disk
- Temporary uploads (status 0) promoted to permanent (status 1) on save
- Cron cleans orphaned temp files after 6 hours
- Block editor uploads via `/api/block-editor/upload` restricted to image types (jpeg, png, gif, webp)
- Upload endpoint returns Editor.js-compatible response: `{ success: 1, file: { url: "..." } }`

### 6.2 Verify File Upload Security

`[CLI]`

```bash
# Verify upload requires authentication
curl -s -o /dev/null -w "%{http_code}" -X POST http://localhost:3000/file/upload
# Expect: 401
```

---

## Step 7: Form Alter & Validation Plugins

### 7.1 Review ritrovo_access Plugin

`[REFERENCE]` Key behaviors:
- `tap_item_access`: Kernel denies anonymous on internal stages; plugin checks authenticated users' permissions for Incoming/Curated; Neutral for Live
- `tap_perm`: Declares 7 permissions: `view incoming conferences`, `view curated conferences`, `edit conferences`, `publish conferences`, `post comments`, `edit own comments`, `edit any comments`
- `tap_item_view`: Strips `editor_notes` field from render tree for non-editors
- Grant/Deny/Neutral aggregation: any Deny wins → any Grant → Neutral falls through
- Operation-aware: view operations check stage-specific view permissions; non-view (edit, delete) require `edit conferences`

### 7.2 Build the Plugin

`[CLI]`

```bash
cargo build --target wasm32-wasip1 -p ritrovo_access --release
mkdir -p plugin-dist
cp target/wasm32-wasip1/release/ritrovo_access.wasm plugin-dist/
```

**Verify:** `plugin-dist/ritrovo_access.wasm` exists.

### 7.3 Install the Plugin

`[CLI]`

```bash
cargo run --release --bin trovato -- plugin install ritrovo_access
```

> Restart the server after installing so `tap_install` fires.

### 7.4 Verify Plugin Installation

`[CLI]`

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, status FROM plugin_status WHERE name = 'ritrovo_access';"
```

**Verify:** ritrovo_access, status 1 (enabled).

### 7.5 Assign Plugin Permissions to Roles

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

**Verify:** Check the permission matrix:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT r.name AS role, rp.permission FROM role_permissions rp JOIN roles r ON rp.role_id = r.id WHERE r.name IN ('viewer', 'editor', 'publisher') ORDER BY r.name, rp.permission;"
```

### 7.6 Test Stage-Based Access Control

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

# Anonymous access should be denied (kernel returns 404 to avoid revealing item exists)
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

---

## Step 8: User Profile Form

### 8.1 Test Profile Access Control

`[CLI]`

```bash
# Authenticated access
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
# Expect: 200

# Unauthenticated redirect
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
# Expect: 302 or 303
```

### 8.2 Verify Profile Form Content

`[CLI]`

```bash
curl -s -b /tmp/trovato-alice.txt http://localhost:3000/user/profile | grep -c 'name="mail"'
# Expect: >= 1 (email field present)
```

### 8.3 Test Profile Update

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
echo -n "3. trovato_block_editor plugin: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COALESCE((SELECT status::text FROM plugin_status WHERE name = 'trovato_block_editor'), 'not installed');" | tr -d ' '
echo -n "4. ritrovo_access plugin: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT COALESCE((SELECT status::text FROM plugin_status WHERE name = 'ritrovo_access'), 'not installed');" | tr -d ' '
echo -n "5. Profile page: "
rm -f /tmp/trovato-test.txt
LP=$(curl -s -c /tmp/trovato-test.txt http://localhost:3000/user/login)
TC=$(echo "$LP" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')
curl -s -b /tmp/trovato-test.txt -c /tmp/trovato-test.txt -X POST http://localhost:3000/user/login \
  -d "username=admin&password=trovato-admin1&_token=$TC" -o /dev/null -w "%{http_code}"
echo ""
echo -n "6. Profile accessible: "; curl -s -b /tmp/trovato-test.txt -o /dev/null -w "%{http_code}" http://localhost:3000/user/profile
echo -n "7. Blocks field type: "; $(brew --prefix libpq)/bin/psql -t postgres://trovato:trovato@localhost:5432/trovato -c "SELECT settings->'fields' FROM item_type WHERE type = 'conference';" | grep -c '"Blocks"' | tr -d ' '
echo ""
```

Expected output:
```
1. Form state table: 5
2. Text format perms: >= 2
3. trovato_block_editor plugin: 1
4. ritrovo_access plugin: 1
5. Profile page: 303
6. Profile accessible: 200
7. Blocks field type: 1
```

Create a database backup:
```bash
$(brew --prefix libpq)/bin/pg_dump \
  postgres://trovato:trovato@localhost:5432/trovato \
  -Fc -f backups/tutorial-part-05-$(date +%Y%m%d).dump
```

Record backup in `TOOLS.md -> Backups`.

All discoveries should be recorded in `TOOLS.md`.
