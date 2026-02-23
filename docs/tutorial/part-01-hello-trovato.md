# Part 1: Hello, Trovato

Welcome to the Ritrovo tutorial. Over the next eight parts you will build a fully functional tech conference aggregator using Trovato. By the end of Part 1 you will have a running Trovato instance with a `conference` content type, a handful of manually created conferences, and a Gather listing that displays them at `/conferences`.

---

## Step 1: Install Trovato

Before anything else, you need a running Trovato instance. The full details are in [INSTALL.md](../../INSTALL.md); here is the short version.

### Prerequisites

- **Rust** (stable toolchain, 1.85+) -- install from <https://rustup.rs/>
- **PostgreSQL 16+** -- any standard installation works
- **Redis 7+** -- used for caching and sessions

Verify everything is available:

```bash
rustc --version        # 1.85 or newer
psql --version         # 16 or newer
redis-cli ping         # should print PONG
```

### Start the Services

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

The `.env` defaults work with Docker Compose out of the box:

```env
PORT=3000
DATABASE_URL=postgres://trovato:trovato@localhost:5432/trovato
REDIS_URL=redis://127.0.0.1:6379
DATABASE_MAX_CONNECTIONS=10
RUST_LOG=info,tower_http=debug,sqlx=warn
```

If you prefer to manage PostgreSQL and Redis yourself, create the database manually:

```bash
psql -c "CREATE USER trovato WITH PASSWORD 'trovato';"
psql -c "CREATE DATABASE trovato OWNER trovato;"
```

### What Happens on First Startup

On first startup, Trovato will:

1. Connect to PostgreSQL and Redis.
2. Run all database migrations (including the ones that create the `conference` Item Type, seed data, and Gather definition).
3. Discover and install plugins from `plugins/`.
4. Start listening on `http://localhost:3000`.

Subsequent server starts skip the installer and apply any new migrations automatically.

### Run the Installer

Open `http://localhost:3000` in your browser. You will be redirected to the web installer, which walks you through four steps:

1. **Welcome** -- confirms PostgreSQL and Redis are connected.
2. **Create Admin Account** -- set username, email, and password (minimum 12 characters).
3. **Site Configuration** -- set site name, slogan, and contact email.
4. **Complete** -- links to the site and admin dashboard.

### Verify the Health Check

After the installer finishes, verify everything is healthy:

```bash
curl http://localhost:3000/health
# {"status":"healthy","postgres":true,"redis":true}
```

You now have a working Trovato instance with an admin account.

---

## Step 2: Define the Conference Item Type

Every piece of content in Trovato is an **Item**. Items are typed -- a blog post, a page, and a conference are all Items, but each has its own set of fields. The blueprint that describes which fields an Item has is called an **Item Type**.

Trovato ships with one built-in Item Type: `page` (a simple page with a body field). Ritrovo needs a `conference` type with fields for dates, location, CFP (Call for Papers -- the submission process conferences use to solicit talk proposals) info, topics, files, and more.

### Creating the Type

Trovato offers two ways to create an Item Type:

1. **Admin UI** -- Navigate to `/admin/structure/types/add`, fill in the form, then add fields one by one at `/admin/structure/types/{machine_name}/fields`.
2. **SQL migration** -- Insert directly into the `item_type` table. This is what Ritrovo does so the type definition is reproducible and version-controlled.

**Why is direct SQL safe here?** In some CMS platforms, direct database queries bypass the hook system, which can leave caches stale or skip side effects. Trovato's content type system is different: no taps fire when a type is created (neither through the admin UI nor through SQL). The type registry loads all types from the database at startup and caches them in memory. Both approaches produce identical results.

For production sites that need version-controlled configuration without raw SQL, Trovato also provides `config export/import`:

```bash
# Export all configuration (including item types) to YAML
cargo run --release -- config export ./config/

# Import configuration from YAML (with dry-run validation)
cargo run --release -- config import ./config/ --dry-run
cargo run --release -- config import ./config/
```

The Ritrovo migration lives at `crates/kernel/migrations/20260224000001_seed_conference_item_type.sql`. When you run `sqlx migrate run` (or start the server, which runs pending migrations automatically), the `conference` type is created.

After the migration runs, verify the type exists:

```bash
curl http://localhost:3000/api/content-types | jq '.[] | select(. == "conference")'
```

You can also visit `/admin/structure/types` in your browser. You should see "Conference" listed alongside "Basic Page". Click it to inspect the field definitions.

### Field Types

Trovato supports these field types:

| FieldType | Description | Example |
|---|---|---|
| `Text` | Short text with optional max length | City name, URL, language code |
| `TextLong` | Long text (rich text with a format like `filtered_html`) | Description, editor notes |
| `Date` | A calendar date | Start date, CFP deadline |
| `Boolean` | True/false | "Is this an online event?" |
| `File` | A file upload (image, PDF, etc.) | Conference logo, schedule PDF |
| `RecordReference` | A link to another record (item, category term, etc.) | Topics, speakers |
| `Integer` | Whole number | Attendee count |
| `Float` | Decimal number | Rating score |
| `Email` | Email address | Contact email |
| `Compound` | A structured field containing sub-fields | Multi-section layouts |

### The Conference Fields

The `conference` Item Type defines 17 fields (the conference name is handled by the built-in `title` column on every Item):

| Field | Type | Required | Multi-value | Purpose |
|---|---|---|---|---|
| `field_url` | Text (max 2048) | no | no | Conference website URL |
| `field_start_date` | Date | **yes** | no | When it starts |
| `field_end_date` | Date | **yes** | no | When it ends |
| `field_city` | Text (max 255) | no | no | City (blank for online-only) |
| `field_country` | Text (max 255) | no | no | Country |
| `field_online` | Boolean | no | no | Whether it is online |
| `field_cfp_url` | Text (max 2048) | no | no | Call for Papers URL |
| `field_cfp_end_date` | Date | no | no | CFP deadline |
| `field_description` | TextLong | no | no | Rich-text description |
| `field_topics` | RecordReference (category_term) | no | **yes** | Topic categories |
| `field_logo` | File | no | no | Conference logo image |
| `field_venue_photos` | File | no | **yes** | Venue/event photos |
| `field_schedule_pdf` | File | no | no | Schedule as PDF |
| `field_speakers` | RecordReference (speaker) | no | **yes** | Linked speaker profiles |
| `field_language` | Text (max 10) | no | no | ISO 639-1 language code |
| `field_source_id` | Text (max 255) | no | no | Dedup key for imports |
| `field_editor_notes` | TextLong | no | no | Internal notes for editors |

### Required vs. Optional

Only `field_start_date` and `field_end_date` are required. Everything else is optional so that conferences can be created incrementally -- an importer might supply just the name, dates, and URL, with editors enriching the record later. The `field_source_id` field is a computed dedup key that the importer plugin will use in Part 2.

### Multi-Value Fields (Cardinality)

Most fields have a cardinality of 1 (single value). Three fields use cardinality -1, meaning they accept an unlimited number of values:

- **field_topics** -- a conference can span multiple topics (Rust, WebAssembly, Systems)
- **field_venue_photos** -- a gallery of event photos
- **field_speakers** -- many speakers per conference

Multi-value fields are stored as JSON arrays in the JSONB `fields` column on the `item` table.

<details>
<summary>Under the Hood: JSONB Field Storage</summary>

The `item_type.settings` column stores field definitions as a JSONB object with a `fields` array. Each entry is a `FieldDefinition`:

```json
{
  "fields": [
    {
      "field_name": "field_start_date",
      "field_type": "Date",
      "label": "Start Date",
      "required": true,
      "cardinality": 1
    },
    {
      "field_name": "field_topics",
      "field_type": {"RecordReference": "category_term"},
      "label": "Topics",
      "required": false,
      "cardinality": -1
    },
    {
      "field_name": "field_city",
      "field_type": {"Text": {"max_length": 255}},
      "label": "City",
      "required": false,
      "cardinality": 1
    }
  ]
}
```

Notice the three shapes of `field_type`:

- **Unit variants** like `Date`, `Boolean`, `File`, `TextLong` serialize as a plain JSON string: `"Date"`.
- **Newtype variants** like `RecordReference("category_term")` serialize as `{"RecordReference": "category_term"}`.
- **Struct variants** like `Text { max_length: Some(255) }` serialize as `{"Text": {"max_length": 255}}`.

This matches Rust's default serde externally-tagged enum serialization. When the kernel boots, it deserializes these definitions into `FieldDefinition` structs (defined in `crates/plugin-sdk/src/types.rs`) and registers them in the `ContentTypeRegistry`.

Actual item data is stored in the `item.fields` JSONB column -- not in `item_type.settings`. The `item_type` defines the schema; `item.fields` holds the values. This separation means you can add or remove fields from a type without migrating existing item data.

</details>

---

## Step 3: Create Your First Conference

Now that the `conference` Item Type exists, let's create some conferences. This is deliberately manual -- you should feel the friction that motivates the importer plugin in Part 2.

### Navigating to the Form

1. Open `/admin/content` in your browser. This is the content listing page.
2. Click **Add content** (or go directly to `/admin/content/add`).
3. You'll see a list of available Item Types. Click **Conference**.

This opens the auto-generated form at `/admin/content/add/conference`. Trovato inspects the `conference` type's field definitions and renders the correct HTML input for each field type:

- **Date** fields (`field_start_date`, `field_end_date`, `field_cfp_end_date`) render as `<input type="date">` -- your browser shows a date picker.
- **Boolean** fields (`field_online`) render as a checkbox.
- **Text** fields (`field_city`, `field_country`, `field_url`) render as standard text inputs.
- **TextLong** fields (`field_description`, `field_editor_notes`) render as multi-line textareas.
- **File** fields (`field_logo`, `field_schedule_pdf`) render as file inputs (upload wiring comes later).
- **RecordReference** fields (`field_topics`, `field_speakers`) render as text inputs accepting UUIDs (autocomplete comes later).

The title field at the top uses the custom label "Conference Name" (from `title_label` in the Item Type definition).

### Walkthrough: Creating a Conference

The Ritrovo migration seeds three conferences automatically (see "Seeded Conferences" below), so let's create a fourth one by hand. Fill in the form with these values:

| Field | Value |
|---|---|
| Conference Name | RustNation UK 2026 |
| Conference Website | https://rustnationuk.com |
| Start Date | 2026-03-17 |
| End Date | 2026-03-18 |
| City | London |
| Country | United Kingdom |
| Online Event | (unchecked) |
| Description | A Rust conference in the heart of London. |

Leave the remaining fields blank (they're optional) and make sure **Published** is checked. Click **Create content**.

You'll be redirected to `/admin/content` where "RustNation UK 2026" now appears in the list alongside the seeded conferences.

### What Happened on Submit

When you submitted the form, the kernel:

1. **Validated** -- checked that `field_start_date` and `field_end_date` (the two required fields) were present.
2. **Extracted fields** -- separated the dynamic field values (`field_url`, `field_start_date`, etc.) from system fields (`title`, `status`, CSRF token).
3. **Stored the item** -- inserted a row into the `item` table with the field values as a flat JSONB object in the `fields` column.
4. **Created a revision** -- inserted a snapshot into `item_revision` for the revision history.
5. **Generated a URL alias** -- every item gets a system path like `/item/{uuid}`. In a later part of this tutorial we will configure pathauto to generate human-friendly aliases like `/conferences/rustconf-2026`.

### Viewing the Item

Every item is viewable at `/item/{id}` (where `{id}` is the item's UUID). This is the public, non-admin view. Trovato resolves templates in priority order:

1. `templates/elements/item--conference--{id}.html` (item-specific override)
2. `templates/elements/item--conference.html` (type-specific template)
3. `templates/elements/item.html` (default fallback)

The default template renders all fields with their labels. In Part 3 we will create a custom `item--conference.html` template with proper layout, but for now the default rendering shows that content creation works end to end.

There is also a JSON API at `/api/item/{id}` for programmatic access:

```bash
curl http://localhost:3000/api/item/YOUR-ITEM-UUID | jq .
```

### Inspecting the Database Row

Connect to your database and query the item:

```sql
SELECT id, title, fields, created, stage_id
FROM item
WHERE type = 'conference'
ORDER BY created DESC
LIMIT 1;
```

The `fields` column contains a flat JSON object:

```json
{
  "field_url": "https://rustnationuk.com",
  "field_start_date": "2026-03-17",
  "field_end_date": "2026-03-18",
  "field_city": "London",
  "field_country": "United Kingdom",
  "field_description": "A Rust conference in the heart of London."
}
```

Notice that `title` is a column on the `item` table itself, not inside `fields`. Boolean fields that are unchecked are simply absent from the JSON (the checkbox was not checked, so `field_online` is not submitted).

### Item IDs and Timestamps

Every item gets a **UUIDv7** identifier. UUIDv7 encodes a millisecond timestamp in its most significant bits, which means IDs are naturally time-sorted -- you can ORDER BY `id` and get chronological order without an extra index on `created`.

The `created` and `changed` columns store **Unix timestamps** (seconds since epoch), not SQL `TIMESTAMP` values. This keeps time handling consistent across time zones and avoids database-specific timestamp semantics.

### Stages

Every item has a `stage_id` that defaults to the **live** stage (a deterministic UUID seeded during installation). The live stage is the production-visible stage -- items on it are visible to all visitors.

In Part 4, we will explore how Stages let you prepare content changes on a draft or review stage before promoting them to live. For now, you can ignore stages entirely: every item you create goes directly to the live stage and is immediately visible.

### Seeded Conferences

The Ritrovo migration also seeds three conferences so you have data to work with even without filling in forms:

1. **RustConf 2026** -- Portland, OR, Sep 9--11. Includes CFP URL and deadline.
2. **EuroRust 2026** -- Paris, France, Oct 15--16. A European Rust conference.
3. **WasmCon Online 2026** -- Online-only, Jul 22--23. Exercises the `field_online` boolean.

You can see all of them at `/admin/content`.

---

## Step 4: Build Your First Gather

You have conferences in the database, but no public page that lists them. That's where **Gathers** come in. A Gather is Trovato's declarative query engine -- you define *what* you want (which item type, which filters, which sort order) and Trovato generates the SQL, handles pagination, and renders the results.

### The Ritrovo Migration

Like the conference Item Type and seed data, the Gather definition is created by a migration (`crates/kernel/migrations/20260226000002_seed_conference_gather.sql`). This migration:

1. Inserts a `gather_query` row with query ID `upcoming_conferences`.
2. Creates a URL alias from `/conferences` to `/gather/upcoming_conferences`.

When the server starts (or when you run `sqlx migrate run`), this migration runs automatically. You don't need to create the Gather by hand -- but let's walk through what it contains.

### Gather Definition

The Gather definition is a JSONB document stored in the `gather_query` table:

```
GatherDefinition {
  base_table: "item",
  item_type: "conference",
  filters: [
    { field: "status", operator: "equals", value: 1 }
  ],
  sorts: [
    { field: "fields.field_start_date", direction: "asc" }
  ],
  pager: { items_per_page: 25 }
}
```

This tells Trovato: "Select all published conference items, sort them by start date (soonest first), and show 25 per page."

This is a simplified version of the full Upcoming Conferences Gather -- no exposed filters, no relationships, no topic joins yet. Those come in Part 2.

### How It Works

When a visitor hits `/conferences`, the kernel:

1. **Resolves the URL** -- the path alias middleware rewrites `/conferences` to `/gather/upcoming_conferences` transparently (the visitor's URL bar still shows `/conferences`).
2. **Loads the definition** -- reads the `upcoming_conferences` row from `gather_query`.
3. **Builds a parameterized SQL query** -- the Gather engine translates the definition into a SQL query:

```sql
SELECT id, current_revision_id, type, title, author_id, status,
       created, changed, promote, sticky, fields, stage_id, language
FROM item
WHERE type = 'conference' AND status = 1 AND stage_id = $1
ORDER BY item.fields->>'field_start_date' ASC
LIMIT 25 OFFSET 0
```

4. **Renders the results** -- using the configured display format (table, with pagination controls).

No code was written. No templates were edited. The Gather system translated your declarative definition into a working page.

### Viewing the Listing

Visit `http://localhost:3000/conferences` in your browser. You should see a table listing all four conferences (the three seeded ones plus "RustNation UK 2026" if you created it in Step 3), sorted by start date with the soonest conference first.

The Gather is also available as JSON via the REST API:

```bash
curl http://localhost:3000/api/query/upcoming_conferences/execute
```

This returns a JSON response with the query results, pagination info, and total count -- useful for building custom front ends or feeding data to other services.

### URL Routing

Gathers are served at `/gather/{query_id}` by default. To give a Gather a clean URL like `/conferences`, Trovato uses the same **URL alias** system that gives items human-friendly paths. The migration inserts an alias record:

```sql
INSERT INTO url_alias (id, source, alias, language, stage_id, created)
VALUES (gen_random_uuid(), '/gather/upcoming_conferences', '/conferences', 'en', 'live', ...);
```

The path alias middleware intercepts incoming requests, looks up the alias, and rewrites the URI before it reaches the router. Query strings and pagination parameters pass through unchanged.

### Pagination

The Gather renders 25 items per page with next/previous controls. The pager shows the total result count so visitors know how many conferences exist. Pagination is handled via a `page` query parameter -- `/conferences?page=2` shows the second page.

### Empty State

If no conferences match the filters (for example, if you delete all conferences), the Gather displays the configured empty text: "No conferences found." rather than a blank page.

### Creating Gathers Through the Admin UI

The migration creates the Gather automatically, but you can also create Gathers through the admin UI:

1. Navigate to `/admin/gather`.
2. Click **Create gather query** (or go directly to `/admin/gather/create`).
3. Fill in the query ID, label, base table, item type, filters, sorts, and display settings.
4. Click **Save**.

The admin UI at `/admin/gather` lists all Gather definitions and lets you edit, clone, or delete them. You can use it to experiment with different filter and sort combinations.

### Exposed Filters (Preview)

In the Gather definition, each filter has an "Exposed" option. If you enable it, the filter becomes a URL query parameter that visitors can use to narrow results. For example, you could expose a filter on `fields.field_country` so visitors can filter conferences by country. We'll revisit exposed filters in a later part of this tutorial when we build a more advanced search experience.

---

## What You've Built

By the end of Part 1, you have:

- A running Trovato instance with PostgreSQL and Redis.
- A `conference` Item Type with 17 fields for dates, location, CFP details, topics, and more.
- Four conferences (three seeded, one created by hand), each viewable at `/item/{uuid}`.
- A Gather listing at `/conferences` that displays all published conferences sorted by start date.

This is enough to see the shape of Trovato. Everything after this builds on these fundamentals.

---

## What's Deferred

These are explicitly **not** in Part 1 (to set expectations):

- **Plugins** -- Part 2 introduces the conference importer
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
- [Ritrovo Overview](../ritrovo/overview.md)
- [Documentation Architecture](../ritrovo/documentation-architecture.md)
- [Content Model Design](../design/Design-Content-Model.md)
- [Query Engine Design (Gather)](../design/Design-Query-Engine.md)
