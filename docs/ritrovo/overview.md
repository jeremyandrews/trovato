# Ritrovo -- Trovato's Reference Application, Demo & Tutorial

**Status:** Design brief v2 (expanded scope, ready for BMAD)
**Name:** Ritrovo (Italian: "meeting place" -- etymologically related to Trovato, both from *trovare* = "to find." A ritrovo is where you find each other again.)
**Purpose:** Triple-role reference application: (1) a comprehensive tutorial showing how to build a real site with Trovato, (2) a turnkey demo you can install and explore immediately, and (3) a regression test suite that validates every core Trovato feature through real-world usage.
**Data source:** [confs.tech/conference-data](https://github.com/tech-conferences/conference-data) -- open-source, crowd-maintained JSON of tech conferences across 30+ categories.

---

## Why Ritrovo

Drupal has Umami (a recipe site with real content). Trovato needs something equivalent -- but more ambitious. Ritrovo is a non-trivial application that proves the CMS works end-to-end, showcases every distinctive feature, gives new users a codebase to study, and serves as an ongoing regression test.

Conferences beat recipes for Trovato's audience because:

1. **Real, auto-refreshing data.** The confs.tech GitHub repo is crowd-sourced and actively maintained. A cron plugin imports new conferences automatically -- the demo never goes stale.
2. **Naturally maps to Trovato's strengths.** Stages, Gather queries, JSONB fields, user subscriptions, file uploads, multilingual content, REST API, and plugin taps all have obvious roles.
3. **Relevant audience.** Trovato targets Rust/web developers -- people who attend and care about tech conferences.
4. **Complexity ceiling is high.** A conference site has users, permissions, editorial workflows, media, search, API consumers, multilingual content, and notifications -- enough to exercise every Trovato subsystem.

---

## What It Demonstrates

This is the master checklist. Every Trovato feature should appear here.

| Trovato Feature | How Ritrovo Uses It |
|---|---|
| **Item Types & CCK** | `conference` and `speaker` item types with JSONB fields, RecordReference relations, File fields |
| **Categories** | Three-level hierarchical topic category: domain > area > specific (e.g., Languages > Systems > Rust). Demonstrates nested category queries, breadcrumb generation, hierarchical browsing |
| **Stages & Revisions** | Three-stage workflow (Incoming > Curated > Live) with full revision history. Tutorial explicitly demos creating a draft revision, previewing, reverting to a previous revision, and staging a new version of the site |
| **Gather** | Six Gather definitions: Upcoming, Open CFPs (Call for Papers -- the submission process conferences use to solicit talk proposals), By Topic, By Location, Conferences This Month (Tile), CFPs Closing Soon (Tile). Exposed filters, contextual filters, relationships, pagination |
| **Render Tree** | Conference card, detail page, speaker card, CFP badge, topic pills, user profile -- all structured JSON RenderElements rendered by Tera templates |
| **Form API** | Conference edit form (standard), conference submission form (multi-step: basic info > details > review & submit), user profile form, subscription management form |
| **WYSIWYG** | Rich text editing for conference descriptions and user bios. AJAX "add another" for multi-value speaker references |
| **Plugins & Taps** | Five plugins demonstrating the full tap lifecycle (see Plugins section) |
| **Plugin-to-Plugin** | `ritrovo_cfp` writes CFP-closing-soon events to the `ritrovo_notifications` queue; `ritrovo_notify` processes them via `tap_queue_worker`. Demonstrates plugin-to-plugin communication through shared queue infrastructure |
| **Cron** | `ritrovo_importer` daily import, `ritrovo_notify` digest emails, temp file cleanup |
| **Queue** | Import validation queue: new conference sources queued, validated, then processed. Demonstrates `tap_queue_info` and `tap_queue_worker` |
| **Tiles** | Sidebar tiles: CFPs Closing Soon, Conferences This Month, Topic Cloud, Recent Comments, My Subscriptions (authenticated) |
| **Slots** | Configurable page layout: Header, Navigation, Content, Sidebar, Footer slots. Admin UI for assigning Tiles to Slots. Demonstrates the full site-building experience |
| **Search** | Full-text search across conference names, descriptions, cities, topics, speaker names. Search field weighting (title = A, description = B, city/topics = C). Stage-aware indexing (Live only for anonymous) |
| **Users & Auth** | Registration, login/logout, user profiles with bio and avatar, role assignment. Users are Records (demonstrates "everything is an Item" philosophy) |
| **Permissions** | Five roles (anonymous, authenticated, editor, publisher, admin) with granular `tap_item_access` grant/deny. Field-level visibility (editors see CFP notes, anonymous don't). Stage-scoped access |
| **Files** | Conference logo (image, required), venue photo gallery (multi-value images), speaker headshot, schedule PDF upload. Local + S3 storage backends. Private vs. public file access |
| **Menus** | Main nav, User menu, Admin menu, Footer menu. Dynamic menu items from plugins via `tap_menu`. Hierarchical menu with breadcrumb generation |
| **REST API** | JSON endpoints: `/api/v1/conferences`, `/api/v1/conferences/{id}`, `/api/v1/topics`, `/api/v1/search?q=`. Authentication via API key. Rate limiting via Tower middleware |
| **Revisions** | Every conference edit creates a revision. Tutorial walks through: edit, preview draft, revert to revision N, compare revisions. Revision log with author and timestamp |
| **i18n** | Bilingual site: English (default) + Italian. Non-English conferences imported and translated (via LLM plugin or manual). Language switcher. Translated UI strings, translated content, translated URL aliases |
| **Multi-step Forms** | "Submit a Conference" for registered users: Step 1 (name, dates, location), Step 2 (CFP details, topics, description, logo upload), Step 3 (review & confirm). Form state persisted in PostgreSQL across steps |
| **AJAX** | Conditional CFP fields, "Add another speaker" multi-value, topic autocomplete, inline subscription toggle, exposed filter form submission without full page reload |
| **Caching** | Tag-based cache invalidation on conference update. Stage-scoped cache keys. L1 (moka) + L2 (Redis) demonstrated via Gander profiling showing cache hits vs. misses |
| **Batch** | Bulk publish from Curated to Live. Bulk re-import. Progress tracking via Redis polling |

---

## Content Model

### Item Type: `conference`

**Core JSONB fields:**

| Field | Type | Source | Notes |
|---|---|---|---|
| `name` | `TextValue` (plain) | confs.tech `name` | Title field, required |
| `url` | `TextValue` (plain) | confs.tech `url` | Conference website |
| `start_date` | Date | confs.tech `startDate` | Required |
| `end_date` | Date | confs.tech `endDate` | Required |
| `city` | `TextValue` (plain) | confs.tech `city` | Nullable for online-only |
| `country` | `TextValue` (plain) | confs.tech `country` | Nullable for online-only |
| `online` | Boolean | confs.tech `online` | Default false |
| `cfp_url` | `TextValue` (plain) | confs.tech `cfpUrl` | Nullable |
| `cfp_end_date` | Date | confs.tech `cfpEndDate` | Nullable |
| `description` | `TextValue` (filtered_html) | Manual / enriched | WYSIWYG-edited rich text |
| `topics` | Category reference (multi) | Mapped from confs.tech topic | Links to topic category hierarchy |
| `logo` | File (image) | Manual upload | Conference logo, required for Live stage |
| `venue_photos` | File (image, multi) | Manual upload | Venue/event photos, optional |
| `schedule_pdf` | File (pdf) | Manual upload | Conference schedule, optional |
| `speakers` | RecordReference (multi) | Manual / linked | References to `speaker` Items |
| `language` | `TextValue` (plain) | Detected / manual | Primary language of conference (ISO 639-1) |
| `source_id` | `TextValue` (plain) | Computed | Dedup key: slugified `name + start_date + city` |
| `editor_notes` | `TextValue` (plain) | Manual | Internal notes, visible only to editors (field-level access) |

### Item Type: `speaker`

Demonstrates a second content type with RecordReference relationships.

| Field | Type | Notes |
|---|---|---|
| `name` | `TextValue` (plain) | Title field, required |
| `bio` | `TextValue` (filtered_html) | WYSIWYG bio |
| `headshot` | File (image) | Speaker photo |
| `website` | `TextValue` (plain) | Personal site |
| `conferences` | Reverse reference | Computed: conferences referencing this speaker |

### Item Type: `comment`

User discussion on conferences. Demonstrates nested items and per-item access control.

| Field | Type | Notes |
|---|---|---|
| `body` | `TextValue` (filtered_html) | Comment text |
| `conference` | RecordReference | Parent conference |
| `parent` | RecordReference (self) | Nullable, for threaded replies |

### Categories: `topic`

Three-level hierarchy demonstrating nested categories and recursive CTE queries:

```
Languages/
  Systems/
    Rust, Go, C, C++
  JVM/
    Java, Kotlin, Scala, Clojure
  Web/
    JavaScript, TypeScript, PHP, Ruby, Python
  Mobile/
    Swift, Kotlin (cross-listed)
  Functional/
    Elixir, Haskell, Erlang
Infrastructure/
  DevOps, Kubernetes, Cloud, Networking, Observability
AI & Data/
  AI, Machine Learning, Data Engineering, LLMs
Web Platform/
  CSS, UX, GraphQL, WebAssembly, Accessibility
Security/
  AppSec, Privacy, Cryptography
```

The third nesting level (Systems > Rust) demonstrates hierarchical category queries: "show me all Languages conferences" returns everything under Languages including all grandchildren. Breadcrumbs show the full path: Languages > Systems > Rust.

---

## Users, Roles & Permissions

### Roles

| Role | Capabilities |
|---|---|
| **Anonymous** | View Live conferences, search, browse topics, view speaker profiles, read comments, use REST API (read-only, rate-limited) |
| **Authenticated** | Everything anonymous can do, plus: subscribe to conferences, post comments, submit conferences (goes to Incoming), edit own profile/bio/avatar, manage subscriptions, use REST API with API key |
| **Editor** | Everything authenticated can do, plus: view Incoming + Curated stages, edit any conference, add/edit speakers, promote Incoming > Curated, see `editor_notes` field, manage import queue |
| **Publisher** | Everything editor can do, plus: promote Curated > Live, revert revisions, bulk publish, manage categories |
| **Admin** | Everything, plus: manage users/roles, configure Slots/Tiles, manage plugins, site settings, manage menus, view Gander profiling data |

### Access Control Demonstration

The `ritrovo_access` plugin implements `tap_item_access` with grant/deny/neutral aggregation:

- **Stage-based access:** Anonymous users denied access to Incoming/Curated items. Editors granted Incoming + Curated. Publishers granted all.
- **Field-level access:** `editor_notes` field hidden from non-editor roles via `tap_item_view` (the field is stripped from the render tree).
- **Comment moderation:** Authenticated users can edit/delete own comments. Editors can edit/delete any comment. Anonymous can only read.
- **Subscription privacy:** Users can only view their own subscriptions.

### User Profiles

Users are Records (Items), demonstrating the "everything is an Item" architecture. User profiles have:

- Display name
- Bio (WYSIWYG, `filtered_html`)
- Avatar (File upload, image)
- Timezone
- Notification preferences (email digest frequency)
- Public profile page at `/user/{username}`
- "My Subscriptions" page (private, authenticated only)

---

## Plugins

### `ritrovo_importer`

**Purpose:** Cron-driven import from confs.tech GitHub data, with queue-based validation.

**Taps implemented:**

- `tap_cron` -- Daily. Fetches confs.tech JSON, computes diffs, queues new/changed items for validation before creating/updating.
- `tap_queue_info` -- Declares the `ritrovo_import` queue.
- `tap_queue_worker` -- Processes queued imports: validates data integrity (required fields, date sanity, dedup check), then creates Items in Incoming stage or updates existing Items. Logs results. Bad data is logged and skipped, not silently dropped.
- `tap_plugin_install` -- First install: full historical import + category seeding.

**SDK features demonstrated:** Cron, Queue API, Item CRUD via host functions, category term creation, stage-aware creation, HTTP requests via `http_request()`, structured logging, error handling.

### `ritrovo_cfp`

**Purpose:** Computed field, display enhancement, and notification trigger for CFP tracking.

**Taps implemented:**

- `tap_item_view` -- Computes "days until CFP closes." Injects RenderElement badge: green/yellow/red.
- `tap_item_insert` / `tap_item_update` -- Validates `cfp_end_date` not after `end_date`. When a CFP enters the 7-day window, writes a `cfp_closing_soon` event to the `ritrovo_notifications` queue for `ritrovo_notify` to process.

### `ritrovo_notify`

**Purpose:** Subscription and notification system.

**Taps implemented:**

- `tap_menu` -- Registers `/user/{uid}/subscriptions` route.
- `tap_item_view` -- Injects "Subscribe/Unsubscribe" toggle button on conference detail pages (authenticated users only, via AJAX).
- `tap_item_update` -- When a subscribed conference changes (dates, venue, CFP), queues a notification.
- `tap_queue_info` -- Declares the `ritrovo_notifications` queue.
- `tap_queue_worker` -- Sends notification emails (or queues them for digest).
- `tap_cron` -- Sends daily digest emails to users with pending notifications.

**SDK features demonstrated:** Plugin-to-plugin communication via shared queues, user-context operations, AJAX endpoints from plugins, queue processing, cron, email dispatch.

### `ritrovo_translate`

**Purpose:** i18n support -- detects non-English conferences, provides translation workflow.

**Taps implemented:**

- `tap_item_insert` -- On new conference creation, detects language from text fields. If non-English, flags for translation.
- `tap_item_view` -- Adds language indicator badge. Shows "View in English / Vedi in italiano" switcher.
- `tap_cron` -- Processes translation queue. (For the demo, translations can be seeded statically; in production, this could call an LLM API.)
- `tap_form_alter` -- Adds language selector to conference edit form.

**SDK features demonstrated:** i18n integration, language detection, content translation workflow, `tap_form_alter`.

### `ritrovo_access`

**Purpose:** Granular access control.

**Taps implemented:**

- `tap_item_access` -- Returns Grant/Deny/Neutral based on role, stage, and item type. Demonstrates the aggregation model.
- `tap_perm` -- Declares permissions: `view incoming`, `view curated`, `edit conferences`, `publish conferences`, `administer site`, `post comments`, `edit own comments`, `edit any comments`.
- `tap_item_view` -- Strips `editor_notes` field from render tree for non-editor roles. (Field-level access via render tree manipulation.)

---

## Gather Definitions

### "Upcoming Conferences" (main listing)

```
GatherDefinition {
  base_item_type: "conference",
  fields: [name, start_date, end_date, city, country, topics, cfp_end_date, logo, language],
  relationships: [
    { field: "speakers", target_type: "speaker", join: Left }
  ],
  filters: [
    { field: "start_date", op: Gte, value: ":current_date" }
  ],
  exposed_filters: [
    { field: "topics", op: InCategory, label: "Topic" },
    { field: "country", op: Eq, label: "Country" },
    { field: "online", op: Eq, label: "Online only" },
    { field: "language", op: Eq, label: "Language" }
  ],
  sorts: [
    { field: "start_date", direction: Asc }
  ],
  pager: { items_per_page: 25 }
}
```

Stage-aware: CTE wraps query. Anonymous sees Live only; editors see Curated + Live.

### "Open CFPs"

```
GatherDefinition {
  base_item_type: "conference",
  fields: [name, cfp_url, cfp_end_date, start_date, city, topics, logo],
  filters: [
    { field: "cfp_end_date", op: Gte, value: ":current_date" },
    { field: "cfp_url", op: IsNotNull }
  ],
  sorts: [
    { field: "cfp_end_date", direction: Asc }
  ],
  pager: { items_per_page: 20 }
}
```

### "By Topic" (parameterized)

```
GatherDefinition {
  base_item_type: "conference",
  contextual_filters: [
    { field: "topics", source: UrlArgument(0) }
  ],
  filters: [
    { field: "start_date", op: Gte, value: ":current_date" }
  ],
  sorts: [{ field: "start_date", direction: Asc }],
  pager: { items_per_page: 25 }
}
```

Demonstrates hierarchical category queries: browsing "Languages" shows all conferences tagged with any descendant term.

### "By Location" (faceted)

```
GatherDefinition {
  base_item_type: "conference",
  contextual_filters: [
    { field: "country", source: UrlArgument(0) },
    { field: "city", source: UrlArgument(1), default: Ignore }
  ],
  filters: [
    { field: "start_date", op: Gte, value: ":current_date" }
  ],
  sorts: [{ field: "start_date", direction: Asc }],
  pager: { items_per_page: 25 }
}
```

### "Conferences This Month" (Tile)

```
GatherDefinition {
  base_item_type: "conference",
  display: Tile,
  filters: [
    { field: "start_date", op: Gte, value: ":first_of_month" },
    { field: "start_date", op: Lt, value: ":first_of_next_month" }
  ],
  sorts: [{ field: "start_date", direction: Asc }],
  pager: { items_per_page: 5, no_pager: true }
}
```

### "CFPs Closing Soon" (Tile)

```
GatherDefinition {
  base_item_type: "conference",
  display: Tile,
  filters: [
    { field: "cfp_end_date", op: Gte, value: ":current_date" },
    { field: "cfp_end_date", op: Lte, value: ":current_date_plus_14" },
    { field: "cfp_url", op: IsNotNull }
  ],
  sorts: [{ field: "cfp_end_date", direction: Asc }],
  pager: { items_per_page: 5, no_pager: true }
}
```

---

## Menus

### Main Navigation

Registered via core + `tap_menu` from plugins:

- Home (`/`)
- Conferences (`/conferences`) -- Upcoming Conferences Gather
- Topics (`/topics`) -- Hierarchical topic browser
- Open CFPs (`/cfps`) -- Open CFPs Gather
- About (`/about`) -- Static page Item

### User Menu

- **Anonymous:** Login (`/user/login`), Register (`/user/register`)
- **Authenticated:** My Profile (`/user/{uid}`), My Subscriptions (`/user/{uid}/subscriptions`), Logout (`/user/logout`)

### Admin Menu

Hierarchical, demonstrates multi-level menu with breadcrumbs:

- Content
  - Conferences (list/edit)
  - Speakers (list/edit)
  - Comments (moderation queue)
  - Import Queue (pending validations)
- Structure
  - Menus (manage menu items)
  - Slots (assign Tiles to layout regions)
  - Categories (manage topic hierarchy)
- Configuration
  - Plugins (enable/disable, settings)
  - Users & Roles
  - Site Settings (name, language, default stage)
  - Translation (pending translations queue)
- Reports
  - Gander (profiling/performance)
  - Search Statistics

### Footer Menu

- About, API Documentation, Data Sources (credits confs.tech), Privacy Policy

---

## Stages & Revisions

### Stages

| Stage | Purpose | Access |
|---|---|---|
| **Incoming** | Raw imports + user submissions. Unreviewed. | Editors+ |
| **Curated** | Editor-reviewed: description enriched, logo uploaded, topics verified. | Editors+ |
| **Live** | Published, visible to all. | Everyone |

### Revision Workflow (Tutorial Focus)

The tutorial explicitly walks through these scenarios, which are killer features:

1. **Basic revision:** Edit a Live conference's description. A new revision is created. The old revision is accessible in history.
2. **Revert:** A bad edit goes Live. Revert to the previous revision. The revert itself creates a new revision (audit trail preserved).
3. **Stage a new version:** Create a draft revision in Curated stage while the current Live version remains visible. Preview the draft. When ready, publish the Curated revision to Live (atomic swap).
4. **Cross-stage field updates:** Importer updates structured fields (dates, CFP info) on a Live item even while a Curated draft exists for that same item. The Curated draft's description is untouched.
5. **Revision comparison:** View diff between two revisions. See who changed what and when.

---

## Files & Media

### File Fields

| Field | Item Type | Type | Required | Access |
|---|---|---|---|---|
| `logo` | conference | Image (jpg/png/svg, max 2MB) | Required for Live | Public |
| `venue_photos` | conference | Image (multi, max 5MB each) | Optional | Public |
| `schedule_pdf` | conference | PDF (max 10MB) | Optional | Public |
| `headshot` | speaker | Image (jpg/png, max 2MB) | Optional | Public |
| `avatar` | user | Image (jpg/png, max 1MB) | Optional | Public |

### Upload Flow (demonstrated in tutorial)

1. File uploaded via multipart POST to `/file/upload`
2. Stored as temporary (status=0) with 6-hour TTL
3. On Item save, referenced files become permanent (status=1)
4. Orphaned temp files cleaned by cron
5. On Item delete, files removed only if no other Items reference them

### Storage Backends

Demo ships with `LocalFileStorage`. Tutorial includes a chapter on configuring `S3FileStorage` as a production alternative. Public files served directly via static file handler; private files (like `editor_notes` attachments, if we add them) route through Kernel for access control.

---

## REST API

### Endpoints

| Method | Path | Auth | Description |
|---|---|---|---|
| GET | `/api/v1/conferences` | Optional | List upcoming conferences. Supports `?topic=`, `?country=`, `?online=`, `?page=`, `?per_page=` |
| GET | `/api/v1/conferences/{id}` | Optional | Single conference with all fields, speakers, comments |
| GET | `/api/v1/topics` | None | Full topic hierarchy |
| GET | `/api/v1/topics/{id}/conferences` | Optional | Conferences for a topic (includes descendants) |
| GET | `/api/v1/search?q=` | Optional | Full-text search |
| GET | `/api/v1/speakers` | Optional | List speakers |
| GET | `/api/v1/speakers/{id}` | Optional | Single speaker with linked conferences |
| POST | `/api/v1/conferences` | Required (editor+) | Create conference |
| PATCH | `/api/v1/conferences/{id}` | Required (editor+) | Update conference |
| POST | `/api/v1/conferences/{id}/subscribe` | Required (auth) | Subscribe to conference |
| DELETE | `/api/v1/conferences/{id}/subscribe` | Required (auth) | Unsubscribe |

**Authentication:** API key in `Authorization: Bearer {key}` header. Keys managed in user profile.

**Rate limiting:** Tower middleware, configurable per-role. Anonymous: 60 req/min. Authenticated: 300 req/min. Demonstrated via Gander profiling.

**Stage awareness:** API returns Live content by default. `?stage=curated` available to editors+.

The API is "mostly for free" -- Gather definitions drive the query logic; the API layer is a thin JSON serializer on top of the same Gather engine that powers the HTML pages.

---

## i18n / Multilingual

### Architecture

Bilingual site: English (default) + Italian.

- **UI strings:** Translated via locale files (`.po` or JSON). Language switcher in header.
- **Content translation:** Each Item can have translations stored as parallel field sets in JSONB. The conference `name`, `description`, and `city` fields are translatable.
- **URL aliases:** `/conferences/rustconf-2026` (EN) and `/it/conferenze/rustconf-2026` (IT).

### Import & Translation Workflow

1. `ritrovo_importer` imports conferences. Most are English. Some (European conferences) have non-English names/descriptions.
2. `ritrovo_translate` detects language on insert.
3. Non-English conferences flagged for translation. For the demo, ~20 Italian conferences are seeded with hand-written English translations.
4. The translation queue in the admin UI shows pending items. Editors can translate manually via a side-by-side form.
5. Stretch goal: `ritrovo_translate` can call an LLM API (optional, disabled by default) for auto-translation of descriptions. This is a cool demo but not required for the core i18n proof.

### What This Proves

- Trovato handles multilingual content natively
- Language-aware routing and URL generation work
- Gather queries can filter by language
- The template system renders the correct translation based on user language preference

---

## Form API

### Conference Edit Form (standard)

All field widgets, WYSIWYG description, AJAX conditional CFP fields, file upload for logo/photos/PDF, multi-value speaker reference with "Add another speaker" AJAX button, topic autocomplete.

### "Submit a Conference" (multi-step, authenticated users)

Demonstrates Form API multi-step with PostgreSQL-backed form state:

**Step 1: Basics**
- Name (required)
- URL
- Start date, End date
- City, Country, Online checkbox
- Language selector

**Step 2: Details**
- CFP URL, CFP end date (conditional, AJAX)
- Topics (multi-select autocomplete)
- Description (WYSIWYG)
- Logo upload (required)
- Venue photos (optional, multi-value with "Add another" AJAX)

**Step 3: Review & Submit**
- Read-only summary of all entered data with thumbnail previews
- "Edit" links back to previous steps
- Submit button creates Item in Incoming stage
- Confirmation page with link to submitted conference

Form state cached in `form_state_cache` table between steps. CSRF token validated on each step.

### User Profile Form

- Display name, Bio (WYSIWYG), Avatar upload, Timezone select, Notification preferences (checkboxes)
- Demonstrates user-context form (editing own profile)

---

## Slots & Layout

### Default Slot Configuration

| Slot | Contents |
|---|---|
| **Header** | Site name/logo, Language switcher |
| **Navigation** | Main menu, User menu |
| **Content** | Page-specific content (Gather results, Item view, form, etc.) |
| **Sidebar** | Tiles: CFPs Closing Soon, Conferences This Month, Topic Cloud, Recent Comments, My Subscriptions (auth only) |
| **Footer** | Footer menu, credits, powered-by |

### Admin UI

Admin can:

- Assign Tiles to Slots via drag-and-drop (or simple form)
- Control Tile visibility per role (e.g., "My Subscriptions" only shows for authenticated)
- Control Tile visibility per page path (e.g., Topic Cloud only on `/conferences` and `/topics`)
- Reorder Tiles within a Slot

This is the site-building experience. It proves Trovato can be configured by a site builder, not just a developer.

---

## Search

Ritrovo uses a two-layer search architecture. See [Epic 2: Search That Thinks](epic-02.md) for the full design.

### Layer 1: Pagefind (user-facing default)

The `trovato_search` plugin provides client-side search powered by [Pagefind](https://pagefind.app/) -- a Rust/WASM static search library. Results appear in ~50ms with zero server involvement.

The plugin's `tap_cron` handler detects content changes and signals the kernel to rebuild the index. The kernel exports live-stage published items as HTML fragments, runs the Pagefind CLI, and atomically deploys the WASM index to `./static/pagefind/`. Only items on the live stage with public visibility are indexed (the human-in-the-middle guarantee). WASM plugins cannot spawn subprocesses or access the filesystem, so this split architecture is necessary.

### Layer 2: tsvector (server-side)

PostgreSQL `tsvector` handles server-side search for Gather queries, API consumers, admin search, and cron jobs. Also serves as the no-JavaScript fallback for user-facing search (progressive enhancement).

| Content Type | Field | Weight |
|---|---|---|
| conference | name | A |
| conference | description | B |
| conference | city | C |
| conference | topics (denormalized) | C |
| speaker | name | A |
| speaker | bio | B |

### Behavior

- Stage-aware: anonymous users see only live-stage results; editors see their active stage + live
- Pagefind search runs entirely client-side (~50ms, no server load)
- tsvector search provides server-side relevance ranking (`ts_rank`) with highlighted snippets
- Exposed search box in header (all pages)
- Empty state handled gracefully

---

## Caching

### What Gets Cached

- **Gather results:** Tag-based. Cache key includes query params + stage. Invalidated when any conference in the result set is updated.
- **Item views:** Per-item, per-stage, per-role cache. Invalidated on edit.
- **Menu:** Cached until menu items change or plugin enable/disable.
- **Category hierarchy:** Cached until terms added/moved/deleted.

### Tutorial Demonstration

The tutorial includes a Gander profiling chapter showing:

1. First page load: all cache misses, full query execution
2. Second load: L1 (moka) cache hits, sub-millisecond
3. Edit a conference: tag-based invalidation clears relevant entries
4. Third load: L1 miss, L2 (Redis) hit
5. Cache clear: back to full misses

This is how you teach developers to think about caching.

---

## Documentation: Two Tracks, One Source

The tutorial tells two stories from the same codebase. See [Documentation Architecture](documentation-architecture.md) for the full specification.

**Track 1: The User Story** -- the tutorial as designed in Epics 1-8. How to build a real site with Trovato. Answers "what do I do?" A developer follows along, types commands, sees results. Never needs to understand internals to succeed.

**Track 2: Under the Hood** -- optional companion sections within each tutorial step. How Trovato works internally. Answers "how does this actually work?" Covers generated SQL, WASM boundaries, cache internals, render tree construction. For readers who want to contribute to core or understand architecture.

Under the Hood sections appear as collapsible `<details>` blocks at the end of each tutorial step. Closed by default. Skipping them loses nothing from the user story.

**Both tracks are tested.** Code blocks tagged `trovato-test` (user story) and `trovato-test:internal` (internals) are extracted from the tutorial markdown and run as integration tests via `cargo test --test tutorial`. If a Trovato core change breaks what the tutorial promises, CI fails. The fix requires updating both code and docs before the PR merges.

Tutorial chapters live in `docs/tutorial/` in the Trovato repo, rendered via mdbook.

---

## Tutorial & Epics

Eight parts, ordered as a narrative arc. Each part builds on the previous one, and every part ends with something visible and satisfying -- a working feature, not just plumbing. Epic files contain full BMAD stories and detailed tutorial steps.

1. **Part 1: Hello, Trovato** -- Install, define conference type, create content, first Gather. *(Trovato Phase 3)*
2. **Part 2: Real Data, Real Site** -- Importer plugin, categories, advanced Gathers, search. *(Trovato Phases 2-3)*
3. **Part 3: Look & Feel** -- Templates, files, speakers, Slots/Tiles, menus. *(Trovato Phases 3, 5)*
4. **Part 4: The Editorial Engine** -- Users, roles, permissions, stages, revisions. *(Trovato Phases 3, 5)*
5. **Part 5: Forms & User Input** -- Form API, multi-step, AJAX, CFP tracker plugin. *(Trovato Phase 4)*
6. **Part 6: Community** -- Comments, subscriptions, notifications, plugin-to-plugin. *(Trovato Phases 4-5)*
7. **Part 7: Going Global** -- i18n, translation plugin, REST API. *(Trovato Phases 4-5)*
8. **Part 8: Production Ready** -- Caching, batch, S3, testing. *(Trovato Phase 6)*

Supporting epics (span multiple tutorial parts):

- **[Epic 2: Search That Thinks](epic-02.md)** -- Progressive enhancement search: Pagefind client-side WASM, ranking signals, AI query expansion, AI summaries (spans Part 2 search + AI integration)
- **[Epic 3: AI as a Building Block](epic-03.md)** -- AI provider registry, `ai_request()` kernel service, token budgets, content enrichment, chatbot (spans Parts 2, 5-6)
- **Epic 14: Demo Installer & Seed Data** *(planned)*
- **Epic 15: Test Suite, Tested Documentation & Enforcement** *(planned)*

### Appendix (planned)

- Full plugin source for all five plugins
- Complete Gather definitions as TOML/config
- Tera template files
- confs.tech data mapping reference
- REST API documentation
- Translation string catalog (EN/IT)

---

## Installable Demo

```bash
trovato demo install ritrovo
```

This:

1. Runs migrations (creates Item Types, categories, stages, roles, permissions)
2. Installs all five plugins (`ritrovo_importer`, `ritrovo_cfp`, `ritrovo_notify`, `ritrovo_translate`, `ritrovo_access`)
3. Creates admin, editor, and demo user accounts
4. Runs initial import (current + next year conferences from confs.tech)
5. Publishes a curated subset to Live with descriptions, logos, and speaker profiles for ~20 marquee conferences
6. Seeds ~10 Italian conferences with English translations
7. Configures Gather pages, Tiles, Slots, and Menus
8. Applies the Ritrovo theme (Tera templates)
9. Enables REST API with demo API key

The user gets a fully functional, multilingual conference aggregator with real data, user accounts, editorial workflow, and API access out of the box.

---

## Testing Strategy

Ritrovo serves as Trovato's integration test suite. The tutorial IS the test suite -- every code block in the tutorial markdown is a testable assertion about Trovato's behavior.

See [Documentation Architecture](documentation-architecture.md) for the full enforcement specification.

### Approach

1. **Write the tutorial with tested code blocks.** Each tutorial step includes `trovato-test` (user story) and `trovato-test:internal` (Under the Hood) fenced code blocks that compile and run.
2. **Extract and run on every commit.** A build script reads the tutorial markdown, extracts tagged code blocks, and generates integration test functions. `cargo test --test tutorial` runs them all.
3. **Coverage enforcement.** A CI script verifies every tutorial step has at least one testable code block. You can't add a tutorial step without also asserting what it does.
4. **Breakage = update both.** When a core change breaks a tutorial test, the developer must update the tutorial prose and code blocks to match the new behavior. The PR can't merge until both code and docs are consistent.

### Test Categories

All of these are validated through the tutorial code blocks:

- **Content model tests:** Create Item Types, fields, categories. CRUD Items. Verify JSONB storage.
- **Stage tests:** Create in Incoming, promote to Curated, publish to Live. Verify stage-aware queries.
- **Revision tests:** Edit, revert, compare. Verify revision chain integrity.
- **Plugin tests:** Install/enable/disable plugins. Verify taps fire in correct order. Test WASM boundary.
- **Gather tests:** Execute each Gather definition. Verify filters, sorts, pagination, relationships.
- **Form tests:** Submit forms, validate multi-step state persistence, test CSRF, test AJAX endpoints.
- **Permission tests:** Verify each role's access to each stage, item type, and field.
- **File tests:** Upload, reference, orphan cleanup, public/private access.
- **API tests:** Hit every REST endpoint, verify response format, test auth and rate limiting.
- **i18n tests:** Verify translations, language switching, URL aliases.
- **Search tests:** Index, query, verify relevance ranking, verify stage-awareness.
- **Cache tests:** Verify invalidation on content change, stage-scoped keys.

---

## Open Questions

1. **confs.tech rate limiting?** GitHub raw content has no auth required but may rate-limit. The cron plugin should cache ETags and use conditional requests (`If-None-Match`).
2. **WYSIWYG implementation** -- Form API with `filtered_html` is designed but WYSIWYG editor integration (TipTap? ProseMirror?) is a Trovato-level decision. Ritrovo documents the requirement; Trovato core delivers the widget.
3. **Demo data freshness** -- Ship static seed data for offline-first experience, but enable cron importer by default so it refreshes on first run.
4. **Tutorial vs. Trovato readiness** -- Each tutorial part depends on corresponding Trovato phases being complete. Part 1 needs Phase 3, Part 2 needs Phase 2 + 3, Parts 3-4 need Phase 3 + 5, Part 5 needs Phase 4, Parts 6-7 need Phase 4 + 5, Part 8 needs Phase 6.
5. **LLM translation** -- The `ritrovo_translate` plugin can optionally call an LLM API for auto-translation. Cool demo, but adds an external dependency. Keep it optional (disabled by default) with a manual translation workflow as the primary path.
6. **Comment spam** -- Do we need CAPTCHA or rate limiting on comment submission? Probably yes for production, but the demo can skip this. Note it as a known gap.
7. **Email sending** -- The notification plugin needs an email transport. For the demo, log to console. For production, SMTP or a service like Postmark. The Queue API abstracts this.
8. **Speaker data source** -- confs.tech doesn't include speaker data. Speakers are manually created for the seed data. Could consider importing from a conference's website or API as a stretch goal.

---

## Related

- [Trovato Architecture Overview](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Overview.md)
- [Content Model](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Content-Model.md)
- [Query Engine (Gather)](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Query-Engine.md)
- [Render Tree & Forms](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Render-Theme.md)
- [Plugin SDK](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Plugin-SDK.md)
- [Web Layer](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Web-Layer.md)
- [Infrastructure](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Infrastructure.md)
- [Phases](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Phases.md)
- [Terminology](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Terminology.md)
- [confs.tech conference-data](https://github.com/tech-conferences/conference-data)
