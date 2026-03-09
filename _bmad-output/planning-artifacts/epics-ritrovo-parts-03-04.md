---
stepsCompleted: ['step-01-validate-prerequisites', 'step-02-design-epics', 'step-03-create-stories', 'step-04-final-validation']
inputDocuments:
  - docs/tutorial/plan-parts-03-04.md
  - docs/design/Design-Render-Theme.md
  - docs/design/Design-Content-Model.md
  - docs/design/Design-Web-Layer.md
  - docs/design/Design-Infrastructure.md
  - docs/tutorial/part-01-hello-trovato.md
  - docs/tutorial/part-02-ritrovo-importer.md
---

# trovato - Ritrovo Tutorial Parts 3 & 4 Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for the Ritrovo tutorial Parts 3 (Look & Feel) and Part 4 (The Editorial Engine), decomposing the requirements from the plan document and architecture design docs into implementable stories.

## Requirements Inventory

### Functional Requirements

FR1: Render Tree pipeline (Build → Alter → Sanitize → Render) converts Item data to HTML via structured JSON RenderElements — plugins never produce raw HTML
FR2: Tera template engine resolves templates via specificity chain: item--{type}--{id}.html → item--{type}.html → item.html
FR3: Conference detail page renders with a type-specific template showing title, dates, location, description, and external links
FR4: Open CFPs Gather uses a dedicated template with CFP-specific styling (deadline display, submit link)
FR5: Base page layout defines five named Slot regions: Header, Navigation, Content, Sidebar, Footer
FR6: File upload for conference logos and venue photos with MIME validation and magic byte checking
FR7: Uploaded files transition from temporary (status=0) to permanent (status=1) on item save; cron cleans orphaned temps after 6 hours
FR8: Speaker item type with fields: bio (TextLong), company (Text), website (Text), photo (File), conferences (RecordReference → conference, multi-value)
FR9: RecordReference forward references (speaker → conferences) and reverse references (conference → speakers) render on detail pages
FR10: Pathauto generates /speakers/{name} URL aliases for speaker items
FR11: Tiles are content blocks placed in Slot regions, ordered by weight, with path-based visibility rules
FR12: Six Tiles configured: site branding (Header), search box (Header), conferences this month (Sidebar), open CFPs sidebar (Sidebar), topic cloud (Sidebar), footer info (Footer)
FR13: Tile visibility rules restrict Tiles to matching URL path patterns (e.g., CFP tile only on /conferences*, /topics/*, /cfps*)
FR14: Main navigation menu with Conferences, Open CFPs, Topics (with hierarchical children), and Speakers links
FR15: Footer menu with About and Contact placeholder links
FR16: Active menu item highlighting and active trail (parent highlighted when child page is current)
FR17: Breadcrumbs built from menu hierarchy and category hierarchy
FR18: Full-text search with PostgreSQL tsvector, field weighting (title=A, description=B, city/country=C)
FR19: Search box Tile in Header Slot on all pages, posting to /search?q={query}
FR20: Search results page with relevance ranking, highlighted snippets, content type badges, pagination, and empty state
FR21: Search API at /api/search?q={query} returns JSON with relevance scores
FR22: User registration (open with admin approval), login/logout, session management via Redis
FR23: User profile page at /user/{username} with display name and bio
FR24: Session fixation protection — session ID cycled after authentication state changes
FR25: Five roles: anonymous, authenticated, editor, publisher, admin
FR26: Permission-to-role mapping via config import (e.g., editor gets "edit any conference", "view internal content")
FR27: ritrovo_access WASM plugin implementing tap_perm (declare permissions), tap_item_access (Grant/Deny/Neutral), tap_item_view (strip editor_notes from non-editors)
FR28: Grant/Deny/Neutral access aggregation: any Grant + no Deny = allow; any Deny = deny; all Neutral = default
FR29: Field-level access control via render tree manipulation — editor_notes stripped for users without "edit any conference" permission
FR30: Three stages: Incoming (Internal, default), Curated (Internal), Live (Public) — defined as tags in stages category
FR31: Editorial workflow as directed graph: incoming→curated, curated→live, live→curated, curated→incoming — each transition requires a specific permission
FR32: Invalid stage transitions rejected by kernel
FR33: Imported conferences land on Incoming stage (not Live) — requires ritrovo_importer plugin update
FR34: Stage-aware Gathers: CTE wraps queries filtering by stage visibility per role
FR35: Stage-aware search: anonymous sees only Live/Public content
FR36: Extensibility demo: add "Legal Review" stage between Curated and Live as config-only change (no code, no plugin rebuild)
FR37: Every item edit creates a new revision in item_revision table
FR38: Revert to previous revision creates a NEW revision (never deletes old ones)
FR39: Draft-while-live: Curated draft exists while Live version remains publicly visible
FR40: Cross-stage field updates: importer updates Live item that has Curated draft — kernel writes one revision with other_stage_revisions context
FR41: Emergency unpublish: set active=false on Live revision for immediate removal without stage transition
FR42: Admin content list with stage/type/author/date filters
FR43: Bulk operations on content list: select multiple items, change stage (respects workflow permissions)
FR44: Import queue management: admin page showing Incoming items with approve/reject actions
FR45: Role-based Tile visibility (upgrade from path-based in Part 3 to also support role-based rules in Part 4)

### NonFunctional Requirements

NFR1: All user content rendered through the Render Tree is HTML-escaped unless a text format is specified — prevents XSS by construction
NFR2: File uploads validated against MIME allowlist and magic bytes — reject disguised executables (ELF/PE with image MIME types)
NFR3: Maximum file upload size: 10 MB
NFR4: Passwords stored as Argon2id hashes (RFC 9106: m=65536, t=3, p=4); minimum 12 characters
NFR5: Sessions stored in Redis with HttpOnly, Secure, SameSite=Strict cookies
NFR6: CSRF protection on all state-changing endpoints (POST/PUT/DELETE); logout must be POST
NFR7: WASM plugin sandbox constraints: 5s DB timeout, 30s HTTP timeout, 10 clock ticks
NFR8: Search index uses PostgreSQL-native tsvector for zero-dependency full-text search
NFR9: Stage-scoped cache keys (live uses bare keys, non-live uses st:{stage_id}:{key} prefix)
NFR10: Tag-based cache invalidation on item save
NFR11: All config entities (tiles, menus, roles, stages, workflows) importable via YAML config files
NFR12: Tutorial serves as regression test suite — every feature demonstrated must have trovato-test blocks run in CI

### Additional Requirements

- Render Tree architecture already implemented in kernel (theme/render.rs, theme/engine.rs, plugin-sdk/render.rs)
- Slots & Tiles model exists (models/tile.rs, routes/tile_admin.rs) — needs config import support verification
- Menu system exists (menu/registry.rs) — needs config import support verification
- File upload infrastructure exists (file/service.rs, file/storage.rs) — needs wiring to item forms
- Search service exists (search/mod.rs, routes/search.rs) — needs config-driven field weight setup
- Stages fully implemented (models/stage.rs, stage/mod.rs) with vocabulary-based system
- Revisions fully implemented (models/item.rs, content/item_service.rs) with get_revisions and revert_to_revision
- Roles & Permissions fully implemented (models/role.rs, permissions.rs)
- RecordReference field type exists in plugin-sdk
- Speaker item type does not exist yet — must be created as YAML config
- Workflows partially implemented — stage system covers primary use case but formal directed-graph workflow validation needs verification
- ritrovo_importer plugin exists and must be updated to target Incoming stage instead of Live
- ritrovo_access plugin does not exist yet — second WASM plugin to be built in Part 4
- Each part needs a recipe (agent-friendly doc) and TOOLS.md updates
- Database backup snapshots needed at end of Part 3 and Part 4

### FR Coverage Map

| FR | Story | Description |
|----|-------|-------------|
| FR1 | 1.1 | Render Tree pipeline wiring |
| FR2 | 1.1 | Template specificity chain |
| FR3 | 1.1 | Conference detail template |
| FR4 | 1.2 | CFP gather template |
| FR5 | 3.1 | Base page layout with Slots |
| FR6 | 1.3 | File upload integration |
| FR7 | 1.4 | File lifecycle management |
| FR8 | 2.1 | Speaker content type |
| FR9 | 2.2 | RecordReference rendering |
| FR10 | 2.3 | Speaker pathauto |
| FR11 | 3.2 | Tiles config import & rendering |
| FR12 | 3.2 | Six configured Tiles |
| FR13 | 3.3 | Tile path-based visibility rules |
| FR14 | 3.4 | Main navigation menu |
| FR15 | 3.5 | Footer menu |
| FR16 | 3.6 | Active menu highlighting |
| FR17 | 3.6 | Breadcrumbs |
| FR18 | 4.1 | Full-text search field weights |
| FR19 | 4.4 | Search box Tile |
| FR20 | 4.2 | Search results page |
| FR21 | 4.3 | Search JSON API |
| FR22 | 5.1 | User registration & login |
| FR23 | 5.3 | User profile page |
| FR24 | 5.2 | Session fixation protection |
| FR25 | 6.1 | Role definitions |
| FR26 | 6.1 | Permission-to-role mapping |
| FR27 | 6.2 | ritrovo_access plugin |
| FR28 | 6.3 | Access aggregation |
| FR29 | 6.4 | Field-level access control |
| FR30 | 7.1 | Stage definitions |
| FR31 | 7.2 | Workflow transitions |
| FR32 | 7.2 | Invalid transition rejection |
| FR33 | 7.3 | Importer targets Incoming |
| FR34 | 7.4 | Stage-aware Gathers |
| FR35 | 7.4 | Stage-aware search |
| FR36 | 7.8 | Extensibility demo |
| FR37 | 7.5 | Revision tracking |
| FR38 | 7.5 | Revert to revision |
| FR39 | 7.6 | Draft-while-live |
| FR40 | 7.6 | Cross-stage field updates |
| FR41 | 7.7 | Emergency unpublish |
| FR42 | 8.1 | Admin content list |
| FR43 | 8.2 | Bulk stage operations |
| FR44 | 8.3 | Import queue management |
| FR45 | 8.4 | Role-based Tile visibility |

## Epic List

- **Epic 1: Site Presentation & Media** — Render Tree templates, file uploads, conference detail pages (FR1–FR7; NFR1–NFR3)
- **Epic 2: Speaker Profiles & Relationships** — Speaker content type, RecordReference rendering, pathauto (FR8–FR10)
- **Epic 3: Site Layout & Navigation** — Slots, Tiles, menus, breadcrumbs, base page layout (FR5, FR11–FR17; NFR11)
- **Epic 4: Conference Search** — Full-text search indexing, results page, search API, search Tile (FR18–FR21; NFR8)
- **Epic 5: User Accounts & Authentication** — Registration, login/logout, sessions, profile pages (FR22–FR24; NFR4–NFR6)
- **Epic 6: Roles & Access Control Plugin** — Roles, permissions, ritrovo_access WASM plugin, field-level access (FR25–FR29; NFR7)
- **Epic 7: Editorial Workflow & Revisions** — Stages, workflow graph, revisions, draft-while-live, stage-aware queries (FR30–FR41; NFR9–NFR10)
- **Epic 8: Admin Content Management** — Content list, bulk operations, import queue, role-based Tile visibility (FR42–FR45; NFR12)

## Epic 1: Site Presentation & Media

Wire the existing Render Tree infrastructure to produce themed HTML for conference pages, connect file uploads to item forms, and deliver polished conference detail and CFP templates.

**FRs:** FR1, FR2, FR3, FR4, FR6, FR7 | **NFRs:** NFR1, NFR2, NFR3

### Story 1.1: Conference Detail Template via Render Tree

As a site visitor,
I want to view a conference detail page with title, dates, location, description, and external links,
So that I can learn about a conference before deciding to attend.

**Acceptance Criteria:**

**Given** a conference item exists in the database
**When** I visit the conference detail URL
**Then** the page renders through the Render Tree pipeline (Build → Alter → Sanitize → Render)
**And** a type-specific template `item--conference.html` is resolved via the specificity chain (`item--conference--{id}.html` → `item--conference.html` → `item.html`)
**And** the page displays: title, dates, location, description, and external links
**And** all user-supplied content is HTML-escaped by the Render Tree (NFR1)

### Story 1.2: CFP Gather Template

As a site visitor,
I want the Open CFPs listing to use a dedicated template with CFP-specific styling,
So that I can quickly see deadlines and submission links.

**Acceptance Criteria:**

**Given** the Open CFPs gather query exists and returns conferences with open CFPs
**When** I visit the Open CFPs listing page
**Then** it renders using `query--ritrovo.open_cfps.html` (not the default gather template)
**And** each result displays the CFP deadline prominently
**And** each result includes a "Submit" link to the CFP URL
**And** the template inherits from the base page layout

### Story 1.3: File Upload on Item Forms

As a site editor,
I want to upload conference logos and venue photos through the item edit form,
So that conference pages display visual media.

**Acceptance Criteria:**

**Given** I am editing a conference item
**When** I select a file and submit the form
**Then** the file is uploaded and associated with the item
**And** the upload validates the MIME type against the allowlist (NFR2)
**And** the upload validates magic bytes match the declared MIME type (NFR2)
**And** files larger than 10 MB are rejected with a clear error message (NFR3)
**And** disguised executables (ELF/PE headers with image MIME types) are rejected (NFR2)

### Story 1.4: File Lifecycle Management

As a system administrator,
I want uploaded files to transition from temporary to permanent on item save and orphaned temps to be cleaned up,
So that disk space is not wasted by abandoned uploads.

**Acceptance Criteria:**

**Given** a file was uploaded but the item form has not been saved
**When** the item is saved
**Then** the file status transitions from temporary (status=0) to permanent (status=1)
**And** the file is linked to the saved item

**Given** a temporary file has existed for more than 6 hours without being attached to an item
**When** the cleanup cron task runs
**Then** the orphaned temporary file is deleted from storage and its database record is removed

## Epic 2: Speaker Profiles & Relationships

Create the speaker content type, wire RecordReference rendering for bidirectional speaker↔conference links, and configure pathauto for speaker URLs.

**FRs:** FR8, FR9, FR10

### Story 2.1: Speaker Content Type Definition

As a site editor,
I want a Speaker content type with bio, company, website, photo, and conferences fields,
So that I can create and manage speaker profiles.

**Acceptance Criteria:**

**Given** the speaker content type YAML config is imported
**When** I visit the admin content creation form
**Then** "Speaker" appears as a content type option
**And** the form includes fields: bio (TextLong), company (Text), website (Text), photo (File), conferences (RecordReference → conference, multi-value)
**And** I can create a speaker item with all fields populated
**And** the speaker item is stored with correct field types in the database

### Story 2.2: RecordReference Rendering on Detail Pages

As a site visitor,
I want to see linked conferences on a speaker page and linked speakers on a conference page,
So that I can navigate between related content.

**Acceptance Criteria:**

**Given** a speaker item references one or more conferences via the RecordReference field
**When** I view the speaker detail page
**Then** the referenced conferences are displayed as clickable links (forward reference)

**Given** a speaker references a conference
**When** I view that conference's detail page
**Then** the speaker appears in a "Speakers" section as a clickable link (reverse reference)

**Given** a referenced item is deleted
**When** I view the referring item's detail page
**Then** the deleted reference is not displayed (no broken links)

### Story 2.3: Speaker Pathauto URL Aliases

As a site visitor,
I want speaker pages to have clean URLs like `/speakers/{name}`,
So that speaker URLs are readable and shareable.

**Acceptance Criteria:**

**Given** a pathauto pattern `/speakers/[item:title]` is configured for the speaker content type
**When** a new speaker item is created with the name "Jane Doe"
**Then** a URL alias `/speakers/jane-doe` is generated and resolves to the speaker detail page

**Given** a speaker with the same name already exists
**When** a second speaker "Jane Doe" is created
**Then** the alias is deduplicated (e.g., `/speakers/jane-doe-1`)

**Given** a speaker's name is updated
**When** the item is saved
**Then** the URL alias is regenerated to reflect the new name

## Epic 3: Site Layout & Navigation

Configure the base page layout with five Slot regions, import Tiles and menus via YAML config, wire active trail highlighting and breadcrumbs.

**FRs:** FR5, FR11, FR12, FR13, FR14, FR15, FR16, FR17 | **NFRs:** NFR11

### Story 3.1: Base Page Layout with Slot Regions

As a site visitor,
I want every page to share a consistent layout with header, navigation, content, sidebar, and footer areas,
So that the site feels cohesive and professional.

**Acceptance Criteria:**

**Given** the base `page.html` template is defined
**When** any page is rendered
**Then** the HTML output contains five named Slot regions: Header, Navigation, Content, Sidebar, Footer
**And** the Content slot contains the page-specific content
**And** other slots are populated by assigned Tiles
**And** the layout renders correctly with no Tiles assigned (empty slots collapse gracefully)

### Story 3.2: Tile Config Import & Rendering

As a site administrator,
I want to import Tile definitions via YAML config files,
So that content blocks appear in the correct Slot regions without manual database setup.

**Acceptance Criteria:**

**Given** YAML config files define six Tiles: site branding (Header), search box (Header), conferences this month (Sidebar), open CFPs sidebar (Sidebar), topic cloud (Sidebar), footer info (Footer)
**When** the config is imported
**Then** all six Tiles are created in the database with correct slot assignments and weights
**And** each Tile renders its content in the assigned Slot region
**And** Tiles within the same Slot are ordered by weight (lower weight = higher position)

### Story 3.3: Tile Path-Based Visibility Rules

As a site administrator,
I want Tiles to appear only on pages matching specific URL path patterns,
So that contextually relevant content blocks appear on the right pages.

**Acceptance Criteria:**

**Given** the open CFPs sidebar Tile is configured with visibility paths `/conferences*`, `/topics/*`, `/cfps*`
**When** I visit `/conferences/rustconf-2026`
**Then** the CFP sidebar Tile is rendered

**Given** the same Tile configuration
**When** I visit `/speakers/jane-doe`
**Then** the CFP sidebar Tile is NOT rendered

**Given** a Tile has no visibility path restrictions
**When** I visit any page
**Then** the Tile is rendered on all pages

### Story 3.4: Main Navigation Menu

As a site visitor,
I want a main navigation menu with links to Conferences, Open CFPs, Topics, and Speakers,
So that I can navigate to the primary sections of the site.

**Acceptance Criteria:**

**Given** the main menu YAML config is imported
**When** any page is rendered
**Then** the Navigation slot contains a menu with: Conferences, Open CFPs, Topics, Speakers
**And** the Topics menu item has hierarchical children matching the topic category tags
**And** each menu link resolves to the correct page

### Story 3.5: Footer Menu

As a site visitor,
I want a footer menu with About and Contact links,
So that I can find site information and contact details.

**Acceptance Criteria:**

**Given** the footer menu YAML config is imported
**When** any page is rendered
**Then** the Footer slot contains a menu with About and Contact links
**And** the links point to placeholder pages (or anchors)

### Story 3.6: Active Menu Highlighting & Breadcrumbs

As a site visitor,
I want the current page's menu item highlighted and breadcrumbs showing my location,
So that I always know where I am in the site.

**Acceptance Criteria:**

**Given** I am on the Open CFPs page
**When** the page renders
**Then** the "Open CFPs" menu item has an active class applied

**Given** I am on a topic child page (e.g., `/topics/web-frameworks`)
**When** the page renders
**Then** the "Topics" parent menu item has an active-trail class
**And** the child item has an active class

**Given** I am on a conference detail page under the Conferences section
**When** the page renders
**Then** breadcrumbs display: Home > Conferences > {conference title}

**Given** I am on a topic tag page nested under a parent topic
**When** the page renders
**Then** breadcrumbs reflect the category hierarchy: Home > Topics > {parent} > {child}

## Epic 4: Conference Search

Configure field weights for the existing search service, create a search results page with relevance ranking and snippets, expose a JSON search API, and add a search box Tile.

**FRs:** FR18, FR19, FR20, FR21 | **NFRs:** NFR8

### Story 4.1: Search Index Field Weight Configuration

As a site administrator,
I want search field weights configured so that title matches rank higher than description matches,
So that search results are ordered by relevance.

**Acceptance Criteria:**

**Given** search field weight YAML config is imported with title=A, description=B, city/country=C
**When** the search index is rebuilt
**Then** conference items are indexed with PostgreSQL tsvector using the configured weights
**And** a search for a term appearing in a conference title ranks that result above one where the term only appears in the description

### Story 4.2: Search Results Page

As a site visitor,
I want to search for conferences and see relevant results with snippets,
So that I can quickly find conferences matching my interests.

**Acceptance Criteria:**

**Given** conferences exist in the search index
**When** I visit `/search?q=rust`
**Then** matching conferences are displayed ordered by relevance score
**And** each result shows a highlighted snippet with the search term bolded
**And** each result shows a content type badge (e.g., "Conference")
**And** results are paginated (with next/previous controls)

**Given** no conferences match the search query
**When** I visit `/search?q=xyznonexistent`
**Then** an empty state message is displayed (e.g., "No results found for 'xyznonexistent'")

**Given** a search query with special characters
**When** I submit the search
**Then** the query is safely handled without errors (no SQL injection, HTML escaped in display)

### Story 4.3: Search JSON API

As an API consumer,
I want to query `/api/search?q={query}` and receive JSON results with relevance scores,
So that I can integrate search into other applications.

**Acceptance Criteria:**

**Given** conferences exist in the search index
**When** I GET `/api/search?q=rust`
**Then** the response is JSON with an array of results
**And** each result includes: item ID, title, type, URL, snippet, and relevance score
**And** results are ordered by descending relevance score

**Given** an empty query string
**When** I GET `/api/search?q=`
**Then** the response is a 400 error with a descriptive message

### Story 4.4: Search Box Tile

As a site visitor,
I want a search box in the header on every page,
So that I can search from anywhere on the site.

**Acceptance Criteria:**

**Given** the search box Tile is configured in the Header slot with no path visibility restrictions
**When** any page is rendered
**Then** a search form is displayed in the Header slot
**And** the form posts to `/search?q={query}`
**And** submitting the form navigates to the search results page with the entered query

## Epic 5: User Accounts & Authentication

Implement user registration with admin approval, login/logout with Redis sessions, session fixation protection, and user profile pages.

**FRs:** FR22, FR23, FR24 | **NFRs:** NFR4, NFR5, NFR6

### Story 5.1: User Registration & Login

As a site visitor,
I want to register for an account and log in,
So that I can access authenticated features of the site.

**Acceptance Criteria:**

**Given** I am an anonymous visitor
**When** I submit the registration form with a valid username, email, and password (minimum 12 characters)
**Then** my account is created in a pending/unapproved state
**And** my password is stored as an Argon2id hash (m=65536, t=3, p=4) (NFR4)
**And** I see a message indicating my account awaits admin approval

**Given** I have an approved account
**When** I submit the login form with correct credentials
**Then** I am authenticated and redirected to the home page
**And** a session is created in Redis (NFR5)

**Given** I submit the login form with incorrect credentials
**When** the login is processed
**Then** I see a generic error message ("Invalid username or password")
**And** no information is leaked about whether the username exists

**Given** I am logged in
**When** I submit a POST request to the logout endpoint
**Then** my session is destroyed in Redis
**And** I am redirected to the home page

**Given** someone attempts to log out via GET request
**When** the request is processed
**Then** it is rejected (logout must be POST) (NFR6)

### Story 5.2: Session Management & Fixation Protection

As a site user,
I want my session to be secure against hijacking and fixation attacks,
So that my account cannot be compromised.

**Acceptance Criteria:**

**Given** a user logs in successfully
**When** the session is created
**Then** the session cookie is set with HttpOnly, Secure, and SameSite=Strict flags (NFR5)
**And** the session data is stored in Redis (not in the cookie)

**Given** a user authenticates (login)
**When** authentication succeeds
**Then** the session ID is cycled (new ID issued, old ID invalidated) (FR24)

**Given** a user logs out
**When** the session is destroyed
**Then** the session ID is invalidated in Redis and cannot be reused

**Given** any state-changing endpoint (POST/PUT/DELETE)
**When** a request is submitted without a valid CSRF token
**Then** the request is rejected with a 403 error (NFR6)

### Story 5.3: User Profile Page

As a registered user,
I want a profile page at `/user/{username}`,
So that other users can see my display name and bio.

**Acceptance Criteria:**

**Given** a user account exists with username "jdoe", display name "Jane Doe", and a bio
**When** I visit `/user/jdoe`
**Then** the page displays the user's display name and bio

**Given** I visit `/user/nonexistent`
**When** the page is requested
**Then** a 404 Not Found page is returned

**Given** I am logged in as "jdoe"
**When** I visit my own profile page
**Then** I see my profile (editing profile is deferred to a future epic)

## Epic 6: Roles & Access Control Plugin

Define roles and permission mappings via config import, build the ritrovo_access WASM plugin implementing tap_perm/tap_item_access/tap_item_view, and wire field-level access control via render tree manipulation.

**FRs:** FR25, FR26, FR27, FR28, FR29 | **NFRs:** NFR7

### Story 6.1: Role Definitions & Permission Mapping via Config Import

As a site administrator,
I want to define roles and their permissions via YAML config files,
So that access control is reproducible and version-controlled.

**Acceptance Criteria:**

**Given** YAML config files define five roles: anonymous, authenticated, editor, publisher, admin
**When** the config is imported
**Then** all five roles are created in the database with correct machine names
**And** permission-to-role mappings are applied (e.g., editor gets "edit any conference", "view internal content")
**And** re-importing the same config is idempotent (no duplicates)

### Story 6.2: ritrovo_access WASM Plugin — Permission Declaration & Item Access

As a site administrator,
I want the ritrovo_access plugin to declare custom permissions and control item-level access,
So that content visibility is enforced by role.

**Acceptance Criteria:**

**Given** the ritrovo_access WASM plugin is compiled and installed
**When** the plugin system initializes
**Then** `tap_perm` declares permissions: "edit any conference", "view internal content", "administer content"
**And** the declared permissions are available for role assignment

**Given** an anonymous user requests a conference item
**When** `tap_item_access` is invoked
**Then** the plugin returns Grant for items on the Live/Public stage
**And** the plugin returns Neutral for items on Internal stages (Incoming, Curated)

**Given** an editor user requests an Incoming or Curated conference
**When** `tap_item_access` is invoked
**Then** the plugin returns Grant (editors can view internal content)

**Given** the plugin executes
**When** it exceeds sandbox constraints
**Then** the WASM runtime enforces 5s DB timeout, 30s HTTP timeout, 10 clock ticks (NFR7)

### Story 6.3: Access Aggregation & Item Visibility

As a site visitor,
I want content access to be determined by combining all plugin access responses,
So that access control is consistent and predictable.

**Acceptance Criteria:**

**Given** multiple plugins respond to `tap_item_access` for the same item
**When** any plugin returns Grant and no plugin returns Deny
**Then** access is allowed

**Given** any plugin returns Deny for an item
**When** access is evaluated
**Then** access is denied regardless of other Grant responses

**Given** all plugins return Neutral for an item
**When** access is evaluated
**Then** access is denied (default deny)

### Story 6.4: Field-Level Access via Render Tree

As a site visitor,
I want sensitive fields like editor_notes hidden from non-editors,
So that internal editorial content is not exposed publicly.

**Acceptance Criteria:**

**Given** a conference item has an `editor_notes` field populated
**When** an anonymous or authenticated (non-editor) user views the item
**Then** `tap_item_view` strips the `editor_notes` element from the render tree before rendering
**And** the field does not appear in the rendered HTML

**Given** a conference item has an `editor_notes` field populated
**When** an editor or admin views the item
**Then** the `editor_notes` field is present in the rendered output

**Given** a plugin attempts to manipulate the render tree
**When** it supplies tag names or attributes
**Then** tag names are validated against SAFE_TAGS and attributes against is_valid_attr_key()

## Epic 7: Editorial Workflow & Revisions

Configure stages and workflow transitions via config, update the importer to target Incoming, wire stage-aware Gathers and search, implement revision tracking with draft-while-live and emergency unpublish, and demonstrate extensibility with a config-only stage addition.

**FRs:** FR30, FR31, FR32, FR33, FR34, FR35, FR36, FR37, FR38, FR39, FR40, FR41 | **NFRs:** NFR9, NFR10

### Story 7.1: Stage Definitions via Config Import

As a site administrator,
I want to define editorial stages via YAML config,
So that the content workflow is reproducible and version-controlled.

**Acceptance Criteria:**

**Given** YAML config defines three stages as tags in the "stages" category: Incoming (Internal, default), Curated (Internal), Live (Public)
**When** the config is imported
**Then** all three stage tags are created with correct visibility attributes (Internal vs Public)
**And** Incoming is marked as the default stage for new content
**And** re-importing the config is idempotent

### Story 7.2: Workflow Transition Graph

As a site administrator,
I want editorial workflow transitions defined as a directed graph with permission requirements,
So that content moves through stages in a controlled manner.

**Acceptance Criteria:**

**Given** workflow config defines transitions: incoming→curated, curated→live, live→curated, curated→incoming
**When** the config is imported
**Then** each transition is recorded with its required permission

**Given** an editor with "transition incoming to curated" permission
**When** they attempt to move an item from Incoming to Curated
**Then** the transition succeeds

**Given** an editor without the required transition permission
**When** they attempt a stage transition
**Then** the transition is rejected with a permission error

**Given** an editor attempts an undefined transition (e.g., incoming→live)
**When** the transition is processed
**Then** the kernel rejects it as an invalid transition (FR32)

### Story 7.3: Importer Targets Incoming Stage

As a site administrator,
I want imported conferences to land on the Incoming stage instead of Live,
So that imported content goes through editorial review before publication.

**Acceptance Criteria:**

**Given** the ritrovo_importer plugin is updated
**When** new conferences are imported
**Then** they are created with the Incoming stage (not Live)
**And** existing Live conferences are not affected by the plugin update

**Given** the importer runs and creates new items
**When** viewing the admin content list
**Then** newly imported items appear with "Incoming" stage

### Story 7.4: Stage-Aware Gathers & Search

As a site visitor,
I want public listings and search results to show only published content,
So that draft and in-review content is not exposed.

**Acceptance Criteria:**

**Given** conferences exist on Incoming, Curated, and Live stages
**When** an anonymous user views a gather listing (e.g., upcoming conferences)
**Then** only Live/Public items are included (CTE wraps the query filtering by stage visibility)

**Given** an editor user views the same gather listing
**When** the gather executes
**Then** items on all stages visible to their role are included

**Given** an anonymous user searches via `/search?q=rust`
**When** results are returned
**Then** only Live/Public items appear in the results (FR35)

**Given** stage-scoped cache keys are in use
**When** a Live item is cached
**Then** it uses a bare cache key
**And** non-live items use `st:{stage_id}:{key}` prefix (NFR9)

### Story 7.5: Revision Tracking

As a site editor,
I want every edit to create a new revision and to be able to revert to a previous version,
So that content history is preserved and mistakes can be undone.

**Acceptance Criteria:**

**Given** an editor edits a conference item and saves
**When** the save completes
**Then** a new revision is created in the item_revision table
**And** the previous revision is preserved unchanged

**Given** an item has multiple revisions
**When** an editor reverts to revision N
**Then** a NEW revision is created with the content of revision N (FR38)
**And** the old revision N is not deleted or modified
**And** the revision history shows the revert as the latest entry

**Given** item save occurs
**When** the cache is updated
**Then** tag-based cache invalidation fires for the item's tags (NFR10)

### Story 7.6: Draft-While-Live & Cross-Stage Updates

As a site editor,
I want to work on a draft version of a published conference while the live version remains visible to the public,
So that editorial work doesn't disrupt the published site.

**Acceptance Criteria:**

**Given** a conference is on the Live stage
**When** an editor creates a Curated draft
**Then** the Live version remains publicly visible
**And** the Curated draft is visible only to users with appropriate permissions

**Given** a Live conference has a Curated draft
**When** an anonymous user visits the conference detail page
**Then** they see the Live version (not the draft)

**Given** the importer updates a Live item that has a Curated draft
**When** the update is processed
**Then** the kernel writes one revision with `other_stage_revisions` context (FR40)
**And** neither the Live version nor the Curated draft is corrupted

### Story 7.7: Emergency Unpublish

As a site administrator,
I want to immediately remove a Live item from public view without going through the workflow,
So that problematic content can be taken down instantly.

**Acceptance Criteria:**

**Given** a conference item is on the Live stage and publicly visible
**When** an admin sets `active=false` on the Live revision
**Then** the item is immediately removed from public listings and detail page
**And** no stage transition occurs (the item remains on the Live stage)
**And** the action is recorded in the revision history

**Given** an emergency-unpublished item
**When** an admin sets `active=true`
**Then** the item is restored to public visibility

### Story 7.8: Extensibility Demo — Config-Only Stage Addition

As a tutorial reader,
I want to see a new "Legal Review" stage added between Curated and Live using only config changes,
So that I understand the system's extensibility without code changes.

**Acceptance Criteria:**

**Given** the existing three-stage workflow is running
**When** a YAML config adds a "Legal Review" stage tag and new workflow transitions (curated→legal_review, legal_review→live, live→legal_review)
**Then** the new stage appears in the system
**And** items can transition through the new stage
**And** no code changes or plugin rebuilds are required (FR36)
**And** the original three transitions continue to work alongside the new ones

## Epic 8: Admin Content Management

Build the admin content list with filters, bulk stage operations, import queue management, and upgrade Tile visibility to support role-based rules.

**FRs:** FR42, FR43, FR44, FR45 | **NFRs:** NFR12

### Story 8.1: Admin Content List with Filters

As a site editor,
I want an admin content list page with filters for stage, type, author, and date,
So that I can find and manage content efficiently.

**Acceptance Criteria:**

**Given** I am logged in as an editor or admin
**When** I visit the admin content list page
**Then** all content items are listed with columns: title, type, stage, author, last modified date
**And** I can filter by stage (dropdown: All, Incoming, Curated, Live)
**And** I can filter by content type (dropdown: All, Conference, Speaker)
**And** I can filter by author (text input or dropdown)
**And** I can filter by date range
**And** filters are combinable (e.g., stage=Incoming AND type=Conference)
**And** the list is paginated

### Story 8.2: Bulk Stage Operations

As a site editor,
I want to select multiple items and change their stage in bulk,
So that I can process batches of content efficiently.

**Acceptance Criteria:**

**Given** I am on the admin content list with items displayed
**When** I select multiple items via checkboxes and choose "Change stage" from the bulk actions dropdown
**Then** I am prompted to select a target stage

**Given** I select a valid target stage for the bulk action
**When** the operation executes
**Then** each selected item's stage is changed if the transition is valid for that item
**And** the operation respects workflow permissions (items I lack permission to transition are skipped with a warning)
**And** a summary is displayed: N items transitioned, M items skipped

**Given** I attempt a bulk stage change without selecting any items
**When** I submit the action
**Then** a validation error is displayed

### Story 8.3: Import Queue Management

As a site administrator,
I want an admin page showing Incoming items from the importer with approve and reject actions,
So that I can review and triage imported content.

**Acceptance Criteria:**

**Given** conferences have been imported and are on the Incoming stage
**When** I visit the import queue admin page
**Then** I see a list of Incoming items sorted by import date (newest first)
**And** each item shows: title, source, import date

**Given** I click "Approve" on an Incoming item
**When** the action processes
**Then** the item transitions from Incoming to Curated (using the workflow transition)
**And** the transition respects permissions (must have "transition incoming to curated" permission)

**Given** I click "Reject" on an Incoming item
**When** the action processes
**Then** the item is marked as rejected (active=false or deleted, per policy)

### Story 8.4: Role-Based Tile Visibility

As a site administrator,
I want Tile visibility rules to support role-based conditions in addition to path-based rules,
So that certain Tiles are shown only to specific user roles.

**Acceptance Criteria:**

**Given** a Tile is configured with visibility role "editor"
**When** an editor views a page where the Tile's path rules match
**Then** the Tile is rendered

**Given** the same Tile with role "editor"
**When** an anonymous user views the same page
**Then** the Tile is NOT rendered

**Given** a Tile with both path and role visibility rules
**When** a user visits a matching path but does not have the required role
**Then** the Tile is NOT rendered (both conditions must be satisfied)

**Given** a Tile with no role restriction (only path rules)
**When** any user visits a matching path
**Then** the Tile is rendered regardless of role (backward compatible with Part 3 behavior)
