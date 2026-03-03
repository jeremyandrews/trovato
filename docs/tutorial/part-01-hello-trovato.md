# Part 1: Hello, Trovato

Welcome to the Ritrovo tutorial. Over the next eight parts you will build a fully functional tech conference aggregator using Trovato. By the end of Part 1 you will have a running Trovato instance with a `conference` content type, three hand-created conferences, a Gather listing that displays them at `/conferences`, and human-friendly URL aliases like `/conferences/rustconf-2026`.

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
cargo run --release --bin trovato
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

### Troubleshooting Startup

**"role trovato does not exist"** -- If you see this error after running `docker compose up -d`, you likely have a local PostgreSQL installation already listening on port 5432. The Docker container maps to the same port, but your local Postgres answers first. Fix: stop your local PostgreSQL (e.g. `brew services stop postgresql`) and retry, or use the manual setup above with your local instance instead of Docker.

**Starting fresh** -- If you need to reset the database (bad migration state, want a clean slate), destroy and recreate the Docker volumes:

```bash
docker compose down -v
docker compose up -d
```

This deletes all data and gives you a fresh PostgreSQL and Redis. On the next `cargo run`, all migrations run from scratch and the web installer will appear again.

### What Happens on First Startup

On first startup, Trovato will:

1. Connect to PostgreSQL and Redis.
2. Run all database migrations.
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

## Step 2: Create the Conference Item Type

Every piece of content in Trovato is an **Item**. Items are typed -- a blog post, a page, and a conference are all Items, but each has its own set of fields. The blueprint that describes which fields an Item has is called an **Item Type**.

Trovato ships with one built-in Item Type: `page` (a simple page with a body field). Ritrovo needs a `conference` type with fields for dates, location, CFP (Call for Papers -- the submission process conferences use to solicit talk proposals) info, and more.

> **Shortcut:** To skip creating this content type by hand, import the pre-built config from the project root:
>
> ```bash
> cargo run --release --bin trovato -- config import docs/tutorial/config --dry-run
> cargo run --release --bin trovato -- config import docs/tutorial/config
> ```
>
> This creates the `conference` type with all 12 fields. If a `conference` type already exists it will be overwritten. The running server will pick up the new type automatically within the cache TTL window (60 seconds by default, configurable via `CACHE_TTL`). Then continue at Step 3 below.

### Creating the Type

1. Navigate to `/admin/structure/types` in your browser. You should see "Basic Page" listed.
2. Click **Add content type** (or go directly to `/admin/structure/types/add`).
3. Fill in the form:

| Field | Value |
|---|---|
| Name | Conference |
| Machine name | conference |
| Description | A tech conference or meetup event |
| Title field label | Conference Name |

4. Click **Save content type**.

You are redirected to the content types list. "Conference" now appears alongside "Basic Page".

### Adding Fields

Click **Manage fields** next to "Conference" (or go to `/admin/structure/types/conference/fields`). You will see the built-in **Title** field and an "Add a new field" form at the bottom.

Add each field below one at a time. For each row, fill in the **Label**, verify the auto-generated **Machine name**, select the **Type**, and click **Add field**:

| Label | Machine name | Type |
|---|---|---|
| Website URL | `field_url` | Text (plain) |
| Start Date | `field_start_date` | Date |
| End Date | `field_end_date` | Date |
| City | `field_city` | Text (plain) |
| Country | `field_country` | Text (plain) |
| Online | `field_online` | Boolean |
| CFP URL | `field_cfp_url` | Text (plain) |
| CFP End Date | `field_cfp_end_date` | Date |
| Description | `field_description` | Text (long) |
| Language | `field_language` | Text (plain) |
| Source ID | `field_source_id` | Text (plain) |
| Editor Notes | `field_editor_notes` | Text (long) |

That is 12 fields. The machine name auto-generates from the label (prefixed with `field_`), but you can edit it before clicking **Add field**. Make sure the machine names match the table above -- they are the keys used in the JSON field storage and by the importer plugin in Part 2.

> **Important:** For "Website URL", the auto-generated machine name will be `field_website_url`. Edit it to `field_url` before clicking **Add field** -- the importer plugin in Part 2 expects this shorter name.

After adding all fields, make **Start Date** and **End Date** required: click the **Edit** link next to each one, check the **Required** checkbox, and save.

### What About the Other Fields?

The importer plugin (installed in Part 2) will automatically add three more fields to this type when it starts up: `field_topics` (for tagging conference topics), `field_twitter` (Twitter/X handle), and `field_coc_url` (Code of Conduct URL). Trovato lets you add fields to a type at any time without migrating existing data -- new fields are simply absent from older items until edited.

### Verify the Type

Confirm the type exists via the API:

```bash
curl http://localhost:3000/api/content-types | jq '.[] | select(. == "conference")'
```

You should see `"conference"` in the output.

### Field Types Reference

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
      "field_name": "field_city",
      "field_type": {"Text": {"max_length": null}},
      "label": "City",
      "required": false,
      "cardinality": 1
    }
  ]
}
```

Notice the shapes of `field_type`:

- **Unit variants** like `Date`, `Boolean`, `File`, `TextLong` serialize as a plain JSON string: `"Date"`.
- **Struct variants** like `Text { max_length: None }` serialize as `{"Text": {"max_length": null}}`.
- **Newtype variants** like `RecordReference("category_term")` serialize as `{"RecordReference": "category_term"}` (used in later parts).

This matches Rust's default serde externally-tagged enum serialization. When the kernel boots, it deserializes these definitions into `FieldDefinition` structs (defined in `crates/plugin-sdk/src/types.rs`) and registers them in the `ContentTypeRegistry`.

Actual item data is stored in the `item.fields` JSONB column -- not in `item_type.settings`. The `item_type` defines the schema; `item.fields` holds the values. This separation means you can add or remove fields from a type without migrating existing item data.

For production sites that need version-controlled configuration, Trovato provides `config export/import`:

```bash
# Export all configuration (including item types) to YAML
cargo run --release --bin trovato -- config export ./config/

# Import configuration from YAML
cargo run --release --bin trovato -- config import ./config/
```

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

The title field at the top uses the custom label "Conference Name" (from `title_label` in the Item Type definition).

### Conference 1: RustConf 2026

This first conference gets a detailed field-by-field walkthrough. Fill in the form with these values:

| Field | Value |
|---|---|
| Conference Name | RustConf 2026 |
| Website URL | https://rustconf.com |
| Start Date | 2026-09-09 |
| End Date | 2026-09-11 |
| City | Portland |
| Country | United States |
| CFP URL | https://rustconf.com/cfp |
| CFP End Date | 2026-06-15 |
| Description | The official Rust conference, featuring talks on the latest Rust developments. |
| Published | (checked) |

Leave the remaining fields blank (they're optional). Click **Create content**.

You'll be redirected to `/admin/content` where "RustConf 2026" now appears in the list.

### Conference 2: EuroRust 2026

Go back to `/admin/content/add/conference` and create a second conference:

| Field | Value |
|---|---|
| Conference Name | EuroRust 2026 |
| Website URL | https://eurorust.eu |
| Start Date | 2026-10-15 |
| End Date | 2026-10-16 |
| City | Paris |
| Country | France |
| Description | Europe's premier Rust conference, bringing together Rustaceans from across the continent. |
| Published | (checked) |

Click **Create content**.

### Conference 3: WasmCon Online 2026

One more -- this time an online-only conference, which exercises the `field_online` boolean checkbox. Navigate to `/admin/content/add/conference`:

| Field | Value |
|---|---|
| Conference Name | WasmCon Online 2026 |
| Website URL | https://wasmcon.dev |
| Start Date | 2026-07-22 |
| End Date | 2026-07-23 |
| Online | (checked) |
| Description | A virtual conference dedicated to WebAssembly, covering toolchains, runtimes, and the component model. |
| Published | (checked) |

Notice that **City** and **Country** are left blank -- this is an online event, so a physical location doesn't apply. Click **Create content**.

You now have three conferences in your content listing at `/admin/content`.

### What Happened on Submit

When you submitted each form, the kernel:

1. **Validated** -- checked that `field_start_date` and `field_end_date` (the two required fields) were present.
2. **Extracted fields** -- separated the dynamic field values (`field_url`, `field_start_date`, etc.) from system fields (`title`, `status`, CSRF token).
3. **Stored the item** -- inserted a row into the `item` table with the field values as a flat JSONB object in the `fields` column.
4. **Created a revision** -- inserted a snapshot into `item_revision` for the revision history.
5. **Generated a URL alias** -- every item gets a system path like `/item/{uuid}`. In Step 5 we will configure pathauto to generate human-friendly aliases like `/conferences/rustconf-2026` automatically.

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
  "field_url": "https://rustconf.com",
  "field_start_date": "2026-09-09",
  "field_end_date": "2026-09-11",
  "field_city": "Portland",
  "field_country": "United States",
  "field_cfp_url": "https://rustconf.com/cfp",
  "field_cfp_end_date": "2026-06-15",
  "field_description": "The official Rust conference, featuring talks on the latest Rust developments."
}
```

Notice that `title` is a column on the `item` table itself, not inside `fields`. Boolean fields that are unchecked are simply absent from the JSON (the checkbox was not checked, so `field_online` is not submitted). For WasmCon Online 2026, you would see `"field_online": "1"` present in the JSON.

### Item IDs and Timestamps

Every item gets a **UUIDv7** identifier. UUIDv7 encodes a millisecond timestamp in its most significant bits, which means IDs are naturally time-sorted -- you can ORDER BY `id` and get chronological order without an extra index on `created`.

The `created` and `changed` columns store **Unix timestamps** (seconds since epoch), not SQL `TIMESTAMP` values. This keeps time handling consistent across time zones and avoids database-specific timestamp semantics.

### Stages

Every item has a `stage_id` that defaults to the **live** stage (a deterministic UUID seeded during installation). The live stage is the production-visible stage -- items on it are visible to all visitors.

In Part 4, we will explore how Stages let you prepare content changes on a draft or review stage before promoting them to live. For now, you can ignore stages entirely: every item you create goes directly to the live stage and is immediately visible.

---

## Step 4: Build Your First Gather

You have conferences in the database, but no public page that lists them. That's where **Gathers** come in. A Gather is Trovato's declarative query engine -- you define *what* you want (which item type, which filters, which sort order) and Trovato generates the SQL, handles pagination, and renders the results.

### Creating the Gather via Admin UI

Let's create a Gather that lists all published conferences sorted by start date.

1. Navigate to `/admin/gather` in your browser.
2. Click **Create gather query** (or go directly to `/admin/gather/create`).
3. Fill in the form:

| Field | Value |
|---|---|
| Query ID | upcoming_conferences |
| Label | Upcoming Conferences |
| Description | Published conferences sorted by start date |
| Base Table | item |
| Item Type | conference |

4. **Add a filter** -- this ensures only published conferences appear:
   - Field: `status`
   - Operator: `equals`
   - Value: `1`

5. **Add a sort** -- this orders conferences by when they start:
   - Field: `fields.field_start_date`
   - Direction: `asc`

6. **Display settings:**
   - Format: `table`
   - Items per page: `25`
   - Empty text: `No conferences found.`

7. Click **Save**.

### Gather Definition

Here's what the form created in the database. The Gather definition is a JSONB document stored in the `gather_query` table:

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

When a visitor hits the Gather's URL, the kernel:

1. **Loads the definition** -- reads the `upcoming_conferences` row from `gather_query`.
2. **Builds a parameterized SQL query** -- the Gather engine translates the definition into a SQL query:

```sql
SELECT id, current_revision_id, type, title, author_id, status,
       created, changed, promote, sticky, fields, stage_id, language
FROM item
WHERE type = 'conference' AND status = 1 AND stage_id = $1
ORDER BY item.fields->>'field_start_date' ASC
LIMIT 25 OFFSET 0
```

3. **Renders the results** -- using the configured display format (table, with pagination controls).

No code was written. No templates were edited. The Gather system translated your declarative definition into a working page.

### Creating the URL Alias

The Gather is accessible at `/gather/upcoming_conferences`, but that's not a user-friendly URL. Let's create a clean alias.

1. Navigate to `/admin/structure/aliases/add`.
2. Fill in the form:

| Field | Value |
|---|---|
| Source path | /gather/upcoming_conferences |
| Alias | /conferences |

3. Click **Save**.

Now `/conferences` transparently resolves to `/gather/upcoming_conferences` -- visitors see the clean URL in their browser.

### Viewing the Listing

Visit `http://localhost:3000/conferences` in your browser. You should see a table listing all three conferences you created in Step 3, sorted by start date with the soonest conference first (WasmCon Online in July, then RustConf in September, then EuroRust in October).

The Gather is also available as JSON via the REST API:

```bash
curl http://localhost:3000/api/query/upcoming_conferences/execute
```

This returns a JSON response with the query results, pagination info, and total count -- useful for building custom front ends or feeding data to other services.

### URL Routing

Gathers are served at `/gather/{query_id}` by default. The URL alias you just created gives it a clean URL. The path alias middleware intercepts incoming requests, looks up the alias, and rewrites the URI before it reaches the router. Query strings and pagination parameters pass through unchanged.

### Pagination

The Gather renders 25 items per page with next/previous controls. The pager shows the total result count so visitors know how many conferences exist. Pagination is handled via a `page` query parameter -- `/conferences?page=2` shows the second page.

### Empty State

If no conferences match the filters (for example, if you delete all conferences), the Gather displays the configured empty text: "No conferences found." rather than a blank page.

### Exposed Filters (Preview)

In the Gather definition, each filter has an "Exposed" option. If you enable it, the filter becomes a URL query parameter that visitors can use to narrow results. For example, you could expose a filter on `fields.field_country` so visitors can filter conferences by country. We'll revisit exposed filters in a later part of this tutorial when we build a more advanced search experience.

---

## Step 5: Human-Friendly URLs

Every item is accessible at `/item/{uuid}`, but UUIDs are terrible to share, bookmark, or read aloud. This step configures pathauto to generate aliases like `/conferences/rustconf-2026` automatically whenever a conference is saved.

### How It Works

Trovato's pathauto system reads a pattern from site configuration (stored in `site_config` under the key `pathauto_patterns`), expands tokens using the item's data, slugifies the result, and creates an entry in the `url_alias` table automatically on save. The path alias middleware intercepts incoming requests and rewrites `/conferences/rustconf-2026` to `/item/{uuid}` before routing -- so the canonical URL remains `/item/{uuid}` internally, while visitors always see the clean alias.

### Option A: Admin UI

Navigate to `/admin/config/pathauto`. You will see a table listing every registered content type with a text input for its URL pattern.

For the **Conference** row, enter:

```
conferences/[title]
```

Click **Save configuration**.

Now click **Regenerate aliases** next to Conference. Trovato will iterate all existing conference items, generate the appropriate alias for each, and report how many were created. After this step, the three conferences you created in Step 3 will have aliases:

| Conference | Alias |
|---|---|
| RustConf 2026 | `/conferences/rustconf-2026` |
| EuroRust 2026 | `/conferences/eurorust-2026` |
| WasmCon Online 2026 | `/conferences/wasmcon-online-2026` |

Any new conference saved after this point will have its alias generated automatically on save -- no manual step needed.

### Option B: Config Import

If you are following the config-import workflow, the pattern is already waiting for you in `docs/tutorial/config/variable.pathauto_patterns.yml`. Import it and then regenerate via the admin UI:

```bash
# Import the pathauto pattern
cargo run --release --bin trovato -- config import docs/tutorial/config
```

> **Warning:** `config import` replaces the **entire** `pathauto_patterns` value in `site_config`. If you have already configured patterns for other content types via the admin UI, those patterns will be overwritten. Import first, then add any additional patterns back through the UI.

After importing, navigate to `/admin/config/pathauto` and click **Regenerate aliases** next to the Conference row. The regenerate step must be done through the UI — the regenerate endpoint is a CSRF-protected form POST that requires a live admin session, not a scriptable API call.

### Verify

```bash
curl -I http://localhost:3000/conferences/rustconf-2026
# HTTP/1.1 200 OK

curl -I http://localhost:3000/conferences/eurorust-2026
# HTTP/1.1 200 OK

curl -I http://localhost:3000/conferences/wasmcon-online-2026
# HTTP/1.1 200 OK
```

The tutorial integration tests for Step 5 (`test_part01_step05_*` in `tutorial_test.rs`) verify these aliases at the model layer, confirming the correct rows exist in `url_alias` after regeneration.

### How Slugification Works

The `[title]` token is run through `slugify()`, which:

1. Converts to lowercase.
2. Replaces any character that is not `a–z`, `0–9` with a hyphen.
3. Collapses consecutive hyphens into one.
4. Strips leading and trailing hyphens.
5. Truncates at 128 characters, breaking at a word boundary.

So "RustConf 2026" becomes `rustconf-2026`, "C++ Now" becomes `c-now`, and "  ⚡ Fast!" becomes `fast`. The full pattern `conferences/[title]` becomes `/conferences/rustconf-2026` -- the leading slash is added automatically.

If two items produce the same base alias (e.g., two conferences both named "Rust Fest"), pathauto generates `/conferences/rust-fest` for the first and `/conferences/rust-fest-1` for the second. Up to 99 numeric suffixes are tried before falling back to a UUID fragment.

### What About Title Changes?

When you edit a conference and save it through the admin form, `update_alias_item` runs automatically. If the title changed, the alias is regenerated to match the new title. The old alias is removed -- it does not redirect automatically. Redirect handling (for SEO continuity) is covered in a later part of the tutorial.

If you clear the pattern in `/admin/config/pathauto` and regenerate, the service stops generating new aliases but does not delete existing ones.

<details>
<summary>Under the Hood: How the Alias Resolver Works</summary>

When a request arrives for `/conferences/rustconf-2026`, the path alias middleware:

1. Looks up the alias in the `url_alias` table: `SELECT source FROM url_alias WHERE alias = '/conferences/rustconf-2026'`.
2. Finds the source `/item/{uuid}`.
3. Rewrites the request URI to `/item/{uuid}` in-place before passing it to the router.

The router never sees the alias -- it only ever sees canonical `/item/{uuid}` paths. This is why the same middleware that resolves `/conferences/rustconf-2026` → `/item/{uuid}` also resolves `/conferences` → `/gather/upcoming_conferences`: both are just rows in `url_alias`.

```trovato-test:internal
-- Verify alias rows exist in the database after regeneration
SELECT source, alias
FROM url_alias
WHERE alias LIKE '/conferences/%'
ORDER BY alias;
-- Expected: three rows, one per conference
```

The alias lookup hits the database on every request — the path alias middleware does not use a Redis cache or in-process cache. This means newly created aliases are immediately effective without any cache invalidation step. The trade-off is a database query per non-system request; this is acceptable for sites at tutorial scale and can be addressed with a read replica or connection pool tuning for high-traffic deployments.

</details>

---

## What You've Built

By the end of Part 1, you have:

- A running Trovato instance with PostgreSQL and Redis.
- A `conference` Item Type with 12 fields for dates, location, CFP details, and more.
- Three conferences (created by hand), each viewable at `/item/{uuid}`.
- A Gather listing at `/conferences` that displays all published conferences sorted by start date.
- Human-friendly URL aliases (`/conferences/rustconf-2026`, etc.) generated automatically by pathauto.

You also now understand:

- How Items, Item Types, and JSONB field storage relate to each other.
- How Gathers translate declarative definitions into parameterized SQL.
- How URL aliases and path alias middleware decouple public URLs from internal identifiers.
- How pathauto generates clean URLs from content using configurable token patterns.

This is enough to see the shape of Trovato. Everything after this builds on these fundamentals.

---

## What's Deferred

These are explicitly **not** in Part 1 (to set expectations):

- **Plugins** -- Part 2 introduces the conference importer
- **Categories** -- Part 2
- **Search** -- Part 2
- **Templates/theming** -- Part 3 (Part 1 uses default rendering)
- **File uploads** -- Part 3
- **Speakers** -- Part 3
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
