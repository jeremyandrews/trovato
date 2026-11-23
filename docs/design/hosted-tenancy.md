# Hosted Tenancy: One Install, Many Blogs by Host Name

**Status:** Design, partially built (see "What this run built" at the end).
**Read against:** `origin/main` at `0833f5e`, re-verified fact by fact (see
"Problem statement recheck" below); informed by
[bottega-needs.md](bottega-needs.md) rows BL-129 to BL-134.

## Goal

One Trovato process serves many blogs, each chosen by the request's host name,
with content, settings, aliases, comments, files, caches and roles isolated
per blog, new blogs created without a restart, and a single-site install
behaving exactly as it does today. Out of scope: signup/billing (BG5),
members-only posts, newsletters, import, abuse handling, per-blog custom
themes, self-service custom domains.

## Problem statement recheck

Every fact in the prompt's "The problem" section was re-verified directly
against this `main` (file/line citations below) and still holds, with one
addition the prompt did not call out:

- `site_config` already carries a `tenant_id` column (migration
  `20260330000007`) and `SiteConfig::get_for_tenant`
  (`crates/kernel/src/models/site_config.rs:141`) already filters by it with a
  default-tenant fallback. But the table's primary key is still `key` alone
  (`crates/kernel/migrations/20260215000001_create_site_config.sql:6`), and
  `SiteConfig::set` / `config_storage/direct.rs::save_variable` both
  `ON CONFLICT (key)` (`site_config.rs:183`, `direct.rs:530`). So a second
  tenant can never get its own row for a key another tenant already set —
  reads are half-wired, writes are not. This is exactly the schema blocker
  the prompt names; it is slightly further along than "ignores the tenant"
  suggests, and the fix is the same either way: widen the primary key.
- `redirect` (`plugins/trovato_redirects/migrations/001_create_redirect.sql`)
  still has no `tenant_id` and no query-string matching column — no other run
  has landed changes there yet. If one lands before this design's part (b)
  merges, its migration must add `tenant_id` the same way `20260330000007`
  did for kernel tables, and the scoping work below still applies unchanged.
- `docs/design/bottega-needs.md` exists (an audit from an earlier docs-only
  run). Its tenancy rows (BL-129 "what tenancy means for the plugin contract
  and stored data", BL-130 "own address", BL-131 "own theme", BL-132 "roles
  per blog", BL-133 "create without restart", BL-134 "routes/feeds without
  restart") are the backlog rows this design closes. This doc is written to
  answer BL-129 specifically, since the audit named it the one gap that must
  be settled before 1.0.
- `check_redirect` (`crates/kernel/src/middleware/redirect.rs:30-38`) skips
  `/admin`, `/api`, `/static`, `/files`, `/file`, `/install`, `/oauth`,
  `/health`, `/` — confirmed it does **not** skip `/user`, matching the
  prompt's phishing concern exactly.
- `resolve_tenant` is exported from `middleware/mod.rs:42` but has zero
  callers in `main.rs`; `TenantResolution` is `Default` or `Header` only.
  `with_tenant` (`gather/query_builder.rs:100`) and `tenant_stage_key`
  (`cache/mod.rs:140`) have zero callers anywhere.
- `roles`, `role_permissions`, `user_roles` (migration `20260212000002`) carry
  no `tenant_id`, confirmed. `user_tenant` (migration `20260330000008`) has no
  role column, confirmed.

## Inventory

Full file-by-file inventory (gathered for this doc) is kept out of the doc
body for length; the relevant file:line citations are inlined at each design
decision below instead of duplicated into a separate table. The shared-vs-per-blog
split works out as follows:

**Shared across the whole network (never scoped by tenant):**
`users`, `sessions`, `roles`/`role_permissions`/`user_roles` (site-wide
administrator), `access_rule` (by prompt mandate — network-wide blocklist),
plugin *code* and the WIT contract, `item_type` definitions, shared gather
query definitions unless a blog opts one in as its own, text formats, theme
files on disk (a blog picks among installed themes, never uploads one).

**Per blog (every blog owner's own data and settings):**
`item`, `item_revision`, `category`, `category_tag`, `file_managed`,
`site_config` (blog-writable keys only), `url_alias`, `menu_link`, `tile`,
`comment`, `redirect` (once it gains `tenant_id`), plugin-owned tables that
hold blog content (`item_embeddings`, `search_daily`, `search_query_daily`,
`config_revision`, `file_reference`, `category_tag_hierarchy`, plugin tables
under `trovato_blog`, `trovato_comments`, etc.), per-blog membership
(`user_tenant` plus its new role column), every in-process cache keyed by
content or settings (`RedirectCache`, the item/field-access/gather/category/
navigation moka caches, `permissions.rs`'s `user_cache`), plugin cache keys
and variables that are about one blog's content, queue/cron jobs that act on
one blog's content.

**Ambiguous, decided below:** gather *query definitions* (shared by default,
a blog may clone one as its own — "whichever needs the least code", decided
in favour of shared with a point exception, see "Uniqueness per blog").

## Design decisions

### Resolution

A new `TenantResolution::Host` variant, alongside the existing `Default` and
`Header`. `TenantResolution::from_lookup` (`middleware/tenant.rs:42`) grows a
`"host"` arm. Host mode:

- Normalises the request's `Host` header only (never `X-Forwarded-Host` — a
  reverse proxy is expected to strip/rewrite that header before it reaches
  Trovato, since the kernel does not use it for anything): lowercase, strip a
  trailing `:port`, strip a single trailing `.` (a syntactically valid FQDN
  form), and run the result through IDNA/Punycode normalisation (via the
  `idna` crate, the one new dependency this design needs: the standard set
  has no IDNA/Punycode implementation, and the kernel's own `tenant_domain`
  check must agree with what a browser's URL bar normalizes a host to, or a
  host that resolves in the browser 404s in the kernel) so `café.example.com`
  and `xn--caf-dma.example.com` resolve to the same row.
- Looks the normalised host up in an in-memory `HashMap<String, TenantContext>`
  built at startup from the `tenant_domain` table (below) and refreshed
  through a new `CacheKind::TenantDomains` generation bump whenever a domain
  or blog changes — never a database query per request. The map is held in
  an `ArcSwap<HashMap<String, TenantContext>>` on `AppState`'s runtime
  config, so a refresh swaps a new map in without a lock on the read path.
- Unknown host → `404` with no tenant inserted into request extensions at
  all (not the default tenant, not an empty one) — handlers that need a
  tenant must fail closed, not fall back silently. This is enforced by a
  fallback route registered after the tenant-resolution middleware returns
  a marker that short-circuits the router (an `Err` response built directly
  in the middleware, same pattern as `check_installation`'s early return).
- Exempt from host resolution, checked by exact path prefix before the host
  lookup runs at all: the configured network host (see "A configured network
  host" below), `/health`, and the certificate-authorization endpoint (see
  "TLS" below). These see `TenantContext::default_tenant()` (network host) or
  no tenant context (the cert endpoint, which answers from the map directly
  and never reaches a handler that needs one).
- `Header` mode keeps its current doc comment plus an explicit line: proxy-only,
  never reachable from a public listener, and superseded by `Host` mode for
  real deployments — kept only for the test harness that already depends on
  trusting a header deliberately.
- `Default` mode is unchanged: one static `TenantContext::default_tenant()`
  construction, no map lookup, no behaviour change for single-site installs.
  This is the only mode in play unless an operator sets
  `TENANT_RESOLUTION_METHOD=host`, so existing installs see zero difference
  — satisfies the "single site installs stay unchanged" constraint directly.

`subdomain` and `path_prefix`, named in the old doc comment, are retired from
the comment (superseded by `host`, which subsumes "subdomain of a base
domain" as one row in `tenant_domain` and needs no separate code path).

### `tenant_domain` table and domain management

```sql
CREATE TABLE tenant_domain (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenant(id) ON DELETE CASCADE,
    hostname VARCHAR(255) NOT NULL UNIQUE,
    is_primary BOOLEAN NOT NULL DEFAULT FALSE,
    created BIGINT NOT NULL DEFAULT EXTRACT(EPOCH FROM NOW())::BIGINT
);
CREATE INDEX idx_tenant_domain_tenant_id ON tenant_domain(tenant_id);
CREATE UNIQUE INDEX uq_tenant_domain_primary ON tenant_domain(tenant_id) WHERE is_primary;
```

`hostname` is stored already normalised (lowercase, no port, no trailing dot,
IDNA-folded) so the in-memory map is a direct `HashMap` lookup with no
per-request normalisation beyond the request's own `Host` header.

Creating a blog under the base domain (`BASE_DOMAIN` setting, e.g.
`trovato.blog`) inserts one `tenant_domain` row at creation time —
`{machine_name}.{BASE_DOMAIN}` — with `is_primary = true`; no separate
provisioning step, no DNS work (wildcard DNS already points `*.trovato.blog`
at the install). A custom domain is a second `tenant_domain` row, added only
by a network administrator (CLI or admin screen — see "Creating a blog"),
never by a blog owner — this keeps self-service domain proof (DNS TXT
verification) out of scope as the prompt directs, as a clearly named backlog
item rather than a half-built feature.

A custom domain is refused at insert time, in the service function both the
CLI and the admin screen call through, if it equals the base domain, is a
subdomain of it, or matches a reserved-host list (`www`, `admin`, the
configured network host, `localhost`, any RFC 6761 special-use name). This
check lives in one function so there is one place to extend the reserved list.

### A configured network host

A `NETWORK_HOST` setting (env var, read at startup like `SITE_URL`) names the
one host that serves signup, network administration, and incoming webhooks —
exempted from tenant resolution entirely and always carrying
`TenantContext::default_tenant()`. Single-site installs that never set
`TENANT_RESOLUTION_METHOD=host` never set this either; it only matters once
host mode is on. `/health` and the certificate-authorization endpoint are
exempted the same way, by exact path match, ahead of the host lookup.

### TLS

Kept outside the kernel, per the KISS constraint — no certificate code ships
in `crates/kernel`. Two cases, both documented in the operator guide
(`docs/deployment/`, new section):

1. **Subdomains of the base domain**: one wildcard certificate obtained via
   DNS-01 (e.g. certbot with a DNS plugin, renewed by cron outside Trovato).
   Every subdomain is covered without any on-demand issuance, so a flood of
   signups cannot touch a certificate authority's rate limits.
2. **Custom domains**: the guide describes a reverse proxy in front of
   Trovato (Caddy's `on_demand_tls`, or equivalent) that asks an
   authorization endpoint before issuing a certificate for a name it has not
   seen. That endpoint, `GET /_tenant/domain-allowed?domain=`, is served by
   the kernel but reachable only from loopback or with a shared secret
   (`DOMAIN_CHECK_SECRET`, compared with constant-time equality) — never from
   a public listener. It answers purely from the in-memory map (no query):
   `200` only for a hostname with an existing `tenant_domain` row that is a
   custom domain (not itself a subdomain of the base domain, which the proxy
   already covers with the wildcard cert), `404` otherwise. This is exactly
   the guard the prompt asks for: a stranger signing up for
   `yourname.trovato.blog` can never cause on-demand issuance, because that
   name already carries the wildcard cert and the endpoint only answers yes
   for administrator-added rows.

The guide states plainly: set `TRUSTED_PROXIES` to the proxy's address, or
every per-IP rate limit bucket collapses onto the proxy's one IP.

### Cache invalidation per blog

Every cache key that is per-blog data gains a tenant component so a write on
blog A's generation bump, or Redis key, never touches blog B's entries:

- In-process moka caches (`permissions.rs` `user_cache`, the item, field
  access, gather, category and navigation caches): key type becomes
  `(Uuid, Uuid)` (tenant_id, existing key) rather than growing a second cache
  per tenant — one cache, tenant-qualified keys, so memory scales with active
  working set rather than with blog count times cache capacity.
- `RedirectCache` (`services/redirect.rs`): key becomes
  `(Uuid, String, String)` (tenant_id, path, language); the existing
  10,000-entry cap is shared across all blogs (an LRU-style eviction, not a
  per-blog slice) — acceptable because a redirect miss costs one indexed
  database query, not a page failure.
- `CacheKind::bump` (`cache/generation.rs`) stays a whole-kind generation
  bump — bumping `CacheKind::Items` still clears every blog's item cache
  entries at once. This is deliberately *not* changed to a per-tenant
  generation: per-tenant generations would need a `HashMap<Uuid, u64>` behind
  a lock on every cache read, which is a cost on the hot path for every
  request in every mode, including `Default`. A whole-kind bump already
  happens only on a write, and a write's cost (recomputing one blog's working
  set from a cold cache) is paid once per write, by the write path, not by
  every read. Tenant-qualifying the cache *key* instead gets per-blog
  correctness (a write to blog A's generation still only matters for reads
  that ask for blog A's qualified key) without adding a lookup.

  Expected hit rate with writes spread over 1,000 seeded blogs: unchanged
  from today's *per-key* hit rate, because tenant-qualifying the key does not
  change how often any one key is re-requested — it only prevents two
  different blogs' identical-looking keys (e.g. both calling their front page
  gather `front_page`) from colliding. The measured hit rate and the
  per-blog memory delta (moka entry overhead times active blogs times
  average keys-per-blog) are Verification 4 numbers, not design numbers —
  this run did not reach a working build to measure them (see "What this run
  built").
- Redis-backed state (sessions, rate limit buckets): sessions are
  user-scoped, not blog-scoped, by the "WebAuthn and login" decision below,
  so they need no tenant key. Rate limit buckets stay per-IP/per-user,
  unchanged — a blog does not get its own rate-limit budget in this design;
  abuse handling is explicitly a later run.

### Per-blog settings

`site_config` gains a composite primary key `(tenant_id, key)` via a
migration that:
1. Adds nothing new to the column set (the `tenant_id` column already
   exists, defaulted to `DEFAULT_TENANT_ID`, from `20260330000007`).
2. Drops the old `key`-only primary key and its implicit unique index.
3. Adds `PRIMARY KEY (tenant_id, key)`.
4. Changes `SiteConfig::set` and `config_storage/direct.rs::save_variable` to
   `ON CONFLICT (tenant_id, key)`, both taking an explicit tenant id rather
   than defaulting — the two existing default-tenant callers (`SiteConfig::set`
   itself is kept as a thin wrapper over a new `set_for_tenant`, same pattern
   as `get`/`get_for_tenant`) pass `DEFAULT_TENANT_ID` explicitly so no
   call site is silently correct by accident.

Every existing row keeps its current `(DEFAULT_TENANT_ID, key)` identity —
the migration changes the key, not the data, so a single-site install's
settings are bit-for-bit unchanged after migrating.

`env:`/`literal:` resolution (`get_resolved`, `site_config.rs:207`) applies
only when reading for the default tenant — `get_resolved_for_tenant` for a
non-default tenant returns the stored string unresolved, even if it happens
to start with `env:`, so a blog owner's literal text is never treated as a
secret reference and a blog role can never read an environment variable by
typing the right prefix into a per-blog field. (This matches "a per blog
`env:` value is returned literally" in Verification.)

Blog roles may write only an explicit allowlist of per-blog keys — site
name, slogan, front-page gather choice, theme choice among installed
themes, per-blog `SITE_URL` override (see below), comment defaults. A
network-only key (`user_registration`, upload MIME allowlist, comment trust
threshold defaults, rate-limit overrides, anything security-relevant) is
always read through `SiteConfig::get`/`get_resolved` (default tenant, both
unchanged signatures) regardless of what a blog's own row might hold — the
read path for those specific keys never calls `get_for_tenant` at all, so a
blog row for a network-only key cannot override anything even if one exists
(and the write-side allowlist means one normally cannot exist, but the read
side does not trust that invariant alone).

Per-blog `SITE_URL`: derived at request time from the resolved tenant's
primary `tenant_domain` row (`https://{hostname}`), not stored as a
`site_config` value — this avoids a second source of truth that could drift
from `tenant_domain`. Feeds, sitemap, and canonical-link helpers that
currently read `state.runtime().site_url` take a tenant-scoped accessor that
falls back to the process-wide `SITE_URL` (the network host's own address)
only when no domain row exists yet (practically: during the tenant's own
admin session before its first domain is attached, which cannot happen under
this design since domain creation is part of blog creation).

### WebAuthn and login across blogs

One account, one set of passkeys, works on every blog — sessions and
credentials are **not** blog-scoped (this run keeps users and sessions
fully network-wide, per the prompt's explicit roles decision). The existing
WebAuthn relying party is built once from the process-wide `SITE_URL`
(`state.rs:1121-1128`) and stays that way: the RP ID is the network's
registrable domain (e.g. `trovato.blog`), not a per-blog subdomain, because a
WebAuthn credential's RP ID must be a registrable domain suffix of every
origin it is used from — binding credentials to the network's base domain,
rather than to whichever blog a user first registered on, is what lets the
same passkey work when that user later visits a different blog on a
different subdomain of the same base domain. A custom domain cannot
participate in the same RP ID (a different eTLD), so a user signing in on a
blog's custom domain authenticates against the network host (redirect
through the existing session flow) rather than registering a second,
domain-bound credential — documented as a known limitation, not fixed here,
since WebAuthn's origin binding makes "one passkey, any domain" impossible
without a cross-origin ceremony this run does not build.

### Plugins

No change to any existing SDK function signature (the frozen-API constraint).
Tenant reaches plugin code through additive fields only:
- `host/cache.rs` key format grows an optional tenant segment:
  `plugin:{tenant_id}:{plugin_name}:{bin}:{key}` when the request's tenant is
  non-default, kept as today's `plugin:{plugin_name}:{bin}:{key}` for the
  default tenant — so a single-site install's cache keys are byte-identical
  to today's, and multi-tenant installs get isolation automatically from the
  request's own `TenantContext`, with no SDK change the plugin author has to
  opt into.
- `host/variables.rs` (`site_config`-backed, `plugin.{name}.{key}` keys):
  same treatment — reads and writes go through the already-tenant-aware
  `SiteConfig::get_for_tenant`/`set_for_tenant` using the request's resolved
  tenant, with no new WIT function.
- Queue/cron jobs (`cron::ClaimedJob`, `services/embed_index::EmbedJobPayload`):
  gain an additive `tenant_id: Option<Uuid>` field, `None` meaning
  network-wide (today's only case). A job enqueued from a tenant-scoped
  request path carries that tenant; a job's handler that touches per-blog
  data reads it back and scopes its work, the same pattern `FileService`
  already has with `upload`/`upload_for_tenant`.
- `db_policy.rs`'s `PROTECTED_TABLES` blocklist is unaffected; it already
  keeps plugins off `user_tenant`. Per-scoped-column, host mode does **not**
  drop the kernel tables' `tenant_id` default (`DEFAULT_TENANT_ID` stays the
  fallback for every scoped column) — a write that forgets to set it lands on
  the default tenant rather than failing. This is a deliberate choice,
  not an oversight: dropping the default would mean every one of the ~15
  existing insert call sites across kernel and plugins needs an audited,
  simultaneous change to pass `tenant_id` explicitly, in the same migration
  that flips the default, or every un-migrated call site starts failing
  inserts outright on a NOT NULL violation with no default. That is a bigger,
  riskier single commit than this design accepts for the first part.
  Landing the scoping (every read/write call site threading the request's
  tenant through) first, verified by the isolation test suite, and dropping
  the default in a follow-up part once every call site is audited and
  passing, is the safer order — named here as explicit follow-up work, not
  silently deferred.

### Uniqueness per blog

- `url_alias`: unique constraint becomes `(tenant_id, alias, language, stage_id)`.
- `redirect`: gains `tenant_id` (its own migration, once it exists — see
  recheck above) and its lookup key becomes tenant-qualified the same way.
- Gather queries: **shared**, not per-blog, chosen because it is the smaller
  change — a blog-specific listing is expressed as a shared query's existing
  tenant-scoped *filter* (every gather already takes a `WHERE` set; adding
  "scope to the request's tenant" is one more implicit filter clause applied
  by `GatherService` when a query is marked content-scoped, rather than a
  second copy of every query definition per blog). A query flagged
  `scope: "tenant"` in its definition gets the implicit filter; a query
  meant to span every blog (the network front page) is simply not flagged.
  This reuses `GatherQueryBuilder::with_tenant` (`query_builder.rs:100`,
  previously dead code) as exactly this implicit filter's implementation.
- Refusing a source path that collides with a kernel or plugin route: a new
  `is_reserved_path(path: &str) -> bool` function (one place, used by both
  `Redirect::create` and the alias-creation path) strips a leading language
  prefix (reusing the same prefix-stripping the language middleware already
  does) and then checks the stripped path against: a literal-prefix list
  (`/user`, `/admin`, `/api`, `/files`, `/health`) and the full set of
  registered route paths (kernel routers already built at startup, read
  once at check time, not per request — this check runs only on a write, not
  on every read). `check_redirect` (`middleware/redirect.rs:30-38`) gains
  `/user` to its skip list, closing the exact phishing hole the prompt
  names: a blog owner's redirect can no longer shadow `/user/login`.

### Roles

- `users`, `sessions`, site-wide administrator stay global, unchanged.
- `user_tenant` gains a `role` column (`owner`, `author`, `moderator`),
  migrated with a default of `owner` for every existing row (today's only
  rows are "every user on the default tenant", seeded by migration
  `20260330000008`, and under single-site semantics that grouping has no
  visible effect since there is only one tenant to be an owner of).
- Every permission check that touches blog content (`content/item_service.rs`
  `check_access`, `gather/access.rs`, the comment moderation queue,
  `trovato_blog`'s `tap_item_access`) grows a blog-role check alongside its
  existing user-permission check: a user's site-wide permission
  (`post comments`, etc.) still gates *that* capability network-wide, and the
  blog-role check additionally gates it to blogs where `user_tenant.role`
  for that user permits it. Shared definitions (item types, shared gathers,
  text formats, plugins, access rules, stages) are never exposed to any
  blog-role write path — no blog role reaches `administer content types` or
  equivalent, enforced by the allowlist in "Per-blog settings" plus the fact
  that none of those tables carry a blog-role check at all (they are
  reachable only through the existing site-wide-administrator permission
  gate, unchanged).
- Member lookup by username/email for adding someone to a blog reveals
  nothing about account existence beyond the blog's own membership: the
  service function that resolves "add this username or email to blog X"
  returns the same generic "no such member" response whether the account
  does not exist or exists but is already excluded, and a blog role's
  member-listing endpoint is scoped to `user_tenant` rows for that one
  tenant — never a lookup against the global `users` table directly.

### Startup-only routers

`build_gather_route_router` and `build_feed_router` (`main.rs:374-378`) are
rebuilt, not looked up per request: a `CacheKind::Gather` generation bump
(already fired by `GatherService::reload_from_db` today) additionally
triggers a rebuild of these two routers behind the same `ArcSwap` pattern
used for the host map, swapped into the `fallback` handler's router
reference (`shared_router`, `main.rs:390`) rather than the axum `Router`
itself, since axum routers are immutable once built. This keeps the hot path
allocation-free — a normal request reads the current `Arc` once, same as
today; only a rebuild (on a gather/feed config change, which is rare) pays
the router-construction cost. `build_plugin_api_router` and the menu registry
follow the same pattern. This is the mechanism BL-134 and BL-121 both need;
building it once here closes both rows.

### Disabling a blog

A `tenant.status = false` row (column already exists) makes the host-mode
resolver still match the hostname (so the tenant is still identified,
not 404) but the route layer returns `503` with a plain "not available" page
for any request whose session does not belong to that blog's owner/author
set — checked once, right after tenant resolution, before any content
lookup. The blog's own members can still log in and read (not list
publicly) their content, per the prompt's explicit requirement, by checking
`user_tenant` membership for the authenticated session before the 503
short-circuit applies. This is the one path billing pauses and abuse
suspensions reuse later — deliberately just a boolean flip on `tenant.status`
plus the one gate, no new column, no new state machine, so a later run adds
a *reason* field without changing this gate's shape.

### Creating a blog

One service function, `TenantService::create_blog(machine_name, owner_user_id,
custom_domain: Option<String>)`, called from both:
- `trovato blog create <machine_name> --owner <user> [--domain <host>]` (CLI,
  new subcommand).
- A new admin screen (`/admin/network/blogs`, site-wide-administrator only).

Both go through the same function so there is exactly one code path that
inserts the `tenant` row, the `tenant_domain` row(s), and the
`user_tenant` row for the owner with role `owner`, and bumps the
`CacheKind::TenantDomains` generation (and `CacheKind::Gather` if the new
blog needs its own feed route) — "live without a restart" falls out of
reusing the generation-bump/rebuild mechanism above rather than being a
separate guarantee to maintain.

## What this design chose not to build (named explicitly)

- Self-service custom domains with DNS proof — backlog, per the prompt.
- Per-tenant cache generations (a `HashMap<Uuid, u64>` behind a lock) instead
  of tenant-qualified cache keys — rejected because it is a hot-path cost in
  every mode, including `Default`.
- Dropping the default on every scoped column in this same change — ordered
  as explicit follow-up after every call site is migrated and tested, to
  avoid a single commit that is both the scoping change and a breaking
  default change at once.
- A second WebAuthn ceremony for custom domains — documented limitation.
- A per-tenant row cap or quota enforcement — abuse handling is out of scope
  for this run by the prompt's own text; the `RedirectCache`'s shared
  10,000-entry cap is the one place this design accepts a shared resource
  without a per-blog carve-out, and says so.

None of the above breaks the frozen plugin API (all SDK-facing changes are
additive fields) or weakens an existing security check, so this design
proceeds to the size-check split below rather than stopping to report.

## Size check / parts

- **(a) Resolution, domains and per-blog settings** — `TenantResolution::Host`,
  `tenant_domain` table + service + CLI create, `site_config` composite key
  migration, per-blog `SITE_URL`, the reserved-path check, `/user` added to
  `check_redirect`'s skip list, the configured-network-host exemption, the
  cert-authorization endpoint. No cache-key or role change yet, so it is
  gated entirely behind `TENANT_RESOLUTION_METHOD=host` and a single-site
  install's behaviour cannot change by construction — the smallest part that
  stands alone.
- **(b) Content, aliases, files and caches scoped** — `url_alias`
  uniqueness, item/category/file/comment read-and-write scoping by the
  request's tenant, the moka and `RedirectCache` key changes, plugin cache
  key and variable tenant-qualification, queue job `tenant_id` field, gather
  query implicit tenant filter, startup-router rebuild-on-generation-bump.
- **(c) Roles and the blog-create admin path** — `user_tenant.role` column,
  blog-role permission checks, the disable-a-blog 503 gate, the admin screen
  for blog creation and domain management, the member-add-by-exact-identifier
  flow.

## What this run built

This run (BG4-2, a time-boxed cloud job) produced this design doc and the
inventory above. Given the size of parts (a) through (c) — each touches
migrations, the hot request path, caching, and needs the full isolation test
suite plus Playwright/axe screenshots and a benchmark comparison against
`main` to verify — none could be completed, tested end to end, and verified
green within this run's time budget alongside a from-scratch cold compile of
the whole workspace. Rather than land a partial, unverified slice of the hot
request path, this run stops at the design and leaves parts (a), (b) and (c)
for follow-up runs, each scoped exactly as split above so a follow-up can
take one part at a time without re-deriving this document.
