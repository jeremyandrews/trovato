# Ritrovo Tutorial: Parts 3 & 4 Plan

## Part 3: Look & Feel

### Narrative

Part 2 ended with 5,000+ conferences flowing through five Gather queries, but
the site still renders everything as raw HTML tables. Part 3 transforms Ritrovo
from a data pipe into a real website.

The reader learns the **Render Tree** — the structured pipeline that turns
Items into HTML without ever letting plugins inject raw markup. They build Tera
templates for conference cards, detail pages, and a base layout with named
Slots. They wire up file uploads for logos and venue photos. They create a
second content type (Speaker) that references conferences through
RecordReference fields — and see the reverse references render automatically.
They configure Tiles that place dynamic Gather content into Sidebar slots, set
up main and footer navigation menus, and add full-text search with weighted
fields.

**Start state:** Raw HTML tables, no site chrome, no files, one content type.
**End state:** A polished, navigable conference directory with card-based
listings, speaker profiles, sidebar tiles, proper navigation, full-text search,
and file uploads for logos and photos.

---

### Tutorial Steps

#### Step 1: The Render Tree & Tera Templates

**Kernel features:** Render Tree (Build → Alter → Sanitize → Render), Tera
template engine, template suggestion resolution chain.

**What the reader does:**

1. **Understand the Render Tree pipeline.** Read an "Under the Hood" section
   explaining the four phases: Build (kernel/plugins construct a JSON
   RenderElement tree), Alter (`tap_item_view_alter` lets plugins mutate the
   tree), Sanitize (text formats filter HTML), Render (Tera templates convert
   the tree to HTML). Emphasize that plugins never produce raw HTML — they
   produce structured data that the kernel renders safely.

2. **Inspect the default rendering.** Visit `/item/{id}` for an existing
   conference. The kernel's fallback `item.html` template renders every field
   as a labeled row. Show the JSON RenderElement tree via the debug endpoint
   or by adding `?format=json` (if supported), or explain the structure
   conceptually.

3. **Create a conference detail template.** Write
   `templates/elements/item--conference.html` that extends `item.html` and
   overrides the content block. The template uses Tera variables (`title`,
   `fields.field_start_date`, `fields.field_city`, etc.) to render a styled
   conference detail page with a header, date range, location, description,
   and external link. Include CSS in a `<style>` block.

4. **Create a base page layout.** Modify `templates/page.html` (or create
   `templates/base.html` if it doesn't exist) to include a site header with
   the site name, navigation placeholder, content area, sidebar placeholder,
   and footer. This introduces the concept of page regions (Slots) that will
   be formally configured in Step 4.

5. **Improve the Gather results template.** The existing
   `query--ritrovo.upcoming_conferences.html` already renders conference cards.
   Enhance it with better CSS and ensure the other Gather queries
   (`open_cfps`, `by_topic`, etc.) either share this template or have their
   own. Create `query--ritrovo.open_cfps.html` with CFP-specific styling
   (deadline countdown, "Submit Talk" link).

6. **Template suggestion resolution.** Explain and demonstrate the override
   chain: `item--conference--{uuid}.html` → `item--conference.html` →
   `item.html`. Show that creating a more specific template automatically
   takes precedence. Same for Gather queries: `query--{query_id}.html` →
   `query.html`.

**Config files:** None for this step (templates are `.html` files, not config
entities).

**Template files:**
- `templates/elements/item--conference.html` — Conference detail page
- `templates/gather/query--ritrovo.open_cfps.html` — CFP listing
- Modifications to `templates/page.html` or `templates/base.html`

**Verification:**
- Visit `/item/{id}` — renders with conference-specific template
- Visit `/conferences` — card layout with improved styling
- Visit `/cfps` — CFP-specific template with deadline display

**Tests:**
- `trovato-test`: Conference detail page contains expected HTML structure
- `trovato-test`: Gather page uses query-specific template
- `trovato-test:internal`: Template suggestion chain resolves correctly

---

#### Step 2: File Uploads & Media

**Kernel features:** File upload endpoint, `file_managed` table, temp-to-permanent
lifecycle, MIME validation, magic byte checking, cron cleanup of orphaned temps.

**What the reader does:**

1. **Understand file field declarations.** The conference type defined in Part 1
   does not have file fields yet. Add `field_logo` (File type) and
   `field_venue_photo` (File type) to the conference item type via an updated
   YAML config. Re-import.

2. **Upload files via the admin UI.** Edit an existing conference (e.g.,
   RustConf 2026), upload a logo image. Show the file widget in the form,
   the upload progress, and the resulting file reference in the item's JSONB
   fields.

3. **Understand the file lifecycle.** Explain: uploaded file starts as
   temporary (status=0), gets a UUID filename in the temp directory. When the
   item is saved, referenced files are promoted to permanent (status=1) and
   moved to the permanent storage directory. Cron cleans up unreferenced
   temp files after 6 hours.

4. **Display files in templates.** Update `item--conference.html` to show the
   logo (if present) in the conference header and the venue photo in the
   detail body. Use the file's `uri` field to construct the `<img>` tag.

5. **File validation.** Demonstrate that uploading a non-image file (e.g., a
   `.exe` renamed to `.jpg`) is rejected. The kernel validates magic bytes
   against the declared MIME type. Show the allowed MIME types list.

6. **Verify via API.** `GET /api/item/{id}` shows the file field with
   `file_id`, `uri`, `filemime`, `filesize`. `GET /api/files/{file_id}`
   returns file metadata.

**Config files:**
- `item_type.conference.yml` — Updated with `field_logo` and `field_venue_photo`

**Template files:**
- Updated `templates/elements/item--conference.html` — Logo and photo display

**Verification:**
- Upload a logo → appears on conference detail page
- Upload a venue photo → appears in detail body
- `file_managed` table shows permanent file record
- Reject invalid file type → error message

**Tests:**
- `trovato-test`: Conference with logo renders `<img>` tag
- `trovato-test`: File upload creates `file_managed` record with status=1
- `trovato-test:internal`: Magic byte validation rejects disguised executables

---

#### Step 3: The Speaker Content Type

**Kernel features:** Second Item Type, RecordReference field type, forward and
reverse reference rendering, config import for item types.

**What the reader does:**

1. **Define the Speaker item type.** Create `item_type.speaker.yml` with fields:
   - `field_bio` (TextLong) — Speaker biography
   - `field_company` (Text) — Company/organization
   - `field_website` (Text) — Personal website URL
   - `field_photo` (File) — Headshot photo
   - `field_conferences` (RecordReference → conference, multi-value) — Conferences
     this speaker presents at

2. **Import the config.** `cargo run --release --bin trovato -- config import
   docs/tutorial/config` adds the speaker type.

3. **Create a few speakers manually.** Via the admin UI, create 3 speakers
   and link them to existing imported conferences using the RecordReference
   field. This demonstrates the reference picker widget.

4. **Create a speaker detail template.** Write
   `templates/elements/item--speaker.html` with photo, bio, company, website,
   and a "Conferences" section that lists linked conferences with links to
   their detail pages.

5. **Add a pathauto pattern for speakers.** Update `variable.pathauto_patterns.yml`
   to include `speaker: speakers/[title]`. Re-import and regenerate aliases.
   Speakers now have URLs like `/speakers/jane-doe`.

6. **Reverse references.** On the conference detail template, add a "Speakers"
   section that queries for speakers referencing this conference. This uses
   the Gather system or a reverse reference lookup. Show how the kernel can
   resolve "which speakers reference this conference" without the conference
   item knowing about speakers.

**Config files:**
- `item_type.speaker.yml` — Speaker content type definition
- `variable.pathauto_patterns.yml` — Updated with speaker pattern

**Template files:**
- `templates/elements/item--speaker.html` — Speaker detail page

**Verification:**
- `/api/content-types` includes `speaker`
- Create speaker → visible at `/speakers/jane-doe`
- Speaker detail page shows linked conferences
- Conference detail page shows linked speakers (reverse reference)

**Tests:**
- `trovato-test`: Speaker item type exists with expected fields
- `trovato-test`: Speaker alias resolves to speaker detail page
- `trovato-test`: Conference detail page shows speakers section
- `trovato-test:internal`: RecordReference stores target_id and target_type

---

#### Step 4: Slots, Tiles & Layout

**Kernel features:** Slot regions (named page areas), Tile entity (content blocks
placed in regions), Tile visibility rules, Gather-backed Tiles.

**What the reader does:**

1. **Understand Slots.** Slots are named regions in the page template: Header,
   Navigation, Content, Sidebar, Footer. The base template defines where each
   Slot renders. Tiles are assigned to Slots by machine name.

2. **Configure the base layout with Slots.** Update `templates/page.html` to
   render Slot regions using Tera blocks. Each region iterates over tiles
   assigned to it and renders them by weight order.

3. **Create Tiles via config import.** Define YAML config files for:
   - `tile.conferences_this_month` — Sidebar Tile backed by the
     `conferences_this_month` Gather query (shows condensed list of
     conferences happening this month)
   - `tile.open_cfps_sidebar` — Sidebar Tile backed by the `open_cfps`
     Gather query (top 5 CFPs closing soonest)
   - `tile.topic_cloud` — Sidebar Tile showing topic tags as a weighted
     cloud/list, linking to `/topics/{slug}`
   - `tile.site_branding` — Header Tile with site name and tagline
   - `tile.footer_info` — Footer Tile with copyright and links

4. **Tile visibility rules.** Configure `tile.open_cfps_sidebar` to only
   appear on conference-related pages (path patterns: `/conferences*`,
   `/topics/*`, `/cfps*`). Demonstrate that visiting `/speakers/jane-doe`
   does NOT show the CFP tile.

5. **Tile templates.** Create Tera templates for each Tile type. Gather-backed
   tiles use a condensed card format. The topic cloud tile iterates over
   taxonomy terms with their counts.

6. **Verify the layout.** Visit various pages and confirm that the sidebar
   shows the correct tiles, the header has branding, and the footer has
   info. Tiles appear in weight order within their region.

**Config files:**
- `tile.conferences_this_month.yml`
- `tile.open_cfps_sidebar.yml`
- `tile.topic_cloud.yml`
- `tile.site_branding.yml`
- `tile.footer_info.yml`

**Template files:**
- Updated `templates/page.html` — Slot regions
- `templates/tiles/gather-sidebar.html` — Condensed Gather tile
- `templates/tiles/topic-cloud.html` — Topic cloud tile
- `templates/tiles/site-branding.html` — Header branding
- `templates/tiles/footer-info.html` — Footer content

**Verification:**
- Sidebar shows "Conferences This Month" and "Open CFPs" tiles
- CFP tile hidden on speaker pages (visibility rule)
- Topic cloud links to `/topics/{slug}` routes
- Tiles render in weight order

**Tests:**
- `trovato-test`: Sidebar region contains expected tiles
- `trovato-test`: Tile visibility rule excludes tiles on non-matching paths
- `trovato-test:internal`: Tiles load from config with correct region and weight

---

#### Step 5: Menus & Navigation

**Kernel features:** Menu system, hierarchical menu items, active trail,
breadcrumbs, admin and user menu placeholders.

**What the reader does:**

1. **Define the main navigation menu.** Create config files for menu items:
   - Conferences (`/conferences`) — weight 0
   - Open CFPs (`/cfps`) — weight 10
   - Topics (parent item, no link) — weight 20
     - Child items for top-level topic categories (Languages, Infrastructure,
       etc.) linking to `/topics/{slug}`
   - Speakers (`/speakers`) — weight 30 (placeholder; Gather for speakers
     list comes later or skip if not needed)

2. **Define the footer menu.** Create config files:
   - About (placeholder, `/about`)
   - Contact (placeholder, `/contact`)

3. **Render menus in templates.** Update the Navigation Slot in the page
   template to render the main menu. Update the Footer Slot to render the
   footer menu. Show active menu item highlighting (current page gets an
   `active` class) and active trail (parent items highlighted when a child
   is active).

4. **Breadcrumbs.** Add breadcrumb rendering to the content area. Breadcrumbs
   are built from the menu hierarchy (Home → Conferences → RustConf 2026)
   and from the category hierarchy for topic pages (Home → Topics →
   Languages → Rust). Show both sources working together.

5. **Admin and User menu placeholders.** Note that the admin menu and user
   menu (Login/Register/My Account) are placeholders until Part 4 adds
   authentication. Show them as static links for now.

**Config files:**
- `menu.main.yml` — Main navigation menu definition
- `menu_link.conferences.yml` — Conferences link
- `menu_link.open_cfps.yml` — Open CFPs link
- `menu_link.topics.yml` — Topics parent
- `menu_link.topics_languages.yml` — Topics > Languages
- `menu_link.topics_infrastructure.yml` — Topics > Infrastructure
- `menu_link.topics_ai_data.yml` — Topics > AI & Data
- `menu_link.footer_about.yml` — Footer: About
- `menu_link.footer_contact.yml` — Footer: Contact
(Exact config entity format depends on kernel's menu config import support.)

**Template files:**
- Updated `templates/page.html` — Menu rendering in nav/footer slots
- `templates/macros/menu.html` — Menu rendering macro (ul/li with active class)
- `templates/macros/breadcrumb.html` — Breadcrumb rendering macro

**Verification:**
- Main nav shows Conferences, Open CFPs, Topics (with dropdown), Speakers
- Active page highlighted in nav
- Breadcrumbs show correct trail on conference detail and topic pages
- Footer menu renders About and Contact links

**Tests:**
- `trovato-test`: Main menu renders expected items
- `trovato-test`: Active trail highlights parent when on child page
- `trovato-test`: Breadcrumbs include category hierarchy on topic pages
- `trovato-test:internal`: Menu items load from config with hierarchy

---

#### Step 6: Full-Text Search

**Kernel features:** PostgreSQL tsvector, field weight configuration, search
index, relevance ranking, search results template.

**What the reader does:**

1. **Configure search fields.** Create a YAML config file defining which
   fields to index and their weights:
   - `title` → weight A (highest)
   - `field_description` → weight B
   - `field_city` → weight C
   - `field_country` → weight C
   - (Topic labels are denormalized or excluded for now)

2. **Import the config and rebuild the index.** Run config import, then
   trigger a search index rebuild via CLI or admin UI. The kernel updates
   the `search_vector` tsvector column on all items.

3. **Add a search box to the Header Slot.** Create a `tile.search_box`
   config placing a search form in the Header region. The form POSTs to
   `/search?q={query}`.

4. **Create a search results template.** Write `templates/search.html`
   (or verify the existing one) that shows search results with:
   - Query echo ("Results for: {query}")
   - Result count
   - Each result: title (linked), highlighted snippet, content type badge
   - Pagination
   - Empty state: "No results found for {query}"

5. **Test search weighting.** Search "rust" — conferences with "Rust" in the
   title (RustConf) should rank higher than conferences that mention Rust
   only in the description. Search "berlin" — city matches appear.

6. **Search API.** `GET /api/search?q=rust` returns JSON results with scores.
   Verify relevance ordering.

**Config files:**
- `variable.search_field_config.yml` — Field weights for search indexing
- `tile.search_box.yml` — Search box tile in Header slot

**Template files:**
- `templates/search.html` — Search results page (may already exist)
- `templates/tiles/search-box.html` — Search form widget

**Verification:**
- Search box appears in header on all pages
- Search "rust" → results with title matches ranked first
- Search "berlin" → city-matched conferences appear
- Empty search → "No results" message
- `/api/search?q=rust` → JSON with ranked results

**Tests:**
- `trovato-test`: Search returns results for known conference titles
- `trovato-test`: Title matches rank higher than description matches
- `trovato-test`: Search API returns JSON with score field
- `trovato-test:internal`: tsvector column populated with weighted fields

---

### BMAD Stories — Part 3

#### Story 3.1: Render Tree & Custom Templates

**As a** tutorial reader,
**I want** to understand and use the Render Tree pipeline with Tera templates,
**So that** I can control how conference data renders as HTML.

**Acceptance Criteria:**
- [ ] Conference detail page renders with `item--conference.html` template
- [ ] Base page layout has Header, Navigation, Content, Sidebar, Footer regions
- [ ] Open CFPs Gather uses a dedicated template with CFP-specific styling
- [ ] Template suggestion chain documented: `item--{type}--{id}` → `item--{type}` → `item`
- [ ] Under the Hood: Build → Alter → Sanitize → Render pipeline explained
- [ ] `trovato-test`: detail page contains `conf-detail` class
- [ ] `trovato-test:internal`: RenderElement JSON structure validated

#### Story 3.2: File Upload & Media Management

**As a** conference editor,
**I want** to upload logos and venue photos to conferences,
**So that** listings and detail pages include visual media.

**Acceptance Criteria:**
- [ ] Conference type updated with `field_logo` (File) and `field_venue_photo` (File)
- [ ] File upload widget in admin form accepts images
- [ ] Uploaded files stored in `file_managed` with status transition (temp → permanent)
- [ ] Conference detail template displays logo and venue photo
- [ ] Invalid file types rejected with clear error message
- [ ] `trovato-test`: upload creates permanent file record
- [ ] `trovato-test:internal`: magic byte validation rejects mismatched MIME

#### Story 3.3: Speaker Content Type with Relationships

**As a** tutorial reader,
**I want** to create a Speaker content type linked to conferences,
**So that** I learn RecordReference relationships and reverse reference rendering.

**Acceptance Criteria:**
- [ ] `item_type.speaker.yml` defines Speaker with bio, company, website, photo, conferences fields
- [ ] Speakers created via admin UI with conference references
- [ ] Speaker detail template shows linked conferences
- [ ] Conference detail template shows linked speakers (reverse reference)
- [ ] Pathauto generates `/speakers/{name}` aliases
- [ ] `trovato-test`: Speaker type has RecordReference field to conference
- [ ] `trovato-test`: reverse reference resolves on conference page

#### Story 3.4: Slot/Tile Layout Configuration

**As a** site builder,
**I want** to place dynamic content blocks (Tiles) into page regions (Slots),
**So that** the site has a consistent layout with contextual sidebar content.

**Acceptance Criteria:**
- [ ] Five Tiles created via config import (branding, 2× sidebar, topic cloud, footer)
- [ ] Sidebar tiles render Gather results in condensed format
- [ ] Topic cloud tile links to `/topics/{slug}` routes
- [ ] Tile visibility rules: CFP tile only on conference-related paths
- [ ] Tiles render in weight order within their region
- [ ] `trovato-test`: sidebar region contains expected tile machine names
- [ ] `trovato-test:internal`: visibility rule excludes tile on non-matching path

#### Story 3.5: Menu System & Navigation

**As a** site visitor,
**I want** main navigation, footer links, and breadcrumbs,
**So that** I can navigate the site intuitively.

**Acceptance Criteria:**
- [ ] Main menu with Conferences, Open CFPs, Topics (with children), Speakers
- [ ] Footer menu with About and Contact placeholders
- [ ] Active menu item highlighted on current page
- [ ] Active trail highlights parent when on child page
- [ ] Breadcrumbs from menu hierarchy and category hierarchy
- [ ] Admin/User menu placeholders noted (functional in Part 4)
- [ ] `trovato-test`: nav contains expected menu items
- [ ] `trovato-test`: breadcrumbs include category path on topic pages

#### Story 3.6: Full-Text Search

**As a** site visitor,
**I want** to search for conferences by keyword,
**So that** I can find conferences matching my interests.

**Acceptance Criteria:**
- [ ] Search field config imported (title=A, description=B, city/country=C)
- [ ] Search index rebuilt after config import
- [ ] Search box tile in Header slot on all pages
- [ ] Search results page with relevance ranking and highlighted snippets
- [ ] Title matches rank higher than description matches
- [ ] Search API (`/api/search?q=`) returns JSON with scores
- [ ] Empty state: "No results found"
- [ ] `trovato-test`: search "rust" returns results with RustConf ranked high
- [ ] `trovato-test:internal`: tsvector column has weighted fields

---

### Config Files Inventory — Part 3

| File | Entity Type | Purpose |
|------|-------------|---------|
| `item_type.conference.yml` | item_type | **Updated:** add field_logo, field_venue_photo |
| `item_type.speaker.yml` | item_type | Speaker content type definition |
| `variable.pathauto_patterns.yml` | variable | **Updated:** add speaker pattern |
| `tile.site_branding.yml` | tile | Header: site name and tagline |
| `tile.search_box.yml` | tile | Header: search form |
| `tile.conferences_this_month.yml` | tile | Sidebar: this month's conferences |
| `tile.open_cfps_sidebar.yml` | tile | Sidebar: closing CFPs (with visibility rule) |
| `tile.topic_cloud.yml` | tile | Sidebar: topic tag cloud |
| `tile.footer_info.yml` | tile | Footer: copyright and links |
| `menu.main.yml` | menu | Main navigation definition |
| `menu.footer.yml` | menu | Footer navigation definition |
| `menu_link.conferences.yml` | menu_link | Main nav: Conferences |
| `menu_link.open_cfps.yml` | menu_link | Main nav: Open CFPs |
| `menu_link.topics.yml` | menu_link | Main nav: Topics parent |
| `menu_link.topics_languages.yml` | menu_link | Main nav: Topics > Languages |
| `menu_link.topics_infrastructure.yml` | menu_link | Main nav: Topics > Infrastructure |
| `menu_link.topics_ai_data.yml` | menu_link | Main nav: Topics > AI & Data |
| `menu_link.footer_about.yml` | menu_link | Footer: About |
| `menu_link.footer_contact.yml` | menu_link | Footer: Contact |
| `variable.search_field_config.yml` | variable | Search field weights |

**Note:** The exact entity types for menu and tile config import depend on
what the kernel's config storage layer supports. If menus/tiles aren't yet
config-importable, the tutorial step must either (a) add that support as a
kernel enhancement, or (b) use admin UI + document the config structure.
Verify kernel support before writing.

---

### Template Files Inventory — Part 3

| File | Purpose |
|------|---------|
| `templates/page.html` | **Modified:** Slot regions, menu rendering |
| `templates/elements/item--conference.html` | Conference detail page |
| `templates/elements/item--speaker.html` | Speaker detail page |
| `templates/gather/query--ritrovo.open_cfps.html` | CFP listing template |
| `templates/tiles/gather-sidebar.html` | Condensed Gather result for sidebar |
| `templates/tiles/topic-cloud.html` | Topic cloud widget |
| `templates/tiles/site-branding.html` | Header branding |
| `templates/tiles/search-box.html` | Search form widget |
| `templates/tiles/footer-info.html` | Footer content |
| `templates/macros/menu.html` | Menu rendering macro |
| `templates/macros/breadcrumb.html` | Breadcrumb rendering macro |

---

### Recipe Outline — Part 3

```
# Recipe: Part 3 — Look & Feel

> Synced with: docs/tutorial/part-03-look-and-feel.md
> Sync hash: (generated)
> Last verified: (date)

## Prerequisites
- Parts 1 and 2 completed
- Check TOOLS.md for server start, config import commands
- Database backup recommended (see TOOLS.md § Backups)

## Step 1: Render Tree & Templates
### 1.1 [REFERENCE] Read Render Tree architecture
### 1.2 [CLI] Inspect default rendering via curl
### 1.3 [CLI] Create item--conference.html template
### 1.4 [CLI] Update page.html with slot regions
### 1.5 [CLI] Create query--ritrovo.open_cfps.html
### 1.6 [CLI] Verify template resolution
    Expect: Conference detail renders with new template
    Record template paths in TOOLS.md § Templates

## Step 2: File Uploads
### 2.1 [CLI] Update item_type.conference.yml with file fields
### 2.2 [CLI] Config import
### 2.3 [UI-ONLY] Upload logo via admin form
### 2.4 [CLI] Verify file_managed record
### 2.5 [CLI] Update item--conference.html with image rendering
### 2.6 [CLI] Test invalid file rejection
    Record file upload endpoints in TOOLS.md § Files/Media

## Step 3: Speaker Content Type
### 3.1 [CLI] Create item_type.speaker.yml
### 3.2 [CLI] Config import
### 3.3 [UI-ONLY] Create 3 speakers with conference references
### 3.4 [CLI] Create item--speaker.html template
### 3.5 [CLI] Update pathauto patterns, regenerate aliases
### 3.6 [CLI] Verify speaker detail page and reverse references

## Step 4: Slots & Tiles
### 4.1 [REFERENCE] Read Slot/Tile architecture
### 4.2 [CLI] Create tile config YAML files
### 4.3 [CLI] Config import tiles
### 4.4 [CLI] Create tile templates
### 4.5 [CLI] Update page.html to render tiles per slot
### 4.6 [CLI] Verify tile visibility rules
### 4.7 [UI-ONLY] Browse site, confirm sidebar/header/footer tiles

## Step 5: Menus & Navigation
### 5.1 [CLI] Create menu and menu_link config files
### 5.2 [CLI] Config import menus
### 5.3 [CLI] Create menu and breadcrumb macros
### 5.4 [CLI] Update page.html with menu rendering
### 5.5 [CLI] Verify active trail and breadcrumbs
### 5.6 [UI-ONLY] Navigate site, confirm menu behavior

## Step 6: Full-Text Search
### 6.1 [CLI] Create search field config YAML
### 6.2 [CLI] Config import and rebuild search index
### 6.3 [CLI] Create search box tile config
### 6.4 [CLI] Verify search results via API
### 6.5 [CLI] Verify search weighting (title > description)
### 6.6 [UI-ONLY] Search from header, verify results page

## Completion Checklist
    [CLI] Verify all templates, tiles, menus, search
    [CLI] Create database backup
    Record backup in TOOLS.md § Backups
```

---

### What's Deferred — Part 3

| Feature | Deferred To | Reason |
|---------|-------------|--------|
| User authentication | Part 4 | Auth needed before roles/stages |
| Role-based tile visibility | Part 4 | Requires roles system |
| Stage-aware search | Part 4 | Requires stages |
| WYSIWYG editor | Part 5 | Requires text format infrastructure |
| Comment system | Part 6 | Depends on users + permissions |
| Internationalization | Part 7 | Separate concern |
| Image import from confs.tech | Future | Importer doesn't fetch images; manual upload only |
| Speaker Gather query | Future/Part 3 stretch | List all speakers page (simple to add but not core) |
| Admin menu | Part 4 | Functional admin nav requires auth |

---
---

## Part 4: The Editorial Engine

### Narrative

Part 3 gave Ritrovo its visual identity. Part 4 gives it editorial
discipline. The reader builds a multi-user CMS with roles, stages, workflows,
and revision history.

The tutorial introduces users as Items (everything is an Item — this is
architecturally important), then builds five roles from anonymous to admin.
The reader writes their second WASM plugin (`ritrovo_access`) implementing
`tap_item_access` for grant/deny/neutral access control and `tap_item_view`
for field-level stripping (hiding `editor_notes` from non-editors). They
configure three stages (Incoming, Curated, Live) with a required workflow
defining valid transitions. They walk through all five revision scenarios
from the design spec. They make Gathers and search stage-aware. And they
build out the admin UI with stage filters, bulk operations, and import queue
management.

**Start state:** Single anonymous user, everything on Live stage, no access
control.
**End state:** Five roles, three stages, directed workflow transitions, full
revision history, stage-aware content visibility, a second WASM plugin, and
a proper admin content management interface.

---

### Tutorial Steps

#### Step 1: Users & Authentication

**Kernel features:** User as Item, registration, login/logout, Argon2id
password hashing, session management (Redis), session fixation protection.

**What the reader does:**

1. **Understand Users as Items.** Explain that users in Trovato are stored in
   the `users` table (not as generic Items in this implementation), but they
   follow the same patterns: UUID IDs, JSONB `data` field for extensions,
   timestamps, and they integrate with the permission system. The admin user
   created during install in Part 1 already exists.

2. **Enable registration.** Configure whether registration is open, admin-only,
   or closed. For the tutorial, set it to open (with admin approval — a
   config variable).

3. **Register test users.** Via the registration form at `/user/register`,
   create users for each editorial role:
   - `editor_alice` — will become an editor
   - `publisher_bob` — will become a publisher
   - `viewer_carol` — stays as authenticated user
   These users demonstrate the role system in later steps.

4. **Login/logout flow.** Demonstrate the login form at `/user/login`,
   session creation, the user menu showing "My Account" and "Logout",
   and the logout POST (never GET — CSRF protection).

5. **User profile page.** Visit `/user/{username}` to see the public profile.
   Show that the profile template (`templates/user/profile.html`) can be
   customized.

6. **Session architecture.** Under the Hood: Redis session storage, session
   fixation protection (cycle session ID after auth state change), Argon2id
   parameters, minimum password length (12 chars).

**Config files:**
- `variable.user_registration.yml` — Registration mode (open with approval)

**Template files:**
- Modifications to `templates/user/profile.html` (if needed)
- Update user menu in page template

**Verification:**
- Register three test users → accounts created
- Login as `editor_alice` → session established, user menu shows name
- Logout → session destroyed, redirected to login
- `/user/editor_alice` → public profile page

**Tests:**
- `trovato-test`: Registration creates user account
- `trovato-test`: Login establishes session, logout destroys it
- `trovato-test`: Profile page renders for authenticated user
- `trovato-test:internal`: Password stored as Argon2id hash

---

#### Step 2: Roles & the ritrovo_access Plugin

**Kernel features:** Role model, permission system, `tap_perm`, `tap_item_access`
(Grant/Deny/Neutral aggregation), field-level access via render tree.

**What the reader does:**

1. **Define five roles via config import.**
   - `anonymous` (well-known UUID, already exists)
   - `authenticated` (well-known UUID, already exists)
   - `editor` — can view all stages, edit items, see editor_notes
   - `publisher` — editor permissions + can promote items to Live
   - `admin` (superuser flag, already exists)

2. **Assign permissions to roles.** Config files map permissions to roles:
   - `anonymous`: "access content" (view Live items only)
   - `authenticated`: "access content"
   - `editor`: "access content", "edit any conference", "edit any speaker",
     "view internal content", "use editorial workflow"
   - `publisher`: all editor permissions + "publish content",
     "unpublish content"

3. **Assign roles to test users.** Via admin UI:
   - `editor_alice` → editor role
   - `publisher_bob` → publisher role
   - `viewer_carol` → no extra roles (authenticated only)

4. **Build the ritrovo_access plugin.** Scaffold a second WASM plugin:
   ```
   cargo run --release --bin trovato -- plugin new ritrovo_access
   ```
   Implement:
   - `tap_perm` — Declares permissions: "edit any conference",
     "edit any speaker", "view internal content", "publish content",
     "unpublish content", "use editorial workflow"
   - `tap_item_access` — Returns Grant for editors/publishers viewing
     internal-stage items, Neutral for everything else. The kernel
     aggregates: any Grant + no Deny = access. Any Deny = no access.
     All Neutral = fall back to default (deny for internal, allow for
     public).
   - `tap_item_view` — Strips `editor_notes` field from the render tree
     for users without the "edit any conference" permission. This
     demonstrates field-level access control through render tree
     manipulation — directly building on Part 3's Render Tree knowledge.

5. **Build, install, and test the plugin.**
   ```
   cargo build --target wasm32-wasip1 -p ritrovo_access --release
   cargo run --release --bin trovato -- plugin install ritrovo_access
   ```

6. **Verify access control.** Login as each user and confirm:
   - `viewer_carol` sees Live items only, no `editor_notes`
   - `editor_alice` sees all stages, sees `editor_notes`
   - `publisher_bob` sees all stages, can change item stage
   - Anonymous sees Live items only

**Config files:**
- `role.editor.yml` — Editor role definition with permissions
- `role.publisher.yml` — Publisher role definition with permissions

**Plugin files:**
- `plugins/ritrovo_access/ritrovo_access.info.toml`
- `plugins/ritrovo_access/src/lib.rs`
- `plugins/ritrovo_access/Cargo.toml`

**Verification:**
- Anonymous: can view `/conferences`, cannot see editor_notes
- editor_alice: can view internal items, sees editor_notes
- publisher_bob: all editor powers + stage transitions
- viewer_carol: same as anonymous (plus authenticated perms)

**Tests:**
- `trovato-test`: Anonymous cannot see internal-stage items
- `trovato-test`: Editor can view internal-stage items
- `trovato-test`: editor_notes stripped from render tree for non-editors
- `trovato-test:internal`: tap_item_access returns correct Grant/Neutral

---

#### Step 3: Stages & Workflows

**Kernel features:** Vocabulary-based stage system, StageVisibility enum
(Internal/Public/Accessible), directed workflow transitions, stage-aware
Gathers, stage-aware search.

**What the reader does:**

1. **Define three stages via config import.** Stages are tags in the `stages`
   category:
   - `incoming` — visibility: Internal, weight: 0, default for new items
   - `curated` — visibility: Internal, weight: 10
   - `live` — visibility: Public, weight: 20 (well-known UUID)

2. **Define the editorial workflow.** Config file specifying valid transitions
   as a directed graph:
   - `incoming` → `curated` (requires "use editorial workflow")
   - `curated` → `live` (requires "publish content")
   - `live` → `curated` (requires "unpublish content")
   - `curated` → `incoming` (requires "use editorial workflow")
   Invalid transitions are rejected by the kernel.

3. **Update the import pipeline.** The `ritrovo_importer` plugin currently
   creates all conferences on the Live stage. Modify it so new imports land
   on the `incoming` stage instead. This means imported conferences are not
   publicly visible until an editor promotes them.

4. **Walk through the editorial workflow.**
   - Login as `editor_alice`
   - View the content list filtered by Incoming stage — see newly imported
     conferences
   - Select a conference, review it, promote to Curated
   - Login as `publisher_bob`
   - View Curated items, publish to Live
   - Verify: the conference is now visible to anonymous users

5. **Stage-aware Gathers.** The five Gather queries from Part 2 already have
   `stage_aware: true` in their definitions. Demonstrate that:
   - Anonymous users see only Live conferences on `/conferences`
   - Editors see Incoming + Curated + Live when logged in
   - The Gather engine wraps queries with a CTE that filters by stage
     visibility per role

6. **Stage-aware search.** Verify that searching as an anonymous user returns
   only Live content. Editors see results from all stages they can access.

7. **Extensibility demo.** Add a "Legal Review" stage between Curated and
   Live as a **config-only change**:
   - Create `stage.legal_review.yml` with visibility: Internal, weight: 15
   - Update workflow config to add transitions:
     `curated → legal_review`, `legal_review → live`
   - Remove direct `curated → live` transition
   - Re-import config. No code changes, no plugin rebuild.
   - Demonstrate the new workflow path works

**Config files:**
- `category.stages.yml` — Stages category (if not pre-existing)
- `tag.{uuid}.incoming.yml` — Incoming stage
- `tag.{uuid}.curated.yml` — Curated stage
- `tag.{uuid}.live.yml` — Live stage (well-known UUID)
- `variable.workflow.editorial.yml` — Workflow transition graph
- `tag.{uuid}.legal_review.yml` — Extensibility demo stage

**Verification:**
- New imports land on Incoming (not Live)
- Editor promotes Incoming → Curated → Live
- Invalid transition (Incoming → Live) rejected
- Anonymous sees only Live on `/conferences`
- Adding Legal Review stage works without code changes

**Tests:**
- `trovato-test`: New items created on Incoming stage
- `trovato-test`: Valid stage transition succeeds
- `trovato-test`: Invalid stage transition rejected
- `trovato-test`: Anonymous Gather excludes Internal items
- `trovato-test`: Editor Gather includes Internal items
- `trovato-test`: Search results filtered by stage visibility
- `trovato-test:internal`: Workflow validates transition permissions

---

#### Step 4: Revisions

**Kernel features:** item_revision table, current_revision_id, revert, revision
log, draft-while-live, cross-stage field updates, emergency unpublish.

**What the reader does:**

Walk through all five revision scenarios from the design spec:

1. **Basic revision.** Edit a Live conference (change the description).
   Verify: `item_revision` table has a new row, `item.current_revision_id`
   updated, previous revision preserved. View revision history via admin UI
   or API.

2. **Revert.** Make a bad edit to a Live conference (change title to
   "WRONG"). Revert to the previous revision. Verify: revert creates a NEW
   revision (not a delete), the title is restored, revision history shows
   three entries (original, bad edit, revert).

3. **Stage a new version (draft-while-live).** A Live conference has a
   published version visible to the public. An editor creates a Curated
   draft with significant changes. Verify: the public still sees the Live
   version, the editor sees the Curated draft when in that stage context,
   and publishing the draft replaces the Live version.

4. **Cross-stage field updates.** The importer runs and updates structured
   fields (start_date, end_date) on a Live conference that also has a
   Curated draft. Verify: the kernel writes ONE revision, and the
   `tap_item_save` context includes `other_stage_revisions` so the plugin
   is aware of the draft.

5. **Emergency unpublish.** Set `active = false` on a Live conference's
   revision. Verify: the item immediately disappears from public Gathers
   and search without going through a stage transition. This is a safety
   valve for urgent content removal.

**Config files:** None (revisions are kernel behavior, not configuration).

**Verification:**
- Revision history shows all edits with timestamps and log messages
- Revert creates new revision, doesn't delete old ones
- Draft-while-live: public sees old version until publish
- Cross-stage update writes single revision
- Emergency unpublish removes item from public view immediately

**Tests:**
- `trovato-test`: Edit creates new revision row
- `trovato-test`: Revert creates new revision with old content
- `trovato-test`: Draft revision doesn't affect public view
- `trovato-test`: Emergency unpublish excludes item from Gathers
- `trovato-test:internal`: current_revision_id tracks active revision

---

#### Step 5: Admin UI Buildout

**Kernel features:** Content list with filters, bulk operations, stage-based
content management, role-based tile visibility.

**What the reader does:**

1. **Content list with filters.** The admin content list at
   `/admin/content` gains:
   - Stage filter (dropdown: All / Incoming / Curated / Live)
   - Type filter (Conference / Speaker)
   - Author filter
   - Date range filter
   - Text search within title

2. **Bulk operations.** Select multiple items via checkboxes, then:
   - Change stage (e.g., promote 10 Curated items to Live)
   - Toggle status (publish/unpublish)
   Bulk operations respect workflow rules and permissions.

3. **Import queue management.** Admin page showing items in Incoming stage
   with importer metadata (source, import date). Quick approve (→ Curated)
   and reject (→ delete or archive) actions.

4. **Role-based tile visibility.** Upgrade Part 3's path-based tile
   visibility to also support role-based rules. Configure:
   - "Editor Tools" sidebar tile visible only to editors and above
   - Shows quick links to content list, pending imports, recent edits

5. **Category management in admin.** Verify the category/tag admin pages
   work with the new slug field (from the recent commit). Show that adding
   a new topic tag is immediately available in Gather filters and the topic
   cloud tile.

**Config files:**
- `tile.editor_tools.yml` — Editor-only sidebar tile

**Template files:**
- Updated admin content list template with filters
- Bulk operation confirmation template
- Import queue management template
- `templates/tiles/editor-tools.html` — Editor tools tile

**Verification:**
- Content list filters work (stage, type, author, date)
- Bulk stage change respects workflow permissions
- Import queue shows Incoming items with quick actions
- Editor tools tile visible to editors, hidden from viewers

**Tests:**
- `trovato-test`: Content list filters by stage correctly
- `trovato-test`: Bulk stage change only for authorized users
- `trovato-test`: Role-based tile visibility
- `trovato-test:internal`: Bulk operation respects workflow transitions

---

### BMAD Stories — Part 4

#### Story 4.1: Users & Authentication

**As a** site visitor,
**I want** to register, log in, and manage my profile,
**So that** I have an identity for editorial actions.

**Acceptance Criteria:**
- [ ] Registration form creates user account (open with approval mode)
- [ ] Login establishes Redis session with HttpOnly cookie
- [ ] Logout destroys session via POST (never GET)
- [ ] Session ID cycled after login (fixation protection)
- [ ] Public profile at `/user/{username}`
- [ ] User menu shows name, My Account, Logout when authenticated
- [ ] `trovato-test`: registration creates user record
- [ ] `trovato-test:internal`: password stored as Argon2id

#### Story 4.2: Roles & Access Control Plugin

**As a** site administrator,
**I want** five roles with permission-based access control,
**So that** different users have appropriate content access.

**Acceptance Criteria:**
- [ ] Editor and publisher roles created via config import
- [ ] `ritrovo_access` plugin declares permissions via `tap_perm`
- [ ] `tap_item_access` returns Grant for editors viewing internal items
- [ ] `tap_item_view` strips `editor_notes` for non-editors
- [ ] Role assignment via admin UI
- [ ] Anonymous sees only Live/Public content
- [ ] Editor sees Internal + Public content
- [ ] `trovato-test`: access control enforced per role
- [ ] `trovato-test:internal`: Grant/Deny/Neutral aggregation correct

#### Story 4.3: Stages & Workflows

**As an** editor,
**I want** a staged editorial workflow with enforced transitions,
**So that** content goes through review before publication.

**Acceptance Criteria:**
- [ ] Three stages: Incoming (Internal), Curated (Internal), Live (Public)
- [ ] Workflow defines valid transitions as directed graph
- [ ] Invalid transitions rejected with error
- [ ] Imports land on Incoming stage
- [ ] Editor promotes Incoming → Curated, Publisher promotes Curated → Live
- [ ] Extensibility: "Legal Review" stage added as config-only change
- [ ] Stage-aware Gathers filter by visibility per role
- [ ] Stage-aware search excludes internal content for anonymous
- [ ] `trovato-test`: workflow enforces valid transitions
- [ ] `trovato-test:internal`: CTE wraps Gather query with stage filter

#### Story 4.4: Revision History

**As an** editor,
**I want** full revision tracking with revert and draft-while-live,
**So that** no edit is ever lost and drafts don't affect live content.

**Acceptance Criteria:**
- [ ] Every edit creates new revision in `item_revision`
- [ ] Revision history viewable with timestamps and log messages
- [ ] Revert creates NEW revision (never deletes)
- [ ] Draft-while-live: Curated draft invisible to public
- [ ] Cross-stage update writes single revision with awareness context
- [ ] Emergency unpublish: `active=false` removes from public immediately
- [ ] `trovato-test`: five scenarios from design spec verified
- [ ] `trovato-test:internal`: current_revision_id tracks correctly

#### Story 4.5: Admin UI Buildout

**As an** administrator,
**I want** a content management interface with filters and bulk operations,
**So that** I can efficiently manage thousands of conferences.

**Acceptance Criteria:**
- [ ] Content list filterable by stage, type, author, date
- [ ] Bulk stage change for selected items
- [ ] Bulk operations respect workflow permissions
- [ ] Import queue page for Incoming items with approve/reject
- [ ] Role-based tile visibility (editor tools tile)
- [ ] `trovato-test`: filters return correct content subset
- [ ] `trovato-test:internal`: bulk operation validates per-item permissions

---

### Config Files Inventory — Part 4

| File | Entity Type | Purpose |
|------|-------------|---------|
| `variable.user_registration.yml` | variable | Registration mode |
| `role.editor.yml` | role | Editor role with permissions |
| `role.publisher.yml` | role | Publisher role with permissions |
| `category.stages.yml` | category | Stages vocabulary |
| `tag.{uuid}.incoming.yml` | tag | Incoming stage (Internal) |
| `tag.{uuid}.curated.yml` | tag | Curated stage (Internal) |
| `tag.{uuid}.live.yml` | tag | Live stage (Public, well-known UUID) |
| `tag.{uuid}.legal_review.yml` | tag | Legal Review stage (extensibility demo) |
| `variable.workflow.editorial.yml` | variable | Workflow transition graph |
| `tile.editor_tools.yml` | tile | Editor-only sidebar tile |

**Note:** The Live stage with its well-known UUID may already exist in the
system. The config import should be idempotent (update if exists).

---

### Template Files Inventory — Part 4

| File | Purpose |
|------|---------|
| `templates/user/profile.html` | **Modified:** enhanced public profile |
| `templates/tiles/editor-tools.html` | Editor tools sidebar |
| Admin templates for content list filters | **Modified:** stage/type filters |
| Admin templates for bulk operations | Confirmation dialogs |
| Admin templates for import queue | Incoming item management |

---

### Recipe Outline — Part 4

```
# Recipe: Part 4 — The Editorial Engine

> Synced with: docs/tutorial/part-04-editorial-engine.md
> Sync hash: (generated)
> Last verified: (date)

## Prerequisites
- Parts 1, 2, and 3 completed
- Check TOOLS.md for server start, config import, plugin build commands
- Database backup recommended (see TOOLS.md § Backups)

## Step 1: Users & Authentication
### 1.1 [REFERENCE] Read Users architecture
### 1.2 [CLI] Configure registration mode
### 1.3 [CLI] Register three test users via curl
### 1.4 [CLI] Test login/logout flow via curl
### 1.5 [CLI] Verify session in Redis
### 1.6 [CLI] Verify user profile page
    Record user creation commands in TOOLS.md § Roles & Access

## Step 2: Roles & ritrovo_access Plugin
### 2.1 [CLI] Create role config YAML files
### 2.2 [CLI] Config import roles
### 2.3 [CLI] Scaffold ritrovo_access plugin
### 2.4 [CLI] Implement tap_perm, tap_item_access, tap_item_view
### 2.5 [CLI] Build and install plugin
### 2.6 [CLI] Assign roles to test users
### 2.7 [CLI] Test access control per role via curl
    Record role testing commands in TOOLS.md § Roles & Access

## Step 3: Stages & Workflows
### 3.1 [CLI] Create stage config YAML files
### 3.2 [CLI] Create workflow config
### 3.3 [CLI] Config import stages and workflow
### 3.4 [CLI] Update ritrovo_importer to target Incoming stage
### 3.5 [CLI] Build and install updated importer
### 3.6 [CLI] Verify imports land on Incoming
### 3.7 [CLI] Walk through editorial workflow (promote Incoming→Curated→Live)
### 3.8 [CLI] Verify stage-aware Gathers
### 3.9 [CLI] Verify stage-aware search
### 3.10 [CLI] Extensibility demo: add Legal Review stage
    Record stage/workflow commands in TOOLS.md § Stages & Workflows

## Step 4: Revisions
### 4.1 [CLI] Scenario 1: Basic revision (edit Live item)
### 4.2 [CLI] Scenario 2: Revert (bad edit, restore)
### 4.3 [CLI] Scenario 3: Draft-while-live
### 4.4 [CLI] Scenario 4: Cross-stage field update
### 4.5 [CLI] Scenario 5: Emergency unpublish
### 4.6 [CLI] Verify revision history via API
    Record revision inspection commands in TOOLS.md § Revisions

## Step 5: Admin UI Buildout
### 5.1 [CLI] Verify content list filters (stage, type, author)
### 5.2 [CLI] Test bulk stage change
### 5.3 [CLI] Verify import queue management
### 5.4 [CLI] Create editor tools tile config
### 5.5 [CLI] Verify role-based tile visibility
### 5.6 [UI-ONLY] Browse admin UI as each role

## Completion Checklist
    [CLI] Verify all roles, stages, workflow, revisions, access control
    [CLI] Create database backup
    Record backup in TOOLS.md § Backups
```

---

### What's Deferred — Part 4

| Feature | Deferred To | Reason |
|---------|-------------|--------|
| WYSIWYG editor | Part 5 | Rich text editing is a form enhancement |
| AJAX form interactions | Part 5 | Progressive enhancement |
| Comments | Part 6 | Depends on full user system being stable |
| User subscriptions/notifications | Part 6 | Depends on comments |
| Internationalization | Part 7 | Separate concern |
| REST API authentication (tokens) | Part 5+ | API auth is separate from session auth |
| Revision diff UI | Part 5+ | Visual diff display is a UI enhancement |
| Content scheduling | Future | Time-based stage transitions |
| Multi-site | Future | Out of scope for Ritrovo |

---
---

## Cross-Part Integration Notes

### Dependency Chain

Part 3 has no dependency on Part 4 features. Part 4 depends on Part 3:
- Render Tree knowledge needed for `tap_item_view` field stripping
- Templates needed for stage-aware rendering differences
- Tiles needed for role-based visibility upgrade
- Search needed for stage-aware search demonstration

### Config Import Cumulative State

After Part 3, config import covers approximately:
- 2 item types (conference updated, speaker new)
- 1 category + 32 tags
- 5 gather queries
- 2 URL aliases
- 1 pathauto patterns variable (updated)
- ~5 tiles
- ~2 menus + ~9 menu links
- 1 search config variable
- **Total: ~60 config entities**

After Part 4, add approximately:
- 2 roles
- 1 stages category + 3-4 stage tags
- 1 workflow variable
- 1 registration variable
- 1 tile (editor tools)
- **Total: ~70 config entities**

### Plugin Inventory

| Plugin | Part | Taps |
|--------|------|------|
| `ritrovo_importer` | 2 (updated in 4) | tap_install, tap_cron, tap_queue_info, tap_queue_worker |
| `ritrovo_access` | 4 | tap_perm, tap_item_access, tap_item_view |

### Template Inventory Growth

| Part | Templates Added | Cumulative |
|------|----------------|------------|
| 1-2 | ~2 gather templates | ~2 |
| 3 | ~11 (detail, tiles, macros, search) | ~13 |
| 4 | ~3 (editor tools, admin updates) | ~16 |

### Database Backup Snapshots

| Snapshot | Contents |
|----------|----------|
| `backups/tutorial-part-03-{date}.dump` | After Part 3: templates, tiles, menus, search, speakers |
| `backups/tutorial-part-04-{date}.dump` | After Part 4: users, roles, stages, workflow, revisions |

---

## New TOOLS.md Sections Required

### For Part 3

**§ Templates**
- Template directory: `templates/`
- Template suggestion resolution chain
- How to verify a new template is picked up (restart server or check hot reload)
- Useful debug: `curl -s http://localhost:3000/item/{id}` to test rendering

**§ Files/Media**
- File upload endpoint (multipart POST to item save)
- File storage path (configurable, default `files/`)
- Verify upload via: `SELECT * FROM file_managed ORDER BY created DESC LIMIT 5;`
- Curl command for file upload (multipart form)
- Allowed MIME types reference
- Max file size (10 MB)

**§ Search**
- Rebuild search index: CLI command or admin trigger
- Test search: `curl -s 'http://localhost:3000/api/search?q=rust' | jq`
- Search field config table: `search_field_config`
- Verify tsvector: `SELECT id, title, search_vector FROM item WHERE search_vector IS NOT NULL LIMIT 3;`

### For Part 4

**§ Roles & Access**
- Create test users via curl (registration endpoint)
- Login as specific user: curl with cookie jar
- Test access with different sessions:
  ```
  # Login as editor
  curl -s -c /tmp/editor.txt -X POST http://localhost:3000/user/login -d '...'
  # Test access
  curl -s -b /tmp/editor.txt http://localhost:3000/gather/incoming_items
  ```
- Assign role via admin API or SQL
- List user permissions: `SELECT * FROM role_permission WHERE role_id = '...';`

**§ Stages & Workflows**
- Inspect item stage: `SELECT stage_id FROM item WHERE id = '...';`
- Trigger stage transition via admin UI or API endpoint
- Verify stage-aware Gather:
  ```
  curl -s 'http://localhost:3000/api/query/ritrovo.upcoming_conferences/execute' | jq '.items | length'
  ```
- Workflow transition graph (text representation)
- Add new stage: create tag config YAML, update workflow, re-import

**§ Revisions**
- View revision history: `SELECT * FROM item_revision WHERE item_id = '...' ORDER BY created;`
- Revert via admin UI or API
- Verify draft-while-live: compare anonymous vs editor view of same item
- Emergency unpublish: SQL to set active=false (or admin action)

**§ Backups (updates)**
- Add new snapshot rows for Part 3 and Part 4 completion points
