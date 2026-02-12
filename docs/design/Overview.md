# Trovato

A content management system built in Rust, reimagining Drupal 6's mental model with modern foundations: WASM-sandboxed plugins, JSONB field storage, a JSON Render Tree for security, and Stages from day one.

**Domain:** trovato.rs
**Codebase:** TBD
**Implementation method:** BMAD
**Design docs:** v2.1 (Feb 2026)

## What It Is

Drupal 6's strengths (everything is an item, bolt-on fields via CCK, Gather for querying, taps for extensibility) rebuilt in Rust with:

- **Axum + Tokio** for async HTTP
- **PostgreSQL** with hybrid relational/JSONB schema (no more N+1 JOINs for fields)
- **WebAssembly plugins** loaded at runtime, sandboxed per-request (pooled instantiation)
- **JSON Render Tree** (plugins return structured JSON, never raw HTML; Kernel sanitizes and renders via Tera)
- **Stages** for content staging baked into the schema from the start
- **Redis** for sessions, cache, distributed locks
- **SeaQuery** for type-safe Gather query building
- **Gander-style observability** middleware

## Key Design Decisions

- Plugins are untrusted (WASM boundary enforces this)
- No persistent state in the binary (all state in Postgres/Redis; horizontal scaling without session affinity)
- Handle-based data access across WASM boundary (avoids serialization bottleneck)
- SDK-first plugin design (write the code you want devs to write, then build the host)
- Structured DB API in WIT prevents SQL injection from plugins

## Architecture

See [[Projects/Trovato/Architecture]] for component breakdown.

## Phases

See [[Projects/Trovato/Phases]] for the development roadmap.

## Design Documents

**Overview & architecture:**
- [[Projects/Trovato/Design-Overview]] — What, why, architecture diagram, and index to detail docs

**Detailed design (split from v2.1):**
- [[Projects/Trovato/Design-Web-Layer|Web Layer & Sessions]] — HTTP routing, middleware, sessions, authentication
- [[Projects/Trovato/Design-Plugin-System|Plugin & Tap System]] — WASM plugin loading, tap dispatch, SDK
- [[Projects/Trovato/Design-Render-Theme|Render Tree & Forms]] — JSON render pipeline, Form API
- [[Projects/Trovato/Design-Content-Model|Content Model]] — Items/CCK, stages, revisions, categories
- [[Projects/Trovato/Design-Query-Engine|Gather Query Engine]] — Dynamic query builder
- [[Projects/Trovato/Design-Infrastructure|Infrastructure]] — Files, cron, search, caching, error handling
- [[Projects/Trovato/Design-Project-Meta|Project Meta]] — Benchmarks, migration, structure, deps, roadmap, decisions, gaps

**Plugin SDK (BMAD critical path):**
- [[Projects/Trovato/Design-Plugin-SDK|Plugin SDK Spec]] — Types, macros, host functions, WIT interface, mutation model, examples

**Reference:**
- [[Projects/Trovato/Terminology]] — Drupal → Trovato naming map
- [[Projects/Trovato/Design-v2.1]] — Original monolithic design (kept for reference)

## Open Questions / Gaps (from Section 22)

See [[Projects/Trovato/Design-Project-Meta#22. Remaining Gaps and Honest Assessments|Section 22]] for full details and decision criteria.

**Resolved:**
- Serialization cost → Phase 0 benchmarks dual-mode access
- Plugin-to-Plugin communication → Phase 4 via `invoke_plugin` host function
- Plugin SDK → [[Projects/Trovato/Design-Plugin-SDK|SDK Spec]] written
- Rate limiting → Phase 6 via Tower middleware

**Open (assigned):**
- Item-level access control → Phase 3: design `tap_item_access` with grant/deny aggregation
- Gather exposed filters → Phase 4 (query param parsing) + Phase 5 (form-rendered filters)
- Testing strategy → Phase 1 (infrastructure) + ongoing per phase

**Deferred (post-MVP, risk accepted):**
- Stage merge conflicts: "Last Publish Wins" for v1; add conflict warning on publish in Phase 3
- WASI Component Model migration: budget 2-4 weeks in year two
