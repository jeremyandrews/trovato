# Trovato

A content management system built in Rust, reimagining Drupal 6's mental model with modern foundations: WASM-sandboxed plugins, JSONB field storage, a JSON Render Tree for security, and Stages from day one.

**Codebase:** https://github.com/jeremyandrews/trovato
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

## Inclusivity-First Design Principles

Baked into the kernel from day one — not bolted on after:

- **Accessibility by default** — semantic HTML, skip links, ARIA attributes, required alt text on images, form error association
- **i18n from day one** — language column on all content, RTL direction support, locale-aware date formatting, configurable language negotiation
- **Security by design** — Content-Security-Policy headers, field-level access control, crypto host functions for plugins, secret config references
- **Privacy by default** — consent tracking fields, personal data markers on fields, user data export, no external resource loading
- **Multi-tenancy as infrastructure** — tenant_id on all content tables, tenant resolution middleware, invisible for single-tenant sites (like the language column)
- **API-first** — route metadata annotations, API versioning headers, content-negotiated JSON responses
- **AI as a governed resource** — metadata audit trail, request interception tap, per-feature configuration toggles

## Architecture

See [Architecture](Architecture.md) for component breakdown.

## Phases

See [Phases](Phases.md) for the development roadmap.

## Design Documents

**Overview & architecture:**
- [Design-Overview](Design-Overview.md) — What, why, architecture diagram, and index to detail docs

**Detailed design (split from v2.1):**
- [Web Layer & Sessions](Design-Web-Layer.md) — HTTP routing, middleware, sessions, authentication
- [Plugin & Tap System](Design-Plugin-System.md) — WASM plugin loading, tap dispatch, SDK
- [Render Tree & Forms](Design-Render-Theme.md) — JSON render pipeline, Form API
- [Content Model](Design-Content-Model.md) — Items/CCK, stages, revisions, categories
- [Gather Query Engine](Design-Query-Engine.md) — Dynamic query builder
- [Infrastructure](Design-Infrastructure.md) — Files, cron, search, caching, error handling
- [Project Meta](Design-Project-Meta.md) — Benchmarks, migration, structure, deps, roadmap, decisions, gaps

**Plugin SDK:**
- [Plugin SDK Spec](Design-Plugin-SDK.md) — Types, macros, host functions, WIT interface, mutation model, examples

**Reference:**
- [Terminology](Terminology.md) — Drupal → Trovato naming map

## Open Questions / Gaps (from Section 22)

See [Section 22](Design-Project-Meta.md) for full details and decision criteria.

**Resolved:**
- Serialization cost → Phase 0 benchmarks dual-mode access
- Plugin-to-Plugin communication → Phase 4 via `invoke_plugin` host function
- Plugin SDK → [SDK Spec](Design-Plugin-SDK.md) written
- Rate limiting → Phase 6 via Tower middleware

**Open (assigned):**
- Item-level access control → Phase 3: design `tap_item_access` with grant/deny aggregation
- Gather exposed filters → Phase 4 (query param parsing) + Phase 5 (form-rendered filters)
- Testing strategy → Phase 1 (infrastructure) + ongoing per phase

**Deferred (post-MVP, risk accepted):**
- Stage merge conflicts: "Last Publish Wins" for v1; add conflict warning on publish in Phase 3
- WASI Component Model migration: budget 2-4 weeks in year two
