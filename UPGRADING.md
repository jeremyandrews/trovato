# Upgrading

What an operator has to know or do before moving a running site to a newer
version. Only versions that need something are listed; a version that is not
here needs nothing beyond the ordinary upgrade.

Each entry says what changed, who it affects, and how to find out whether that
is you **before** you upgrade.

## Upgrading with no downtime, and rolling back

These hold for every release from the one that added `trovato migrate` on.
The design is [docs/design/upgrades-and-probes.md](docs/design/upgrades-and-probes.md);
the step by step procedure is [docs/deployment/upgrades.md](docs/deployment/upgrades.md).

### Expand, then contract

**Every migration leaves the previous release working.** While the migration
step runs, the previous release is still serving. After it, the previous
release is what a rollback redeploys. A migration that breaks it turns an
upgrade into an outage and a rollback into a restore from backup.

Fine at any time (expand):

- a new table, view or function;
- a new column that is nullable or has a `DEFAULT`;
- a non-unique index on an existing table built `CONCURRENTLY`, as the only
  statement of a kernel migration whose first line is `-- no-transaction`
  (Postgres runs a file of several statements as one transaction block, where
  `CONCURRENTLY` fails), and without `IF NOT EXISTS`. A failed or cancelled
  build leaves an INVALID index under its name; the retry then fails on it
  loudly, and the fix is `DROP INDEX CONCURRENTLY <name>;` by hand, then
  `trovato migrate` again. (Plugin migrations always run in one transaction,
  so a plugin index on an existing table needs a contract marker. A `UNIQUE`
  index on an existing table is a new constraint and needs one too.)
- inserting or updating rows the previous release reads the same way.

Only in a later release, after no released code reads or writes the object
(contract): dropping a table, column, view, function, type, index or constraint
(the previous release's `ON CONFLICT` writes need its unique ones); renaming a table
or column; changing a column's type; `SET NOT NULL`; adding a `NOT NULL` column
without a `DEFAULT`; removing a column's `DEFAULT`; adding a constraint;
`TRUNCATE`; a plain or unique `CREATE INDEX` on a table that already exists. A
`DROP INDEX CONCURRENTLY`, like a concurrent build, needs a `-- no-transaction`
kernel file of its own.

A rename takes three releases:

```sql
-- 0.108.0: add the new column; 0.108.0's code writes both and reads the old one.
ALTER TABLE item ADD COLUMN summary TEXT;
UPDATE item SET summary = teaser WHERE summary IS NULL;   -- if it is small; see below

-- 0.109.0's code reads `summary` only. No migration.

-- 0.110.0: drop the old column, naming the release that stopped using it.
-- trovato:contract 0.109.0: item.teaser unread since 0.109.0
ALTER TABLE item DROP COLUMN teaser;
```

A `NOT NULL` column: add it with a `DEFAULT` (fast on Postgres 11 and later),
or add it nullable, backfill, and add the constraint in a later release with a
marker.

**Backfills.** A migration that rewrites rows runs while the site serves.
Keep it to about one second of row locks on any table the site writes,
measured on the largest install. Anything bigger goes in batches of at most
1000 rows, each its own short transaction, from cron or the queue. Add the
column in one release, backfill, then constrain or drop in a later one.

**The rule is checked.** `cargo test -p trovato-kernel --lib migration_rule`
(run by `./scripts/pre-commit-check.sh` and CI) refuses a contract statement in
a new kernel or plugin migration unless the comment lines at the top of the
file carry `-- trovato:contract <release>: <reason>`, naming a release older
than the one being built (the checker cannot see tags; whether that release really stopped
using the object is the reviewer's question).

**When a release cannot keep the previous one working**, name the migration
`<version>_incompatible_<description>.sql`. Older binaries refuse to start on a
schema that holds it (with `MIGRATE_ON_START=false`, to become ready), so that
release needs planned downtime and
cannot be rolled back by image. Its `UPGRADING.md` entry must say so.

### Rolling back

**Redeploy the previous image or binary. Never roll back the schema.** There
are no down migrations. The previous release runs on the newer schema: it
accepts migrations it does not know unless one is marked incompatible.

- Kubernetes: `kubectl rollout undo deployment/trovato`, then
  `kubectl rollout status deployment/trovato`.
- One server: reinstall the previous binary and restart it. With the default
  `MIGRATE_ON_START=true` it finds nothing to apply and starts.

When the newer release is fixed, upgrade again. Its migrations are already
applied, so `trovato migrate` finds nothing to do.

## Unreleased

### Migrations can run as their own step; `/health` answers anonymous callers with its status only

**Who is affected:** anyone running more than one server, anyone running on
Kubernetes, and anyone reading `pool` or `services` from `/health`. A single
server with the default settings migrates and starts exactly as before.

**What changed.**

- `trovato migrate` applies pending kernel and plugin migrations under one
  Postgres advisory lock; `--status` and `--dry-run` change nothing. Plugin
  migrations, which took no lock before, now take the same one wherever they
  run.
- `MIGRATE_ON_START` (default `true`). Set to `false`, `serve` never changes
  the schema: it waits, alive and not ready, until `trovato migrate` has run.
  The `plugin`, `config` and `user` commands refuse to run on a kernel schema
  that is behind; once it is current, `plugin install`, `plugin migrate` and
  `config import` apply the plugin migrations they need, under the lock.
- An applied migration the binary does not know is accepted unless its name
  marks it incompatible, where it used to stop the server at boot. This is
  what makes a rollback by image work.
- `/health/live` and `/health/ready` are new. `/health` keeps its status code
  and `status` field. Its `postgres`, `redis`, `pool` and `services` fields now
  go only to a caller on loopback with no `X-Forwarded-For`, `Forwarded` or
  `X-Real-IP` header, or to one sending `Authorization: Bearer <HEALTH_TOKEN>`.
  Probes no longer count against the visitor rate limits.
- On SIGTERM the server reports not ready at once and keeps serving for
  `SHUTDOWN_DRAIN_SECS`. That is 5 under Kubernetes and 0 elsewhere, so a
  single server stops as fast as before.
- The image's `HEALTHCHECK` uses `/health/live`.

**How to find out.** If a monitor reads `pool` or `services` from `/health`
from another host, set `HEALTH_TOKEN` and send it as a bearer token. A probe
that only reads the status code needs nothing.

### One path per JSON capability; the item API takes `access content`

**Who is affected:** API clients of `/api/v1/user/export`, `/admin/queue/dlq`
or the item list; anyone relying on anonymous or role-less item reads.

**What changed.**

- `GET /api/v1/user/export` is removed. Use `GET /user/data-export`, the
  signed-in account's own export (once an hour).
- The dead-letter queue endpoints move from `/admin/queue/dlq...` to
  `/api/v1/queue/dlq...`, require `administer site`, and the list answers a
  page (`data`, `total`, `page`, `per_page`) instead of `{count, jobs}`.
- `/api/v1/items`, `/api/v1/items/{type}` and `/api/v1/item/{id}` require
  `access content`. For anyone but the site administrator the lists hold
  published items only, whatever `status` asks for.
- A title over 1000 bytes is refused on the item write paths, and a search or
  autocomplete `q` over 1000 bytes with 400.
- The item edit forms take the content lock while `trovato_content_locking` is
  enabled; a save under another editor's lock answers 409.

**How to find out.** Search clients for `user/export` and `admin/queue`, and
check that the role your API token's account holds grants `access content`.

### The JSON API is under `/api/v1`, and every list is `{data, total, page, per_page}`

**Who is affected:** any client of the kernel's JSON API or the MCP server: a
headless front end, a script, an integration. Not plugin routes, which keep
their paths, and not the site's own pages and scripts, which are updated.

**What changed.**

- Every kernel JSON endpoint is served under `/api/v1`: `/api/items` is
  `/api/v1/items`, `/api/tokens` is `/api/v1/tokens`, and so on, the rest of
  the path unchanged. The old unversioned paths still answer, from the same
  handlers, with a `Deprecation: true` header; they are removed at 2.0. The
  OpenAPI document is at `/api/v1/openapi.json` (the old path is an alias).
- `/api/search` and `/api/v1/search` are one search. It takes `q`, `page` and
  `per_page` (`limit` is an alias), searches the caller's stages as
  `/api/search` did, and answers `id`, `type`, `title`, `rank`, `snippet`,
  `url` per result.
- Every list answers `{"data": [...], "total": n, "page": p, "per_page": k}`
  and takes `page` and `per_page`, with `limit` and `offset` as aliases. That
  changes the body of:
  - `/api/v1/items`: `items` → `data`; `pagination.{total,page,per_page}` move
    to the top level; `total_pages` is gone (`ceil(total / per_page)`).
  - `/api/v1/search`: `results` → `data`; `limit`, `query`, `total_pages` gone.
  - `/api/v1/item/{id}/comments`: `comments` → `data`.
  - `/api/v1/query/{id}/execute`, `/api/v1/gather/query`: `items` → `data`;
    `total_pages`, `has_next`, `has_prev` gone.
  - `/api/v1/media/browse`: `items` → `data`, `page_size` → `per_page` (in
    the request too).
  - Bare arrays become pages: `/api/v1/items/autocomplete`,
    `/api/v1/items/{type}`, `/api/v1/content-types`, `/api/v1/queries`,
    `/api/v1/tokens`, `/api/v1/categories` and every tag list. A list that
    returned everything now returns its first page: 20 items of a type, 100
    categories, tags, gathers, tokens or content types, 50 comments; ask for
    more with `per_page` (up to each list's maximum) or `page`.
  - MCP: `list_items`, `search`, `run_gather`, `list_categories`, `list_tags`
    and `list_content_types` answer the same shape; `search` takes `page` and
    `per_page` beside `limit` and `offset`.

The deprecated aliases answer in the new shapes too: an alias keeps a path
working, not an old body.

**How to find out.** Search the client for `/api/` and for `.items`,
`.pagination`, `.results`, `.comments` and `page_size` read from an API
response. A request to an unversioned path in the server's access log, or a
`Deprecation` header in the client's, is a call to move.

### A config export never contains a secret; plugin API 0.107

**Who is affected:** anyone who moves a site with `trovato config export` and
`config import`, and in particular anyone with webhooks, OAuth clients or the
`trovato_captcha` Turnstile key; a plugin whose config tables hold a secret;
anyone pinning the plugin API version.

**What changed.**

- Export no longer writes a secret, in any form. Where one belongs, the file
  holds a reference to an environment variable instead:

  ```yaml
  # webhook.0193a5a0-0092-7000-8000-000000000004.yml
  secret: env:TROVATO_SECRET_WEBHOOK_0193A5A0_0092_7000_8000_000000000004_SECRET
  ```

  The name is always `TROVATO_SECRET_<TYPE>_<ID>_<COLUMN>`, upper case, with
  every other character as `_`. An empty secret is left out of the file. This
  covers `webhook.secret`, `oauth_client.client_secret_hash`, the variable
  `plugin.trovato_captcha.turnstile_secret_key`
  (`TROVATO_SECRET_VARIABLE_PLUGIN_TROVATO_CAPTCHA_TURNSTILE_SECRET_KEY_VALUE`)
  and, if an older import put it back, `smtp_password`.
- Import resolves each secret before it writes anything, first match wins:
  1. the target already holds a value for it: that value is kept;
  2. the variable the file names is set in the environment of
     `trovato config import`: its value is stored. For an OAuth client the
     variable holds the plaintext client secret, which import hashes; the
     plaintext is never stored;
  3. otherwise import fails, naming each variable to set, and writes nothing.

  So re-importing an export onto the site it came from needs no variable, and
  importing into a fresh install needs one per secret. The variables are read
  by the import only; the server does not need them.
- A file that holds a secret in the clear, as every `webhook` and
  `oauth_client` file from an export made before this release does, is refused,
  whatever the target holds. Re-export from the source site, or replace each
  value with the `env:` reference the error names.
- A plugin names its secret columns beside its config tables (see the plugin
  development guide). The key is additive; the project version moves to
  0.107.0, so `KERNEL_API_VERSION` is `(0, 107)` and the bundled manifests
  declare `api_version = "0.107"`. A config table column whose name says it
  holds a credential (`secret`, `password`, `api_key`, ...) is treated as a
  secret even when undeclared, and export warns.

**How to find out.** `grep -rl 'secret:\|client_secret_hash:' <export dir>`
on an export made before this release: every file it lists holds a secret in
the clear. Remove that directory from any repository it was committed to,
rotate the webhook secrets and OAuth client secrets it held, and export again.

### HSTS behind a TLS proxy needs the proxy in `TRUSTED_PROXIES`

**Who is affected:** a site served over HTTPS through a proxy that terminates
TLS (nginx, Caddy, a load balancer, a Kubernetes ingress, a CDN) and does not
list that proxy in `TRUSTED_PROXIES`. Find out with
`curl -sI https://your.site/ | grep -i strict-transport-security`: after the
upgrade it prints nothing unless the proxy is listed (or the proxy adds the
header itself).

**What changed.** `Strict-Transport-Security` is sent when the request arrived
over HTTPS, which behind a proxy is known only from its `X-Forwarded-Proto`.
That header used to be believed from any peer. It is now believed only from a
peer in `TRUSTED_PROXIES`, the same rule `X-Forwarded-For` follows. List the
proxy (see "Behind a reverse proxy" in `INSTALL.md`), which also gives each
visitor their own address for rate limiting and sign-in lockout. `COOKIE_SECURE`
is unchanged.

### A bad `TRUSTED_PROXIES` entry stops startup

**Who is affected:** a site that sets `TRUSTED_PROXIES`. Check that every entry
is an IP address or a CIDR range; a hostname, a stray character or a prefix
longer than the address family allows (`/33` for IPv4) is now refused.

**What changed.** An entry that did not parse used to be dropped without a word,
so the proxy it was meant to name was not trusted and every visitor behind it
shared one rate-limit budget. The server now refuses to start and names the
entry. CIDR ranges are accepted, so a proxy tier whose addresses change can be
listed as its network. A site with `TRUSTED_PROXIES` unset is unaffected.

`CLIENT_IP_STORAGE` is new and defaults to `full`, which stores what was stored
before; see "Client addresses and privacy" in `INSTALL.md` for `truncated` and
`none`.

### URL aliases: every item write, in the item's language and stage

**Who is affected:** sites with a pathauto pattern for a content type whose
items are also written by a plugin, a bulk action on `/admin/content`, a
revert, or `config import`; sites whose items are not all in `en` or not all
on the live stage.

**What changed.**

- Pathauto now runs on every item create and update, not only the two edit
  forms. Items those other paths wrote before the upgrade have no alias.
  **Check:** for each type with a pattern,
  `SELECT id, title FROM item i WHERE type = '<type>' AND NOT EXISTS (SELECT 1 FROM url_alias a WHERE a.source = '/item/' || i.id)`.
  **Repair:** press *Regenerate* for the type at `/admin/config/pathauto`,
  which creates the missing aliases and leaves ones that match alone.
- A new alias is written in the item's language and on the item's stage, not
  `en` on the live stage. Resolving an address, and linking to an item, now
  prefer an alias in the reader's language and otherwise take one in any
  language, so an existing `en` alias of an item in another language keeps
  working and nothing needs rewriting.
- Deleting an item now deletes its URL aliases and every menu link to its
  `/item/{id}` path or to one of its aliases, in the same transaction; a link
  nested under one of those moves up to that link's parent. Aliases and links
  left behind by items deleted before the upgrade stay until removed at
  `/admin/structure/aliases` and `/admin/structure/menus`.

### Page builder: no inline styles, server-side validation, plugin components

**Who is affected:** sites with `trovato_page_builder` enabled and pages built
with it; anyone building the Docker image from a fork that added plugins.

**What changed.**

- The page builder's sanitizer no longer keeps a `style` attribute, and no
  standard component writes one. Three props that did are no longer rendered:
  `backgroundColor` on Hero, Call to Action and Section, `gap` on Columns, and
  the standard Hero's `backgroundImage` as a CSS background (it is now an
  `<img>` behind the content, so the picture still shows). The first two are
  replaced by declared variants: a `background` select (none, light, dark,
  brand, the classes `pb-bg--light`, `pb-bg--dark`, `pb-bg--brand`) and
  `gapSize` (default, none, small, large, the classes `pb-columns--gap-*`).
  Stored values are kept and still validate; they are hidden in the editor and
  ignored by the renderer. **Check before upgrading:** find pages that use them
  (`SELECT id, title FROM item WHERE fields::text ~ '"(backgroundColor|gap)"\s*:\s*"[^"]'`)
  and, after the upgrade, pick the nearest `background` or `gapSize` for each,
  or restyle `.pb-bg--*` in your theme.
- `/static/css/page-builder.css` is now linked on every page with page builder
  content. It was not linked before, so a theme that styled the `pb-*`
  classes itself will now get both; check those pages after upgrading.
- A page builder field is now validated on the server whenever a create or
  update changes it: every prop against its component's declared field type.
  A page whose stored layout breaks a rule (a select value that is not one of
  its options, a `javascript:` URL, a component type no enabled plugin
  declares) still renders as before, but saving a change to its layout is
  refused, naming the component and field, until the value is fixed or the
  component removed. Saves that do not touch the layout are not affected.
- Plugins can ship components. A plugin's `components/`, `templates/` and
  `static/` are read at boot; a component file that does not parse, or a
  component type two enabled plugins both declare, stops the server with an
  error naming them. Plugin changes were already restart bound; so are their
  components.
- The Docker image assembles a plugin directory only for each plugin it built
  (BL-122), and copies its `components/`, `templates/` and `static/`. A plugin
  you add to `plugins/` must also be added to the Dockerfile's build list, as
  before, or it is no longer shipped as a manifest with nothing behind it.

### Several servers can share one site; plugin changes on the admin page wait for a restart

**Who is affected:** everyone, in four small ways; anyone running, or planning
to run, more than one server, in the ways
[docs/deployment/multi-server.md](docs/deployment/multi-server.md) describes.

**What changed.**

- The server checks at startup that it can write, read back and delete a file
  in `UPLOADS_DIR`, and refuses to start, naming the directory, if it cannot.
  **Check before upgrading:** the user the server runs as can create and delete
  files in `UPLOADS_DIR` (or create the directory if it does not exist yet).
- Enabling or disabling a plugin at `/admin/plugins` now changes only its
  stored status, as `trovato plugin enable` and `disable` already did. It takes
  effect when the server restarts (every server, if more than one); the page
  says so and marks what is waiting. A script or habit that relied on the
  admin page applying a change at once now needs a restart after it.
- `trovato config import` no longer asks for a restart, because the servers'
  caches now follow a write within about 250 ms. It still asks when the import
  changed which plugins are enabled.
- The chat, assistant and AI provider rate limits count in Redis, so they are
  per site rather than per server, and a counter survives a server restart.
- New optional `PAGEFIND_OUTPUT_DIR`. Unset, the Pagefind index goes where it
  always has, `pagefind` under the first `STATIC_DIR`.

Nothing to configure for a single server.

### Config export carries plugin configuration and leaves runtime state out; plugin API 0.106

**Who is affected:** anyone who moves a site with `trovato config export` and
`config import`; a plugin that stores site configuration in its own tables;
anyone pinning the plugin API version.

**What changed.**

- A manifest may name its configuration tables:

  ```toml
  [config]
  tables = ["redirect"]
  ```

  Export writes each row as `<table>.<primary key>.yml` after the kernel's
  types, and import upserts them by primary key. The in-tree plugins declare
  `image_style`, `redirect`, `webhook`, `oauth_client`, `config_translation`
  and `locale_string`. A new `plugin_status.<name>.yml` entity records whether
  each plugin is enabled; import applies it first and runs the enabled
  plugins' migrations before writing their rows. The key is additive: a
  manifest without it declares nothing. The project version moves to 0.106.0,
  so `KERNEL_API_VERSION` is `(0, 106)` and the bundled manifests declare
  `api_version = "0.106"`.
- `installed`, `update_status`, `update_check` and `queue_runner` are runtime
  state of one deployment. Export no longer writes them, and import ignores
  them with a warning.
- Import now prints that the server must be restarted: a running server keeps
  its cached configuration and its plugin set until then.
- A role, URL alias or plugin row whose name (or other unique key) is held by
  another id on the target, as plugin migrations seed them, takes over that row
  under the id the file names, so an export imports into another install. Role,
  stage and URL alias files' timestamps are now applied on update.
- Export skips, with a warning, a gather query no import would accept (the
  `oauth_client` and `webhook` admin lists and `scheduled_items`, which the
  plugins seed).
- `trovato_oauth2` now runs `004_add_scopes_column.sql`, which its manifest
  never listed; without the column every OAuth client lookup found nothing.
- `StageAwareConfigStorage` and `AppState::config_storage_for_stage` are gone.
  Nothing called them; configuration is not staged.

**How to find out.** Re-export once to pick up plugin configuration. An export
holds no secret (see "A config export never contains a secret" above), so the
directory can be committed.

### Errors on HTML pages: a themed page for browsers, `AppError` for scripts

Every error a page answers with is decided in one place from the request's
`Accept` header. A browser (`Accept` naming `text/html`) is shown the site's
`error.html` in the normal page wrapper, or `error--404.html` and the like if
the theme has one, and is redirected to `/user/login` when signed out. Every
other caller gets the `AppError` JSON body (`code`, `message`, `request_id`)
with the same status.

- **Themes**: to restyle errors, override `error.html` or add
  `error--{status}.html`. The context carries `status`, `error_title`,
  `message`, `home` and `home_label`.
- **Scripts calling HTML pages**: a failure that was an HTML page (or an empty
  `404`) is now JSON unless you send `Accept: text/html`. `/item/{id}` and the
  item forms answer a browser with the themed page instead of JSON.

### JSON errors are one shape: read `message`, not `error`

**Who is affected:** an API client, script or theme JavaScript that reads the
reason out of a kernel JSON error.

**What changed.** Every JSON error from a kernel route is now

```json
{"code": "not_found", "message": "Comment not found", "request_id": "..."}
```

with `details` added for a validation failure. The `{"error": ...}` bodies
(comments, categories, gathers, items, passkeys, sessions, recovery, account
deletion, CSRF and API token failures), `/api/v1`'s `{"error", "status"}`, the
rate limiter's `{"error", "retry_after"}` and the plain-text answers of the
content-lock, OAuth, image style and bearer-token routes are gone. Status codes
are unchanged, except that the passkey, session and recovery JSON endpoints
answer an anonymous caller `401` instead of redirecting it to the login form.
The rate limit still sends `Retry-After`.

A plugin's own API routes (`handler_type = "api"`) answer the same body plus
`error`, the same string as `message`, until 2.0.

**What to do.** Read `message` (and branch on `code` rather than on the
text). A client of plugin API routes needs nothing now and should move to
`message` before 2.0. Find readers with `grep -rn '\.error\b' static/ templates/`
in a theme or site.

### Anonymous JSON callers get `401`, signed-in ones `403`

**Who is affected:** an API client or script that calls kernel JSON endpoints
without signing in, or with a session or token for a user who lacks the
permission, and tells the two cases apart by status code.

**What changed.** Every JSON refusal follows one rule: `401` when nobody is
signed in (no session, or the session's account is blocked or deleted), `403`
when a signed-in user lacks the permission. The ad hoc gather endpoint
(`POST /api/gather/query`), the dead-letter queue and embed admin endpoints,
`/system/ajax`, the block editor endpoints and `POST /api/lock/break` used to
answer `403` to an anonymous caller. Browser screens are unchanged except the
translation screens, which answered `400` and now redirect to the login form or
answer `403` like every other admin screen. `/admin/config/ai/features`, which
was never reachable, is gone.

**What to do.** If a client treats `403` from those endpoints as "sign in",
make it treat `401` that way.

### Boot warns about every plugin holding `raw_sql`

**Who is affected:** an operator whose site runs a plugin with `raw_sql = true`
under `[capabilities]`, and anyone alerting on warnings in the boot log.

**What changed.** Boot logs one warning, `plugins holding raw_sql: ...`, with
the holders' names in `plugins`, and the admin plugin listing marks each holder
**raw SQL**. Nothing a plugin may do changed.

**What to do.** Nothing, unless an alert fires on any boot warning: then expect
this one while a `raw_sql` plugin is enabled. To make it go away, move the
plugin's reads to the `query` interface and its writes to `db` and `item-api`
(`docs/plugin-capabilities.md`) and drop the grant.

### `query-items` lists the request's stage only

**Who is affected:** a plugin that calls `item-api` `query-items` (or the SDK's
`host::query_items`) on a site with more than one stage.

**What changed.** `query-items` used to list items on every stage. It now lists
the request's read stages only: Live, unless the request is a plugin route
served to a viewer who reads another stage. The order is still newest change
first, with the id breaking ties. A published item on an `accessible` or
`internal` stage no longer appears in a live listing.

**What to do.** Nothing, unless a plugin relied on seeing other stages' items in
a live request, which was the defect. For listings, the new `query` host
interface (`host::query`) sorts, joins, pages and returns each row's URL.

### A translation save fires `tap_item_update`

**Who is affected:** a plugin that implements `tap_item_update`, on a site
whose editors translate content.

**What changed.** A translation saved or removed on the admin translation
screen, or through the new `item-api` translation calls, is now an edit of the
item. `tap_item_update` fires with the item, and the item cache drops it.
Before, nothing fired, so a plugin that mirrors or indexes items did not hear
about a translation.

**What to do.** Nothing, unless a `tap_item_update` handler assumes the item
row itself changed. The item it receives is the stored item; the translation
is in `item_translation`, which `host::list_translations` reads.

### Gather definitions can group filters and summarise rows

**Who is affected:** code that builds a `trovato_kernel::gather::QueryDefinition`
as a Rust struct literal, and anyone who edits a gather in the admin builder.

**What changed.** `QueryDefinition` has two new fields: `filter_groups` (for
`or` and nested groups) and `aggregation` (`group_by`, `aggregates`, `having`,
`distinct`, which are flattened into the JSON). Stored definitions and config
files without them read as before and emit the same SQL. The admin builder has
no control for either one, so it will not save a gather that has them. Its edit
form says so and turns saving off, because saving would drop them.

**What to do.** In a struct literal, add the two fields or end it with
`..Default::default()`. Change a gather that uses them in its config file, or
in the plugin that installs it.

### Follow-up sentiment classification is available and off

**Who is affected:** a site that uses the AI search assistant's follow-up
questions and wants to know whether answers help.

**What changed.** "Classify follow-up sentiment" on `/admin/config/search` sends
each follow-up question, with the answer before it, to the site's Chat provider
for a one word label (satisfied, confused, unmet) and counts the labels per day on
the search report. It is off by default and does nothing while analytics is off.

**What to do.** Nothing, to keep it off. Before switching it on: every follow-up
costs one extra provider call, and the follow-up text leaves the site for the
provider. The search endpoints, and this classifier, do not record token usage
against the AI budgets today, so watch the provider's own billing.

### Search analytics are on by default

**Who is affected:** every site. The upgrade adds two tables,
`search_query_daily` and `search_daily`, and starts counting searches.

**What is recorded.** Per UTC day: each normalized query with how often it was
searched and how often it found nothing, and the day's expansion cache hits and
misses. The query is lowercased, email addresses become `[email]` and numbers of
six or more digits (phone, account, order numbers) become `[number]`; digits
inside a word are kept. No IP address, account, session, user agent,
referrer or time finer than the day is stored. Rows older than 90 days are deleted
by cron.

**What to do.** Nothing, to keep the default. To record nothing, untick "Record
search analytics" on `/admin/config/search`; to keep fewer days, lower "Keep
analytics for (days)" there. If your privacy notice lists what the site
stores, add search queries counted per day. A site that runs its own search page
template and wants browser searches counted sends `POST /api/search/log` with
`{"query": "...", "results": n}` as `scolta.js` does.

Server searches are counted from the first request after the upgrade. Browser
searches are counted once a visitor's browser loads the new `scolta.js`; static
files are served with a one day cache lifetime, so a returning visitor's searches
may go uncounted for up to a day.

### Rebuild the Pagefind index for titles and field weights to apply

**Who is affected:** a site with the `trovato_search` plugin enabled, which builds
the static Pagefind index the search page uses in the browser.

**What changed.** The index now holds each item's title inside the indexed region
and each configured search field at the weight its `search_field_config` letter
gives it. An index built by an earlier version has neither: titles are not
searchable on their own and every field weighs the same. Nothing breaks, but
browser ranking stays as it was until the index is rebuilt.

**What to do.** Request a rebuild and let cron run it:

```sh
psql "$DATABASE_URL" -c "UPDATE pagefind_index_status SET rebuild_requested = true WHERE id = 1"
curl -X POST http://localhost:3000/cron/<your cron key>
```

Editing any published item does the same on the next cron run. Search ranking
settings are new and default to the values the search template used to carry,
so there is nothing else to migrate; see `/admin/config/search`.

### The password reset link opens a page; `GET` answers JSON only when asked for it

**Who is affected:** a client (a script, an app, a test harness) that fetches
`GET /user/password-reset/{token}` and reads JSON from it. Browsers and the
`POST` endpoints are not affected: a `POST` with `Content-Type:
application/json` gets the same JSON answers it always did.

**What changed.** The link a reset mail carries now opens a set-password page,
and `GET /user/password-reset` is a page asking for an email address. `GET`
on the link answers `{"success": true, ...}` or a 400 only when the request
sends `Accept: application/json`; without it, the answer is HTML.

**How to find out, before upgrading.** Search your clients for
`password-reset/` and check each `GET` sends `Accept: application/json`. A site
whose resets are all done in a browser has nothing to do.

### Plugin mail from `tap_cron` and `tap_queue_worker` now sends

**Who is affected:** a plugin that calls `send-to-site-contacts`
(`host::mail_send_to_site_contacts`) from `tap_cron`, `tap_queue_worker` or
`tap_install`.

**What changed.** Those calls always failed with `ERR_MAIL_NOT_CONFIGURED`,
because background dispatch carried no email service, even on a site with SMTP
configured. They now send, through the same email service the kernel uses, and
each one is counted against the plugin's `mail` bucket (100 an hour by default,
`TROVATO_RATE_LIMIT_MAIL` or `rate_limit.mail` to change it), exactly as a call
from a request is.

**How to find out.** Search your plugin for `mail_send_to_site_contacts` in
code reached from those taps. A plugin that called it there and treated the
error as "mail is off" will now deliver; make sure that is what it should do,
and that a loop cannot spend the hour's bucket on mail nobody wants.

### SMTP settings stored in `/admin/config/site` are deleted; use the environment

**Who is affected:** a site whose administrator entered SMTP settings in the
"Email / SMTP" section of `/admin/config/site`.

**What changed.** Those values were never used for delivery. Mail has always
been sent with `SMTP_HOST`, `SMTP_PORT`, `SMTP_USERNAME`, `SMTP_PASSWORD`,
`SMTP_ENCRYPTION` and `SMTP_FROM_EMAIL` from the environment; the form only
stored its values, the password in clear text, in `site_config`. The form is
gone, the page now shows the effective environment configuration read only, and
migration `20261121000001_remove_site_config_smtp` deletes the six keys
(`smtp_host`, `smtp_port`, `smtp_username`, `smtp_password`,
`smtp_encryption`, `smtp_from`).

**How to find out, before upgrading.** Read back what was stored:

```sql
SELECT key, value FROM site_config WHERE key LIKE 'smtp_%';
```

No rows means nothing to do. Otherwise, if those are the settings you meant
mail to use, set them in the environment (`SMTP_HOST`, `SMTP_PORT`,
`SMTP_USERNAME`, `SMTP_PASSWORD`, `SMTP_ENCRYPTION`, `SMTP_FROM_EMAIL`) and
restart; the migration deletes the rows on upgrade. A stored password was
readable to anyone with database or backup access, so rotate it.

### File fields are indexed and promoted; uploads to 10 MB; served type is the stored type

**Who is affected:** a site with File fields; anyone who saves items through
the item API, a plugin or `config import`; a reverse proxy in front of uploads.

**What changed.** Migration `20261120100001_index_file_field_references`
records a `file_reference` row for every file id an existing item's File field
holds (read from the item type's field definitions), then promotes every
`Temporary` file that has a reference to `Permanent`. It is additive and safe to
rerun. Reversing it means deleting the backfilled rows
(`DELETE FROM file_reference` for the pairs it added); no data is lost either
way, and promoted files simply stay.

Behaviour changes that follow: a file in a File field is now served to every
viewer of a published item that references it (before, only to its uploader
and administrators); a referenced file is promoted on every write path, so the
temporary file cleanup no longer deletes files attached through the item API, a
plugin or config import; a save that attaches a file the acting user cannot
fetch is refused with 403; uploads up to 10 MB work, and a larger one is a 413;
an upload whose filename extension disagrees with its content is refused with
415; `/files/` answers with the stored type, not one guessed from the path.

**How to find out.** Files the migration will make public:
`SELECT count(*) FROM file_managed f WHERE NOT EXISTS (SELECT 1 FROM
file_reference r WHERE r.file_id = f.id);` before and after shows how many
files gained a reference. If a proxy fronts the kernel, check its body limit on
`/file/upload` (see INSTALL.md, "Serving uploaded files").

### Plugins filter user HTML through `host::filter_text`; plugin API 0.105

**Who is affected:** a plugin that renders HTML a user wrote; anyone pinning
the plugin API version.

**What changed.** A new host interface, `text-format`, has one call,
`filter-text(text, format)` (SDK: `host::filter_text`), which runs the
kernel's own text format filter for a format the acting user may use, and
returns an error for any other, never the text unfiltered. A plugin that returns
user written HTML must filter it through this: the kernel does not filter a
plugin's output. Declare `text-format` in `host_interfaces`. The project
version moves to 0.105.0, so `KERNEL_API_VERSION` is `(0, 105)` and the bundled
manifests declare `api_version = "0.105"`. A plugin that calls `filter_text`
should declare `0.105`, which a 0.104 kernel refuses cleanly instead of failing
to link the import.

**How to find out.** `grep -rln "| safe\|text/html" plugins/*/src` lists
plugins that return HTML; check each one's user written parts.

### Form AJAX for every editor; conditional fields

**Who is affected:** a plugin implementing `tap_form_ajax`; a script or theme
calling `POST /system/ajax`; a site that wants a field shown only for some
values of another.

**What changed.** `/system/ajax` no longer requires `administer site`. It
requires an `X-CSRF-Token` header (the form prints one in `_ajax_token`, and
each response carries the next in the same header, since tokens are single
use), a form state built for the caller, and that the caller may still open
the form; otherwise 403. `tap_form_ajax` now runs with the full host API. "Add
another" on a multi value field is built by the server and works on
`/item/{id}/edit` for any editor. The old built-in `remove_{field}_{n}`
trigger and `templates/form/multi-value-item.html` are gone: clearing a value
removes it on save. A field definition may carry `settings.visible_when: {field,
equals}`; existing definitions load unchanged.

**How to find out.** `grep -rn "system/ajax\|tap_form_ajax" plugins themes`
lists callers to check for the header.

### Content and profile forms run the form taps; one form renderer

**Who is affected:** a plugin that implements `tap_form_alter`,
`tap_form_validate` or `tap_form_submit`; a theme that overrides
`templates/admin/content-form.html`, `templates/user/profile.html` or any
`templates/form/*.html`; a script that posts to `/item/add/{type}`,
`/item/{id}/edit` or `/user/profile` without loading the page.

**What changed.** The item forms (on `/item/...` and `/admin/content/...`) and
the profile form are now built as a kernel `Form` and rendered by the one form
renderer, so the three form taps fire for them, as documented in
`docs/plugin-development.md` (Forms). Before, none of them fired for any form a
route served, so a plugin's implementation of these taps starts running on
upgrade. Form ids are `item_add_{type}`, `item_edit_{type}` and
`user_profile_form`. Alterations chain in weight order. A plugin that adds a
button needing a script names a kernel asset library in `libraries` (the AI
Assist buttons use `ai_assist`); the admin content page no longer loads
`ai-assist.js` and `file-upload.js` on its own. The `/item/...` forms render
inside the site theme rather than as a bare page, and no form carries an inline
`<style>`; form styles are in `static/css/forms.css`.

`content-form.html` now prints only `form_html`; a theme override of it that
builds its own inputs must print `form_html` instead. Form element templates
read element properties directly (`element.options`, not
`element.element_type.options`). The item forms carry their CSRF token as
`_token` (was `_csrf`), and the profile form now needs `_form_build_id` too.

**How to find out.** `grep -rn "tap_form_" plugins/*/src` lists the plugins
whose form taps will start firing; `ls themes/*/templates/form
themes/*/templates/admin/content-form.html` lists overrides to review.

### Item form saves keep what the form did not show; Booleans are JSON booleans

**Who is affected:** a site whose items carry keys the edit form does not
render (plugin written keys, keys a type no longer declares, multi value
fields), a site whose Boolean fields were saved from an item form, and a
script that posts urlencoded bodies to `/item/add/{type}`, `/item/{id}/edit`
or the admin content forms without first loading the page.

**What changed.** A save from either item form now starts from the stored
fields and overwrites only the fields the form rendered, so nothing else is
erased. A checked Boolean is stored as JSON `true` and an unchecked one, when
the form showed it, as `false`; a migration rewrites the `"1"`, `"0"`, `"true"`
and `"on"` strings already stored in items for fields a type declares as
Boolean. Gather filters on a Boolean field now match items that were saved
from the admin form. Revisions are immutable history and keep the string they
were saved with, so reverting to a revision from before the upgrade stores the
string again until the item is next saved from a form. A field with a cardinality other than 1
is posted as `name[0]`, `name[1]` and stored as an array; more values than its
cardinality is a field error.

Each rendered form now saves server side state under its `_form_build_id`, and
a urlencoded submission must carry the build id of a form rendered for the same
user (and, on an edit, the same item). One that does not is refused with "This
form has expired" and nothing is saved. JSON submissions are unchanged: absent
top level keys keep their stored values, and a `fields` object replaces the
field map, with fields the user may not edit copied back.

**How to find out.** `SELECT type, count(*) FROM item WHERE fields::text ~ '"1"'
GROUP BY type` lists types that may hold Boolean strings before the upgrade; a
script posting item forms without a build id will start receiving 400s.

### Reading a stage needs `use stages`; gather totals count what the viewer can see

**Who is affected:** a site whose editors preview a stage other than Live, and
anything that reads a gather's `total` or pager for a signed-in viewer.

**What changed.** A new kernel permission, `use stages`, decides who may read a
stage other than Live: switch the session's stage at `/admin/stage/switch`, and
see it, with Live beneath it, in search, gathers (`?stage=`) and URL aliases.
No role is granted it on upgrade; administrators hold it implicitly. Before,
the switch was for administrators only, but a gather took `?stage=` from anyone.
If editors who are not administrators should preview stages, grant their role
`use stages` at `/admin/people/permissions` or in its role file. A request for
a stage without the permission is answered from Live, not refused, and a
switch to a stage that does not exist is now a 400.

A gather's `total`, `total_pages` and `has_next` now count only the rows the
viewer can see, up to the gather scan cap (`max_scan`, 1000 by default). For an
anonymous visitor nothing changes on Live; for a signed-in viewer who cannot
see some matching rows (unpublished items by others, say), the numbers go
down to what they can actually page through.

### New content lands on the default stage; stage moves follow the workflow

**Who is affected:** a site that marked a stage other than Live as the default
at `/admin/structure/stages`, a site with a `workflow.editorial` variable (the
tutorial ships one), and a plugin that calls `save-item`.

**What changed.** A create that names no stage used to land on Live whatever
the stage screen said. It now lands on the stage marked default. Find out
whether that is you before upgrading:

```sql
SELECT machine_name FROM stage_config WHERE is_default;
```

If that answers anything but `live`, new items (from the forms, the JSON API,
`save-item` and MCP) will start arriving there, and will not be public until
they are moved to Live.

The `workflow.editorial` variable is now enforced. A user moves an item from one
stage to another only when a transition names exactly that pair with a
permission they hold; with no workflow, only administrators move items.
`config import` now refuses a workflow transition that names a stage or a
permission it cannot find. The tutorial's file named `publish conferences`, a
permission the Part 5 plugin declares, and would be refused before that plugin
is enabled; it now names the kernel's `publish content`, which the tutorial's
publisher role holds. If your site copied the tutorial's file, check each
`permission:` against what your roles hold.

`save-item` now honours a `stage` machine name it used to ignore; a plugin that
sends one will see its writes moved, or refused with `ERR_ITEM_ACCESS_DENIED`.

An item whose stage row is missing (a `stage_config` row deleted by hand) is now
denied to everyone but administrators, where it was treated as visible.

### Login and registration admit their stated counts; rate limits by role

**Who is affected:** a site that tuned `login` or `register`
(`TROVATO_RATE_LIMIT_LOGIN`, `TROVATO_RATE_LIMIT_REGISTER`, or the
`rate_limit.login` / `rate_limit.register` site configuration keys) to what it
saw in practice, and a site that wants different limits for different roles.

**What changed.** Each sign-in and registration attempt used to be counted twice
against the same per-address budget, once by the rate-limit middleware and again
by the handler, so the configured count admitted about half as many attempts as
it said (`register`, 3 an hour, admitted one). Each attempt is now counted once,
so the stated count is what an address gets: twice what it effectively allowed
before. If you raised either count to compensate, lower it again.

New keys, nothing to do unless you want them: a bucket can give a role its own
count, `TROVATO_RATE_LIMIT_<BUCKET>__<ROLE>` or `rate_limit.<bucket>.<role>`
(the role's name lower-cased, other characters as underscores, so
`anonymous_user`, `authenticated_user`). An anonymous request gets the
`anonymous_user` count per address; a signed-in request gets the most generous
count among its user's roles per user. With no role keys set, every limit is what
it was. See "Rate Limits" in `INSTALL.md`.

### List every proxy hop in `TRUSTED_PROXIES`

**Who is affected:** a site behind more than one proxy (for example a CDN in
front of a load balancer) that sets `TRUSTED_PROXIES`. Find out by checking how
many proxies a request passes through before it reaches Trovato, and whether
`TRUSTED_PROXIES` names each of them.

**What changed.** The client address is now read from the right of
`X-Forwarded-For`: the first entry that is not a trusted proxy is the client.
The leftmost entry was used before, and a client can write whatever it likes
there. If only the proxy nearest Trovato is listed, the client address now
resolves to the next proxy out, and every visitor behind that proxy shares one
rate-limit bucket and one login lockout counter. List every proxy hop's address
in `TRUSTED_PROXIES`. A site behind a single proxy needs no change.

Related changes that need nothing from an operator:
- The login lockout now counts five failures per username and client address,
  plus fifty per username across all addresses.
- Password reset requests have their own limits: five per client address per
  fifteen minutes, and three mails per destination address per hour, the second
  overridable as `TROVATO_RATE_LIMIT_RESET_MAIL`.

### API token requests no longer receive a session cookie

**Who is affected:** any client that authenticates with an API token
(`Authorization: Bearer <token>`) and then relied on the session cookie the
response set, sending the cookie instead of the token on later requests. Find
out by checking whether the client stores cookies from API responses and sends
requests without the `Authorization` header afterwards.

**What changed.** A token request is now stateless. It is authenticated for that
request only: the response carries no `Set-Cookie`, nothing is stored in Redis,
and nothing is added to the user's list of signed-in devices. A client must send
its token on every request. A session the old behaviour created stays valid
until it idles out (24 hours without use), but no new ones are created; such a
session appears on its user's sessions page and can be ended there.

Two related changes need nothing from an operator: revoking a token takes effect
on the next request instead of up to a minute later, and a token-authenticated
write no longer needs a CSRF token.

### `item-api` acts as the requesting user, and needs a capability in the background

**Who is affected:** a plugin that imports the `item-api` interface —
`get-item`, `save-item`, `delete-item`, `query-items`. No in-tree plugin does;
the SDK has no binding for it, so this is plugins that hand-roll the imports,
which in practice means Argus and Netgrasp. Find out with
`wasm-tools print <plugin>.wasm | grep item-api`, or look for `item-api` in the
plugin's import section.

**What changed.** These four functions decided nothing. They called the `Item`
model directly and used the requesting user only as an author id, so a plugin
handling an anonymous visitor's request could read unpublished items and
restricted fields through it, and rewrite or delete any item by id.

They are decided as the requesting user now. A request-scoped call — any user
that is not the background principal, anonymous included — gets that user's own
answer: `get-item` requires `view` and drops fields the user may not see,
`query-items` returns only what the user may view, `save-item` requires `edit`
on an update and `create {type} content` on a create, and `delete-item`
requires `delete`. A denied read is indistinguishable from a missing item and
writes `null`, exactly as a missing id does. A denied write returns
`ERR_ITEM_ACCESS_DENIED` (-60).

Writes now go through the item service as well; see the next entry.

**Background contexts need a capability.** Cron and the queue worker run under
the kernel-internal background principal, which has no identity and no
permissions, so there is no user whose authority an `item-api` call could act
with. A plugin that calls these functions from `tap_cron` or `tap_queue_worker`
must declare the new manifest capability:

```toml
[capabilities]
item_background = true
```

With it, a background call behaves as it did before, with kernel authority.
Without it, every `item-api` function returns `ERR_ITEM_BACKGROUND_DENIED`
(-61). This is the same declared, auditable manifest plane `ai_background`
already uses, and for the same reason.

**What to do.** If your plugin writes items from a request path on a user's
behalf, check that the user it is acting for actually has the rights you were
relying on the kernel not to check. If it touches items from cron or the queue
worker, add `item_background = true` before you upgrade, or those jobs start
failing on the first run.

### A plugin's `save-item` and `delete-item` fire the item taps

**Who is affected:** a plugin that writes items through `item-api` (`save-item`,
`delete-item`), and a plugin that implements `tap_item_presave`,
`tap_item_insert`, `tap_item_update`, `tap_item_update_index` or
`tap_item_delete`. Find out with `wasm-tools print <plugin>.wasm | grep -E
'item-api|tap_item_'`.

**What changed.** A plugin's item write is now the same write a person makes:
it goes through the kernel's item service, so the item write taps fire on the
other plugins, the field and publish gates apply, a revision is recorded and the
item cache is refreshed at once instead of after `CACHE_TTL_ITEMS`. Three
consequences a plugin can see:

- A write that changes a field the acting user may not edit, or changes whether
  an item is published without `publish content`, is refused with
  `ERR_ITEM_ACCESS_DENIED` (-60). It used to be written.
- `delete-item` on an id that names no item returns `ERR_ITEM_NOT_FOUND` (-62)
  instead of `0`.
- A tap that writes items while handling an item write no longer runs alone: its
  write fires the taps of every plugin not already handling one around it, and a
  write nested four write taps deep is refused with `ERR_ITEM_WRITE_DEPTH` (-63).

`save-item` now also takes `language` on a create, which must be one of the
site's languages.

**What to do.** A plugin that implements an item write tap and writes items
from it should check it does not depend on its own write being invisible to the
other plugins. A plugin that treated `delete-item`'s `0` as "gone or never
there" should accept -62 as the second half of that. The SDK now binds all four
functions (`trovato_sdk::host::get_item`, `save_item`, `delete_item`,
`query_items`); hand-written `item-api` imports keep working.

### A plugin with an import the kernel does not provide fails to load

**Who is affected:** a plugin whose module imports a function the kernel does
not provide: a WASI function other than `fd_write`, `random_get`,
`environ_get`, `environ_sizes_get`, `proc_exit` and `clock_time_get`, or a
function missing from a host interface it declares. Find out by looking for a
`cannot be linked` error naming the plugin in the startup log.

**What changed.** Such a plugin used to load and report success, then fail every
tap at dispatch with `unknown import`. It now fails to load, with a message
naming the import, and the rest of the site starts without it. A plugin built
with Rust after 1.96 imports `clock_time_get`, which the kernel now provides,
so those plugins load and run.

**What to do.** Nothing, unless the startup log names one of your plugins; that
plugin was already failing on every request.

### A plugin call now has a wall clock ceiling

**Who is affected:** any site; in practice only a plugin that makes many slow
host calls in one invocation.

**What changed.** Guest CPU was already bounded by epoch interruption, and the
deadline callback extended that budget by however long a call had spent parked
inside host functions, so that waiting on an AI provider or a feed fetch was
not billed as computing. The extension had no ceiling, so a guest looping over
slow host calls was never interrupted and could hold a request open
indefinitely.

Total elapsed time per call is now bounded too, separately from CPU: 120
seconds for a request-scoped call and 900 seconds for a background one, both
far above the epoch budgets they sit beside, and both overridable with
`PLUGIN_REQUEST_WALLCLOCK_SECS` and `PLUGIN_BACKGROUND_WALLCLOCK_SECS`. A call
stopped by this bound is logged distinctly from one that exhausted its CPU, so
the two are separable.

### Plugin raw SQL is parsed, and some tables are off limits to every plugin

**Who is affected:** a site running a plugin that declares `raw_sql = true` or
names a kernel table in `db_tables`. Find out with
`SELECT name FROM plugin_status WHERE status = 1;` and read those plugins'
`.info.toml` files; a site running only the shipped plugins is affected in
exactly one place, described below.

**What changed.** Two things, both narrowing what a plugin's database access
reaches.

A protected list now sits under the structured table allowlist and is checked
before it, whatever a plugin's manifest or migrations say: `users`,
`user_roles`, `roles`, `role_permissions`, `plugin_permission`, `api_tokens`,
`password_reset_tokens`, `email_verification_tokens`, `recovery_codes`,
`recovery_email_challenges`, `webauthn_credentials`, `security_audit_log`,
`site_config`, `plugin_status`, `plugin_migration`, `_sqlx_migrations`,
`form_state_cache`, `user_tenant`, `oauth_client` and `webhook`. A call to one
of these is refused with the same `table-not-declared` ABI code an undeclared
table has always returned; the host log says `table-protected` so the two are
separable. Raw SQL is checked against the same list, which it never was before.

And raw statements are parsed rather than scanned for their first keyword.
`query-raw` takes a read and nothing else, and runs in a transaction the server
opens `READ ONLY`. `execute-raw` takes one INSERT, UPDATE or DELETE and nothing
else — narrower than before by `SET`, `RESET`, `DO`, `CALL`, `COPY`, `LOCK`,
`COMMENT`, transaction control, `VACUUM`, `REFRESH` and `DISCARD`. A statement
that does not parse is refused.

**`site_config` deserves its own line.** It holds the SMTP password. The
configuration form writes whatever an administrator types into the
`smtp_password` key, in the clear, unless they used the `env:` indirection, so
a plugin reading the table was reading a credential. If you have ever typed an
SMTP password into `/admin/config` rather than setting `SMTP_PASSWORD` in the
environment, that password is sitting in your database in plain text today;
protecting the table stops plugins reading it but does not remove it. Move it
to the environment and re-save the form.

**What to do.** Nothing, for the shipped plugins. Every raw statement they send
is pinned in a kernel test and still runs, with one exception: `trovato_ai`'s
field-rules read from `site_config` is refused now. That read was already
failing for its own reason — it filters on a column named `name` and the column
is `key` — so no behaviour a site relies on changes; the plugin logs the error
code and returns no rules, as it did before.

For a plugin of your own: if it reads a protected table through raw SQL, it
stops getting rows. If it uses `execute-raw` for anything but one row change,
it stops. Neither is recoverable by a manifest change, which is the point —
raw SQL remains a declared trust grant, but the grant no longer carries the
ability to write through the read path, run DDL through the write path, or
change the session every later kernel query on that pooled connection runs
under.

### The image no longer ships the `argus` plugin

**Who is affected:** a site that runs Argus from the published image, which is
what the old `argus` compose profile in this repository did. Find out with
`SELECT status FROM plugin_status WHERE name = 'argus';`: no row, or a status
of 0, means this is not you.

**What changed.** Argus moved to its own repository, `jeremyandrews/argus`. An
image built after this change has no `plugins/argus`, so a site with it enabled
starts without it: its routes, cron work and queue jobs stop, and its tables and
data stay exactly where they are.

**What to do.** Install it from its repository as an overlay: append its
plugin directory to `PLUGINS_DIR`, or use that repository's own
`docker-compose.yml`, which runs this kernel's published image unmodified. It is
the same plugin with the same migration files, so migrations the site has
already applied are not run again.

## v0.104.0 — 2026-09-24

Four queue and cron changes alter what a running site does with work it has
already claimed. None of them needs a migration, and none needs a configuration
change before you upgrade. Read the first two if you run plugin queues.

### A queue now drains at the width it declared, not at its plugin's widest

**Who is affected:** a site running a plugin that declares more than one queue
in `tap_queue_info` with different `concurrency` values. A plugin with a single
queue, or with the same width on all of them, sees no change.

**What changed.** The kernel read the **maximum** `concurrency` across a
plugin's queues, once per plugin, and applied it to a claim that named no queue.
A plugin declaring `analyze: 4, cluster: 1, summarize: 1` ran all three four
wide, out of four slots they shared. Each queue now claims within itself and
drains at its own declared width, still clamped to `QUEUE_CONCURRENCY_CAP`. A
queue holding rows the plugin never declared drains at width 1.

**What you will notice.** Throughput moves on both sides. A queue that was
inheriting a larger sibling's width slows to the width it asked for, and a queue
that was starving behind a sibling's stuck jobs now drains alongside them. If
you tuned a plugin's declarations while the maximum was the only number that
mattered, those declarations now mean what they say.

**What to do.** Nothing, unless a queue you relied on running wide was only
running wide by inheritance. `CronService::resolved_queue_widths` reports the
width the drain will honor for each queue, which is the answer that did not
exist before.

### Queue rows that have spent their attempts are retired on the first drain

**Who is affected:** a site whose `plugin_queue` table holds rows that were
claimed, passed `max_attempts`, and were never given a terminal outcome. If the
table has none, nothing happens.

**What changed.** `claim_batch` selected on `status`, `next_attempt_at` and
`locked_until` with no `attempts < max_attempts` term, so a row whose claimer
died after the bound was handed back to a worker forever, `attempts` climbing
past its own limit. The bound is now part of the eligibility predicate, and a
reaper at the head of every drain retires rows that are claimed, past the bound,
and past their lease.

**What you will notice.** On the first drain after the upgrade, those rows move
to dead with a `dead_reason` saying the claim lease expired with no terminal
outcome recorded. They are rows that were already past their limit; what changes
is that they now have an ending, and stop taking a worker slot on every cycle.

**What to do.** Count them first if you want to know the size of it:

```sql
SELECT plugin, queue, count(*)
FROM plugin_queue
WHERE status = 'claimed'
  AND attempts >= max_attempts
  AND locked_until < now()
GROUP BY plugin, queue;
```

Crash recovery is unchanged: a job with attempts still on it whose claimer died
is reclaimed exactly as before.

### A job that burns its whole CPU budget is dead-lettered on the first occurrence

**Who is affected:** a site with a plugin queue job that runs the background tap
epoch budget (`PLUGIN_BACKGROUND_TAP_DEADLINE_SECS`, default 150) to exhaustion.

**What changed.** Such a job was recorded as an ordinary failed attempt and
rescheduled, and the retry got the same budget to burn against the same wall. It
is now dead-lettered at once, whatever `attempts` says, with a reason naming the
budget.

The counterpart matters as much: the budget now counts only what the guest
executed. Time parked in a host call — an AI request, a feed fetch — no longer
counts against it, so a job that is merely waiting on a slow provider is not cut
off at all. The old wall-clock reckoning could not tell the two apart, and a
single slow response was enough to lose a job on its first attempt.

**What to do.** Nothing, unless you were relying on retries to carry a job that
genuinely computes past the budget. Raise `PLUGIN_BACKGROUND_TAP_DEADLINE_SECS`
for that deployment if so; it is settable per service, and its default is
unchanged.

### A cron run outlives the client that triggered it

**Who is affected:** a site that pokes `/cron` from a scheduler with a timeout
shorter than a run — the usual `curl -m 30` in a crontab.

**What changed.** The handler awaited the run inline, so a client hanging up
dropped the whole run mid-await: claimed rows were left with their tasks aborted
where they stood, the cron lock was never released, and a leaked heartbeat kept
renewing it until the process restarted. The route now runs cron on a task of
its own and awaits that, so a disconnect cancels only the waiting.

**What you will notice.** Your poker's timeout stops meaning what it used to
mean. `curl -m 30` still returns a timeout on a run that takes longer, but the
run now finishes, releases its lock and completes its queue bookkeeping instead
of dying there. A timeout from the poker is no longer evidence that the run
failed.

**What to do.** Nothing is required. If you alert on the poker's exit status,
that alert now reports "the run took longer than 30 seconds" rather than "the
run died", and should be read, or re-tuned, accordingly.

## v0.103.0 — 2026-09-21

### `/admin` takes a new `access administration pages` permission

**Who is affected:** a site that has delegated an admin screen to a role — one
holding `administer comments`, say — and wants that role to use the dashboard.
A site whose only administrators are superusers or roles holding `administer
site` needs to do nothing: the migration below covers it.

**What changed.** Every admin screen has its own permission, and until now
`/admin` itself took `administer site`. So a delegated role could reach the
screen it was given by typing the address, and got 403 on the dashboard that
would have linked to it. The delegation worked everywhere except at the front
door.

`/admin` now takes `access administration pages`. It is admission to the
administration section and confers no authority inside it: every screen still
asks for its own permission, and the dashboard shows only the links the viewer
may actually open.

**The migration.** `access administration pages` is granted once to every role
that already holds `administer site`, so no existing site loses its dashboard.
That is the only automatic grant. It is a one-time correction for a permission
that was split in two, not a rule: `administer site` does not imply admission
afterwards, and a role granted `administer site` from now on gets exactly that.

**What to do.** If you want a delegated role in the administration section, add
`access administration pages` to it, at `/admin/people/permissions` or in its
`role.*.yml`. This is new capability, not a regression: before this permission
existed there was no way to give that role the dashboard at all.

Superusers are unaffected.

### `administer site` no longer makes its holder a site administrator

**Who is affected:** a site that granted the `administer site` permission to a
role and relied on it as a blanket pass. If no role holds `administer site`,
nothing here applies to you.

**What changed.** The kernel had two notions of administrator that did not
agree. `require_admin` and `require_permission` read the `users.is_admin`
column, the superuser flag. The plugin-facing `UserContext::is_admin()` read
something else entirely: whether the permission list contained the string
`administer site`. So a role granted `administer site` was an administrator to
every check keyed on the context — the item routes among them — while still
being refused by `require_permission` on the admin screens themselves.

There is now one notion. The `users.is_admin` column travels on the request
context as itself, and `administer site` is an ordinary permission with no
structural meaning. It still opens the structure and configuration screens that
0.102 gated on it, and it no longer opens anything else.

**What you will notice.** A role holding `administer site` loses the implicit
pass it had on checks that ask for a *different* permission. The concrete case
is content: `/item/add/{type}` asks for `create {type} content`, and a role
whose only grant was `administer site` used to pass that check and will now be
refused. The same applies to the other context-based checks — item and field
visibility, menu entries, gather results, file serving, the AI assistant's
scope gate.

**What to do.** Grant those roles the permissions they actually need. This lists
every role holding `administer site` and how many users are in it, so you can
see whom it affects before you upgrade:

```sql
SELECT r.name AS role,
       count(ur.user_id) AS users
FROM roles r
JOIN role_permissions rp ON rp.role_id = r.id
LEFT JOIN user_roles ur ON ur.role_id = r.id
WHERE rp.permission = 'administer site'
GROUP BY r.name
ORDER BY users DESC, r.name;
```

For each role the query returns, decide what it was really using the blanket
pass for and grant that: the per-type `create {type} content`, `edit any
content`, `access files`, and so on. `/admin/people/permissions` is the screen;
`role.*.yml` is the file.

A user carrying the `users.is_admin` column is unaffected — they still hold
everything, and they now hold it on the plugin side too, which they did not
before. The superuser column remains non-delegable: it cannot be granted to a
role, and only a superuser may set or clear it.

**The other direction, which costs you nothing.** A plugin asking
`current-user-has-permission` now gets the effective answer, so a superuser is
no longer refused by a plugin's own check. Before, the context builder replaced
a superuser's real permissions with the `administer site` marker, so a plugin's
check saw the marker and nothing else: an administrator could open a plugin's
screen through the kernel's gate and have every action on it refuse them. A
plugin that wrote its own administrator special case can drop it; one that did
not now works for administrators without changes.
