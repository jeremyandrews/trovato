# Epic 5: Look & Feel

**Tutorial Part:** 3
**Trovato Phase Dependency:** Phase 3 (Render Tree, Templates, Files), Phase 5 (Tiles, Slots)
**BMAD Epic:** 34
**Status:** Complete (tutorial written, all features implemented)

---

## Narrative

*The data pipe becomes a website. Part 2 ended with 5,000+ conferences flowing through five Gather queries, but every page renders with default templates -- raw field labels, no images, no navigation, no sidebar. Part 3 transforms Ritrovo from a database browser into a polished conference directory with styled detail pages, speaker profiles, file uploads, a five-region page layout, full-text search, and a cohesive visual theme.*

The reader builds the entire presentation layer. They learn the Render Tree pipeline (Build > Alter > Sanitize > Render), write Tera templates with the specificity chain, wire up file uploads with MIME validation and magic-byte checking, create a second content type (Speaker) with RecordReference links to conferences, configure Slots and Tiles for a five-region page layout, set up navigation menus with breadcrumbs, integrate PostgreSQL full-text search with weighted fields, and consolidate everything into a premium CSS theme with design tokens.

No plugins are written in this part (the importer from Part 2 handles all content creation). This is pure configuration, templates, and understanding how the kernel renders safe HTML.

---

## Tutorial Steps

### Step 1: The Render Tree & Tera Templates

Tour the Render Tree pipeline that turns Items into safe HTML. Cover the four phases (Build, Alter, Sanitize, Render), the template resolution specificity chain (`item--conference--{uuid}.html` > `item--conference.html` > `item.html`), the `safe_urls` pattern that prevents `javascript:` URI injection in `href` attributes, and the conference detail and CFP listing templates.

**What to cover:**

- The four Render Tree phases: Build (load + HTML-escape), Alter (`tap_item_view_alter`), Sanitize (format filters), Render (Tera)
- Template resolution chain: item-specific > type-specific > default
- The `safe_urls` pattern: why Tera autoescape doesn't prevent `javascript:` URIs and how the kernel builds a safe URL map
- Conference detail template layout: header, media, description, external links, speakers (reverse references)
- CFP listing template: cards linking to detail pages, not to raw external URLs
- `FilterPipeline::for_format_safe()` -- why plugins and user content use the safe variant
- Forward and reverse RecordReference resolution in the view handler

### Step 2: File Uploads & Media

Wire up file uploads for conference logos and venue photos. Cover the AJAX upload widget, MIME allowlist, magic-byte validation, temp-to-permanent lifecycle, and file serving with security hardening.

**What to cover:**

- File field configuration in the conference Item Type YAML
- AJAX upload flow: client-side size check, multipart POST, magic-byte validation, UUID-based temp storage
- `ALLOWED_MIME_TYPES` allowlist and `validate_magic_bytes()` -- how a `.exe` renamed to `.jpg` is rejected
- Temp-to-permanent lifecycle: `status = 0` on upload, promoted to `status = 1` on Item save, orphan cleanup via cron
- File serving at `/files/{path}` with directory traversal prevention, `Content-Disposition`, `X-Content-Type-Options: nosniff`, and `Cache-Control`
- The `file_managed` database table

### Step 3: The Speaker Content Type

Create a second content type demonstrating RecordReference fields and bidirectional relationships.

**What to cover:**

- Speaker Item Type definition: bio, company, website, photo (File), conferences (RecordReference multi-value)
- RecordReference storage: JSONB array of UUIDs
- Forward references: speaker detail page resolves conference UUIDs into linked titles
- Reverse references: conference detail page automatically discovers speakers referencing it -- no `field_speakers` needed on the conference type
- Pathauto alias pattern: `speakers/[title]`
- Speaker detail template with `safe_urls` for website links

### Step 4: Page Layout -- Slots, Tiles & Navigation

Add site chrome with a five-region Slot system, dynamic Tiles, navigation menus, and breadcrumbs.

**What to cover:**

- Five Slots: Header, Navigation, Content, Sidebar, Footer
- Tile configuration: machine_name, region, tile_type (`custom_html`, `menu`, `gather_query`), config JSON, visibility rules, weight
- Tile visibility: path patterns and role restrictions (role-based visibility deferred to Part 4)
- `inject_site_context()` -- how every page gets site name, menus, tiles, auth state, and CSRF token
- Navigation menus from the `menu_link` table with active highlighting
- Breadcrumbs: item pages show Home > Type Label > Title; Gather pages show Home > Query Label
- Ritrovo tiles: search_box (Header), topic_cloud (Sidebar), conferences_this_month (Sidebar), open_cfps_sidebar (Sidebar), footer_info (Footer)
- Note: tile and menu link configuration is via admin UI only (not yet supported by `config import`)

### Step 5: Full-Text Search

Integrate PostgreSQL tsvector search with weighted fields, relevance ranking, and a search API.

**What to cover:**

- Search field configuration via YAML: content type, field name, weight (A-D)
- Six search field configs: conference title (A), description (B), city (C), country (C); speaker title (A), bio (B)
- Index rebuild via admin UI (`/admin/structure/types/{type}/search`)
- Search flow: `tsquery` parsing, `ts_rank_cd()` weighted ranking, GIN index for sub-millisecond lookups
- Search results page at `/search?q={query}` with result count, type badges, snippets, pagination
- JSON search API at `/api/search?q={query}`
- Stage-aware search: anonymous sees Live only; editors see their accessible stages

### Step 6: Theme & Visual Design

Consolidate all visual design into a cohesive premium theme with CSS design tokens, shared card components, a polished front page, and content pages.

**What to cover:**

- CSS custom properties (design tokens): `--primary`, `--accent`, `--teal`, `--rose`, gradients, shadows, transitions
- BEM naming convention: `.card__title`, `.speaker-card__photo--placeholder`
- Shared card component: CSS base class (`.card`) with modifier variants (`.card--conf`, `.card--cfp`, `.card--speaker`), Tera template include (`gather/includes/conf-card.html`) shared across all conference listings
- Front page template (`page--front.html`): hero section with CSS animations, stat cards, call-to-action buttons
- Content pages (About, Contact) via UUID-specific templates -- bypassing `FilterPipeline::for_format_safe()` without weakening the security model
- Topics page: alphabetical sort via `tags.sort_by()` for directory usability
- Gather route inline rendering: pretty URLs at `/topics/{slug}` render gather queries inline (200 response, no redirect)
- Hiding duplicate field output: CSS hides generic `{{ children | safe }}` fields that the template already renders explicitly

---

## BMAD Stories

### Story 34.1: Render Tree Pipeline & Conference Templates

**Status:** Complete

**As a** developer building a Trovato site,
**I want** styled templates for conference and CFP pages that render safely through the Render Tree,
**So that** the site has a polished presentation without security vulnerabilities.

**Acceptance criteria:**

- Tera templates created for conference detail (`item--conference.html`) and CFP listing (`query--ritrovo.open_cfps.html`)
- Conference detail shows header (title, dates, location), media (logo, venue photo), description, external links, and speakers
- Template resolution chain works: item-specific > type-specific > default
- All URLs in `href` attributes use the `safe_urls` map (no raw `item.fields` values in links)
- `| safe` usage in templates has `{# SAFE: reason #}` comments justifying pre-sanitization
- Reverse references render automatically (speakers linked to a conference appear on the conference page)
- All user content passes through `FilterPipeline::for_format_safe()`

### Story 34.2: File Uploads with Security Validation

**Status:** Complete

**As a** content editor,
**I want to** upload conference logos and venue photos,
**So that** conferences have visual identity on the site.

**Acceptance criteria:**

- AJAX file upload widget handles client-side size validation and server-side MIME + magic-byte checking
- `validate_magic_bytes()` rejects disguised executables (e.g., ELF binary with `.jpg` extension)
- Uploaded files stored as temporary (`status = 0`) with UUID-based filenames; promoted to permanent (`status = 1`) on Item save
- Orphaned temp files cleaned up by cron after 6 hours
- File serving at `/files/{path}` includes directory traversal prevention, `X-Content-Type-Options: nosniff`, and `Content-Disposition` headers
- `ALLOWED_MIME_TYPES` allowlist enforced (images, PDFs, documents, archives)
- File fields render in conference templates as `<img>` tags pointing to `/files/{path}`

### Story 34.3: Speaker Content Type with RecordReference

**Status:** Complete

**As a** site builder,
**I want** a Speaker content type linked to conferences via RecordReference fields,
**So that** speakers have profiles with links to their conferences and conferences show their speakers.

**Acceptance criteria:**

- Speaker Item Type created with fields: bio (TextLong), company (Text), website (Text), photo (File), conferences (RecordReference multi-value)
- RecordReference stores conference UUIDs as a JSONB array
- Speaker detail page resolves conference UUIDs into `referenced_items.field_conferences` with linked titles
- Conference detail page shows reverse references: speakers whose `field_conferences` contains the conference UUID
- Pathauto pattern `speakers/[title]` generates URL aliases like `/speakers/jane-doe`
- Speaker detail template uses `safe_urls.field_website` for external links
- Speaker search fields configured: title (weight A), bio (weight B)

### Story 34.4: Slots, Tiles & Navigation

**Status:** Complete

**As a** site visitor,
**I want** a site with navigation, sidebar content, and breadcrumbs,
**So that** I can navigate the conference directory intuitively.

**Acceptance criteria:**

- Five Slots implemented: Header, Navigation, Content, Sidebar, Footer
- Tiles assignable to Slots with weight-based ordering
- Tile visibility rules support path patterns (e.g., `"/conferences*"`)
- `inject_site_context()` provides site name, menus, tiles, auth state, and CSRF token to all pages
- Main menu renders from `menu_link` table with active link highlighting
- Breadcrumbs display on item pages (Home > Type > Title) and Gather pages (Home > Query Label)
- Ritrovo tiles configured: search_box, topic_cloud, conferences_this_month, open_cfps_sidebar, footer_info
- Auth pages (login, register, profile) call `inject_site_context()` for consistent site chrome

### Story 34.5: Full-Text Search with Weighted Fields

**Status:** Complete

**As a** site visitor,
**I want** to search across conferences and speakers with relevance ranking,
**So that** I can find specific events quickly.

**Acceptance criteria:**

- Search field configuration importable via YAML (content type, field, weight A-D)
- Six search field configs: conference title (A), description (B), city (C), country (C); speaker title (A), bio (B)
- `search_vector` tsvector column populated on items with configured search fields
- GIN index on `search_vector` for sub-millisecond lookups
- Search results ranked by `ts_rank_cd()` with weight-aware scoring
- HTML search page at `/search?q={query}` with result count, type badges, text snippets, pagination
- JSON search API at `/api/search?q={query}`
- Search is stage-aware: anonymous sees Live only
- Search reindex triggerable via admin UI per content type
- Empty state handled gracefully

### Story 34.6: Premium Theme with Design Token System

**Status:** Complete

**As a** site visitor,
**I want** a visually polished, consistent site design,
**So that** the conference directory looks and feels professional.

**Acceptance criteria:**

- Single `theme.css` file with CSS custom properties (design tokens) for colors, gradients, shadows, radii, transitions
- BEM naming convention for all CSS classes
- Shared card component: `.card` base class with `.card--conf`, `.card--cfp`, `.card--speaker` modifiers
- Tera template include (`gather/includes/conf-card.html`) shared across all conference listing queries
- Polished front page (`page--front.html`) with hero section, stat cards, and call-to-action buttons
- Content pages (About, Contact) use UUID-specific templates for rich layout HTML
- Topics page sorted alphabetically for directory usability
- Gather route aliases render inline at pretty URLs (200, not 307 redirect)
- CSS hides duplicate field output from generic `{{ children | safe }}` rendering
- All inline `<style>` blocks in public-facing templates consolidated into `theme.css`

---

## Payoff

A working, polished conference directory. The reader understands:

- How the Render Tree pipeline (Build > Alter > Sanitize > Render) ensures safe HTML output
- How template resolution follows a specificity chain and how UUID-specific templates solve the rich content problem
- How `safe_urls` prevents `javascript:` URI injection in template `href` attributes
- How file uploads work with MIME validation, magic-byte checking, and a temp-to-permanent lifecycle
- How RecordReference fields create bidirectional relationships without denormalization
- How Slots and Tiles provide a configurable page layout with visibility rules
- How PostgreSQL tsvector provides full-text search with configurable field weights
- How CSS design tokens create a maintainable, rebrandable theme
- How shared Tera includes and CSS base classes eliminate template duplication

The site now looks, navigates, and searches like a production application. Part 4 adds editorial discipline.

---

## What's Deferred

These are explicitly **not** in Part 3 (and the tutorial should say so):

- **Users/auth** -- Part 4 (single admin user for now)
- **Role-based tile visibility** -- Part 4 (requires roles system)
- **Stage-aware search filtering** -- Part 4 (requires stages)
- **WYSIWYG editor** -- Part 5 (rich text editing is a form enhancement)
- **Comments** -- Part 6 (depends on users + permissions)
- **Internationalization** -- Part 7 (separate concern)
- **Pagefind client-side search** -- Epic 2 (tsvector only in this part)
- **Image import from confs.tech** -- Future (importer doesn't fetch images; manual upload only)
- **Config import for tiles/menus** -- Future (ConfigStorage does not yet support tile or menu_link entity types)

---

## Related

- [Part 3: Look & Feel](../tutorial/part-03-look-and-feel.md)
- [Ritrovo Overview](overview.md)
- [Documentation Architecture](documentation-architecture.md)
- [Epic 1: Hello, Trovato](epic-01.md) -- Part 1 foundations
- [Epic 4: From Demo to Data-Driven](epic-04.md) -- Part 2 plugin + data
- [Content Model Design](../design/Design-Content-Model.md)
- [Render Tree & Forms Design](../design/Design-Render-Theme.md)
