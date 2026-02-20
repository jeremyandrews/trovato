---
stepsCompleted: ['step-01-requirements-extraction', 'step-02-design-epics', 'step-03-create-stories', 'step-04-final-validation']
status: complete
inputDocuments:
  - docs/design/Overview.md
  - docs/design/Phases.md
  - docs/design/Terminology.md
  - docs/design/Design-Web-Layer.md
  - docs/design/Design-Plugin-System.md
  - docs/design/Design-Plugin-SDK.md
  - docs/design/Design-Content-Model.md
  - docs/design/Design-Query-Engine.md
  - docs/design/Design-Render-Theme.md
  - docs/design/Design-Infrastructure.md
  - docs/design/Design-Project-Meta.md
elicitationApplied:
  - User Persona Focus Group (merged epics, added UI stories)
  - Cross-Functional War Room (scoped MVPs, resolved dependencies)
---

# Trovato - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for Trovato, decomposing the requirements from the design documents into implementable stories. Trovato is a Rust-based CMS reimagining Drupal 6's mental model with modern foundations: WASM-sandboxed plugins, JSONB field storage, a JSON Render Tree for security, and Stages from day one.

## Requirements Inventory

### Functional Requirements

**Phase 0: Critical Spike**
- FR-0.1: Benchmark handle-based vs full-serialization data access (500 calls each measuring read 3 fields + write 1 field + return render element)
- FR-0.2: Benchmark Store pooling under concurrency (100 parallel requests instantiating plugins and calling taps)
- FR-0.3: Validate async host functions (WASM → host → SQLx query → return without deadlocks)
- FR-0.4: Produce written recommendation on data access mode with benchmark numbers

**Phase 1: Skeleton**
- FR-1.1: Axum web server with static route definitions and fallback dynamic handler
- FR-1.2: PostgreSQL database connection using SQLx with pooling
- FR-1.3: Redis connection for sessions using tower-sessions
- FR-1.4: User authentication with Argon2id password hashing
- FR-1.5: Session management storing user ID and active stage
- FR-1.6: Profiling middleware (Gander) logging request duration and breakdown
- FR-1.7: Queue API trait design for future queue workers
- FR-1.8: Role and permission system with DashMap-based fast lookups
- FR-1.9: Health check endpoint verifying Postgres and Redis connectivity
- FR-1.10: User CRUD with is_admin flag replacing User 1 magic

**Phase 2: Plugin Kernel + SDK**
- FR-2.1: Plugin SDK crate with proc macros (#[plugin_info], #[plugin_tap])
- FR-2.2: Reference plugins source files (blog, page, categories) as specifications
- FR-2.3: WASM plugin loader reading .info.toml and compiling .wasm files
- FR-2.4: Tap registry with weight-based ordering
- FR-2.5: RequestState with lazy plugin instantiation per-request
- FR-2.6: Host functions: item-api (get/set title, fields, metadata)
- FR-2.7: Host functions: db interface (select, insert, update, delete)
- FR-2.8: Host functions: variables (get/set persistent config)
- FR-2.9: Host functions: request-context (per-request shared state)
- FR-2.10: Host functions: user-api (current_user_id, has_permission)
- FR-2.11: Host functions: cache-api (get, set with tags, invalidate_tag)
- FR-2.12: Host functions: logging (log with level and plugin name)
- FR-2.13: Handle-based data access (ItemHandle with i32 index)
- FR-2.14: Full-serialization data access opt-in via .info.toml
- FR-2.15: Menu registry with path pattern matching and breadcrumb support
- FR-2.16: Plugin dependency topological sort

**Phase 3: Content, Fields, & Stages**
- FR-3.1: Content type definition and admin management
- FR-3.2: JSONB field storage with field type definitions
- FR-3.3: Item CRUD operations (create, load, update, delete)
- FR-3.4: Tap invocation during CRUD (tap_item_insert, tap_item_update, tap_item_delete)
- FR-3.5: Stage schema (stage table, stage_association, stage_deletion)
- FR-3.6: Stage-aware item loading with revision overrides
- FR-3.7: Text format filter pipeline with configurable filter chains
- FR-3.8: Revision system creating new revision on every save
- FR-3.9: Item access control (tap_item_access with Grant/Deny/Neutral aggregation)
- FR-3.10: Auto-generated admin forms for content type testing

**Phase 4: Gather Query Engine & Categories**
- FR-4.1: SeaQuery-based view query builder
- FR-4.2: View definitions with fields, filters, sorts, relationships, arguments, pager
- FR-4.3: Filter operators (Equals, Contains, Between, In, IsNull, etc.)
- FR-4.4: JSONB field query support with automatic numeric casting
- FR-4.5: Stage-aware Gather queries using CTE wrapping
- FR-4.6: Categories vocabularies CRUD
- FR-4.7: Categories terms CRUD with hierarchy
- FR-4.8: Recursive CTEs for hierarchical term queries
- FR-4.9: Breadcrumb generation for category navigation
- FR-4.10: Inter-plugin communication (invoke_plugin host function)
- FR-4.11: Basic exposed filters from URL query parameters

**Phase 5: Form API, Theming, & Admin UI**
- FR-5.1: Declarative Form definitions with element types
- FR-5.2: Form rendering to HTML via Tera templates
- FR-5.3: Form validation pipeline with required field checking
- FR-5.4: Form submission pipeline with plugin validation
- FR-5.5: tap_form_alter for plugin form modifications
- FR-5.6: CSRF token protection for all forms
- FR-5.7: Form state cache in Postgres for multi-step forms
- FR-5.8: AJAX form operations (add another item)
- FR-5.9: Tera template engine with layered resolution
- FR-5.10: Template suggestions (item--{type}--{id}.html, item--{type}.html, item.html)
- FR-5.11: Preprocess taps (tap_preprocess_item) for template variables
- FR-5.12: RenderElement JSON to HTML conversion
- FR-5.13: Full exposed filter forms rendered in Gather output

**Phase 6: Files, Search, Cron, & Hardening**
- FR-6.1: File upload via multipart form data
- FR-6.2: File storage backends (LocalFileStorage, S3FileStorage)
- FR-6.3: Temporary file cleanup via cron
- FR-6.4: File reference tracking on item save
- FR-6.5: PostgreSQL full-text search with tsvector
- FR-6.6: Dynamic search field configuration per content type
- FR-6.7: Search trigger updating search_vector column
- FR-6.8: Cron with Redis distributed locking and heartbeat
- FR-6.9: Queue workers processing tap_queue_worker
- FR-6.10: Rate limiting via Tower middleware
- FR-6.11: Prometheus metrics endpoint
- FR-6.12: Batch API for long-running operations

### Non-Functional Requirements

**Performance**
- NFR-1: Store pooling instantiation target: ~5µs per request
- NFR-2: Handle-based access must be >5x faster than full serialization for item taps
- NFR-3: <10ms p95 latency for Store pooling under 100 concurrent requests
- NFR-4: tap_item_view must be O(1) per call (runs ~500x per page render)
- NFR-5: Cache hit rates optimized via two-tier caching (L1 Moka + L2 Redis)
- NFR-6: JSONB field queries must support expression indexes for performance

**Security**
- NFR-7: Plugins sandboxed via WASM with no filesystem/network access by default
- NFR-8: No raw HTML from plugins - JSON RenderElements only, Kernel sanitizes
- NFR-9: Structured DB API prevents SQL injection from plugins
- NFR-10: raw_sql permission required and explicitly granted for direct SQL
- NFR-11: Session cookies: HttpOnly, Secure, SameSite=Strict
- NFR-12: Argon2id for password hashing
- NFR-13: CSRF protection on all form submissions
- NFR-14: Plugin panic catching - plugin crash never brings down Kernel

**Reliability**
- NFR-15: Health check returns 503 if Postgres or Redis unreachable
- NFR-16: Graceful degradation on plugin errors
- NFR-17: Wasmtime traps on plugin panic, Kernel continues
- NFR-18: Cron lock heartbeat prevents premature expiration

**Maintainability**
- NFR-19: Workspace dependencies in root Cargo.toml only
- NFR-20: Rust Edition 2024 - do not downgrade
- NFR-21: Plugin rebuild detection via build.rs rerun-if-changed
- NFR-22: SQLx offline mode with committed .sqlx/ directory

**Observability**
- NFR-23: Structured logging with tracing crate
- NFR-24: Request duration breakdown in profiling middleware
- NFR-25: WASM tap invocation duration metrics
- NFR-26: Database query duration metrics

### Additional Requirements

**From Architecture/Design:**
- AR-1: Use Trovato terminology (item not node, tap not hook, plugin not module, gather not views)
- AR-2: UUIDv7 for all entity IDs (time-sortable, eliminates enumeration attacks)
- AR-3: Stage as content staging replacement (edit in stages, preview safely, publish atomically)
- AR-4: is_admin boolean replaces Drupal's User 1 magic
- AR-5: Pooled Store model for WASM concurrency
- AR-6: SDK-first plugin design (write code you want devs to write, then build host)
- AR-7: wit-bindgen for WIT interface code generation
- AR-8: WASI Preview 1 core modules (not Component Model yet)
- AR-9: wasm32-wasip1 compilation target for plugins
- AR-10: cdylib crate type required for plugins
- AR-11: Tag-based cache invalidation with Redis Lua scripts
- AR-12: Stage-scoped cache keys prevent preview pollution
- AR-13: Two-layer testing: MockKernel (unit) + TestEnvironment (integration)

**Testing Requirements:**
- TR-1: Every tap function needs happy path + error path tests
- TR-2: tap_item_view: test with 0, 1, and many items
- TR-3: Lifecycle taps: test fresh install AND upgrade scenarios
- TR-4: Integration tests must load actual .wasm files
- TR-5: No phase gate passes without test coverage

### FR Coverage Map

| FR | Epic | Description |
|----|------|-------------|
| FR-0.1 | Epic 1 | Benchmark handle vs full serialization |
| FR-0.2 | Epic 1 | Benchmark Store pooling concurrency |
| FR-0.3 | Epic 1 | Validate async host functions |
| FR-0.4 | Epic 1 | Written recommendation |
| FR-1.1 | Epic 2 | Axum web server |
| FR-1.2 | Epic 2 | PostgreSQL connection |
| FR-1.3 | Epic 2 | Redis sessions |
| FR-1.4 | Epic 2 | User authentication |
| FR-1.5 | Epic 2 | Session management |
| FR-1.6 | Epic 5 | Profiling middleware |
| FR-1.7 | Epic 5 | Queue API design |
| FR-1.8 | Epic 2 | Role/permission system |
| FR-1.9 | Epic 2 | Health check |
| FR-1.10 | Epic 2 | User CRUD with is_admin |
| FR-2.1 - FR-2.16 | Epic 3 | Plugin SDK + Kernel |
| FR-3.1 | Epic 4 | Content type definition |
| FR-3.2 | Epic 4 | JSONB field storage |
| FR-3.3 | Epic 4 | Item CRUD |
| FR-3.4 | Epic 4 | CRUD tap invocation |
| FR-3.5 | Epic 6 | Stage schema |
| FR-3.6 | Epic 6 | Stage-aware loading |
| FR-3.7 | Epic 4 | Text format filters |
| FR-3.8 | Epic 4 | Revision system |
| FR-3.9 | Epic 5 | Item access control |
| FR-3.10 | Epic 4 | Auto-generated forms |
| FR-4.1 - FR-4.5 | Epic 7 | Gather query engine |
| FR-4.6 - FR-4.9 | Epic 8 | Categories |
| FR-4.10 | Epic 8 | Inter-plugin invoke |
| FR-4.11 | Epic 7 | Exposed filters |
| FR-5.1 - FR-5.8 | Epic 9 | Form API |
| FR-5.9 - FR-5.12 | Epic 10 | Theme engine |
| FR-5.13 | Epic 9 | Exposed filter forms |
| FR-6.1 - FR-6.4 | Epic 11 | File management |
| FR-6.5 - FR-6.7 | Epic 12 | Search |
| FR-6.8 - FR-6.9, FR-6.12 | Epic 13 | Cron & queues |
| FR-6.10 - FR-6.11 | Epic 14 | Production readiness |

## Epic List

| Epic | Title | Phase | Gate Criteria |
|------|-------|-------|---------------|
| 1 | Platform Architecture Validation | 0 | Written recommendation on data access mode with benchmarks |
| 2 | User Authentication & Access Control | 1 | User can log in, reset password, see stage in session, log out |
| 3 | Plugin Development Platform | 2 | Blog plugin loads, reads fields via host functions, returns RenderElement |
| 4 | Content Modeling & Basic CRUD | 3 | Create content type with 5 fields, CRUD an item with auto-generated forms |
| 5 | Content Access Control | 3 | Users can only edit own content, access denied pages work |
| 6 | Content Staging Workflow | 3 | Create stage, edit in stage, publish to live |
| 7 | Dynamic Content Listings (Gather) | 4 | Code-defined view with filters + pager renders correctly |
| 8 | Content Categorization | 4 | Vocabulary/term admin UI, category filter in Gather |
| 9 | Form API | 5 | Forms with tap_form_alter, validation, AJAX support |
| 10 | Themed Content Presentation | 5 | Template suggestions work, preprocess taps add variables |
| 11 | File & Media Management | 6 | Upload files inline, S3 backend works |
| 12 | Content Search | 6 | Full-text search including drafts |
| 13 | Scheduled Operations & Background Tasks | 6 | Cron with distributed locking, queue workers |
| 14 | Production Readiness | 6 | Rate limiting, metrics, load testing passes |

---

## Epic 1: Platform Architecture Validation

**Goal:** The team has documented proof that WASM architecture is production-viable, giving plugin developers confidence to invest in the platform.

**FRs covered:** FR-0.1, FR-0.2, FR-0.3, FR-0.4

**Scope:**
- Benchmarks for handle-based vs full-serialization (500 calls)
- Benchmarks for Store pooling (100 concurrent requests)
- Async host function validation (no deadlocks)
- Written recommendation with fallback plan if results are poor

**Gate:** Written recommendation on data access mode with benchmark numbers. If handle-based >5x faster, it becomes default. Include fallback options if benchmarks fail.

---

## Epic 2: User Authentication & Access Control

**Goal:** Administrators can log in securely, reset forgotten passwords, manage sessions, and control access. Accounts lock after repeated failed attempts.

**FRs covered:** FR-1.1, FR-1.2, FR-1.3, FR-1.4, FR-1.5, FR-1.8, FR-1.9, FR-1.10

**Scope:**
- Axum server with PostgreSQL and Redis connections
- User authentication with Argon2id
- Session management with stage tracking
- Role and permission system
- Health check endpoint
- Password reset flow
- "Remember me" functionality
- Account lockout after failed attempts

**Gate:** User can log in, reset password, see active stage in session, log out.

---

## Epic 3: Plugin Development Platform

**Goal:** Plugin developers can write plugins using the Rust SDK, access all host functions, compile to WASM, and load them into the Kernel. Clear error messages guide developers when things go wrong.

**FRs covered:** FR-2.1 through FR-2.16

**Scope:**
- Plugin SDK crate with proc macros (#[plugin_info], #[plugin_tap])
- Reference plugin source files as specifications
- WASM plugin loader reading .info.toml
- Tap registry with weight-based ordering
- RequestState with lazy plugin instantiation
- All host functions: item-api, db, variables, request-context, user-api, cache-api, logging
- Handle-based and full-serialization data access modes
- Menu registry with breadcrumb support
- Plugin dependency topological sort

**DX Stories (added via elicitation):**
- Plugin developer sees helpful error when tap signature doesn't match WIT
- Plugin developer sees clear compilation errors with actionable fixes
- Plugin load failure includes plugin name, expected vs actual exports

**Gate:** Blog plugin registers route, reads fields via handle-based host functions, returns JSON RenderElement. Source code looks clean.

**Scope Note:** Time-boxed to 8 weeks. Define "good enough for gate" vs "full vision."

---

## Epic 4: Content Modeling & Basic CRUD

**Goal:** Site administrators can define content types with custom fields (via code/config), and immediately create/edit/delete items using auto-generated admin forms. Revision history is viewable and restorable.

**FRs covered:** FR-3.1, FR-3.2, FR-3.3, FR-3.4, FR-3.7, FR-3.8, FR-3.10

**Scope:**
- Content type definition via code/config (admin UI deferred)
- JSONB field storage with field type definitions
- Item CRUD operations with tap invocation
- Text format filter pipeline
- Revision system (new revision on every save)
- Auto-generated admin forms (not full Form API)
- Revision history UI to view and restore

**Gate:** Create content type with 5 fields, create/edit/delete items, view revision history and restore.

---

## Epic 5: Content Access Control

**Goal:** Administrators can define granular per-item access rules; plugins can participate in access decisions. Users can only edit their own content unless granted broader permissions. Access denied pages are informative without leaking sensitive information.

**FRs covered:** FR-3.9, FR-1.6, FR-1.7

**Scope:**
- tap_item_access with Grant/Deny/Neutral aggregation
- "Edit own content" access pattern
- Unpublished content visibility rules
- Access Denied page UX with appropriate messaging
- Profiling middleware integration
- Queue API trait design

**Gate:** Non-admin user can create content and edit only their own items. Unpublished items invisible to users without permission.

---

## Epic 6: Content Staging Workflow

**Goal:** Editors can create isolated stages, edit content safely, preview changes, and publish to live. UI clearly explains what "Publish" will do.

**FRs covered:** FR-3.5, FR-3.6, FR-3.11

**Scope (MVP):**
- Stage schema (stage table, stage_association, stage_deletion)
- Stage-aware item loading with revision overrides
- Create stage, edit in stage, publish stage
- Conflict detection warning on publish
- Clear UI feedback on publish action

**Deferred:**
- Diff view ("what changed in my stage")
- Merge conflict resolution UI

**Gate:** Create a stage, edit items in stage, publish stage to live. Warning shown if live content changed since stage created.

---

## Epic 7: Dynamic Content Listings (Gather)

**Goal:** Developers can define dynamic content listings ("Recent Articles", "My Drafts") in code with filters, sorting, and pagination. Listings are stage-aware.

**FRs covered:** FR-4.1, FR-4.2, FR-4.3, FR-4.4, FR-4.5, FR-4.11

**Scope:**
- SeaQuery-based view query builder
- View definitions with fields, filters, sorts, relationships, pager
- Filter operators (Equals, Contains, Between, In, IsNull, etc.)
- JSONB field query support with numeric casting
- Stage-aware queries using CTE wrapping
- Exposed filters via URL query parameters

**Scope Clarification:** Code-defined Gathers only. Admin UI for Gather builder = future epic.

**Gate:** "Recent Articles" view with category filter and pager renders correctly with stage filtering.

---

## Epic 8: Content Categorization

**Goal:** Administrators can create vocabularies and manage terms through a basic admin UI. Content can be tagged and filtered by category with breadcrumb navigation.

**FRs covered:** FR-4.6, FR-4.7, FR-4.8, FR-4.9, FR-4.10

**Scope:**
- Categories vocabularies CRUD
- Categories terms CRUD with hierarchy
- Basic admin UI for vocabulary and term management
- Recursive CTEs for hierarchical queries
- Breadcrumb generation
- Inter-plugin communication (invoke_plugin)

**Deferred:**
- Drag-and-drop term reordering
- Fancy tree visualization

**Gate:** Create vocabulary, add hierarchical terms via admin UI, filter Gather by category, breadcrumbs work.

---

## Epic 9: Form API

**Goal:** Developers can define complex forms with validation pipelines. Plugins can alter forms via tap_form_alter. Forms support AJAX operations and CSRF protection.

**FRs covered:** FR-5.1, FR-5.2, FR-5.3, FR-5.4, FR-5.5, FR-5.6, FR-5.7, FR-5.8, FR-5.13

**Scope:**
- Declarative Form definitions with element types
- Form rendering to HTML via Tera
- Form validation pipeline
- Form submission pipeline with plugin validation
- tap_form_alter support
- CSRF token protection
- Form state cache for multi-step forms
- AJAX operations ("Add another item")
- Exposed filter forms in Gather output
- Inline validation, clear error messages, mobile-friendly

**Gate:** Admin creates content type via form UI, forms support AJAX "Add another item", tap_form_alter modifies forms.

---

## Epic 10: Themed Content Presentation

**Goal:** Content renders through customizable Tera templates with plugin preprocessing support. Template suggestions allow per-type and per-item customization.

**FRs covered:** FR-5.9, FR-5.10, FR-5.11, FR-5.12

**Scope:**
- Tera template engine with layered resolution
- Template suggestions (item--{type}--{id}.html, item--{type}.html, item.html)
- Preprocess taps (tap_preprocess_item) for template variables
- RenderElement JSON to HTML conversion

**Deferred:** Live preview while editing

**Gate:** Template suggestions resolve correctly, preprocess taps add variables to templates.

---

## Epic 11: File & Media Management

**Goal:** Users can upload files inline (drag-and-drop), manage media library, with reliable storage (local or S3).

**FRs covered:** FR-6.1, FR-6.2, FR-6.3, FR-6.4

**Scope:**
- File upload via multipart form data
- Inline drag-and-drop uploads in editor
- File storage backends (LocalFileStorage, S3FileStorage)
- Temporary file cleanup via cron
- File reference tracking on item save

**Gate:** Upload file via drag-drop, file stored in S3, file linked to item, orphan files cleaned up.

---

## Epic 12: Content Search

**Goal:** Users can search published content and their own drafts, with configurable field weights per content type.

**FRs covered:** FR-6.5, FR-6.6, FR-6.7

**Scope:**
- PostgreSQL full-text search with tsvector
- Dynamic search field configuration per content type
- Search trigger updating search_vector
- Search includes user's own drafts

**Gate:** Search returns relevant results, field weights configurable, users can find their drafts.

---

## Epic 13: Scheduled Operations & Background Tasks

**Goal:** System reliably runs scheduled tasks and processes queued work without conflicts across servers.

**FRs covered:** FR-6.8, FR-6.9, FR-6.12

**Scope:**
- Cron with Redis distributed locking
- Lock heartbeat prevents premature expiration
- Queue workers processing tap_queue_worker
- Batch API for long-running operations

**Gate:** Cron runs on exactly one server, queue items processed reliably.

---

## Epic 14: Production Readiness

**Goal:** Operations teams can monitor system health via metrics endpoint, enforce rate limits, and handle production load.

**FRs covered:** FR-6.10, FR-6.11

**Scope:**
- Rate limiting via Tower middleware
- Prometheus metrics endpoint
- Load testing with goose

**Gate:** Rate limiting works, metrics endpoint exposes all key metrics, system handles load test.

---

# Stories

## Epic 1: Platform Architecture Validation

### Story 1.1: Benchmark Host Binary Setup

As a **kernel developer**,
I want a standalone benchmark binary that initializes Wasmtime with pooling allocator,
So that I have a foundation to run all architecture validation benchmarks.

**Acceptance Criteria:**

**Given** a fresh checkout of the repository
**When** I run `cargo build -p benchmarks-phase0`
**Then** a binary is produced that initializes Wasmtime Engine with pooling allocator config
**And** the binary can load a test plugin compiled to `wasm32-wasip1`
**And** basic host functions (log_message, get_variable) are registered

---

### Story 1.2: Test Plugin for Benchmarking

As a **kernel developer**,
I want a minimal test plugin that exercises both data access modes,
So that I can benchmark handle-based vs full-serialization performance.

**Acceptance Criteria:**

**Given** the benchmark host binary from Story 1.1
**When** I compile the test plugin with `cargo build --target wasm32-wasip1`
**Then** the plugin exports `tap_item_view` (handle-based) receiving an i32 handle
**And** the plugin exports `tap_item_view_full` (full-serialization) receiving JSON string
**And** both modes read 3 fields, write 1 field, and return a RenderElement JSON

---

### Story 1.3: Handle-Based vs Full-Serialization Benchmark

As a **kernel developer**,
I want to benchmark 500 calls each for handle-based and full-serialization modes,
So that I can determine which data access mode to use as the default.

**Acceptance Criteria:**

**Given** the benchmark host and test plugin are built
**When** I run `cargo run -p benchmarks-phase0 -- --bench serialization`
**Then** 500 calls are made using handle-based access (read 3 fields, write 1, return JSON)
**And** 500 calls are made using full-serialization (same operations)
**And** wall-clock time and p50/p95/p99 latency are reported for each mode
**And** the speedup ratio is calculated and displayed

---

### Story 1.4: Store Pooling Concurrency Benchmark

As a **kernel developer**,
I want to benchmark Store pooling under 100 concurrent requests,
So that I can validate the pooled instantiation model scales.

**Acceptance Criteria:**

**Given** the benchmark host and test plugin are built
**When** I run `cargo run -p benchmarks-phase0 -- --bench concurrency`
**Then** 100 parallel async tasks each instantiate a plugin, call a tap, and return
**And** p50/p95/p99 instantiation latency is reported
**And** p50/p95/p99 total request latency is reported
**And** instantiation must be <10ms p95 per request (NFR-3)

---

### Story 1.5: Async Host Function Validation

As a **kernel developer**,
I want to validate that async host functions work without deadlocks,
So that I can confirm WASM→host→SQLx→return is safe under Tokio.

**Acceptance Criteria:**

**Given** the benchmark host with a mock async host function (simulates SQLx delay)
**When** I run `cargo run -p benchmarks-phase0 -- --bench async`
**Then** the plugin calls an async host function that awaits a 10ms delay
**And** 100 concurrent calls complete without deadlock
**And** total execution time is reasonable (~1-2s for 100x10ms with concurrency)

---

### Story 1.6: Architecture Recommendation Document

As a **kernel developer**,
I want a written recommendation with benchmark results and fallback options,
So that the team can make an informed decision on the data access mode.

**Acceptance Criteria:**

**Given** all benchmarks from Stories 1.3, 1.4, 1.5 have been run
**When** I analyze the benchmark results
**Then** a `docs/phase-gates/phase-0-complete.md` document is created
**And** it includes benchmark numbers for all three objectives
**And** it recommends handle-based if >5x faster, otherwise full-serialization
**And** it documents fallback options (Extism, scripting language) if benchmarks fail
**And** the Phase 0 gate is documented as passed or failed with rationale

---

## Epic 2: User Authentication & Access Control

### Story 2.1: Axum Server with Database Connections

As a **kernel developer**,
I want an Axum web server with PostgreSQL and Redis connections,
So that I have the foundation for all web functionality.

**Acceptance Criteria:**

**Given** Docker containers for Postgres and Redis are running
**When** I run `cargo run -p trovato-kernel`
**Then** the server starts on port 3000 (configurable via env)
**And** SQLx connects to PostgreSQL with connection pooling
**And** Redis client connects for session storage
**And** server logs successful startup with tracing

---

### Story 2.2: User Table and CRUD Operations

As a **site administrator**,
I want user records stored in the database with proper password hashing,
So that user accounts are secure and manageable.

**Acceptance Criteria:**

**Given** the database is running
**When** the server starts
**Then** a `users` table exists with columns: id (UUIDv7), name, pass (Argon2id hash), mail, is_admin, created, access, login, status, timezone, language, data (JSONB)
**And** User 1 bypass is replaced with `is_admin` boolean
**And** anonymous user is represented by `Uuid::nil()`

---

### Story 2.3: User Login with Session

As a **site user**,
I want to log in with my username and password,
So that I can access protected functionality.

**Acceptance Criteria:**

**Given** a user exists with username "admin" and valid password
**When** I POST to `/user/login` with correct credentials
**Then** Argon2id verifies the password against the stored hash
**And** a session is created in Redis with the user ID
**And** session cookie is set with HttpOnly, Secure, SameSite=Strict (NFR-11)
**And** `login` and `access` timestamps are updated

**Given** invalid credentials are submitted
**When** I POST to `/user/login`
**Then** 401 Unauthorized is returned
**And** no session is created
**And** error message does not reveal whether username or password was wrong

---

### Story 2.4: User Logout

As a **logged-in user**,
I want to log out of the system,
So that my session is terminated securely.

**Acceptance Criteria:**

**Given** I am logged in with a valid session
**When** I GET `/user/logout`
**Then** my session is deleted from Redis
**And** session cookie is cleared
**And** I am redirected to the home page

---

### Story 2.5: Session with Stage Tracking

As a **content editor**,
I want my session to track my active stage,
So that I see the correct content version when working.

**Acceptance Criteria:**

**Given** I am logged in
**When** I access any page
**Then** my session includes `active_stage` (defaults to "live" or None)
**And** the stage context is available to all handlers via session extractor

**Given** I switch stages via admin toolbar
**When** I POST to `/admin/stage/switch` with stage_id
**Then** my session `active_stage` is updated
**And** subsequent requests use the new stage context

---

### Story 2.6: Role and Permission System

As a **site administrator**,
I want to assign roles to users and permissions to roles,
So that I can control who can do what.

**Acceptance Criteria:**

**Given** the database is running
**When** the server starts
**Then** `role` table exists with id, name (unique)
**And** `role_permission` table exists with role_id, permission
**And** `users_roles` table exists with user_id, role_id
**And** "anonymous user" and "authenticated user" roles are seeded

**Given** a user with roles
**When** `user_has_permission(state, user, "access content")` is called
**Then** DashMap is checked for fast lookup
**And** is_admin users always return true
**And** anonymous users check only anonymous role permissions

---

### Story 2.7: Password Reset Flow

As a **user who forgot their password**,
I want to reset my password via email,
So that I can regain access to my account.

**Acceptance Criteria:**

**Given** I have an account with email "user@example.com"
**When** I POST to `/user/password-reset` with my email
**Then** a time-limited reset token is generated and stored
**And** an email would be sent (mock/log for MVP)
**And** success message shown regardless of whether email exists (security)

**Given** I have a valid reset token
**When** I GET `/user/password-reset/{token}`
**Then** I see a form to enter a new password

**When** I POST the new password
**Then** my password is updated with Argon2id
**And** the token is invalidated
**And** I am redirected to login

---

### Story 2.8: Account Lockout After Failed Attempts

As a **security-conscious administrator**,
I want accounts to lock after repeated failed login attempts,
So that brute force attacks are mitigated.

**Acceptance Criteria:**

**Given** a user account exists
**When** 5 failed login attempts occur within 15 minutes
**Then** the account is temporarily locked for 15 minutes
**And** subsequent login attempts return "Account temporarily locked"
**And** failed attempt count is stored in Redis with TTL

**Given** an account is locked
**When** the lockout period expires
**Then** login attempts are allowed again
**And** successful login resets the failed attempt counter

---

### Story 2.9: Health Check Endpoint

As an **operations engineer**,
I want a health check endpoint,
So that load balancers know if the server is healthy.

**Acceptance Criteria:**

**Given** the server is running
**When** I GET `/health`
**Then** if Postgres AND Redis are reachable, return 200 OK with `{"status": "healthy"}`
**And** if either is unreachable, return 503 with `{"status": "unhealthy", "postgres": true/false, "redis": true/false}`
**And** response time is <100ms (don't run expensive queries)

---

### Story 2.10: Remember Me Functionality

As a **returning user**,
I want to stay logged in across browser sessions,
So that I don't have to log in every time.

**Acceptance Criteria:**

**Given** I am on the login form
**When** I check "Remember me" and submit valid credentials
**Then** session expiry is extended to 30 days (vs 24 hours default)
**And** a longer-lived session token is stored

**Given** I don't check "Remember me"
**When** I log in
**Then** session expires after 24 hours of inactivity (default)

---

## Epic 3: Plugin Development Platform

### Story 3.1: Plugin SDK Crate Structure

As a **plugin developer**,
I want a `trovato_sdk` crate with core types and prelude,
So that I can import everything I need with `use trovato_sdk::prelude::*`.

**Acceptance Criteria:**

**Given** the workspace is set up
**When** I add `trovato_sdk` as a dependency
**Then** `trovato_sdk::prelude::*` exports: ItemHandle, TapContext, RenderElement, ContentTypeDefinition, FieldDefinition, FieldType, MenuDefinition, PermissionDefinition, AccessResult
**And** the crate compiles for both native (tests) and `wasm32-wasip1` targets
**And** `[lib] crate-type = ["cdylib", "rlib"]` is configured

---

### Story 3.2: Plugin Tap Proc Macro

As a **plugin developer**,
I want a `#[plugin_tap]` proc macro that generates WASM exports,
So that I don't have to write WASM boilerplate manually.

**Acceptance Criteria:**

**Given** I write a function with `#[plugin_tap]`
**When** I compile to `wasm32-wasip1`
**Then** the macro generates a WASM export named `tap-{function_name}` (e.g., `tap-item-view`)
**And** for `&ItemHandle` parameter, it generates handle-based export receiving i32
**And** for `&Item` parameter, it generates full-serialization export receiving JSON string
**And** return values are serialized to JSON automatically
**And** panics are caught and converted to error responses

---

### Story 3.3: WIT Interface Definition

As a **kernel developer**,
I want the complete WIT interface defined in `crates/wit/kernel.wit`,
So that both SDK and Kernel can generate bindings from the same source.

**Acceptance Criteria:**

**Given** the WIT file exists
**When** I review the interface
**Then** it includes: item-api, db, variables, request-context, user-api, cache-api, plugin-api, logging interfaces
**And** plugin world exports all tap functions (lifecycle, item CRUD, menu, perm, etc.)
**And** handle-based exports use `s32` for item-handle
**And** full-serialization exports are suffixed with `-full`

---

### Story 3.4: Plugin Info Manifest Parser

As a **kernel developer**,
I want to parse `.info.toml` files for plugin metadata,
So that the Kernel knows what taps each plugin implements.

**Acceptance Criteria:**

**Given** a plugin directory with `{name}.info.toml`
**When** the Kernel scans the plugins directory
**Then** it parses: name, version, description, dependencies, taps.implements, taps.options
**And** `data_mode` defaults to "handle" if not specified
**And** invalid TOML produces a clear error with file path and line number

---

### Story 3.5: WASM Plugin Loader

As a **kernel developer**,
I want to compile and load WASM plugins at startup,
So that plugins can be invoked at runtime.

**Acceptance Criteria:**

**Given** enabled plugins in the `system` table
**When** the Kernel starts
**Then** each plugin's `.wasm` file is compiled to a Wasmtime `Module`
**And** plugins are stored in `PluginRegistry` (compiled modules are Send+Sync)
**And** compilation errors include plugin name and wasmtime error details
**And** missing `.wasm` file produces clear error: "Plugin 'blog' wasm file not found at plugins/blog/blog.wasm"

---

### Story 3.6: Plugin Dependency Resolution

As a **kernel developer**,
I want plugins loaded in dependency order,
So that a plugin's dependencies are always available when it loads.

**Acceptance Criteria:**

**Given** plugins with dependencies declared in `.info.toml`
**When** the Kernel loads plugins
**Then** topological sort orders plugins by dependencies
**And** circular dependencies produce error: "Circular dependency detected: blog → categories → blog"
**And** missing dependencies produce error: "Plugin 'blog' requires 'item' which is not enabled"

---

### Story 3.7: Tap Registry

As a **kernel developer**,
I want a registry of which plugins implement which taps,
So that I can invoke all implementors of a tap efficiently.

**Acceptance Criteria:**

**Given** plugins are loaded with their tap declarations
**When** `tap_registry.get_implementors("tap_item_view")` is called
**Then** it returns all plugins implementing that tap, sorted by weight
**And** weight defaults to 0, lower weights run first
**And** equal weights sort by load order (dependency order)

---

### Story 3.8: RequestState and Lazy Plugin Instantiation

As a **kernel developer**,
I want per-request plugin instances via RequestState,
So that concurrent requests are isolated and Store reuse is efficient.

**Acceptance Criteria:**

**Given** a request is being processed
**When** a tap needs to be invoked
**Then** `RequestState.get_or_create_store(plugin)` lazily instantiates the plugin
**And** the same Store is reused for multiple taps on the same plugin within the request
**And** Stores are dropped at the end of the request
**And** instantiation uses pooling allocator (~5µs target)

---

### Story 3.9: Host Function - Item API

As a **plugin developer**,
I want host functions to read and write item fields,
So that my plugin can access content without full serialization.

**Acceptance Criteria:**

**Given** a plugin is invoked with an ItemHandle
**When** the plugin calls `get_title(handle)`, `get_field_string(handle, "field_body")`, etc.
**Then** the Kernel looks up the item in RequestState by handle index
**And** returns the requested field value
**And** `set_field_string(handle, "field_computed", "value")` writes to the item
**And** invalid handle returns empty/error (doesn't panic)
**And** type mismatches (e.g., `get_field_int` on a string field) return None

---

### Story 3.10: Host Function - Database API

As a **plugin developer**,
I want host functions for structured database queries,
So that I can read/write data safely without SQL injection risk.

**Acceptance Criteria:**

**Given** a plugin calls `db_select(query_json)`
**When** the query JSON is valid (table, fields, conditions, order_by, limit)
**Then** the Kernel builds a SeaQuery query and executes it
**And** results are returned as JSON array

**Given** a plugin calls `db_insert(table, data_json)`
**When** the data is valid
**Then** a new row is inserted and the UUID is returned

**And** raw SQL is blocked unless plugin has `raw_sql` permission in `.info.toml`

---

### Story 3.11: Host Function - User API

As a **plugin developer**,
I want host functions to check user permissions,
So that my plugin can enforce access control.

**Acceptance Criteria:**

**Given** a plugin is invoked
**When** it calls `current_user_id()`
**Then** the current user's UUID is returned (or nil UUID for anonymous)

**When** it calls `current_user_has_permission("edit own blog content")`
**Then** true/false is returned based on the user's roles

---

### Story 3.12: Host Function - Cache API

As a **plugin developer**,
I want host functions for caching,
So that my plugin can cache expensive computations.

**Acceptance Criteria:**

**Given** a plugin calls `cache_set(bin, key, value, tags_json)`
**When** the call completes
**Then** the value is stored in L2 Redis cache with the specified tags

**Given** a plugin calls `cache_get(bin, key)`
**When** the key exists
**Then** the cached value is returned
**When** the key doesn't exist
**Then** None is returned

**Given** a plugin calls `cache_invalidate_tag(tag)`
**When** the call completes
**Then** all cache entries with that tag are deleted

---

### Story 3.13: Host Function - Variables and Request Context

As a **plugin developer**,
I want host functions for persistent config and per-request state,
So that my plugin can store settings and share data with other plugins.

**Acceptance Criteria:**

**Given** a plugin calls `variable_get("blog_posts_per_page", "10")`
**When** the variable exists
**Then** the stored value is returned
**When** the variable doesn't exist
**Then** the default value is returned

**Given** a plugin calls `variable_set("blog_posts_per_page", "25")`
**Then** the value is persisted to the database

**Given** a plugin calls `request_context_set("computed_title", "My Title")`
**Then** other plugins in the same request can read it with `request_context_get`

---

### Story 3.14: Host Function - Logging

As a **plugin developer**,
I want a host function for logging,
So that I can debug my plugin and report errors.

**Acceptance Criteria:**

**Given** a plugin calls `log("info", "blog", "Processing item 123")`
**When** the call completes
**Then** the message is logged via tracing with structured fields: level, plugin, message
**And** log levels supported: debug, info, warning, error

---

### Story 3.15: Tap Dispatcher

As a **kernel developer**,
I want to invoke all plugins implementing a tap with proper error handling,
So that taps execute reliably even if one plugin fails.

**Acceptance Criteria:**

**Given** a tap needs to be invoked (e.g., `tap_item_view`)
**When** `invoke_tap(request_state, "tap_item_view", item_handle)` is called
**Then** all implementing plugins are invoked in weight order
**And** results are collected (for view taps) or mutations accumulated (for alter taps)
**And** if a plugin panics, it is caught and logged, other plugins continue
**And** if a plugin returns an error, it is logged, processing continues

---

### Story 3.16: Menu Registry

As a **plugin developer**,
I want my plugin's routes registered via `tap_menu`,
So that users can access my plugin's pages.

**Acceptance Criteria:**

**Given** a plugin implements `tap_menu` returning MenuDefinitions
**When** the Kernel builds the menu registry
**Then** exact paths are stored for fast lookup
**And** wildcard paths (e.g., `/blog/{id}`) are stored with pattern matching
**And** `menu_registry.resolve("/blog/123")` returns the matching MenuItem
**And** breadcrumb generation walks parent chain

---

### Story 3.17: Reference Blog Plugin

As a **plugin developer**,
I want a working blog plugin as a reference,
So that I can learn from a real example.

**Acceptance Criteria:**

**Given** the blog plugin source in `plugins/blog/`
**When** I review the code
**Then** it demonstrates: `#[plugin_tap]` usage, `tap_item_info` returning ContentTypeDefinition, `tap_item_view` reading fields via ItemHandle, `tap_menu` and `tap_perm` declarations
**And** the code is clean and idiomatic (no raw pointers, no JSON string manipulation)
**And** it compiles to WASM and loads successfully

---

### Story 3.18: Plugin Error Messages DX

As a **plugin developer**,
I want clear error messages when my plugin fails to load,
So that I can fix issues quickly.

**Acceptance Criteria:**

**Given** a plugin has a WIT signature mismatch
**When** the Kernel tries to invoke a tap
**Then** error includes: plugin name, tap name, expected signature, actual exports found

**Given** a plugin's `.info.toml` declares a tap it doesn't export
**When** the Kernel invokes that tap
**Then** error includes: "Plugin 'blog' declares 'tap_item_view' but doesn't export it. Check function signature matches WIT."

**Given** a plugin panics during execution
**When** the Kernel catches the trap
**Then** log includes: plugin name, tap name, panic message (if available)

---

## Epic 4: Content Modeling & Basic CRUD

### Story 4.1: Item Type Table and Schema

As a **site administrator**,
I want content types stored in the database,
So that I can define different kinds of content.

**Acceptance Criteria:**

**Given** the database is running
**When** migrations run
**Then** `item_type` table exists with columns: type (PK), label, description, settings (JSONB)
**And** a "page" type is seeded as the default content type

---

### Story 4.2: Item Table with JSONB Fields

As a **content editor**,
I want items stored with flexible field storage,
So that I can have different fields per content type without schema changes.

**Acceptance Criteria:**

**Given** the database is running
**When** migrations run
**Then** `item` table exists with: id (UUIDv7), current_revision_id, type, title, author_id, status, created, changed, promote, sticky, fields (JSONB), stage_id, search_vector
**And** `item_revision` table exists with: id (UUIDv7), item_id, title, fields (JSONB), created, log_message
**And** GIN index exists on `fields` column

---

### Story 4.3: Content Type Definition via Code

As a **plugin developer**,
I want to define content types via `tap_item_info`,
So that my plugin can register its content types.

**Acceptance Criteria:**

**Given** a plugin implements `tap_item_info`
**When** the Kernel starts
**Then** ContentTypeDefinitions are collected from all plugins
**And** each content type is registered in `item_type` table if not exists
**And** field definitions are stored in the content type's settings JSONB

---

### Story 4.4: Create Item

As a **content editor**,
I want to create new content items,
So that I can add content to the site.

**Acceptance Criteria:**

**Given** I am logged in with "create {type} content" permission
**When** I POST to `/item/add/{type}` with title and fields
**Then** a new item is created with UUIDv7
**And** a revision is created with the same content
**And** `tap_item_insert` is invoked for all implementing plugins
**And** I am redirected to the item view page

**Given** I don't have permission
**When** I try to create content
**Then** 403 Forbidden is returned

---

### Story 4.5: Load Item

As a **site visitor**,
I want to view content items,
So that I can read the site's content.

**Acceptance Criteria:**

**Given** an item exists with id "abc-123"
**When** I GET `/item/abc-123`
**Then** the item is loaded from the database
**And** `tap_item_view` is invoked with the ItemHandle
**And** the RenderElement result is converted to HTML
**And** the page is displayed

**Given** the item doesn't exist
**When** I GET `/item/nonexistent`
**Then** 404 Not Found is returned

---

### Story 4.6: Update Item

As a **content editor**,
I want to edit existing items,
So that I can correct or update content.

**Acceptance Criteria:**

**Given** I have "edit own {type} content" permission and I'm the author
**When** I GET `/item/{id}/edit`
**Then** an auto-generated edit form is displayed with current values

**When** I POST updated values
**Then** the item is updated
**And** a new revision is created (revisions are immutable)
**And** `current_revision_id` is updated to point to new revision
**And** `tap_item_update` is invoked
**And** `changed` timestamp is updated

---

### Story 4.7: Delete Item

As a **content editor**,
I want to delete items,
So that I can remove outdated content.

**Acceptance Criteria:**

**Given** I have "delete own {type} content" permission and I'm the author
**When** I POST to `/item/{id}/delete` with confirmation
**Then** `tap_item_delete` is invoked
**And** the item is deleted (or soft-deleted via status)
**And** I am redirected to the content list

---

### Story 4.8: Revision History View

As a **content editor**,
I want to see an item's revision history,
So that I can track changes over time.

**Acceptance Criteria:**

**Given** an item with multiple revisions
**When** I GET `/item/{id}/revisions`
**Then** a list of revisions is displayed with: revision id, created date, log message
**And** each revision links to a view of that revision
**And** each revision has a "Revert to this revision" button

---

### Story 4.9: Revert to Previous Revision

As a **content editor**,
I want to revert to a previous revision,
So that I can undo unwanted changes.

**Acceptance Criteria:**

**Given** I am viewing the revision history
**When** I click "Revert" on a previous revision
**Then** a new revision is created with the old revision's content
**And** `current_revision_id` is updated
**And** log message indicates "Reverted to revision {id}"
**And** `tap_item_update` is invoked

---

### Story 4.10: Text Format Filter Pipeline

As a **content editor**,
I want text fields filtered for security,
So that XSS attacks are prevented.

**Acceptance Criteria:**

**Given** a text field with `format: "filtered_html"`
**When** the RenderElement is processed
**Then** the Kernel runs the text through the filter pipeline
**And** script tags are stripped
**And** only allowed HTML tags are preserved
**And** URLs are converted to links (if configured)

**Given** a text field without `#format`
**When** the RenderElement is processed
**Then** the text is HTML-escaped (plain text)

---

### Story 4.11: Auto-Generated Admin Forms

As a **site administrator**,
I want basic CRUD forms auto-generated from field definitions,
So that I can manage content before the full Form API is built.

**Acceptance Criteria:**

**Given** a content type with field definitions
**When** I access the add/edit form
**Then** form fields are generated based on FieldType (Text → input, TextLong → textarea, Boolean → checkbox, etc.)
**And** required fields are marked and validated
**And** form submits and saves correctly
**And** this is a temporary solution replaced by Form API in Epic 9

---

## Epic 5: Content Access Control

### Story 5.1: Item Access Tap Interface

As a **plugin developer**,
I want to implement `tap_item_access` to control item access,
So that my plugin can enforce custom access rules.

**Acceptance Criteria:**

**Given** a plugin implements `tap_item_access`
**When** the tap is invoked with (item_handle, operation, context)
**Then** the plugin returns AccessResult: Grant, Deny, or Neutral
**And** operations include: "view", "edit", "delete"

---

### Story 5.2: Access Result Aggregation

As a **kernel developer**,
I want access results aggregated correctly,
So that security is enforced consistently.

**Acceptance Criteria:**

**Given** multiple plugins implement `tap_item_access`
**When** access is checked
**Then** if ANY plugin returns Deny, access is denied
**And** if NO plugin returns Deny and ANY returns Grant, access is granted
**And** if ALL return Neutral, fall back to role-based permission check
**And** aggregation stops early on first Deny (optimization)

---

### Story 5.3: Edit Own Content Pattern

As a **content editor**,
I want to edit only my own content by default,
So that I can't accidentally modify others' work.

**Acceptance Criteria:**

**Given** a user with "edit own blog content" permission
**When** they try to edit their own blog post
**Then** access is granted

**When** they try to edit someone else's blog post
**Then** access is denied (unless they have "edit any blog content")

---

### Story 5.4: Unpublished Content Visibility

As a **content editor**,
I want unpublished content hidden from regular users,
So that draft content isn't accidentally visible.

**Acceptance Criteria:**

**Given** an item with status=0 (unpublished)
**When** an anonymous user tries to view it
**Then** 404 Not Found is returned (don't reveal existence)

**When** the author tries to view it
**Then** the item is displayed

**When** a user with "view unpublished content" permission tries to view it
**Then** the item is displayed

---

### Story 5.5: Access Denied Page

As a **site visitor**,
I want a helpful access denied page,
So that I understand why I can't access something.

**Acceptance Criteria:**

**Given** access is denied to a resource
**When** the response is generated
**Then** 403 status is returned with a user-friendly page
**And** the message doesn't reveal sensitive information (no "item exists but you can't see it")
**And** if user is anonymous, a link to login is shown
**And** if user is logged in, a generic "You don't have permission" message is shown

---

### Story 5.6: Profiling Middleware

As a **kernel developer**,
I want request profiling middleware,
So that I can identify performance bottlenecks.

**Acceptance Criteria:**

**Given** a request is processed
**When** the request completes
**Then** tracing logs include: path, total duration, db query time, tap invocation time
**And** slow requests (>500ms) are logged at warning level
**And** request profile data is available via request extensions

---

### Story 5.7: Queue API Trait Design

As a **kernel developer**,
I want the Queue trait designed,
So that Epic 13 can implement queue workers.

**Acceptance Criteria:**

**Given** the trait is defined
**When** I review it
**Then** it includes: `push(item: &str)`, `pop() -> Option<String>`, `len() -> u64`
**And** the trait is async-compatible
**And** a stub implementation exists for testing

---

## Epic 6: Content Staging Workflow

### Story 6.1: Stage Table Schema

As a **site administrator**,
I want stages stored in the database,
So that I can create content staging environments.

**Acceptance Criteria:**

**Given** the database is running
**When** migrations run
**Then** `stage` table exists with: id (varchar), label, description, created, owner_id
**And** "live" stage is seeded as the default/production stage

---

### Story 6.2: Stage Association Table

As a **kernel developer**,
I want stage associations tracked,
So that items can have different revisions per stage.

**Acceptance Criteria:**

**Given** the schema exists
**When** I review it
**Then** `stage_association` table exists with: stage_id, item_id, target_revision_id
**And** primary key is (stage_id, item_id)
**And** this allows an item to point to different revisions in different stages

---

### Story 6.3: Stage Deletion Tracking

As a **kernel developer**,
I want stage deletions tracked,
So that items deleted in a stage don't appear in that stage.

**Acceptance Criteria:**

**Given** the schema exists
**When** I review it
**Then** `stage_deletion` table exists with: stage_id, entity_type, entity_id
**And** when an item is "deleted" in a stage, a record is added here
**And** the item still exists in live and other stages

---

### Story 6.4: Create Stage

As a **content editor**,
I want to create a new stage,
So that I can work on changes without affecting live.

**Acceptance Criteria:**

**Given** I have "create stage" permission
**When** I POST to `/admin/stage/create` with label "Spring Campaign"
**Then** a new stage is created with a unique id
**And** I am set as the owner
**And** my session switches to the new stage

---

### Story 6.5: Stage-Aware Item Loading

As a **content editor**,
I want items loaded with stage overrides,
So that I see my stage's version of content.

**Acceptance Criteria:**

**Given** I am in "Spring Campaign" stage
**And** item "abc" has a stage association pointing to revision "rev-2"
**When** I load item "abc"
**Then** I see the content from "rev-2", not the live revision

**Given** item "xyz" has no stage association
**When** I load item "xyz"
**Then** I see the live version

---

### Story 6.6: Edit Item in Stage

As a **content editor**,
I want edits in a stage isolated from live,
So that I can make changes without affecting production.

**Acceptance Criteria:**

**Given** I am in "Spring Campaign" stage
**When** I edit item "abc" and save
**Then** a new revision is created
**And** a stage_association is created/updated pointing to the new revision
**And** the live stage still points to the original revision
**And** other stages are unaffected

---

### Story 6.7: Publish Stage

As a **site administrator**,
I want to publish a stage to live,
So that my staged changes become visible to everyone.

**Acceptance Criteria:**

**Given** I am in "Spring Campaign" stage with pending changes
**When** I POST to `/admin/stage/{id}/publish`
**Then** for each stage_association: the item's current_revision_id is updated to the stage's revision
**And** stage_associations are deleted (merged into live)
**And** stage_deletions cause actual item deletions in live
**And** the stage can be kept (for continued work) or deleted

---

### Story 6.8: Publish Conflict Detection

As a **content editor**,
I want warnings when live content changed since I started my stage,
So that I don't accidentally overwrite someone else's work.

**Acceptance Criteria:**

**Given** I have a stage association for item "abc" created when live was at rev-1
**And** live has since been updated to rev-3
**When** I try to publish
**Then** a warning is shown: "Item 'abc' was modified in live since you started editing"
**And** I can choose to: overwrite anyway, skip this item, or cancel
**And** this is a warning, not a blocker (Last Publish Wins for MVP)

---

### Story 6.9: Publish UI Feedback

As a **content editor**,
I want clear feedback when publishing,
So that I understand what will happen.

**Acceptance Criteria:**

**Given** I am about to publish a stage
**When** I click "Publish"
**Then** a confirmation dialog shows: number of items to update, number of items to delete
**And** after publish completes, a success message shows what was published
**And** any conflicts that were overwritten are logged

---

## Epic 7: Dynamic Content Listings (Gather)

### Story 7.1: View Definition Types

As a **kernel developer**,
I want Gather view definitions as Rust types,
So that views can be defined in code.

**Acceptance Criteria:**

**Given** the types are defined
**When** I review them
**Then** ViewDefinition has: name, base_table, displays
**And** ViewDisplay has: id, display_type, fields, filters, sorts, relationships, arguments, pager, path, style
**And** ViewField, ViewFilter, ViewSort, ViewRelationship, ViewArgument are defined per design doc

---

### Story 7.2: SeaQuery View Builder

As a **kernel developer**,
I want a ViewQueryBuilder that generates SQL from ViewDefinition,
So that views execute efficiently.

**Acceptance Criteria:**

**Given** a ViewDefinition with fields, filters, sorts
**When** `ViewQueryBuilder::build(display, base_table, args)` is called
**Then** a valid PostgreSQL query string and parameters are returned
**And** fields are selected (with JSONB extraction for custom fields)
**And** filters are applied as WHERE conditions
**And** sorts are applied as ORDER BY
**And** LIMIT and OFFSET are applied from pager settings

---

### Story 7.3: Filter Operators

As a **plugin developer**,
I want standard filter operators in Gather,
So that I can filter content flexibly.

**Acceptance Criteria:**

**Given** a ViewFilter with various operators
**When** the query is built
**Then** Equals → `=`, NotEquals → `!=`, Contains → `LIKE %x%`, StartsWith → `LIKE x%`
**And** GreaterThan/LessThan/GTE/LTE work with numeric values
**And** In/NotIn work with arrays
**And** IsNull/IsNotNull work without values
**And** Between works with [min, max] arrays

---

### Story 7.4: JSONB Field Queries

As a **plugin developer**,
I want to filter and sort by JSONB fields,
So that custom fields are queryable.

**Acceptance Criteria:**

**Given** a filter on `field_rating > 4`
**When** the query is built
**Then** the condition is: `(fields->'field_rating'->>'value')::numeric > 4`
**And** numeric casting is automatic for comparison operators
**And** string comparisons don't cast

**Given** a sort by `field_date DESC`
**When** the query is built
**Then** ORDER BY uses JSONB extraction

---

### Story 7.5: Stage-Aware Gather Queries

As a **content editor in a stage**,
I want Gather queries to respect my stage,
So that I see staged content in listings.

**Acceptance Criteria:**

**Given** I am in "Spring Campaign" stage
**When** a Gather query executes
**Then** the query is wrapped with a CTE that:
- Includes items from live and my stage
- Excludes items in stage_deletion for my stage
- Uses stage revision overrides via stage_association
**And** live stage queries have no CTE overhead

---

### Story 7.6: Exposed Filters from URL

As a **site visitor**,
I want to filter listings via URL parameters,
So that I can find specific content.

**Acceptance Criteria:**

**Given** a Gather with exposed filters
**When** I access `/articles?field_category=5&status=1`
**Then** URL parameters are parsed
**And** matching filters are applied to the query
**And** invalid parameters are ignored (security)
**And** only filters marked `exposed: true` accept URL input

---

### Story 7.7: Pager Support

As a **site visitor**,
I want pagination on listings,
So that I can browse large amounts of content.

**Acceptance Criteria:**

**Given** a Gather with 100 results and pager setting of 25
**When** I view the listing
**Then** only 25 items are shown
**And** pager links show: Previous, 1, 2, 3, 4, Next
**And** `?page=2` loads the second page
**And** total count is available for pager rendering

---

### Story 7.8: Execute Gather and Return Results

As a **kernel developer**,
I want to execute a Gather and get results,
So that listings can be rendered.

**Acceptance Criteria:**

**Given** a ViewDefinition and request context
**When** `gather_execute(view, display_id, request_state)` is called
**Then** the query is built and executed
**And** results are returned as a Vec of rows (as serde_json::Value)
**And** errors are handled gracefully (empty results, not 500)

---

## Epic 8: Content Categorization

### Story 8.1: Category Vocabulary Table

As a **site administrator**,
I want category vocabularies stored,
So that I can organize different types of categories.

**Acceptance Criteria:**

**Given** the database is running
**When** migrations run
**Then** `category` table exists with: id (UUIDv7), machine_name (unique), label, description, settings (JSONB)
**And** a "tags" vocabulary is seeded as default

---

### Story 8.2: Category Term Table with Hierarchy

As a **site administrator**,
I want category terms with parent-child relationships,
So that I can create hierarchical categories.

**Acceptance Criteria:**

**Given** the database is running
**When** migrations run
**Then** `category_tag` table exists with: id (UUIDv7), vocabulary_id, label, description, weight, parent_id (nullable, self-reference)
**And** `category_tag_hierarchy` table stores materialized paths for efficient tree queries

---

### Story 8.3: Vocabulary Admin UI

As a **site administrator**,
I want to manage vocabularies through the admin,
So that I can create new category types.

**Acceptance Criteria:**

**Given** I have "administer categories" permission
**When** I access `/admin/categories`
**Then** I see a list of vocabularies
**And** I can create a new vocabulary with machine_name and label
**And** I can edit existing vocabularies
**And** I can delete empty vocabularies

---

### Story 8.4: Term Admin UI

As a **site administrator**,
I want to manage terms within a vocabulary,
So that I can build my category structure.

**Acceptance Criteria:**

**Given** I am viewing a vocabulary
**When** I access `/admin/categories/{vocab}/terms`
**Then** I see a list of terms (hierarchically indented)
**And** I can add a new term with label and optional parent
**And** I can edit existing terms
**And** I can delete terms (with confirmation if they have children)

---

### Story 8.5: Recursive CTE for Hierarchy Queries

As a **kernel developer**,
I want efficient hierarchical term queries,
So that "all children of term X" is fast.

**Acceptance Criteria:**

**Given** a term with nested children
**When** `get_term_descendants(term_id)` is called
**Then** a recursive CTE returns all descendant terms
**And** depth is included for indentation

**When** `get_term_ancestors(term_id)` is called
**Then** the path from root to term is returned
**And** this is used for breadcrumb generation

---

### Story 8.6: Category Field Type

As a **plugin developer**,
I want a RecordReference field type for categories,
So that content can be tagged with terms.

**Acceptance Criteria:**

**Given** a field defined as `FieldType::RecordReference("category_tag")`
**When** I save an item with this field
**Then** the field stores: `[{"target_id": "uuid", "target_type": "category_tag"}]`
**And** multiple terms can be referenced (cardinality: -1)
**And** the admin form shows a term selector

---

### Story 8.7: Breadcrumb Generation

As a **site visitor**,
I want breadcrumbs on category pages,
So that I can navigate the category hierarchy.

**Acceptance Criteria:**

**Given** I am viewing term "Rust" under "Programming" under "Tech"
**When** the breadcrumb is generated
**Then** it shows: Home > Tech > Programming > Rust
**And** each segment links to that term's page

---

### Story 8.8: Category Filter in Gather

As a **plugin developer**,
I want to filter Gather by category,
So that listings can show category-specific content.

**Acceptance Criteria:**

**Given** a Gather filtering by category term
**When** the query is built
**Then** items with that term in their category field are returned
**And** hierarchy is respected: filtering by "Programming" includes items tagged with "Rust" (child term)

---

### Story 8.9: Inter-Plugin Communication

As a **plugin developer**,
I want to call functions in other plugins,
So that I can extend their functionality.

**Acceptance Criteria:**

**Given** plugin A calls `invoke_plugin("blog", "get_featured", "{}")`
**When** plugin "blog" is loaded and exports "get_featured"
**Then** the function is invoked and result returned

**Given** plugin "blog" doesn't exist or doesn't export the function
**When** invoke is called
**Then** an error is returned (not a panic)

**Given** `plugin_exists("blog")` is called
**Then** true/false is returned based on whether the plugin is enabled

---

## Epic 9: Form API

### Story 9.1: Form Definition Types

As a **kernel developer**,
I want Form and FormElement types defined,
So that forms can be built declaratively.

**Acceptance Criteria:**

**Given** the types are defined
**When** I review them
**Then** Form has: form_id, action, method, elements (BTreeMap), token
**And** FormElement has: element_type, title, description, default_value, required, weight, children
**And** ElementType enum includes: Textfield, Textarea, Select, Checkbox, Checkboxes, Radio, Hidden, Password, File, Submit, Fieldset, Markup

---

### Story 9.2: Form Rendering to HTML

As a **site visitor**,
I want forms rendered as HTML,
So that I can interact with the site.

**Acceptance Criteria:**

**Given** a Form definition
**When** it is rendered
**Then** each FormElement becomes appropriate HTML (Textfield → `<input type="text">`, etc.)
**And** elements are ordered by weight
**And** Fieldset creates `<fieldset>` with legend
**And** required fields have `required` attribute
**And** CSRF token is included as hidden field

---

### Story 9.3: Form Validation Pipeline

As a **kernel developer**,
I want server-side form validation,
So that invalid data is rejected.

**Acceptance Criteria:**

**Given** a form is submitted
**When** validation runs
**Then** required fields are checked
**And** `tap_form_validate` is invoked for plugin validation
**And** validation errors are collected (not short-circuited)
**And** if errors exist, form is re-displayed with error messages

---

### Story 9.4: Form Submission Pipeline

As a **kernel developer**,
I want form submissions processed correctly,
So that data is saved after validation.

**Acceptance Criteria:**

**Given** a form passes validation
**When** submission is processed
**Then** `tap_form_submit` is invoked for all implementing plugins
**And** the primary handler processes the data (create item, login user, etc.)
**And** success redirects to appropriate page

---

### Story 9.5: tap_form_alter Support

As a **plugin developer**,
I want to alter forms via tap_form_alter,
So that I can add fields or modify existing forms.

**Acceptance Criteria:**

**Given** a form is being built
**When** `tap_form_alter` is invoked
**Then** plugins receive the form JSON and form_id
**And** plugins can add elements, remove elements, modify properties
**And** the altered form is used for rendering and validation

---

### Story 9.6: CSRF Token Protection

As a **security engineer**,
I want CSRF protection on all forms,
So that cross-site attacks are prevented.

**Acceptance Criteria:**

**Given** a form is rendered
**Then** a unique token is generated and stored in session
**And** the token is included as hidden field `form_token`

**Given** a form is submitted
**When** the token doesn't match session
**Then** 403 Forbidden is returned
**And** the error is logged as potential CSRF attempt

---

### Story 9.7: Form State Cache

As a **kernel developer**,
I want form state cached for multi-step forms,
So that complex workflows are supported.

**Acceptance Criteria:**

**Given** a multi-step form
**When** step 1 is submitted
**Then** form state is saved to `form_state_cache` table with `form_build_id`
**And** step 2 loads state from cache
**And** cache entries expire after 6 hours

---

### Story 9.8: AJAX Form Operations

As a **content editor**,
I want AJAX form updates,
So that I can add fields without page reload.

**Acceptance Criteria:**

**Given** a form with "Add another item" button
**When** I click the button
**Then** JS sends POST to `/system/ajax`
**And** server loads form state, modifies form, re-renders affected section
**And** HTML fragment is returned and inserted into page
**And** form state is updated in cache

---

### Story 9.9: Inline Validation UX

As a **content editor**,
I want clear validation feedback,
So that I can fix errors quickly.

**Acceptance Criteria:**

**Given** I submit a form with errors
**When** the form re-displays
**Then** error messages appear next to the relevant fields
**And** the form scrolls to the first error
**And** successfully filled fields retain their values
**And** error styling is visible (red border, icon)

---

### Story 9.10: Exposed Filter Forms in Gather

As a **site visitor**,
I want filter forms on listings,
So that I can filter content interactively.

**Acceptance Criteria:**

**Given** a Gather with exposed filters
**When** the listing renders
**Then** a filter form is rendered above results
**And** form submits as GET (URL parameters)
**And** current filter values are shown in form
**And** "Reset" button clears all filters

---

## Epic 10: Themed Content Presentation

### Story 10.1: Tera Template Engine Setup

As a **kernel developer**,
I want Tera initialized with template directories,
So that templates can be rendered.

**Acceptance Criteria:**

**Given** the server starts
**When** Tera initializes
**Then** templates are loaded from `templates/` directory
**And** theme overrides are loaded from `templates/themes/{theme}/`
**And** compilation errors are logged with file and line number

---

### Story 10.2: Template Suggestions

As a **theme developer**,
I want template suggestions for specific content,
So that I can customize rendering per content type.

**Acceptance Criteria:**

**Given** an item of type "blog" with id "abc-123"
**When** the template is resolved
**Then** suggestions are checked in order: `item--blog--abc-123.html`, `item--blog.html`, `item.html`
**And** the first existing template is used
**And** suggestion order matches Drupal 6 conventions

---

### Story 10.3: Preprocess Taps

As a **plugin developer**,
I want to add template variables via tap_preprocess_item,
So that I can pass computed data to templates.

**Acceptance Criteria:**

**Given** an item is being rendered
**When** `tap_preprocess_item` is invoked
**Then** plugins receive the current context variables and item handle
**And** plugins can add new variables to the context
**And** variables are available in the template

---

### Story 10.4: RenderElement to HTML Conversion

As a **kernel developer**,
I want RenderElements converted to HTML,
So that plugin output becomes displayable.

**Acceptance Criteria:**

**Given** a RenderElement JSON tree
**When** rendering occurs
**Then** `#type: container` becomes `<div>` with attributes
**And** `#type: markup` becomes `<{tag}>{value}</{tag}>` with format filtering
**And** children are sorted by `#weight` and recursively rendered
**And** `#attributes.class` becomes `class="..."` on the element

---

### Story 10.5: Text Format Rendering

As a **kernel developer**,
I want text formats applied during rendering,
So that user content is safe.

**Acceptance Criteria:**

**Given** a RenderElement with `#format: "filtered_html"`
**When** rendering occurs
**Then** the value is run through the filter pipeline
**And** dangerous tags are stripped
**And** the result is marked as safe HTML

**Given** no `#format` is specified
**When** rendering occurs
**Then** the value is HTML-escaped

---

### Story 10.6: Base Page Template

As a **theme developer**,
I want a base page template,
So that all pages have consistent structure.

**Acceptance Criteria:**

**Given** the base templates
**When** I review them
**Then** `base.html` provides: doctype, html, head, body structure
**And** `page.html` extends base and provides: header, main content, footer regions
**And** content is inserted via `{% block content %}{% endblock %}`

---

## Epic 11: File & Media Management

### Story 11.1: File Managed Table

As a **kernel developer**,
I want file metadata stored in the database,
So that uploaded files are tracked.

**Acceptance Criteria:**

**Given** the database is running
**When** migrations run
**Then** `file_managed` table exists with: id (UUIDv7), owner_id, filename, uri, filemime, filesize, status (0=temp, 1=permanent), created, changed

---

### Story 11.2: File Storage Trait

As a **kernel developer**,
I want a pluggable file storage backend,
So that files can be stored locally or in S3.

**Acceptance Criteria:**

**Given** the FileStorage trait
**When** I review it
**Then** it defines: `write(uri, data)`, `read(uri)`, `delete(uri)`, `exists(uri)`, `public_url(uri)`
**And** it is async and Send+Sync
**And** `LocalFileStorage` and `S3FileStorage` implementations exist

---

### Story 11.3: File Upload Endpoint

As a **content editor**,
I want to upload files,
So that I can attach media to content.

**Acceptance Criteria:**

**Given** I have permission to upload files
**When** I POST to `/file/upload` with multipart form data
**Then** the file is validated (size limits, MIME whitelist)
**And** the file is written to storage with status=0 (temporary)
**And** the file record is created in `file_managed`
**And** the file id and URL are returned

---

### Story 11.4: Inline Drag-and-Drop Upload

As a **content editor**,
I want to drag and drop files into the editor,
So that uploading is seamless.

**Acceptance Criteria:**

**Given** I am editing content
**When** I drag a file onto the editor
**Then** JS handles the drop event
**And** file is uploaded via AJAX to `/file/upload`
**And** on success, the file is inserted at cursor position
**And** upload progress is shown

---

### Story 11.5: File Reference Tracking

As a **kernel developer**,
I want files linked to items tracked,
So that orphan files can be cleaned up.

**Acceptance Criteria:**

**Given** an item has file fields
**When** the item is saved
**Then** referenced files are marked as permanent (status=1)
**And** files no longer referenced are candidates for cleanup

**Given** an item is deleted
**When** cleanup runs
**Then** files only referenced by that item are deleted

---

### Story 11.6: Temporary File Cleanup

As a **site administrator**,
I want temporary files cleaned up automatically,
So that storage isn't wasted.

**Acceptance Criteria:**

**Given** cron runs
**When** temporary file cleanup executes
**Then** files with status=0 older than 6 hours are deleted
**And** both the storage file and database record are removed
**And** cleanup is logged

---

### Story 11.7: S3 Storage Backend

As a **site administrator**,
I want files stored in S3,
So that the system scales without local disk.

**Acceptance Criteria:**

**Given** S3 is configured via environment variables
**When** files are uploaded
**Then** they are stored in the configured S3 bucket
**And** public URLs use CloudFront or S3 direct URLs
**And** private files use signed URLs with expiration

---

## Epic 12: Content Search

### Story 12.1: Search Vector Column

As a **kernel developer**,
I want items to have a search vector,
So that full-text search is possible.

**Acceptance Criteria:**

**Given** the item table
**Then** `search_vector` column exists as tsvector
**And** a GIN index exists on search_vector
**And** the column is populated by a trigger

---

### Story 12.2: Search Field Configuration

As a **site administrator**,
I want to configure which fields are searchable,
So that search is relevant for my content.

**Acceptance Criteria:**

**Given** the configuration table
**When** I review it
**Then** `search_field_config` has: id, bundle, field_name, weight (A/B/C/D)
**And** title is always indexed as weight A
**And** admins can add field configurations per content type

---

### Story 12.3: Search Index Trigger

As a **kernel developer**,
I want search vectors updated automatically,
So that search stays current.

**Acceptance Criteria:**

**Given** an item is inserted or updated
**When** the trigger fires
**Then** the search_vector is rebuilt from configured fields
**And** field weights are applied (A=highest, D=lowest)
**And** the trigger reads configuration from search_field_config

---

### Story 12.4: Search Query Endpoint

As a **site visitor**,
I want to search for content,
So that I can find what I'm looking for.

**Acceptance Criteria:**

**Given** I access `/search?q=rust+programming`
**When** the search executes
**Then** `plainto_tsquery` is used for the query
**And** results are ordered by `ts_rank`
**And** only published items are returned (status=1)
**And** pagination is supported

---

### Story 12.5: Search Own Drafts

As a **content editor**,
I want to search my own unpublished content,
So that I can find my work in progress.

**Acceptance Criteria:**

**Given** I am logged in
**When** I search
**Then** results include my unpublished items (author_id = current user)
**And** other users' unpublished items are excluded
**And** draft results are visually distinguished

---

### Story 12.6: Search Results Display

As a **site visitor**,
I want search results displayed nicely,
So that I can evaluate relevance.

**Acceptance Criteria:**

**Given** search results
**When** they are rendered
**Then** each result shows: title (linked), snippet with highlighted matches, content type
**And** "No results found" is shown for empty results
**And** search query is preserved in the search box

---

## Epic 13: Scheduled Operations & Background Tasks

### Story 13.1: Cron Endpoint

As a **operations engineer**,
I want a cron trigger endpoint,
So that scheduled tasks can be run.

**Acceptance Criteria:**

**Given** cron is configured
**When** an external scheduler calls `/cron/{key}`
**Then** cron tasks are executed if the key is valid
**And** invalid keys return 403
**And** cron can also run via CLI: `cargo run -- cron`

---

### Story 13.2: Distributed Lock for Cron

As a **kernel developer**,
I want cron to run on exactly one server,
So that tasks don't run multiple times.

**Acceptance Criteria:**

**Given** multiple servers running
**When** cron triggers simultaneously
**Then** only one server acquires the Redis lock
**And** other servers skip cron execution
**And** lock uses `SET NX EX` pattern

---

### Story 13.3: Lock Heartbeat

As a **kernel developer**,
I want the cron lock extended during execution,
So that long-running tasks don't lose the lock.

**Acceptance Criteria:**

**Given** cron is running a slow task
**When** 60 seconds pass
**Then** a background task extends the lock TTL
**And** the lock is released when cron completes
**And** if the server crashes, lock expires after 5 minutes

---

### Story 13.4: Queue Implementation

As a **kernel developer**,
I want a Redis-based queue implementation,
So that background tasks can be processed.

**Acceptance Criteria:**

**Given** the Queue trait
**When** RedisQueue is used
**Then** `push` adds items to a Redis list
**And** `pop` atomically removes and returns items
**And** queue operations are async

---

### Story 13.5: Queue Worker

As a **kernel developer**,
I want queue items processed by plugins,
So that background work can be done.

**Acceptance Criteria:**

**Given** plugins implement `tap_queue_info` and `tap_queue_worker`
**When** the queue worker runs
**Then** it pops items from each declared queue
**And** invokes the appropriate plugin's worker
**And** failures are logged and items can be retried

---

### Story 13.6: Plugin Cron Tap

As a **plugin developer**,
I want my plugin's cron tasks executed,
So that I can do periodic maintenance.

**Acceptance Criteria:**

**Given** my plugin implements `tap_cron`
**When** cron runs
**Then** my tap is invoked
**And** errors are caught and logged (don't break other plugins)
**And** I can check elapsed time and skip if cron runs too frequently

---

### Story 13.7: Batch API Design

As a **kernel developer**,
I want a batch API for long operations,
So that admins can run large tasks without timeouts.

**Acceptance Criteria:**

**Given** a large operation (e.g., reindex all content)
**When** batch is started
**Then** progress is stored in Redis with batch_id
**And** `/batch/{id}/status` returns progress percentage
**And** operations run in chunks across multiple cron runs
**And** completion triggers a callback

---

## Epic 14: Production Readiness

### Story 14.1: Rate Limiting Middleware

As a **operations engineer**,
I want rate limiting on endpoints,
So that abuse is prevented.

**Acceptance Criteria:**

**Given** rate limiting is configured
**When** requests exceed limits
**Then** 429 Too Many Requests is returned
**And** limits are: login (5/min/IP), forms (30/min/session), API (configurable)
**And** rate counters use Redis for distributed counting

---

### Story 14.2: Rate Limit Configuration

As a **site administrator**,
I want to configure rate limits,
So that I can tune for my traffic patterns.

**Acceptance Criteria:**

**Given** rate limit configuration
**When** I set custom limits
**Then** limits are stored in config (TOML or database)
**And** per-route limits can be specified
**And** certain paths can be excluded (e.g., static assets)

---

### Story 14.3: Prometheus Metrics Endpoint

As a **operations engineer**,
I want a metrics endpoint,
So that I can monitor the system.

**Acceptance Criteria:**

**Given** the server is running
**When** I GET `/metrics`
**Then** Prometheus-format metrics are returned including:
- HTTP request duration histogram
- HTTP request count by status code
- WASM tap invocation duration histogram
- Database query duration histogram
- Cache hit/miss counters
- Active WASM instance count
- Redis connection pool stats

---

### Story 14.4: Metrics Access Control

As a **security engineer**,
I want metrics restricted to internal access,
So that system details aren't exposed publicly.

**Acceptance Criteria:**

**Given** metrics endpoint
**When** accessed from external IP
**Then** 403 is returned (or NGINX blocks before reaching app)

**When** accessed from internal IP or with auth token
**Then** metrics are returned

---

### Story 14.5: Load Testing Setup

As a **kernel developer**,
I want load testing with goose,
So that we can verify performance.

**Acceptance Criteria:**

**Given** a goose test suite
**When** I run load tests
**Then** tests simulate: anonymous page views, logged-in editing, search queries, Gather listings
**And** results show: requests/second, latency percentiles, error rates
**And** baseline metrics are documented

---

### Story 14.6: Performance Baseline Documentation

As a **kernel developer**,
I want performance baselines documented,
So that regressions can be detected.

**Acceptance Criteria:**

**Given** load tests have run
**When** I review documentation
**Then** `docs/performance-baseline.md` includes:
- Test environment specs
- Target metrics (requests/sec, p95 latency)
- Current measured metrics
- Known bottlenecks and future optimization opportunities

---

# Phase 7-9: Post-MVP Epics

## Epic List (Phase 7-9)

| Epic | Title | Phase | Gate Criteria |
|------|-------|-------|---------------|
| 15 | Drupal 6 Alignment Audit | 7 (parallel) | Documented alignment report with intentional vs accidental divergences |
| 16 | Admin Interface Completion | 7 | All admin CRUD operations functional via UI |
| 17 | Installer & Setup Experience | 8 | Fresh install completes via wizard, creates working site |
| 18 | Display & Theming Layer | 7 | Content renders via API and themed HTML with comments |
| 19 | CI & Test Infrastructure | 7 | PR pipeline enforces fmt, clippy, tests; coverage tracked |
| 20 | Use Case Exploration | 8 | Three use cases validated with working examples |
| 21 | Complete Stage Workflow | 9 | All config entities stage-aware with atomic publish |
| 22 | Modern CMS Features | 9 | Selected D7+ features implemented and documented |
| 23 | Gather UI & Query Consolidation | 9 | Admin UI for Gather definitions; hardcoded listings converted to Gather queries |
| 24 | Block Editor (Editor.js) | 9 | Block-based content editing via Editor.js, structured JSON storage, server-side rendering |
| 25 | Coding Standards & Enforcement | 10 | `.rustfmt.toml` + `clippy.toml` configured; zero violations in CI; `docs/coding-standards.md` published; all existing code compliant |

---

## Epic 15: Drupal 6 Alignment Audit

**Goal:** Systematically document where Trovato aligns with and diverges from Drupal 6 + CCK + Views patterns, plus the two modern additions (Workspaces/Stages, Gander/Profiling). Identify and address critical gaps.

**Scope:**
- Review all Drupal 6 core concepts against Trovato implementation
- Include CCK (field system) and Views (Gather) alignment
- Include modern additions: Workspaces→Stages, Gander→Profiling
- Document intentional divergences with rationale
- Identify accidental gaps that should be addressed
- **Implement Path Aliases** (critical gap identified)
- Create alignment roadmap for future work

**Gate:** Comprehensive alignment report completed with prioritized gap list; path aliases functional.

**Note:** This epic runs in parallel with other Phase 7 work.

---

### Story 15.1: Core Concept Mapping

As a **project lead**,
I want a systematic mapping of Drupal 6 core concepts to Trovato equivalents,
So that we understand our alignment baseline.

**Acceptance Criteria:**

1. **Given** the Drupal 6 + CCK + Views architecture
   **When** I review the mapping document
   **Then** it covers all major subsystems with alignment status

2. Document must include mapping tables for:
   - Content System: nodes→items, node types→item types, revisions
   - CCK: field definitions, field instances, field types, JSONB storage
   - Views→Gather: view definitions, filters, sorts, relationships, arguments, pagers, exposed filters
   - Taxonomy→Categories: vocabularies, terms, hierarchy, term references
   - Users: authentication, roles, permissions, access control
   - Hooks→Taps: lifecycle hooks, form hooks, render hooks
   - Modules→Plugins: WASM loading, dependency resolution
   - Form API: elements, validation, submission, AJAX
   - Theme System: templates, suggestions, preprocess, regions
   - Filters: text formats, filter pipeline
   - Menu System: routing, menu trees, breadcrumbs
   - Files: upload, storage, cleanup
   - Search: indexing, querying
   - Cron/Queue: scheduled tasks, background processing
   - Modern Additions: Workspaces→Stages, Gander→Profiling

3. Each concept has: D6 implementation, Trovato implementation, alignment status (✅ aligned / ⚠️ partial / ❌ missing)

**Tasks:**
- [ ] Create docs/alignment/drupal6-mapping.md
- [ ] Document Content System alignment
- [ ] Document CCK→JSONB Fields alignment
- [ ] Document Views→Gather alignment
- [ ] Document Taxonomy→Categories alignment
- [ ] Document all remaining subsystems
- [ ] Add alignment status badges to each section

**Dev Notes:**
- Reference Design-Content-Model.md, Design-Query-Engine.md, Terminology.md
- Use sprint-status.yaml to verify completed stories per subsystem
- Include code references (file:line) for key implementations

---

### Story 15.2: Intentional Divergence Documentation

As an **architect**,
I want intentional divergences documented with rationale,
So that future developers understand why we differ from D6.

**Acceptance Criteria:**

1. **Given** the concept mapping from 15.1
   **When** I review intentional divergences
   **Then** each divergence explains: what D6 did, what Trovato does differently, why the change was made, benefits gained

2. Must document these key divergences:
   - WASM sandboxed plugins vs PHP modules (security, isolation)
   - JSONB field storage vs EAV tables (performance, no N+1 JOINs)
   - RenderElement JSON vs raw HTML (XSS prevention, alterability)
   - UUIDv7 vs auto-increment IDs (no enumeration, time-sortable, merge-safe)
   - Stages vs simple published flag (content staging from day one)
   - is_admin boolean vs User 1 magic (explicit, self-documenting)
   - SeaQuery AST vs string SQL (injection-safe)
   - Argon2id vs MD5/SHA (modern password hashing)
   - Redis sessions vs database sessions (TTL native, multi-server)
   - Handle-based WASM data access vs full serialization (performance)

3. Each divergence references Decision Log in Design-Project-Meta.md

**Tasks:**
- [ ] Create docs/alignment/intentional-divergences.md
- [ ] Document each divergence with D6 behavior and Trovato approach
- [ ] Include performance/security rationale
- [ ] Cross-reference Design-Project-Meta.md §21 Decision Log

---

### Story 15.3: Gap Analysis and Prioritization

As a **product owner**,
I want gaps prioritized by impact,
So that we can plan remediation work.

**Acceptance Criteria:**

1. **Given** the concept mapping shows missing features
   **When** I review the gap analysis
   **Then** each gap has: description, impact assessment, effort estimate, recommended resolution

2. Must analyze these identified gaps:
   - **Path Aliases** (HIGH priority - addressed in 15.5)
   - Localization/i18n (LOW - explicitly deferred)
   - Text format per-role permissions (MEDIUM - verify implementation)
   - hook_user variants / tap_user_* coverage (LOW - verify)
   - Actions/Trigger system (LOW - not common D6 usage)
   - Update status checking (LOW - different deployment model)

3. Gaps sorted by priority with clear recommendations

**Tasks:**
- [ ] Create docs/alignment/gap-analysis.md
- [ ] Audit each potential gap against codebase
- [ ] Assign priority (High/Medium/Low) based on user impact
- [ ] Estimate effort (S/M/L) for remediation
- [ ] Recommend: implement now, defer, or intentionally skip

---

### Story 15.4: Alignment Roadmap

As a **project lead**,
I want an alignment roadmap,
So that we have a plan to address critical gaps.

**Acceptance Criteria:**

1. **Given** the gap analysis
   **When** I review the roadmap
   **Then** it shows which gaps to address and when

2. Roadmap must specify:
   - Gaps addressed in Epic 15 (Path Aliases)
   - Gaps addressed in Epic 21 (Stage completeness)
   - Gaps addressed in Epic 22 (Modern CMS features)
   - Gaps intentionally deferred (with rationale)
   - Success criteria for "Drupal 6 + CCK + Views parity"

3. Maintained as living document in docs/alignment/roadmap.md

**Tasks:**
- [ ] Create docs/alignment/roadmap.md
- [ ] Map gaps to future epics
- [ ] Define success criteria for D6 parity
- [ ] Document deferred items with rationale

---

### Story 15.5: Path Alias System

As a **content editor**,
I want URL aliases for content,
So that pages have human-readable URLs instead of /item/{uuid}.

**Acceptance Criteria:**

1. **Given** an item exists
   **When** I create a path alias
   **Then** the item is accessible via both /item/{id} and the alias path

2. **Given** a path alias exists
   **When** a request comes for that path
   **Then** the system routes to the correct item without redirect (internal rewrite)

3. **Given** an alias path is requested
   **When** the item is loaded
   **Then** the canonical URL uses the alias in templates/links

4. **Given** multiple aliases exist for one item
   **When** I view the item
   **Then** the most recent alias is canonical

5. **Given** I'm an admin
   **When** I edit an item
   **Then** I can set/change the URL alias

6. Path aliases are stage-aware (alias can differ per stage)

**Tasks:**
- [ ] Create `url_alias` table schema (AC: 1, 2)
- [ ] Create `UrlAlias` model with CRUD operations (AC: 1)
- [ ] Add path alias middleware to resolve aliases before routing (AC: 2)
- [ ] Add `get_canonical_url()` helper for templates (AC: 3)
- [ ] Handle multiple aliases - most recent wins (AC: 4)
- [ ] Add alias field to item edit form (AC: 5)
- [ ] Make aliases stage-aware (AC: 6)
- [ ] Add integration tests for alias resolution
- [ ] Add admin UI for managing aliases (/admin/config/aliases)

**Dev Notes:**
- Middleware runs before router, rewrites request path
- Store original path in request extension for canonical URL
- Pattern: `/blog/my-post` → internal `/item/01936e3b-4f5a-7000-8000-000000000001`
- Consider auto-generation from title (pathauto pattern) as future enhancement

**Schema:**
```sql
CREATE TABLE url_alias (
    id UUID PRIMARY KEY,
    source VARCHAR(255) NOT NULL,      -- /item/{uuid}
    alias VARCHAR(255) NOT NULL,
    language VARCHAR(12) DEFAULT 'en',
    stage_id VARCHAR(64) DEFAULT 'live' REFERENCES stage(id),
    created BIGINT NOT NULL,
    UNIQUE (alias, stage_id)
);
CREATE INDEX idx_alias_source ON url_alias(source);
CREATE INDEX idx_alias_alias ON url_alias(alias);
```

**References:**
- Drupal 6 path module for behavior reference
- Design-Web-Layer.md for middleware patterns

---

## Epic 16: Admin Interface Completion

**Goal:** Complete the admin UI to enable full system management through the browser, supporting both manual testing and day-to-day administration.

**FRs covered:** Admin CRUD for users, roles, permissions, content, categories, files

**Scope:**
- User management (list, create, edit, delete)
- Role management with permission matrix
- Content listing with filters and bulk operations
- Category and tag management
- File browser and management
- All operations testable via integration tests

**Gate:** Administrator can manage all system entities through the UI.

---

### Story 16.1: User List and Search

As a **site administrator**,
I want to view and search all users,
So that I can manage user accounts.

**Acceptance Criteria:**

**Given** I have "administer users" permission
**When** I access `/admin/people`
**Then** I see a paginated list of users with: username, email, status, roles, last access
**And** I can search by username or email
**And** I can filter by status (active/blocked) and role

---

### Story 16.2: User Create and Edit

As a **site administrator**,
I want to create and edit user accounts,
So that I can onboard new users and update existing ones.

**Acceptance Criteria:**

**Given** I have "administer users" permission
**When** I access `/admin/people/add`
**Then** I can create a user with: username, email, password, status, roles, is_admin flag
**And** validation ensures unique username and email
**And** password is hashed with Argon2id

**When** I access `/admin/people/{id}/edit`
**Then** I can edit all user fields except username
**And** password field is optional (only updates if provided)
**And** I cannot remove my own admin access

---

### Story 16.3: User Delete with Safety

As a **site administrator**,
I want to delete user accounts safely,
So that I can remove users without breaking the system.

**Acceptance Criteria:**

**Given** I have "administer users" permission
**When** I delete a user at `/admin/people/{id}/delete`
**Then** confirmation is required
**And** I cannot delete my own account
**And** I cannot delete the anonymous user (Uuid::nil)
**And** content authored by deleted user is reassigned or preserved

---

### Story 16.4: Role Management

As a **site administrator**,
I want to manage roles,
So that I can organize permissions into logical groups.

**Acceptance Criteria:**

**Given** I have "administer permissions" permission
**When** I access `/admin/people/roles`
**Then** I see all roles with user counts
**And** I can create new roles with unique machine names
**And** I can edit role names
**And** I cannot delete "anonymous" or "authenticated" built-in roles
**And** deleting a role removes it from all users

---

### Story 16.5: Permission Matrix

As a **site administrator**,
I want a permission matrix UI,
So that I can assign permissions to roles visually.

**Acceptance Criteria:**

**Given** I have "administer permissions" permission
**When** I access `/admin/people/permissions`
**Then** I see a grid: rows are permissions, columns are roles
**And** checkboxes indicate which roles have which permissions
**And** I can toggle permissions and save changes
**And** permissions are grouped by module/category
**And** admin role permissions are shown but not editable (always has all)

---

### Story 16.6: Content List with Filters

As a **site administrator**,
I want to view all content with filters,
So that I can manage content across the site.

**Acceptance Criteria:**

**Given** I have "administer content" permission
**When** I access `/admin/content`
**Then** I see a paginated list with: title, type, author, status, created, changed
**And** I can filter by: content type, status (published/unpublished), author
**And** I can sort by any column
**And** each row links to edit and delete actions

---

### Story 16.7: Content Quick Edit Actions

As a **content editor**,
I want quick actions on the content list,
So that I can efficiently manage content.

**Acceptance Criteria:**

**Given** I am viewing the content list
**When** I select items via checkboxes
**Then** I can apply bulk actions: publish, unpublish, delete
**And** confirmation is required for destructive actions
**And** success/failure feedback is shown

---

### Story 16.8: Category Management UI

As a **site administrator**,
I want to manage categories and tags through the UI,
So that I can organize content taxonomy.

**Acceptance Criteria:**

**Given** I have "administer categories" permission
**When** I access `/admin/structure/categories`
**Then** I see all category vocabularies
**And** I can create, edit, and delete vocabularies
**When** I access `/admin/structure/categories/{vocab}/tags`
**Then** I see tags in hierarchical tree view
**And** I can create, edit, delete, and reorder tags
**And** I can set parent relationships for hierarchy

---

### Story 16.9: File Management UI

As a **site administrator**,
I want to view and manage uploaded files,
So that I can track and clean up media.

**Acceptance Criteria:**

**Given** I have "administer files" permission
**When** I access `/admin/content/files`
**Then** I see files with: filename, type, size, status (temp/permanent), owner, upload date
**And** I can filter by status and MIME type
**And** I can delete orphaned/temporary files
**And** I can view file details and usage (which items reference it)

---

### Story 16.10: Admin Dashboard Enhancement

As a **site administrator**,
I want a useful admin dashboard,
So that I have quick access to common tasks.

**Acceptance Criteria:**

**Given** I access `/admin`
**When** the dashboard loads
**Then** I see: recent content, system status, quick links to common admin tasks
**And** navigation sidebar links to all admin sections
**And** user count, content count, and other key metrics are displayed

---

## Epic 17: Installer & Setup Experience

**Goal:** Create a guided installation experience that sets up Trovato for first-time use, making onboarding seamless for developers and site administrators.

**Scope:**
- Database connection setup
- Initial admin user creation
- Default content type creation
- Site configuration (name, email, timezone)
- Environment validation

**Gate:** Fresh install completes via wizard and produces working site.

---

### Story 17.1: Installation Detection

As a **new user**,
I want the system to detect if installation is needed,
So that I'm guided to setup on first access.

**Acceptance Criteria:**

**Given** the database has no users table or it's empty
**When** I access any page
**Then** I am redirected to `/install`
**And** installation cannot be bypassed until complete
**And** if already installed, `/install` returns 404

---

### Story 17.2: Database Configuration Step

As a **new user**,
I want to configure database connection,
So that Trovato can store data.

**Acceptance Criteria:**

**Given** I am on the install wizard
**When** I reach the database step
**Then** I can enter: host, port, database name, username, password
**And** connection is tested before proceeding
**And** migrations are run automatically on success
**And** clear error messages explain connection failures

---

### Story 17.3: Admin Account Creation

As a **new user**,
I want to create the initial admin account,
So that I can manage the site.

**Acceptance Criteria:**

**Given** database is configured
**When** I reach the admin account step
**Then** I enter: username, email, password (with confirmation)
**And** password strength is validated
**And** account is created with is_admin=true
**And** I am logged in automatically after creation

---

### Story 17.4: Site Configuration

As a **new user**,
I want to configure basic site settings,
So that the site has proper identity.

**Acceptance Criteria:**

**Given** admin account is created
**When** I reach the site configuration step
**Then** I can set: site name, site email, default timezone
**And** settings are saved to variables table
**And** reasonable defaults are pre-filled

---

### Story 17.5: Installation Complete

As a **new user**,
I want confirmation that installation succeeded,
So that I can start using the site.

**Acceptance Criteria:**

**Given** all installation steps complete
**When** I reach the final step
**Then** I see success message with next steps
**And** links to: admin dashboard, create content, view site
**And** installation is marked complete (prevents re-running)

---

## Epic 18: Display & Theming Layer

**Goal:** Enable content to be displayed both as JSON API responses and as themed HTML pages, with full support for comments and interactive elements.

**Scope:**
- JSON API endpoints for headless usage
- Tera template rendering for server-side HTML
- Threaded comments system
- Pagination and breadcrumbs
- Template override system

**Gate:** Content renders via API and themed HTML with working comments.

---

### Story 18.1: JSON API Content Endpoints

As a **frontend developer**,
I want JSON API endpoints for content,
So that I can build decoupled frontends.

**Acceptance Criteria:**

**Given** content exists in the system
**When** I GET `/api/item/{id}`
**Then** I receive JSON with: id, type, title, fields, author, created, changed, status
**And** field values are properly typed (not all strings)
**And** referenced entities can be included via `?include=author,category`

**When** I GET `/api/items?type=article&status=1`
**Then** I receive paginated list of items matching filters
**And** pagination metadata includes: total, page, per_page, links

---

### Story 18.2: JSON API for Categories

As a **frontend developer**,
I want JSON API for categories,
So that I can build navigation and filters.

**Acceptance Criteria:**

**Given** categories exist
**When** I GET `/api/categories`
**Then** I receive list of vocabularies
**When** I GET `/api/category/{vocab}/tags`
**Then** I receive tags with hierarchy information
**And** parent/children relationships are included

---

### Story 18.3: Theme Template System

As a **theme developer**,
I want a template override system,
So that I can customize content display.

**Acceptance Criteria:**

**Given** default templates exist in `templates/`
**When** a theme provides `templates/themes/{theme}/item--article.html`
**Then** the theme template is used instead of default
**And** template suggestions follow order: item--{type}--{id}.html, item--{type}.html, item.html
**And** templates have access to: item, user, site variables

---

### Story 18.4: Page Layout Templates

As a **theme developer**,
I want page layout templates,
So that I can control overall page structure.

**Acceptance Criteria:**

**Given** page templates exist
**When** a page renders
**Then** layout includes: header, navigation, main content, sidebar, footer regions
**And** regions can contain blocks
**And** page--{path}.html suggestions work for specific paths

---

### Story 18.5: Comment Entity and Storage

As a **kernel developer**,
I want comments stored as entities,
So that users can discuss content.

**Acceptance Criteria:**

**Given** the comment system is enabled
**Then** `comment` table exists with: id, item_id, parent_id (for threading), author_id, body, status, created, changed
**And** comments support threading (parent_id references another comment)
**And** comment count is tracked on parent item

---

### Story 18.6: Comment Display

As a **site visitor**,
I want to view comments on content,
So that I can read discussions.

**Acceptance Criteria:**

**Given** an item has comments
**When** I view the item
**Then** comments are displayed below content
**And** threaded comments are visually indented
**And** each comment shows: author, date, body
**And** pagination handles large comment threads

---

### Story 18.7: Comment Submission

As a **authenticated user**,
I want to post comments,
So that I can participate in discussions.

**Acceptance Criteria:**

**Given** I am logged in and comments are enabled for this content type
**When** I submit a comment
**Then** the comment is saved with my user as author
**And** I can reply to existing comments (threading)
**And** CSRF protection is enforced
**And** the page refreshes to show my comment

---

### Story 18.8: Comment Moderation

As a **site administrator**,
I want to moderate comments,
So that I can remove inappropriate content.

**Acceptance Criteria:**

**Given** I have "administer comments" permission
**When** I access `/admin/content/comments`
**Then** I see all comments with: content title, author, excerpt, status, date
**And** I can approve, unpublish, or delete comments
**And** bulk operations are supported

---

## Epic 19: CI & Test Infrastructure

**Goal:** Establish comprehensive CI/CD pipeline that enforces code quality and ensures all changes are properly tested.

**Scope:**
- GitHub Actions CI pipeline
- cargo fmt enforcement
- cargo clippy checks
- Test coverage tracking
- Pre-commit hooks

**Gate:** PR pipeline enforces fmt, clippy, and tests; coverage is tracked.

---

### Story 19.1: GitHub Actions Workflow

As a **developer**,
I want CI to run on every PR,
So that code quality is automatically verified.

**Acceptance Criteria:**

**Given** a PR is opened
**When** CI runs
**Then** the workflow: checks out code, sets up Rust toolchain, runs tests
**And** workflow completes in reasonable time (<10 min)
**And** status is reported on the PR

---

### Story 19.2: Cargo Fmt Enforcement

As a **developer**,
I want code formatting enforced,
So that the codebase stays consistent.

**Acceptance Criteria:**

**Given** CI runs on a PR
**When** code doesn't match `cargo fmt`
**Then** CI fails with clear message about formatting issues
**And** `cargo fmt --check` is used (no auto-fix in CI)
**And** instructions explain how to fix locally

---

### Story 19.3: Cargo Clippy Checks

As a **developer**,
I want linting enforced,
So that common mistakes are caught.

**Acceptance Criteria:**

**Given** CI runs on a PR
**When** clippy finds warnings
**Then** CI fails (warnings are errors in CI)
**And** clippy output clearly identifies issues
**And** reasonable allow list exists for intentional exceptions

---

### Story 19.4: Integration Test Suite

As a **developer**,
I want all integration tests to run in CI,
So that regressions are caught.

**Acceptance Criteria:**

**Given** CI runs on a PR
**When** tests execute
**Then** all tests in `crates/kernel/tests/` run
**And** database and Redis are available (via services)
**And** test failures clearly identify which test failed and why
**And** tests run in parallel where possible

---

### Story 19.5: Test Coverage Tracking

As a **developer**,
I want test coverage tracked,
So that we can identify undertested code.

**Acceptance Criteria:**

**Given** CI runs
**When** tests complete
**Then** coverage report is generated (via cargo-tarpaulin or similar)
**And** coverage percentage is reported on PR
**And** coverage trends are visible over time
**And** critical paths have minimum coverage requirements

---

### Story 19.6: Pre-commit Hooks

As a **developer**,
I want pre-commit hooks available,
So that I catch issues before pushing.

**Acceptance Criteria:**

**Given** I have the repo cloned
**When** I run the setup script
**Then** pre-commit hooks are installed
**And** hooks run: cargo fmt --check, cargo clippy
**And** hooks can be bypassed with --no-verify for WIP commits
**And** setup instructions are in CONTRIBUTING.md

---

### Story 19.7: WASM Plugin Test Infrastructure

As a **plugin developer**,
I want plugin tests to run in CI,
So that SDK changes don't break plugins.

**Acceptance Criteria:**

**Given** CI runs
**When** plugin tests execute
**Then** wasm32-wasip1 target is available
**And** reference plugins compile successfully
**And** plugin integration tests pass
**And** SDK compatibility is verified

---

## Epic 20: Use Case Exploration

**Goal:** Validate Trovato against real-world use cases to identify gaps and prove the platform's versatility. The three plugin projects (Argus, Netgrasp, Goose) serve as primary validation that Trovato's Plugin system, Record/Gather model, and auth/roles architecture work under diverse, genuine workloads.

**Scope:**
- Traditional website (pages, blog, navigation)
- Backend API (headless CMS)
- Argus: AI news intelligence (composite Gather responses, vector search)
- Netgrasp: Network monitor (lean runtime, event-driven state machine)
- Goose: Load testing UI (high-throughput writes, real-time queries)

**Gate:** Five use cases validated; Argus/Netgrasp/Goose can use Trovato without bypassing Record/Gather with custom endpoints.

**Cross-references:** [[Projects/Argus]], [[Projects/Netgrasp]], [[Projects/Goose]]

---

### Story 20.1: Traditional Website Use Case

As a **site builder**,
I want to build a traditional website with Trovato,
So that the CMS serves its primary purpose.

**Acceptance Criteria:**

1. **Given** a fresh Trovato installation
   **When** I follow the website tutorial
   **Then** I can create: home page, about page, blog with posts, navigation menu

2. Pages render with theming (base template, page template, content templates)

3. Blog has list view with pagination via Gather

4. Navigation menu renders from MenuRegistry

5. The result feels like a real website, not a demo

**Tasks:**
- [ ] Create "Building Your First Site" tutorial document
- [ ] Define Page and Blog Post content types with fields
- [ ] Create example Gather for blog listing
- [ ] Add sample theme with homepage, blog, about page templates
- [ ] Verify menu system renders navigation

---

### Story 20.2: Headless CMS Use Case

As a **frontend developer**,
I want to use Trovato as a headless CMS,
So that I can build custom frontends.

**Acceptance Criteria:**

1. **Given** content exists in Trovato
   **When** I query the JSON API
   **Then** I can: list items, fetch single items, filter by type/field, paginate

2. Authentication works via session cookies or API tokens

3. API documentation (OpenAPI spec or equivalent) is complete

4. Example frontend (vanilla JS or simple framework) demonstrates:
   - Fetching and displaying content list
   - Single item view
   - Filtering and search
   - Authenticated actions (if applicable)

**Tasks:**
- [ ] Verify /api/items and /api/item/{id} endpoints cover common use cases
- [ ] Add API authentication documentation
- [ ] Create simple example frontend (static HTML + fetch)
- [ ] Document all JSON API endpoints

---

### Story 20.3: Argus Integration - Composite Gather Responses

As an **Argus developer**,
I want Trovato to produce composite JSON responses,
So that the iOS app can fetch a Story with embedded articles, entities, and user state in one request.

**Acceptance Criteria:**

1. **Given** Argus content types exist (Article, Story, Topic, Feed, Entity)
   **When** I define a Gather for "story with details"
   **Then** the response includes the story plus nested arrays of related articles, entities, and user reactions

2. Gather supports "includes" or relationship loading:
   ```json
   {
     "id": "...",
     "title": "Story Title",
     "summary": "...",
     "articles": [
       { "id": "...", "title": "...", "summary": "..." }
     ],
     "entities": [
       { "id": "...", "name": "...", "type": "person" }
     ],
     "user_reaction": { "bookmarked": true, "read": true }
   }
   ```

3. No custom hand-rolled endpoints required for composite responses

4. Auth roles work: "admin" (full access), "reader" (stories, articles, feedback only)

5. Document any Trovato enhancements needed (e.g., Gather includes, nested loading)

**Content Types for Argus:**
- Article: URL, title, content, relevance_score, summary, critical_analysis, vector_embedding, feed_id, topic_id, story_id
- Story: title, summary, source_attribution (JSON), topic_id, article_count, relevance_score, active flag
- Topic: name, relevance_prompt, threshold
- Feed: URL, name, fetch_interval, health_status
- Entity: canonical_name, aliases, type, description
- Reaction: user_id, item_id, type (upvote/downvote/bookmark/flag)
- Discussion: story_id, parent_id, user_id, content, created

**Key Validation:**
- If Gather can produce the composite Story response natively, the abstraction is validated
- If custom endpoints are needed, document what Gather lacks

**Tasks:**
- [ ] Define Argus content types as Trovato item_types
- [ ] Design Gather "includes" feature if not present
- [ ] Create "story_with_details" Gather definition
- [ ] Test composite response structure
- [ ] Document gaps or required Trovato enhancements
- [ ] Verify auth roles (admin vs reader)

**Dev Notes:**
- pgvector integration may be needed for vector_embedding field (separate story or defer)
- This is the most demanding test of Gather's expressiveness

---

### Story 20.4: Netgrasp Integration - Lean Runtime Validation

As a **Netgrasp developer**,
I want Trovato to run efficiently on a Raspberry Pi,
So that the network monitor doesn't overwhelm limited hardware.

**Acceptance Criteria:**

1. **Given** Netgrasp content types exist (Device, Person, Event, PresenceSession)
   **When** Trovato runs on Raspberry Pi 4 (4GB RAM)
   **Then** idle memory usage is under 100MB and CPU is negligible

2. **Given** 500 devices and 10,000 events in the database
   **When** I query the device list Gather
   **Then** response time is under 100ms

3. Event-driven state machine pattern works:
   - Device state changes (online → idle → offline) create Event records
   - Person presence derived from device ownership
   - Gather can filter devices by state, type, owner

4. Auth roles work: "network_admin" (view + edit), "viewer" (read-only dashboard)

5. Web dashboard renders via Trovato pages + templates (no heavy JS framework)

**Content Types for Netgrasp:**
- Device: mac, display_name, hostname, vendor, device_type, os_family, state (online/idle/offline), last_ip, current_ap, owner_id, hidden, notify, baseline
- Person: name, notes, notification_prefs
- Event: device_id, event_type, timestamp, details (JSON)
- PresenceSession: device_id, start_time, end_time
- IPHistory: device_id, ip_address, first_seen, last_seen
- LocationHistory: device_id, location, start_time, end_time

**Key Validation:**
- Binary size and runtime memory are acceptable for embedded deployment
- Event ingestion (10s polling, ~100 events/minute peaks) doesn't cause performance issues

**Tasks:**
- [ ] Define Netgrasp content types as Trovato item_types
- [ ] Benchmark Trovato on Raspberry Pi 4 (memory, CPU, query latency)
- [ ] Create device list Gather with state/type/owner filters
- [ ] Create event log Gather with time range filter
- [ ] Test event write throughput (simulate device state changes)
- [ ] Verify auth roles (network_admin vs viewer)
- [ ] Document performance baseline and any optimizations needed

**Dev Notes:**
- SQLite mentioned in original doc, but PostgreSQL is Trovato's database
- For Pi deployment, may need lightweight PostgreSQL or evaluate SQLite support as future work
- This is the "lean runtime" stress test

---

### Story 20.5: Goose Integration - High-Throughput Metrics Dashboard

As a **Goose developer**,
I want Trovato to handle high-throughput metric writes and real-time queries,
So that the load testing UI can show live results during active tests.

**Acceptance Criteria:**

1. **Given** a load test is running at 1000 requests/second
   **When** Goose writes endpoint metrics to Trovato
   **Then** writes complete without blocking test execution (async/buffered)

2. **Given** metrics are being written in real-time
   **When** the dashboard queries recent metrics via Gather
   **Then** results reflect data written within the last 5 seconds

3. Historical queries work efficiently:
   - "All runs for site X" with pagination
   - "Endpoints sorted by p99 latency" within a run
   - "Compare run A vs run B" showing regressions

4. Auth roles work: "operator" (run tests, view results), "viewer" (read-only)

5. Dashboard can render live charts (requests/sec, response time, errors)

**Content Types for Goose:**
- TestRun: target_site_id, start_time, end_time, config (JSON), status, aggregate_metrics (JSON)
- Scenario: name, description, target_site_id, user_count, hatch_rate, duration, task_config
- EndpointResult: test_run_id, url_pattern, method, request_count, error_count, avg_ms, p50_ms, p90_ms, p95_ms, p99_ms, rps
- Site: name, base_url, description, environment
- ComparisonSnapshot: name, run_ids (JSON array), annotations, created

**Key Validation:**
- Write throughput: Can Trovato handle 1000+ metric writes/second?
- Query freshness: Can Gather return data written seconds ago?
- This is a different access pattern than Argus (write-once) or Netgrasp (event-driven)

**Tasks:**
- [ ] Define Goose content types as Trovato item_types
- [ ] Benchmark write throughput (simulate 1000 req/s metric ingestion)
- [ ] Test Gather query freshness (write → query latency)
- [ ] Create "runs by site" Gather with date range filter
- [ ] Create "endpoints by latency" Gather with aggregation
- [ ] Design real-time dashboard pattern (polling vs SSE vs WebSocket)
- [ ] Verify auth roles (operator vs viewer)
- [ ] Document performance characteristics and any buffering strategies

**Dev Notes:**
- May need write batching or async queue for high-throughput scenarios
- Real-time updates could use polling initially, SSE/WebSocket later
- Goose also tests Trovato itself (Phase 6), creating a dual relationship

---

## Epic 21: Complete Stage Workflow

**Goal:** Extend staging (workspaces) to cover all configurable entities, enabling full preview-and-publish workflows. The central lesson from Drupal Workspaces: define the interface contract now, even if the UI is deferred.

**Scope:**
- ConfigStorage trait for all config entity access (v1.0 required)
- Config revision schema scaffolding (v1.0 schema only)
- Stage-aware content types, fields, categories (post-MVP)
- Stage-aware menus and URL aliases (post-MVP)
- Conflict detection with warn-only UI (post-MVP)
- Atomic publish ordering framework (v1.0 framework)
- Stage hierarchy support (post-MVP)

**Gate:**
- v1.0: ConfigStorage trait used by all config reads/writes; publish ordering framework in place
- Post-MVP: Full config staging UI with conflict detection

**Design Principles:**
1. All config reads/writes go through ConfigStorage trait - no bypassing
2. Keep interface surface small (load, save, delete, list)
3. v1.0 uses DirectConfigStorage; post-MVP swaps in StageAwareConfigStorage decorator
4. Publish ordering: config first, then content, then dependent entities (menus, aliases)

**Entity Tiers:**
- Tier 1 (must be stage-aware): item_type, field_config, field_instance, category_*
- Tier 2 (important): menu, menu_link, url_alias
- Tier 3 (defer - security risk): role_permission, variable

**Cross-references:** [[Projects/Trovato/Design-Content-Model]], Drupal Workspaces integration patterns

---

### Story 21.1: ConfigStorage Trait (v1.0 Required)

As an **architect**,
I want all config entity reads/writes to go through a ConfigStorage interface,
So that stage-aware config is a decorator swap, not a rewrite.

**Acceptance Criteria:**

1. `ConfigStorage` trait defined with core methods:
   ```rust
   #[async_trait]
   pub trait ConfigStorage: Send + Sync {
       async fn load(&self, entity_type: &str, id: &str) -> Result<Option<ConfigEntity>>;
       async fn save(&self, entity: &ConfigEntity) -> Result<()>;
       async fn delete(&self, entity_type: &str, id: &str) -> Result<()>;
       async fn list(&self, entity_type: &str, filter: Option<&Filter>) -> Result<Vec<ConfigEntity>>;
   }
   ```

2. All config entity types use this interface:
   - `item_type`, `field_config`, `field_instance`
   - `category_vocabulary`, `category_term`
   - `menu`, `menu_link`
   - `url_alias` (from Story 15.5)
   - `variable` (site config)

3. v1.0 implementation: `DirectConfigStorage` - no stage awareness, just clean interface

4. No code paths bypass the trait (no raw SQL for config reads)

5. Interface is small and stable - enables decoration without changing call sites

**Tasks:**
- [ ] Define ConfigStorage trait in crates/kernel/src/config/mod.rs
- [ ] Define ConfigEntity enum or struct covering all entity types
- [ ] Implement DirectConfigStorage
- [ ] Refactor item_type loading to use ConfigStorage
- [ ] Refactor field_config/field_instance to use ConfigStorage
- [ ] Refactor category_vocabulary/category_term to use ConfigStorage
- [ ] Refactor menu/menu_link to use ConfigStorage
- [ ] Add ConfigStorage to AppState
- [ ] Audit for any raw SQL config reads and migrate them

**Dev Notes:**
- This is the Drupal Workspaces lesson: if subsystems bypass entity loading, stage awareness breaks
- Keep the interface surface small (Fabian's principle)
- The trait enables post-MVP stage awareness without touching call sites

---

### Story 21.2: Config Revision Schema (v1.0 Schema Only)

As an **architect**,
I want config revision tables in the schema,
So that adding config staging later is schema-ready.

**Acceptance Criteria:**

1. Migration creates these tables (empty, not populated in v1.0):
   ```sql
   CREATE TABLE config_revision (
       id UUID PRIMARY KEY,
       entity_type VARCHAR(64) NOT NULL,
       entity_id UUID NOT NULL,
       data JSONB NOT NULL,
       created BIGINT NOT NULL,
       author_id UUID REFERENCES users(id)
   );
   CREATE INDEX idx_config_revision_entity ON config_revision(entity_type, entity_id);

   CREATE TABLE config_stage_association (
       stage_id VARCHAR(64) NOT NULL REFERENCES stage(id),
       entity_type VARCHAR(64) NOT NULL,
       entity_id UUID NOT NULL,
       target_revision_id UUID NOT NULL REFERENCES config_revision(id),
       PRIMARY KEY (stage_id, entity_type, entity_id)
   );
   ```

2. `stage_deletion` table already supports config entities via `entity_type` column

3. No v1.0 code writes to these tables - they're scaffolding for post-MVP

4. Schema documented in design docs for future reference

**Tasks:**
- [ ] Create migration file for config_revision table
- [ ] Create migration file for config_stage_association table
- [ ] Verify stage_deletion already has entity_type column
- [ ] Document schema in Design-Content-Model.md

---

### Story 21.3: Stage-Aware Content Types & Fields (Post-MVP)

As a **site administrator**,
I want to add/modify content types and fields in a stage,
So that I can test schema changes before going live.

**Acceptance Criteria:**

1. **Given** I create a content type in a stage
   **When** I view that stage
   **Then** the type exists and I can create items of that type
   **And** the type does not exist in Live

2. **Given** I add a field to an existing content type in a stage
   **When** I view items in that stage
   **Then** the field appears on edit forms
   **And** Live items don't have the field

3. **Given** I publish a stage with content type changes
   **When** publishing completes
   **Then** the content type/field exists in Live
   **And** existing items get default values for new fields

4. Stage preview shows content forms with staged field configuration

**Tasks:**
- [ ] Implement StageAwareConfigStorage decorator
- [ ] Wire item_type reads through stage-aware path
- [ ] Wire field_config/field_instance through stage-aware path
- [ ] Handle field migration on publish (add columns, backfill defaults)
- [ ] Update admin UI forms to show staged fields
- [ ] Add integration tests for staged content type workflow

**Dev Notes:**
- This is Tier 1 - content depends on type definitions existing in stage
- Field migration on publish needs careful handling
- Requires 21.1 (ConfigStorage trait) to be complete

---

### Story 21.4: Stage-Aware Categories (Post-MVP)

As a **site administrator**,
I want to modify categories in a stage,
So that I can reorganize taxonomy and tag staged content with new terms.

**Acceptance Criteria:**

1. **Given** I create a vocabulary/term in a stage
   **When** I edit items in that stage
   **Then** I can tag items with the new term
   **And** the term doesn't exist in Live

2. **Given** I reorganize term hierarchy in a stage
   **When** I view breadcrumbs/hierarchy in that stage
   **Then** the new hierarchy is reflected
   **And** Live hierarchy is unchanged

3. **Given** I delete a term in a stage (via stage_deletion)
   **When** I view items in that stage
   **Then** items show the term as orphaned/removed
   **And** Live items still have the term

4. **Given** I publish a stage with category changes
   **When** publishing completes
   **Then** vocabulary/term changes apply to Live
   **And** term deletions remove terms from Live

**Tasks:**
- [ ] Wire category_vocabulary through StageAwareConfigStorage
- [ ] Wire category_term through StageAwareConfigStorage
- [ ] Handle hierarchy queries with stage awareness (recursive CTE adjustment)
- [ ] Update category admin UI for stage context
- [ ] Handle term deletion via stage_deletion table
- [ ] Add integration tests

**Dev Notes:**
- Terms may be referenced by staged content - must resolve within stage
- Hierarchy queries need stage-aware CTE wrapping

---

### Story 21.5: Stage-Aware Menus & Aliases (Post-MVP)

As a **site administrator**,
I want menus and URL aliases to be stage-aware,
So that navigation and URLs work correctly in stage preview.

**Acceptance Criteria:**

1. **Given** I create a menu item linking to staged content
   **When** I preview the stage
   **Then** the menu item appears and links work
   **And** Live menus don't show the item

2. **Given** I create a URL alias for staged content
   **When** I access that alias in stage preview
   **Then** the alias resolves to the staged item
   **And** the alias doesn't exist in Live

3. **Given** two stages create the same alias for different items
   **When** I try to create the second alias
   **Then** a conflict warning is shown

4. **Given** I publish a stage with menu/alias changes
   **When** publishing completes
   **Then** menus and aliases are live

**Tasks:**
- [ ] Wire menu/menu_link through StageAwareConfigStorage
- [ ] Wire url_alias through StageAwareConfigStorage (connects to 15.5)
- [ ] Update path alias middleware to be stage-aware
- [ ] Update menu rendering to be stage-aware
- [ ] Handle alias conflicts across stages
- [ ] Add integration tests

**Dev Notes:**
- URL aliases (15.5) should use ConfigStorage from day one to enable this
- Menu items may reference staged content - must validate references

---

### Story 21.6: Conflict Detection - Warn Only (Post-MVP)

As a **site administrator**,
I want warnings when publishing over changed live content,
So that I don't accidentally overwrite others' work.

**Acceptance Criteria:**

1. **Given** I have staged changes to an item
   **And** that item was modified in Live after my stage was created
   **When** I attempt to publish
   **Then** a conflict warning is displayed:
   - "Item 'About Us' was modified in Live on Feb 10 by admin"

2. **Given** I have staged changes to config (field, term, menu)
   **And** that config was changed in Live
   **When** I attempt to publish
   **Then** a conflict warning is displayed:
   - "Field 'field_tags' on 'Article' was changed in Live"

3. For each conflict, I can choose:
   - **Overwrite** - publish anyway (Last Publish Wins)
   - **Skip** - don't publish this entity, continue with others
   - **Cancel** - abort entire publish operation

4. No merge UI - detect and warn only

**Tasks:**
- [ ] Add conflict detection to stage_publish() for items
- [ ] Add conflict detection for config entities
- [ ] Create conflict display UI (list of conflicts with actions)
- [ ] Implement skip/overwrite/cancel logic
- [ ] Add integration tests for conflict scenarios

**Dev Notes:**
- Compare stage_association.target_revision_id vs current live revision
- For config: compare config_stage_association vs current live state
- Full three-way merge requires field-level diff semantics - defer to future

---

### Story 21.7: Atomic Publish Ordering Framework (v1.0 Framework)

As an **architect**,
I want a publish ordering framework,
So that config and content publish in the correct dependency order.

**Acceptance Criteria:**

1. Publish order is defined and enforced:
   ```
   Phase 1: Content types, fields (nothing depends on these)
   Phase 2: Categories (may be referenced by content)
   Phase 3: Content items (depend on types and categories existing)
   Phase 4: Menus, aliases (reference content)
   ```

2. All phases run in single Postgres transaction

3. If any phase fails, entire transaction rolls back

4. v1.0: Only Phase 3 (items) is active - other phases are no-op hooks
   Post-MVP: Wire up other phases as config staging is added

5. Cache invalidation follows same ordering (invalidate after all writes)

6. Publish function accepts phase callbacks:
   ```rust
   pub struct PublishPhases {
       pub config_types: Box<dyn Fn(&mut Transaction) -> Result<()>>,
       pub categories: Box<dyn Fn(&mut Transaction) -> Result<()>>,
       pub items: Box<dyn Fn(&mut Transaction) -> Result<()>>,
       pub dependents: Box<dyn Fn(&mut Transaction) -> Result<()>>,
   }
   ```

**Tasks:**
- [ ] Refactor stage_publish() into phased structure
- [ ] Define PublishPhases struct with phase callbacks
- [ ] v1.0: Implement items phase (existing logic)
- [ ] v1.0: Add no-op hooks for other phases
- [ ] Ensure single transaction wraps all phases
- [ ] Document publish ordering in design docs
- [ ] Add integration tests for rollback on failure

**Dev Notes:**
- The framework is what matters for v1.0
- Current stage_publish() becomes Phase 3 (items)
- Ordering prevents "Event item published before Event type" errors

---

### Story 21.8: Stage Hierarchy Support (Post-MVP)

As a **site administrator**,
I want to publish a child stage to its parent (not necessarily Live),
So that I can have multi-level editorial workflows.

**Acceptance Criteria:**

1. **Given** a stage with `upstream_id` set to another stage (not Live)
   **When** I load content in the child stage
   **Then** I see parent's staged content as baseline
   **And** child's changes override parent

2. **Given** I publish child stage to its parent
   **When** publishing completes
   **Then** parent stage receives child's changes
   **And** Live is unchanged

3. **Given** I later publish parent stage to Live
   **When** publishing completes
   **Then** all changes (including merged child changes) go Live

4. Publish ordering guarantees apply to parent-child publishes

**Tasks:**
- [ ] Update stage_publish() to accept target_stage parameter
- [ ] Implement inheritance: child falls back to parent's stage_association
- [ ] Handle child → parent publish flow
- [ ] Update stage admin UI to show hierarchy
- [ ] Add integration tests for multi-level workflow

**Dev Notes:**
- Schema already has `upstream_id` on `stage` table
- Enables "Spring Campaign" → "Q1 Staging" → "Live" workflows
- Complex but powerful for enterprise editorial teams

---

## Epic 22: Plugin Architecture & Standard Plugins

**Governing Principle:** Core enables. Plugins implement.

The Kernel provides infrastructure and interfaces. It does not provide features. Features are plugins. The "blog" content type is a plugin. The categories system is a plugin. Comments, media, search, image processing -- all plugins. If someone doesn't like the standard implementation, they replace the plugin. Core stays small.

**Goal:** Complete the Kernel infrastructure that enables plugins to implement features, then build/refactor the standard plugins that ship with Trovato.

**Scope:**
- Kernel Infrastructure: Plugin migrations, Gather extension API, compound field type, image hooks, language column
- Standard Plugins: Media, Redirects, OAuth2, Image Styles, Scheduled Publishing, Webhooks, Audit Log, Content Locking
- Refactoring: Move Categories and Comments from Kernel code to plugins
- Multilingual: Language infrastructure (Kernel) + translation plugins (post-MVP)

**Gate:**
- v1.0: Plugin migration infrastructure works; Gather extension API formalized; language column added
- Post-MVP: Standard plugin inventory complete; Categories/Comments refactored to plugins

**Plugin Tiers:**
- **Standard Plugins** - Ship with Trovato, replaceable (Page, Blog, Categories, Comments, Media, etc.)
- **Contrib Plugins** - Community-built (Content Moderation, GraphQL, Layout Builder)
- **Project-Specific Plugins** - Single use case (Argus, Netgrasp, Goose)

**Cross-references:** [[Projects/Trovato/Design-Plugin-System]], Argus/Netgrasp/Goose integration docs

---

## Section A: Kernel Infrastructure

These must exist in Core for plugins to work properly.

---

### Story 22.1: Plugin Entity Type Registration

As a **plugin developer**,
I want to declare database tables in my plugin's info.toml,
So that my plugin can manage its own entity types without modifying Kernel code.

**Acceptance Criteria:**

1. Plugin `info.toml` supports migration declarations:
   ```toml
   [migrations]
   files = ["migrations/001_create_media.sql", "migrations/002_add_media_fields.sql"]
   depends_on = ["categories"]  # Run after categories plugin migrations
   ```

2. Kernel provides plugin migration runner that:
   - Reads migration declarations from installed plugins
   - Resolves dependency order (Categories before Blog)
   - Runs migrations on plugin install
   - Tracks applied migrations in `plugin_migration` table

3. `plugin_migration` table schema:
   ```sql
   CREATE TABLE plugin_migration (
       plugin VARCHAR(64) NOT NULL,
       migration VARCHAR(255) NOT NULL,
       applied_at BIGINT NOT NULL,
       PRIMARY KEY (plugin, migration)
   );
   ```

4. Migration rollback supported for plugin uninstall (optional, can be manual)

**Tasks:**
- [ ] Add migration declaration support to info.toml parser
- [ ] Create plugin_migration table
- [ ] Implement migration dependency resolver
- [ ] Implement migration runner (run on plugin install)
- [ ] Add CLI command: `trovato migrate:plugin <plugin_name>`
- [ ] Document plugin migration format

**Dev Notes:**
- Similar to D6's system table tracking schema versions
- Categories and Comments will need refactoring to use this (see 22.14)

---

### Story 22.2: Gather Extension API

As a **plugin developer**,
I want to register custom filter types, relationship types, and sort handlers with Gather,
So that my plugin can extend query capabilities without modifying Kernel code.

**Acceptance Criteria:**

1. Plugins can register custom filter types:
   ```rust
   gather.register_filter_type("hierarchical_term", |params, builder| {
       // Generate SQL fragment for hierarchical term filtering
       // Uses recursive CTE pattern
   });
   ```

2. Plugins can register custom relationship types:
   ```rust
   gather.register_relationship_type("term_items", |params, builder| {
       // Generate JOIN for term → items relationship
   });
   ```

3. Plugins can register custom sort handlers:
   ```rust
   gather.register_sort_handler("term_weight", |direction, builder| {
       // Generate ORDER BY for term weight
   });
   ```

4. Registration happens during plugin initialization (tap_init or similar)

5. Categories plugin uses this API (refactored from any special-casing)

**Tasks:**
- [ ] Audit current Categories integration - is it generic or special-cased?
- [ ] Define FilterTypeHandler, RelationshipTypeHandler, SortHandler traits
- [ ] Add registration methods to GatherEngine
- [ ] Store registered handlers in plugin-accessible registry
- [ ] Refactor Categories to use registration API
- [ ] Document Gather extension API for plugin developers

**Dev Notes:**
- Check if Netgrasp's "device state transition" filter can be implemented via this API
- This is what makes Gather truly extensible vs. hard-coded

---

### Story 22.3: Compound/Polymorphic Field Type

As a **content editor**,
I want fields that store arrays of typed, reorderable content sections,
So that I can build rich pages without a separate "paragraphs" system.

**Acceptance Criteria:**

1. Field type `compound` registered in core field type registry:
   ```rust
   FieldType::Compound {
       allowed_types: vec!["text", "image", "quote", "video"],
       min_items: 0,
       max_items: None,  // unlimited
   }
   ```

2. JSONB storage format:
   ```json
   "field_sections": [
     { "type": "text", "body": { "value": "<p>...</p>", "format": "filtered_html" }, "weight": 0 },
     { "type": "image", "file_id": "...", "alt": "...", "caption": "...", "weight": 1 },
     { "type": "quote", "text": "...", "attribution": "...", "weight": 2 }
   ]
   ```

3. Each section type defines its own schema for validation

4. Form widget supports:
   - Adding sections of any allowed type
   - Reordering via drag-drop or weight adjustment
   - Removing sections
   - Inline editing per section type

5. Render pipeline handles compound fields (iterates sections, applies per-type templates)

**Tasks:**
- [ ] Define CompoundFieldType in field type registry
- [ ] Define section type schema format
- [ ] Implement validation for compound fields
- [ ] Create compound field form widget (AJAX add/remove/reorder)
- [ ] Create compound field display formatter
- [ ] Add section type templates (text, image, quote, video as examples)
- [ ] Document compound field usage for plugin developers

**Dev Notes:**
- This is Paragraphs without the complexity
- Section types could be plugin-registered in future (22.2 pattern)

---

### Story 22.4: Image Processing Hooks

As a **plugin developer**,
I want a `tap_file_url` integration point,
So that my Image Styles plugin can rewrite image URLs and trigger on-demand processing.

**Acceptance Criteria:**

1. Kernel provides `tap_file_url` hook:
   ```rust
   // Called when generating public URL for a file
   fn tap_file_url(file: &File, context: &UrlContext) -> Option<String>;
   ```

2. Plugins can intercept and rewrite URLs:
   - Original: `/files/images/photo.jpg`
   - Rewritten: `/files/styles/thumbnail/images/photo.jpg`

3. Kernel routing handles style URL pattern and delegates to plugin for processing

4. Integration with FileStorage trait for reading source and writing processed files

**Tasks:**
- [ ] Define tap_file_url in WIT interface
- [ ] Add URL generation hook point in file serving code
- [ ] Add route pattern for `/files/styles/{style}/**`
- [ ] Define FileProcessor trait for on-demand processing
- [ ] Document image hook usage

**Dev Notes:**
- The hook is Kernel infrastructure; the actual Image Styles plugin is 22.10

---

### Story 22.5: Language Infrastructure

As an **architect**,
I want language support built into the Kernel from day one,
So that multilingual can be added post-MVP without retrofitting the item table.

**Acceptance Criteria:**

1. `language` table created:
   ```sql
   CREATE TABLE language (
       id VARCHAR(12) PRIMARY KEY,
       label VARCHAR(255) NOT NULL,
       weight INTEGER NOT NULL DEFAULT 0,
       is_default BOOLEAN NOT NULL DEFAULT false,
       direction VARCHAR(3) NOT NULL DEFAULT 'ltr'
   );
   INSERT INTO language (id, label, is_default) VALUES ('en', 'English', true);
   ```

2. `language` column added to item table:
   ```sql
   ALTER TABLE item ADD COLUMN language VARCHAR(12) DEFAULT 'en' REFERENCES language(id);
   ```

3. `LanguageNegotiator` trait defined for pluggable detection:
   ```rust
   #[async_trait]
   pub trait LanguageNegotiator: Send + Sync {
       async fn negotiate(&self, request: &Request) -> Option<String>;
       fn priority(&self) -> i32;
   }
   ```

4. Negotiation middleware chains negotiators (URL prefix, domain, cookie, header)

5. Resolved language available in request context for templates and queries

**Tasks:**
- [ ] Create language table migration
- [ ] Add language column to item table migration
- [ ] Define LanguageNegotiator trait
- [ ] Implement default negotiators (URL prefix, Accept-Language header)
- [ ] Add negotiation middleware to request pipeline
- [ ] Store resolved language in request context
- [ ] Document language infrastructure

**Dev Notes:**
- Same principle as stage_id - add column now, avoid retrofitting later
- Monolingual sites never think about it
- Full multilingual plugins (22.15-22.17) are post-MVP

---

## Section B: Standard Plugins

Plugins that ship with Trovato. All are replaceable.

---

### Story 22.6: Media Plugin

As a **content editor**,
I want a media library,
So that I can reuse uploaded files across content with metadata.

**Acceptance Criteria:**

1. Media entity type wrapping file_managed:
   ```sql
   CREATE TABLE media (
       id UUID PRIMARY KEY,
       file_id UUID NOT NULL REFERENCES file_managed(id),
       name VARCHAR(255) NOT NULL,
       alt_text VARCHAR(255),
       caption TEXT,
       credit VARCHAR(255),
       created BIGINT NOT NULL,
       changed BIGINT NOT NULL,
       author_id UUID REFERENCES users(id)
   );
   ```

2. Media browser (Gather-powered) for selecting/reusing existing media

3. Image field widget integrates with media browser (select existing or upload new)

4. Media usage tracked (which items reference which media)

5. Media admin UI for browsing, editing metadata, viewing usage

**Tasks:**
- [ ] Create media plugin with info.toml and migrations (uses 22.1)
- [ ] Define Media entity type
- [ ] Create media browser Gather configuration
- [ ] Create media field widget
- [ ] Implement media usage tracking
- [ ] Create media admin UI
- [ ] Add integration tests

**Dev Notes:**
- Sites reference media items via RecordReference fields, not file_managed directly
- Gets revision tracking, stage awareness, searchability for free

---

### Story 22.7: Redirects Plugin

As a **site administrator**,
I want automatic redirects when URL aliases change,
So that old links don't break.

**Acceptance Criteria:**

1. `redirect` table:
   ```sql
   CREATE TABLE redirect (
       id UUID PRIMARY KEY,
       source VARCHAR(512) NOT NULL,
       destination VARCHAR(512) NOT NULL,
       status_code SMALLINT NOT NULL DEFAULT 301,
       language VARCHAR(12),
       created BIGINT NOT NULL,
       UNIQUE (source, language)
   );
   ```

2. When URL alias changes, old alias automatically becomes a redirect

3. Redirect middleware checks redirects before alias resolution

4. Admin UI for managing redirects (view, edit, delete, create manual)

5. Redirect loops detected and prevented

**Tasks:**
- [ ] Create redirects plugin with migrations
- [ ] Implement redirect creation on alias change (hook into URL Aliases plugin)
- [ ] Add redirect middleware to routing pipeline
- [ ] Create redirects admin UI
- [ ] Add loop detection
- [ ] Add integration tests

---

### Story 22.8: OAuth2 Provider Plugin

As an **API consumer** (iOS app, SPA, external service),
I want bearer token authentication,
So that I can authenticate without cookies.

**Acceptance Criteria:**

1. `oauth_client` table for application registration:
   ```sql
   CREATE TABLE oauth_client (
       id UUID PRIMARY KEY,
       client_id VARCHAR(64) NOT NULL UNIQUE,
       client_secret_hash VARCHAR(255) NOT NULL,
       name VARCHAR(255) NOT NULL,
       redirect_uris JSONB DEFAULT '[]',
       grant_types JSONB DEFAULT '["authorization_code"]',
       created BIGINT NOT NULL
   );
   ```

2. Supported grant types:
   - `authorization_code` - for user-facing apps (Argus iOS)
   - `client_credentials` - for server-to-server (webhooks, integrations)

3. JWT tokens (self-contained, signature-verified):
   - Access token with configurable expiration
   - Refresh token for token renewal
   - No per-request Redis lookup (signature verification only)

4. Token revocation via Redis blocklist (for logout, security)

5. OAuth endpoints: `/oauth/authorize`, `/oauth/token`, `/oauth/revoke`

6. Admin UI for managing OAuth clients

**Tasks:**
- [ ] Create oauth plugin with migrations
- [ ] Implement JWT signing/verification
- [ ] Implement authorization_code flow
- [ ] Implement client_credentials flow
- [ ] Implement token revocation with Redis blocklist
- [ ] Create OAuth client admin UI
- [ ] Add integration tests
- [ ] Document OAuth setup for API consumers

**Dev Notes:**
- Argus iOS app and Goose management UI need this
- Session cookies don't work for mobile apps or SPAs

---

### Story 22.9: Image Styles Plugin

As a **site builder**,
I want named image styles with processing effects,
So that images are automatically resized/cropped for different contexts.

**Acceptance Criteria:**

1. Image styles configuration:
   ```sql
   CREATE TABLE image_style (
       id UUID PRIMARY KEY,
       name VARCHAR(64) NOT NULL UNIQUE,
       label VARCHAR(255) NOT NULL,
       effects JSONB NOT NULL  -- [{"type": "scale", "width": 200}, {"type": "crop", "width": 200, "height": 200}]
   );
   ```

2. Default styles: thumbnail (100x100 crop), medium (500 wide scale), large (1000 wide scale)

3. Processing effects: scale, crop, resize, desaturate

4. On-demand processing: first request generates styled image, cached to disk

5. URL pattern: `/files/styles/{style_name}/path/to/image.jpg`

6. Uses tap_file_url hook (22.4) to rewrite image URLs

7. Admin UI for managing styles and effects

**Tasks:**
- [ ] Create image_styles plugin with migrations
- [ ] Implement image processing using `image` crate
- [ ] Implement effect chain (scale → crop → etc.)
- [ ] Implement on-demand processing with disk caching
- [ ] Register tap_file_url handler
- [ ] Create styles admin UI
- [ ] Add integration tests

---

### Story 22.10: Scheduled Publishing Plugin

As a **content editor**,
I want to schedule content to publish/unpublish at specific times,
So that content goes live automatically.

**Acceptance Criteria:**

1. Fields added to items (via field_config, not schema change):
   - `publish_on` - timestamp when item should be published
   - `unpublish_on` - timestamp when item should be unpublished

2. Cron handler (`tap_cron`) checks timestamps and updates item status

3. Items with future `publish_on` are created with status=0 (unpublished)

4. Admin UI shows scheduled state on content list

5. Scheduling visible on item edit form

**Tasks:**
- [ ] Create scheduled_publishing plugin
- [ ] Register publish_on/unpublish_on fields
- [ ] Implement cron handler for status changes
- [ ] Add scheduling UI to item edit form
- [ ] Add scheduled filter to content list
- [ ] Add integration tests

---

### Story 22.11: Webhooks Plugin

As an **integration developer**,
I want HTTP notifications on content changes,
So that external systems stay synchronized.

**Acceptance Criteria:**

1. `webhook` table:
   ```sql
   CREATE TABLE webhook (
       id UUID PRIMARY KEY,
       name VARCHAR(255) NOT NULL,
       url VARCHAR(512) NOT NULL,
       events JSONB NOT NULL,  -- ["item.create", "item.update", "item.delete"]
       secret VARCHAR(255),  -- for HMAC signature
       active BOOLEAN NOT NULL DEFAULT true,
       created BIGINT NOT NULL
   );
   ```

2. Taps fire webhooks on item CRUD: tap_item_insert, tap_item_update, tap_item_delete

3. Payload includes: event type, item ID, item type, changed fields, timestamp

4. HMAC signature in header for verification

5. Retry logic for failed deliveries (queue-based)

6. Admin UI for managing webhooks

**Tasks:**
- [ ] Create webhooks plugin with migrations
- [ ] Implement webhook dispatch on item taps
- [ ] Implement HMAC signing
- [ ] Implement retry queue
- [ ] Create webhooks admin UI
- [ ] Add integration tests

**Dev Notes:**
- Essential for headless/decoupled architectures
- Argus could use this for pipeline triggers

---

### Story 22.12: Audit Log Plugin

As a **site administrator**,
I want a log of significant actions,
So that I can audit who did what and when.

**Acceptance Criteria:**

1. `audit_log` table:
   ```sql
   CREATE TABLE audit_log (
       id UUID PRIMARY KEY,
       action VARCHAR(64) NOT NULL,
       entity_type VARCHAR(64),
       entity_id UUID,
       user_id UUID REFERENCES users(id),
       ip_address VARCHAR(45),
       details JSONB,
       created BIGINT NOT NULL
   );
   CREATE INDEX idx_audit_log_created ON audit_log(created DESC);
   CREATE INDEX idx_audit_log_user ON audit_log(user_id);
   CREATE INDEX idx_audit_log_entity ON audit_log(entity_type, entity_id);
   ```

2. Actions logged via taps:
   - Item CRUD (create, update, delete, publish)
   - User login/logout/failed login
   - Permission changes
   - Config changes

3. Gather-powered admin view for browsing the log with filters

4. Log retention policy (configurable, default 90 days)

**Tasks:**
- [ ] Create audit_log plugin with migrations
- [ ] Implement logging via taps
- [ ] Create audit log admin UI (Gather-powered)
- [ ] Implement retention cleanup in cron
- [ ] Add integration tests

---

### Story 22.13: Content Locking Plugin

As a **content editor**,
I want to know if someone else is editing content,
So that we don't overwrite each other's work.

**Acceptance Criteria:**

1. `editing_lock` table:
   ```sql
   CREATE TABLE editing_lock (
       entity_type VARCHAR(64) NOT NULL,
       entity_id UUID NOT NULL,
       user_id UUID NOT NULL REFERENCES users(id),
       locked_at BIGINT NOT NULL,
       expires_at BIGINT NOT NULL,
       PRIMARY KEY (entity_type, entity_id)
   );
   ```

2. Lock acquired on form load, released on save or timeout (configurable, default 15 min)

3. Lock check on form load: if locked by another user, show warning with options:
   - View content (read-only)
   - Break lock (admin only)
   - Wait and retry

4. Lock heartbeat via AJAX to extend expiration while editing

5. Stale locks cleaned up by cron

**Tasks:**
- [ ] Create content_locking plugin with migrations
- [ ] Implement lock acquisition on form load
- [ ] Implement lock release on save
- [ ] Implement lock heartbeat
- [ ] Add lock warning UI
- [ ] Implement break lock for admins
- [ ] Add cron cleanup
- [ ] Add integration tests

---

### Story 22.14: Refactor Categories and Comments to Plugins

As an **architect**,
I want Categories and Comments to be true plugins,
So that they follow the "core enables, plugins implement" principle.

**Acceptance Criteria:**

1. Categories plugin:
   - Own `info.toml` with migration declarations
   - Migrations moved from Kernel to plugin
   - Model code moved to plugin
   - Routes registered via tap_menu
   - Uses Gather extension API (22.2) for hierarchical filters

2. Comments plugin:
   - Own `info.toml` with migration declarations
   - Migrations moved from Kernel to plugin
   - Model code moved to plugin
   - Routes registered via tap_menu
   - tap_form_alter to add comment forms to items

3. Kernel migrations only contain core tables (users, roles, items, stages, etc.)

4. Plugin migrations run after Kernel migrations via 22.1 infrastructure

**Tasks:**
- [ ] Create categories plugin structure (info.toml, migrations/)
- [ ] Move category migrations from Kernel to plugin
- [ ] Move category model/service code to plugin crate
- [ ] Register category routes via tap_menu
- [ ] Create comments plugin structure
- [ ] Move comment migrations from Kernel to plugin
- [ ] Move comment model/service code to plugin crate
- [ ] Register comment routes via tap_menu
- [ ] Update Kernel migrations to remove category/comment tables
- [ ] Add integration tests for plugin loading

**Dev Notes:**
- This is refactoring, not new functionality
- Validates that 22.1 (plugin migrations) works correctly
- Sets the pattern for all future entity-providing plugins

---

## Section C: Multilingual Plugins (Post-MVP)

---

### Story 22.15: Content Translation Plugin

As a **multilingual site editor**,
I want to translate content field-by-field,
So that I can have partial translations with fallback.

**Acceptance Criteria:**

1. `item_translation` table:
   ```sql
   CREATE TABLE item_translation (
       item_id UUID NOT NULL REFERENCES item(id) ON DELETE CASCADE,
       language VARCHAR(12) NOT NULL REFERENCES language(id),
       title VARCHAR(255) NOT NULL,
       fields JSONB DEFAULT '{}'::jsonb,
       PRIMARY KEY (item_id, language)
   );
   ```

2. Field-level fallback: if field not in translation JSONB, fall back to default language

3. Translations participate in revisions (add language to item_revision)

4. Translations participate in staging (stage_association pattern)

5. Translation UI: side-by-side editing with source language

6. Gather accepts language parameter for filtered queries

**Tasks:**
- [ ] Create content_translation plugin with migrations
- [ ] Implement translation storage and fallback
- [ ] Integrate with revision system
- [ ] Integrate with stage system
- [ ] Create translation UI
- [ ] Update Gather for language filtering
- [ ] Add integration tests

---

### Story 22.16: Locale Plugin (UI Strings)

As a **multilingual site builder**,
I want to translate UI strings,
So that the interface appears in the user's language.

**Acceptance Criteria:**

1. `locale_string` table:
   ```sql
   CREATE TABLE locale_string (
       id UUID PRIMARY KEY,
       source TEXT NOT NULL,
       translation TEXT NOT NULL,
       language VARCHAR(12) NOT NULL REFERENCES language(id),
       context VARCHAR(255),
       UNIQUE (source, language, context)
   );
   ```

2. Tera function `{{ t("Read more") }}` for translatable strings

3. .po file import for bulk translation loading

4. Translation UI for managing strings

5. String extraction tool for finding translatable strings in templates

**Tasks:**
- [ ] Create locale plugin with migrations
- [ ] Implement t() Tera function
- [ ] Implement .po file parser and importer
- [ ] Create locale string admin UI
- [ ] Create string extraction tool
- [ ] Add integration tests

---

### Story 22.17: Config Translation Plugin

As a **multilingual site administrator**,
I want to translate config labels (menus, field labels, vocabulary names),
So that the admin interface and public-facing config are localized.

**Acceptance Criteria:**

1. `config_translation` table:
   ```sql
   CREATE TABLE config_translation (
       entity_type VARCHAR(64) NOT NULL,
       entity_id UUID NOT NULL,
       language VARCHAR(12) NOT NULL REFERENCES language(id),
       data JSONB NOT NULL,
       PRIMARY KEY (entity_type, entity_id, language)
   );
   ```

2. Translation overlays for: menu labels, field labels, vocabulary/term names

3. Builds on ConfigStorage trait (Epic 21) - translations applied at read time

4. Translation UI for config entities

**Tasks:**
- [ ] Create config_translation plugin with migrations
- [ ] Implement ConfigStorage decorator for translation overlay
- [ ] Create config translation UI
- [ ] Add integration tests

**Dev Notes:**
- Depends on 21.1 (ConfigStorage trait)

---

## Epic 23: Gather UI & Query Consolidation

**Governing Principle:** Every list a user might want to customize is a Gather. Every query that isn't a Gather has a reason not to be.

The Gather engine already exists as a backend: SeaQuery-based dynamic query builder with JSONB field extraction, category-aware hierarchical filters, contextual values, includes (nested sub-queries), and display configuration. What's missing is the admin UI for managing Gather definitions, and the migration of hardcoded model-layer list queries to Gather definitions.

**Goal:** Build the admin interface for creating, editing, and previewing Gather definitions (Part 1), then convert hardcoded listing queries in the model layer to Gather definitions so every listing runs through Gather and can be customized through the UI (Part 2).

**Scope:**
- Part 1: Block-based Gather admin UI (Metabase-inspired, progressive disclosure, live preview)
- Part 2: Convert ~40 hardcoded listing queries to default Gather definitions
- Performance guardrails: mandatory pagination caps, join depth limits, query timeouts, JSONB index warnings

**Gate:**
- Admin can create, edit, clone, and preview Gather definitions via the UI
- Core content listings (Items, Users, Comments) run through Gather
- Performance guardrails enforced at engine level, not just UI

**Design Philosophy:** "I found the Views UI overly complex. I hate that it was trivially easy to build absurdly bad queries that performed badly. I want something that makes sense and doesn't encourage abuse." — Progressive disclosure, performance guardrails baked into the engine, live preview at every step.

**UI Architecture:** Block-based composition (Metabase pattern). The query is built as a vertical stack of blocks — each block is one decision (base table, filter, sort, pagination). No modal dialogs, no separate tabs, no rearrange vs configure modes.

**Performance Guardrails:**
- Mandatory pagination (max configurable cap, default 100, no "show all")
- Join depth limit (default 3, configurable per-site)
- Required join conditions (schema-aware pre-population)
- JSONB filter warnings (check for expression indexes)
- No OR groups in v1 (all filters are AND)
- No leading wildcard searches (default to StartsWith, offer Contains with warning)
- Query cost indicator (EXPLAIN integration for admin preview)

**Cross-references:** Epic 7 (Gather backend), Epic 9 (Form API/exposed filters), Epic 22 (plugin architecture)

---

### Story 23.1: Gather Admin List Page

As a **site administrator**,
I want to see all registered Gather definitions in one place,
So that I can manage, edit, clone, and delete queries.

**Acceptance Criteria:**

1. Admin page at `/admin/gather` listing all Gather definitions
2. Table columns: query_id, label, item_type, plugin (source), created, changed
3. Actions per row: edit, clone, delete, preview
4. Plugin-provided views show "Provided by: {plugin}" badge
5. Filter by plugin source (all, core, specific plugin)
6. Pagination for the list itself

**Tasks:**
- [ ] Create Gather admin list route and handler
- [ ] Build list template with action links
- [ ] Implement clone action (duplicate definition with new query_id)
- [ ] Implement delete action with confirmation
- [ ] Add "Provided by" badge for plugin-registered views
- [ ] Add source filter dropdown

---

### Story 23.2: Gather Query Builder UI

As a **site administrator**,
I want a block-based query editor for building Gather definitions,
So that I can create custom content listings without writing code.

**Acceptance Criteria:**

1. Edit page at `/admin/gather/{query_id}/edit` with block-based editor
2. Base block: content type selector (dropdown of registered types)
3. Filter blocks: field picker + operator selector + value input, inline editing
4. Sort blocks: field picker + direction toggle, drag-to-reorder
5. Field picker shows base table columns and JSONB fields from the selected content type
6. Operator selector filtered to valid operators for the field type
7. Adding blocks via "+ Filter" / "+ Sort" buttons below the stack
8. Removing blocks via X button on each block
9. Create page at `/admin/gather/create` using the same editor

**Tasks:**
- [ ] Create Gather edit route and handler
- [ ] Build block-based editor form (base, filters, sorts sections)
- [ ] Implement field picker with content type awareness
- [ ] Implement operator selector filtered by field type
- [ ] Add filter value input (text, number, date, boolean, select)
- [ ] Add drag-to-reorder for sort blocks
- [ ] Implement save handler (serialize to ViewDefinition JSON)
- [ ] Create new-view flow starting with content type picker
- [ ] Handle exposed filter configuration (toggle + label)

**Dev Notes:**
- Each block must be a complete, valid query modification — no partial states
- Field picker must enumerate both base table columns and JSONB fields from content type definition
- Operator list per field type: Text → Equals/Contains/StartsWith/EndsWith/IsNull; Integer/Float → Equals/GreaterThan/LessThan/Between; Boolean → Equals; RecordReference → Equals/In/IsNull; Category → HasTag/HasAnyTag/HasAllTags/HasTagOrDescendants

---

### Story 23.3: Live Preview Panel

As a **site administrator**,
I want to see query results update as I build the definition,
So that I get immediate feedback on my query without saving first.

**Acceptance Criteria:**

1. Preview panel shows results from the current (unsaved) definition
2. Preview updates on every block change (debounced, ~500ms after last edit)
3. Shows result count, first N rows, and execution time
4. Shows "No results" with the configured empty_text if query returns nothing
5. Preview runs with current user's permissions (not bypassing access)
6. Preview panel at `/admin/gather/{query_id}/preview` for full-page view

**Tasks:**
- [ ] Create preview API endpoint (POST, accepts ViewDefinition JSON, returns results)
- [ ] Add preview panel to editor page (right side or bottom)
- [ ] Implement debounced AJAX preview on block changes
- [ ] Show result count, rows, and execution time
- [ ] Add full-page preview route
- [ ] Show generated SQL for admin users (collapsible)

**Dev Notes:**
- Preview endpoint must enforce all performance guardrails (pagination cap, join depth)
- Consider query timeout for preview (shorter than normal, e.g., 2s)

---

### Story 23.4: Relationship Editor

As a **site administrator**,
I want to add JOIN relationships to a Gather definition,
So that I can include data from related tables.

**Acceptance Criteria:**

1. Relationship block: join type (Inner/Left) + target table + local field + foreign field
2. Schema-aware pre-population: known relationships shown as suggestions
3. Join depth counter visible, "+ Relationship" grayed out at limit (default 3)
4. Joined table fields available in field picker and filter/sort blocks
5. Join depth limit configurable in admin settings

**Tasks:**
- [ ] Add relationship block to query builder
- [ ] Implement schema-aware relationship suggestions
- [ ] Add join depth counter and limit enforcement
- [ ] Make joined table fields available in field/filter/sort pickers
- [ ] Add admin setting for max join depth

---

### Story 23.5: Display Configuration

As a **site administrator**,
I want to configure how Gather results are displayed,
So that I can control format, pagination style, and empty text.

**Acceptance Criteria:**

1. Display section in editor: format (Table/List/Grid/Custom), items per page, pager style
2. Pager style options: Full (numbered pages), Mini (prev/next), Infinite scroll
3. Empty text: configurable message when no results
4. Header/footer: optional text above/below results
5. Show total count toggle
6. Items per page constrained by admin-configurable maximum (default 100)

**Tasks:**
- [ ] Add display configuration section to editor
- [ ] Implement format selector with preview of each format
- [ ] Add pager style selector
- [ ] Add empty text, header, footer fields
- [ ] Enforce max items_per_page from admin settings

---

### Story 23.6: Performance Guardrails

As a **site administrator**,
I want the system to prevent me from building expensive queries,
So that my site stays performant even with custom listings.

**Acceptance Criteria:**

1. Maximum pagination cap enforced in `GatherService::execute()` (not just UI)
2. Join depth validated in `ViewDefinition` deserialization (reject definitions exceeding limit)
3. Query timeout via `SET statement_timeout` before Gather execution (configurable, default 5s)
4. JSONB filter notice: UI checks for expression indexes, shows warning if missing
5. No leading wildcard: Contains operator shows performance note, StartsWith is default for text
6. Query cost indicator in preview panel (EXPLAIN output, parsed cost estimate)
7. Admin settings page for guardrail configuration

**Tasks:**
- [ ] Enforce pagination cap in GatherService::execute() (ignore client limit > max)
- [ ] Add join depth validation to ViewDefinition deserialization
- [ ] Add statement_timeout wrapper around Gather query execution
- [ ] Build expression index registry (query pg_catalog for expression indexes)
- [ ] Add JSONB index check to filter UI
- [ ] Add EXPLAIN integration for admin preview
- [ ] Create admin settings page for guardrail values

**Dev Notes:**
- statement_timeout should be SET LOCAL (transaction-scoped) not session-scoped
- Expression index detection: query `pg_indexes` for indexes on `(fields->>'field_name')`

---

### Story 23.7: Core Content Gather Views

As a **developer**,
I want hardcoded content listing queries replaced with default Gather definitions,
So that admins can customize all content listings through the UI.

**Acceptance Criteria:**

1. Default Gather views registered by core for:
   - `core.published_items`: Published items (replaces `Item::list_published()`)
   - `core.items_by_type`: Items by type (replaces `Item::list_by_type()`)
   - `core.items_by_author`: Items by author (replaces `Item::list_by_author()`)
   - `core.all_items`: Admin content list with exposed filters (replaces `Item::list_filtered()` and `Item::list_all()`)
2. Existing model methods become thin wrappers calling `GatherService::execute()`
3. Admin-customized versions take precedence over defaults
4. "Reset to default" action restores the core definition

**Tasks:**
- [ ] Define default Gather views for Item listings
- [ ] Register defaults during kernel initialization
- [ ] Refactor Item::list_published() to use Gather
- [ ] Refactor Item::list_by_type() to use Gather
- [ ] Refactor Item::list_by_author() to use Gather
- [ ] Replace Item::list_filtered() with exposed-filter Gather view
- [ ] Refactor Item::list_all() to use Gather
- [ ] Add "Reset to default" action for core views
- [ ] Verify existing routes return identical results

**Dev Notes:**
- The `Item::list_filtered()` method with manual string concatenation is the highest-priority conversion
- Comment queries that use recursive CTEs may need a custom Gather extension (Story 22.2)

---

### Story 23.8: Admin Entity Gather Views

As a **developer**,
I want admin entity listing queries converted to Gather definitions,
So that admin tables are customizable and consistent.

**Acceptance Criteria:**

1. Default Gather views registered for:
   - `core.user_list`: All users (replaces `User::list_paginated()`)
   - `core.comment_list`: All comments (replaces `Comment::list_all()`)
   - `core.comment_moderation`: Comments by status (replaces `Comment::list_by_status()`)
   - `core.category_terms`: Tags by vocabulary (replaces `Tag::list_by_category()`)
   - `core.url_aliases`: All aliases (replaces `UrlAlias::list_all()`)
   - `core.roles`: All roles (replaces `Role::list()`)
   - `core.content_types`: All types (replaces `ItemType::list()`)
2. Admin pages updated to use Gather results
3. Exposed filters work on admin pages (status filter on comments, type filter on content)

**Tasks:**
- [ ] Define default Gather views for User, Comment, Category, Alias, Role, ItemType
- [ ] Register defaults during kernel initialization
- [ ] Refactor admin user list to use Gather
- [ ] Refactor comment moderation to use Gather
- [ ] Refactor category admin to use Gather
- [ ] Refactor URL alias admin to use Gather
- [ ] Refactor role and content type admin lists to use Gather
- [ ] Add exposed filters to admin pages where appropriate

**Dev Notes:**
- Non-item tables (users, roles, url_alias) need Gather to support base_table != "item"
- This may require extending ViewDefinition to support arbitrary base tables
- Recursive CTE queries (Tag::get_descendants) may not convert directly — evaluate

---

### Story 23.9: Search Integration

As a **site administrator**,
I want full-text search available as a Gather filter type,
So that search results can be customized through the Gather UI.

**Acceptance Criteria:**

1. New filter operator: `FullTextSearch` using existing tsvector infrastructure
2. Filter value is the search query string
3. Results ranked by ts_rank (sort handler)
4. Integrates with existing SearchService::search() logic
5. Exposed as a filter in the Gather UI

**Tasks:**
- [ ] Add FullTextSearch filter operator to FilterOperator enum
- [ ] Implement tsvector filter in query builder
- [ ] Add ts_rank sort handler
- [ ] Register as Gather extension via Story 22.2 API
- [ ] Add to filter operator list in Gather UI

**Dev Notes:**
- Search already works (Epic 12). This story makes it available as a Gather filter type.
- The existing SearchService can remain for programmatic use; Gather provides the UI path.

---

### Story 23.10: Include/Sub-query Editor

As a **site administrator**,
I want to configure nested sub-queries (includes) through the UI,
So that I can build composite responses like stories-with-articles.

**Acceptance Criteria:**

1. Include section in editor: add sub-query with parent_field, child_field, singular toggle
2. Sub-query inherits the block-based editor (recursive UI)
3. Separate display config per include (items per page, format)
4. Preview shows nested results

**Tasks:**
- [ ] Add include section to query builder
- [ ] Implement recursive block editor for sub-query definition
- [ ] Add parent/child field pickers with relationship awareness
- [ ] Add singular/array toggle
- [ ] Show nested results in preview

**Dev Notes:**
- This is the most complex UI feature. Consider deferring to v2 if timeline is tight.
- The backend already fully supports includes (validated by Argus plugin).

---

## Epic 24: Block Editor -- Standard Plugin (Editor.js Integration)

**Governing Principle:** Core enables. Plugins implement.

The Kernel provides compound field type infrastructure (Epic 22.3), field widget registry, render pipeline, and file storage. The Block Editor plugin provides a visual editing widget using Editor.js that outputs structured JSON matching the compound field storage model.

**Goal:** Provide a block-based visual content editing experience that outputs clean structured JSON, is renderable to any target (HTML, AMP, RSS, mobile), and is replaceable because the data format is the contract, not the editor.

**Scope:**
- Block type registry with JSON schema validation
- Editor.js field widget (JavaScript, browser-side)
- Server-side block validation and sanitization
- Server-side block rendering (JSON → Render Tree → HTML)
- Image upload endpoint via FileStorage trait
- 8 standard block types (paragraph, heading, image, list, quote, code, delimiter, embed)

**Gate:**
- Content types with compound fields render Editor.js in the admin form
- All 8 standard block types work end-to-end (edit → validate → store → render)
- Image upload works with local file storage
- Public pages render block content as pure HTML (no JS dependency for readers)

**Why Editor.js:** Apache 2.0 licensed, block-based architecture maps directly to compound field storage, clean JSON output (no HTML blobs), vanilla JS (no framework dependency), standard tool interface for custom block types.

**What this is NOT:**
- Not a Layout Builder (content editing, not page structure)
- Not a Gutenberg clone (content blocks and layout blocks are separate concerns)
- Not CKEditor (no GPL, no HTML blob output, no maintenance burden)

**Dependencies:**
- Epic 22 Story 22.3 (compound field type) — storage layer
- Field widget registry (Epic 7 / Form API)
- FileStorage trait (existing)
- Asset pipeline for plugin JS/CSS (existing or minor extension)

**Cross-references:** Epic 22 (compound field type, standard content types), Design-Content-Model, Design-Render-Theme, Design-Plugin-System

---

### Story 24.1: Block Type Registry & Compound Field Integration

As a **plugin developer**,
I want to register block types with JSON schemas,
So that the compound field type can validate blocks on save.

**Acceptance Criteria:**

1. `BlockTypeDefinition` struct with type_name, label, JSON schema, allowed_formats, plugin
2. Block type registration via `tap_block_type_info` (or compound field extension mechanism)
3. Standard block types registered: paragraph, heading, image, list, quote, code, delimiter, embed
4. Compound field type's per-type validation dispatches to block type schemas
5. Unknown block types rejected on save

**Tasks:**
- [ ] Define BlockTypeDefinition struct
- [ ] Create block type registry (in-memory, populated at startup)
- [ ] Register 8 standard block types with JSON schemas
- [ ] Wire compound field validation to block type registry
- [ ] Add tap or extension point for plugin-provided block types
- [ ] Add tests for schema validation of each standard type

**Dev Notes:**
- Block storage format: `{ "type": "paragraph", "data": { "text": "..." }, "weight": 0 }`
- This maps directly to Editor.js output with `id` stripped and `weight` added

---

### Story 24.2: Server-Side Block Validation

As a **content administrator**,
I want blocks validated on save to prevent malformed content,
So that stored content is always well-formed and safe.

**Acceptance Criteria:**

1. Each block's `data` validated against its registered JSON schema
2. Text fields sanitized via `ammonia` (filtered_html rules from Kernel)
3. File references validated (image URLs point to real managed files)
4. Unknown block types rejected with clear error message
5. Field-level `allowed_types` enforced (only configured block types accepted)
6. Validation errors returned per-block with block index

**Tasks:**
- [ ] Implement per-block schema validation
- [ ] Integrate ammonia text sanitization for text-containing blocks
- [ ] Add file reference validation for image blocks
- [ ] Enforce field-level allowed_types list
- [ ] Return structured validation errors with block index
- [ ] Add tests for valid and invalid blocks of each type

**Dev Notes:**
- ammonia is already in use or planned for text format filtering
- File reference validation: query file_managed table for the referenced URL

---

### Story 24.3: Editor.js Field Widget

As a **content editor**,
I want a visual block editor when editing compound fields,
So that I can compose rich content with text, images, and embeds.

**Acceptance Criteria:**

1. Field widget loads Editor.js when compound field has `widget: "block_editor"` in settings
2. Tool configuration derived from field's `allowed_block_types`
3. Initial data loaded from Item's compound field value (mapped from Trovato → Editor.js format)
4. Save handler extracts Editor.js JSON, maps to compound field format, submits with form
5. Undo/redo via editorjs-undo package
6. Configurable inline tools (bold, italic, link)
7. Configurable placeholder text

**Tasks:**
- [ ] Create Editor.js field widget JavaScript module
- [ ] Implement Trovato → Editor.js data mapping (add `id`, remove `weight`)
- [ ] Implement Editor.js → Trovato data mapping (strip `id`, add `weight`)
- [ ] Configure Editor.js tools from field settings
- [ ] Integrate editorjs-undo for undo/redo
- [ ] Wire save handler to form submission
- [ ] Ship JS assets in plugin's static/ directory
- [ ] Add widget_settings schema to field configuration

**Dev Notes:**
- Editor.js is vanilla JS — no React/Vue dependency
- Widget settings in field_instance: `{ "widget": "block_editor", "allowed_block_types": [...], "inline_tools": [...], "placeholder": "..." }`
- Static assets loaded via Kernel's asset pipeline when widget is active

---

### Story 24.4: Image Upload Endpoint

As a **content editor**,
I want images uploaded inline while editing,
So that I can add images without leaving the editor.

**Acceptance Criteria:**

1. POST endpoint at `/api/block-editor/upload` accepting multipart/form-data
2. MIME type validation (image/jpeg, image/png, image/gif, image/webp)
3. File size limit (configurable, default 10MB)
4. Storage via FileStorage trait (local or S3)
5. Creates `file_managed` record for tracking
6. Returns Editor.js expected response: `{ "success": 1, "file": { "url": "..." } }`
7. CSRF protection on upload endpoint
8. Access control: requires create/edit permission on the item type

**Tasks:**
- [ ] Create upload route and handler
- [ ] Implement MIME type and size validation
- [ ] Integrate with FileStorage trait
- [ ] Create file_managed record on upload
- [ ] Return Editor.js response format
- [ ] Add CSRF token validation
- [ ] Add permission check (item type create/edit)
- [ ] Add configurable file size and dimension limits

**Dev Notes:**
- If Image Styles plugin is active later, it can hook into the upload to generate thumbnails
- Upload endpoint is stateful via CSRF token tied to the editing session

---

### Story 24.5: Server-Side Block Rendering

As a **site visitor**,
I want block content rendered as clean HTML,
So that pages load fast with no JavaScript dependency.

**Acceptance Criteria:**

1. Compound field render handler iterates blocks and builds Render Tree elements
2. Each block type has a Tera template: `block--paragraph.html`, `block--heading.html`, etc.
3. Templates are theme-overridable (standard template suggestion pattern)
4. No Editor.js JavaScript on public-facing pages
5. Render output is semantic HTML (proper heading levels, figure/figcaption for images, etc.)
6. Code blocks render with syntax highlighting CSS classes

**Tasks:**
- [ ] Implement block-to-RenderTree mapper for each standard block type
- [ ] Create Tera templates for all 8 block types
- [ ] Wire into compound field display formatter
- [ ] Add template suggestion support (block--{type}.html, block--{type}--{field}.html)
- [ ] Verify semantic HTML output for accessibility
- [ ] Add render tests for each block type

**Dev Notes:**
- The Render Tree is assembled server-side; templates produce final HTML
- This is the critical architectural boundary: editor is a backend tool, public site is pure HTML

---

### Story 24.6: Code Block Syntax Highlighting

As a **content editor**,
I want code blocks rendered with syntax highlighting,
So that code snippets are readable on the published page.

**Acceptance Criteria:**

1. Code block `data` includes `language` field (optional, auto-detect if missing)
2. Server-side highlighting via `syntect` crate
3. Output: `<pre><code>` with `<span>` elements for syntax tokens
4. CSS classes for theme styling (light/dark mode support)
5. Language selector in Editor.js code block tool

**Tasks:**
- [ ] Add syntect dependency
- [ ] Implement syntax highlighting in code block renderer
- [ ] Support explicit language field and auto-detection fallback
- [ ] Generate CSS class output compatible with standard highlight themes
- [ ] Add language selector to Editor.js code tool configuration

---

### Story 24.7: Embed Block Whitelist & Rendering

As a **content editor**,
I want to embed YouTube, Vimeo, and other media inline,
So that I can include rich media in content without raw HTML.

**Acceptance Criteria:**

1. Configurable service whitelist (YouTube, Vimeo, etc.)
2. URL pattern matching validates embed sources
3. Render output: responsive iframe with CSP-safe headers
4. oEmbed integration if feasible; otherwise direct iframe generation
5. Caption support below embed

**Tasks:**
- [ ] Define embed service whitelist configuration
- [ ] Implement URL pattern matching for whitelisted services
- [ ] Generate responsive iframe HTML in render template
- [ ] Add CSP header considerations for embed sources
- [ ] Evaluate oEmbed integration (may defer to later)
- [ ] Add caption rendering below embed

---

### Story 24.8: Read-Only Mode & Content Preview

As a **content editor**,
I want to preview block content before saving,
So that I can verify how the content will appear on the live site.

**Acceptance Criteria:**

1. Editor.js readOnly mode toggle in the editing form
2. Preview renders using the same server-side templates as the public page
3. "Preview" button triggers server-side render of current (unsaved) block data
4. Preview opens in modal or side panel (not a new page)

**Tasks:**
- [ ] Add preview button to block editor widget
- [ ] Create preview endpoint (POST, accepts block JSON, returns rendered HTML)
- [ ] Implement readOnly toggle for Editor.js
- [ ] Render preview using same templates as public display
- [ ] Display preview in modal or side panel

---

### Story 24.9: Block Editor Documentation

As a **plugin developer**,
I want documentation on creating custom block types,
So that I can extend the editor for my domain-specific needs.

**Acceptance Criteria:**

1. Plugin development guide section: "Creating Custom Block Types"
2. Covers: BlockTypeDefinition schema, Editor.js tool implementation, Tera template, registration
3. End-user editing guide: how to use the block editor
4. Configuration reference: widget settings, allowed types, embed whitelist

**Tasks:**
- [ ] Write custom block type development guide
- [ ] Write end-user editing guide
- [ ] Write configuration reference
- [ ] Add example: creating a "callout" custom block type

---

## Epic 25: Coding Standards & Enforcement

**Goal:** Define, document, and enforce consistent coding standards across the entire Trovato codebase. Automate everything that can be automated so CI catches any deviation before it merges. A contributor can read one document and know exactly how to write Trovato code.

**Scope:**
- Phase 1: Define standards (rustfmt, Clippy lints, naming conventions, plugin conventions, error handling, documentation)
- Phase 2: Write the standards document (`docs/coding-standards.md`)
- Phase 3: Enforce via CI (GitHub Actions) and pre-commit hooks
- Phase 4: Retrofit all existing code to 100% compliance

**Gate:**
- `.rustfmt.toml` and `clippy.toml` configured with project-specific settings
- `docs/coding-standards.md` published with examples and rationale
- CI pipeline runs `cargo fmt --check`, `cargo clippy` (warnings-as-errors), `cargo test`, and `cargo doc --no-deps` on every PR
- Zero rustfmt violations, zero Clippy warnings, complete rustdoc on public API
- Trovato terminology used consistently throughout (no Drupal terminology in code/comments/docs)

**Design Philosophy:** Standards are only as good as their enforcement. Every rule that can be automated must be automated. Rules that cannot be automated get a manual review checklist. Pre-commit hooks ensure contributors never fail CI on formatting.

**Cross-references:** Epic 19 (CI & Test Infrastructure — extends the existing pipeline)

---

### Story 25.1: Configure rustfmt

As a **contributor**,
I want a project-wide `.rustfmt.toml` configuration,
So that all code is formatted consistently without manual decisions.

**Acceptance Criteria:**

1. `.rustfmt.toml` exists at the workspace root with project-specific settings
2. Settings include: max width, import grouping (`group_imports = "StdExternalCrate"`), trailing commas, brace style
3. `cargo fmt --check` passes on the entire codebase with the new config
4. Decision rationale documented as comments in `.rustfmt.toml`

**Tasks:**
- [ ] Survey current formatting patterns in the codebase
- [ ] Decide max line width (100 vs 120)
- [ ] Configure `group_imports`, `imports_granularity`, `reorder_imports`
- [ ] Configure trailing comma, brace style, match arm handling
- [ ] Run `cargo fmt` and fix any resulting changes
- [ ] Verify `cargo fmt --check` passes clean

---

### Story 25.2: Configure Clippy Lints

As a **contributor**,
I want a project-wide Clippy configuration that catches common mistakes,
So that code quality is enforced automatically and consistently.

**Acceptance Criteria:**

1. `clippy.toml` exists at the workspace root (if needed for config values)
2. Crate-level lint attributes set in `lib.rs` / `main.rs` files
3. `clippy::unwrap_used` denied in non-test code
4. `clippy::todo` and `clippy::unimplemented` denied
5. `clippy::panic` denied in production code paths
6. `cargo clippy --all -- -D warnings` passes clean
7. Any `#[allow(...)]` annotations include a comment explaining why

**Tasks:**
- [ ] Decide baseline lint level (`clippy::all` + `clippy::pedantic` vs selective)
- [ ] Identify lints that conflict with project patterns (document and allow)
- [ ] Add crate-level `#![warn(...)]` / `#![deny(...)]` attributes
- [ ] Fix all existing Clippy warnings
- [ ] Verify clean `cargo clippy --all -- -D warnings`

---

### Story 25.3: Define Naming Conventions

As a **contributor**,
I want documented naming conventions for all Trovato code,
So that code reads consistently and uses Trovato terminology throughout.

**Acceptance Criteria:**

1. Trovato terminology documented: Tap (not Hook), Item (not Node), Plugin (not Module), Gather (not Views), Tile (not Block/Region), Category (not Taxonomy/Vocabulary)
2. Function naming patterns documented for Taps (`tap_item_view`, `tap_form_alter`, etc.)
3. Module organization rules documented (when to split into separate files)
4. Test naming convention documented (`test_<function>_<scenario>` or chosen pattern)
5. All existing code audited for Drupal terminology in variable names, comments, and docs

**Tasks:**
- [ ] Define and document Trovato terminology mapping (Drupal 6 → Trovato)
- [ ] Define function naming patterns for each subsystem
- [ ] Define module organization rules
- [ ] Define test naming convention
- [ ] Grep codebase for Drupal terminology and fix all occurrences
- [ ] Grep comments and docs for Drupal terminology and fix

---

### Story 25.4: Define Plugin Conventions

As a **plugin developer**,
I want documented conventions for writing Trovato plugins,
So that all plugins follow the same patterns and are easy to understand.

**Acceptance Criteria:**

1. `.info.toml` structure and required fields documented with examples
2. Tap registration patterns documented
3. Plugin-side DB API usage patterns documented
4. Plugin configuration exposure patterns documented
5. Plugin documentation requirements documented (README per plugin, rustdoc on public types)
6. All existing plugins audited against the conventions

**Tasks:**
- [ ] Document `.info.toml` format with all fields and their meaning
- [ ] Document tap registration patterns (naming, weights, when to use which tap)
- [ ] Document plugin DB access patterns and limitations
- [ ] Document plugin configuration patterns
- [ ] Document plugin README and rustdoc requirements
- [ ] Audit all existing plugins for compliance

---

### Story 25.5: Define Error Handling Standards

As a **contributor**,
I want documented error handling conventions,
So that errors are handled consistently with appropriate context and logging.

**Acceptance Criteria:**

1. Error type strategy documented (custom per-subsystem vs unified enum)
2. `Result` vs `Option` usage guidelines documented
3. Error context strategy documented (`thiserror` patterns)
4. Logging level guidelines documented (which errors at which levels)
5. `render_error` (400) vs `render_server_error` (500) usage documented
6. All existing error handling audited for consistency

**Tasks:**
- [ ] Evaluate and decide on error type strategy
- [ ] Document Result vs Option usage guidelines
- [ ] Document error context and chaining patterns
- [ ] Document logging level guidelines (error, warn, info, debug, trace)
- [ ] Document HTTP error response guidelines (400 vs 500)
- [ ] Audit existing code for error handling consistency

---

### Story 25.6: Define Documentation Standards

As a **contributor**,
I want documented requirements for code documentation,
So that public APIs are consistently documented and code comments add value.

**Acceptance Criteria:**

1. Rustdoc requirements documented: all public types, all public functions, module-level docs
2. Code comment standards documented: when to comment (why, not what), when not to
3. `cargo doc --no-deps` passes with zero warnings
4. All public types and functions have rustdoc

**Tasks:**
- [ ] Document rustdoc requirements
- [ ] Document code comment standards with examples
- [ ] Run `cargo doc --no-deps` and fix all warnings
- [ ] Add missing rustdoc to all public types and functions
- [ ] Verify zero doc warnings

---

### Story 25.7: Write Coding Standards Document

As a **contributor**,
I want a single reference document covering all coding standards,
So that I can look up any convention quickly with examples.

**Acceptance Criteria:**

1. `docs/coding-standards.md` exists with all standards from stories 25.1-25.6
2. Each rule includes examples of correct and incorrect code
3. Decision rationale included (why we chose this, not just what)
4. Quick Start section at the top with the 5 most important rules
5. Links to relevant design docs for architectural decisions

**Tasks:**
- [ ] Write Quick Start section (top 5 rules)
- [ ] Write rustfmt section with examples
- [ ] Write Clippy section with lint rationale
- [ ] Write naming conventions section with Trovato terminology
- [ ] Write plugin conventions section
- [ ] Write error handling section
- [ ] Write documentation standards section
- [ ] Write manual review checklist (things CI cannot catch)
- [ ] Cross-reference from CLAUDE.md and CONTRIBUTING.md

---

### Story 25.8: Enforce in CI

As a **maintainer**,
I want CI to reject any PR that violates coding standards,
So that standards are enforced automatically without manual review burden.

**Acceptance Criteria:**

1. GitHub Actions workflow runs `cargo fmt --check` (zero tolerance)
2. GitHub Actions workflow runs `cargo clippy --all -- -D warnings`
3. GitHub Actions workflow runs `cargo test --all`
4. GitHub Actions workflow runs `cargo doc --no-deps` with no warnings
5. Custom lint checks for Drupal terminology in code and comments
6. All checks must pass before merge

**Tasks:**
- [ ] Update `.github/workflows/ci.yml` to add all checks
- [ ] Add `cargo fmt --check` step
- [ ] Add `cargo clippy --all -- -D warnings` step
- [ ] Add `cargo doc --no-deps` step
- [ ] Add custom terminology grep check
- [ ] Verify all checks pass on current codebase

---

### Story 25.9: Pre-commit Hooks

As a **contributor**,
I want a pre-commit hook that auto-formats my code,
So that I never fail CI on formatting issues.

**Acceptance Criteria:**

1. Pre-commit hook configuration provided (`.pre-commit-config.yaml` or shell script)
2. Hook runs `cargo fmt` automatically on staged Rust files
3. Hook optionally runs `cargo clippy` (configurable, off by default for speed)
4. Setup instructions documented in `docs/coding-standards.md`

**Tasks:**
- [ ] Create pre-commit hook script or config
- [ ] Test hook with staged Rust file changes
- [ ] Document setup instructions
- [ ] Add note about hook in Quick Start section of coding standards

---

### Story 25.10: Retrofit Existing Code

As a **maintainer**,
I want all existing code to comply with the new standards,
So that the codebase is 100% consistent from day one.

**Acceptance Criteria:**

1. `cargo fmt` produces zero changes
2. `cargo clippy --all -- -D warnings` passes clean
3. All public types and functions have rustdoc
4. No Drupal terminology in code, comments, or docs
5. All `#[allow(...)]` annotations have explanatory comments
6. All plugins comply with plugin conventions

**Tasks:**
- [ ] Run `cargo fmt` on entire codebase
- [ ] Fix all Clippy warnings
- [ ] Add missing rustdoc to all public API surface
- [ ] Grep and fix Drupal terminology throughout
- [ ] Review all `#[allow(...)]` annotations
- [ ] Audit all plugins against conventions
- [ ] Run full CI pipeline and verify green

---

### Story 25.11: Ongoing Maintenance Plan

As a **maintainer**,
I want a documented maintenance plan for coding standards,
So that standards stay current and enforced over time.

**Acceptance Criteria:**

1. CLAUDE.md updated with enforceable coding standards section that all AI-assisted development must follow — this is the primary enforcement mechanism for Claude Code sessions
2. CLAUDE.md rules cover: rustfmt compliance, Clippy lint compliance, Trovato terminology (never Drupal terms), `render_error` vs `render_server_error` usage, `require_admin` vs `require_login` usage, shared helper usage (html_escape, SESSION_USER_ID, is_valid_machine_name, require_csrf), plugin convention adherence, rustdoc on all new public API, new admin routes go in domain-specific `admin_*.rs` modules (not `admin.rs`), new admin templates use macros from `templates/admin/macros/`
3. CLAUDE.md includes a "before committing" checklist: `cargo fmt`, `cargo clippy --all -- -D warnings`, `cargo test --all`, `cargo doc --no-deps`
4. Standards doc versioning process documented (update in same PR as convention changes)
5. Terminology enforcement periodic grep documented
6. Annual review process documented

**Tasks:**
- [ ] Write CLAUDE.md coding standards section with all enforceable rules
- [ ] Write CLAUDE.md "before committing" checklist
- [ ] Add CLAUDE.md cross-reference to `docs/coding-standards.md` for full rationale
- [ ] Document standards doc versioning process
- [ ] Document periodic terminology enforcement procedure
- [ ] Document annual review process
- [ ] Add onboarding note: "Read docs/coding-standards.md first"

---

## Epic 26: Kernel Minimality Audit

**Goal:** Audit every Kernel subsystem to ensure no feature logic has crept in. The core kernel enables; plugins implement. Extract anything that could be a plugin, then establish ongoing maintenance processes to prevent kernel bloat from recurring.

**Scope:**
- Phase 1: Audit all kernel components and classify as infrastructure (Keep) or feature (Extract)
- Phase 2: Plan and execute extractions for feature services
- Phase 3: Resolve kernel interface modifications needed to support extractions
- Phase 4: Define plugin SDK versioning scheme for API stability
- Phase 5: Establish ongoing maintenance processes (PR gates, line-count tracking, quarterly reviews)

**Gate:**
- `docs/kernel-minimality-audit.md` published with full classification of every kernel subsystem
- All viable extractions executed (scheduled publishing, translation, webhooks)
- CLAUDE.md updated with kernel minimality rules
- PR template with kernel justification checklist
- Kernel line-count tracking automated
- Quarterly boundary review process documented

**Design Philosophy:** A bloated kernel is harder to maintain, harder to reason about, and harder to secure. Every kernel addition must justify why it can't be a plugin. Automated tracking catches drift before it becomes debt.

**Cross-references:** Epic 22 (Modern CMS Features — plugin extractions), Epic 25 (Coding Standards — CLAUDE.md enforcement)

---

### Story 26.1: Audit and Classify All Kernel Subsystems

As a **maintainer**,
I want every kernel subsystem audited and classified as infrastructure or feature,
So that we have a clear baseline of what belongs in the kernel and what doesn't.

**Acceptance Criteria:**

1. Every directory and file under `crates/kernel/src/` is examined
2. Each subsystem classified as Infrastructure (Keep) or Feature (Extract candidate)
3. Classification includes reasoning for each decision
4. Summary table with all subsystems, classifications, and verdicts
5. Results documented in `docs/kernel-minimality-audit.md`

**Tasks:**
- [x] Enumerate all kernel subsystems (plugin, tap, content, gather, theme, routes, models, host, middleware, form, config_storage, cache, batch, file, search, stage, cron, menu, permissions, metrics, session, auth, db, state, error)
- [x] Classify each as infrastructure or feature with reasoning
- [x] Document in `docs/kernel-minimality-audit.md` Section 1
- [x] Create summary table (Section 5)

---

### Story 26.2: Identify Extraction Candidates with Plans

As a **maintainer**,
I want extraction candidates identified with concrete plans,
So that we know exactly what to move and how.

**Acceptance Criteria:**

1. Each feature service evaluated for extraction feasibility
2. Blocking dependencies identified (e.g., missing host functions, WASM limitations)
3. Extraction plan documented for each viable candidate
4. Non-viable extractions documented with reasoning (WASM constraints, hot-path, etc.)

**Tasks:**
- [x] Evaluate redirect service (kept — hot-path middleware)
- [x] Evaluate image style service (kept — WASM can't do image processing/filesystem I/O)
- [x] Evaluate scheduled publishing (extracted via tap_cron)
- [x] Evaluate translation service (extracted — dead kernel code removed)
- [x] Evaluate webhook service (extracted — dead kernel code removed)
- [x] Evaluate email service (kept — WASM can't do SMTP)
- [x] Evaluate audit service (kept — compliance infrastructure)
- [x] Evaluate content lock service (kept — data integrity, already gated)
- [x] Document in `docs/kernel-minimality-audit.md` Section 2

---

### Story 26.3: Execute Service Extractions

As a **maintainer**,
I want feature services extracted from the kernel to plugins,
So that the kernel contains only infrastructure.

**Acceptance Criteria:**

1. Scheduled publishing service removed from kernel, implemented as plugin `tap_cron` handler
2. Translation service removed from kernel (service, PO parser, translated config)
3. Webhook service removed from kernel (service, cron task, HKDF key block)
4. `AppStateInner` no longer carries extracted service fields or accessors
5. `set_plugin_services()` signature simplified
6. Zero kernel callers remain for extracted services

**Tasks:**
- [x] Extract scheduled publishing to plugin tap_cron
- [x] Extract translation service (delete kernel service, PO parser, translated config)
- [x] Extract webhook service (delete kernel service, cron task, state fields)
- [x] Verify zero remaining kernel callers for all extracted services
- [x] Clean up `AppState::new()` (remove instantiation blocks, HKDF key derivation)

---

### Story 26.4: Resolve Kernel Interface Modifications

As a **maintainer**,
I want kernel interfaces updated to support the new plugin boundary,
So that plugins can provide functionality previously hardcoded in the kernel.

**Acceptance Criteria:**

1. CategoryService coupling with GatherService resolved (decision: keep in kernel)
2. `tap_cron` dispatch activated in cron runner
3. Redirect middleware made conditional on plugin enablement
4. File path host function evaluated (no longer needed)
5. Email abstraction evaluated (no longer needed)

**Tasks:**
- [x] Resolve CategoryService/GatherService coupling (keep CategoryService in kernel)
- [x] Activate tap_cron dispatch with timeout and per-plugin failure logging
- [x] Make RedirectCache conditional (`Option<Arc<>>`, early-return when None)
- [x] Close file path host function (not needed — image styles kept in kernel)
- [x] Close email abstraction (not needed — WASM can't do SMTP)
- [x] Document in `docs/kernel-minimality-audit.md` Section 3

---

### Story 26.5: Define Plugin SDK Versioning Scheme

As a **plugin developer**,
I want a clear versioning contract for the plugin SDK,
So that I know when my plugin will break and when it won't.

**Acceptance Criteria:**

1. Semver stability tiers defined (Stable/MAJOR, Semi-stable/MINOR, Internal)
2. Each tier lists exactly what's covered (host functions, types, taps, etc.)
3. Compatibility matrix with examples
4. Plugin `.info.toml` declares `sdk_version` for load-time compatibility check
5. Documented in `docs/kernel-minimality-audit.md` Section 4

**Tasks:**
- [x] Define Stable tier (host function signatures, error codes, SDK types, tap signatures, proc macros)
- [x] Define Semi-stable tier (new taps, new host functions, new types, additive fields)
- [x] Define Internal tier (service implementations, DB schema, routes, middleware, cron)
- [x] Create compatibility matrix
- [x] Document in `docs/kernel-minimality-audit.md` Section 4

---

### Story 26.6: Update CLAUDE.md with Kernel Minimality Rules

As a **maintainer**,
I want CLAUDE.md to enforce kernel minimality during AI-assisted development,
So that new code follows the kernel boundary rules automatically.

**Acceptance Criteria:**

1. Governing principle stated: "The core kernel enables. Plugins implement."
2. Decision framework for new services: "Does any other kernel subsystem depend on this?"
3. Feature service placement rule: "If only callers are gated routes or cron tasks, it's a feature"
4. `Option<Arc<>>` pattern documented for plugin-optional services
5. `plugin_gate!` macro usage documented for plugin-specific routes
6. `tap_cron` preference documented for plugin cron tasks
7. Infrastructure services listed explicitly

**Tasks:**
- [x] Add "Kernel Minimality Rules" section to CLAUDE.md
- [x] Document governing principle
- [x] Document decision framework for new services
- [x] Document feature service placement rule
- [x] Document `Option<Arc<>>` pattern, `plugin_gate!` macro, `tap_cron` preference
- [x] List infrastructure services that stay in kernel

---

### Story 26.7: Create PR Template with Kernel Justification Checklist

As a **maintainer**,
I want every PR that touches kernel code to include a kernel justification,
So that feature logic doesn't creep into the kernel during code review.

**Acceptance Criteria:**

1. `.github/PULL_REQUEST_TEMPLATE.md` exists with standard PR sections
2. Template includes a "Kernel Boundary" checklist section that activates when kernel files are modified
3. Checklist asks: "Why can't this be a plugin?", "Does this contain CMS-specific business logic?", "Could a plugin provide this through an existing Tap or trait?"
4. Template includes sections for: summary, changes, test plan, kernel boundary justification

**Tasks:**
- [ ] Create `.github/PULL_REQUEST_TEMPLATE.md`
- [ ] Add summary and changes sections
- [ ] Add test plan section
- [ ] Add kernel boundary checklist (conditional guidance for kernel PRs)
- [ ] Verify template renders correctly on GitHub

---

### Story 26.8: Create Kernel Line-Count Tracking Script

As a **maintainer**,
I want automated tracking of kernel lines of code,
So that I can detect kernel bloat trends before they become problems.

**Acceptance Criteria:**

1. Script counts lines of Rust code in `crates/kernel/src/` (excluding tests, blanks, comments)
2. Script counts lines in plugin SDK (`crates/plugin-sdk/src/`)
3. Script outputs kernel LOC, SDK LOC, and ratio
4. Baseline measurement recorded in `docs/kernel-minimality-audit.md`
5. Script can be run manually or in CI to compare against baseline

**Tasks:**
- [ ] Create `scripts/kernel-loc.sh` (or similar) using `tokei` or `scc` or plain `wc`
- [ ] Count kernel LOC, SDK LOC, compute ratio
- [ ] Record baseline measurement in audit doc
- [ ] Add instructions for running in `docs/kernel-minimality-audit.md`
- [ ] Optionally add as CI informational step (non-blocking)

---

### Story 26.9: Document Quarterly Boundary Review Process

As a **maintainer**,
I want a documented process for periodic kernel boundary reviews,
So that kernel minimality is maintained over time and extraction debt doesn't accumulate.

**Acceptance Criteria:**

1. Quarterly review process documented in `docs/kernel-minimality-audit.md`
2. Process includes: re-run audit checklist against new kernel code, compare LOC trends, review plugin extraction backlog
3. Plugin extraction backlog section added to audit doc for tracking candidates
4. New subsystem rule documented: any proposed kernel subsystem requires justification for why it can't be a plugin or trait
5. Review cadence tied to major releases or quarterly, whichever comes first

**Tasks:**
- [ ] Add "Ongoing Maintenance" section to `docs/kernel-minimality-audit.md`
- [ ] Document quarterly review process steps
- [ ] Document new subsystem justification rule
- [ ] Add plugin extraction backlog section (running list of "things in kernel that maybe shouldn't be")
- [ ] Cross-reference from CLAUDE.md

---

