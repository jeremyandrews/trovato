# Epic 13 (D): Privacy Infrastructure (GDPR)

**Tutorial Parts Affected:** 6 (community — user registration), 4 (editorial — revisions)
**Trovato Phase Dependency:** Phase 1 (User Auth) — already complete
**BMAD Epic:** 43
**Status:** ~85% complete. Implemented: personal_data bool on FieldDefinition (SDK), user consent fields migration (consent_given, consent_date, consent_version), retention_days on items, /api/v1/user/export endpoint (filters to PII fields, queries items/comments/files), tap_user_export hook registered. Remaining: plugin-side tap_user_export implementations, export format documentation.
**Estimated Effort:** 2–3 weeks
**Dependencies:** None (independent of A–C)
**Blocks:** None

---

## Narrative

*GDPR is not a checkbox. It is not a cookie consent banner bolted onto the footer. It is a set of data subject rights — access, rectification, erasure, portability — that your platform either supports or doesn't. The kernel's job is to make supporting them structurally possible. Plugins implement the policy.*

Trovato's privacy posture today is mixed. On the positive side: `tap_user_delete` exists (plugins can clean up when a user is deleted), the default theme loads no external images or fonts (no tracking pixels, no Google Fonts CDN calls), and there's no analytics tracking built in. These are good defaults — the absence of bad practices is itself a privacy feature.

What's missing is the infrastructure for GDPR's active requirements:

1. **No consent tracking.** The user table has no consent fields. A site can't record *when* a user consented, *what version* of the privacy policy they consented to, or *whether* they consented at all. Without this, a cookie consent plugin has nowhere to store its state in the user record.

2. **No personal data markers.** Field definitions don't indicate which fields contain PII. A "full name" field and a "favorite color" field look identical to the system. Without markers, a data export plugin can't automatically find all personal data — it would need per-content-type configuration instead of per-field metadata.

3. **No data export hook.** `tap_user_delete` lets plugins clean up on deletion, but there's no `tap_user_export` for data portability. GDPR Article 20 requires providing users their data in a machine-readable format.

4. **No retention metadata.** Items and revisions have no `retention_days` — a data retention cron job can't know which content should be aged out.

**Lowest common denominator test:** A personal blog ignores all of this — nullable columns, no PII fields marked, no retention set. An EU-facing SaaS site depends on every piece being there from day one. The `language` column precedent applies: it exists for everyone, matters for some, costs nothing for the rest.

**Before this epic:** User deletion hook exists. No consent tracking, no PII markers, no export hook, no retention metadata.

**After this epic:** User table has consent fields. Field definitions can be marked as PII. `tap_user_export` exists for data portability plugins. Items and revisions carry retention metadata. A GDPR compliance plugin has everything it needs from the kernel.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| Consent columns on user table | Schema is kernel — plugins can't add columns to the user table |
| `personal_data` flag on FieldDefinition | SDK type definitions are kernel — this is the plugin API contract |
| `tap_user_export` hook | Tap infrastructure is kernel — plugins subscribe to taps |
| `retention_days` on items/revisions | Schema is kernel — retention metadata must be queryable for any cleanup plugin. Note: `item.retention_days` (content lifecycle) and `users.data_retention_days` (account lifecycle) are independent — no precedence relationship; retention plugins handle them separately. |
| Verify no external resource loading | Template defaults are kernel |

Every item is schema or infrastructure. The *policies* (what constitutes consent, how long to retain data, what format to export in) are all plugin territory.

---

## BMAD Stories

### Story 43.1: User Consent Schema

**As a** GDPR compliance plugin developer,
**I want** consent metadata stored on the user record,
**So that** I can track when users consented and to which privacy policy version.

**Acceptance criteria:**

- [ ] Migration adds columns to `users` table: `consent_given` (BOOLEAN, DEFAULT FALSE), `consent_date` (TIMESTAMPTZ, NULLABLE), `consent_version` (VARCHAR(64), NULLABLE), `data_retention_days` (INTEGER, NULLABLE)
- [ ] Existing users get `consent_given = false`, `consent_date = NULL`, `consent_version = NULL`, `data_retention_days = NULL`
- [ ] `User` model struct in kernel includes consent fields
- [ ] User admin form (`/admin/people/{id}/edit`) displays consent fields as read-only info (not editable by admins — consent is user-initiated)
- [ ] `User` serialization to plugins includes consent fields (plugins can read but not directly write — consent changes go through a kernel service method)
- [ ] Kernel `UserService` gains `record_consent(user_id, version)` method that sets `consent_given = true`, `consent_date = NOW()`, `consent_version = version`
- [ ] Kernel `UserService` gains `withdraw_consent(user_id)` method that sets `consent_given = false` (preserves `consent_date` and `consent_version` for audit trail)
- [ ] At least 2 integration tests: record consent, withdraw consent

**Implementation notes:**
- Migration: `ALTER TABLE users ADD COLUMN consent_given BOOLEAN DEFAULT FALSE, ADD COLUMN consent_date TIMESTAMPTZ, ADD COLUMN consent_version VARCHAR(64), ADD COLUMN data_retention_days INTEGER`
- Modify `crates/kernel/src/models/user.rs` — add fields
- Modify `crates/kernel/src/services/user.rs` — add consent methods
- Consent *collection* (the UI that asks for consent) is plugin territory — the kernel stores the result

---

### Story 43.2: Personal Data Flag on Field Definitions

**As a** plugin developer building data export/deletion functionality,
**I want** field definitions to indicate which fields contain personal data,
**So that** I can automatically find all PII in the system without per-content-type configuration.

**Acceptance criteria:**

- [ ] `FieldDefinition` in `crates/plugin-sdk/src/types.rs` gains `personal_data: bool` field (default `false`)
- [ ] Admin UI for field management (`/admin/structure/types/{type}/fields`) includes a "Contains personal data" checkbox
- [ ] Field definitions serialized to YAML via `config export` include `personal_data` when `true`
- [ ] `config import` accepts `personal_data` on field definitions
- [ ] ContentTypeRegistry exposes method `personal_data_fields(item_type: &str) -> Vec<&str>` returning field names marked as PII
- [ ] Existing field definitions default to `personal_data: false` (no migration needed for field data — default handles it)
- [ ] At least 1 integration test: define a content type with PII fields, query `personal_data_fields()`

**Implementation notes:**
- Modify `crates/plugin-sdk/src/types.rs` — add `personal_data: bool` to `FieldDefinition` with `#[serde(default)]`
- Modify `crates/kernel/src/content/type_registry.rs` — add `personal_data_fields()` method
- This is a metadata marker — the kernel doesn't *do* anything with it. Export and deletion plugins use it to find PII automatically.
- Backward compatible: `#[serde(default)]` means existing definitions without the field deserialize as `false`

---

### Story 43.3: User Data Export Tap

**Blocked by:** Story 43.2 (requires `personal_data_fields()` method for AC #7)

**As a** GDPR compliance plugin developer,
**I want** a `tap_user_export` hook,
**So that** plugins can contribute their data to a user's data portability export.

**Acceptance criteria:**

- [ ] New `tap_user_export` tap added to the tap registry
- [ ] Tap signature: `tap_user_export(user_id: Uuid) -> UserExportData` where `UserExportData` is a structured type containing `plugin_name: String`, `data_type: String`, `records: Vec<serde_json::Value>`
- [ ] Kernel aggregates `UserExportData` from all plugins that implement `tap_user_export`
- [ ] Kernel provides a `/api/v1/user/export` endpoint (authenticated — users can only export their own data; admins can export any user's data)
- [ ] Export format: JSON (machine-readable per GDPR Article 20)
- [ ] Export includes: user profile fields, all items authored by the user, all comments by the user, all file uploads by the user, plus plugin-contributed data
- [ ] Items include only fields marked `personal_data: true` plus title and metadata (not all fields)
- [ ] Export endpoint is rate-limited (1 request per hour per user — exports can be expensive)
- [ ] At least 2 integration tests: export with no plugins, export with a mock plugin contributing data

**Implementation notes:**
- Add `tap_user_export` to `crates/kernel/src/tap/` registry
- Add `UserExportData` type to `crates/plugin-sdk/src/types.rs`
- Add export route to `crates/kernel/src/routes/api_v1.rs`
- The kernel handles the core user data export; plugins add their own data via the tap
- JSON format is simplest and most portable. CSV or XML export is plugin territory.

---

### Story 43.4: Retention Metadata on Items and Revisions

**As a** data retention plugin developer,
**I want** retention metadata on items and revisions,
**So that** I can implement automated data cleanup policies.

**Acceptance criteria:**

- [ ] Migration adds `retention_days` (INTEGER, NULLABLE) to `item` table
- [ ] Migration adds `retention_days` (INTEGER, NULLABLE) to `item_revision` table
- [ ] When `retention_days` is NULL, no automatic retention policy applies (content kept indefinitely — the default)
- [ ] Admin UI item edit form includes optional "Retention period (days)" field
- [ ] Content type definition can specify a default `retention_days` for new items of that type
- [ ] `Item` model includes `retention_days` field, serialized to plugins
- [ ] Kernel does NOT implement the retention cron job — only the schema. The cron job is plugin territory.
- [ ] At least 1 integration test: create item with retention_days, verify it persists

**Implementation notes:**
- Migration: `ALTER TABLE item ADD COLUMN retention_days INTEGER; ALTER TABLE item_revision ADD COLUMN retention_days INTEGER`
- Modify `crates/kernel/src/models/item.rs` — add field
- The kernel stores the metadata. A retention plugin runs a cron job (`tap_cron`) that queries `WHERE retention_days IS NOT NULL AND created + retention_days * 86400 < NOW()` and deletes/archives expired content.
- Note: the audit log service already has `cleanup(retention_days)` (see `services/audit.rs`) — the retention plugin can use a similar pattern

---

### Story 43.5: External Resource Loading Audit

**As a** privacy-conscious site operator,
**I want** the default theme to load no external resources,
**So that** no third-party tracking occurs without explicit opt-in.

**Acceptance criteria:**

- [ ] Audit all templates (93 files) for external URLs: no `<link>`, `<script>`, `<img>`, `<iframe>`, `@import`, `url()` referencing external domains
- [ ] Audit `static/` directory for references to external resources (CDNs, analytics, fonts)
- [ ] Audit `base.html` CSS for `@font-face` rules loading from external URLs
- [ ] Verify render pipeline does not inject external resources (no hardcoded CDN URLs in Rust code)
- [ ] Document the "no external resources" policy in operational docs
- [ ] If any external resources are found, replace with local equivalents or remove
- [ ] Add a CI check (grep-based) that flags new external URLs in template files

**Implementation notes:**
- `grep -rn 'https\?://' templates/ static/` — check for external URLs
- This is primarily a verification story, not a development story
- The current state is likely clean (no CDN fonts, no analytics) but must be verified and documented

---

## Plugin SDK Changes

| Change | File | Breaking? | Affected Plugins |
|---|---|---|---|
| `personal_data: bool` on `FieldDefinition` | `crates/plugin-sdk/src/types.rs` | No (`#[serde(default)]`) | All plugins that define content types — existing definitions work unchanged |
| `UserExportData` type | `crates/plugin-sdk/src/types.rs` | No (new type) | None — plugins opt in by implementing `tap_user_export` |

**Migration guide:** No action required. `personal_data` defaults to `false` for all existing field definitions. Plugins that want to mark fields as PII add `personal_data: true` to their field definitions. Plugins that want to contribute to user exports implement `tap_user_export`.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Content-Model.md` | Add "Privacy Metadata" section: `retention_days` on items/revisions, `personal_data` on field definitions. Document the consent fields on users. |
| `docs/design/Design-Plugin-SDK.md` | Add `tap_user_export` to tap reference. Document `UserExportData` type. Document `personal_data` on `FieldDefinition`. |
| `docs/design/Design-Web-Layer.md` | Add "Privacy by Default" section: no external resources policy, consent service methods, export endpoint. |

---

## Tutorial Impact

| Tutorial Part | Sections Affected | Nature of Change |
|---|---|---|
| `part-06-community.md` | User registration section | Brief mention that user accounts include consent tracking fields (nullable, not required for basic registration). Note: consent UI is plugin territory. |
| `part-04-editorial-engine.md` | Revisions section | Brief mention that revisions can carry `retention_days` metadata for automated cleanup. Note: retention cron is plugin territory. |

These are minor additions — a sentence or two noting the fields exist. The tutorial doesn't demonstrate GDPR workflows (those are plugin-level features).

---

## Recipe Impact

Recipes for parts 4 and 6 need minor updates matching tutorial changes. Run `docs/tutorial/recipes/sync-check.sh` and update hashes.

---

## Screenshot Impact

| Part | Screenshots | Reason |
|---|---|---|
| Part 6 | User admin form | Consent fields visible (read-only) on user edit form |

---

## Config Fixture Impact

Content type YAML definitions in `docs/tutorial/config/` may include `personal_data: true` on appropriate fields (e.g., email fields, name fields) as examples.

---

## Migration Notes

**Database migrations:**
1. `YYYYMMDD000001_add_user_consent_fields.sql` — ADD consent_given, consent_date, consent_version, data_retention_days to users
2. `YYYYMMDD000002_add_item_retention_days.sql` — ADD retention_days to item
3. `YYYYMMDD000003_add_revision_retention_days.sql` — ADD retention_days to item_revision

**Breaking changes:** None. All columns are nullable with defaults. All SDK changes use `#[serde(default)]`.

**Upgrade path:** Run migrations. No data transformation needed. Existing data gets null/false defaults.

---

## What's Deferred

- **Cookie consent UI** — Plugin. The kernel stores consent; a plugin presents the banner and records the choice.
- **Data retention cron job** — Plugin. The kernel stores `retention_days`; a plugin runs the cleanup.
- **Right to rectification UI** — Plugin. Standard edit forms already support rectification; a dedicated "my data" page is a plugin feature.
- **Data Processing Agreement (DPA) templates** — Documentation/legal, not code.
- **Anonymization** (replace PII with hashed/pseudonymized values instead of deleting) — Plugin. The kernel's `tap_user_delete` and `personal_data` markers provide the infrastructure.
- **Privacy Impact Assessment tooling** — External tooling, not kernel.
- **Consent versioning UI** (comparing consent across policy versions) — Plugin.

---

## Related

- [Design-Content-Model.md](../design/Design-Content-Model.md) — Item and user schema
- [Design-Plugin-SDK.md](../design/Design-Plugin-SDK.md) — Tap reference and SDK types
- [Epic C (12): Security Hardening](epic-12-security.md) — Session management complements privacy infrastructure
- [Epic F (15): Versioning & Audit](epic-15-versioning.md) — Revision schema changes coordinated
