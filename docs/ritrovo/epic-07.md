# Epic 7: Forms & User Input

**Tutorial Part:** 5
**Trovato Phase Dependency:** Phase 4 (Form API, Multi-Step Forms, AJAX), Phase 5 (Field-Level Access)
**BMAD Epic:** 36
**Status:** Complete (all features implemented)

---

## Narrative

*Until now, content creation has been a backend affair -- the admin types into auto-generated forms, the importer pushes data through host functions. Part 5 opens the front door. Registered users can submit conferences through a guided multi-step form. Editors get WYSIWYG rich text editing, AJAX-powered "add another" for speakers, and topic autocomplete. And the `ritrovo_cfp` plugin demonstrates computed fields and cross-plugin event triggers, while `ritrovo_access` shows field-level access control through render tree manipulation.*

The reader builds two plugins and the full form experience. They learn the Form API pipeline (Build > Validate > Submit), implement multi-step forms with PostgreSQL-backed state persistence, wire up AJAX interactions for conditional fields and multi-value references, integrate a WYSIWYG editor for rich text fields, and see how plugins alter forms and inject computed render elements.

Two new plugins debut in this part: `ritrovo_cfp` (CFP badge injection via `tap_item_view`, date validation via `tap_item_insert`/`tap_item_update`, and cross-plugin notification queueing) and `ritrovo_access` (field-level visibility via `tap_item_view` render tree manipulation, stage-based Grant/Deny via `tap_item_access`).

---

## Tutorial Steps

### Step 1: The Form API

Tour the Form API pipeline that powers content editing. Understand how form definitions become render elements, how validation works, and how submission is processed.

**What to cover:**

- Form API pipeline: Build (definition to RenderElements), Validate (server-side field rules), Submit (process and save)
- How the conference edit form is auto-generated from the Item Type definition
- Form rendering through Tera templates (form elements, field widgets, validation error display)
- `tap_form_alter` -- how plugins can modify forms (add/remove/reorder fields, inject buttons)
- CSRF protection via `require_csrf` on every form submission
- Form state caching in `form_state_cache` for multi-step persistence

### Step 2: WYSIWYG Rich Text Editing

Upgrade plain text description fields to rich text editing with `filtered_html` format.

**What to cover:**

- Text format types: `plain_text` (escaped), `filtered_html` (ammonia-sanitized rich text), `full_html` (trusted editors only)
- WYSIWYG editor integration for `filtered_html` fields
- HTML sanitization pipeline: ammonia strips disallowed tags, attributes, and `javascript:` URIs
- `use filtered_html` and `use full_html` permissions controlling format access
- How the render pipeline stores the format alongside the value and applies the correct filter on display

### Step 3: AJAX Form Interactions

Add progressive enhancement to content forms: conditional CFP fields, "Add another speaker" for multi-value references, and topic autocomplete.

**What to cover:**

- Conditional fields: CFP URL and CFP end date appear only when the user starts entering CFP information (AJAX callback)
- "Add another speaker": multi-value RecordReference field with AJAX-powered add/remove (partial form rebuild without full page reload)
- Topic autocomplete: category reference field with type-ahead search against the topic category
- AJAX callback routing: how the kernel handles partial form rebuilds
- Form state maintained during AJAX round-trips via `form_state_cache`
- Progressive enhancement: forms work without JavaScript (full page reload fallback)

### Step 4: Multi-Step Conference Submission

Build a user-facing form for registered users to submit conferences -- three steps with PostgreSQL-backed state persistence.

**What to cover:**

- Multi-step form definition: Step 1 (basics: name, URL, dates, location), Step 2 (details: CFP, topics, description, logo upload), Step 3 (review & submit)
- Form state serialization: PostgreSQL `form_state_cache` table stores serialized form data between steps
- Step transition mechanics: each step validates before advancing, back navigation preserves entered data
- File uploads tracked across steps: temporary files referenced in form state, promoted to permanent on final submit
- CSRF token validated on each step
- Final submission creates an Item on the Incoming stage (requires authentication)
- Confirmation page with link to the submitted conference
- `"create content"` permission required; anonymous users redirect to login

### Step 5: The `ritrovo_cfp` Plugin

Build the CFP tracking plugin: computed display fields, date validation, and cross-plugin event triggers.

**What to cover:**

- `tap_item_view` -- Computes "days until CFP closes" and injects a color-coded RenderElement badge (green >14 days, yellow 7-14, red <7)
- `tap_item_insert` / `tap_item_update` -- Validates `cfp_end_date` is not after `end_date`; when a CFP enters the 7-day window, writes a `cfp_closing_soon` event to the `ritrovo_notifications` queue
- Cross-plugin communication: `ritrovo_cfp` produces events for `ritrovo_notify` to consume (Part 6)
- SDK features demonstrated: `tap_item_view` render element injection, field validation, queue writing via host functions, structured logging

### Step 6: The `ritrovo_access` Plugin

Build the access control plugin: stage-based Grant/Deny and field-level visibility through render tree manipulation.

**What to cover:**

- `tap_item_access` -- Returns Grant/Deny/Neutral based on role, stage, and item type
  - Anonymous denied access to Incoming/Curated items
  - Editors granted Incoming + Curated
  - Publishers granted all stages
- `tap_perm` -- Declares permissions: `view incoming`, `view curated`, `edit conferences`, `publish conferences`, `post comments`, `edit own comments`, `edit any comments`
- `tap_item_view` -- Strips `editor_notes` field from the render tree for non-editor roles (field-level access via render tree manipulation, not database filtering)
- How Grant/Deny/Neutral aggregation works: any Deny wins, then any Grant, else Neutral falls through to role-based checks
- SDK features demonstrated: `tap_item_access` with structured access decisions, `tap_perm` for permission declaration, `tap_item_view` for render tree alteration

### Step 7: User Profile Form

Build the profile editing experience: display name, bio with WYSIWYG, timezone selection, and notification preferences.

**What to cover:**

- Profile form at `/user/profile` (editing own data)
- Display name and email fields
- Bio field with WYSIWYG (filtered_html)
- Timezone select
- Form renders through the same Form API pipeline as content forms
- User-context form: editing own profile, not arbitrary content

---

## BMAD Stories

### Story 36.1: Form API Pipeline & Conference Edit Form

**Status:** Complete (all features implemented)

**As a** content editor,
**I want** a form system that auto-generates content editing forms from Item Type definitions,
**So that** I can create and edit conferences through a web interface.

**Acceptance criteria:**

- Form API implements Build > Validate > Submit pipeline
- Conference edit form auto-generated from Item Type definition with all field widgets
- Field validation rules enforced server-side (required fields, type checking)
- Validation errors displayed inline next to the relevant field
- CSRF protection via `require_csrf` on every form submission
- `tap_form_alter` dispatched during Build phase, allowing plugins to modify the form
- Form submission creates/updates Items via the kernel API (triggering taps and revisions)

### Story 36.2: WYSIWYG Editor for Rich Text Fields

**Status:** Complete (all features implemented)

**As a** content editor,
**I want** a WYSIWYG editor for conference descriptions,
**So that** I can format text with bold, italic, links, and lists without writing HTML.

**Acceptance criteria:**

- WYSIWYG editor renders for `filtered_html` format fields
- HTML sanitization via ammonia strips disallowed tags, attributes, and `javascript:` URIs
- `plain_text` fields remain as plain text inputs (no WYSIWYG)
- `full_html` available only to users with `use full_html` permission
- `use filtered_html` permission required to see the WYSIWYG toolbar (otherwise falls back to plain textarea)
- Sanitized output stored in the database; displayed via `| safe` with `{# SAFE: #}` comment

### Story 36.3: AJAX Form Enhancements

**Status:** Complete (all features implemented)

**As a** content editor,
**I want** conditional fields, multi-value add/remove, and autocomplete in content forms,
**So that** form interaction is efficient and context-sensitive.

**Acceptance criteria:**

- Conditional CFP fields: `cfp_url` and `cfp_end_date` fields appear dynamically when relevant (AJAX callback)
- "Add another speaker": multi-value RecordReference with AJAX add/remove (no full page reload)
- Topic autocomplete: type-ahead search against the topic category hierarchy
- AJAX callbacks route through the Form API and return partial HTML for the affected form region
- Form state maintained in `form_state_cache` during AJAX round-trips
- Progressive enhancement: forms functional without JavaScript via full page reload fallback

### Story 36.4: Multi-Step Conference Submission Form

**Status:** Complete (all features implemented)

**As a** registered user,
**I want to** submit a conference through a guided multi-step form,
**So that** I can contribute conference listings to the directory.

**Acceptance criteria:**

- Three-step form: Step 1 (name, URL, dates, city, country, online, language), Step 2 (CFP details, topics, description, logo upload, venue photos), Step 3 (read-only review with thumbnails, "Edit" links to previous steps, Submit button)
- Form state serialized to PostgreSQL `form_state_cache` table between steps
- Each step validates before allowing advancement; back navigation preserves data
- File uploads tracked in form state across steps; promoted to permanent on final submit
- CSRF token validated on each step
- Final submission creates Item on the Incoming stage
- Confirmation page shown with link to the submitted conference
- `create content` permission required; anonymous users redirected to login
- Form state has a TTL; stale entries cleaned by cron

### Story 36.5: `ritrovo_cfp` Plugin -- CFP Tracking & Cross-Plugin Events

**Status:** Complete (all features implemented)

**As a** site visitor,
**I want** visual indicators of CFP deadlines on conference pages,
**So that** I can see at a glance which conferences are accepting talk proposals and how much time remains.

**Acceptance criteria:**

- WASM plugin `ritrovo_cfp` compiled and installable
- `tap_item_view`: computes days until CFP closes, injects RenderElement badge (green >14d, yellow 7-14d, red <7d)
- `tap_item_insert` / `tap_item_update`: validates `cfp_end_date` not after `end_date`; queues `cfp_closing_soon` event when CFP enters 7-day window
- Event written to the `ritrovo_notifications` queue for `ritrovo_notify` to process (Part 6)
- Plugin uses SDK host functions: `item_load()`, `queue_push()`, structured logging
- CFP badge renders only when `cfp_end_date` is present and in the future
- Validation error returned on save if `cfp_end_date > end_date`

### Story 36.6: `ritrovo_access` Plugin -- Field-Level Access & Stage Gating

**Status:** Complete (all features implemented)

**As a** site administrator,
**I want** fine-grained access control enforced by a plugin,
**So that** anonymous users cannot see editorial stages and `editor_notes` is hidden from non-editors.

**Acceptance criteria:**

- WASM plugin `ritrovo_access` compiled and installable
- `tap_item_access`: returns Deny for anonymous access to Incoming/Curated items; Grant for editors on Incoming/Curated; Grant for publishers on all stages
- `tap_perm`: declares permissions `view incoming`, `view curated`, `edit conferences`, `publish conferences`, `post comments`, `edit own comments`, `edit any comments`
- `tap_item_view`: strips `editor_notes` field from the render tree when the requesting user lacks editor role
- Grant/Deny/Neutral aggregation works correctly with the kernel's layered access model
- Field-level access is render-tree manipulation (the field is removed before template rendering), not database filtering
- Plugin uses SDK host functions for user context inspection

### Story 36.7: User Profile Form

**Status:** Complete (all features implemented)

**As a** registered user,
**I want to** edit my profile information,
**So that** my display name, bio, and preferences are up to date.

**Acceptance criteria:**

- Profile form at `/user/profile` (authenticated only, editing own data)
- Fields: display name, email, bio (WYSIWYG with `filtered_html`), timezone select
- Form renders through the Form API pipeline
- Validation: email format, display name not empty
- Changes saved to the user record
- Unauthenticated requests redirect to `/user/login`
- Password change form available on the same page (current password required)

---

## Payoff

The front door is open. The reader understands:

- How the Form API pipeline (Build > Validate > Submit) generates forms from type definitions
- How `tap_form_alter` lets plugins modify forms without touching kernel code
- How multi-step forms persist state across requests via PostgreSQL
- How AJAX interactions provide progressive enhancement without breaking no-JavaScript fallback
- How text format permissions (`filtered_html`, `full_html`) control what HTML users can produce
- How `tap_item_view` lets plugins inject computed elements (CFP badges) and strip fields (editor_notes) from the render tree
- How `tap_item_access` with Grant/Deny/Neutral aggregation enables plugin-driven access control
- How cross-plugin communication works via shared queues (ritrovo_cfp writes events for ritrovo_notify)

Two plugins, two new taps in action (`tap_form_alter`, `tap_item_access`), and a complete form system. Part 6 adds community features.

---

## What's Deferred

These are explicitly **not** in Part 5 (and the tutorial should say so):

- **Comments** -- Part 6 (depends on the full user and permission system being stable)
- **Subscriptions** -- Part 6 (subscribe/unsubscribe to conferences)
- **Notification delivery** -- Part 6 (ritrovo_notify processes events queued by ritrovo_cfp)
- **Internationalization** -- Part 7 (separate concern)
- **REST API** -- Part 7 (API endpoints, authentication, rate limiting)
- **Translation workflow** -- Part 7 (ritrovo_translate plugin)
- **AI-powered form assistance** -- Epic 3 (AI Assist buttons in forms)
- **Batch operations** -- Part 8 (bulk publish/import at scale)
- **Avatar upload** -- Part 6+ (user profile image, depends on file field in user context)

---

## Related

- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Epic 5: Look & Feel](epic-05.md) -- Part 3 presentation layer
- [Epic 6: The Editorial Engine](epic-06.md) -- Part 4 users, roles, stages
- [Render Tree & Forms Design](../design/Design-Render-Theme.md)
- [Plugin SDK Design](../design/Design-Plugin-SDK.md)
