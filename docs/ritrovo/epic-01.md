# Epic 1: Hello, Trovato

**Tutorial Part:** 1
**Trovato Phase Dependency:** Phase 3 (Content Model, CCK, Gather)
**BMAD Epic:** 29
**Status:** Stories 29.1 and 29.2 complete; 29.3 not started

---

## Narrative

*The appetizer. You install Trovato, define one content type, create a few items by hand, and see them on the site. By the end, the reader understands the core loop: define a type, create items, view them individually and as a listing with Gather.*

This is the "hello world" of Trovato -- but unlike most hello worlds, you walk away with something real. Four tutorial steps, three BMAD stories, and at the end you have a working conference listing page built from scratch.

No plugins, no users, no permissions yet. Just the core loop.

---

## Tutorial Steps

### Step 1: Install Trovato

Before anything else, you need a running Trovato instance. The full details are in [INSTALL.md](../../INSTALL.md); here is the short version.

**Prerequisites:**

- Rust stable toolchain (1.85+)
- PostgreSQL 15+
- Redis 7+

The fastest path uses Docker Compose for the dependencies:

```bash
# Start PostgreSQL and Redis
docker compose up -d

# Build Trovato
cargo build --release

# Configure environment (defaults work with docker-compose)
cp .env.example .env

# Start the server (runs migrations automatically)
cargo run --release
```

On first startup Trovato will:

1. Connect to PostgreSQL and Redis
2. Run all database migrations
3. Discover and install plugins from `plugins/`
4. Start listening on `http://localhost:3000`

Open your browser to `http://localhost:3000`. You will be redirected to the web installer, which walks you through four steps:

1. **Welcome** -- confirms PostgreSQL and Redis are connected
2. **Create Admin Account** -- set username, email, and password (minimum 12 characters)
3. **Site Configuration** -- set site name, slogan, and contact email
4. **Complete** -- links to the site and admin dashboard

After the installer finishes, verify everything is healthy:

```bash
curl http://localhost:3000/health
# {"status":"healthy","postgres":true,"redis":true}
```

You now have a working Trovato instance with an admin account. Subsequent server starts skip the installer and apply any new migrations automatically.

### Step 2: Define the Conference Item Type

Trovato offers two ways to create an Item Type:

1. **Admin UI** -- Navigate to `/admin/structure/types/add`, fill in the form, then add fields one by one at `/admin/structure/types/{machine_name}/fields`.
2. **SQL migration** -- Insert directly into the `item_type` table. This is what Ritrovo does so the type definition is reproducible and version-controlled.

**Why is direct SQL safe here?** In Drupal, direct queries bypass the hook system, which can leave caches stale or skip side effects. Trovato's content type system is different: no taps fire when a type is created (neither through the admin UI nor through SQL). The type registry loads all types from the database at startup and caches them in memory. Both approaches produce identical results.

For production sites that need version-controlled configuration without raw SQL, Trovato also provides `config export/import`:

```bash
# Export all configuration (including item types) to YAML
cargo run --release -- config export ./config/

# Import configuration from YAML (with dry-run validation)
cargo run --release -- config import ./config/ --dry-run
cargo run --release -- config import ./config/
```

**The `conference` Item Type:**

Ritrovo needs a `conference` type with fields for dates, location, CFP (Call for Papers -- the submission process conferences use to solicit talk proposals) information, topics, files, and more.

| Field | Type | Notes |
|---|---|---|
| `name` | `TextValue` (plain) | Title field, required |
| `url` | `TextValue` (plain) | Conference website |
| `start_date` | Date | Required |
| `end_date` | Date | Required |
| `city` | `TextValue` (plain) | Nullable for online-only |
| `country` | `TextValue` (plain) | Nullable for online-only |
| `online` | Boolean | Default false |
| `cfp_url` | `TextValue` (plain) | Link to the Call for Papers page; nullable |
| `cfp_end_date` | Date | Deadline for talk submissions; nullable |
| `description` | `TextValue` (filtered_html) | WYSIWYG comes later; plain text for now |
| `topics` | Category reference (multi) | Declared but category not yet created |
| `logo` | File (image) | Declared but upload not yet wired |
| `venue_photos` | File (image, multi) | Declared but upload not yet wired |
| `schedule_pdf` | File (pdf) | Declared but upload not yet wired |
| `speakers` | RecordReference (multi) | Declared but speaker type not yet created |
| `language` | `TextValue` (plain) | ISO 639-1 |
| `source_id` | `TextValue` (plain) | Dedup key for the importer plugin (Part 2) |
| `editor_notes` | `TextValue` (plain) | Internal notes |

**What to cover in the tutorial prose:**

- How Item Type definitions map to JSONB storage (one row per item, fields stored as a JSON object)
- Field types and their JSONB representation
- Required vs. optional fields
- The `source_id` computed dedup key (foreshadowing the importer in Part 2)

Run the migration (or use the admin UI) and verify the type exists:

```bash
curl http://localhost:3000/api/content-types | jq '.[] | select(.name == "conference")'
```

### Step 3: Create Content Manually

Use the admin UI to create 3-5 conferences by hand. This is deliberately manual -- the reader should feel the friction that motivates the importer plugin in Part 2.

**Conferences to enter** (real conferences, entered manually):

- RustConf 2026 (pick a real one from confs.tech if dates are known)
- A European conference (to have non-US data)
- An online-only conference (to exercise the `online` boolean)
- At least one with a CFP URL and end date

For each, enter: name, URL, start/end dates, city, country, online flag, description (plain text for now), language. Skip file uploads and speakers for now -- those fields exist but are not wired yet.

**What to cover:**

- The admin UI form at `/item/add/conference` (auto-generated from the Item Type definition)
- How JSONB storage works under the hood (show the raw database row)
- How Items get UUIDs and timestamps automatically
- Viewing the created item at its canonical URL

**Viewing items:**

Every item is viewable at `/item/{id}` (where `{id}` is the item's UUID). This is the public, non-admin view. Trovato resolves templates in priority order:

1. `templates/elements/item--conference--{id}.html` (item-specific override)
2. `templates/elements/item--conference.html` (type-specific template)
3. `templates/elements/item.html` (default fallback)

The default template renders all fields with their labels. In Part 3 we will create a custom `item--conference.html` template with proper layout, but for now the default rendering shows that content creation works end to end.

There is also a JSON API at `/api/item/{id}` for programmatic access:

```bash
curl http://localhost:3000/api/item/YOUR-ITEM-UUID | jq .
```

**A note on Stages:**

Every item has a `stage_id` that defaults to the **live** stage (a deterministic UUID seeded during installation). The live stage is the production-visible stage -- items on it are visible to all visitors.

In Part 4, we will explore how Stages let you prepare content changes on a draft or review stage before promoting them to live. For now, you can ignore stages entirely: every item you create goes directly to the live stage and is immediately visible to visitors.

### Step 4: Build Your First Gather

A Gather is Trovato's declarative query engine -- you define *what* you want (which item type, which filters, which sort order) and Trovato generates the SQL, handles pagination, and renders the results.

Create the "Upcoming Conferences" Gather definition:

```
GatherDefinition {
  base_item_type: "conference",
  fields: [name, start_date, end_date, city, country, online],
  filters: [
    { field: "start_date", op: Gte, value: ":current_date" }
  ],
  sorts: [
    { field: "start_date", direction: Asc }
  ],
  pager: { items_per_page: 25 }
}
```

This is a simplified version of the full Upcoming Conferences Gather (no exposed filters, no relationships, no topics yet -- those come in Part 2).

**What to cover:**

- How to define a Gather (admin UI at `/admin/structure/gather/add` or config import)
- How Gather translates to SQL (show the generated query for the curious)
- How to attach a Gather to a URL route (`/conferences`)
- Pagination basics (next/previous controls, 25 items per page)
- The default rendering (list of items with field values -- no custom templates yet, those come in Part 3)

Visit `http://localhost:3000/conferences` and see the listing page with the manually entered conferences, sorted by start date, with pagination controls.

- Empty state handled gracefully when no conferences match the date filter

---

## BMAD Stories

### Story 29.1: Define `conference` Item Type

**Status:** Complete

**As a** developer building a Trovato site,
**I want to** define a `conference` Item Type with all fields,
**So that** I can create and store conference data.

**Acceptance criteria:**

- Item Type definition declares `conference` with all fields from the content model
- Migration creates the appropriate database schema
- JSONB fields are correctly typed and nullable where specified
- File and RecordReference fields are declared (but don't need to be functional yet)
- `source_id` field exists for dedup

### Story 29.2: Admin UI for Manual Conference Creation

**Status:** Complete

**As a** site administrator,
**I want to** create conferences through a web form,
**So that** I can populate the site with content.

**Acceptance criteria:**

- Auto-generated form renders all `conference` fields
- Required fields are validated (name, start_date, end_date)
- Date fields use date picker widgets
- Boolean field renders as checkbox
- Created Items are stored in JSONB and retrievable
- Success message shown after creation with link to view the Item

### Story 29.3: "Upcoming Conferences" Gather with Pagination

**Status:** Not started

**As a** site visitor,
**I want to** see a list of upcoming conferences sorted by date,
**So that** I can discover conferences to attend.

**Acceptance criteria:**

- Gather definition created for `conference` items
- Filter: `start_date >= current_date`
- Sort: `start_date` ascending
- Pagination: 25 items per page with next/previous controls
- Gather attached to `/conferences` route
- Default rendering shows field values (name, dates, city, country, online)
- Empty state handled gracefully (no conferences yet)

---

## Payoff

A working (if sparse) conference listing page you built from scratch. The reader understands:

- How to install Trovato and complete the web installer
- How to define an Item Type with fields
- How JSONB storage works
- How to create content through the admin UI
- How to view individual items at `/item/{id}`
- How Gather queries and displays content as a listing
- How routing connects a URL to a Gather

This is enough to see the shape of Trovato. Everything after this builds on these fundamentals.

---

## What's Deferred

These are explicitly **not** in Part 1 (and the tutorial should say so, to set expectations):

- **Plugins** -- Part 2 introduces the importer
- **Categories** -- Part 2
- **Search** -- Part 2
- **Templates/theming** -- Part 3 (Part 1 uses default rendering)
- **File uploads** -- Part 3 (fields declared but not wired)
- **Speakers** -- Part 3 (RecordReference declared but speaker type not created)
- **Users/auth** -- Part 4
- **Stages** -- Part 4 (everything in default live stage for now)
- **Revisions** -- Part 4

---

## Related

- [INSTALL.md](../../INSTALL.md) -- Full installation guide
- [Building Your First Site](../building-your-first-site.md) -- Simpler getting-started walkthrough
- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Content Model Design](../design/Design-Content-Model.md)
- [Query Engine Design (Gather)](../design/Design-Query-Engine.md)
