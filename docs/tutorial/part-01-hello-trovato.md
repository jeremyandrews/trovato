# Part 1: Hello, Trovato

Welcome to the Ritrovo tutorial. Over the next eight parts you will build a fully functional tech conference aggregator using Trovato. By the end of Part 1 you will have a running Trovato instance with a `conference` content type, a handful of manually created conferences, and a Gather listing that displays them.

---

## Step 1: Installation

### Prerequisites

You need three things installed:

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

Trovato includes a `docker-compose.yml` that runs PostgreSQL 17 and Redis 7 with a single command:

```bash
docker compose up -d
```

This gives you a `trovato` database owned by user `trovato` with password `trovato`, plus Redis on the default port. If you prefer to manage PostgreSQL and Redis yourself, create the database manually:

```bash
psql -c "CREATE USER trovato WITH PASSWORD 'trovato';"
psql -c "CREATE DATABASE trovato OWNER trovato;"
```

### Configure Environment

Copy the example environment file and review the defaults:

```bash
cp .env.example .env
```

The defaults work with Docker Compose out of the box:

```env
PORT=3000
DATABASE_URL=postgres://trovato:trovato@localhost:5432/trovato
REDIS_URL=redis://127.0.0.1:6379
DATABASE_MAX_CONNECTIONS=10
RUST_LOG=info,tower_http=debug,sqlx=warn
```

### Build and Run

Build the kernel and start the server:

```bash
cargo build --release
cargo run --release
```

On first startup, Trovato automatically:

1. Connects to PostgreSQL and Redis.
2. Runs all pending database migrations (including the ones that create the `conference` Item Type and seed data).
3. Discovers and loads WASM plugins from the `plugins/` directory.
4. Starts listening on `http://localhost:3000`.

### Verify the Health Check

```bash
curl http://localhost:3000/health
```

You should see:

```json
{"status":"healthy","postgres":true,"redis":true}
```

### Run the Installer

Open `http://localhost:3000` in your browser. The first-time setup redirects you to the installation wizard, where you will:

1. Confirm that PostgreSQL and Redis are connected.
2. Create an admin account (username, email, password).
3. Set basic site configuration (site name, slogan).

Once complete, you can access the admin dashboard at `http://localhost:3000/admin`.

---

## Step 2: Define the Conference Item Type

Every piece of content in Trovato is an **Item**. Items are typed -- a blog post, a page, and a conference are all Items, but each has its own set of fields. The blueprint that describes which fields an Item has is called an **Item Type**.

Trovato ships with one built-in Item Type: `page` (a simple page with a body field). Ritrovo needs a `conference` type with fields for dates, location, CFP (Call for Papers) info, topics, files, and more.

### What Is an Item Type?

An Item Type is a row in the `item_type` table. It stores:

- **type** -- A machine name (lowercase, no spaces). This is the primary key.
- **label** -- A human-readable name shown in the admin UI.
- **description** -- A short explanation for content editors.
- **has_title / title_label** -- Whether items of this type have a title field, and what to call it. Conferences use "Conference Name" instead of the default "Title".
- **plugin** -- Which plugin owns this type. Core types use `core`; Ritrovo types use `ritrovo`.
- **settings** -- A JSONB column containing the field definitions.

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

Only `field_start_date` and `field_end_date` are required. Everything else is optional so that conferences can be created incrementally -- an importer might supply just the name, dates, and URL, with editors enriching the record later.

### Multi-Value Fields (Cardinality)

Most fields have a cardinality of 1 (single value). Three fields use cardinality -1, meaning they accept an unlimited number of values:

- **field_topics** -- a conference can span multiple topics (Rust, WebAssembly, Systems)
- **field_venue_photos** -- a gallery of event photos
- **field_speakers** -- many speakers per conference

Multi-value fields are stored as JSON arrays in the JSONB `fields` column on the `item` table.

### Creating the Type

Trovato offers two ways to create an Item Type:

1. **Admin UI** -- Navigate to `/admin/structure/types/add`, fill in the form, then add fields one by one at `/admin/structure/types/conference/fields`.
2. **SQL migration** -- Insert directly into the `item_type` table. This is what Ritrovo does so the type is reproducible and version-controlled.

The Ritrovo migration lives at `crates/kernel/migrations/20260224000001_seed_conference_item_type.sql`. When you run `sqlx migrate run` (or start the server, which runs pending migrations automatically), the `conference` type is created.

After the migration runs, visit `/admin/structure/types` in your browser. You should see "Conference" listed alongside "Basic Page". Click it to inspect the field definitions.

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

Now that the `conference` Item Type exists, let's create a conference via the admin UI.

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

The `stage_id` column holds a UUID that references the stage an item belongs to. New items default to the **live** stage (a well-known UUID seeded during installation). In later parts of this tutorial we'll explore how Stages let you prepare content changes on a draft stage before promoting them to live. For now, every item goes directly to the live stage.

### Seeded Conferences

The Ritrovo migration also seeds three conferences so you have data to work with even without filling in forms:

1. **RustConf 2026** -- Portland, OR, Sep 9--11. Includes CFP URL and deadline.
2. **EuroRust 2026** -- Paris, France, Oct 15--16. A European Rust conference.
3. **WasmCon Online 2026** -- Online-only, Jul 22--23. Exercises the `field_online` boolean.

You can see all of them at `/admin/content`.

---

## Step 4: Build a Gather Listing

You have conferences in the database, but no public page that lists them. That's where **Gathers** come in. A Gather is Trovato's declarative query builder -- you describe what data to fetch (base table, filters, sort order) and how to display it (table, list, grid), and Trovato generates the query and renders the results.

### Creating the Gather

1. Navigate to `/admin/gather` in your browser.
2. Click **Create gather query** (or go directly to `/admin/gather/create`).
3. Fill in the form:

| Field | Value |
|---|---|
| Query ID | `conf_listing` |
| Label | Conferences |
| Description | All published conferences |

4. In the **Definition** section, set:

   - **Base table**: `item`
   - **Item type**: `conference` -- this restricts results to conference items only.
   - Add a **filter**: field `status`, operator `equals`, value `1`. Leave "Exposed" unchecked. This ensures only published conferences appear.
   - Add a **sort**: field `created`, direction `desc`. Newest conferences appear first.

5. In the **Display** section, set:

   - **Format**: `table`
   - **Items per page**: `20`
   - **Empty text**: `No conferences found.`
   - Enable the **pager** and **show result count** checkboxes.

6. Click **Save**.

### Viewing the Listing

Open `/gather/conf_listing` in your browser. You should see a table listing all four conferences (the three seeded ones plus "RustNation UK 2026" if you created it in Step 3). Each row shows the item's fields, and the pager appears at the bottom.

The Gather is also available as JSON via the REST API:

```bash
curl http://localhost:3000/api/query/conf_listing/execute
```

This returns a JSON response with the query results, pagination info, and total count -- useful for building custom front ends or feeding data to other services.

### What Just Happened

When you saved the Gather, Trovato:

1. **Stored the definition** -- inserted a row into the `gather_query` table with the query ID, filters, sorts, and display settings as JSONB.
2. **Registered the route** -- the Gather is now accessible at `/gather/conf_listing` (HTML) and `/api/query/conf_listing/execute` (JSON).

When a visitor hits `/gather/conf_listing`, the kernel:

1. Loads the query definition from the database.
2. Builds a parameterized SQL query from the filters and sorts.
3. Executes it against the `item` table, filtering by `type = 'conference'` and `status = 1`.
4. Renders the results using the configured display format (table, in this case).

No code was written. No templates were edited. The Gather system translated your declarative definition into a working page.

### Exposed Filters (Preview)

In the Gather definition, each filter has an "Exposed" checkbox. If you check it, the filter becomes a URL query parameter that visitors can use to narrow results. For example, you could expose a filter on `fields.field_country` so visitors can filter conferences by country. We'll revisit exposed filters in a later part of this tutorial when we build a more advanced search experience.

---

## What You've Built

By the end of Part 1, you have:

- A running Trovato instance with PostgreSQL and Redis.
- A `conference` Item Type with 17 fields for dates, location, CFP details, topics, and more.
- Four conferences (three seeded, one created by hand).
- A Gather listing at `/gather/conf_listing` that displays all published conferences.

In **Part 2** we'll add human-friendly URL aliases, set up the pathauto system for automatic slug generation, and start building navigation menus.
