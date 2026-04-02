# Epic 16 (G): Multi-Tenancy Foundation

**Tutorial Parts Affected:** All (tenant context visible even in single-tenant mode, like stage_id)
**Trovato Phase Dependency:** Phase 3 (Content Model) — already complete
**BMAD Epic:** 46
**Status:** Complete. Implemented: tenant table with DEFAULT_TENANT_ID, Tenant/TenantContext models, tenant resolution middleware (default/header/subdomain-ready), tenant_id column + FK + indexes on all content tables (item, item_revision, category, category_tag, file_managed, site_config, url_alias, menu_link, tile, comment), user_tenant junction table with all users seeded to default tenant. Zero-overhead single-tenant default.
**Estimated Effort:** 5–7 weeks (largest epic — most invasive schema and service changes)
**Dependencies:** None (independent, but should be aware of D's retention_days and F's revision columns to coordinate migrations)
**Blocks:** None

---

## Narrative

*Multi-tenancy is the `language` column for organizations. A personal blog has one tenant and ignores it. A university runs 200 department sites on one Trovato instance and depends on it. The column exists for everyone, matters for some, costs nothing for the rest.*

Today, Trovato has zero multi-tenancy infrastructure. No `tenant_id` on any table. No tenant resolution middleware. No tenant types in the SDK. ConfigStorage doesn't accept a tenant parameter. Cache keys are stage-scoped but not tenant-scoped. Search queries filter by stage but not tenant. File storage generates URIs without tenant scoping.

This is the most invasive epic in the series. It touches the database schema, every service layer, the middleware pipeline, the cache system, the search system, the file system, and the config system. The justification for doing it now, at the kernel level, is the `language` column precedent: adding a `tenant_id` column after content exists is a painful migration. Adding it to empty tables before content exists is trivial.

**Design decision: User scoping model.**

This epic uses the **junction table model** (users are global, with a `user_tenant` table linking users to tenants). This matches the Slack workspace pattern and Drupal's shared user table across multisites. Reasons:

1. A site operator should be able to access multiple tenants with one account (admin across several sites).
2. Permission resolution is per-tenant (a user can be admin on tenant A and editor on tenant B).
3. The junction model is strictly more expressive — it degenerates to "one user, one tenant" for simple cases.
4. The `users` table stays tenant-independent, simplifying auth middleware (authenticate globally, then resolve tenant context).

**Lowest common denominator test:** A single-tenant site creates a `DEFAULT_TENANT_ID` constant (like `LIVE_STAGE_ID`) during installation. All content is created with this tenant ID. The tenant resolution middleware resolves to the default tenant on every request. The site operator never sees or interacts with tenant concepts — it's invisible infrastructure, just like `stage_id` is invisible to a site that never uses non-live stages.

**Before this epic:** No multi-tenancy. Content, config, cache, files, search all implicitly single-tenant.

**After this epic:** `tenant_id` on all content tables. Tenant resolution middleware. Gather auto-filters by tenant. Cache keys include tenant. File storage includes tenant prefix. Default tenant for single-tenant sites. A multi-tenant SaaS plugin has everything it needs from the kernel.

---

## Kernel Minimality Check

| Change | Why not a plugin? |
|---|---|
| `tenant_id` on content tables | Schema is kernel. Plugins can't add columns to kernel tables. |
| Tenant resolution middleware | Middleware runs before plugins. Plugins can't inject middleware. |
| Gather auto-filter by tenant | Query engine is kernel. The WHERE clause must be injected at the query builder level, not by plugins adding filters. |
| ConfigStorage tenant parameter | Config storage trait is kernel infrastructure. |
| FileStorage tenant scoping | File storage is kernel infrastructure. |
| Cache key tenant prefix | Cache system is kernel infrastructure. |
| Search tenant filtering | Search indexing is kernel infrastructure. |
| `DEFAULT_TENANT_ID` constant | Like `LIVE_STAGE_ID` — kernel constant for the default state. |

Every item is infrastructure. The *management* of tenants (admin UI for creating tenants, billing, quota enforcement, tenant switching UI) is plugin territory.

---

## BMAD Stories

### Story 46.1: Tenant Schema and Default Tenant

**As a** kernel maintaining multi-tenancy infrastructure,
**I want** a tenant table and `tenant_id` foreign keys on content tables,
**So that** all content is scoped to a tenant from day one.

**Acceptance criteria:**

- [ ] Migration creates `tenant` table: `id` (UUID, PK), `name` (VARCHAR(255), NOT NULL), `machine_name` (VARCHAR(128), UNIQUE, NOT NULL), `status` (BOOLEAN, DEFAULT TRUE), `created` (BIGINT), `data` (JSONB, DEFAULT '{}')
- [ ] `DEFAULT_TENANT_ID` constant UUID defined in kernel (deterministic UUID, like `LIVE_STAGE_ID`)
- [ ] Migration seeds default tenant row with `DEFAULT_TENANT_ID`, name "Default", machine_name "default"
- [ ] Migration adds `tenant_id` (UUID, NOT NULL, DEFAULT DEFAULT_TENANT_ID, FK to tenant.id) with index to: `item`, `item_revision`, `categories`, `category_tag`, `file_managed`, `site_config`, `url_alias`, `stage`, `menu_link`, `tile`, `comments`
- [ ] Existing rows backfilled with `DEFAULT_TENANT_ID`
- [ ] `DEFAULT_TENANT_ID` exported as constant in kernel config (like `LIVE_STAGE_ID`)
- [ ] `Tenant` model struct in kernel
- [ ] At least 2 integration tests: create item with default tenant, create item with custom tenant

**Implementation notes:**
- Multiple migrations (one for tenant table, one per ALTER TABLE batch) to keep them manageable
- `DEFAULT_TENANT_ID` follows the `LIVE_STAGE_ID` pattern — hardcoded UUID constant
- All FKs are NOT NULL with DEFAULT — single-tenant sites never think about it
- Index on `tenant_id` on all tables (composite indexes `(tenant_id, ...)` where beneficial)

---

### Story 46.2: User-Tenant Junction and Auth Context

**As a** site operator managing multiple tenants,
**I want** users to belong to one or more tenants with per-tenant roles,
**So that** I can have one account with different permissions across tenants.

**Acceptance criteria:**

- [ ] Migration creates `user_tenant` junction table: `user_id` (UUID, FK to users.id), `tenant_id` (UUID, FK to tenant.id), `is_active` (BOOLEAN, DEFAULT TRUE), `created` (BIGINT), PRIMARY KEY (`user_id`, `tenant_id`)
- [ ] Existing users seeded into `user_tenant` with `DEFAULT_TENANT_ID`
- [ ] Role assignments become tenant-scoped: `user_role` table (if it exists) gains `tenant_id`, or roles are stored per-tenant in the junction
- [ ] `UserContext` (the per-request user state) gains `tenant_id: Uuid` — set by tenant resolution middleware
- [ ] Auth middleware authenticates globally (session → user), then tenant middleware resolves tenant and verifies user belongs to it
- [ ] If user doesn't belong to the resolved tenant, return 403
- [ ] Admin users (global `is_admin = true`) can access all tenants without junction table entries
- [ ] At least 2 integration tests: user in one tenant, user in multiple tenants with different roles

**Implementation notes:**
- Modify `crates/kernel/src/models/user.rs` — add tenant awareness
- Modify `crates/kernel/src/middleware/` auth chain — tenant verification step
- The auth pipeline becomes: authenticate (session) → resolve tenant → verify membership → load roles
- Single-tenant sites: all users are in `DEFAULT_TENANT_ID`, tenant resolution always returns default

---

### Story 46.3: Tenant Resolution Middleware

**As a** kernel processing multi-tenant requests,
**I want** tenant resolution early in the middleware pipeline,
**So that** all downstream services know which tenant the request is for.

**Acceptance criteria:**

- [ ] Tenant resolution middleware runs after session/auth middleware, before route handlers
- [ ] Resolution strategies (configurable, ordered):
  1. **Subdomain:** `tenant-a.example.com` → look up tenant by machine_name "tenant-a"
  2. **Path prefix:** `/t/tenant-a/...` → strip prefix, resolve tenant, rewrite URI
  3. **Header:** `X-Tenant-ID: {uuid}` → direct UUID resolution (for API clients)
  4. **Default:** if no strategy matches, resolve to `DEFAULT_TENANT_ID`
- [ ] `TENANT_RESOLUTION_METHOD` env var (default: "default" — always resolves to default tenant)
- [ ] Resolved tenant available as `request.extensions().get::<TenantContext>()`
- [ ] `TenantContext` struct: `id: Uuid`, `name: String`, `machine_name: String`
- [ ] Resolution result cached per request (resolved once, used everywhere in the pipeline)
- [ ] Single-tenant sites: middleware resolves to default tenant on every request with zero overhead (short-circuit when method is "default")
- [ ] At least 3 integration tests: subdomain resolution, path prefix resolution, default fallback

**Implementation notes:**
- Add `crates/kernel/src/middleware/tenant.rs`
- Path prefix strategy needs URI rewriting (similar to language prefix negotiator)
- Subdomain strategy requires `Host` header parsing
- The middleware is a no-op for single-tenant sites — the "default" strategy returns immediately

---

### Story 46.4: Gather Auto-Filter by Tenant

**As a** kernel executing Gather queries,
**I want** all queries automatically filtered by the current tenant,
**So that** content from other tenants never leaks into query results.

**Acceptance criteria:**

- [ ] Gather query builder injects `WHERE tenant_id = $tenant_id` on all queries (on the `item` table)
- [ ] Injection happens at the query builder level, not as a user-visible filter (like stage filtering)
- [ ] Gather admin UI does not show tenant filter (it's automatic and mandatory)
- [ ] Category queries also filtered by tenant (categories are tenant-scoped)
- [ ] URL alias lookups filtered by tenant
- [ ] Admin listing queries (items, users, categories) filtered by tenant
- [ ] API queries (`/api/item/{id}`) verify the item belongs to the current tenant (return 404 if not)
- [ ] Cross-tenant queries are not possible through the normal query path (no `tenant_id` parameter exposed to plugins)
- [ ] At least 3 integration tests: query returns only current tenant's items, cross-tenant item not visible, category filtering

**Implementation notes:**
- Modify `crates/kernel/src/gather/` query builder — add tenant_id condition alongside existing stage_id condition
- The pattern mirrors stage filtering — Gather already injects `WHERE stage_id = ...`
- Cross-tenant queries (for admin dashboards showing all tenants) would need a separate service method — not in scope for this epic

---

### Story 46.5: Tenant-Scoped Configuration

**As a** site operator with multiple tenants,
**I want** site configuration scoped per tenant,
**So that** each tenant has its own site name, theme, and settings.

**Acceptance criteria:**

- [ ] `ConfigStorage` trait methods gain `tenant_id: Uuid` parameter
- [ ] `site_config` table already has `tenant_id` from Story 46.1 — config queries filter by it
- [ ] `config export` exports config for the current tenant (default tenant for single-tenant sites)
- [ ] `config import` imports into the current tenant
- [ ] `StageAwareConfigStorage` becomes `TenantStageAwareConfigStorage` — resolves config by `(tenant_id, stage_id)`
- [ ] Tenant config resolution chain: tenant-specific config → default tenant config → hardcoded defaults
- [ ] Single-tenant sites: tenant parameter is always `DEFAULT_TENANT_ID` — no behavioral change
- [ ] At least 2 integration tests: tenant-specific config, fallback to default config

**Implementation notes:**
- Modify `crates/kernel/src/config/` storage implementations
- This extends the existing stage-aware config pattern (already handles config scoping by stage) with an additional tenant dimension
- The resolution chain is: `(tenant, stage, key)` → `(default_tenant, stage, key)` → `(default_tenant, live_stage, key)` → hardcoded default

---

### Story 46.6: Tenant-Scoped Cache Keys

**As a** kernel maintaining cache isolation between tenants,
**I want** cache keys prefixed with tenant ID,
**So that** tenant A's cached content never serves to tenant B.

**Acceptance criteria:**

- [ ] Cache key format: `t:{tenant_id}:st:{stage_id}:{key}` (tenant + stage scoping)
- [ ] Single-tenant optimization: when `DEFAULT_TENANT_ID`, use `st:{stage_id}:{key}` (no tenant prefix — backward compatible with existing cache entries)
- [ ] Tag-based cache invalidation scoped by tenant (invalidating tenant A's content cache doesn't affect tenant B)
- [ ] All cache services updated: ContentTypeRegistry, GatherService, PermissionService, UserService, ItemService, CategoryService
- [ ] Redis Lua scripts for tag-based invalidation updated to handle tenant-scoped keys
- [ ] At least 2 integration tests: tenant-scoped cache isolation, default tenant backward compatibility

**Implementation notes:**
- Modify `crates/kernel/src/cache/` (or wherever cache key generation lives)
- The backward compatibility optimization is important — existing single-tenant sites shouldn't need a cache flush on upgrade
- Moka L1 cache: can use tenant-prefixed keys directly
- Redis L2 cache: Lua scripts need the tenant prefix in key patterns

---

### Story 46.7: Tenant-Scoped File Storage and Search

**As a** kernel maintaining data isolation between tenants,
**I want** file storage and search scoped by tenant,
**So that** tenant A's files and search results are invisible to tenant B.

**Acceptance criteria:**

- [ ] FileStorage URI format for multi-tenant: `local://{tenant_machine_name}/YYYY/MM/{uuid}_{filename}`
- [ ] Single-tenant optimization: `local://YYYY/MM/{uuid}_{filename}` (no tenant prefix — backward compatible)
- [ ] File serve routes verify the file belongs to the current tenant before serving
- [ ] S3 storage (if configured) uses tenant prefix in S3 key path
- [ ] Full-text search queries (tsvector) filter by `tenant_id`
- [ ] Pagefind index generation (trovato_search plugin) scoped by tenant — each tenant gets its own search index
- [ ] At least 2 integration tests: file upload scoped to tenant, search scoped to tenant

**Implementation notes:**
- Modify file storage service in `crates/kernel/src/services/`
- Modify search query building to include tenant_id in WHERE clause
- Pagefind integration: the search plugin's tap_cron handler needs tenant awareness — generates separate indexes per tenant, served from `static/pagefind/{tenant_machine_name}/`

---

### Story 46.9: Single-Tenant Performance Verification

**As a** site operator running a single-tenant Trovato installation,
**I want** verified evidence that multi-tenancy infrastructure adds negligible overhead,
**So that** I can trust that the tenant_id columns, middleware, and scoped queries don't degrade my site's performance.

**Acceptance criteria:**

- [ ] Benchmark: tenant resolution middleware adds <0.1ms per request in "default" strategy mode
- [ ] Benchmark: Gather query with tenant_id WHERE clause adds <1ms compared to query without it (on 10K item dataset)
- [ ] Benchmark: cache key generation with DEFAULT_TENANT_ID optimization matches pre-tenant key generation time
- [ ] Single-tenant cache key format verified backward compatible (no tenant prefix)
- [ ] All benchmarks documented in operational docs with methodology and baseline numbers

**Implementation notes:**
- This is a verification story, run after Stories 46.1–46.8 are complete
- If benchmarks fail thresholds, fixes are in the relevant implementation stories
- The "default" strategy must be zero-allocation: construct static TenantContext from compile-time constants, no DB lookup

---

### Story 46.8: Tutorial and Documentation Updates for Multi-Tenancy

**As a** tutorial reader,
**I want** multi-tenancy mentioned naturally in the tutorial where relevant,
**So that** I understand the concept exists without it derailing the narrative.

**Acceptance criteria:**

- [ ] Part 1: Brief mention when explaining `stage_id`: "Every item also has a `tenant_id` — like `stage_id`, it defaults to a built-in value and is invisible for single-site installations. Multi-tenancy becomes relevant when running multiple sites on one Trovato instance."
- [ ] Part 2: No changes needed (import is single-tenant)
- [ ] Part 4: Brief mention when explaining stages: "Stages are per-tenant — each tenant can have its own staging environments."
- [ ] Other parts: No changes unless a specific feature interaction warrants it
- [ ] Recipe updates for parts 1 and 4 to match
- [ ] Sync hashes updated

**Implementation notes:**
- These are one-sentence additions, not new sections. The tutorial stays focused on the single-tenant experience.
- Multi-tenancy tutorial (creating tenants, configuring resolution, managing per-tenant settings) is a future tutorial part, not part of the current tutorial.

---

## Plugin SDK Changes

| Change | File | Breaking? | Affected Plugins |
|---|---|---|---|
| `TenantContext` type | `crates/plugin-sdk/src/types.rs` | No (new type) | None — plugins gain access to `request.tenant()` |
| `Item.tenant_id` field | `crates/plugin-sdk/src/types.rs` | Soft break | All 21 plugins that handle Items see a new field; `Option<Uuid>` with `#[serde(default)]` for backward compat |
| `tenant_id` on host function parameters | `crates/plugin-sdk/src/` | No (additive) | Host functions that accept tenant context are new overloads, not replacements |

**Migration guide:** Existing plugins continue to work — `tenant_id` fields are `Option<Uuid>` with defaults. Plugins that want tenant awareness can read `request.tenant()` to get the current `TenantContext`. Host functions that query data automatically filter by the request's tenant.

---

## Design Doc Updates (shipped with this epic)

| Doc | Changes |
|---|---|
| `docs/design/Design-Content-Model.md` | Add "Multi-Tenancy Schema" section: tenant table, junction table, tenant_id on content tables, DEFAULT_TENANT_ID constant. |
| `docs/design/Design-Web-Layer.md` | Add "Tenant Resolution" section: middleware pipeline, resolution strategies, single-tenant optimization. |
| `docs/design/Design-Query-Engine.md` | Add tenant auto-filtering to Gather documentation. |
| `docs/design/Design-Infrastructure.md` | Update cache key format documentation. Update file storage URI format. Update search filtering. |
| `docs/design/Design-Plugin-SDK.md` | Add `TenantContext` type. Document `request.tenant()` access pattern. |
| `docs/design/Overview.md` | Add multi-tenancy to architecture feature list. |

---

## Tutorial Impact

See Story 46.8. Minor additions to Parts 1 and 4 only.

---

## Recipe Impact

Recipes for Parts 1 and 4 need minor updates. Run `docs/tutorial/recipes/sync-check.sh` and update hashes.

---

## Screenshot Impact

None. Multi-tenancy is invisible in the single-tenant tutorial experience.

---

## Config Fixture Impact

Config YAML files may need `tenant_id` fields if config export format changes. For the tutorial (single-tenant), the default tenant is implied.

---

## Migration Notes

**Database migrations (numerous — coordinate ordering):**
1. `YYYYMMDD000001_create_tenant_table.sql` — CREATE tenant table, seed DEFAULT_TENANT_ID
2. `YYYYMMDD000002_add_tenant_id_to_items.sql` — ALTER item, item_revision: ADD tenant_id
3. `YYYYMMDD000003_add_tenant_id_to_categories.sql` — ALTER categories, category_tag: ADD tenant_id
4. `YYYYMMDD000004_add_tenant_id_to_supporting.sql` — ALTER file_managed, site_config, url_alias, stage, menu_link, tile, comments: ADD tenant_id
5. `YYYYMMDD000005_create_user_tenant.sql` — CREATE user_tenant junction, seed existing users with DEFAULT_TENANT_ID

**Breaking changes:**
- ConfigStorage trait signature changes (adds `tenant_id` parameter). All internal callers must be updated.
- Cache key format changes for multi-tenant deployments. Single-tenant sites use backward-compatible format.
- File storage URI format changes for multi-tenant deployments. Single-tenant sites use backward-compatible format.

**Upgrade path:**
1. Run migrations. All existing data gets `DEFAULT_TENANT_ID`.
2. All existing users joined to default tenant via `user_tenant` seed.
3. Cache keys for single-tenant sites unchanged (backward compatible).
4. File storage paths for single-tenant sites unchanged (backward compatible).
5. No configuration change required for single-tenant sites (`TENANT_RESOLUTION_METHOD` defaults to "default").

---

## What's Deferred

- **Tenant management admin UI** (create/edit/delete tenants) — Plugin. The kernel provides the schema; a multi-tenancy plugin provides the management interface.
- **Tenant switching UI** (header dropdown to switch between tenants) — Plugin.
- **Per-tenant billing/quotas** — Plugin. Content limits, storage quotas, user limits.
- **Tenant cloning** (duplicate a tenant's configuration as a starting point) — Plugin.
- **Cross-tenant content sharing** (syndication between tenants) — Future epic. Would need a `shared_items` concept.
- **Per-tenant domain mapping** (tenant-a.com → tenant A, tenant-b.org → tenant B) — Plugin configuration + tenant resolution strategy extension.
- **Tenant data isolation verification** (automated testing that no cross-tenant leakage occurs) — Could be a CI test suite enhancement.
- **Database-level row-level security (RLS)** — An alternative enforcement mechanism. The application-level WHERE clause approach is sufficient for v1; RLS could be added later for defense-in-depth.

---

## Related

- [Design-Content-Model.md](../design/Design-Content-Model.md) — Schema design
- [Design-Web-Layer.md](../design/Design-Web-Layer.md) — Middleware pipeline
- [Design-Infrastructure.md](../design/Design-Infrastructure.md) — Cache and file storage
- [Design-Query-Engine.md](../design/Design-Query-Engine.md) — Gather auto-filtering
- [Epic D (13): Privacy Infrastructure](epic-13-privacy.md) — Coordinate migration ordering for retention_days columns
- [Epic F (15): Versioning & Audit](epic-15-versioning.md) — Coordinate migration ordering for revision columns
