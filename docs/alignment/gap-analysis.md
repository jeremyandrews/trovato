# Gap Analysis and Prioritization

Gaps between Trovato and Drupal 6 + CCK + Views, sorted by priority. Each gap includes impact assessment, effort estimate, and recommended resolution. Gaps are identified from the alignment tables in `drupal6-mapping.md`; scheduling is in `roadmap.md`.

**Priority:** HIGH = blocks common use cases | MEDIUM = noticeable absence | LOW = edge case or intentional omission

**Effort:** S = hours | M = days | L = weeks

**Resolution:** Implement = build it | Defer = planned for later | Skip = intentionally omit

---

## HIGH Priority

### 1. ~~HTML Filter Uses Regex Instead of Allowlist Parser~~ RESOLVED

**Status:** Resolved in Epic 24 (Story 24.2). `FilteredHtmlFilter` now uses the `ammonia` crate (v4) with explicit tag/attribute allowlists. Regex-based filtering was fully replaced. Security regression tests cover 11 XSS vectors.

**File:** `crates/kernel/src/content/filter.rs`

---

### 2. ~~No Gather Admin UI~~ RESOLVED

**Status:** Resolved in Epic 23 (Stories 23.1-23.10). Full admin UI with query builder, live preview, relationship editor, display configuration, and performance guardrails. Admin and content gather views registered.

**Files:** `crates/kernel/src/routes/gather_admin.rs`, `templates/admin/gather-form.html`

---

### 3. ~~No Email Delivery Infrastructure~~ RESOLVED

**Status:** Resolved in Epic 22. `EmailService` in `crates/kernel/src/services/email.rs` provides full SMTP support via `lettre` crate with STARTTLS/TLS/plain modes, optional authentication, and pre-built password reset email method. Wired into password reset flow.

**File:** `crates/kernel/src/services/email.rs`

---

## MEDIUM Priority

### 4. ~~No Automatic Path Alias Generation (Pathauto)~~ RESOLVED

**Status:** Resolved in Epic 28 (Story 28.3). Pattern-based URL alias generation using configurable patterns per content type (e.g., `[type]/[title]`, `news/[yyyy]/[mm]/[title]`). Aliases auto-generated on item create and updated on item edit. Unique alias collision handling with numeric suffixes. Configured via `pathauto_patterns` in site config.

**File:** `crates/kernel/src/services/pathauto.rs`

---

### 5. ~~Text Format Per-Role Permissions~~ RESOLVED

**Status:** Resolved. `permitted_text_formats()` in `crates/kernel/src/routes/item.rs` checks `"use filtered_html"` and `"use full_html"` permissions per user role. `FilterPipeline::for_format_checked()` downgrades `full_html` to `filtered_html` when the user lacks permission. Permissions available in the admin permissions matrix.

**Files:** `crates/kernel/src/routes/item.rs`, `crates/kernel/src/content/filter.rs`

---

### 6. ~~Missing `tap_user_*` Lifecycle Hooks~~ RESOLVED

**Status:** Resolved. All five user lifecycle taps are declared in KNOWN_TAPS and dispatched from route handlers:
- `tap_user_login` — dispatched in `auth.rs` login handler
- `tap_user_logout` — dispatched in `auth.rs` logout handler
- `tap_user_register` — dispatched in `admin_user.rs` user create handler
- `tap_user_update` — dispatched in `admin_user.rs` user update handler
- `tap_user_delete` — dispatched in `admin_user.rs` user delete handler

**Files:** `crates/kernel/src/routes/auth.rs`, `crates/kernel/src/routes/admin_user.rs`

---

### 7. No File Usage Reference Counting

**Gap:** No tracking of which items reference which files. Files are marked permanent/temporary but there's no `file_usage` equivalent.

**D6 equivalent:** The `file_usage` table tracked references from entities to files, with module and type columns. Files with zero references could be safely deleted. Reference counting prevented orphan files and premature deletion.

**Impact:** Files marked permanent are never automatically cleaned up even if no item references them. Over time, storage accumulates orphan files.

**Effort:** M (2-3 days). Create `file_usage` table, update file references on item save/delete, add cleanup cron task for zero-reference permanent files.

**Resolution:** Defer. Current behavior (permanent files stay forever, temporary files cleaned up by cron) is acceptable for v1.0. Implement when storage management becomes a concern.

**File:** `crates/kernel/src/file/service.rs`

---

### 8. No Tile/Block Subsystem

**Gap:** There is no tile/block code in the kernel -- no data model, no templates, no placement UI. The Terminology.md defines "Block -> Tile" and "Region -> Slot" as renamed concepts, but neither has been implemented. Page layout regions exist only in Tera templates as hardcoded sections.

**D6 equivalent:** Block admin page (`/admin/build/block`) allowed drag-and-drop placement of blocks into theme regions with per-page visibility rules and role-based access. Modules registered blocks via `hook_block`.

**Impact:** Site builders cannot add dynamic content to page regions (sidebars, headers, footers) without editing templates. This is a noticeable gap for sites that need configurable layouts.

**Effort:** L (multiple stories). Tile entity model, `tap_tile_info`, region registry, placement table, admin UI, visibility rules.

**Resolution:** Defer to post-v1.0. Page layout is handled through Tera templates for now.

---

### 9. No Sub-Theme Inheritance

**Gap:** Themes cannot inherit from a parent theme. Each theme is standalone.

**D6 equivalent:** Sub-themes inherited templates, CSS, and settings from their parent, overriding only what needed to change. This enabled theme families (e.g., Zen sub-themes).

**Impact:** Theme customization requires copying and modifying the entire theme rather than overriding specific templates.

**Effort:** M (2-3 days). Add `base_theme` to theme config, implement template resolution chain that falls back to parent theme.

**Resolution:** Defer. Single-theme sites (the common case for v1.0) don't need inheritance.

---

## LOW Priority

### 10. No `hook_init` / `hook_exit` Per-Request Hooks

**Gap:** No per-request lifecycle taps before/after routing.

**D6 equivalent:** `hook_init` ran on every page request before the menu system determined the page callback. `hook_exit` ran after the response was sent.

**Impact:** Minimal. Axum middleware handles the same concern more efficiently. Plugins rarely need per-request initialization.

**Resolution:** Skip. Middleware is the correct pattern for per-request concerns in an async framework.

---

### 11. No Actions/Trigger System

**Gap:** No equivalent of D6's Actions and Triggers modules.

**D6 equivalent:** Actions defined reusable operations (send email, publish node, etc.). Triggers associated actions with events (on node insert, on comment post, etc.).

**Impact:** Low. Webhooks provide the event notification mechanism. Custom actions can be implemented as plugins responding to taps.

**Resolution:** Skip. The tap system + webhooks cover the same use cases more cleanly.

---

### 12. No Update Status Checking

**Gap:** No mechanism to check for available updates to the kernel or plugins.

**D6 equivalent:** The Update Status module checked drupal.org for available module updates and displayed warnings on the admin dashboard.

**Impact:** Minimal. Trovato's deployment model (compiled binary + WASM plugins) doesn't map to D6's download-and-unzip update model.

**Resolution:** Skip. Package management is out of scope for v1.0.

---

### 13. No `hook_requirements` / System Status

**Gap:** No plugin-level system requirements check displayed on an admin status page.

**D6 equivalent:** `hook_requirements` let modules report status (PHP version, missing libraries, configuration issues) on the admin status page.

**Impact:** Low for v1.0 (compiled binary has no runtime dependency discovery). Could be useful as plugins mature.

**Effort:** S (1 day).

**Resolution:** Defer. Could be added as a `tap_requirements` when a system status admin page is built.

---

### 14. No Term Synonyms

**Gap:** Category terms have no synonym support.

**D6 equivalent:** `term_synonym` table allowed alternative names for taxonomy terms.

**Impact:** Minimal. Rarely used in D6 sites.

**Resolution:** Skip.

---

### 15. No Free-Tagging Autocomplete

**Gap:** No autocomplete widget for tag-style category term entry.

**D6 equivalent:** Taxonomy vocabularies could be configured for "free tagging" with an autocomplete text field that created terms on the fly.

**Impact:** Usability gap for sites using tag-style categorization. Terms must be created separately before referencing.

**Effort:** M (2-3 days). AJAX autocomplete endpoint, form element type, term-on-save creation logic.

**Resolution:** Defer. Standard term selection works. Autocomplete is a UX enhancement.

---

### 16. No Poor Man's Cron

**Gap:** Cron only runs via external scheduler hitting `/cron/{key}`. No automatic triggering from web requests.

**D6 equivalent:** `drupal_cron_run()` could be triggered by page requests when enough time had elapsed since the last run.

**Impact:** Minimal. External cron schedulers (systemd timers, crontab, Kubernetes CronJobs) are more reliable than request-triggered cron.

**Resolution:** Skip. External scheduling is the correct approach for production deployments.

---

### 17. ~~No Local Tasks (Admin Tabs)~~ RESOLVED

**Status:** Resolved in Epic 28 (Story 28.4). `local_task` flag added to MenuDefinition, tab bar macro renders horizontal tabs on admin entity pages (View/Edit/Revisions). Plugin-registered local tasks via `tap_menu` are merged with hardcoded tabs.

**Files:** `crates/kernel/src/menu/registry.rs`, `templates/admin/macros/tabs.html`

---

### 18. No Primary/Secondary Links

**Gap:** No concept of primary (main navigation) and secondary (user menu) link sets.

**D6 equivalent:** Theme regions for primary and secondary links, populated from menu trees.

**Impact:** Themes must hardcode navigation structure rather than pulling from a configurable menu.

**Effort:** S (half day). Designate specific menus as primary/secondary, expose in template context.

**Resolution:** Defer. Solvable with template-level configuration.
