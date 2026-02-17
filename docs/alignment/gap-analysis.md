# Gap Analysis and Prioritization

Gaps between Trovato and Drupal 6 + CCK + Views, sorted by priority. Each gap includes impact assessment, effort estimate, and recommended resolution. Gaps are identified from the alignment tables in `drupal6-mapping.md`; scheduling is in `roadmap.md`.

**Priority:** HIGH = blocks common use cases | MEDIUM = noticeable absence | LOW = edge case or intentional omission

**Effort:** S = hours | M = days | L = weeks

**Resolution:** Implement = build it | Defer = planned for later | Skip = intentionally omit

---

## HIGH Priority

### 1. HTML Filter Uses Regex Instead of Allowlist Parser

**Gap:** `FilteredHtmlFilter` strips dangerous tags/attributes via regex patterns rather than parsing HTML with an allowlist. Regex-based HTML filtering is fundamentally unreliable -- edge cases in HTML parsing can bypass blocklist patterns.

**D6 equivalent:** `filter_xss()` used a character-by-character parser with a tag allowlist. It had multiple CVEs over its lifetime and was far from bulletproof, but its allowlist approach was structurally more defensible than a regex blocklist.

**Impact:** Security risk. A sufficiently crafted HTML payload could potentially bypass the regex blocklist and achieve XSS in `filtered_html` format content. This is the most significant open security gap.

**Effort:** M (2-3 days). Replace regex with `ammonia` crate (HTML sanitizer built on `html5ever`) or equivalent Rust HTML parser. Define tag/attribute allowlists per format.

**Resolution:** Implement. This should be addressed before any production deployment.

**File:** `crates/kernel/src/content/filter.rs`

---

### 2. No Gather Admin UI

**Gap:** Gather (the Views equivalent) works programmatically and via config, but there is no admin UI for building query definitions. Content administrators cannot create or modify listings without developer intervention.

**D6 equivalent:** Views provided a full drag-and-drop admin UI for building queries, including field selection, filter configuration, sort ordering, and live preview.

**Impact:** Major usability gap. One of D6's most valued features was the ability for non-developers to build content listings. Without the Gather UI, Trovato requires developer involvement for every new listing.

**Effort:** L (Epic 23 -- 10 stories). This is a substantial UI project covering form builder, display config, exposed filters, includes, contextual filters, live preview, cloning, and access control.

**Resolution:** Defer to Epic 23 (planned). Programmatic/config-driven query creation works for v1.0.

---

### 3. No Email Delivery Infrastructure

**Gap:** Trovato has no SMTP or email-sending capability. Password reset tokens are logged to the console rather than emailed. User registration cannot send welcome emails. Comment notifications, webhook failure alerts, and any other email-dependent feature cannot function.

**D6 equivalent:** `drupal_mail()` provided a composable email pipeline with `hook_mail` for message building and `hook_mail_alter` for modification. Default transport was PHP's `mail()` function; SMTP modules provided authenticated sending.

**Impact:** Any feature that needs to communicate with users outside the browser (password reset, notifications, alerts) is non-functional in a production deployment.

**Effort:** M (2-3 days). Add `lettre` crate for SMTP, create a `MailService` with configurable transport (SMTP, sendmail, or log-only for dev), wire into password reset and registration routes.

**Resolution:** Implement. Required before any multi-user production deployment.

**File:** New `crates/kernel/src/services/mail.rs`

---

## MEDIUM Priority

### 4. No Automatic Path Alias Generation (Pathauto)

**Gap:** URL aliases must be manually created for each item. There is no pattern-based automatic generation from item title or other fields.

**D6 equivalent:** The Pathauto contrib module (not core, but installed on nearly every D6 site) automatically generated URL aliases from configurable patterns (e.g., `blog/[title]`, `news/[yyyy]/[mm]/[title]`).

**Impact:** Editorial friction. Every content item requires manual alias entry. Sites with hundreds of items need automation, but manual alias creation works for smaller sites.

**Effort:** M (3-5 days). Implement a pattern system using item field tokens, trigger on item create/update, integrate with existing `url_alias` infrastructure. Could be a plugin with kernel support.

**Resolution:** Defer. URL aliases work manually. Automatic generation is a post-v1.0 enhancement. The infrastructure (url_alias table, middleware, language/stage columns) is ready.

---

### 5. Text Format Per-Role Permissions

**Gap:** All authenticated users can use any text format. There is no per-format permission assignment (e.g., "use filtered_html", "use full_html").

**D6 equivalent:** Administrators assigned text format permissions per role. The `full_html` format was restricted to trusted roles, while `filtered_html` was available to all authenticated users.

**Impact:** Security concern for multi-author sites. Untrusted editors could use `full_html` to inject arbitrary HTML/JS (bypassing the filter pipeline entirely since `full_html` passes content through unmodified).

**Effort:** S (1 day). Add `format_permission` map to filter configuration. Check permission in `FilterPipeline::process()`. Add format selection to item edit form.

**Resolution:** Implement when prioritized. Low effort, meaningful security improvement.

**File:** `crates/kernel/src/content/filter.rs`

---

### 6. Missing `tap_user_*` Lifecycle Hooks

**Gap:** Only `tap_user_login` exists. There are no taps for user logout, registration, profile update, or deletion.

**D6 equivalent:** `hook_user` was a multi-operation hook covering: login, logout, insert (register), update, delete, view, validate, form.

**Impact:** Plugins cannot react to user lifecycle events beyond login. For example: audit logging of user changes, welcome emails on registration, cleanup on deletion.

**Effort:** S (1 day). Add `tap_user_logout`, `tap_user_register`, `tap_user_update`, `tap_user_delete` to the tap registry and invoke from appropriate route handlers.

**Resolution:** Implement when prioritized. The infrastructure exists; these are straightforward additions.

**Files:** `crates/kernel/src/tap/registry.rs`, `crates/kernel/src/routes/auth.rs`

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

### 17. No Local Tasks (Admin Tabs)

**Gap:** No equivalent of D6's local tasks (the tab navigation on admin pages like View/Edit/Revisions).

**D6 equivalent:** `hook_menu` could define `MENU_LOCAL_TASK` items that appeared as tabs on their parent page.

**Impact:** Admin UX gap. Tab-style navigation between related admin pages (view/edit/translate/revisions) requires manual template handling.

**Effort:** S (1 day). Add `local_task` flag to MenuDefinition, render tab navigation in admin templates.

**Resolution:** Defer. Can be added to the menu system when the admin UI is polished.

---

### 18. No Primary/Secondary Links

**Gap:** No concept of primary (main navigation) and secondary (user menu) link sets.

**D6 equivalent:** Theme regions for primary and secondary links, populated from menu trees.

**Impact:** Themes must hardcode navigation structure rather than pulling from a configurable menu.

**Effort:** S (half day). Designate specific menus as primary/secondary, expose in template context.

**Resolution:** Defer. Solvable with template-level configuration.
