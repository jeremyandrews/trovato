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

These should be addressed before v1.0 production deployment:

### 1. HTML Filter Upgrade (HIGH)

Replace regex-based `FilteredHtmlFilter` with `ammonia` crate for proper HTML allowlist parsing.

**Effort:** M (2-3 days)
**Rationale:** Security gap. Regex HTML filtering is unreliable.
**File:** `crates/kernel/src/content/filter.rs`
**Suggested epic:** Standalone security hardening task, no epic needed.

### 2. Email Delivery Infrastructure (HIGH)

Add SMTP/email sending capability. Password reset routes exist (`crates/kernel/src/routes/password_reset.rs`) but currently log tokens to the console instead of emailing them.

**Effort:** M (2-3 days)
**Rationale:** Password reset, registration notifications, and any user-facing communication requires email delivery.
**Files:** New `crates/kernel/src/services/mail.rs`, update `crates/kernel/src/routes/password_reset.rs`

### 3. Text Format Per-Role Permissions (MEDIUM)

Add format-level permission checks so `full_html` can be restricted to trusted roles.

**Effort:** S (1 day)
**Rationale:** Multi-author sites need this to prevent untrusted editors from using unfiltered HTML.
**File:** `crates/kernel/src/content/filter.rs`

### 4. User Lifecycle Taps (MEDIUM)

Add `tap_user_logout`, `tap_user_register`, `tap_user_update`, `tap_user_delete`.

**Effort:** S (1 day)
**Rationale:** Plugins need to react to user lifecycle events for audit logging, notifications, cleanup.
**Files:** `crates/kernel/src/tap/registry.rs`, `crates/kernel/src/routes/auth.rs`

---

## Gaps Intentionally Deferred (Post-v1.0)

| Gap | Rationale |
|---|---|
| Automatic path alias generation (Pathauto) | Infrastructure ready (url_alias table, middleware). Pattern-based generation is an enhancement, not a blocker. Pathauto was a D6 contrib module, not core. |
| File usage reference counting | Current behavior (permanent = stays, temporary = cleaned) is acceptable. Implement when storage management matters. |
| Tile/Block placement UI | Page layout handled via templates. Admin UI is a product concern for post-v1.0. |
| Sub-theme inheritance | Single-theme sites don't need it. Add when the theming ecosystem matures. |
| Free-tagging autocomplete | Standard term selection works. Autocomplete is a UX enhancement. |
| Local tasks (admin tabs) | Can be added incrementally to the menu system. |
| Stage-aware menus/aliases (Epic 21, Story 21.5) | Requires menu DB storage. Post-MVP. |
| Stage hierarchy support (Epic 21, Story 21.8) | Child-parent-live workflows are an advanced use case. |

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
| 2 | Content listings (Gather query engine) | Partial | No admin UI (Epic 23) |
| 3 | Categorization (vocabularies, terms, hierarchy) | Done | -- |
| 4 | User management (auth, roles, permissions, access) | Partial | Email delivery needed for password reset |
| 5 | Forms (declarative build, validate, submit, AJAX) | Done | -- |
| 6 | Theming (templates, suggestions, preprocess, render) | Done | -- |
| 7 | Files (upload, managed tracking, cleanup, images) | Done | -- |
| 8 | Search (full-text, field weights, ranking) | Done | -- |
| 9 | Cron/Queue (scheduled tasks, distributed locking) | Done | -- |
| 10 | URL aliases (human-readable URLs, stage/language aware) | Done | -- |
| 11 | Text formats (filter pipelines, HTML sanitization) | Partial | Regex filter, no per-role permissions |
| 12 | Comments (threading, moderation) | Done | -- |
| 13 | Multilingual (UI strings, content, config translation) | Done | -- |
| 14 | Blocks/Tiles (renderable content regions) | Missing | No data model or code exists |

**Summary:** 10 of 14 subsystems are fully operational. The 4 remaining gaps are: Gather admin UI (L effort, Epic 23), email delivery infrastructure (M), HTML filter upgrade (M), and tile/block subsystem (L). Text format per-role permissions (S) and user lifecycle taps (S) are smaller items.

Trovato significantly exceeds D6 in: content staging (Stages), security (WASM sandbox, RenderElements, Argon2id, CSRF, rate limiting), performance (JSONB, two-tier cache, async runtime), and modern features (OAuth2, webhooks, image styles, scheduled publishing, content locking, audit logging, config export/import). It is also PostgreSQL-only, which is the primary migration barrier for D6 sites running MySQL.
