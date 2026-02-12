# Trovato: Development Phases

Total estimate: 42-58 weeks, honestly 50-65 weeks with plugins, tests, docs.

## Phase 0: Critical Spike (2 weeks)

Prove the WASM architecture works before writing any Kernel code.

Three objectives:
1. Benchmark handle-based vs. full-serialization data access (500 calls each)
2. Benchmark Store pooling under concurrency (100 parallel requests)
3. Validate async host functions (WASM -> host -> SQLx -> return, no deadlocks)

**Gate:** Written recommendation on data access mode (handle vs. serialize vs. hybrid) with benchmark numbers. If handle-based >5x faster, it becomes the default WIT pattern.

**Status:** Not started

## Phase 1: Skeleton (4 weeks)

Axum server, Postgres connection, Redis sessions, user login/logout, stage tracking, Gander profiling middleware, Queue API design.

**Gate:** User can log in and out, session persists across requests.

**Status:** Not started

## Phase 2: Plugin Kernel + SDK (8 weeks, extended from 6)

SDK-first approach:
- Weeks 1-2: Write three reference plugin source files (blog, page, categories) as specification
- Weeks 3-5: Build plugin-sdk crate (proc macros, types, host function bindings, handle-based wrappers)
- Weeks 6-8: Build Kernel-side plugin loader, tap dispatcher, RequestState

**Gate:** Blog plugin registers route, receives request, reads fields via handle-based host functions, returns JSON Render Element. Source code looks clean (no raw pointers or JSON strings).

**Status:** Not started

## Phase 3: Content, Fields, & Stages (8 weeks)

Content types, JSONB field storage, item CRUD with taps, stage support, text format filters, revisions, dynamic search field config, auto-generated admin forms for testing.

**Gate:** Create content type with 5 fields, CRUD an item, revert revision in different stage.

**Status:** Not started

## Phase 4: Gather Query Engine & Categories (8 weeks)

SeaQuery-based Gather, categories vocabularies/terms/hierarchy, recursive CTEs for hierarchical queries, breadcrumb generation, inter-plugin communication (invoke_plugin).

**Gate:** "Recent Articles" Gather query with category filter + pager renders correctly.

**Status:** Not started

## Phase 5: Form API, Theming, & Admin UI (8 weeks)

Declarative form definitions, validation/submission pipeline, tap_form_alter, CSRF protection, multi-step forms, AJAX support, Tera template suggestions, theme engine.

**Gate:** Admin creates content type via UI, forms support AJAX "Add another item."

**Status:** Not started

## Phase 6: Files, Search, Cron, & Hardening (8 weeks)

File uploads (local/S3), full-text search (tsvector), cron with distributed locking, queue workers, rate limiting (Tower middleware), metrics endpoint, load testing.

**Gate:** All subsystems functional under load.

**Status:** Not started

## Not in Estimate

- WASM tooling debugging: 2-4 weeks
- Plugin SDK + 3 reference plugins: 3-4 weeks
- Comprehensive tests: 3-4 weeks
- Plugin author documentation: 1-2 weeks
