# Alignment Roadmap

Plan for addressing Drupal 6 alignment gaps, mapping each to the appropriate epic or explicitly deferring.

---

## Gaps Addressed in Completed Epics

These gaps have already been resolved:

| Gap | Epic | Status |
|---|---|---|
| Path Alias system | Epic 15, Story 15.5 | Done |
| Localization (UI string translation) | Epic 22, Story 22.16 (Locale Plugin) | Done |
| Content translation | Epic 22, Story 22.15 (Content Translation Plugin) | Done |
| Config translation | Epic 22, Story 22.17 (Config Translation Plugin) | Done |
| OAuth2 API authentication | Epic 22, Story 22.8 | Done |
| Webhooks | Epic 22, Story 22.11 | Done |
| Image styles | Epic 22, Story 22.9 | Done |
| Scheduled publishing | Epic 22, Story 22.10 | Done |
| Content locking | Epic 22, Story 22.13 | Done |
| Audit logging | Epic 22, Story 22.12 | Done |
| Redirects | Epic 22, Story 22.7 | Done |
| Config export/import | Epic 22, Story 22.4 | Done |
| Categories/Comments as plugins | Epic 22, Story 22.14 | Done |
| Stage-aware content types/categories | Epic 21, Stories 21.3-21.4 | Done |
| Config storage trait | Epic 21, Story 21.1 | Done |
| Conflict detection | Epic 21, Story 21.6 | Done |
| Stage-aware menus/aliases | Epic 21, Story 21.5 | Done |
| Stage hierarchy support | Epic 21, Story 21.8 | Done |
| HTML filter (ammonia allowlist) | Epic 24, Story 24.2 | Done |
| Email delivery (SMTP) | Epic 22 (EmailService) | Done |
| Text format per-role permissions | Epic 16 / item routes | Done |
| User lifecycle taps (all 5) | Epic 22 / admin routes | Done |
| Gather admin UI | Epic 23, Stories 23.1-23.10 | Done |
| Block editor (Editor.js) | Epic 24, Stories 24.1-24.9 | Done |
| Security hardening review | Epic 27, Stories 27.1-27.9 | Done |

---

## Gaps to Address in Planned Epics

### Epic 23: Gather UI and Query Consolidation (Backlog)

| Gap | Story | Priority |
|---|---|---|
| No Gather admin UI | 23.1 through 23.8 | HIGH |
| No exposed filter config UI | 23.4 | MEDIUM |
| Convert hardcoded queries to Gather | 23.9 | MEDIUM |
| Gather query access control | 23.10 | MEDIUM |

This is the most significant remaining alignment gap. Gather works programmatically but lacks the admin UI that made D6 Views accessible to non-developers.

### Epic 24: Block Editor (Backlog)

| Gap | Story | Priority |
|---|---|---|
| Block-based content editing | 24.1 through 24.9 | MEDIUM |

Not a D6 alignment gap (D6 had no block editor), but a modern CMS expectation.

---

## Gaps Recommended for Near-Term Implementation

~~Items 1-4 have been resolved. See below for remaining near-term work.~~

### ~~1. HTML Filter Upgrade~~ RESOLVED

Resolved in Epic 24. `FilteredHtmlFilter` uses `ammonia` v4 with tag/attribute allowlists.

### ~~2. Email Delivery Infrastructure~~ RESOLVED

Resolved in Epic 22. `EmailService` with full SMTP support via `lettre` crate.

### ~~3. Text Format Per-Role Permissions~~ RESOLVED

Resolved. `"use filtered_html"` and `"use full_html"` permissions enforced per role.

### ~~4. User Lifecycle Taps~~ RESOLVED

Resolved. All five `tap_user_*` taps implemented (login, logout, register, update, delete).

### 5. Public User Registration (Epic 28)

Add self-service user registration at `/user/register` with email verification. Currently users can only be created by administrators.

**Effort:** M (2-3 days)
**Rationale:** Multi-user CMS deployments need self-service signup.
**Epic:** 28, Story 28.1

### 6. Automatic Path Alias Generation (Epic 28)

Add pattern-based URL alias generation (Pathauto equivalent) for items.

**Effort:** M (2-3 days)
**Rationale:** Editorial friction without automatic URL alias generation.
**Epic:** 28, Story 28.3

---

## Gaps Intentionally Deferred (Post-v1.0)

| Gap | Rationale |
|---|---|
| ~~Automatic path alias generation (Pathauto)~~ | Being addressed in Epic 28, Story 28.3 |
| File usage reference counting | Current behavior (permanent = stays, temporary = cleaned) is acceptable. Implement when storage management matters. |
| Tile/Block placement UI | Page layout handled via templates. Admin UI is a product concern for post-v1.0. |
| Sub-theme inheritance | Single-theme sites don't need it. Add when the theming ecosystem matures. |
| Free-tagging autocomplete | Standard term selection works. Autocomplete is a UX enhancement. |
| ~~Local tasks (admin tabs)~~ | Being addressed in Epic 28, Story 28.4 |
| ~~Stage-aware menus/aliases (Epic 21, Story 21.5)~~ | Resolved in Epic 21 |
| ~~Stage hierarchy support (Epic 21, Story 21.8)~~ | Resolved in Epic 21 |

---

## Gaps Intentionally Skipped

These D6 features are intentionally not being implemented:

| Gap | Rationale |
|---|---|
| `hook_init` / `hook_exit` | Axum middleware handles per-request concerns more efficiently. |
| Actions/Trigger system | Tap system + webhooks cover the same use cases more cleanly. |
| Update status checking | Different deployment model (compiled binary, not download-and-unzip). |
| Term synonyms | Rarely used in D6 sites. |
| Poor man's cron | External scheduling (systemd, cron, k8s) is more reliable. |
| User 1 magic | Replaced with `is_admin` boolean (intentional improvement). |
| EAV field storage | Replaced with JSONB (intentional improvement). |
| Database sessions | Replaced with Redis (intentional improvement). |
| MD5 password hashing | Replaced with Argon2id (intentional improvement). |
| PHP modules | Replaced with WASM sandboxed plugins (intentional improvement). |
| Multi-database support | PostgreSQL-only (intentional; see `intentional-divergences.md` Section 12). |

---

## Success Criteria for D6 + CCK + Views Parity

Trovato reaches D6 core + CCK + Views functional parity when these 14 subsystems are operational. Status is assessed against the `drupal6-mapping.md` alignment tables.

| # | Subsystem | Status | Blocking Gaps |
|---|---|---|---|
| 1 | Content modeling (types, fields, CRUD, revisions) | Done | -- |
| 2 | Content listings (Gather query engine) | Done | -- |
| 3 | Categorization (vocabularies, terms, hierarchy) | Done | -- |
| 4 | User management (auth, roles, permissions, access) | Done | -- |
| 5 | Forms (declarative build, validate, submit, AJAX) | Done | -- |
| 6 | Theming (templates, suggestions, preprocess, render) | Done | -- |
| 7 | Files (upload, managed tracking, cleanup, images) | Done | -- |
| 8 | Search (full-text, field weights, ranking) | Done | -- |
| 9 | Cron/Queue (scheduled tasks, distributed locking) | Done | -- |
| 10 | URL aliases (human-readable URLs, stage/language aware) | Done | -- |
| 11 | Text formats (filter pipelines, HTML sanitization) | Done | -- |
| 12 | Comments (threading, moderation) | Done | -- |
| 13 | Multilingual (UI strings, content, config translation) | Done | -- |
| 14 | Blocks/Tiles (renderable content regions) | Deferred | Post-v1.0 — page layout handled via Tera templates |

**Summary:** 13 of 14 subsystems are fully operational. The only remaining gap is the Tile/Block subsystem (#14), which is intentionally deferred to post-v1.0 — page layout is handled through Tera templates. Epic 28 adds public user registration (self-service signup) and automatic path alias generation (Pathauto).

Trovato significantly exceeds D6 in: content staging (Stages), security (WASM sandbox, RenderElements, Argon2id, CSRF, rate limiting, cargo-audit CI), performance (JSONB, two-tier cache, async runtime), and modern features (OAuth2, webhooks, image styles, scheduled publishing, content locking, audit logging, config export/import, block editor). It is PostgreSQL-only, which is the primary migration barrier for D6 sites running MySQL.
