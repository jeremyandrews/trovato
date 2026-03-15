# Epic 6: The Editorial Engine

**Tutorial Part:** 4
**Trovato Phase Dependency:** Phase 3 (Users, Sessions), Phase 5 (Stages, Permissions, Revisions)
**BMAD Epic:** 35
**Status:** Complete (tutorial written, all features implemented)

---

## Narrative

*A conference aggregator with 5,000 entries and no gatekeepers is a liability. Part 4 gives Ritrovo editorial discipline. You create test users, assign roles with granular permissions, configure a three-stage editorial pipeline (Incoming > Curated > Live), build revision history so no edit is ever lost, and set up admin content management with filters and bulk operations.*

The reader sees every layer of access control: Argon2id password hashing, Redis-backed sessions with fixation protection, role-based permissions checked through a layered model (admin bypass > published view > plugin hook > role-based fallback), stage visibility enforced transparently in Gather SQL, and an append-only revision log with revert. They create three test users -- an editor, a publisher, and a basic viewer -- and walk through the editorial lifecycle from import to public visibility.

By the end, the single-admin demo becomes a multi-user editorial CMS with enforced workflows, tracked revisions, and permission-gated content.

---

## Tutorial Steps

### Step 1: Users & Authentication

Explore the full user model, session architecture, and authentication flow. Enable registration, create three test users for the editorial workflow, and verify login/logout.

**What to cover:**

- User model: UUID, name, email, Argon2id password hash, `is_admin` flag, status, roles via `user_roles` join table, JSONB data
- Session architecture: Redis-backed, HttpOnly/Secure cookies, session ID cycling on auth state changes, `SESSION_USER_ID`
- Password requirements: minimum 12 characters, Argon2id with RFC 9106 params (m=65536, t=3, p=4)
- Registration: disabled by default, enabled via `variable.allow_user_registration.yml`, creates inactive users (status=0), rate limit 3/hour/IP
- Creating test users: `editor_alice`, `publisher_bob`, `viewer_carol` via registration form, then activate via SQL
- Login/logout: POST with CSRF protection, logout is never GET
- User profile page at `/user/profile`

### Step 2: Roles & Permissions

Configure five roles with the permission model that controls who can do what.

**What to cover:**

- Three implicit roles: anonymous, authenticated, administrator (`is_admin` bypass)
- Two custom roles: editor and publisher with distinct permission sets
- Permission model: strings like `"access content"`, `"edit any content"`, declared by plugins via `tap_perm`
- Layered access check for items: (1) admin bypass, (2) published view for anyone with `"access content"`, (3) plugin hook `tap_item_access` with Grant/Deny/Neutral aggregation, (4) role-based fallback checking generic + type-specific + own-vs-any patterns
- Role assignment via SQL (`user_roles` join table) -- no admin UI for user-role assignment
- Admin pages (`/admin/*`) require `is_admin` flag, not role permissions -- editors and publishers use `/item/{id}/edit` directly
- Config import does not support `role` or `role_permission` entity types -- configure via admin UI at `/admin/people/roles` and `/admin/people/permissions`

### Step 3: Stages & the Editorial Workflow

Configure three editorial stages and walk through the content lifecycle from import to publication.

**What to cover:**

- Three stages: Incoming (internal), Curated (internal), Live (public), each stored as a `category_tag` with `stage_config`
- Stage-aware Gathers: the kernel wraps queries with a stage visibility CTE, filtering by user permissions
- Workflow transitions: configurable `variable.workflow.editorial.yml` defining valid stage changes with required permissions
- The editorial lifecycle: importer creates on Incoming > editor promotes to Curated > publisher promotes to Live > public sees it
- Stage-aware search: anonymous sees Live only, editors see their accessible stages
- Extensibility demo: adding a "Legal Review" stage via SQL + config import without code changes
- Note: stage creation not yet supported by `config import` -- stages created via SQL or admin UI

### Step 4: Revision History

Tour the revision system: append-only history, revert capability, and the five key revision scenarios.

**What to cover:**

- Revision model: every edit creates an `item_revision` row with timestamp, author, log message, and full field snapshot
- `current_revision_id` pointer on the `item` table
- Revision history page at `/item/{id}/revisions` (requires authentication)
- Revert: creates a new revision containing the old data -- never deletes history
- Five scenarios: (1) basic revision, (2) revert bad edit, (3) draft-while-live, (4) cross-stage field updates, (5) emergency unpublish
- Note: plugin-imported items don't get revision rows unless edited via the kernel API
- Revision diff UI deferred to a later part

### Step 5: Admin Content Management

Build the admin content list with filters, bulk operations, and quick actions for efficient editorial work at scale.

**What to cover:**

- Content list at `/admin/content` with filtering by content type and publishing status
- Content table: checkbox, title (linked to edit), type, author, status, updated timestamp, operations (Edit, Revisions, Delete)
- Bulk operations: publish, unpublish, delete (with confirmation)
- Bulk operation security: authentication required, CSRF protection, action validation against allowlist, per-item error handling
- Quick actions: Edit, Revisions, Delete links per row
- Flash messages for operation results
- Admin access gated on `is_admin` flag

---

## BMAD Stories

### Story 35.1: User Registration, Login & Session Management

**Status:** Complete

**As a** site administrator,
**I want** multi-user authentication with session security,
**So that** editorial users can log in and perform role-appropriate actions.

**Acceptance criteria:**

- User registration at `/user/register` with configurable enable/disable
- Registration creates inactive users (status=0) pending activation
- Rate limiting on registration: 3 per hour per IP
- Login at `POST /user/login` with Argon2id password verification
- Session stored in Redis with HttpOnly, Secure, SameSite=Lax cookies
- Session ID cycled on every authentication state change (login, logout)
- Logout via `POST /user/logout` with CSRF protection (never GET)
- Password minimum 12 characters, Argon2id with RFC 9106 params
- Login errors reveal nothing about username existence ("Invalid username or password")
- User profile page at `/user/profile` (authenticated only)
- Three test users created and activated: `editor_alice`, `publisher_bob`, `viewer_carol`

### Story 35.2: Role-Based Permission System

**Status:** Complete

**As a** site administrator,
**I want** granular role-based access control,
**So that** editors, publishers, and viewers have appropriate access levels.

**Acceptance criteria:**

- Three implicit roles exist: anonymous, authenticated, administrator
- Custom roles `editor` and `publisher` configurable via admin UI at `/admin/people/roles`
- Permissions assignable to roles at `/admin/people/permissions`
- Editor permissions include: `access content`, `create content`, `edit own content`, `edit any content`, `access files`, `use filtered_html`
- Publisher permissions extend editor with: `delete any content`, `administer files`, `use full_html`
- Layered item access check: admin bypass > published view > plugin hook (`tap_item_access` Grant/Deny/Neutral) > role-based fallback
- Role-based fallback checks five patterns: `"{op} any content"`, `"{op} any {type}"`, `"{op} {type} content"`, `"{op} own content"` (author match), `"{op} own {type}"` (author match)
- Admin pages require `is_admin` flag -- not accessible via role permissions
- Role assignment via SQL (`user_roles` join table)

### Story 35.3: Three-Stage Editorial Workflow

**Status:** Complete

**As a** content editor,
**I want** a three-stage workflow (Incoming > Curated > Live),
**So that** imported content is reviewed before publication.

**Acceptance criteria:**

- Three stages configured: Incoming (internal), Curated (internal), Live (public)
- Each stage stored as a `category_tag` row with `stage_config` (visibility, machine_name)
- Stage-aware Gathers filter items by user's accessible stages via CTE
- Workflow transitions configured via `variable.workflow.editorial.yml`
- Valid transitions: incoming>curated (edit perm), curated>live (admin perm), live>curated (admin perm), curated>incoming (edit perm)
- Invalid transitions rejected by the kernel
- Anonymous users see only Live (public) items in Gathers and search
- Editors see Incoming + Curated + Live
- Publishers see all stages
- Stage-aware search: search results filtered by accessible stages
- Extensibility: new stage addable via SQL + config import without code changes

### Story 35.4: Revision History with Revert

**Status:** Complete

**As a** content editor,
**I want** a complete revision history for every item,
**So that** I can see who changed what and revert bad edits.

**Acceptance criteria:**

- Every item edit creates a new `item_revision` row (timestamp, author, log message, full fields snapshot)
- `item.current_revision_id` updated to point to the latest revision
- Revision history page at `/item/{id}/revisions` (requires authentication)
- History table shows: date, title at revision, log message, "current" badge, Revert button
- Revert creates a new revision containing the selected revision's data -- never deletes history
- Revert creates an audit trail: original > bad edit > revert (containing original data)
- Plugin-imported items created via host functions do not have revisions unless subsequently edited through the kernel API
- Five revision scenarios documented and supported: basic revision, revert, draft-while-live, cross-stage field updates, emergency unpublish

### Story 35.5: Admin Content List with Bulk Operations

**Status:** Complete

**As a** site administrator,
**I want** efficient content management with filters and bulk actions,
**So that** I can manage thousands of items without editing them one by one.

**Acceptance criteria:**

- Content list at `/admin/content` (requires `is_admin`)
- Filterable by content type and publishing status
- Content table columns: checkbox, title (linked), type, author, status badge, updated timestamp, operations
- Bulk operations: publish, unpublish, delete (with JavaScript confirmation)
- Bulk action form includes CSRF token; action validated against allowlist
- Per-item error handling: individual failures logged without aborting batch
- Results reported via flash message: "N item(s) published." or partial failure counts
- Quick actions per row: Edit, Revisions, Delete
- Delete action uses CSRF-protected POST

---

## Payoff

A full editorial CMS. The reader understands:

- How Trovato's session system prevents fixation attacks and enforces CSRF protection
- How the layered permission model aggregates admin bypass, published-view shortcut, plugin Grant/Deny/Neutral hooks, and role-based fallback checks
- How stages separate editorial workflows from public content delivery transparently in Gather SQL
- How revisions create an append-only audit trail where no edit is ever lost
- How the admin content list enables efficient content management at scale

Ritrovo is now a multi-user editorial CMS: conferences flow in from the importer, editors review and curate them, publishers promote them to Live, and visitors browse a polished, searchable directory. Every edit is tracked, every action is authorized, and the workflow is enforced by the kernel.

---

## What's Deferred

These are explicitly **not** in Part 4 (and the tutorial should say so):

- **WYSIWYG editor** -- Part 5 (rich text editing is a form enhancement)
- **AJAX form interactions** -- Part 5 (progressive enhancement)
- **ritrovo_access plugin** -- Part 5 (field-level access control via render tree manipulation)
- **Comments** -- Part 6 (depends on full user system being stable)
- **User notifications** -- Part 6 (depends on comments and subscriptions)
- **Internationalization** -- Part 7 (separate concern)
- **REST API authentication (tokens)** -- Part 7 (API auth is separate from session auth)
- **Revision diff UI** -- Future (visual diff display is a UI enhancement)
- **Content scheduling** -- Future (time-based stage transitions)
- **Config import for roles/stages** -- Future (ConfigStorage does not yet support these entity types)

---

## Related

- [Part 4: The Editorial Engine](../tutorial/part-04-editorial-engine.md)
- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Epic 5: Look & Feel](epic-05.md) -- Part 3 presentation layer
- [Security Audit](../security-audit.md)
- [Content Model Design](../design/Design-Content-Model.md)
