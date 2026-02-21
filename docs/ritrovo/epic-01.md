# Epic 1: Hello, Trovato

**Tutorial Part:** 1
**Trovato Phase Dependency:** Phase 3 (Content Model, CCK, Gather)
**BMAD Epic:** 29
**Status:** Not started

---

## Narrative

*The appetizer. You install Trovato, define one content type, create a few items by hand, and see a real listing page. By the end, the reader understands the core loop: define a type, create items, query with Gather.*

This is the "hello world" of Trovato -- but unlike most hello worlds, you walk away with something real. Four tutorial steps, three BMAD stories, and at the end you have a working conference listing page built from scratch.

No plugins, no users, no permissions yet. Just the core loop.

---

## Tutorial Steps

### Step 1: Install & Scaffold

```bash
cargo install trovato-cli && trovato new ritrovo
```

Walk through what `trovato new` generates:

- Project directory structure
- `Cargo.toml` with Trovato dependencies
- Default configuration (`trovato.toml` or equivalent)
- Database setup (PostgreSQL connection, migrations)
- Dev server startup (`trovato serve`)

Verify the scaffold runs: hit `http://localhost:3000` and see the default Trovato welcome page.

### Step 2: Define the Conference Item Type

Create the `conference` Item Type definition. This is where the reader learns how Trovato's content model works.

**What to cover:**

- `.info.toml` file structure (or however Item Types are declared)
- Field definitions and their JSONB mapping
- Field types: `TextValue` (plain), Date, Boolean, Category reference (multi), File (declared but upload not yet wired), RecordReference (declared but speaker type not yet created)
- Required vs. optional fields
- The `source_id` computed dedup key

**Fields defined in this step** (from Overview, Content Model):

| Field | Type | Notes |
|---|---|---|
| `name` | `TextValue` (plain) | Title field, required |
| `url` | `TextValue` (plain) | Conference website |
| `start_date` | Date | Required |
| `end_date` | Date | Required |
| `city` | `TextValue` (plain) | Nullable for online-only |
| `country` | `TextValue` (plain) | Nullable for online-only |
| `online` | Boolean | Default false |
| `cfp_url` | `TextValue` (plain) | Nullable |
| `cfp_end_date` | Date | Nullable |
| `description` | `TextValue` (filtered_html) | WYSIWYG comes later; plain text for now |
| `topics` | Category reference (multi) | Declared but taxonomy not yet created |
| `logo` | File (image) | Declared but upload not yet wired |
| `venue_photos` | File (image, multi) | Declared but upload not yet wired |
| `schedule_pdf` | File (pdf) | Declared but upload not yet wired |
| `speakers` | RecordReference (multi) | Declared but speaker type not yet created |
| `language` | `TextValue` (plain) | ISO 639-1 |
| `source_id` | `TextValue` (plain) | Dedup key |
| `editor_notes` | `TextValue` (plain) | Internal notes |

Run migration to create the database schema. Verify with a raw SQL query or admin tool that the tables exist.

### Step 3: Create Content Manually

Use the admin UI to create 3-5 conferences by hand. This is deliberately manual -- the reader should feel the friction that motivates the importer plugin in Part 2.

**Conferences to enter** (real conferences, entered manually):

- RustConf 2026 (pick a real one from confs.tech if dates are known)
- A European conference (to have non-US data)
- An online-only conference (to exercise the `online` boolean)
- At least one with a CFP URL and end date

For each, enter: name, URL, start/end dates, city, country, online flag, description (plain text for now), language. Skip file uploads and speakers for now -- those fields exist but aren't wired yet.

**What to cover:**

- The admin UI form (auto-generated from Item Type definition)
- How JSONB storage works under the hood (show the raw database row)
- How Items get IDs and timestamps
- The difference between creating an Item and publishing it (foreshadow Stages, but don't configure them yet -- everything goes to the default stage)

### Step 4: Build Your First Gather

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

- How to define a Gather (config file or API)
- How Gather translates to SQL (show the generated query)
- How to attach a Gather to a URL route (`/conferences`)
- Pagination basics
- The default rendering (list of items with field values -- no custom templates yet, those come in Part 3)

Visit `http://localhost:3000/conferences` and see the listing page with the manually entered conferences, sorted by start date, with pagination controls.

---

## BMAD Stories

### Story 29.1: Define `conference` Item Type

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

- How to define an Item Type with fields
- How JSONB storage works
- How to create content through the admin UI
- How Gather queries and displays content
- How routing connects a URL to a Gather

This is enough to see the shape of Trovato. Everything after this builds on these fundamentals.

---

## What's Deferred

These are explicitly **not** in Part 1 (and the tutorial should say so, to set expectations):

- **Plugins** -- Part 2 introduces the importer
- **Categories/taxonomy** -- Part 2
- **Search** -- Part 2
- **Templates/theming** -- Part 3 (Part 1 uses default rendering)
- **File uploads** -- Part 3 (fields declared but not wired)
- **Speakers** -- Part 3 (RecordReference declared but speaker type not created)
- **Users/auth** -- Part 4
- **Stages** -- Part 4 (everything in default stage for now)
- **Revisions** -- Part 4

---

## Related

- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Trovato Content Model](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Content-Model.md)
- [Trovato Query Engine (Gather)](https://github.com/jeremyandrews/trovato/blob/main/docs/design/Design-Query-Engine.md)
