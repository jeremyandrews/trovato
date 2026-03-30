# Inclusivity-First Foundation: Overall Summary

**Epics:** 10–19 (A–J)
**Total Stories:** 59
**Total Estimated Effort:** 25–38 weeks (parallelized: 16–22 weeks with 2–3 contributors)

---

## Timeline Estimate

| Wave | Epics | Effort | Calendar (parallel) |
|---|---|---|---|
| Wave 1 | A (Accessibility) | 3–4 weeks | 3–4 weeks |
| Wave 2 | B, C, D, E, F (all in parallel) | 2–4 weeks each | 3–4 weeks |
| Wave 3 | G, H (in parallel) | 5–7 / 3–4 weeks | 5–7 weeks |
| Wave 4 | I, J (sequential) | 2–3 / 1–2 weeks | 3–5 weeks |
| **Total** | | | **14–20 weeks** |

With a single contributor working sequentially: 25–38 weeks.
With 2–3 contributors working in parallel (Wave 2 epics, Wave 3 epics): 14–20 weeks.

Epic G (Multi-Tenancy) is the critical path — it's the longest individual epic at 5–7 weeks.

---

## All Database Migrations

| Epic | Migration | Table | Change |
|---|---|---|---|
| A (10) | `require_image_block_alt` | `item`, `item_revision` | JSONB update: add `"alt": ""` to image blocks missing alt |
| D (13) | `add_user_consent_fields` | `users` | ADD consent_given, consent_date, consent_version, data_retention_days |
| D (13) | `add_item_retention_days` | `item` | ADD retention_days INTEGER |
| D (13) | `add_revision_retention_days` | `item_revision` | ADD retention_days INTEGER |
| F (15) | `add_revision_change_summary` | `item_revision` | ADD change_summary JSONB |
| F (15) | `add_revision_ai_generated` | `item_revision` | ADD ai_generated BOOLEAN; CREATE immutability trigger |
| G (16) | `create_tenant_table` | `tenant` | CREATE TABLE; seed DEFAULT_TENANT_ID |
| G (16) | `add_tenant_id_to_items` | `item`, `item_revision` | ADD tenant_id UUID NOT NULL DEFAULT |
| G (16) | `add_tenant_id_to_categories` | `categories`, `category_tag` | ADD tenant_id UUID NOT NULL DEFAULT |
| G (16) | `add_tenant_id_to_supporting` | `file_managed`, `site_config`, `url_alias`, `stage`, `menu_link`, `tile`, `comments` | ADD tenant_id UUID NOT NULL DEFAULT |
| G (16) | `create_user_tenant` | `user_tenant` | CREATE TABLE; seed existing users |
| H (17) | `extend_ai_usage_log` | `ai_usage_log` | ADD latency_ms, plugin_name, finish_reason, status, deny_reason |

**Total: 12 migrations** (1 JSONB update, 4 ADD COLUMN, 2 CREATE TABLE, 5 ADD COLUMN batches)

**Migration ordering:** Migrations must run in this order: **A → D → F → G**.

- **A before F:** Epic A's JSONB backfill migration (adding `alt: ""` to image blocks in `item_revision.fields`) UPDATEs existing revision rows. Epic F's immutability trigger (Story 45.3) prevents all UPDATEs on `item_revision`. If F's trigger lands first, A's migration fails. Since A is Wave 1 and F is Wave 2, this is the natural order — but it must be enforced via migration timestamps.
- **D before F:** Epic D adds `retention_days` to `item_revision` (ALTER TABLE ADD COLUMN — not blocked by the trigger since it's a schema change, not a row UPDATE). However, ordering D before F avoids needing to reason about trigger interactions with DDL.
- **F before G:** Epic G adds `tenant_id` to `item_revision` (ALTER TABLE ADD COLUMN). This is safe after the trigger since DDL is not UPDATE, but ordering avoids confusion.
- **Post-F migrations that need to UPDATE `item_revision`:** Must temporarily disable the trigger: `ALTER TABLE item_revision DISABLE TRIGGER revision_immutability;` ... `ALTER TABLE item_revision ENABLE TRIGGER revision_immutability;` with an explicit comment explaining why.

---

## All SDK Breaking Changes

| Epic | Change | File | Breaking Level | Affected Plugins |
|---|---|---|---|---|
| A (10) | `ElementBuilder` ARIA helpers | `types.rs` | Additive (non-breaking) | 0 |
| B (11) | Un-ignore `Item.language` serde | `types.rs` | Soft addition (`Option<String>`) | 0 (existing plugins get `None`) |
| C (12) | `FieldAccessResult` type | `types.rs` | Additive | 0 |
| C (12) | Crypto host function bindings | SDK | Additive | 0 |
| D (13) | `personal_data` on `FieldDefinition` | `types.rs` | Soft addition (`#[serde(default)]`) | 0 (existing defs get `false`) |
| D (13) | `UserExportData` type | `types.rs` | Additive | 0 |
| G (16) | `TenantContext` type | `types.rs` | Additive | 0 |
| G (16) | `Item.tenant_id` field | `types.rs` | Soft addition (`Option<Uuid>`) | 0 (existing items get `None`) |
| H (17) | `AiRequestContext`/`AiRequestDecision` types | `types.rs` | Additive | 0 |

**No hard breaking changes.** All SDK changes are either additive (new types/methods) or soft additions (`Option<T>` with `#[serde(default)]`). Existing compiled WASM plugins continue to work without recompilation.

**Recommended:** After all epics land, bump the SDK minor version (e.g., 0.1.0 → 0.2.0) and publish a migration guide listing all new types, methods, and fields.

---

## All Design Doc Updates

| Doc | Epics That Modify It |
|---|---|
| `Design-Content-Model.md` | A (alt required), D (privacy metadata, retention), F (revision guarantees, change_summary, ai_generated), G (tenant schema) |
| `Design-Plugin-SDK.md` | A (ARIA helpers), B (Item.language), C (field_access, crypto), D (personal_data, user_export), G (TenantContext), H (tap_ai_request, route metadata) |
| `Design-Render-Theme.md` | A (accessibility defaults), B (format_date, language context), E (lazy loading, asset_url) |
| `Design-Web-Layer.md` | B (language negotiation), C (security headers, CORS), D (privacy defaults), G (tenant resolution), H (API versioning) |
| `Design-Infrastructure.md` | C (SecretConfigProvider), E (query profiler, asset versioning, queue audit), G (cache/file tenant scoping) |
| `Design-Query-Engine.md` | C (max page size, field access), E (depth limiting), G (tenant auto-filter) |
| `ai-integration.md` | H (governance infra, tap_ai_request, per-feature config) |
| `Analysis-Field-Access-Security.md` | C (implementation notes) |
| `Overview.md` | J (inclusivity-first positioning) |
| `Phases.md` | J (status update) |
| `Appendix-Deferred-Issues.md` | J (clear resolved, add new) |
| `Terminology.md` | J (new terms) |

**6 design docs modified by 3+ epics.** The cross-reference audit in Epic J is essential.

---

## New Taps (Hooks)

| Tap | Epic | Purpose |
|---|---|---|
| `tap_field_access` | C (12) | Per-field access control (view/edit, deny-wins, cached per role) |
| `tap_csp_alter` | C (12) | Plugins request CSP source additions (cannot weaken) |
| `tap_user_export` | D (13) | Data portability — plugins contribute to user data export |
| `tap_ai_request` | H (17) | Intercept/modify/block AI calls (deny-wins) |

---

## New Host Functions

| Function | Epic | Purpose |
|---|---|---|
| `crypto_hmac_sha256(key, msg)` | C (12) | HMAC-SHA256 for webhook signing, token validation |
| `crypto_sha256(data)` | C (12) | SHA-256 hashing |
| `crypto_random_bytes(len)` | C (12) | Cryptographically secure random bytes |
| `crypto_constant_time_eq(a, b)` | C (12) | Constant-time comparison for verification |
| `register_route_metadata(meta)` | H (17) | Plugin route documentation registration |

---

## New Middleware

| Middleware | Epic | Pipeline Position |
|---|---|---|
| CSP headers | C (12) | Response (after route handler) |
| Security headers (HSTS, X-Frame-Options, etc.) | C (12) | Response (after route handler) |
| Tenant resolution | G (16) | Request (after auth, before routes) |
| Query profiler (optional, feature-gated) | E (14) | Wraps DB queries |
| API version routing | H (17) | Request (routes /api/v{N}/) |

---

## Kernel-vs-Plugin Boundary Summary

**Kernel adds (infrastructure):**
- Schema: consent fields, personal_data flag, retention_days, change_summary, ai_generated, tenant_id, tenant table, user_tenant junction
- Middleware: CSP, security headers, tenant resolution, query profiler, API versioning
- Taps: field_access, csp_alter, user_export, ai_request
- Host functions: crypto primitives, route metadata registration
- Templates: skip link, `<article>` wrappers, `aria-describedby`, `dir="rtl"`, `loading="lazy"`
- Render pipeline: semantic fallback, asset hashing
- Config: SecretConfigProvider, CORS fix, per-AI-feature toggles, gather depth/page limits

**Plugins implement (features):**
- Cookie consent UI → reads kernel consent fields
- Data retention cron → queries kernel retention_days
- Diff/compare revision UI → reads kernel change_summary
- "Needs review" AI workflow → reads kernel ai_generated flag
- Webhook delivery → uses kernel crypto host functions + tap_item_* events
- OpenAPI generation → reads kernel route metadata
- AI governance policies → implements kernel tap_ai_request
- Tenant management UI → manages kernel tenant table
- Accessibility auditing → scans kernel semantic HTML output
- Full AI prompt/response logging → implements kernel tap_ai_request

This split maintains the governing principle: **the core kernel enables, plugins implement.**

---

## Risk Register

| Risk | Mitigation | Epic |
|---|---|---|
| Multi-tenancy migration on large databases could be slow | Run ALTER TABLE with DEFAULT (instant in PG 11+) | G |
| CSP breaks sites loading external resources | Deploy with CSP_REPORT_ONLY=true first | C |
| CORS default change breaks API consumers | Document in upgrade notes; env var override | C |
| SDK changes require plugin recompilation | All changes are backward-compatible serde defaults | All |
| Image block alt requirement breaks imports | Migration backfills `alt: ""`; importers add alt field | A |
| Revision immutability trigger blocks data fixes | Documented escape hatch: temporarily disable trigger | F |
| Tenant_id on every query adds overhead | Single-tenant sites skip tenant prefix (optimization) | G |
