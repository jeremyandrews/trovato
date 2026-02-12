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

