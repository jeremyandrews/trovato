# Part 3: Look & Feel

Part 2 ended with 5,000+ conferences flowing through five Gather queries, but the site still renders everything with default templates. Part 3 transforms Ritrovo from a data pipe into a real website.

You will learn the **Render Tree** -- the structured pipeline that turns Items into safe HTML. You will tour the Tera templates that control conference detail pages, CFP listings, and the base page layout. You will wire up file uploads for logos and venue photos, create a second content type (Speaker) with RecordReference links to conferences, configure page regions with Tiles, set up navigation menus with breadcrumbs, and add full-text search with weighted fields.

**Start state:** Default HTML rendering, no site chrome, no files, one content type.
**End state:** A navigable conference directory with styled detail pages, speaker profiles, sidebar tiles, proper navigation, full-text search, and file uploads.

> **Where this configuration lives now.** The `conference` and `speaker` types,
> the topic category and its terms, the Gather queries, the roles, the stages, the
> tiles, the menu links and the aliases are Ritrovo's content model, and they moved
> to the Ritrovo repository in
> [jeremyandrews/ritrovo#11](https://github.com/jeremyandrews/ritrovo/pull/11),
> merged as `3322075`. The set lives there at `demo/config/`, and that copy is the
> one being developed against the design brief.
>
> The copy under `docs/tutorial/config/` stays, is still shipped in the released
> image, and is still what the commands in this part import, so everything below
> works as written. The two copies have already diverged, and this one is not the
> one that will be corrected. This part will be rewritten against the Ritrovo set;
> until it is, treat `docs/tutorial/config/` as a frozen snapshot rather than a
> model to copy.

---

## Step 1: The Render Tree & Tera Templates

Every piece of content in Trovato passes through the **Render Tree** before it reaches the browser. Understanding this pipeline is key to controlling how your site looks.

### The Four Phases

When a visitor requests `/item/{id}`, the kernel:

1. **Build** -- Loads the item from the database and constructs a structured data tree. Fields are processed through format-specific filter pipelines (`FilterPipeline::for_format()`). Compound fields are sorted by weight. All user-supplied content is HTML-escaped via `html_escape()`.

2. **Alter** -- Plugins can mutate the tree through the `tap_item_view_alter` tap. A plugin might add a badge, reorder fields, or strip fields based on permissions (you will see this in Part 4's access control plugin).

3. **Sanitize** -- Text format filters run on rich text fields. `filtered_html` keeps a configured tag/attribute allowlist; `full_html` (available to trusted editors) keeps a fixed, more permissive allowlist. Both always strip `<script>`, event handlers and `javascript:`/`data:` URLs -- all HTML output is safe by construction.

4. **Render** -- The Tera template engine converts the data tree into HTML. Template resolution follows a specificity chain, and the result is wrapped in the base page layout.

Plugins never produce raw HTML -- they produce structured data that the kernel renders safely. This is the same principle as React's virtual DOM or SwiftUI's view builders: the framework controls the final output.

### Template Resolution Chain

Tera resolves templates by checking for the most specific file first:

1. `templates/elements/item--conference--{uuid}.html` -- Override for a single item
2. `templates/elements/item--conference.html` -- Override for all conferences
3. `templates/elements/item.html` -- Default fallback for all items

The same pattern applies to Gather queries: `query--{query_id}.html` → `query.html`.

When no template matches at all, the Rust-side render fallback produces semantic HTML: item-type elements render as `<article>`, containers with headings as `<section>`, navigation as `<nav>`. Images get `loading="lazy"` by default to improve page load performance. This means even without custom templates, the output is accessible and performant.

Trovato ships with templates for the conference and speaker types. Let's tour them.

### The Conference Detail Template

Open `templates/elements/item--conference.html`. This template controls how conference items render at `/item/{id}` (and at their pathauto aliases like `/conferences/rustconf-2026`).

Key sections:

- **Header** -- Title, date range, location (city/country), and an "Online" badge for virtual events.
- **Media** -- Logo image (`field_logo`) and venue photo (`field_venue_photo`), rendered as `<img>` tags pointing to the file serving endpoint at `/files/{path}`.
- **Description** -- The conference description rendered with `| safe` (marked with a `{# SAFE: #}` comment because the render pipeline already sanitized it).
- **External links** -- Website and CFP submission links pulled from `safe_urls`, not from raw `item.fields`. This prevents `javascript:` URI injection -- more on this in a moment.
- **Speakers** -- A reverse reference section that lists speakers who reference this conference. The kernel finds these automatically by scanning RecordReference fields across other content types.
- **Children** -- Any child items rendered through the pipeline.

### The safe_urls Pattern

Tera's autoescape prevents `<script>` injection by escaping `<`, `>`, `&`, and `"`. But it does **not** prevent `javascript:` URI injection in `href` attributes -- `<a href="javascript:alert(1)">` passes autoescape unchanged because it contains no HTML-special characters.

The kernel solves this by building a `safe_urls` map in the view handler. For every string field in the item, if the value starts with `http://` or `https://`, it goes into `safe_urls`. The template uses `safe_urls.field_url` instead of `item.fields.field_url` for any value that appears in an `href`:

```html
{% if safe_urls.field_url is defined and safe_urls.field_url %}
<a href="{{ safe_urls.field_url }}" target="_blank" rel="noopener noreferrer">
    Visit website
</a>
{% endif %}
```

This is defense-in-depth: even if an attacker manages to store `javascript:alert(1)` in a URL field, it will never appear in an `href` attribute.

### The Open CFPs Template

Open `templates/gather/query--ritrovo.open_cfps.html`. This template extends the base `gather/query.html` layout and renders conferences with open Calls for Papers as a card grid. Each card shows:

- Conference title (linked to the detail page, not to the raw CFP URL)
- CFP deadline date
- Event dates and location
- "View details & submit" action link

Notice that the CFP template does **not** render external URLs directly. Instead, it links to the item detail page (`/item/{{ row.id }}`), where the full conference template shows the external links through the `safe_urls` pattern. This keeps all external URL rendering centralized in the detail template.

### Verify

Visit any conference detail page. You should see a styled layout with header metadata, description, and external links (if present):

```bash
# Pick a conference ID
ID=$(curl -s http://localhost:3000/api/v1/query/ritrovo.upcoming_conferences/execute | jq -r '.data[0].id')

# View the detail page
curl -s http://localhost:3000/item/$ID | grep -o 'class="conf-detail[^"]*"' | head -5
```

You should see CSS classes like `conf-detail__header`, `conf-detail__meta`, `conf-detail__desc`.

[<img src="images/part-03/conference-detail.png" width="600" alt="Conference detail page with styled header, metadata, description, and external links">](images/part-03/conference-detail.png)

Visit `/cfps` to see the CFP listing:

```bash
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/cfps
# 200
```

<details>
<summary>Under the Hood: Referenced Items and Reverse References</summary>

The `view_item` handler in `routes/item.rs` resolves two kinds of relationships:

**Forward references** -- For each RecordReference field in the item, the handler reads the UUID(s) stored in the field value, loads those items from the database, and passes them to the template as `referenced_items.{field_name}`. For example, a speaker's `field_conferences` contains an array of conference UUIDs. The handler resolves them to `[{id, title, type}, ...]` and the speaker template renders them as links.

Two more maps sit beside it: `referenced_files.{field_name}` resolves File fields to `[{id, url, filename, mime, alt}, ...]`, and `referenced_tags.{field_name}` resolves CategoryReference fields to `[{id, label, slug, url, ancestors}, ...]`, where `url` is the gather route that lists the tag's category. The full list of item template variables, with an example, is in [Design-Render-Theme.md](../design/Design-Render-Theme.md#item-template-variables).

**Reverse references** -- The handler scans all content types that have RecordReference fields. For each type, it loads up to 50 items and checks whether any field value matches the current item's ID. Matches are passed to the template as `reverse_references.{type_name}`. For example, when viewing a conference, the handler finds speakers whose `field_conferences` contains this conference's UUID.

This scan approach works at tutorial scale. At production scale, you would add a JSONB containment index (`CREATE INDEX ON item USING gin (fields jsonb_path_ops)`) and a targeted query instead of scanning.

```sql
-- Reverse reference: find speakers referencing this conference
SELECT id, title
FROM item
WHERE type = 'speaker'
  AND fields @> '{"field_conferences": ["CONFERENCE-UUID"]}'
```

</details>

---

## Step 2: File Uploads & Media

Conferences need visual identity -- logos, venue photos, maybe a schedule PDF. Trovato's file system manages uploads with UUID-based storage, MIME validation, and a temp-to-permanent lifecycle.

### Adding File Fields

The conference type's YAML config (`docs/tutorial/config/item_type.conference.yml`) already includes two File fields:

- `field_logo` -- Conference logo image
- `field_venue_photo` -- Venue or event photo

If you are following from Part 2 and these fields don't exist yet, re-import the config:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

After the cache TTL expires (60 seconds), the content form at `/admin/content/add/conference` will show file upload widgets for these fields.

### How File Upload Works

The file upload widget (`static/js/file-upload.js`) provides an AJAX upload experience:

1. The user selects a file via the file input.
2. JavaScript validates the file size (max 10 MB) on the client.
3. The file is uploaded via `POST /file/upload` as a multipart form with an `X-CSRF-Token` header.
4. The server validates the MIME type against an allowlist (`ALLOWED_MIME_TYPES`), checks magic bytes against the declared content type, and checks that the filename's extension maps to that same type. A `.exe` renamed to `.jpg` is rejected, and so is a PNG named `logo.svg`. A file over 10 MB is refused with `413 Payload Too Large` and a message saying so.
5. On success, the file gets a UUID-based filename in temporary storage and a row in `file_managed` with `status = 0` (temporary).
6. The widget shows a preview (thumbnails for images) with the filename and size, and stores the file's id in the field.
7. When the item is saved, by any path (the content form, the item API, a plugin, or `config import`), the kernel records the reference in `file_reference` and promotes the file to permanent (`status = 1`) in the same transaction. A user may only attach a file they could already fetch: their own upload, or a file on an item they can view.

Unreferenced temporary files are cleaned up by cron after 6 hours.

### Uploading a File

Edit an existing conference (e.g., RustConf 2026) at its edit URL. Find the **Conference Logo** field -- it shows a file input with "Choose file". Select a JPEG or PNG image and the widget will upload it immediately, showing a preview.

Click **Save** to save the conference. The file is now permanent.

### File Serving

Uploaded files are served at `/files/{path}` with security hardening:

- **Directory traversal prevention** -- Paths containing `..`, null bytes, or backslashes are rejected.
- **Access** -- A file is served to anyone who may view an item that references it (so a logo on a published conference is public), and a file no item references only to its uploader and administrators. Anything else is a 404.
- **Content-Type** -- The type stored in `file_managed` when the upload was validated, never a guess from the path. A path with no `file_managed` row falls back to the extension table, which has no SVG entry.
- **Content-Disposition** -- Images and PDFs are served inline (browser displays them). All other types use `attachment` (forces download) to prevent browsers from executing uploaded content.
- **X-Content-Type-Options: nosniff** -- Prevents browsers from MIME-sniffing the response.
- **Cache-Control** -- `public, max-age=604800` (one week) for performance.

The path alias and redirect middleware skip `/files` and `/file` prefixes to avoid interfering with file serving.

### Verify

After uploading a logo to a conference, check the file exists:

```bash
# Find the file record
$(brew --prefix libpq)/bin/psql postgres://trovato:trovato@localhost:5432/trovato \
  -c "SELECT id, filename, filemime, status FROM file_managed ORDER BY created DESC LIMIT 3;"
```

The `status` column should be `1` (permanent) for files referenced by saved items.

### Allowed File Types

The kernel allows these MIME types, and an uploaded file's extension must be one listed for its type. SVG is not allowed: it is XML that a browser will run scripts from.

| MIME Type | Extensions |
|---|---|
| `image/jpeg` | .jpg, .jpeg |
| `image/png` | .png |
| `image/gif` | .gif |
| `image/webp` | .webp |
| `application/pdf` | .pdf |
| `application/msword` | .doc |
| `application/vnd.openxmlformats-officedocument.wordprocessingml.document` | .docx |
| `application/vnd.ms-excel` | .xls |
| `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` | .xlsx |
| `text/plain` | .txt |
| `text/csv` | .csv |
| `application/zip` | .zip |
| `application/gzip` | .gz, .gzip |

---

## Step 3: The Speaker Content Type

Ritrovo tracks speakers alongside conferences. A speaker has a bio, company, photo, and -- critically -- references to the conferences they present at. This introduces Trovato's **RecordReference** field type.

### Speaker Type Definition

The speaker config lives at `docs/tutorial/config/item_type.speaker.yml`. Import it if you haven't already:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

The speaker type has these fields:

| Label | Machine name | Type | Notes |
|---|---|---|---|
| Biography | `field_bio` | TextLong | Speaker's bio, supports filtered_html |
| Company | `field_company` | Text | Current employer/organization |
| Website | `field_website` | Text | Personal website URL |
| Photo | `field_photo` | File | Headshot image |
| Conferences | `field_conferences` | RecordReference | Links to conference items (multi-value) |

### RecordReference Fields

A RecordReference field stores the UUID of another item. The `field_conferences` field on speakers stores an array of conference UUIDs. In the database, this looks like:

```json
{
  "field_conferences": ["uuid-of-rustconf", "uuid-of-eurorust"]
}
```

When the speaker detail page renders, the kernel resolves these UUIDs into actual item data (id, title, type) and passes them to the template as `referenced_items.field_conferences`. The speaker template renders them as clickable links to the conference detail pages.

### Creating Speakers

Navigate to `/admin/content/add/speaker` and create a few speakers. The **Conferences** field uses an autocomplete widget — start typing a conference name and select from the dropdown results. Under the hood, the autocomplete queries `GET /api/v1/items/autocomplete?type=conference&q=...` and stores the selected item's UUID in a hidden field.

> **Tip:** You can link multiple conferences by adding values to the multi-value field. Each autocomplete selection stores the conference UUID for that slot.

### Pathauto for Speakers

The pathauto patterns config (`docs/tutorial/config/variable.pathauto_patterns.yml`) includes a pattern for speakers:

```
speaker: speakers/[title]
```

After importing the config, regenerate aliases for speakers:

1. Navigate to `/admin/config/pathauto`.
2. Click **Regenerate aliases** next to Speaker.

Speakers now have URLs like `/speakers/jane-doe`.

### The Speaker Detail Template

Open `templates/elements/item--speaker.html`. The template shows:

- Name and company in the header
- Photo (if uploaded)
- Biography
- Website link (via `safe_urls.field_website`)
- A **Conferences** section listing linked conferences with links to their detail pages (forward references)

### Reverse References on Conferences

Visit a conference detail page that has speakers linked to it. Below the conference description, you will see a **Speakers** section listing every speaker who references this conference. These reverse references are resolved automatically -- the conference item type does not need a `field_speakers` field.

The kernel discovers these by scanning the `speaker` content type's RecordReference fields and finding matches. This is a powerful pattern: adding a new content type that references conferences (e.g., `workshop`, `sponsor`) would automatically show up in reverse reference sections without modifying the conference template.

### Verify

```bash
# Confirm speaker type exists
curl -s http://localhost:3000/api/v1/content-types | jq '.data[] | select(. == "speaker")'
# "speaker"

# Check a speaker's detail page (after creating one)
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/speakers/YOUR-SPEAKER-SLUG
# 200
```

---

## Step 4: Page Layout -- Slots, Tiles & Navigation

Until now, pages rendered their content but had no site chrome -- no header, no navigation, no sidebar, no footer. The base page template (`templates/page.html`) provides all of this through a five-region **Slot** system.

### The Five Regions

| Region | Purpose |
|---|---|
| **Header** | Site branding, search box |
| **Navigation** | Main menu |
| **Content** | Page content (items, gathers, forms) |
| **Sidebar** | Contextual tiles (topic cloud, CFP list) |
| **Footer** | Footer menu, site info |

Each region can hold **Tiles** -- content blocks assigned to a specific region with a weight (for ordering) and optional visibility rules.

### How It Works

On every page request, the `inject_site_context` helper in `routes/helpers.rs` builds the page context:

1. **Loads site config** -- Site name and slogan from `site_config`.
2. **Loads menus** -- Every config menu from the `menu_link` table, weight ordered, from a cache (no query on a warm page), filtered for the viewer, plus the plugin-registered entries.
3. **Loads tiles** -- For each region, calls `state.tiles().render_region()` which filters tiles by visibility rules (path patterns, user roles) and renders their HTML.
4. **Builds authentication context** -- Current user, admin status, CSRF token for the logout form.
5. **Passes current path** -- Used for active menu highlighting and breadcrumbs.

### Menus

Config menus live in the `menu_link` table. Each menu link has a title, path, weight (sort order), an optional parent link, and a menu name. Any menu name works: `main` and `footer` are the two the stock theme has always drawn, and every other name (`user`, `sidebar`, `legal`) reaches templates too.

Templates get every menu as a **tree** in `menus_by_name`, keyed by menu name. Each link has `title`, `path`, `active` and `children` (the links under it, also weight ordered), and only the links this viewer may follow are there. A link the viewer cannot follow is dropped with everything under it. Hiding a link is not access control: every page still checks for itself.

```html
{% import "macros/menu.html" as menu %}
<nav class="site-nav" id="site-nav" aria-label="Main">
    <ul class="site-nav__menu">
        {{ menu::items(links=menus_by_name | get(key="main", default=[]), prefix="site-nav") }}
    </ul>
</nav>
```

`macros/menu.html` draws each link as `<li>` with its children in a nested `<ul class="site-nav__children">`. The stock theme shows children as a dropdown on hover and on keyboard focus on wide screens, and indented under their parent on narrow ones.

**Active trail** -- `active` is set by the kernel, by one rule: the page asked for is the link's page, or a page under it. So a link to `/devices` is active on `/devices/new`, and a link to `/` is active only on `/`. The stock theme gives an active link the `site-nav__link--active` class and `aria-current="page"`. Links are matched against the address as asked for (`requested_path`), so an aliased page still marks its menu link.

**Plugin entries** -- A plugin registers navigation through `tap_menu`. Root entries arrive in `menus` (and as a tree in `menus_tree`); entries registered with a parent arrive in `menus_by_parent`, keyed by the parent path, so `menus_by_parent["/user"]` is everything plugins put in the account menu. The entries are the ones this viewer may see, by the same permission rule as the root entries. A `{uid}` segment in an entry's path becomes the signed-in viewer's id (`/user/{uid}/subscriptions` becomes `/user/7f3.../subscriptions`). An entry with `{uid}` is left out for an anonymous visitor, and so is an entry with any other `{...}` segment, because nobody can follow it.

The stock account area is Profile and Logout (or Login), then `menus_by_parent["/user"]`, then the site's own `user` menu.

**Menu button** -- Below 768 px the stock nav folds behind a `Menu` button (`<button aria-expanded aria-controls="site-nav">`), toggled by `static/js/site-nav.js`. With JavaScript off the button stays hidden and the nav stays open.

`main_menu`, `footer_menu`, `main_menu_owned` and `menus` are still there with their old flat shapes, for themes written against them. If no `main` config menu exists, the stock template draws the plugin-registered root entries instead.

### Tiles

Tiles are content blocks placed in page regions. Each tile has:

- **machine_name** -- Unique identifier (e.g., `search_box`, `topic_cloud`)
- **region** -- Which slot it appears in (header, navigation, sidebar, footer)
- **tile_type** -- `custom_html`, `menu`, or `gather_query`. A `menu` tile renders the config menu its `menu_name` names (default `main`) as a nested list, filtered for the viewer, through `elements/tile--menu.html`; override it for one tile with `elements/tile--menu--{machine_name}.html`. A `gather_query` tile lists the first `items` rows (1 to 50, default 5) of the gather its `query_id` names, run as the viewer, linked to each item and then to the gather's own page, through `elements/tile--gather_query.html`; a viewer who may not run the gather sees no tile. It shows no pager and no exposed filters.
- **config** -- JSON configuration (HTML content, gather query ID, etc.)
- **visibility** -- JSON rules for when the tile should appear (path patterns, role restrictions)
- **weight** -- Sort order within the region

The YAML config files in `docs/tutorial/config/` document the tile configurations for Ritrovo:

| Tile | Region | Purpose |
|---|---|---|
| `search_box` | Header | Search form posting to `/search` |
| `topic_cloud` | Sidebar | Topic tag links to `/topics/{slug}` |
| `conferences_this_month` | Sidebar | Condensed upcoming conferences |
| `open_cfps_sidebar` | Sidebar | Open CFPs with deadlines |
| `footer_info` | Footer | Site branding and info |

> **Note:** Tile and menu link configuration can be imported via `config import` using `tile.{uuid}.yml` and `menu_link.{uuid}.yml` files, or managed through the admin UI. The tutorial's set is in `docs/tutorial/config/`, indexed by machine name in that directory's `README.md`.

### Tile Visibility Rules

Each tile can have path-based, role-based and audience-based visibility rules. For example, the open CFPs sidebar tile might only appear on conference-related pages:

```json
{
  "paths": ["/conferences*", "/topics/*", "/cfps*"]
}
```

Visiting `/speakers/jane-doe` would not show the CFP tile because the path doesn't match any pattern.

A login tile is the classic case for `audience`: it should appear for a
signed-out visitor and disappear once they sign in. `roles` cannot express
that — every viewer, signed in or not, carries the `anonymous user` role in
the list a tile's visibility is matched against, so `{"roles": ["anonymous
user"]}` would match everyone. `audience` is decided from whether the viewer
is actually signed in, not from that role list:

```json
{
  "audience": "anonymous"
}
```

`{"audience": "authenticated"}` is the reverse — shown only once signed in.
Leaving `audience` out of the visibility JSON (or any other rule) shows the
tile to everyone, same as `{}`. The tile admin form exposes this as a "Show
to" field: Everyone, Anonymous visitors only, or Signed-in users only.

### Breadcrumbs

The page template renders breadcrumbs above the content area:

```html
{% if breadcrumbs %}
<nav class="breadcrumb" aria-label="Breadcrumb">
    <ol class="breadcrumb__list">
        {% for crumb in breadcrumbs %}
        <li class="breadcrumb__item">
            {% if crumb.path %}<a href="{{ crumb.path }}">{% endif %}
            {{ crumb.title }}
            {% if crumb.path %}</a>{% endif %}
        </li>
        {% endfor %}
    </ol>
</nav>
{% endif %}
```

Item pages show: **Home** > **Content Type Label** > **Item Title**

Gather pages show: **Home** > **Query Label**

### Verify

Visit the homepage and inspect the page structure:

```bash
# Check for site header
curl -s http://localhost:3000/ | grep -o 'class="site-header[^"]*"' | head -1

# Check for navigation menu
curl -s http://localhost:3000/conferences | grep -o 'class="site-nav[^"]*"' | head -3

# Check for breadcrumbs on a conference detail page
ID=$(curl -s http://localhost:3000/api/v1/query/ritrovo.upcoming_conferences/execute | jq -r '.data[0].id')
curl -s http://localhost:3000/item/$ID | grep -o 'class="breadcrumb[^"]*"' | head -1

# Check for sidebar (only appears if tiles are configured)
curl -s http://localhost:3000/conferences | grep -o 'class="page-layout__sidebar[^"]*"' | head -1
```

---

## Step 5: Full-Text Search

With thousands of conferences in the database, visitors need a way to find specific events. Trovato provides full-text search built on PostgreSQL's `tsvector` engine with configurable field weights.

### Search Field Configuration

Search field config files define which item fields are indexed and their relevance weights:

| Weight | Meaning | Fields |
|---|---|---|
| **A** (highest) | Primary match | Title |
| **B** | Secondary match | Description, speaker bio |
| **C** | Supporting match | City, country |
| **D** (lowest) | Background match | (none configured) |

The config files live at `docs/tutorial/config/search_field_config.*.yml`. Import them:

```bash
cargo run --release --bin trovato -- config import docs/tutorial/config
```

This imports six search field configuration entities:

| File | Content Type | Field | Weight |
|---|---|---|---|
| `search_field_config.conference_title.yml` | conference | title | A |
| `search_field_config.conference_description.yml` | conference | field_description | B |
| `search_field_config.conference_city.yml` | conference | field_city | C |
| `search_field_config.conference_country.yml` | conference | field_country | C |
| `search_field_config.speaker_title.yml` | speaker | title | A |
| `search_field_config.speaker_bio.yml` | speaker | field_bio | B |

### Rebuilding the Search Index

After importing search field config, the search index needs to be rebuilt so that existing items get their `search_vector` column populated. Trigger this via the admin UI at `/admin/structure/types/{type}/search` — click the "Reindex" button for each content type that has search fields configured.

### How Search Works

When a user searches for "rust":

1. PostgreSQL parses the query into a `tsquery`: `'rust':*`
2. The query is matched against the `search_vector` tsvector column on the `item` table
3. Results are ranked by `ts_rank_cd()`, which accounts for field weights -- a title match (weight A) ranks higher than a description match (weight B)
4. Results are returned with relevance scores

The search is **stage-aware**: anonymous users see only items on the Live stage. Authenticated users with appropriate permissions may see items on other stages.

### Search Results

The search results page at `/search?q={query}` shows:

- Query echo ("Results for: rust")
- Total result count
- Each result with title (linked), content type badge, and a text snippet
- Pagination
- Empty state when no results match

### Search API

The JSON search API is available at `/api/v1/search?q={query}`:

```bash
curl -s 'http://localhost:3000/api/v1/search?q=rust' | jq '{total: .total, first_title: .data[0].title}'
```

### Verify Search Weighting

Search for "rust" -- RustConf (title match, weight A) should appear near the top. Search for "berlin" -- conferences in Berlin (city match, weight C) should appear, ranked lower than conferences with "Berlin" in the title.

```bash
# Title match should rank high
curl -s 'http://localhost:3000/api/v1/search?q=rust' | jq '.data[0].title'
# Should be "RustConf 2026" or similar

# City match
curl -s 'http://localhost:3000/api/v1/search?q=berlin' | jq '.data | length'
# Should return > 0
```

<details>
<summary>Under the Hood: tsvector and Weighted Fields</summary>

Each item's `search_vector` column is a PostgreSQL `tsvector` built from the configured fields with their weights:

```sql
SELECT id, title, search_vector
FROM item
WHERE search_vector IS NOT NULL
LIMIT 1;
```

The vector looks like:

```
'2026':2B 'berlin':4C 'confer':3B 'develop':8B 'rust':1A 'rustconf':1A
```

The letter suffixes (A, B, C, D) correspond to the weight grades. PostgreSQL's `ts_rank_cd()` function uses these to score matches: a word with weight A contributes more to the relevance score than one with weight C.

The index uses a GIN (Generalized Inverted Index) for fast lookups:

```sql
CREATE INDEX idx_item_search_vector ON item USING gin(search_vector);
```

This gives you sub-millisecond search across thousands of items.

</details>

### Client-Side Search with Pagefind

The tsvector search above runs on the server. Trovato also supports **Pagefind** for instant client-side search — results appear as you type, with no server round-trip.

**How it works:**

1. A cron task (via the `trovato_search` plugin) detects content changes and rebuilds the Pagefind index at `static/pagefind/`
2. The search page loads `scolta.js`, which initializes Pagefind and provides client-side scoring
3. Scoring includes recency decay, title/content match boost, content type filters, and deduplication
4. If Pagefind isn't available (first install, index not yet built), the page falls back to tsvector server-side search

**Trigger an index rebuild:**

Pagefind indexes are built during cron runs. Trigger one manually:

```bash
curl -s -X POST http://localhost:3000/cron/default-cron-key | jq '.status'
```

After the index builds, visit `/search` — you'll see the Scolta search UI with instant client-side results.

> **Note:** Scolta also supports AI-powered search features (query expansion, streaming summaries, follow-up conversation) when the `trovato_ai` plugin is enabled. These are covered in the AI tutorial section (Epic 3: AI as a Building Block).

---

## Step 6: Theme & Visual Design

Steps 1-5 gave the site all its structural pieces -- templates, speakers, navigation, search. But the site still renders with the default base theme and has inline `<style>` blocks scattered across templates. This step consolidates all visual design into a single premium theme, adds a polished front page, creates content pages, and ensures visual consistency across the site.

### The Design System

The theme file at `static/css/theme.css` defines a complete design system using CSS custom properties (design tokens):

```css
:root {
    --primary: #6366f1;         /* Indigo/violet primary */
    --accent: #f59e0b;          /* Amber/orange accent */
    --teal: #14b8a6;            /* Teal for speakers */
    --rose: #f43f5e;            /* Rose for tags/topics */
    --gradient-primary: linear-gradient(135deg, #6366f1 0%, #8b5cf6 50%, #a78bfa 100%);
    --shadow-md: 0 12px 20px -4px rgba(0,0,0,0.08), ...;
    /* ... 40+ tokens for colors, radii, shadows, transitions, gradients */
}
```

Every component in the site references these tokens rather than hard-coding colors. Changing `--primary` rebrands the entire site.

The naming convention follows BEM (Block-Element-Modifier): `.card__title`, `.speaker-card__photo--placeholder`, `.site-nav__link--active`. This makes CSS classes self-documenting and avoids specificity wars.

### Consolidating Inline Styles

Trovato's default templates include inline `<style>` blocks for basic styling. The premium theme moves all public-facing styles into `theme.css`:

- `gather/query.html`, `gather/row.html`, `gather/pager.html` -- Gather chrome
- `gather/query--ritrovo.open_cfps.html` -- CFP card styles
- `gather/query--ritrovo.upcoming_conferences.html` -- Conference card styles
- `elements/comments.html` -- Comment thread styles

The `base.html` template retains its inline `<style>` as a safety net -- if `theme.css` fails to load, the page is still usable. Since `theme.css` loads after the inline block of `base.html`, it wins in the CSS cascade.

> **Accessibility note:** The base template provides several accessibility features by default. A **skip link** (`<a href="#main-content">Skip to main content</a>`) is the first focusable element on every page, letting keyboard users bypass navigation. The `<main>` element has `id="main-content"` as the skip link target. All item content renders inside `<article>` elements. The CSS uses **logical properties** (`margin-inline-start` instead of `margin-left`) so layouts work correctly for right-to-left languages. AJAX operations announce changes via an `aria-live="polite"` region so screen readers stay informed. These are infrastructure defaults -- you don't need to add them to your templates.

### The Front Page

The front page template at `templates/page--front.html` extends `page.html` and overrides two blocks:

- **`header`** -- Calls `{{ super() }}` to keep the site header, then adds a hero section with animated floating orbs, a grid overlay, stats bar, and two call-to-action buttons.
- **`content`** -- Four colorful stat cards (Conferences, Open CFPs, Speakers, Topics) and a split panel highlighting the technology stack.

The hero uses pure CSS animations (`@keyframes float`) and `backdrop-filter` for depth -- no JavaScript required.

[<img src="images/part-03/front-page.png" width="600" alt="Front page with animated hero section, stat cards, and technology showcase">](images/part-03/front-page.png)

### Shared Card Component

Conference listings, CFP listings, topic results, and speaker grids all render as card-based layouts. Without care, the card HTML and CSS would be duplicated across every gather query template.

The theme uses a two-layer pattern to eliminate this duplication:

**CSS base class (`theme.css`):** A shared `.card` class provides the container, hover animation, shadow, and border-radius. Variant modifier classes add type-specific accents:

- `.card--conf` -- Adds a gradient left border that fades in on hover
- `.card--cfp` -- Adds a solid amber left border
- `.card--speaker` -- Switches to flex layout for the photo + info pattern

Shared sub-elements (`.card__title`, `.card__meta`, `.card__dates`, `.card__location`, `.card__desc`, `.card__actions`, `.card__more`, `.card__website`) are defined once and used by all card types.

**Template include (`gather/includes/conf-card.html`):** The conference card HTML lives in a single include file. Every gather query that shows conferences uses:

```html
{% for row in rows %}
{% include "gather/includes/conf-card.html" %}
{% endfor %}
```

This means `query--ritrovo.upcoming_conferences.html`, `query--ritrovo.by_topic.html`, and any future conference listing query all share the same card markup. A change to the card layout is a single-file edit.

### Site-Wide Page Context

Every page that renders through Tera templates needs site context -- the site name, slogan, navigation menus, footer menus, authentication state, the `viewer` (`id`, `authenticated`, `roles`), and CSRF token. The `inject_site_context()` helper in `routes/helpers.rs` provides this. It must be called before rendering any page that extends `page.html`; a route that has already loaded its viewer calls `inject_site_context_for()` with it, so the viewer is loaded once.

The auth handlers (`login_form`, `render_login_error`, `render_register_form`, `render_profile`) all call `inject_site_context()` so that login, registration, and profile pages display the same site header, navigation, and footer as every other page. Without this, users navigating to `/user/login` would see a bare page with no navigation back to the site.

### About and Contact Pages

The About and Contact pages need rich layout HTML (grids with icons, multi-column cards). The `filtered_html` format's ammonia sanitization strips `class` and `style` attributes along with tags like `<svg>` -- so it cannot carry a rich layout. `full_html` (for an author with the `use full_html` permission) now keeps `div`, `span`, `figure`, tables, headings, lists, images and `class`/`id` on every tag, which covers a card grid, but it still has no `<svg>` or `style` attribute, so a layout that needs inline icons or CSS beyond what classes provide still needs a template.

The solution uses the template resolution chain. Instead of storing layout HTML in the database body field, create UUID-specific templates:

```
templates/elements/item--page--{uuid-of-about}.html
templates/elements/item--page--{uuid-of-contact}.html
```

These templates contain the rich layout HTML directly, bypassing the filter pipeline. The database items exist as `page` type content (so they have URL aliases and appear in admin), but the template handles all rendering. This approach:

- Respects the security model -- no weakening of the text format sanitizers
- Keeps rich layouts in version-controlled template files
- Uses Trovato's existing template specificity chain

### Topics: Alphabetical Sort

The topics browse page (`/topics`) originally displayed tags sorted by weight (their database ordering). For a directory site, alphabetical ordering is more intuitive. The `browse_topics` handler in `routes/category.rs` sorts tags after loading:

```rust
tags.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
```

### Gather Route Inline Rendering

Gather route aliases (e.g., `/topics/{slug}`) originally redirected with HTTP 307 to `/gather/{query_id}?param={value}`. This exposed ugly UUID-based URLs in the browser address bar. The updated `gather_routes.rs` handler calls `execute_and_render()` directly, rendering the gather query inline at the pretty URL. Visitors who click ".NET" on the topics page stay at `/topics/dotnet` and see conference cards -- no redirect, no UUID in the URL.

The by-topic query also gets its own template (`gather/query--ritrovo.by_topic.html`) that renders conference cards instead of a raw table, matching the visual style of the main conferences listing.

[<img src="images/part-03/topic-rust.png" width="600" alt="Topic page for Rust showing filtered conference cards at /topics/rust">](images/part-03/topic-rust.png)

### Hiding Duplicate Field Output

Item templates like `item--conference.html` explicitly render key fields (dates, location, links) in structured layouts. But the template also includes `{{ children | safe }}`, which renders **all** fields through the kernel's generic field renderer. This creates duplicate content -- dates appear in both the structured header and the raw field list.

The theme hides these duplicates with CSS:

```css
.item--conference .item__content .field,
.item--speaker .item__content .field {
    display: none;
}
```

This keeps the generic `children` output available (plugins can add fields that the template doesn't know about) while hiding the duplicate rendering of fields that the template already handles.

A template that lays out every field itself can skip `children` altogether and print `{{ plugin_output | safe }}` instead: that is every plugin's `tap_item_view` output on its own, the same string `children` ends with, so a plugin's contribution survives without the field dump. Item templates also get `viewer` (`id`, `authenticated` and `roles`, and nothing else about who is looking), so a template can link to `/user/{{ viewer.id }}/...` for a signed-in reader. See "The Viewer in the Render Context" in `docs/design/Design-Render-Theme.md`.

### Verify

```bash
# Front page has hero section
curl -s http://localhost:3000/ | grep -c 'hero__title'
# > 0

# Topics are alphabetical (first should be .NET)
curl -s http://localhost:3000/topics | grep -o 'topic-chip">[^<]*' | head -1
# topic-chip">.NET

# /topics/rust renders inline (200, not 307)
curl -s -o /dev/null -w "%{http_code}" http://localhost:3000/topics/rust
# 200

# /topics/rust shows conference cards, not a table
curl -s http://localhost:3000/topics/rust | grep -c 'card--conf'
# > 0

# About page renders with rich layout
curl -s http://localhost:3000/about | grep -c 'page-grid'
# > 0

# Login page has site header
curl -s http://localhost:3000/user/login | grep -c 'site-header'
# > 0
```

[<img src="images/part-03/gather-queries-admin.png" width="600" alt="Gather queries admin interface showing configured query definitions">](images/part-03/gather-queries-admin.png)

[<img src="images/part-03/search-config.png" width="600" alt="Search configuration admin showing field weights for conference and speaker types">](images/part-03/search-config.png)

[<img src="images/part-03/content-types-admin.png" width="600" alt="Content types admin showing the conference and speaker type definitions">](images/part-03/content-types-admin.png)

---

## What You've Built

By the end of Part 3, you have:

- **A premium visual theme** with a design token system, gradient accents, layered shadows, and smooth transitions -- all in a single `theme.css` file.
- **A shared card component** with a CSS base class (`.card`) and Tera template include (`gather/includes/conf-card.html`) so that all card-based listings share markup and styles without duplication.
- **A polished front page** with an animated hero section, feature cards, and technology showcase.
- **Styled templates** for conference detail pages, speaker profiles, CFP listings, and topic browsing -- all rendering through the safe Render Tree pipeline.
- **Content pages** (About, Contact) using UUID-specific templates to bypass filter pipeline limitations for rich layout HTML.
- **File uploads** with MIME validation, magic byte checking, and a temp-to-permanent lifecycle.
- **A second content type** (Speaker) linked to conferences via RecordReference fields, with automatic forward and reverse reference rendering.
- **A five-region page layout** with header, navigation, content, sidebar, and footer slots.
- **Navigation menus** with active highlighting and breadcrumbs.
- **Tile-based dynamic content** in sidebar and footer regions with path-based visibility rules.
- **Full-text search** with weighted fields, relevance ranking, and a search API.
- **Pretty topic URLs** that render conference cards inline at `/topics/{slug}` without redirects.

You also now understand:

- How CSS design tokens create a maintainable, rebrandable theme.
- How shared CSS base classes and Tera `{% include %}` eliminate template and style duplication across card-based listings.
- How the Render Tree pipeline ensures safe HTML output without plugins producing raw markup.
- How template resolution follows a specificity chain (item-specific → type-specific → default) and how UUID-specific templates solve the rich content problem.
- How `inject_site_context()` ensures consistent page chrome (header, nav, footer) across all pages including auth forms.
- How `safe_urls` prevents `javascript:` URI injection in template `href` attributes.
- How RecordReference fields create bidirectional relationships without denormalization.
- How PostgreSQL tsvector provides efficient full-text search with configurable field weights.
- How gather route aliases render inline for clean, user-friendly URLs.

The site now looks, navigates, and feels like a production application. Part 4 adds editorial discipline: users, roles, stages, workflows, and revision history.

---

## What's Deferred

| Feature | Deferred To | Reason |
|---|---|---|
| User authentication | Part 4 | Auth needed before roles and stages |
| Role-based tile visibility | Part 4 | Requires roles system |
| Stage-aware search filtering | Part 4 | Requires stages |
| WYSIWYG editor | Part 5 | Requires text format infrastructure |
| Comment system | Part 6 | Depends on users + permissions |
| Internationalization | Part 7 | Separate concern |
| Image import from confs.tech | Future | Importer doesn't fetch images; manual upload only |
| Config import for tiles/menus | Done | ConfigStorage supports tile and menu_link entity types |

---

## Related

- [Part 1: Hello, Trovato](part-01-hello-trovato.md)
- [Part 2: The Ritrovo Importer Plugin](part-02-ritrovo-importer.md)
- [Content Model Design](../design/Design-Content-Model.md)
- [Query Engine Design (Gather)](../design/Design-Query-Engine.md)
