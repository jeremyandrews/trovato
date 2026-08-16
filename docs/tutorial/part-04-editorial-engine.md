# Part 4: The Editorial Engine

Part 3 gave Ritrovo its visual identity. Part 4 gives it editorial discipline.

With 5,000+ conferences flowing in from the importer, someone needs to review them before they go live. You need users who can log in, roles that control who can do what, stages that separate incoming content from published content, workflows that enforce review steps, and revision history so no edit is ever lost.

By the end of this part, Ritrovo will have multi-user authentication, five roles from anonymous to admin, three editorial stages with enforced transitions, full revision tracking, and an admin interface with filters and bulk operations.

**Start state:** Single admin user, everything on Live stage, no access control.
**End state:** Multiple users, role-based permissions, three-stage editorial workflow, revision history, and efficient admin content management.

---

## Step 1: Users & Authentication

Trovato already has a user system -- you created an admin account during the installer in Part 1. This step explores the full authentication architecture and creates test users for the editorial workflow.

### The User Model

Users in Trovato are stored in the `users` table with:

- **UUID** identifier (UUIDv7, time-sortable like items)
- **Name** (`name` column) -- unique username, used for login
- **Email** (`mail` column) -- unique, used for account recovery
- **Password hash** (`pass` column) -- Argon2id with RFC 9106 parameters (m=65536 KiB, t=3 iterations, p=4 parallelism)
- **Admin flag** (`is_admin`) -- superuser bypass for all permission checks
- **Status** -- 1=active, 0=blocked
- **Roles** -- assigned via `user_roles` join table (many-to-many with `roles`)
- **Consent tracking** -- `consent_given`, `consent_date`, `consent_version`, `data_retention_days` for GDPR compliance (nullable -- consent collection is plugin territory; the kernel stores the result)
- **Data** -- JSONB extension field for profile information

### Session Architecture

When a user logs in:

1. The kernel verifies the password against the Argon2id hash.
2. A new session is created in Redis with an HttpOnly, Secure cookie.
3. The session ID is **cycled** (regenerated) to prevent session fixation attacks -- if an attacker obtained a session cookie before login, it becomes invalid.
4. The user's ID is stored in the session (`SESSION_USER_ID`).

Sessions expire after inactivity (configurable via `SESSION_TTL`). Logout is always a POST request (never GET) with CSRF protection, preventing logout CSRF attacks.

### Password Requirements

- Minimum 12 characters (do not reduce this -- it is a security baseline)
- Argon2id with RFC 9106 recommended parameters
- No maximum length (within reason -- the hash function normalizes input)

### Enabling Registration

By default, Trovato disables user registration. The tutorial config at `docs/tutorial/config/variable.allow_user_registration.yml` enables it:

```yaml
key: allow_user_registration
value: true
```

Import it:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

### Creating Test Users

For the editorial workflow demonstration, create three test users with different roles. Register via the form at `/user/register` or via curl:

```bash
# Get the registration form and extract CSRF token
rm -f /tmp/trovato-register.txt
REG_PAGE=$(curl -s -c /tmp/trovato-register.txt http://localhost:3000/user/register)
CSRF=$(echo "$REG_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

# Register editor_alice
curl -s -b /tmp/trovato-register.txt -c /tmp/trovato-register.txt \
  -X POST http://localhost:3000/user/register \
  -d "username=editor_alice&mail=alice@example.com&password=tutorial-editor1&confirm_password=tutorial-editor1&_token=$CSRF" \
  | grep -o 'Registration successful'
# Expect: Registration successful
```

Repeat for `publisher_bob` and `viewer_carol`, fetching a fresh CSRF token each time. Note: the registration endpoint has a rate limit of 3 per hour per IP — clear the Redis key `rate:register:unknown` between registrations if needed.

[<img src="images/part-04/registration-page.png" width="600" alt="The user registration form with username, email, and password fields">](images/part-04/registration-page.png)

Users are created in **inactive** status pending email verification. Since there is no mail server in the tutorial, activate them via SQL:

```bash
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "UPDATE users SET status = 1 WHERE name IN ('editor_alice', 'publisher_bob', 'viewer_carol');"
```

### Login and Logout

Login via `POST /user/login` establishes a session:

```bash
rm -f /tmp/trovato-alice.txt
LOGIN_PAGE=$(curl -s -c /tmp/trovato-alice.txt http://localhost:3000/user/login)
CSRF=$(echo "$LOGIN_PAGE" | grep -oE 'name="_token" value="[a-f0-9]+"' | grep -oE '[a-f0-9]{64}')

curl -s -b /tmp/trovato-alice.txt -c /tmp/trovato-alice.txt \
  -X POST http://localhost:3000/user/login \
  -d "username=editor_alice&password=tutorial-editor1&_token=$CSRF" \
  -o /dev/null -w "%{http_code}"
# Expect: 303 (redirect to homepage or dashboard)
```

The page template's user menu now shows the username, "My Account", and a "Logout" button (which submits a POST form with CSRF token).

[<img src="images/part-04/login-page.png" width="600" alt="The Trovato login page with username and password fields">](images/part-04/login-page.png)

### User Profile

Logged-in users can manage their account at `/user/profile`. This page shows the profile edit form (name, email, timezone) and a password change form. It requires authentication -- unauthenticated requests redirect to `/user/login`.

### Verify

```bash
# Confirm users exist
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT name, mail FROM users WHERE name IN ('editor_alice', 'publisher_bob', 'viewer_carol');"
# Expect: three rows
```

<details>
<summary>Under the Hood: Rate Limiting and Account Security</summary>

The login endpoint implements rate limiting to prevent brute-force password attacks. After too many failed attempts from the same IP or for the same account, the endpoint returns temporary lockout responses.

The kernel also:
- Never reveals whether a username exists (login errors say "Invalid username or password" for both cases)
- Stores only the Argon2id hash, never the plaintext password
- Cycles the session ID after every authentication state change (login, logout, role change)
- Uses `HttpOnly` and `SameSite=Lax` cookie attributes to prevent XSS-based session theft

</details>

---

## Step 2: Roles & Permissions

With users in place, the next step is controlling what each user can do. Trovato uses a role-based permission system where roles are assigned to users and permissions are assigned to roles.

### Built-in Roles

Every Trovato installation has three implicit roles:

| Role | Assigned To | Key Permissions |
|---|---|---|
| **anonymous** | All unauthenticated visitors | `access content` (view Live items) |
| **authenticated** | All logged-in users | `access content` |
| **administrator** | Users with the admin flag | All permissions (superuser) |

These roles exist automatically -- you don't need to create them.

### Custom Roles

For the editorial workflow, Ritrovo needs three additional roles:

| Role | Purpose | Key Permissions |
|---|---|---|
| **viewer** | Can view content on all editorial stages | `access content`, `view incoming conferences`, `view curated conferences` |
| **editor** | Reviews and curates content | Viewer permissions + `create content`, `edit own content`, `edit any content`, `access files`, `use filtered_html`, `edit conferences` |
| **publisher** | Publishes content to Live | Editor permissions + `delete any content`, `administer files`, `use full_html`, `publish conferences` |

The role definitions live at `docs/tutorial/config/role.viewer.yml`, `role.editor.yml`, and `role.publisher.yml`. These YAML files can be imported via `config import`, or roles can be managed through the admin UI at `/admin/people/roles` and `/admin/people/permissions`.

[<img src="images/part-04/roles-admin.png" width="600" alt="The roles administration page showing the list of defined roles">](images/part-04/roles-admin.png)

Note that the `view incoming conferences`, `view curated conferences`, `edit conferences`, and `publish conferences` permissions are declared by the `ritrovo_access` plugin (installed in Part 5). They must be added to role permissions after that plugin is installed. The role YAML files document the final intended state including these permissions.

### Assigning Roles to Test Users

There is no admin UI for assigning roles to users — the `/admin/people` list does not have an Edit action with role checkboxes. Instead, assign roles directly via SQL:

```sql
-- Assign editor role to editor_alice
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r
WHERE u.name = 'editor_alice' AND r.name = 'editor';

-- Assign publisher role to publisher_bob
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r
WHERE u.name = 'publisher_bob' AND r.name = 'publisher';

-- Assign viewer role to viewer_carol
INSERT INTO user_roles (user_id, role_id)
SELECT u.id, r.id FROM users u, roles r
WHERE u.name = 'viewer_carol' AND r.name = 'viewer';
```

> **Note:** At this point the viewer role only has `access content` — functionally identical to bare `authenticated`. The viewer role becomes meaningful in Part 5 when the `ritrovo_access` plugin is installed and `view incoming conferences` / `view curated conferences` are assigned. Creating the role now ensures viewer_carol has the right role binding before those permissions are added.

### Permission Model

Permissions in Trovato are strings like `"access content"`, `"edit any content"`, `"use filtered_html"`. Plugins declare permissions via the `tap_perm` tap. The kernel aggregates permissions from all enabled plugins and maps them to roles.

[<img src="images/part-04/permissions-matrix.png" width="600" alt="The permissions matrix showing roles across columns and permissions down rows">](images/part-04/permissions-matrix.png)

When the kernel checks whether a user can perform an action:

1. It loads the user's roles (implicit + assigned).
2. For each role, it checks whether the role has the required permission.
3. If any role grants the permission, the user can proceed.

For items specifically, the access check is layered:

1. **Admin bypass:** `is_admin` users always have access.
2. **Stage visibility:** If the item is on an internal stage (Incoming or Curated), anonymous users are denied immediately. The "published view" shortcut (step 3) is skipped for internal stages.
3. **Published view:** For items on public stages, anyone with `"access content"` can view published items.
4. **Plugin hook:** `tap_item_access` lets plugins return **Grant**, **Deny**, or **Neutral**. The kernel passes the user's permissions, authentication status, and the item's stage info to plugins so they can make informed decisions. Any Deny = denied. Any Grant with no Deny = allowed.
5. **Role-based fallback:** The kernel checks both generic and type-specific permission patterns: `"edit any content"`, `"edit any conference"`, `"edit own content"` (for the item's author), and `"edit conference content"`. Any match grants access.

> **Note:** Admin pages (`/admin/*`) require `is_admin` — they are not accessible through role permissions. Editors and publishers use the item routes (`/item/{id}/edit`, `/item/{id}/revisions`) directly, not the admin UI.

### Verify

Log in as each user and confirm different access levels:

```bash
# As anonymous (no cookie jar) -- should see /conferences
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/conferences
# 200

# As admin -- should see /admin/content
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# 200

# As editor_alice -- can edit items but not access admin pages
# (log in as alice first, store cookie jar at /tmp/trovato-alice.txt)
ID=$(psql -tA -c "SELECT id FROM item WHERE type = 'conference' LIMIT 1;")
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/edit
# 200
curl -s -b /tmp/trovato-alice.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# 403

# As viewer_carol -- can view but not edit
curl -s -b /tmp/trovato-carol.txt -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/edit
# 403
```

---

## Step 3: Stages & the Editorial Workflow

Every item in Trovato has a `stage_id` that determines its visibility. Until now, all content has been on the **Live** stage (the well-known UUID `0193a5a0-0000-7000-8000-000000000001`), which means everything is publicly visible. The editorial workflow introduces two additional stages.

### Three Stages

| Stage | Visibility | Purpose |
|---|---|---|
| **Incoming** | Internal | New imports land here, invisible to the public |
| **Curated** | Internal | Editor-reviewed content, ready for publisher approval |
| **Live** | Public | Published content, visible to all visitors |

"Internal" means only authenticated users with the right permissions can see items on that stage. The kernel denies anonymous users on internal stages before dispatching to any plugins. The `ritrovo_access` plugin (installed in Part 5) then checks whether authenticated users have the `view incoming conferences` or `view curated conferences` permissions.

### How Stages Work

Stages are stored as the `stage_id` column on the `item` table -- a foreign key to a stage definition. The kernel's Gather engine wraps every query with a stage visibility filter:

- **Anonymous users** see only items where `stage_id` points to a Public stage.
- **Viewers** (with `view incoming conferences` + `view curated conferences`) see items on all three tutorial stages.
- **Editors** and **Publishers** inherit viewer permissions and also see all three stages, plus have additional editing/publishing capabilities.

This filtering happens transparently in the SQL -- the Gather definition doesn't need to specify stage logic explicitly. The `stage_aware: true` flag in the Gather's definition JSON enables this behavior.

### The Workflow Transition Graph

Not all stage changes are valid. The editorial workflow defines which transitions are allowed:

```
incoming → curated     (requires: "edit any content")
curated  → live        (requires: "publish conferences")
live     → curated     (requires: "publish conferences")
curated  → incoming    (requires: "edit any content")
```

An editor (who has `edit any content`) can move content between Incoming and Curated. A publisher (who has `publish conferences`) can promote content to Live or unpublish it. Invalid transitions (like Incoming → Live) are rejected by the kernel.

The workflow is configured in `docs/tutorial/config/variable.workflow.editorial.yml`:

```yaml
key: workflow.editorial
value:
  transitions:
    - from: incoming
      to: curated
      permission: "edit any content"
      label: "Promote to Curated"
    - from: curated
      to: live
      permission: "publish conferences"
      label: "Publish"
    - from: live
      to: curated
      permission: "publish conferences"
      label: "Unpublish to Curated"
    - from: curated
      to: incoming
      permission: "edit any content"
      label: "Demote to Incoming"
```

### Import Pipeline and Stages

When the `ritrovo_importer` plugin imports conferences from confs.tech, it creates items on the configured default stage. In a production setup, you would configure it to target the **Incoming** stage so that imported conferences are not publicly visible until an editor reviews and promotes them.

### Walking Through the Workflow

Here is the editorial lifecycle for a newly imported conference:

1. **Importer creates conference** on the Incoming stage. It appears in `/admin/content` filtered by Incoming but is invisible on `/conferences`.

2. **Editor reviews** -- `editor_alice` logs in, filters the content list by Incoming, opens a conference, reviews the data (title, dates, location, description), and promotes it to Curated.

3. **Publisher approves** -- `publisher_bob` filters by Curated, verifies the conference meets quality standards, and promotes it to Live.

4. **Public sees it** -- The conference now appears on `/conferences`, in search results, and at its pathauto alias.

### Stage-Aware Gathers

The five Gather queries from Part 2 all have `stage_aware: true`. Demonstrate the effect:

```bash
# Anonymous sees only Live conferences
curl -s http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute | jq '.total'

# An editor would see more results (Incoming + Curated + Live)
# depending on their session and permissions
```

### Stage-Aware Search

Search is also stage-aware. An anonymous search for "rust" returns only Live conferences. An authenticated editor's search returns matches from all stages they can access.

### Extensibility Demo: Adding a New Stage

One of Trovato's strengths is that stages are configuration, not code. You can add a new stage without writing any Rust.

To add a "Legal Review" stage between Curated and Live, you would:

1. Create the stage in the database (a `category_tag` row in the `stages` category plus a `stage_config` row with `visibility: internal`). A reference config is at `docs/tutorial/config/stage.legal_review.yml`.
2. Update `variable.workflow.editorial.yml` to replace the `curated → live` transition with `curated → legal_review` and `legal_review → live`, then re-import.

No code changes, no plugin rebuild. The new workflow path works as soon as the config is imported and the stage exists in the database.

Stages can be imported via `config import` using `stage.{machine_name}.yml` files, or created via the admin UI. The workflow variable is also importable.

### Verify

```bash
# Check stages exist
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT ct.id, ct.label, sc.machine_name, sc.visibility
      FROM stage_config sc
      JOIN category_tag ct ON sc.tag_id = ct.id
      ORDER BY ct.weight;"
```

<details>
<summary>Under the Hood: Stage Visibility SQL</summary>

When a stage-aware Gather runs, the kernel wraps the query with a CTE that filters items by stage visibility:

```sql
WITH visible_stages AS (
    SELECT ct.id FROM category_tag ct
    JOIN stage_config sc ON ct.id = sc.tag_id
    WHERE sc.visibility = 'public'
    -- For authenticated users with permissions, this also includes 'internal' stages
)
SELECT i.*
FROM item i
JOIN visible_stages vs ON i.stage_id = vs.id
WHERE i.type = 'conference' AND i.status = 1
ORDER BY i.fields->>'field_start_date' ASC
```

The `visible_stages` CTE adapts based on the requesting user's permissions. Anonymous users see only Public stages. Editors see Internal and Public. This is the same mechanism that powers `load_for_view()` in the item handler -- the permission check is centralized.

</details>

---

## Step 4: Revision History

Every edit to an item creates a new row in the `item_revision` table. No data is ever overwritten -- you can always see what changed and when, and revert to any previous version.

### How Revisions Work

When an item is saved:

1. The kernel snapshots the current state (title, fields, status, stage) into a new `item_revision` row.
2. The `item.current_revision_id` pointer is updated to the new revision.
3. The previous revision remains in the database, untouched.

Each revision records:
- **Timestamp** -- When the edit was made
- **Author** -- Who made the edit
- **Log message** -- An optional description of the change (e.g., "Updated via admin UI", "Imported by ritrovo_importer")
- **Change summary** -- A structured JSONB diff of what changed (which fields were added, removed, or modified). Auto-generated by the kernel when saving.
- **AI-generated flag** -- Whether AI was involved in creating this revision (set automatically when `ai_request()` is called during the save process)
- **Full field snapshot** -- The complete `fields` JSONB at that point in time
- **Retention days** -- Optional data lifecycle period; when set, a retention plugin can clean up expired content

Revisions are immutable at the database level -- a PostgreSQL trigger prevents UPDATE operations on the `item_revision` table. Restoring a previous version creates a *new* revision with the old content, preserving the full audit trail.

### Viewing Revision History

The revision history page is available at `/item/{id}/revisions` (requires authentication). It shows a table with:

- Date and time of each revision
- Title at that revision
- Log message
- A "current" badge on the latest revision
- A **Revert** button on older revisions

Navigate to any conference and click the **Revisions** link in the admin operations column on the content list page.

[<img src="images/part-04/revision-history.png" width="600" alt="The revision history page showing a table of revisions with timestamps, titles, and revert buttons">](images/part-04/revision-history.png)

### Revert

Reverting creates a **new** revision with the content from the selected historical revision. It never deletes old revisions. After a revert, the revision history shows:

1. Original version
2. Bad edit
3. Revert (contains original version's data)

This means the "bad edit" is still in the history for auditing purposes. The `current_revision_id` simply points to the revert revision.

### Five Revision Scenarios

The Trovato design spec defines five key revision scenarios:

1. **Basic revision** -- Edit a Live conference (change the description). The revision table gets a new row, and `current_revision_id` updates.

2. **Revert** -- Make a bad edit (change title to "WRONG"), then revert to the previous revision. The title is restored, and the history shows three entries.

3. **Draft-while-live** -- A Live conference has a published version visible to the public. An editor creates a Curated draft with significant changes. The public still sees the Live version. Publishing the draft replaces the Live version.

4. **Cross-stage field updates** -- The importer updates structured fields (dates, location) on a Live conference that also has a Curated draft. The kernel writes one revision, and the `tap_item_save` context includes `other_stage_revisions` so plugins are aware of the draft.

5. **Emergency unpublish** -- Set `active = false` on a Live conference's revision. The item immediately disappears from public Gathers and search without going through a stage transition. This is a safety valve for urgent content removal.

### Verify

```bash
# Pick a conference that has revisions (plugin-imported items don't get revision rows —
# only items created or edited via the kernel API have revision history)
ID=$($(brew --prefix libpq)/bin/psql -tA postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id FROM item WHERE type = 'conference' AND current_revision_id IS NOT NULL LIMIT 1;")

# Check revision count
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT COUNT(*) FROM item_revision WHERE item_id = '$ID';"
# At least 1

# View the revision history page (requires auth)
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/item/$ID/revisions
# 200
```

---

## Step 5: Admin Content Management

With thousands of conferences and an editorial workflow, administrators need efficient tools for managing content. The admin content list at `/admin/content` provides filtering, bulk operations, and quick actions.

### Content List with Filters

The content list supports filtering by:

- **Content type** -- Conference, Speaker, or all types
- **Publishing status** -- Published, Unpublished, or all

Filter controls appear above the content table. Selecting a filter and clicking **Filter** reloads the list with the matching items.

[<img src="images/part-04/admin-content-list.png" width="600" alt="The admin content list with type and status filter controls">](images/part-04/admin-content-list.png)

### Content Table

Each row in the content table shows:

| Column | Content |
|---|---|
| Checkbox | For bulk selection |
| Title | Item title (linked to edit form) |
| Type | Content type label |
| Author | Username of the item's creator |
| Status | Published / Unpublished badge |
| Updated | Last modification timestamp |
| Operations | Edit, Revisions, Delete links |

### Bulk Operations

Select multiple items via checkboxes (or "Select all" to check the entire page), choose an action from the dropdown, and click **Apply**:

| Action | Effect |
|---|---|
| **Publish** | Sets `status = 1` on selected items |
| **Unpublish** | Sets `status = 0` on selected items |
| **Delete** | Permanently removes selected items (with confirmation) |

Bulk operations report results: "5 item(s) published." or "3 item(s) published. 2 item(s) failed." if some items couldn't be processed.

The delete action prompts for JavaScript confirmation before submitting to prevent accidental data loss.

### Bulk Operation Security

- **Authentication required** -- Only administrators can access `/admin/content`.
- **CSRF protection** -- The bulk action form includes a CSRF token.
- **Action validation** -- The server validates the action string against an allowlist (`publish`, `unpublish`, `delete`). Unknown actions are rejected.
- **Per-item error handling** -- If an individual item update fails (e.g., permission denied), the error is logged and counted without aborting the entire batch.

### Quick Actions

The operations column provides per-item quick actions:

- **Edit** -- Opens the content edit form.
- **Revisions** -- Opens the revision history page.
- **Delete** -- Submits a CSRF-protected POST to delete the single item.

### Flash Messages

After bulk operations or single-item actions, a flash message appears at the top of the content list confirming the result. Flash messages are stored in the session and cleared after display.

### Verify

```bash
# Content list loads with all items
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" http://localhost:3000/admin/content
# 200

# Filter by type
curl -s -b /tmp/trovato-cookies.txt -o /dev/null -w "%{http_code}" "http://localhost:3000/admin/content?type=conference"
# 200

# Check for bulk action form
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/admin/content | grep -c 'name="action"'
# 1 (the bulk action dropdown)

# Check for checkboxes
curl -s -b /tmp/trovato-cookies.txt http://localhost:3000/admin/content | grep -c 'name="ids\[\]"'
# Should be > 0 (one per item row)
```

---

## What You've Built

By the end of Part 4, you have:

- **Multi-user authentication** with Argon2id password hashing, Redis sessions, and session fixation protection.
- **Six roles** -- anonymous, authenticated, viewer, editor, publisher, and administrator -- with permission-based access control.
- **Three editorial stages** -- Incoming (internal), Curated (internal), and Live (public) -- with enforced workflow transitions.
- **Revision history** on items created or edited through the kernel API, with revert capability and audit trail.
- **Admin content management** with type and status filters, bulk operations (publish/unpublish/delete), and per-item quick actions.
- **Stage-aware content visibility** in Gathers and search -- anonymous users see only Live content.

You also now understand:

- How Trovato's session system prevents fixation attacks and enforces CSRF protection.
- How the role → permission → access check chain controls what each user can do.
- How stages separate editorial workflows from public content delivery.
- How revisions create an append-only audit trail where no edit is ever lost.
- How the kernel aggregates Grant/Deny/Neutral access decisions from plugins.

Ritrovo is now a full editorial CMS: conferences flow in from the importer, editors review and curate them, publishers promote them to Live, and visitors browse a polished, searchable directory. Every edit is tracked, every action is authorized, and the workflow is enforced by the kernel.

---

## What's Deferred

| Feature | Deferred To | Reason |
|---|---|---|
| WYSIWYG editor | Part 5 | Rich text editing is a form enhancement |
| AJAX form interactions | Part 5 | Progressive enhancement |
| Comments | Part 6 | Depends on full user system being stable |
| User notifications | Part 6 | Depends on comments |
| Internationalization | Part 7 | Separate concern |
| REST API authentication (tokens) | Part 5+ | API auth is separate from session auth |
| Revision diff UI | Part 5+ | Visual diff display is a UI enhancement |
| Content scheduling | Future | Time-based stage transitions |
| Config import for roles/stages | Done | ConfigStorage supports role and stage entity types |
| ritrovo_access WASM plugin | Part 5 | Stage-based access control and field-level visibility |

---

## Related

- [Part 1: Hello, Trovato](part-01-hello-trovato.md)
- [Part 2: The Ritrovo Importer Plugin](part-02-ritrovo-importer.md)
- [Part 3: Look & Feel](part-03-look-and-feel.md)
- [Content Model Design](../design/Design-Content-Model.md)
- [Security Audit](../security-audit.md)
